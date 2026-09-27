//! Drives the real router over TCP with WebSocket clients, against the fake claude.

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tower::ServiceExt;
use uuid::Uuid;
use vigie2::auth::{self, Auth, Device, Registry};
use vigie2::child::Program;
use vigie2::manager::SessionManager;
use vigie2::server::{AppState, build_app};
use vigie2::session::Answer;
use vigie2::store::{EventKind, Store};
use vigie2::wire::{ClientMessage, ServerMessage, SessionList, SessionView, WireEvent};

const TIMEOUT: Duration = Duration::from_secs(20);
const DEVICE: &str = "test-device";

/// A turn with an ask, a permission mode change, and an interrupted turn.
const SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "in", "msg": {"type": "user", "message": {"role": "user", "content": "hello"}, "parent_tool_use_id": null, "session_id": "s"}}
{"t": 0, "dir": "out", "msg": {"type": "noise", "n": 1}}
{"t": 0, "dir": "out", "msg": {"type": "noise", "n": 2}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "ask-1", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls"}, "tool_use_id": "t1"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "ask-1", "response": {"behavior": "allow", "updatedInput": {"command": "ls"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "result", "subtype": "success", "is_error": false, "result": "done", "duration_ms": 1, "duration_api_ms": 1, "num_turns": 1, "stop_reason": "end_turn", "total_cost_usd": 0, "usage": {}, "session_id": "s", "uuid": "r1"}}
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "mode-1", "request": {"subtype": "set_permission_mode", "mode": "plan"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "mode-1", "response": {"mode": "plan"}}}}
{"t": 0, "dir": "in", "msg": {"type": "user", "message": {"role": "user", "content": "count"}, "parent_tool_use_id": null, "session_id": "s"}}
{"t": 0, "dir": "out", "msg": {"type": "noise", "n": 3}}
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "int-1", "request": {"subtype": "interrupt"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "int-1", "response": {"still_queued": []}}}}
{"t": 0, "dir": "out", "msg": {"type": "result", "subtype": "error_during_execution", "is_error": true, "duration_ms": 1, "duration_api_ms": 1, "num_turns": 1, "stop_reason": null, "total_cost_usd": 0, "usage": {}, "session_id": "s", "uuid": "r2"}}
"#;

struct Server {
    dir: TempDir,
    addr: SocketAddr,
    router: Router,
    auth: Arc<Auth>,
    manager: SessionManager,
    cwd: PathBuf,
}

impl Server {
    async fn start(script: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("script.ndjson");
        std::fs::write(&script_path, script.trim()).unwrap();
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();

        let registry = Registry::load(dir.path().join("devices.json")).unwrap();
        let now = chrono::Utc::now();
        registry
            .insert(Device {
                id: DEVICE.into(),
                label: "test".into(),
                issued_at: now,
                last_seen: now,
                revoked: false,
                last_ip: None,
            })
            .unwrap();
        let auth = Arc::new(Auth::new(registry, false));
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let manager = SessionManager::start(store, fake(&script_path))
            .await
            .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = AppState {
            auth: Arc::clone(&auth),
            pair_scheme: Arc::new("http".into()),
            pair_port: addr.port(),
            pair_extra_hosts: Arc::new(Vec::new()),
            web_dir: Arc::new(dir.path().join("web")),
            manager: manager.clone(),
            allowed_origins: Arc::new(BTreeSet::from([origin(addr)])),
        };
        let router = build_app(state);
        let service = router
            .clone()
            .into_make_service_with_connect_info::<SocketAddr>();
        tokio::spawn(async move { axum::serve(listener, service).await });
        Self {
            dir,
            addr,
            router,
            auth,
            manager,
            cwd,
        }
    }

    async fn rest(&self, method: Method, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri("/vigie2/api/sessions")
            .header(header::COOKIE, format!("{}={DEVICE}", auth::COOKIE_NAME));
        if body.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/json");
        }
        let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
        let mut request = request.body(body).unwrap();
        let peer: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        request.extensions_mut().insert(ConnectInfo(peer));
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn create_session(&self) -> SessionView {
        let (status, body) = self
            .rest(Method::POST, Some(json!({"cwd": self.cwd})))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        serde_json::from_value(body).unwrap()
    }

    async fn client(&self) -> Client {
        self.connect(Some(DEVICE), Some(&origin(self.addr)))
            .await
            .expect("connect")
    }

