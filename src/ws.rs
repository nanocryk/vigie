//! The client WebSocket: session subscriptions and commands. Protocol in `wire`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use axum::{
    Extension,
    extract::{
        ConnectInfo, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Auth, AuthedDevice};
use crate::manager::SessionManager;
use crate::session::{Event, EventBody, HostEvent, Session};
use crate::wire::{ClientMessage, ServerMessage, WireEvent};

/// Stored events read per history query.
const PAGE: u32 = 500;
/// Messages queued for one socket before producers wait.
const OUTGOING_CAPACITY: usize = 1024;

pub async fn handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Extension(device): Extension<AuthedDevice>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim_end_matches('/'));
    let Some(origin) = origin else {
        warn!(ip = %peer.ip(), "ws upgrade missing Origin header");
        return (StatusCode::FORBIDDEN, "missing origin").into_response();
    };
    if !state.allowed_origins.contains(origin) {
        warn!(ip = %peer.ip(), %origin, "ws upgrade origin not in allowlist");
        return (StatusCode::FORBIDDEN, "bad origin").into_response();
    }
    info!(device = %device.label, ip = %peer.ip(), "ws connected");
    let manager = state.manager.clone();
    let auth = Arc::clone(&state.auth);
    ws.on_upgrade(move |socket| async move {
        let label = device.label.clone();
        if let Err(err) = run(socket, manager, auth, device).await {
            debug!("ws ended: {err:#}");
        }
        info!(device = %label, "ws disconnected");
    })
}

async fn run(
    mut socket: WebSocket,
    manager: SessionManager,
    auth: Arc<Auth>,
    device: AuthedDevice,
) -> Result<()> {
    let (out, mut outgoing) = mpsc::channel(OUTGOING_CAPACITY);
    let mut connection = Connection {
        manager: manager.clone(),
        out,
        subscriptions: HashMap::new(),
    };
    let mut devices = auth.subscribe_changes();
    let mut changes = manager.subscribe_changes();
    let mut usage_changes = manager.subscribe_usage();
    let result = loop {
        tokio::select! {
            frame = socket.recv() => match frame {
                Some(Ok(Message::Text(text))) => {
                    if let Some(reply) = connection.on_text(text.as_str())
                        && let Err(err) = send(&mut socket, &reply).await
                    {
                        break Err(err);
                    }
                }
                Some(Ok(Message::Close(_))) | None => break Ok(()),
                Some(Ok(_)) => {}
                Some(Err(err)) => break Err(err.into()),
            },
            Some(message) = outgoing.recv() => {
                if let Err(err) = send(&mut socket, &message).await {
                    break Err(err);
                }
            }
            // Auth is checked at upgrade only, so every registry change
            // re-verifies the device and a revoke closes the socket.
            tick = devices.recv() => match tick {
                Ok(()) | Err(RecvError::Lagged(_)) => {
                    if auth.registry.find_active(&device.id).is_none() {
                        info!(device = %device.label, "ws closing: device revoked");
                        let _ = socket.send(Message::Close(None)).await;
                        break Ok(());
                    }
                }
                Err(RecvError::Closed) => break Ok(()),
            },
            change = changes.recv() => match change {
                Ok(_) | Err(RecvError::Lagged(_)) => {
                    if let Err(err) = send(&mut socket, &ServerMessage::SessionsChanged).await {
                        break Err(err);
                    }
                }
                Err(RecvError::Closed) => break Ok(()),
            },
            change = usage_changes.recv() => match change {
                Ok(()) | Err(RecvError::Lagged(_)) => {
                    if let Err(err) = send(&mut socket, &ServerMessage::UsageChanged).await {
                        break Err(err);
                    }
                }
                Err(RecvError::Closed) => break Ok(()),
            },
        }
    };
    connection.close();
    result
}

async fn send(socket: &mut WebSocket, message: &ServerMessage) -> Result<()> {
    let text = serde_json::to_string(message)?;
    socket.send(Message::Text(text.into())).await?;
    Ok(())
}

struct Connection {
    manager: SessionManager,
    out: mpsc::Sender<ServerMessage>,
    subscriptions: HashMap<Uuid, JoinHandle<()>>,
}

impl Connection {
    /// Returns the immediate reply, if any. Never waits on `out`, which only the
    /// socket loop calling this drains.
    fn on_text(&mut self, text: &str) -> Option<ServerMessage> {
        let message = match serde_json::from_str::<ClientMessage>(text) {
            Ok(message) => message,
            Err(err) => {
                return Some(ServerMessage::Error {
                    message: format!("malformed message: {err}"),
                    session: None,
                });
            }
        };
        match message {
            ClientMessage::Subscribe { session, after_seq } => {
                let task = tokio::spawn(stream_session(
                    self.manager.clone(),
                    session,
                    after_seq,
                    self.out.clone(),
                ));
                if let Some(previous) = self.subscriptions.insert(session, task) {
                    previous.abort();
                }
            }
            ClientMessage::Unsubscribe { session } => {
                if let Some(task) = self.subscriptions.remove(&session) {
                    task.abort();
                }
            }
            command => {
                let manager = self.manager.clone();
                let out = self.out.clone();
                tokio::spawn(async move {
                    let (client_id, result) = execute(&manager, command).await;
                    let _ = out
                        .send(ServerMessage::Ack {
                            client_id,
                            ok: result.is_ok(),
                            error: result.err().map(|err| format!("{err:#}")),
                        })
                        .await;
                });
            }
        }
        None
    }

