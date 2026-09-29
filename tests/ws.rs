//! Drives the real router over TCP with WebSocket clients, against the fake claude.

mod common;

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use chrono::{DateTime, TimeDelta, Utc};
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
use vigie2::child::{Effort, Launch};
use vigie2::manager::SessionManager;
use vigie2::protocol::PermissionMode;
use vigie2::push::{Pusher, Vapid};
use vigie2::replay::{CANNED_DEFAULT_EFFORT, CANNED_DEFAULT_MODEL};
use vigie2::server::{AppState, build_app};
use vigie2::session::{Answer, Event, EventBody, HostEvent, Line, RestartMode, StopReason};
use vigie2::store::{EventKind, SessionState, Store};
use vigie2::wire::{
    CACHE_COLD, ClientMessage, SESSION_LIVE, ServerMessage, SessionList, SessionView, WireEvent,
};

use common::{TIMEOUT, fake};

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
        registry
            .insert(Device::new(DEVICE.into(), "test".into(), Utc::now()))
            .unwrap();
        let auth = Arc::new(Auth::new(registry, false));
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let vapid = Vapid::load_or_generate(dir.path()).unwrap();
        let pusher = Arc::new(Pusher::new(vapid, store.clone(), Arc::clone(&auth)).unwrap());
        let manager = SessionManager::start(store, fake(&script_path))
            .await
            .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = AppState {
            pusher,
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
        self.request(method, "/vigie2/api/sessions", body).await
    }

    async fn request(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
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

    /// A stopped discussion started at `started_at`, with a main request if any.
    async fn seed_session(
        &self,
        started_at: DateTime<Utc>,
        request: Option<(DateTime<Utc>, Ttl)>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        let store = self.manager.store();
        store.session_started(id, &self.cwd, started_at, Some("default".into()));
        store.set_state(id, SessionState::Stopped);
        if let Some((time, ttl)) = request {
            store.record_event(id, &main_request(1, time, ttl));
        }
        store.flush().await.unwrap();
        id
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
            usage_changed: 0,
            catalog_changed: 0,
            synced: Vec::new(),
            windows: Vec::new(),
            pages: Vec::new(),
        })
    }
}

fn origin(addr: SocketAddr) -> String {
    format!("http://{addr}")
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
    usage_changed: usize,
    catalog_changed: usize,
    /// Seqs of the `synced` messages, in order.
    synced: Vec<u64>,
    /// `first_seq` and `start_reached` of the `window` messages.
    windows: Vec<(u64, bool)>,
    pages: Vec<Page>,
}

#[derive(Debug)]
struct Page {
    first_seq: u64,
    start_reached: bool,
    events: Vec<WireEvent>,
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
                // A window's prelude skips ahead; its events then follow without a gap.
                let window = self.windows.last().map_or(0, |window| window.0);
                if let Some(last) = self.events.last() {
                    if last.seq >= window || event.seq < window {
                        assert!(event.seq > last.seq, "duplicate or out of order");
                    }
                    if last.seq >= window {
                        assert_eq!(event.seq, last.seq + 1, "gap or duplicate");
                    }
                    if last.seq < window && event.seq >= window {
                        assert_eq!(event.seq, window, "window starts late");
                    }
                }
                self.events.push(event);
            }
            ServerMessage::Window {
                first_seq,
                start_reached,
                ..
            } => self.windows.push((first_seq, start_reached)),
            ServerMessage::Page {
                first_seq,
                start_reached,
                events,
                ..
            } => self.pages.push(Page {
                first_seq,
                start_reached,
                events,
            }),
            ServerMessage::Ack {
                client_id,
                ok,
                error,
            } => {
                assert!(self.acks.insert(client_id, (ok, error)).is_none());
            }
            ServerMessage::SessionsChanged => self.sessions_changed += 1,
            ServerMessage::UsageChanged => self.usage_changed += 1,
            ServerMessage::CatalogChanged => self.catalog_changed += 1,
            ServerMessage::Synced { seq, .. } => self.synced.push(seq),
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
    ClientMessage::Subscribe {
        session,
        after_seq,
        exchanges: None,
    }
}

fn allow(session: Uuid, request_id: &str, client_id: &str) -> ClientMessage {
    ClientMessage::Answer {
        session,
        request_id: request_id.into(),
        answer: Answer::Allow {
            updated_input: None,
            note: None,
            accept_edits: false,
            folder: None,
            always_for_file: false,
        },
        rewarm: false,
        client_id: client_id.into(),
    }
}

fn say(session: Uuid, text: &str, rewarm: bool, client_id: &str) -> ClientMessage {
    ClientMessage::Send {
        session,
        text: text.into(),
        rewarm,
        client_id: client_id.into(),
    }
}

