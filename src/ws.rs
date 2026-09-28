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
    let mut catalog_changes = manager.subscribe_catalog();
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
            change = catalog_changes.recv() => match change {
                Ok(()) | Err(RecvError::Lagged(_)) => {
                    if let Err(err) = send(&mut socket, &ServerMessage::CatalogChanged).await {
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
            ClientMessage::Subscribe {
                session,
                after_seq,
                exchanges,
            } => {
                // The old stream stops first, so its last messages precede the new one's.
                if let Some(previous) = self.subscriptions.remove(&session) {
                    previous.abort();
                }
                let task = tokio::spawn(stream_session(
                    self.manager.clone(),
                    session,
                    after_seq,
                    exchanges.filter(|_| after_seq == 0),
                    self.out.clone(),
                ));
                self.subscriptions.insert(session, task);
            }
            ClientMessage::History {
                session,
                before_seq,
                exchanges,
            } => {
                let manager = self.manager.clone();
                let out = self.out.clone();
                tokio::spawn(async move {
                    let message = page(&manager, session, before_seq, exchanges)
                        .await
                        .unwrap_or_else(|err| {
                            warn!(%session, "read history page: {err:#}");
                            ServerMessage::Error {
                                message: format!("read history: {err:#}"),
                                session: Some(session),
                            }
                        });
                    let _ = out.send(message).await;
                });
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
            rewarm,
            client_id,
        } => {
            let tag = Some(client_id.clone());
            let result = async {
                manager.unarchive(session).await?;
                if !rewarm {
                    manager.ensure_warm(session).await?;
                }
                manager.open(session).await?.send(text, tag).await
            };
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
            rewarm,
            client_id,
        } => {
            let result = async {
                manager.unarchive(session).await?;
                if !rewarm {
                    manager.ensure_warm(session).await?;
                }
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
        ClientMessage::SetSettings {
            session,
            model,
            effort,
            client_id,
        } => (
            client_id,
            manager.set_settings(session, model, effort).await,
        ),
        ClientMessage::Restart {
            session,
            mode,
            text,
            client_id,
        } => {
            let tag = Some(client_id.clone());
            let result = async {
                manager.unarchive(session).await?;
                manager
                    .restart(session, mode, text.unwrap_or_default(), tag)
                    .await
            };
            (client_id, result.await)
        }
        ClientMessage::Kill {
            session,
            reason,
            client_id,
        } => (client_id, manager.kill(session, reason).await),
        ClientMessage::Archive { session, client_id } => {
            (client_id, manager.archive(session).await)
        }
        ClientMessage::Restore { session, client_id } => {
            (client_id, manager.restore(session).await)
        }
        ClientMessage::Subscribe { .. }
        | ClientMessage::Unsubscribe { .. }
        | ClientMessage::History { .. } => (String::new(), Err(anyhow!("not a command"))),
    }
}

/// The `page` answering a `history` command.
async fn page(
    manager: &SessionManager,
    id: Uuid,
    before_seq: u64,
    exchanges: u32,
) -> Result<ServerMessage> {
    let store = manager.store();
    let start = store.page_start(id, Some(before_seq), exchanges).await?;
    let events = store
        .events_between(id, start.first_seq, before_seq)
        .await?;
    Ok(ServerMessage::Page {
        session: id,
        first_seq: start.first_seq,
        start_reached: start.start_reached,
        events: start
            .prelude
            .into_iter()
            .chain(events)
            .map(|event| WireEvent::stored(id, event))
            .collect(),
    })
}

async fn running(manager: &SessionManager, id: Uuid) -> Result<Session> {
    manager
        .live(id)
        .await
        .ok_or_else(|| anyhow!("session {id} is not running"))
}

/// The receiving socket is gone.
struct Gone;

/// Forwards the events of one session after `after_seq`, or of its last
/// `exchanges` exchanges, following it across child restarts, until the socket
/// goes away or the task is aborted.
async fn stream_session(
    manager: SessionManager,
    id: Uuid,
    after_seq: u64,
    exchanges: Option<u32>,
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
    if let Some(exchanges) = exchanges
        && forwarder.open_window(exchanges).await.is_err()
    {
        return;
    }
    // Viewing a session never spawns its child; only `send` does.
    let mut session = match manager.live(id).await {
        Some(session) => session,
        None => {
            let Some(next) = next_run(&mut forwarder, &mut changes).await else {
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
        let Some(next) = next_run(&mut forwarder, &mut changes).await else {
            return;
        };
        session = next;
    }
}

/// Waits until the session has a running child again, sending what gets stored
/// meanwhile without a child, such as a restart that launched nothing.
async fn next_run(
    forwarder: &mut Forwarder,
    changes: &mut broadcast::Receiver<Uuid>,
) -> Option<Session> {
    let id = forwarder.id;
    loop {
        if let Some(session) = forwarder.manager.live(id).await {
            return Some(session);
        }
        forwarder.catch_up().await.ok()?;
        loop {
            match changes.recv().await {
                Ok(changed) if changed != id => {}
                Ok(_) | Err(RecvError::Lagged(_)) => break,
                Err(RecvError::Closed) => return None,
            }
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

    /// Sends `window` and the prelude of the last `exchanges` exchanges, and moves
    /// `sent` to just before them. A failed read leaves `sent`: the whole history follows.
    async fn open_window(&mut self, exchanges: u32) -> Result<(), Gone> {
        let start = match self
            .manager
            .store()
            .page_start(self.id, None, exchanges)
            .await
        {
            Ok(start) => start,
            Err(err) => {
                warn!(session = %self.id, "read history window: {err:#}");
                return self
                    .send(ServerMessage::Error {
                        message: format!("read history: {err:#}"),
                        session: Some(self.id),
                    })
                    .await;
            }
        };
        self.send(ServerMessage::Window {
            session: self.id,
            first_seq: start.first_seq,
            start_reached: start.start_reached,
        })
        .await?;
        for event in start.prelude {
            self.send(ServerMessage::Event(WireEvent::stored(self.id, event)))
                .await?;
        }
        self.sent = start.first_seq.saturating_sub(1);
        Ok(())
    }

    async fn send_live(&mut self, event: &Event) -> Result<(), Gone> {
        self.sent = event.seq;
        self.send(ServerMessage::Event(WireEvent::live(self.id, event)))
            .await
    }

    /// Sends every stored event after `sent`, then `synced`.
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
                return self
                    .send(ServerMessage::Synced {
                        session: self.id,
                        seq: self.sent,
                    })
                    .await;
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