    async fn connect(
        &self,
        cookie: Option<&str>,
        origin: Option<&str>,
    ) -> Result<Client, tungstenite::Error> {
        let mut request = format!("ws://{}/vigie2/api/ws", self.addr)
            .into_client_request()
            .unwrap();
        if let Some(cookie) = cookie {
            let value = format!("{}={cookie}", auth::COOKIE_NAME);
            request
                .headers_mut()
                .insert(header::COOKIE, value.parse().unwrap());
        }
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert(header::ORIGIN, origin.parse().unwrap());
        }
        let (socket, _) = tokio_tungstenite::connect_async(request).await?;
        Ok(Client {
            socket,
            events: Vec::new(),
            acks: HashMap::new(),
            errors: Vec::new(),
            sessions_changed: 0,
        })
    }
}

fn origin(addr: SocketAddr) -> String {
    format!("http://{addr}")
}

fn fake(script: &Path) -> Program {
    Program {
        path: env!("CARGO_BIN_EXE_fake-claude").into(),
        leading_args: vec![script.into()],
    }
}

fn upgrade_status(result: Result<Client, tungstenite::Error>) -> StatusCode {
    match result {
        Err(tungstenite::Error::Http(response)) => response.status(),
        Err(err) => panic!("unexpected error {err}"),
        Ok(_) => panic!("upgrade accepted"),
    }
}

/// Files every message by type; events must arrive in seq order without gaps.
struct Client {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    events: Vec<WireEvent>,
    acks: HashMap<String, (bool, Option<String>)>,
    errors: Vec<(String, Option<Uuid>)>,
    sessions_changed: usize,
}

impl Client {
    async fn send(&mut self, message: &ClientMessage) {
        let text = serde_json::to_string(message).unwrap();
        self.socket.send(Message::Text(text.into())).await.unwrap();
    }

    async fn send_raw(&mut self, text: &str) {
        self.socket
            .send(Message::Text(text.to_string().into()))
            .await
            .unwrap();
    }

    /// Next frame, or `None` once the server closed the socket.
    async fn frame(&mut self) -> Option<Message> {
        let frame = tokio::time::timeout(TIMEOUT, self.socket.next())
            .await
            .expect("socket went quiet");
        match frame {
            Some(Ok(Message::Close(_))) | None => None,
            Some(Ok(frame)) => Some(frame),
            Some(Err(err)) => panic!("socket error: {err}"),
        }
    }

    async fn pump(&mut self) {
        let Some(frame) = self.frame().await else {
            panic!("socket closed");
        };
        let Message::Text(text) = frame else {
            return;
        };
        match serde_json::from_str::<ServerMessage>(text.as_str()).expect("server message") {
            ServerMessage::Event(event) => {
                if let Some(last) = self.events.last() {
                    assert_eq!(event.seq, last.seq + 1, "gap or duplicate");
                }
                self.events.push(event);
            }
            ServerMessage::Ack {
                client_id,
                ok,
                error,
            } => {
                assert!(self.acks.insert(client_id, (ok, error)).is_none());
            }
            ServerMessage::SessionsChanged => self.sessions_changed += 1,
            ServerMessage::Error { message, session } => self.errors.push((message, session)),
        }
    }

    async fn ack(&mut self, client_id: &str) -> (bool, Option<String>) {
        loop {
            if let Some(ack) = self.acks.get(client_id) {
                return ack.clone();
            }
            self.pump().await;
        }
    }

    /// Pumps until an event matching `pick` arrives; returns its index.
    async fn until(&mut self, pick: impl Fn(&WireEvent) -> bool) -> usize {
        let mut checked = 0;
        loop {
            if let Some(index) = self.events[checked..].iter().position(&pick) {
                return checked + index;
            }
            checked = self.events.len();
            self.pump().await;
        }
    }

    async fn until_error(&mut self) -> (String, Option<Uuid>) {
        while self.errors.is_empty() {
            self.pump().await;
        }
        self.errors.remove(0)
    }
}

fn host_event(event: &WireEvent, name: &str) -> bool {
    event.kind == EventKind::Host && event.payload["event"] == name
}

fn result_line(event: &WireEvent, subtype: &str) -> bool {
    event.kind == EventKind::Line
        && event.payload["type"] == "result"
        && event.payload["subtype"] == subtype
}

fn subscribe(session: Uuid, after_seq: u64) -> ClientMessage {
    ClientMessage::Subscribe { session, after_seq }
}

fn allow(session: Uuid, request_id: &str, client_id: &str) -> ClientMessage {
    ClientMessage::Answer {
        session,
        request_id: request_id.into(),
        answer: Answer::Allow {
            updated_input: None,
        },
        client_id: client_id.into(),
    }
}