    fn close(&mut self) {
        for (_, task) in self.subscriptions.drain() {
            task.abort();
        }
    }
}

/// Runs a command other than a subscription; returns its `client_id` and outcome.
async fn execute(manager: &SessionManager, command: ClientMessage) -> (String, Result<()>) {
    match command {
        ClientMessage::Send {
            session,
            text,
            client_id,
        } => {
            let tag = Some(client_id.clone());
            let result = async { manager.open(session).await?.send(text, tag).await };
            (client_id, result.await)
        }
        ClientMessage::Interrupt { session, client_id } => {
            let result = async {
                running(manager, session).await?.interrupt().await?;
                Ok(())
            };
            (client_id, result.await)
        }
        ClientMessage::Answer {
            session,
            request_id,
            answer,
            client_id,
        } => {
            let result = async {
                running(manager, session)
                    .await?
                    .answer(request_id, answer)
                    .await
            };
            (client_id, result.await)
        }
        ClientMessage::SetPermissionMode {
            session,
            mode,
            client_id,
        } => {
            let result = manager.set_permission_mode(session, mode).await;
            (client_id, result)
        }
        ClientMessage::Subscribe { .. } | ClientMessage::Unsubscribe { .. } => {
            (String::new(), Err(anyhow!("not a command")))
        }
    }
}

async fn running(manager: &SessionManager, id: Uuid) -> Result<Session> {
    manager
        .live(id)
        .await
        .ok_or_else(|| anyhow!("session {id} is not running"))
}

/// The receiving socket is gone.
struct Gone;

/// Forwards the events of one session after `after_seq`, following it across
/// child restarts, until the socket goes away or the task is aborted.
async fn stream_session(
    manager: SessionManager,
    id: Uuid,
    after_seq: u64,
    out: mpsc::Sender<ServerMessage>,
) {
    let mut changes = manager.subscribe_changes();
    let unknown = match manager.store().session(id).await {
        Ok(Some(_)) => None,
        Ok(None) => Some(format!("unknown session {id}")),
        Err(err) => Some(format!("{err:#}")),
    };
    if let Some(message) = unknown {
        let _ = out
            .send(ServerMessage::Error {
                message,
                session: Some(id),
            })
            .await;
        return;
    }
    let mut forwarder = Forwarder {
        manager: manager.clone(),
        id,
        sent: after_seq,
        out,
    };
    // Viewing a session never spawns its child; only `send` does.
    let mut session = match manager.live(id).await {
        Some(session) => session,
        None => {
            if forwarder.catch_up().await.is_err() {
                return;
            }
            let Some(next) = next_run(&manager, id, &mut changes).await else {
                return;
            };
            next
        }
    };
    loop {
        if forwarder.follow(&session).await.is_err() {
            return;
        }
        session.closed().await;
        let Some(next) = next_run(&manager, id, &mut changes).await else {
            return;
        };
        session = next;
    }
}

/// Waits until the session has a running child again.
async fn next_run(
    manager: &SessionManager,
    id: Uuid,
    changes: &mut broadcast::Receiver<Uuid>,
) -> Option<Session> {
    loop {
        if let Some(session) = manager.live(id).await {
            return Some(session);
        }
        match changes.recv().await {
            Ok(_) | Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => return None,
        }
    }
}

struct Forwarder {
    manager: SessionManager,
    id: Uuid,
    /// Seq of the last event sent to the socket.
    sent: u64,
    out: mpsc::Sender<ServerMessage>,
}

impl Forwarder {
    /// Streams one run of the session until its child exits.
    ///
    /// The broadcast is subscribed before history is read, and every event
    /// reaches the store before the broadcast, so the two overlap and never
    /// leave a gap; events already sent are skipped. A lag or a jump in seqs
    /// is filled from the store.
    async fn follow(&mut self, session: &Session) -> Result<(), Gone> {
        let mut events = session.subscribe();
        self.catch_up().await?;
        loop {
            match events.recv().await {
                Ok(event) => {
                    if event.seq == self.sent + 1 {
                        self.send_live(&event).await?;
                    } else if event.seq > self.sent {
                        self.catch_up().await?;
                    }
                    if is_exit(&event) {
                        return Ok(());
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    debug!(session = %self.id, skipped, "subscriber lagged; reading history");
                    self.catch_up().await?;
                }
                Err(RecvError::Closed) => return Ok(()),
            }
        }
    }

    async fn send_live(&mut self, event: &Event) -> Result<(), Gone> {
        self.sent = event.seq;
        self.send(ServerMessage::Event(WireEvent::live(self.id, event)))
            .await
    }

    /// Sends every stored event after `sent`.
    async fn catch_up(&mut self) -> Result<(), Gone> {
        loop {
            let page = match self.manager.events_after(self.id, self.sent, PAGE).await {
                Ok(page) => page,
                Err(err) => {
                    warn!(session = %self.id, "read history: {err:#}");
                    return self
                        .send(ServerMessage::Error {
                            message: format!("read history: {err:#}"),
                            session: Some(self.id),
                        })
                        .await;
                }
            };
            let full = page.len() == PAGE as usize;
            for event in page {
                self.sent = event.seq;
                self.send(ServerMessage::Event(WireEvent::stored(self.id, event)))
                    .await?;
            }
            if !full {
                return Ok(());
            }
        }
    }

    async fn send(&self, message: ServerMessage) -> Result<(), Gone> {
        self.out.send(message).await.map_err(|_| Gone)
    }
}

fn is_exit(event: &Event) -> bool {
    matches!(event.body, EventBody::Host(HostEvent::ChildExited { .. }))
}
