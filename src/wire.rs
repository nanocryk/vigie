//! Messages between the server and its clients: the WebSocket protocol at
//! `/api/ws` and the bodies of the `/api/sessions` routes. `GET /api/usage`
//! answers `usage::Snapshot`.
//!
//! WebSocket frames are JSON text tagged by `type`.
//!
//! Client to server:
//! - `subscribe {session, after_seq}`: opens the session, sends its stored events
//!   with a seq above `after_seq`, then its live events, each once and in order.
//!   Subscribing again to the same session restarts from the new `after_seq`.
//! - `unsubscribe {session}`
//! - `send {session, text, rewarm?, client_id}`: spawns the child again if it
//!   ended. The session emits a `user_sent {text, client_id}` host event before
//!   writing the text.
//! - `interrupt {session, client_id}`
//! - `answer {session, request_id, answer, rewarm?, client_id}`: `answer` is `allow
//!   {updated_input?, note?}`, `deny {message}` or `questions {answers}`, tagged by
//!   `kind`. A non-blank allow `note` is then sent as a user message queued for the
//!   running turn, with its own `user_sent` event.
//! - While the discussion's cache is cold (`gauges::is_cold`), a `send` or an
//!   `answer` without `rewarm: true` is refused with the ack error `cache_cold`
//!   and has no effect. `restart` and `interrupt` are always accepted.
//! - `set_permission_mode {session, mode, client_id}`: a live session asks the CLI;
//!   a stopped one only stores the mode (`default`, `acceptEdits` or `plan`) for its
//!   next launch and announces `sessions_changed`.
//! - `restart {session, mode, text?, client_id}`: moves the discussion to a fresh
//!   Claude session in the same permission mode, and announces `sessions_changed`.
//!   The live child, if any, is stopped first, its pending asks dropped. The
//!   session then records a `restarted {mode, exchanges, total_exchanges, seed?}`
//!   host event and forgets its `cache` and `context`. `mode` `continue` launches
//!   the new session with one message: `seed`, a blank line, then `text`, or
//!   "Say what you want to do next." when `text` is blank; `seed` renders the
//!   last `exchanges` of the discussion's `total_exchanges`. `blank` launches it
//!   with `text` when not blank, else only at the next `send`. The `user_sent`
//!   event carries `text` alone, and there is none for a blank `text`.
//! - `kill {session, client_id}`: stops the live child the graceful way; the
//!   discussion stays listed and the next `send` resumes it. Acked without effect
//!   when not live.
//! - `archive {session, client_id}`: refused with the ack error `session_live`
//!   while the child runs. `restore {session, client_id}` unarchives.
//! - A `send`, `answer` or `restart` on an archived discussion unarchives it
//!   first, then follows its usual rules, the cold check included.
//! - `kill`, `archive` and `restore` announce `sessions_changed`.
//!
//! Server to client:
//! - `event {session, seq, time, kind, payload}`: `kind` `line` carries a CLI
//!   stdout line as JSON (a string when the line is not JSON), `host` a host
//!   event tagged by `event`. `ask_queued` carries `danger {dangerous, reasons}`.
//! - `ack {client_id, ok, error?}`: one per command carrying a `client_id`.
//! - `sessions_changed`: a session was created, changed activity or its count
//!   of pending asks, ended, had
//!   the permission mode of its stopped state changed, its main conversation
//!   made a request or finished a turn (new `cache` and `context`), or the CLI
//!   reported a new context usage, it restarted on a fresh Claude session, or it
//!   was killed, archived or restored; `GET /api/sessions` has the new list.
//! - `usage_changed`: a usage reading was taken or a turn finished;
//!   `GET /api/usage` has the new snapshot.
//! - `error {message, session?}`: a malformed frame, or a `subscribe` that failed.
//!
//! Each session of `GET /api/sessions` carries `archived_at` (null while not
//! archived), `live`, `busy`, `asks` (the
//! permission, question and plan approval asks its running child awaits an
//! answer to, 0 when not live), `cache {last_request_at,
//! ttl_secs}` and `context {used_tokens, max_tokens, percentage, source,
//! autocompact_threshold, categories}`; either is null while unknown. `source`
//! is `cli` when the total comes from the CLI's `get_context_usage` (asked when
//! the child starts and after each turn), `estimate` when a later main request
//! replaced it with its prompt tokens. `max_tokens` is the CLI's when known,
//! else the model's context window. `autocompact_threshold` (nullable) and
//! `categories [{name, tokens, kind}]` (empty while unknown) come from the
//! latest CLI reply.

use std::path::PathBuf;

use chrono::{DateTime, SubsecRound, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::gauges::{Cache, Context};
use crate::session::{Activity, Answer, Event, EventBody, LiveStatus, RestartMode};
use crate::store::{EventKind, SessionRow, SessionState, StoredEvent};

/// Ack error of a `send` or an `answer` refused on a cold cache.
pub const CACHE_COLD: &str = "cache_cold";
/// Ack error of an `archive` refused while the child runs.
pub const SESSION_LIVE: &str = "session_live";

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
        /// Goes through on a cold cache.
        #[serde(default)]
        rewarm: bool,
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
        /// Goes through on a cold cache.
        #[serde(default)]
        rewarm: bool,
        client_id: String,
    },
    SetPermissionMode {
        session: Uuid,
        mode: String,
        client_id: String,
    },
    Restart {
        session: Uuid,
        mode: RestartMode,
        #[serde(default)]
        text: Option<String>,
        client_id: String,
    },
    Kill {
        session: Uuid,
        client_id: String,
    },
    Archive {
        session: Uuid,
        client_id: String,
    },
    Restore {
        session: Uuid,
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
    UsageChanged,
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
    /// Asks of the running child waiting for an answer; 0 when not live.
    pub asks: usize,
    /// Stored permission mode, passed at the next launch; `None` when never recorded.
    pub permission_mode: Option<String>,
    /// Prompt cache of the main conversation; clients count the TTL down themselves.
    pub cache: Option<Cache>,
    pub context: Option<Context>,
    /// `None` while the discussion is not archived.
    pub archived_at: Option<DateTime<Utc>>,
}

impl SessionView {
    /// `status` is `None` when the child is not running.
    pub fn new(row: SessionRow, status: Option<LiveStatus>) -> Self {
        Self {
            id: row.id,
            cwd: row.cwd,
            created_at: row.created_at,
            updated_at: row.updated_at,
            state: row.state,
            last_seq: row.last_seq,
            live: status.is_some(),
            busy: status.is_some_and(|status| status.activity == Activity::Busy),
            asks: status.map_or(0, |status| status.asks),
            permission_mode: row.permission_mode,
            cache: row.cache,
            context: row.context,
            archived_at: row.archived_at,
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