#[tokio::test]
async fn upgrade_requires_a_cookie_and_an_allowed_origin() {
    let server = Server::start(SCRIPT).await;
    let origin = origin(server.addr);
    assert_eq!(
        upgrade_status(server.connect(None, Some(&origin)).await),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        upgrade_status(server.connect(Some("bogus"), Some(&origin)).await),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        upgrade_status(
            server
                .connect(Some(DEVICE), Some("http://evil.example"))
                .await
        ),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        upgrade_status(server.connect(Some(DEVICE), None).await),
        StatusCode::FORBIDDEN
    );
    server.client().await;
}

#[tokio::test]
async fn a_revoked_device_loses_its_socket() {
    let server = Server::start(SCRIPT).await;
    let mut client = server.client().await;
    server.auth.registry.revoke(DEVICE).unwrap();
    server.auth.notify_changed();
    while let Some(frame) = client.frame().await {
        assert!(!matches!(frame, Message::Text(_)), "{frame:?}");
    }
}

#[tokio::test]
async fn malformed_input_and_unknown_sessions_are_reported() {
    let server = Server::start(SCRIPT).await;
    let mut client = server.client().await;
    client.send_raw("{\"type\": \"launch_rockets\"}").await;
    let (message, session) = client.until_error().await;
    assert!(message.starts_with("malformed message"), "{message}");
    assert_eq!(session, None);

    let unknown = Uuid::new_v4();
    client.send(&subscribe(unknown, 0)).await;
    assert_eq!(client.until_error().await.1, Some(unknown));

    client
        .send(&ClientMessage::Interrupt {
            session: unknown,
            client_id: "i".into(),
        })
        .await;
    let (ok, error) = client.ack("i").await;
    assert!(!ok);
    assert!(error.unwrap().contains("not running"));
}

#[tokio::test]
async fn sessions_are_created_over_rest_and_listed() {
    let server = Server::start(SCRIPT).await;
    let mut client = server.client().await;
    let (status, _) = server
        .rest(Method::POST, Some(json!({"cwd": "relative/path"})))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let missing = server.dir.path().join("missing");
    let (status, _) = server
        .rest(Method::POST, Some(json!({"cwd": missing})))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let created = server.create_session().await;
    assert_eq!(created.cwd, server.cwd);
    assert!(created.live);
    while client.sessions_changed == 0 {
        client.pump().await;
    }
    let (status, body) = server.rest(Method::GET, None).await;
    assert_eq!(status, StatusCode::OK);
    let list: SessionList = serde_json::from_value(body).unwrap();
    assert_eq!(list.sessions.len(), 1);
    assert_eq!(list.sessions[0].id, created.id);
    assert!(list.sessions[0].live);

    client.send(&subscribe(created.id, 0)).await;
    let started = client.until(|event| host_event(event, "started")).await;
    assert_eq!(client.events[started].seq, 1);
    assert_eq!(client.events[started].payload["cwd"], json!(server.cwd));
}