fn rewarmed(message: ClientMessage) -> ClientMessage {
    match message {
        ClientMessage::Answer {
            session,
            request_id,
            answer,
            client_id,
            ..
        } => ClientMessage::Answer {
            session,
            request_id,
            answer,
            rewarm: true,
            client_id,
        },
        other => panic!("not an answer: {other:?}"),
    }
}

fn cold() -> (bool, Option<String>) {
    (false, Some(CACHE_COLD.into()))
}

#[derive(Clone, Copy)]
enum Ttl {
    OneHour,
    FiveMinutes,
}

/// A main request at `time` whose cache write used `ttl`.
fn main_request(seq: u64, time: DateTime<Utc>, ttl: Ttl) -> Event {
    let (one_hour, five_minutes) = match ttl {
        Ttl::OneHour => (7, 0),
        Ttl::FiveMinutes => (0, 7),
    };
    let raw = json!({
        "type": "assistant", "parent_tool_use_id": null, "session_id": "s", "uuid": "u",
        "message": {"id": format!("msg_{seq}"), "model": "claude-opus-5-5", "content": [],
            "usage": {"input_tokens": 3, "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 7,
                "cache_creation": {"ephemeral_1h_input_tokens": one_hour,
                    "ephemeral_5m_input_tokens": five_minutes}}}
    })
    .to_string();
    Event {
        seq,
        time,
        body: EventBody::Line(Box::new(Line {
            parsed: Some(serde_json::from_str(&raw).unwrap()),
            raw,
        })),
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

/// A turn reporting rate limits and one assistant message.
const USAGE_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "in", "msg": {"type": "user", "message": {"role": "user", "content": "hello"}, "parent_tool_use_id": null, "session_id": "s"}}
{"t": 0, "dir": "out", "msg": {"type": "assistant", "message": {"id": "msg_1", "model": "claude-opus-5-5", "content": [{"type": "text", "text": "hi"}], "usage": {"input_tokens": 10, "output_tokens": 5}}, "parent_tool_use_id": null, "session_id": "s", "uuid": "a1"}}
{"t": 0, "dir": "out", "msg": {"type": "rate_limit_event", "rate_limit_info": {"status": "allowed", "resetsAt": 4102444800, "rateLimitType": "five_hour", "unifiedWindows": {"five_hour": {"utilization": 0.07, "resetsAt": 4102444800}, "seven_day": {"utilization": 0.51, "resetsAt": 4102531200}}}, "session_id": "s", "uuid": "r0"}}
{"t": 0, "dir": "out", "msg": {"type": "result", "subtype": "success", "is_error": false, "result": "hi", "duration_ms": 1, "duration_api_ms": 1, "num_turns": 1, "stop_reason": "end_turn", "total_cost_usd": 0, "usage": {}, "session_id": "s", "uuid": "r1"}}
"#;

#[tokio::test]
async fn usage_is_announced_after_a_turn_and_served_over_rest() {
    let server = Server::start(USAGE_SCRIPT).await;
    let mut client = server.client().await;
    let id = server.create_session().await.id;
    client
        .send(&ClientMessage::Send {
            session: id,
            text: "hello".into(),
            rewarm: false,
            client_id: "send-1".into(),
        })
        .await;
    assert_eq!(client.ack("send-1").await, (true, None));
    // One for the rate limit line, one for the finished turn.
    while client.usage_changed < 2 {
        client.pump().await;
    }

    let (status, body) = server.request(Method::GET, "/vigie2/api/usage", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sessions"][0]["id"], json!(id));
    assert_eq!(
        body["sessions"][0]["model_mix"],
        json!([{"family": "opus", "share": 1.0}])
    );
    assert_eq!(body["sessions"][0]["bypass"], false);
    assert!(body["history"]["readings"].as_u64().unwrap() >= 2, "{body}");
    assert!(body["facts"].is_array());
    assert!(body["newest_reading"].is_string());
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
            rewarm: false,
            client_id: "send-1".into(),
        })
        .await;
    assert_eq!(first.ack("send-1").await, (true, None));

    let queued = first.until(|event| host_event(event, "ask_queued")).await;
    let sent: Vec<&WireEvent> = first.events[..queued]
        .iter()
        .filter(|event| host_event(event, "user_sent"))
        .collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].payload["text"], "hello");
    assert_eq!(sent[0].payload["client_id"], "send-1");
    let noise = first
        .events
        .iter()
        .position(|event| event.payload == json!({"type": "noise", "n": 1}))
        .expect("first CLI line after the message");
    assert!(sent[0].seq < first.events[noise].seq);
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
    // The launch reported default first; the change follows.
    let modes: Vec<&Value> = second.events[..]
        .iter()
        .filter(|event| host_event(event, "permission_mode"))
        .map(|event| &event.payload["mode"])
        .collect();
    assert_eq!(modes.first().copied(), Some(&json!("default")));
    second
        .until(|event| host_event(event, "permission_mode") && event.payload["mode"] == "plan")
        .await;
    second
        .send(&ClientMessage::Send {
            session: id,
            text: "count".into(),
            rewarm: false,
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
            rewarm: false,
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
    // The whole history comes before `synced`, which names its last seq.
    while client.synced.is_empty() {
        client.pump().await;
    }
    let last = client.events.last().unwrap().seq;
    assert_eq!(client.synced, vec![last]);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(server.manager.live(id).await.is_none());
}

#[tokio::test]
async fn a_blank_restart_reaches_subscribers_without_a_child() {
    let server = Server::start(SCRIPT).await;
    let id = server.create_session().await.id;
    let mut client = server.client().await;
    client.send(&subscribe(id, 0)).await;
    client.until(|event| host_event(event, "started")).await;
    client
        .send(&ClientMessage::Restart {
            session: id,
            mode: vigie2::session::RestartMode::Blank,
            text: None,
            client_id: "restart-1".into(),
        })
        .await;
    assert_eq!(client.ack("restart-1").await, (true, None));
    client.until(|event| host_event(event, "restarted")).await;
    assert!(server.manager.live(id).await.is_none());
    let restarted = client.events.last().unwrap();
    assert_eq!(restarted.payload["mode"], "blank");
    assert_eq!(restarted.payload.get("seed"), None);
}

#[tokio::test]
async fn a_stopped_session_stores_its_mode_and_the_next_send_launches_with_it() {
    let server = Server::start(SCRIPT).await;
    let id = server.create_session().await.id;
    let session = server.manager.live(id).await.expect("live after create");
    session.stop().await;
    assert!(server.manager.live(id).await.is_none());

    let mut client = server.client().await;
    let changes_before = client.sessions_changed;
    client
        .send(&ClientMessage::SetPermissionMode {
            session: id,
            mode: "plan".into(),
            client_id: "mode-1".into(),
        })
        .await;
    assert_eq!(client.ack("mode-1").await, (true, None));
    assert!(
        server.manager.live(id).await.is_none(),
        "setting the mode spawned"
    );
    while client.sessions_changed == changes_before {
        client.pump().await;
    }
    let (status, body) = server.rest(Method::GET, None).await;
    assert_eq!(status, StatusCode::OK);
    let list: SessionList = serde_json::from_value(body).unwrap();
    assert_eq!(list.sessions[0].permission_mode, Some(PermissionMode::Plan));
    assert!(!list.sessions[0].live);

    let stopped_at = list.sessions[0].last_seq;
    client.send(&subscribe(id, stopped_at)).await;
    client
        .send(&ClientMessage::Send {
            session: id,
            text: "hello".into(),
            rewarm: false,
            client_id: "send-1".into(),
        })
        .await;
    assert_eq!(client.ack("send-1").await, (true, None));
    let changed = client
        .until(|event| host_event(event, "permission_mode"))
        .await;
    assert_eq!(client.events[changed].payload["mode"], "plan");
    assert!(
        client.events[..changed]
            .iter()
            .any(|event| host_event(event, "started"))
    );
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
            rewarm: false,
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

#[tokio::test]
async fn a_discussion_idle_for_over_an_hour_takes_a_message_only_with_rewarm() {
    let server = Server::start(SCRIPT).await;
    let id = server
        .seed_session(Utc::now() - TimeDelta::hours(2), None)
        .await;
    let mut client = server.client().await;
    client.send(&subscribe(id, 0)).await;

    client.send(&say(id, "hello", false, "send-1")).await;
    assert_eq!(client.ack("send-1").await, cold());
    assert!(
        server.manager.live(id).await.is_none(),
        "a refused send spawned"
    );
    let row = server.manager.store().session(id).await.unwrap().unwrap();
    assert_eq!(row.last_seq, 0, "a refused send was recorded");

    client.send(&say(id, "hello", true, "send-2")).await;
    assert_eq!(client.ack("send-2").await, (true, None));
    let sent = client.until(|event| host_event(event, "user_sent")).await;
    assert_eq!(client.events[sent].payload["client_id"], "send-2");
    client.until(|event| host_event(event, "ask_queued")).await;
}

#[tokio::test]
async fn an_expired_cache_needs_rewarm_to_send_and_answer_but_not_to_interrupt_or_restart() {
    // The same exchange, without the permission mode change.
    let script = SCRIPT
        .lines()
        .filter(|line| !line.contains("mode-1"))
        .collect::<Vec<_>>()
        .join("\n");
    let server = Server::start(&script).await;
    // Idle for 10 minutes only, past its 5 minute TTL.
    let request_at = Utc::now() - TimeDelta::minutes(10);
    let id = server
        .seed_session(request_at, Some((request_at, Ttl::FiveMinutes)))
        .await;
    let mut client = server.client().await;
    client.send(&subscribe(id, 0)).await;

    client.send(&say(id, "hello", false, "send-1")).await;
    assert_eq!(client.ack("send-1").await, cold());
    assert!(
        server.manager.live(id).await.is_none(),
        "a refused send spawned"
    );
    client.send(&say(id, "hello", true, "send-2")).await;
    assert_eq!(client.ack("send-2").await, (true, None));
    client.until(|event| host_event(event, "ask_queued")).await;

    // The fake makes no main request, so the cache stays cold.
    client.send(&allow(id, "ask-1", "answer-1")).await;
    assert_eq!(client.ack("answer-1").await, cold());
    assert!(
        !client
            .events
            .iter()
            .any(|event| host_event(event, "ask_resolved")),
        "a refused answer resolved the ask"
    );
    client.send(&rewarmed(allow(id, "ask-1", "answer-2"))).await;
    assert_eq!(client.ack("answer-2").await, (true, None));
    client.until(|event| result_line(event, "success")).await;

    client.send(&say(id, "count", false, "send-3")).await;
    assert_eq!(client.ack("send-3").await, cold());
    client.send(&say(id, "count", true, "send-4")).await;
    assert_eq!(client.ack("send-4").await, (true, None));
    client
        .until(|event| event.payload == json!({"type": "noise", "n": 3}))
        .await;
    client
        .send(&ClientMessage::Interrupt {
            session: id,
            client_id: "interrupt-1".into(),
        })
        .await;
    assert_eq!(client.ack("interrupt-1").await, (true, None));
    client
        .until(|event| result_line(event, "error_during_execution"))
        .await;

    client
        .send(&ClientMessage::Restart {
            session: id,
            mode: RestartMode::Blank,
            text: None,
            client_id: "restart-1".into(),
        })
        .await;
    assert_eq!(client.ack("restart-1").await, (true, None));
    client.until(|event| host_event(event, "restarted")).await;
    // A restart forgets the cache and counts as activity.
    client.send(&say(id, "hello", false, "send-5")).await;
    assert_eq!(client.ack("send-5").await, (true, None));
}

fn kill(session: Uuid, reason: StopReason, client_id: &str) -> ClientMessage {
    ClientMessage::Kill {
        session,
        reason,
        client_id: client_id.into(),
    }
}

fn archive(session: Uuid, client_id: &str) -> ClientMessage {
    ClientMessage::Archive {
        session,
        client_id: client_id.into(),
    }
}

fn restore(session: Uuid, client_id: &str) -> ClientMessage {
    ClientMessage::Restore {
        session,
        client_id: client_id.into(),
    }
}

impl Server {
    async fn view(&self, id: Uuid) -> SessionView {
        let (status, body) = self.rest(Method::GET, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let list: SessionList = serde_json::from_value(body).unwrap();
        list.sessions
            .into_iter()
            .find(|session| session.id == id)
            .expect("listed")
    }
}

impl Client {
    /// Acks `message`, then waits for the `sessions_changed` it announces.
    async fn command_announced(
        &mut self,
        message: &ClientMessage,
        client_id: &str,
    ) -> (bool, Option<String>) {
        let before = self.sessions_changed;
        self.send(message).await;
        let ack = self.ack(client_id).await;
        while self.sessions_changed == before {
            self.pump().await;
        }
        ack
    }
}

#[tokio::test]
async fn kill_stops_a_live_session_and_keeps_it_listed() {
    let server = Server::start(SCRIPT).await;
    let id = server.create_session().await.id;
    let mut client = server.client().await;
    assert_eq!(
        client
            .command_announced(&kill(id, StopReason::Kill, "kill-1"), "kill-1")
            .await,
        (true, None)
    );
    assert!(server.manager.live(id).await.is_none());
    let view = server.view(id).await;
    assert!(!view.live);
    assert_eq!(view.state, SessionState::Stopped);
    assert_eq!(view.archived_at, None);

    // Not live any more: acked without effect.
    assert_eq!(
        client
            .command_announced(&kill(id, StopReason::Kill, "kill-2"), "kill-2")
            .await,
        (true, None)
    );
    client
        .send(&kill(Uuid::new_v4(), StopReason::Kill, "kill-3"))
        .await;
    let (ok, error) = client.ack("kill-3").await;
    assert!(!ok);
    assert!(error.unwrap().contains("unknown session"));

    // The next send resumes it.
    client.send(&say(id, "hello", false, "send-1")).await;
    assert_eq!(client.ack("send-1").await, (true, None));
    assert!(server.manager.live(id).await.is_some());
}

#[tokio::test]
async fn archive_is_refused_while_live_and_restore_undoes_it() {
    let server = Server::start(SCRIPT).await;
    let id = server.create_session().await.id;
    let mut client = server.client().await;
    client.send(&archive(id, "archive-1")).await;
    assert_eq!(
        client.ack("archive-1").await,
        (false, Some(SESSION_LIVE.into()))
    );
    assert_eq!(server.view(id).await.archived_at, None);

    client.send(&kill(id, StopReason::Kill, "kill-1")).await;
    assert_eq!(client.ack("kill-1").await, (true, None));
    assert_eq!(
        client
            .command_announced(&archive(id, "archive-2"), "archive-2")
            .await,
        (true, None)
    );
    let view = server.view(id).await;
    assert!(view.archived_at.is_some());
    assert!(!view.live);

    assert_eq!(
        client
            .command_announced(&restore(id, "restore-1"), "restore-1")
            .await,
        (true, None)
    );
    assert_eq!(server.view(id).await.archived_at, None);
    assert!(server.manager.live(id).await.is_none(), "restoring spawned");
}

#[tokio::test]
async fn a_restart_or_a_send_unarchives() {
    let server = Server::start(SCRIPT).await;
    let started_at = Utc::now() - TimeDelta::hours(2);
    let restarted = server.seed_session(started_at, None).await;
    let rewarmed = server.seed_session(started_at, None).await;
    let refused = server.seed_session(started_at, None).await;
    let mut client = server.client().await;
    for (index, id) in [restarted, rewarmed, refused].into_iter().enumerate() {
        let client_id = format!("archive-{index}");
        client.send(&archive(id, &client_id)).await;
        assert_eq!(client.ack(&client_id).await, (true, None));
        assert!(server.view(id).await.archived_at.is_some());
    }

    client
        .send(&ClientMessage::Restart {
            session: restarted,
            mode: RestartMode::Blank,
            text: None,
            client_id: "restart-1".into(),
        })
        .await;
    assert_eq!(client.ack("restart-1").await, (true, None));
    assert_eq!(server.view(restarted).await.archived_at, None);

    client.send(&say(rewarmed, "hello", true, "send-1")).await;
    assert_eq!(client.ack("send-1").await, (true, None));
    assert_eq!(server.view(rewarmed).await.archived_at, None);

    // Unarchived first, then refused on its cold cache.
    client.send(&say(refused, "hello", false, "send-2")).await;
    assert_eq!(client.ack("send-2").await, cold());
    assert_eq!(server.view(refused).await.archived_at, None);
    assert!(server.manager.live(refused).await.is_none());
}

fn set_settings(
    session: Uuid,
    model: Option<Option<&str>>,
    effort: Option<Option<&str>>,
    client_id: &str,
) -> ClientMessage {
    ClientMessage::SetSettings {
        session,
        model: model.map(|model| model.map(str::to_owned)),
        effort: effort.map(|effort| effort.map(str::to_owned)),
        client_id: client_id.into(),
    }
}

/// SCRIPT without its permission mode change.
fn script_without_mode_change() -> String {
    SCRIPT
        .lines()
        .filter(|line| !line.contains("mode-1"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn set_settings_reads_absent_and_null_fields_apart() {
    let absent: ClientMessage = serde_json::from_value(json!({"type": "set_settings",
        "session": Uuid::nil(), "effort": "high", "client_id": "c"}))
    .unwrap();
    assert_eq!(
        absent,
        set_settings(Uuid::nil(), None, Some(Some("high")), "c")
    );
    let null: ClientMessage = serde_json::from_value(json!({"type": "set_settings",
        "session": Uuid::nil(), "model": null, "client_id": "c"}))
    .unwrap();
    assert_eq!(null, set_settings(Uuid::nil(), Some(None), None, "c"));
}

#[tokio::test]
async fn a_changed_model_refuses_a_plain_send_and_takes_a_rewarm_one() {
    let server = Server::start(SCRIPT).await;
    let id = server.create_session().await.id;
    let mut client = server.client().await;
    client.send(&subscribe(id, 0)).await;
    client.until(|event| host_event(event, "started")).await;
    assert_eq!(
        client
            .command_announced(
                &set_settings(id, Some(Some("sonnet")), None, "settings-1"),
                "settings-1"
            )
            .await,
        (true, None)
    );
    let view = server.view(id).await;
    assert_eq!(view.model.as_deref(), Some("sonnet"));
    assert_eq!(view.effort, None);
    assert!(view.settings_changed_at.is_some());
    assert!(view.live, "a live child keeps running on the new model");
    assert_eq!(view.applied_model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(view.applied_effort.as_deref(), Some("medium"));

    client.send(&say(id, "hello", false, "send-1")).await;
    assert_eq!(client.ack("send-1").await, cold());
    client.send(&say(id, "hello", true, "send-2")).await;
    assert_eq!(client.ack("send-2").await, (true, None));
    client.until(|event| host_event(event, "ask_queued")).await;

    client
        .send(&set_settings(id, Some(Some("sonnet")), None, "settings-2"))
        .await;
    assert_eq!(client.ack("settings-2").await, (true, None));
    client
        .send(&set_settings(id, None, Some(Some("extreme")), "settings-3"))
        .await;
    let (ok, error) = client.ack("settings-3").await;
    assert!(!ok);
    assert!(error.unwrap().contains("unknown effort level"));
}

#[tokio::test]
async fn unchanged_settings_keep_sends_allowed() {
    let server = Server::start(SCRIPT).await;
    let id = server.create_session().await.id;
    let mut client = server.client().await;
    client
        .send(&set_settings(id, Some(None), Some(None), "settings-1"))
        .await;
    assert_eq!(client.ack("settings-1").await, (true, None));
    let view = server.view(id).await;
    assert_eq!(view.settings_changed_at, None);
    client.send(&say(id, "hello", false, "send-1")).await;
    assert_eq!(client.ack("send-1").await, (true, None));
}

#[tokio::test]
async fn a_settings_change_interrupts_the_running_turn() {
    let server = Server::start(&script_without_mode_change()).await;
    let id = server.create_session().await.id;
    let mut client = server.client().await;
    client.send(&subscribe(id, 0)).await;
    client.send(&say(id, "hello", false, "send-1")).await;
    assert_eq!(client.ack("send-1").await, (true, None));
    client.until(|event| host_event(event, "ask_queued")).await;
    client.send(&allow(id, "ask-1", "answer-1")).await;
    assert_eq!(client.ack("answer-1").await, (true, None));
    client.until(|event| result_line(event, "success")).await;
    client.send(&say(id, "count", false, "send-2")).await;
    assert_eq!(client.ack("send-2").await, (true, None));
    client
        .until(|event| event.payload == json!({"type": "noise", "n": 3}))
        .await;

    client
        .send(&set_settings(id, None, Some(Some("max")), "settings-1"))
        .await;
    assert_eq!(client.ack("settings-1").await, (true, None));
    client
        .until(|event| result_line(event, "error_during_execution"))
        .await;
    assert_eq!(server.view(id).await.effort, Some(Effort::Max));
}

#[tokio::test]
async fn stored_settings_are_passed_at_launch() {
    let server = Server::start(SCRIPT).await;
    let id = server
        .seed_session(Utc::now() - TimeDelta::minutes(5), None)
        .await;
    let mut client = server.client().await;
    assert_eq!(
        client
            .command_announced(
                &set_settings(id, Some(Some("sonnet")), Some(Some("max")), "settings-1"),
                "settings-1"
            )
            .await,
        (true, None)
    );
    assert!(
        server.manager.live(id).await.is_none(),
        "storing settings spawned"
    );
    let stopped_at = server.view(id).await.last_seq;
    client.send(&subscribe(id, stopped_at)).await;
    client.send(&say(id, "hello", false, "send-1")).await;
    assert_eq!(client.ack("send-1").await, cold());
    client.send(&say(id, "hello", true, "send-2")).await;
    assert_eq!(client.ack("send-2").await, (true, None));
    client.until(|event| host_event(event, "ask_queued")).await;
    client.send(&kill(id, StopReason::Revoke, "kill-1")).await;
    assert_eq!(client.ack("kill-1").await, (true, None));
    let exited = client
        .until(|event| host_event(event, "child_exited"))
        .await;
    assert_eq!(client.events[exited].payload["reason"], "revoke");
    let args = client.events[exited].payload["stderr_tail"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .find(|line| line.starts_with("fake-claude args:"))
        .expect("the fake reports its flags")
        .to_string();
    assert!(args.contains("--model sonnet --effort max"), "{args}");
    let view = server.view(id).await;
    assert_eq!(view.model.as_deref(), Some("sonnet"));
    assert_eq!(view.effort, Some(Effort::Max));
}

#[tokio::test]
async fn clear_restarts_blank_whatever_the_cache() {
    let server = Server::start(SCRIPT).await;
    let id = server
        .seed_session(Utc::now() - TimeDelta::hours(2), None)
        .await;
    let mut client = server.client().await;
    client.send(&subscribe(id, 0)).await;
    client.send(&say(id, "hello", false, "send-1")).await;
    assert_eq!(client.ack("send-1").await, cold());
    client
        .send(&ClientMessage::Restart {
            session: id,
            mode: RestartMode::Clear,
            text: None,
            client_id: "clear-1".into(),
        })
        .await;
    assert_eq!(client.ack("clear-1").await, (true, None));
    let restarted = client.until(|event| host_event(event, "restarted")).await;
    assert_eq!(client.events[restarted].payload["mode"], "clear");
    assert_eq!(client.events[restarted].payload.get("seed"), None);
    assert!(server.manager.live(id).await.is_none());
    client.send(&say(id, "hello", false, "send-2")).await;
    assert_eq!(client.ack("send-2").await, (true, None));
}

#[tokio::test]
async fn a_restart_starts_on_the_stored_settings_and_ends_the_settings_change() {
    let server = Server::start(SCRIPT).await;
    let id = server.create_session().await.id;
    let mut client = server.client().await;
    client
        .send(&set_settings(id, Some(Some("haiku")), None, "settings-1"))
        .await;
    assert_eq!(client.ack("settings-1").await, (true, None));
    assert!(server.view(id).await.settings_changed_at.is_some());
    client
        .send(&ClientMessage::Restart {
            session: id,
            mode: RestartMode::Blank,
            text: Some("hello".into()),
            client_id: "restart-1".into(),
        })
        .await;
    assert_eq!(client.ack("restart-1").await, (true, None));
    let view = server.view(id).await;
    assert_eq!(view.settings_changed_at, None);
    assert_eq!(view.model.as_deref(), Some("haiku"));
    let session = server
        .manager
        .live(id)
        .await
        .expect("launched with the text");
    session.stop().await;
    let row = server.manager.store().session(id).await.unwrap().unwrap();
    let events = server.manager.events_after(id, 0, u32::MAX).await.unwrap();
    let exit = events
        .iter()
        .rev()
        .find(|event| event.kind == EventKind::Host && event.payload.contains("child_exited"))
        .expect("the child exited");
    assert!(exit.payload.contains("--model haiku"), "{}", exit.payload);
    assert!(row.last_seq >= exit.seq);
}

#[tokio::test]
async fn warm_and_new_discussions_take_a_message_without_rewarm() {
    let server = Server::start(SCRIPT).await;
    let request_at = Utc::now() - TimeDelta::minutes(50);
    let warm = server
        .seed_session(
            Utc::now() - TimeDelta::hours(3),
            Some((request_at, Ttl::OneHour)),
        )
        .await;
    let recent = server
        .seed_session(Utc::now() - TimeDelta::minutes(30), None)
        .await;
    let new = server.create_session().await.id;
    let mut client = server.client().await;
    for (index, id) in [warm, recent, new].into_iter().enumerate() {
        let client_id = format!("send-{index}");
        client.send(&say(id, "hello", false, &client_id)).await;
        assert_eq!(client.ack(&client_id).await, (true, None), "{index}");
    }
}

/// Initialization with a catalog; the fake then waits for stdin to close.
const CATALOG_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}, "models": [{"value": "opus", "resolvedModel": "claude-opus-5-5", "displayName": "Opus 5.5"}], "commands": [{"name": "review"}]}}}}
"#;

#[tokio::test]
async fn the_shared_catalog_is_served_and_announced() {
    let server = Server::start(CATALOG_SCRIPT).await;
    let mut client = server.client().await;
    let (status, body) = server
        .request(Method::GET, "/vigie2/api/catalog", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"models": null, "commands": null, "terminal_commands": null, "defaults": null})
    );
    server.create_session().await;
    let body = loop {
        client.pump().await;
        if client.catalog_changed == 0 {
            continue;
        }
        let (_, body) = server
            .request(Method::GET, "/vigie2/api/catalog", None)
            .await;
        if !body["models"].is_null() && !body["defaults"].is_null() {
            break body;
        }
    };
    assert_eq!(body["models"][0]["value"], "opus");
    assert_eq!(body["commands"][0]["name"], "review");
    assert_eq!(
        body["defaults"],
        json!({"model": CANNED_DEFAULT_MODEL, "effort": CANNED_DEFAULT_EFFORT})
    );
}

/// A stopped discussion of six exchanges, with a mode change and a restart in
/// between; returns its id and the stored events as sent.
async fn long_discussion(server: &Server) -> (Uuid, Vec<Event>) {
    let id = server.seed_session(Utc::now(), None).await;
    let started = HostEvent::Started {
        launch: Launch::New(id),
        cwd: server.cwd.clone(),
    };
    let mut bodies = vec![
        EventBody::Host(started.clone()),
        EventBody::Host(HostEvent::Catalog {
            models: Vec::new(),
            commands: Vec::new(),
        }),
        EventBody::Host(HostEvent::PermissionMode {
            mode: "default".into(),
        }),
    ];
    for exchange in 0..6 {
        bodies.push(EventBody::Host(HostEvent::UserSent {
            text: format!("message {exchange}"),
            client_id: None,
            mid_turn: false,
        }));
        for raw in [
            json!({"type": "assistant", "n": exchange}),
            json!({"type": "result", "subtype": "success", "n": exchange}),
        ] {
            bodies.push(EventBody::Line(Box::new(Line {
                raw: raw.to_string(),
                parsed: None,
            })));
        }
        if exchange == 1 {
            bodies.push(EventBody::Host(HostEvent::PermissionMode {
                mode: "plan".into(),
            }));
        }
        if exchange == 3 {
            bodies.push(EventBody::Host(HostEvent::Restarted {
                mode: RestartMode::Blank,
                exchanges: 0,
                total_exchanges: 4,
                seed: None,
            }));
            bodies.push(EventBody::Host(started.clone()));
        }
    }
    let store = server.manager.store();
    let events: Vec<Event> = bodies
        .into_iter()
        .zip(1..)
        .map(|(body, seq)| Event {
            seq,
            time: Utc::now(),
            body,
        })
        .collect();
    for event in &events {
        store.record_event(id, event);
    }
    store.flush().await.unwrap();
    (id, events)
}

fn host_name(event: &Event) -> Option<String> {
    match &event.body {
        EventBody::Host(host) => serde_json::to_value(host).unwrap()["event"]
            .as_str()
            .map(str::to_string),
        EventBody::Line(_) => None,
    }
}

/// Seq of the `exchanges`-th latest `user_sent` below `before`, or 1.
fn expected_start(events: &[Event], before: u64, exchanges: usize) -> u64 {
    events
        .iter()
        .filter(|event| event.seq < before && host_name(event).as_deref() == Some("user_sent"))
        .rev()
        .nth(exchanges - 1)
        .map_or(1, |event| event.seq)
}

fn expected_prelude(events: &[Event], start: u64) -> Vec<u64> {
    let mut seqs: Vec<u64> = ["started", "permission_mode", "catalog", "restarted"]
        .into_iter()
        .filter_map(|name| {
            events
                .iter()
                .filter(|event| event.seq < start && host_name(event).as_deref() == Some(name))
                .map(|event| event.seq)
                .max()
        })
        .collect();
    seqs.sort_unstable();
    seqs
}

fn seqs(events: &[WireEvent]) -> Vec<u64> {
    events.iter().map(|event| event.seq).collect()
}

#[tokio::test]
async fn a_windowed_subscribe_sends_the_prelude_then_the_last_exchanges() {
    let server = Server::start(SCRIPT).await;
    let (id, events) = long_discussion(&server).await;
    let last = events.last().unwrap().seq;
    let mut client = server.client().await;
    client
        .send(&ClientMessage::Subscribe {
            session: id,
            after_seq: 0,
            exchanges: Some(2),
        })
        .await;
    while client.synced.is_empty() {
        client.pump().await;
    }
    let first = expected_start(&events, u64::MAX, 2);
    assert!(first > 1);
    assert_eq!(client.windows, [(first, false)]);
    let prelude = expected_prelude(&events, first);
    // The mode change and the restart both precede the last two exchanges.
    assert_eq!(prelude.len(), 4, "{prelude:?}");
    let mut expected = prelude;
    expected.extend(first..=last);
    assert_eq!(seqs(&client.events), expected);
    assert_eq!(client.synced, [last]);
    let sent = client.events.iter().find(|event| event.seq == first);
    assert!(sent.is_some_and(|event| host_event(event, "user_sent")));
}

#[tokio::test]
async fn history_pages_reach_the_start_of_the_discussion() {
    let server = Server::start(SCRIPT).await;
    let (id, events) = long_discussion(&server).await;
    let mut client = server.client().await;
    let mut before = expected_start(&events, u64::MAX, 2);
    let mut loaded = Vec::new();
    loop {
        client
            .send(&ClientMessage::History {
                session: id,
                before_seq: before,
                exchanges: 2,
            })
            .await;
        let count = client.pages.len();
        while client.pages.len() == count {
            client.pump().await;
        }
        let page = client.pages.last().unwrap();
        assert_eq!(page.first_seq, expected_start(&events, before, 2));
        let mut expected = expected_prelude(&events, page.first_seq);
        expected.extend(page.first_seq..before);
        assert_eq!(seqs(&page.events), expected);
        assert!(page.events.iter().all(|event| event.session == id));
        loaded.splice(
            0..0,
            page.events
                .iter()
                .filter(|event| event.seq >= page.first_seq)
                .cloned(),
        );
        before = page.first_seq;
        if page.start_reached {
            break;
        }
    }
    assert_eq!(client.pages.len(), 3);
    assert_eq!(before, 1);
    assert_eq!(
        seqs(&loaded),
        (1..expected_start(&events, u64::MAX, 2)).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_resubscribe_after_a_reconnect_sends_only_what_came_after() {
    let server = Server::start(SCRIPT).await;
    let (id, events) = long_discussion(&server).await;
    let last = events.last().unwrap().seq;
    let mut client = server.client().await;
    client
        .send(&ClientMessage::Subscribe {
            session: id,
            after_seq: 5,
            exchanges: Some(1),
        })
        .await;
    while client.synced.is_empty() {
        client.pump().await;
    }
    assert!(client.windows.is_empty());
    assert_eq!(seqs(&client.events), (6..=last).collect::<Vec<_>>());
    assert_eq!(client.synced, [last]);
}
