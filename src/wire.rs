//! Messages between the server and its clients: the WebSocket protocol at
//! `/api/ws` and the bodies of the `/api/sessions` routes.
//!
//! WebSocket frames are JSON text tagged by `type`.
//!
//! Client to server:
//! - `subscribe {session, after_seq}`: opens the session, sends its stored events
//!   with a seq above `after_seq`, then its live events, each once and in order.
//!   Subscribing again to the same session restarts from the new `after_seq`.
//! - `unsubscribe {session}`
//! - `send {session, text, client_id}`: spawns the child again if it ended. The
//!   session emits a `user_sent {text, client_id}` host event before writing the text.
//! - `interrupt {session, client_id}`
//! - `answer {session, request_id, answer, client_id}`: `answer` is `allow
//!   {updated_input?, note?}`, `deny {message}` or `questions {answers}`, tagged by
//!   `kind`. A non-blank allow `note` is then sent as a user message queued for the
//!   running turn, with its own `user_sent` event.
//! - `set_permission_mode {session, mode, client_id}`: a live session asks the CLI;
//!   a stopped one only stores the mode (`default`, `acceptEdits` or `plan`) for its
//!   next launch and announces `sessions_changed`.
//!
//! Server to client:
//! - `event {session, seq, time, kind, payload}`: `kind` `line` carries a CLI
//!   stdout line as JSON (a string when the line is not JSON), `host` a host
//!   event tagged by `event`. `ask_queued` carries `danger {dangerous, reasons}`.
//! - `ack {client_id, ok, error?}`: one per command carrying a `client_id`.
//! - `sessions_changed`: a session was created, changed activity, ended, or had
//!   the permission mode of its stopped state changed;
//!   `GET /api/sessions` has the new list.
//! - `error {message, session?}`: a malformed frame, or a `subscribe` that failed.

use std::path::PathBuf;

use chrono::{DateTime, SubsecRound, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::session::{Answer, Event, EventBody};
use crate::store::{EventKind, SessionRow, SessionState, StoredEvent};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Subscribe {
        session: Uuid,
        #[serde(default)]
        after_seq: u64,
    },
    Unsubscribe {
        session: Uuid,
    },
    Send {
        session: Uuid,
        text: String,
        client_id: String,
    },
    Interrupt {
        session: Uuid,
        client_id: String,
    },
    Answer {
        session: Uuid,
        request_id: String,
        answer: Answer,
        client_id: String,
    },
    SetPermissionMode {
        session: Uuid,
        mode: String,
        client_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Event(WireEvent),
    Ack {
        client_id: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    SessionsChanged,
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<Uuid>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireEvent {
    pub session: Uuid,
    pub seq: u64,
    pub time: DateTime<Utc>,
    pub kind: EventKind,
    pub payload: Value,
}

impl WireEvent {
    pub fn live(session: Uuid, event: &Event) -> Self {
        let (kind, payload) = match &event.body {
            EventBody::Line(line) => (EventKind::Line, line_payload(&line.raw)),
            EventBody::Host(host) => (
                EventKind::Host,
                serde_json::to_value(host).unwrap_or(Value::Null),
            ),
        };
        Self {
            session,
            seq: event.seq,
            // The store keeps microseconds; an event reads the same live or replayed.
            time: event.time.trunc_subsecs(6),
            kind,
            payload,
        }
    }

    pub fn stored(session: Uuid, event: StoredEvent) -> Self {
        let payload = match event.kind {
            EventKind::Line => line_payload(&event.payload),
            EventKind::Host => serde_json::from_str(&event.payload).unwrap_or(Value::Null),
        };
        Self {
            session,
            seq: event.seq,
            time: event.time,
            kind: event.kind,
            payload,
        }
    }
}

fn line_payload(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// A session as `GET /api/sessions` lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionView {
    pub id: Uuid,
    pub cwd: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub state: SessionState,
    pub last_seq: u64,
    /// The child is running.
    pub live: bool,
    /// A turn or a background task is running.
    pub busy: bool,
    /// Stored permission mode, passed at the next launch; `None` when never recorded.
    pub permission_mode: Option<String>,
}

impl SessionView {
    pub fn new(row: SessionRow, live: bool, busy: bool) -> Self {
        Self {
            id: row.id,
            cwd: row.cwd,
            created_at: row.created_at,
            updated_at: row.updated_at,
            state: row.state,
            last_seq: row.last_seq,
            live,
            busy,
            permission_mode: row.permission_mode,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<SessionView>,
}

/// Body of `POST /api/sessions`; `cwd` is an existing absolute directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateSession {
    pub cwd: PathBuf,
}