#[tokio::test]
async fn two_clients_share_a_session_and_its_asks() {
    let server = Server::start(SCRIPT).await;
    let id = server.create_session().await.id;
    let mut first = server.client().await;
    let mut second = server.client().await;
    first.send(&subscribe(id, 0)).await;
    second.send(&subscribe(id, 0)).await;

    first
        .send(&ClientMessage::Send {
            session: id,
            text: "hello".into(),
            client_id: "send-1".into(),
        })
        .await;
    assert_eq!(first.ack("send-1").await, (true, None));

    let queued = first.until(|event| host_event(event, "ask_queued")).await;
    let request_id = first.events[queued].payload["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(request_id, "ask-1");
    second.until(|event| host_event(event, "ask_queued")).await;

    first.send(&allow(id, &request_id, "answer-1")).await;
    assert_eq!(first.ack("answer-1").await, (true, None));
    second.send(&allow(id, &request_id, "answer-2")).await;
    let (ok, error) = second.ack("answer-2").await;
    assert!(!ok, "a resolved ask was answered twice");
    assert!(error.unwrap().contains("no pending ask"));
    for client in [&mut first, &mut second] {
        let resolved = client
            .until(|event| host_event(event, "ask_resolved"))
            .await;
        let payload = &client.events[resolved].payload;
        assert_eq!(payload["request_id"], "ask-1");
        assert_eq!(payload["resolution"]["answer"]["kind"], "allow");
        client.until(|event| result_line(event, "success")).await;
    }

    second
        .send(&ClientMessage::SetPermissionMode {
            session: id,
            mode: "plan".into(),
            client_id: "mode-1".into(),
        })
        .await;
    assert_eq!(second.ack("mode-1").await, (true, None));
    second
        .send(&ClientMessage::Send {
            session: id,
            text: "count".into(),
            client_id: "send-2".into(),
        })
        .await;
    assert_eq!(second.ack("send-2").await, (true, None));
    second
        .until(|event| event.payload == json!({"type": "noise", "n": 3}))
        .await;
    second
        .send(&ClientMessage::Interrupt {
            session: id,
            client_id: "interrupt-1".into(),
        })
        .await;
    assert_eq!(second.ack("interrupt-1").await, (true, None));

    let first_end = first
        .until(|event| result_line(event, "error_during_execution"))
        .await;
    let second_end = second
        .until(|event| result_line(event, "error_during_execution"))
        .await;
    assert_eq!(first.events[..=first_end], second.events[..=second_end]);
    assert_eq!(first.events[0].seq, 1);
}

#[tokio::test]
async fn a_late_subscriber_replays_from_its_seq_then_follows_live() {
    let server = Server::start(SCRIPT).await;
    let id = server.create_session().await.id;
    let mut first = server.client().await;
    first.send(&subscribe(id, 0)).await;
    first
        .send(&ClientMessage::Send {
            session: id,
            text: "hello".into(),
            client_id: "send-1".into(),
        })
        .await;
    let queued = first.until(|event| host_event(event, "ask_queued")).await;

    let mut late = server.client().await;
    late.send(&subscribe(id, 2)).await;
    late.until(|event| host_event(event, "ask_queued")).await;
    assert_eq!(late.events[0].seq, 3);
    assert_eq!(late.events[..], first.events[2..=queued]);

    first.send(&allow(id, "ask-1", "answer-1")).await;
    assert_eq!(first.ack("answer-1").await, (true, None));
    let first_end = first.until(|event| result_line(event, "success")).await;
    let late_end = late.until(|event| result_line(event, "success")).await;
    assert_eq!(late.events[..=late_end], first.events[2..=first_end]);
}

#[tokio::test]
async fn subscribing_to_a_stopped_session_replays_without_spawning() {
    let server = Server::start(SCRIPT).await;
    let id = server.create_session().await.id;
    let session = server.manager.live(id).await.expect("live after create");
    session.stop().await;
    session.closed().await;
    assert!(server.manager.live(id).await.is_none());

    let mut client = server.client().await;
    client.send(&subscribe(id, 0)).await;
    client
        .until(|event| host_event(event, "child_exited"))
        .await;
    assert_eq!(client.events[0].seq, 1);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(server.manager.live(id).await.is_none());
}

#[tokio::test]
async fn a_slow_subscriber_still_gets_every_event() {
    /// Well past the session's broadcast capacity once the socket buffers fill.
    const NOISE: usize = 7000;
    let padding = "x".repeat(8 * 1024);
    // Initialization and the first user message: the noise starts once the
    // subscriber is known to be listening.
    let mut script = SCRIPT.trim().lines().take(3).collect::<Vec<_>>().join("\n");
    for index in 0..NOISE {
        script.push_str(&format!(
            "\n{{\"t\": 0, \"dir\": \"out\", \"msg\": {{\"type\": \"noise\", \"n\": {index}, \"pad\": \"{padding}\"}}}}"
        ));
    }
    let server = Server::start(&script).await;
    let id = server.create_session().await.id;
    let mut client = server.client().await;
    client.send(&subscribe(id, 0)).await;
    client.until(|event| host_event(event, "started")).await;
    client
        .send(&ClientMessage::Send {
            session: id,
            text: "hello".into(),
            client_id: "send-1".into(),
        })
        .await;
    // Not reading lets the socket, then the broadcast, back up.
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let row = server.manager.store().session(id).await.unwrap().unwrap();
            if row.last_seq > NOISE as u64 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the fake never finished writing");

    let last = client
        .until(|event| event.payload["n"] == json!(NOISE - 1))
        .await;
    let noise: Vec<u64> = client.events[..=last]
        .iter()
        .filter(|event| event.payload["type"] == "noise")
        .map(|event| event.payload["n"].as_u64().unwrap())
        .collect();
    assert_eq!(noise, (0..NOISE as u64).collect::<Vec<_>>());
}
