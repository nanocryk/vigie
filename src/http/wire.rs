//! Messages between the server and its clients: the WebSocket protocol at
//! `/api/ws` and the bodies of the `/api/sessions` routes. The protocol is
//! specified in `notes/wire.md`.

use std::path::PathBuf;

use chrono::{DateTime, SubsecRound, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::claude::child::Effort;
use crate::claude::protocol::PermissionMode;
use crate::session::advice::RestartMeasure;
use crate::session::agents::AgentView;
use crate::session::background::BackgroundView;
use crate::session::gauges::{Cache, Context};
use crate::session::manager::LiveView;
use crate::session::{Activity, Answer, Event, EventBody, RestartMode, StopReason};
use crate::store::{EventKind, SessionRow, SessionState, StoredEvent, Unread};

/// Ack error of a `send` or an `answer` refused on a cold cache.
pub const CACHE_COLD: &str = "cache_cold";
/// Ack error of an `archive` refused while the child runs.
pub const SESSION_LIVE: &str = "session_live";

/// A client frame. Both kinds are tagged by `type`; a `type` naming a
/// subscription reads as one, any other as a command.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ClientMessage {
    Subscription(Subscription),
    Command(Command),
}

impl<'de> Deserialize<'de> for ClientMessage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let subscription = value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| Subscription::TYPES.contains(&kind));
        let message = if subscription {
            Subscription::deserialize(value).map(Self::Subscription)
        } else {
            Command::deserialize(value).map(Self::Command)
        };
        message.map_err(serde::de::Error::custom)
    }
}

/// What a connection streams; not acked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Subscription {
    Subscribe {
        session: Uuid,
        #[serde(default)]
        after_seq: u64,
        /// With `after_seq` 0: send only the last exchanges, preceded by `window`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exchanges: Option<u32>,
    },
    /// Answered by `page`.
    History {
        session: Uuid,
        before_seq: u64,
        exchanges: u32,
    },
    Unsubscribe {
        session: Uuid,
    },
    /// The discussion on screen (`None` on Home or elsewhere) and whether the user
    /// is there: page visible and recent input. Sent on every change.
    Presence {
        session: Option<Uuid>,
        active: bool,
    },
}

impl Subscription {
    /// The `type` of each variant.
    const TYPES: [&str; 4] = ["subscribe", "history", "unsubscribe", "presence"];
}

/// An action on one session, answered by one `ack` carrying `client_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Command {
    pub session: Uuid,
    pub client_id: String,
    #[serde(flatten)]
    pub action: Action,
}

impl From<Command> for ClientMessage {
    fn from(command: Command) -> Self {
        Self::Command(command)
    }
}

impl From<Subscription> for ClientMessage {
    fn from(subscription: Subscription) -> Self {
        Self::Subscription(subscription)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Send {
        text: String,
        /// Paths of the attached files.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
        /// Goes through on a cold cache.
        #[serde(default)]
        rewarm: bool,
    },
    Interrupt,
    /// A message for subagent `task_id`, relayed by the main agent.
    Relay {
        task_id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
        /// Goes through on a cold cache.
        #[serde(default)]
        rewarm: bool,
    },
    StopTask {
        task_id: String,
    },
    /// Stops every running agent.
    StopAgents,
    Answer {
        request_id: String,
        answer: Answer,
        /// Goes through on a cold cache.
        #[serde(default)]
        rewarm: bool,
    },
    SetPermissionMode {
        mode: PermissionMode,
    },
    /// An absent field keeps its setting; `null` goes back to the CLI's default.
    SetSettings {
        #[serde(
            default,
            deserialize_with = "present",
            skip_serializing_if = "Option::is_none"
        )]
        model: Option<Option<String>>,
        #[serde(
            default,
            deserialize_with = "present",
            skip_serializing_if = "Option::is_none"
        )]
        effort: Option<Option<String>>,
    },
    Restart {
        mode: RestartMode,
        #[serde(default)]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
    },
    Kill {
        #[serde(default)]
        reason: StopReason,
    },
    Archive,
    Restore,
}

/// Tells a field sent as `null` from an absent one, which `default` leaves `None`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
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
    /// Every stored event of `session` up to `seq` was sent.
    Synced {
        session: Uuid,
        seq: u64,
    },
    /// Opens a windowed `subscribe`: its prelude events, then the events from `first_seq`.
    Window {
        session: Uuid,
        first_seq: u64,
        start_reached: bool,
    },
    /// Answers `history`: the prelude, then the events from `first_seq` to below `before_seq`.
    Page {
        session: Uuid,
        first_seq: u64,
        start_reached: bool,
        events: Vec<WireEvent>,
    },
    SessionsChanged,
    UsageChanged,
    CatalogChanged,
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
    pub permission_mode: Option<PermissionMode>,
    /// Prompt cache of the main conversation; clients count the TTL down themselves.
    pub cache: Option<Cache>,
    pub context: Option<Context>,
    /// Main-conversation requests per finished turn; `None` before the first turn.
    pub calls_per_message: Option<f64>,
    /// What a restart would keep, measured after each turn; `None` before.
    pub restart: Option<RestartMeasure>,
    /// `None` while the discussion is not archived.
    pub archived_at: Option<DateTime<Utc>>,
    /// Stored model and effort, passed at every launch; `None` lets the CLI pick.
    pub model: Option<String>,
    pub effort: Option<Effort>,
    /// When a model or effort change made the cache count cold; `None` once a
    /// main request or a restart followed.
    pub settings_changed_at: Option<DateTime<Utc>>,
    /// What the CLI last reported applying, such as `claude-opus-5-5[1m]` and
    /// `medium`; `None` while unknown.
    pub applied_model: Option<String>,
    pub applied_effort: Option<String>,
    /// Subagents of the running child, oldest first; empty when not live.
    pub agents: Vec<AgentView>,
    /// Background tasks of the running child, oldest first; empty when not live.
    pub background: Vec<BackgroundView>,
    /// `None` once the user saw the discussion.
    pub unread: Option<Unread>,
}

impl SessionView {
    /// `live` is `None` when the child is not running.
    pub fn new(row: SessionRow, live: Option<LiveView>) -> Self {
        let status = live.as_ref().map(|live| live.status);
        let (agents, background) = live
            .map(|live| (live.agents, live.background))
            .unwrap_or_default();
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
            calls_per_message: row.calls_per_message,
            restart: row.restart,
            archived_at: row.archived_at,
            model: row.settings.model,
            effort: row.settings.effort,
            settings_changed_at: row.settings_changed_at,
            applied_model: row.applied.model,
            applied_effort: row.applied.effort,
            agents,
            background,
            unread: row.unread,
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
