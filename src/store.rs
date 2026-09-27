//! Session history in SQLite: sessions, their events, `get_usage` samples, and
//! the usage tables fed from both.
//!
//! One writer thread owns the connection and runs every job in submission order,
//! writes and reads alike. A read therefore sees every write queued before it,
//! and nothing blocks the async runtime.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Local, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::child::Launch;
use crate::gauges::{self, Cache, ContextSource, MainRequest, Windows};
use crate::protocol::{ContextUsageResponse, GetUsageResponse, Inbound};
use crate::session::{Event, EventBody, GateFailure, HostEvent};
use crate::usage::db::{MessageTokens, UsageDb};
use crate::usage::inputs::{self, Reading};
use crate::usage::units::Timestamp;
use crate::usage::{self, SessionContext, Snapshot};

/// Applied in order; `PRAGMA user_version` counts the ones already applied.
const MIGRATIONS: [&str; 6] = [
    r#"
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    cwd TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_seq INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL CHECK (state IN ('running', 'stopped', 'exited', 'gate_failed')),
    exit_code INTEGER,
    gate_failure TEXT
) STRICT;
CREATE INDEX sessions_updated_at ON sessions (updated_at);
CREATE TABLE events (
    session_id TEXT NOT NULL REFERENCES sessions (id),
    seq INTEGER NOT NULL,
    time TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('line', 'host')),
    payload TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
) STRICT;
CREATE TABLE usage_samples (
    id INTEGER PRIMARY KEY,
    time TEXT NOT NULL,
    session_id TEXT REFERENCES sessions (id),
    response TEXT NOT NULL
) STRICT;
CREATE INDEX usage_samples_time ON usage_samples (time);
"#,
    // The CLI permission mode the session runs with, or starts with when resumed.
    "ALTER TABLE sessions ADD COLUMN permission_mode TEXT;",
    usage::db::MIGRATION,
    // The prompt cache and context of the main conversation, see `gauges`.
    r#"
ALTER TABLE sessions ADD COLUMN cache_message_id TEXT;
ALTER TABLE sessions ADD COLUMN cache_request_at TEXT;
ALTER TABLE sessions ADD COLUMN cache_ttl_secs INTEGER;
ALTER TABLE sessions ADD COLUMN context_model TEXT;
ALTER TABLE sessions ADD COLUMN context_tokens INTEGER;
ALTER TABLE sessions ADD COLUMN context_window INTEGER;
"#,
    // The latest `get_context_usage` reply; `context_tokens` holds its total until
    // the next main request.
    r#"
ALTER TABLE sessions ADD COLUMN context_source TEXT CHECK (context_source IN ('cli', 'estimate'));
ALTER TABLE sessions ADD COLUMN context_max_tokens INTEGER;
ALTER TABLE sessions ADD COLUMN context_autocompact INTEGER;
ALTER TABLE sessions ADD COLUMN context_categories TEXT;
"#,
    // The Claude session behind the discussion when not its own id, and whether
    // no child launched it yet.
    r#"
ALTER TABLE sessions ADD COLUMN claude_session_id TEXT;
ALTER TABLE sessions ADD COLUMN claude_session_fresh INTEGER NOT NULL DEFAULT 0
    CHECK (claude_session_fresh IN (0, 1));
"#,
];

/// Jobs the writer thread folds into one transaction at most.
const MAX_BATCH: usize = 256;

type Job = Box<dyn FnOnce(&Connection) + Send>;

/// Handle to the history database. Cheap to clone; the writer thread exits once
/// every handle is dropped and the queue is drained.
#[derive(Debug, Clone)]
pub struct Store {
    jobs: mpsc::Sender<Job>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionState {
    Running,
    /// The host closed the child's stdin.
    Stopped,
    /// The child exited without being asked to.
    Exited {
        code: Option<i32>,
    },
    GateFailed {
        reason: GateFailure,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: Uuid,
    pub cwd: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Seq of the latest event, 0 when there is none.
    pub last_seq: u64,
    pub state: SessionState,
    /// Last known CLI permission mode; `None` for sessions from before it was stored.
    pub permission_mode: Option<String>,
    /// `None` until a main request reported the TTL of its cache write.
    pub cache: Option<Cache>,
    /// `None` until a `get_context_usage` reply, or a main request and a
    /// `result`, reported the total and the window.
    pub context: Option<gauges::Context>,
    /// How the next child picks the discussion's current Claude session.
    pub launch: Launch,
}

/// What a line tells about the gauges of the main conversation.
enum GaugeUpdate {
    Request(MainRequest),
    Windows(Windows),
}

impl GaugeUpdate {
    fn from_line(line: &Inbound) -> Option<Self> {
        match line {
            Inbound::Assistant(assistant) => {
                MainRequest::from_assistant(assistant).map(Self::Request)
            }
            Inbound::Result(result) => Some(Self::Windows(Windows::from_result(result))),
            _ => None,
        }
    }

    fn apply(self, connection: &Connection, session: &str, time: &str) -> Result<()> {
        match self {
            // SQLite evaluates every assignment against the row as it was, so the
            // time and the source only move on the first line of a response.
            Self::Request(request) => {
                connection.execute(
                    "UPDATE sessions SET
                         cache_request_at = CASE WHEN cache_message_id IS ?2
                             THEN cache_request_at ELSE ?3 END,
                         context_source = CASE WHEN cache_message_id IS ?2
                             THEN context_source ELSE 'estimate' END,
                         cache_message_id = ?2,
                         cache_ttl_secs = COALESCE(?4, cache_ttl_secs),
                         context_model = COALESCE(?5, context_model),
                         context_tokens = ?6
                     WHERE id = ?1",
                    params![
                        session,
                        request.message_id,
                        time,
                        request.ttl_secs,
                        request.model,
                        request.prompt_tokens
                    ],
                )?;
            }
            Self::Windows(windows) => {
                let model: Option<String> = connection
                    .query_row(
                        "SELECT context_model FROM sessions WHERE id = ?1",
                        [session],
                        |row| row.get(0),
                    )
                    .optional()?
                    .flatten();
                if let Some(window) = windows.for_model(model.as_deref()) {
                    connection.execute(
                        "UPDATE sessions SET context_window = ?2 WHERE id = ?1",
                        params![session, window],
                    )?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// `payload` is a CLI stdout line exactly as received.
    Line,
    /// `payload` is a serialized `HostEvent`.
    Host,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub seq: u64,
    pub time: DateTime<Utc>,
    pub kind: EventKind,
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageSample {
    pub time: DateTime<Utc>,
    pub session_id: Option<Uuid>,
    /// The `get_usage` response payload as JSON text.
    pub response: String,
}

impl Store {
    /// `~/.vigie2/history.db`.
    pub fn default_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("no home dir")?;
        Ok(home.join(".vigie2").join("history.db"))
    }

    /// Opens or creates the database, migrates it, and starts the writer thread.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let connection =
            Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        let journal_mode: String =
            connection.pragma_update_and_check(None, "journal_mode", "wal", |row| row.get(0))?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            bail!("{}: journal mode {journal_mode}, not wal", path.display());
        }
        connection.pragma_update(None, "synchronous", "normal")?;
        connection.pragma_update(None, "foreign_keys", true)?;
        migrate(&connection).with_context(|| format!("migrate {}", path.display()))?;
        let (jobs, queue) = mpsc::channel();
        std::thread::Builder::new()
            .name("vigie2-history".into())
            .spawn(move || run_writer(connection, queue))
            .context("spawn history writer")?;
        Ok(Self { jobs })
    }

    /// Inserts the session, or marks a known one running again, its current Claude
    /// session now launched. A `None` mode keeps the stored one.
    pub fn session_started(
        &self,
        id: Uuid,
        cwd: &Path,
        time: DateTime<Utc>,
        permission_mode: Option<String>,
    ) {
        let cwd = cwd.to_string_lossy().into_owned();
        self.write("session start", move |connection| {
            let time = format_time(time);
            connection.execute(
                "INSERT INTO sessions (id, cwd, created_at, updated_at, state, permission_mode)
                 VALUES (?1, ?2, ?3, ?3, 'running', ?4)
                 ON CONFLICT (id) DO UPDATE SET
                     updated_at = excluded.updated_at, state = 'running',
                     exit_code = NULL, gate_failure = NULL, claude_session_fresh = 0,
                     permission_mode = COALESCE(excluded.permission_mode, permission_mode)",
                params![id.to_string(), cwd, time, permission_mode],
            )?;
            Ok(())
        });
    }

    /// Points the discussion at `claude_session`, which no child launched yet, and
    /// forgets the gauges of the previous one. Fails for an unknown session.
    pub async fn restart_claude_session(&self, session: Uuid, claude_session: Uuid) -> Result<()> {
        self.call(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET claude_session_id = ?2, claude_session_fresh = 1,
                     cache_message_id = NULL, cache_request_at = NULL, cache_ttl_secs = NULL,
                     context_model = NULL, context_tokens = NULL, context_window = NULL,
                     context_source = NULL, context_max_tokens = NULL,
                     context_autocompact = NULL, context_categories = NULL
                 WHERE id = ?1",
                params![session.to_string(), claude_session.to_string()],
            )?;
            if changed == 0 {
                bail!("unknown session {session}");
            }
            Ok(())
        })
        .await
    }

    /// Resolves once stored; fails for an unknown session.
    pub async fn set_permission_mode(&self, session: Uuid, mode: String) -> Result<()> {
        self.call(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET permission_mode = ?2 WHERE id = ?1",
                params![session.to_string(), mode],
            )?;
            if changed == 0 {
                bail!("unknown session {session}");
            }
            Ok(())
        })
        .await
    }

    pub fn record_event(&self, session: Uuid, event: &Event) {
        let seq = event.seq;
        let time = format_time(event.time);
        let permission_mode = match &event.body {
            EventBody::Host(HostEvent::PermissionMode { mode }) => Some(mode.clone()),
            _ => None,
        };
        let (kind, payload) = match &event.body {
            EventBody::Line(line) => (EventKind::Line, line.raw.clone()),
            EventBody::Host(host) => match serde_json::to_string(host) {
                Ok(json) => (EventKind::Host, json),
                Err(err) => {
                    tracing::warn!("serialize host event {seq}: {err}");
                    return;
                }
            },
        };
        let (readings, message) = match &event.body {
            EventBody::Line(line) => match &line.parsed {
                Some(Inbound::RateLimitEvent(limits)) => {
                    (inputs::from_rate_limit_event(&limits.rate_limit_info), None)
                }
                Some(Inbound::Assistant(assistant)) => {
                    (Vec::new(), inputs::from_assistant(assistant))
                }
                _ => (Vec::new(), None),
            },
            EventBody::Host(_) => (Vec::new(), None),
        };
        let gauge = match &event.body {
            EventBody::Line(line) => line.parsed.as_ref().and_then(GaugeUpdate::from_line),
            EventBody::Host(_) => None,
        };
        let at = Timestamp::from_datetime(event.time);
        self.write("event", move |connection| {
            let session = session.to_string();
            connection.execute(
                "INSERT INTO events (session_id, seq, time, kind, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session, seq, time, kind.as_str(), payload],
            )?;
            connection.execute(
                "UPDATE sessions SET last_seq = ?2, updated_at = ?3,
                     permission_mode = COALESCE(?4, permission_mode)
                 WHERE id = ?1",
                params![session, seq, time, permission_mode],
            )?;
            if let Some(gauge) = gauge {
                gauge.apply(connection, &session, &time)?;
            }
            record_usage_inputs(connection, &session, at, &readings, message.as_ref())
        });
    }

    pub fn set_state(&self, session: Uuid, state: SessionState) {
        self.write("session state", move |connection| {
            let (name, exit_code, gate_failure) = match &state {
                SessionState::Running => ("running", None, None),
                SessionState::Stopped => ("stopped", None, None),
                SessionState::Exited { code } => ("exited", *code, None),
                SessionState::GateFailed { reason } => (
                    "gate_failed",
                    None,
                    Some(serde_json::to_string(reason).map_err(json_error)?),
                ),
            };
            connection.execute(
                "UPDATE sessions SET state = ?2, exit_code = ?3, gate_failure = ?4 WHERE id = ?1",
                params![session.to_string(), name, exit_code, gate_failure],
            )?;
            Ok(())
        });
    }

    /// Marks every session still recorded as running as stopped: after a restart
    /// none of them has a child.
    pub async fn stop_running(&self) -> Result<()> {
        self.call(|connection| {
            connection.execute(
                "UPDATE sessions SET state = 'stopped' WHERE state = 'running'",
                [],
            )?;
            Ok(())
        })
        .await
    }

    pub fn record_usage(&self, time: DateTime<Utc>, session: Option<Uuid>, response: String) {
        let readings = match serde_json::from_str::<GetUsageResponse>(&response) {
            Ok(parsed) => inputs::from_get_usage(&parsed),
            Err(err) => {
                tracing::debug!("get_usage response without readings: {err}");
                Vec::new()
            }
        };
        self.write("usage sample", move |connection| {
            let session = session.map(|id| id.to_string());
            connection.execute(
                "INSERT INTO usage_samples (time, session_id, response) VALUES (?1, ?2, ?3)",
                params![format_time(time), session, response],
            )?;
            let db = UsageDb::new(connection);
            let at = Timestamp::from_datetime(time);
            for reading in readings {
                db.record(reading.kind, reading.state, session.as_deref(), at)?;
            }
            Ok(())
        });
    }

    /// Stores a `get_context_usage` reply as the session's context; resolves to
    /// whether that changed the stored values.
    pub async fn record_context_usage(
        &self,
        session: Uuid,
        reply: ContextUsageResponse,
    ) -> Result<bool> {
        let categories = serde_json::to_string(&reply.categories)?;
        self.call(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET context_source = 'cli', context_tokens = ?2,
                     context_max_tokens = ?3, context_autocompact = ?4, context_categories = ?5
                 WHERE id = ?1 AND NOT (context_source IS 'cli' AND context_tokens IS ?2
                     AND context_max_tokens IS ?3 AND context_autocompact IS ?4
                     AND context_categories IS ?5)",
                params![
                    session.to_string(),
                    reply.total_tokens,
                    reply.max_tokens,
                    reply.auto_compact_threshold,
                    categories
                ],
            )?;
            Ok(changed > 0)
        })
        .await
    }

    /// Folds what has elapsed and projects both windows, reporting on `sessions`.
    /// Local time decides which hour of the week a moment falls in.
    pub async fn usage_snapshot(
        &self,
        sessions: Vec<SessionContext>,
        now: DateTime<Utc>,
    ) -> Result<Snapshot> {
        self.call(move |connection| {
            usage::snapshot(
                &UsageDb::new(connection),
                &sessions,
                Timestamp::from_datetime(now),
                &Local,
            )
        })
        .await
    }

    /// Most recently updated first.
    pub async fn sessions(&self) -> Result<Vec<SessionRow>> {
        self.call(|connection| {
            let mut statement =
                connection.prepare(&format!("{SESSION_SELECT} ORDER BY updated_at DESC"))?;
            let rows = statement.query_map([], RawSession::read)?;
            rows.map(|row| row?.parse()).collect()
        })
        .await
    }

    pub async fn session(&self, id: Uuid) -> Result<Option<SessionRow>> {
        self.call(move |connection| {
            connection
                .query_row(
                    &format!("{SESSION_SELECT} WHERE id = ?1"),
                    [id.to_string()],
                    RawSession::read,
                )
                .optional()?
                .map(RawSession::parse)
                .transpose()
        })
        .await
    }

    /// Up to `limit` events with a seq above `after`, in seq order.
    pub async fn events_after(
        &self,
        session: Uuid,
        after: u64,
        limit: u32,
    ) -> Result<Vec<StoredEvent>> {
        self.call(move |connection| {
            let mut statement = connection.prepare(
                "SELECT seq, time, kind, payload FROM events
                 WHERE session_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
            )?;
            let rows = statement.query_map(params![session.to_string(), after, limit], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            rows.map(|row| {
                let (seq, time, kind, payload) = row?;
                Ok(StoredEvent {
                    seq,
                    time: parse_time(&time)?,
                    kind: EventKind::parse(&kind)?,
                    payload,
                })
            })
            .collect()
        })
        .await
    }

    pub async fn latest_usage(&self) -> Result<Option<UsageSample>> {
        self.call(|connection| {
            connection
                .query_row(
                    "SELECT time, session_id, response FROM usage_samples
                     ORDER BY time DESC, id DESC LIMIT 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?
                .map(|(time, session, response)| {
                    Ok(UsageSample {
                        time: parse_time(&time)?,
                        session_id: session.as_deref().map(Uuid::parse_str).transpose()?,
                        response,
                    })
                })
                .transpose()
        })
        .await
    }

    /// Resolves once every job queued before it is committed.
    pub async fn flush(&self) -> Result<()> {
        self.call(|_| Ok(())).await
    }

    fn submit(&self, job: Job) {
        if self.jobs.send(job).is_err() {
            tracing::warn!("history writer stopped");
        }
    }

    /// Queues a write; a failure is logged.
    fn write(
        &self,
        what: &'static str,
        job: impl FnOnce(&Connection) -> Result<()> + Send + 'static,
    ) {
        self.submit(Box::new(move |connection| {
            if let Err(err) = job(connection) {
                tracing::warn!("history {what}: {err:#}");
            }
        }));
    }

    async fn call<T: Send + 'static>(
        &self,
        job: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (reply, response) = oneshot::channel();
        self.submit(Box::new(move |connection| {
            let _ = reply.send(job(connection));
        }));
        response
            .await
            .map_err(|_| anyhow!("history writer stopped"))?
    }
}

fn run_writer(connection: Connection, queue: mpsc::Receiver<Job>) {
    while let Ok(first) = queue.recv() {
        let batch: Vec<Job> = std::iter::once(first)
            .chain(queue.try_iter().take(MAX_BATCH - 1))
            .collect();
        match connection.unchecked_transaction() {
            Ok(transaction) => {
                for job in batch {
                    job(&transaction);
                }
                if let Err(err) = transaction.commit() {
                    tracing::warn!("history commit: {err}");
                }
            }
            Err(err) => {
                tracing::warn!("history transaction: {err}");
                for job in batch {
                    job(&connection);
                }
            }
        }
    }
}

fn record_usage_inputs(
    connection: &Connection,
    session: &str,
    at: Timestamp,
    readings: &[Reading],
    message: Option<&MessageTokens>,
) -> Result<()> {
    let db = UsageDb::new(connection);
    for reading in readings {
        db.record(reading.kind, reading.state, Some(session), at)?;
    }
    if let Some(message) = message {
        db.record_message(session, at, message)?;
    }
    Ok(())
}

/// Brings the schema to `MIGRATIONS.len()`, one transaction per migration.
pub fn migrate(connection: &Connection) -> Result<()> {
    let version: usize = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > MIGRATIONS.len() {
        bail!(
            "schema version {version} is newer than this build ({})",
            MIGRATIONS.len()
        );
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(sql)?;
        transaction.pragma_update(None, "user_version", index + 1)?;
        transaction.commit()?;
    }
    Ok(())
}

const SESSION_SELECT: &str = "SELECT id, cwd, created_at, updated_at, last_seq, state, exit_code, gate_failure, permission_mode, cache_request_at, cache_ttl_secs, context_tokens, context_window, context_source, context_max_tokens, context_autocompact, context_categories, claude_session_id, claude_session_fresh FROM sessions";

struct RawSession {
    id: String,
    cwd: String,
    created_at: String,
    updated_at: String,
    last_seq: u64,
    state: String,
    exit_code: Option<i32>,
    gate_failure: Option<String>,
    permission_mode: Option<String>,
    cache_request_at: Option<String>,
    cache_ttl_secs: Option<u32>,
    context_tokens: Option<u64>,
    context_window: Option<u64>,
    context_source: Option<String>,
    context_max_tokens: Option<u64>,
    context_autocompact: Option<u64>,
    context_categories: Option<String>,
    claude_session_id: Option<String>,
    claude_session_fresh: bool,
}

impl RawSession {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            cwd: row.get(1)?,
            created_at: row.get(2)?,
            updated_at: row.get(3)?,
            last_seq: row.get(4)?,
            state: row.get(5)?,
            exit_code: row.get(6)?,
            gate_failure: row.get(7)?,
            permission_mode: row.get(8)?,
            cache_request_at: row.get(9)?,
            cache_ttl_secs: row.get(10)?,
            context_tokens: row.get(11)?,
            context_window: row.get(12)?,
            context_source: row.get(13)?,
            context_max_tokens: row.get(14)?,
            context_autocompact: row.get(15)?,
            context_categories: row.get(16)?,
            claude_session_id: row.get(17)?,
            claude_session_fresh: row.get(18)?,
        })
    }

    fn parse(self) -> Result<SessionRow> {
        let state = match self.state.as_str() {
            "running" => SessionState::Running,
            "stopped" => SessionState::Stopped,
            "exited" => SessionState::Exited {
                code: self.exit_code,
            },
            "gate_failed" => SessionState::GateFailed {
                reason: serde_json::from_str(
                    self.gate_failure
                        .as_deref()
                        .context("gate_failed without a reason")?,
                )?,
            },
            other => bail!("unknown session state {other}"),
        };
        let cache = match (self.cache_request_at, self.cache_ttl_secs) {
            (Some(at), Some(ttl_secs)) => Some(Cache {
                last_request_at: parse_time(&at)?,
                ttl_secs,
            }),
            _ => None,
        };
        let source = match self.context_source.as_deref() {
            Some("cli") => ContextSource::Cli,
            _ => ContextSource::Estimate,
        };
        let categories = self
            .context_categories
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        let context = self
            .context_tokens
            .zip(self.context_max_tokens.or(self.context_window))
            .and_then(|(used, max)| gauges::Context::new(used, max, source))
            .map(|context| gauges::Context {
                autocompact_threshold: self.context_autocompact,
                categories,
                ..context
            });
        let id = Uuid::parse_str(&self.id)?;
        let claude_session = match self.claude_session_id.as_deref() {
            Some(text) => Uuid::parse_str(text)?,
            None => id,
        };
        let launch = if self.claude_session_fresh {
            Launch::New(claude_session)
        } else {
            Launch::Resume(claude_session)
        };
        Ok(SessionRow {
            id,
            cwd: PathBuf::from(self.cwd),
            created_at: parse_time(&self.created_at)?,
            updated_at: parse_time(&self.updated_at)?,
            last_seq: self.last_seq,
            state,
            permission_mode: self.permission_mode,
            cache,
            context,
            launch,
        })
    }
}

impl EventKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Host => "host",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text {
            "line" => Ok(Self::Line),
            "host" => Ok(Self::Host),
            other => bail!("unknown event kind {other}"),
        }
    }
}

/// Fixed width, so text order is time order.
fn format_time(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_time(text: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(text)
        .with_context(|| format!("time {text}"))?
        .with_timezone(&Utc))
}

fn json_error(err: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::child::Launch;
    use crate::session::{HostEvent, Line};

    fn event(seq: u64, body: EventBody) -> Event {
        Event {
            seq,
            time: Utc::now(),
            body,
        }
    }

    fn line(raw: &str) -> EventBody {
        EventBody::Line(Box::new(Line {
            raw: raw.into(),
            parsed: None,
        }))
    }

    #[test]
    fn migration_from_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        std::fs::write(&path, b"").unwrap();
        let connection = Connection::open(&path).unwrap();
        migrate(&connection).unwrap();
        migrate(&connection).unwrap();
        let version: usize = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len());
        let tables: Vec<String> = connection
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            tables,
            [
                "events",
                "sessions",
                "usage_closed_hours",
                "usage_messages",
                "usage_meta",
                "usage_priors",
                "usage_readings",
                "usage_samples",
                "usage_slots"
            ]
        );
    }

    /// The `out` lines of the `basic` recording, and its `get_usage` answer.
    fn basic_recording() -> (Vec<String>, String) {
        let recording = crate::replay::Recording::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("spike/runs/basic.ndjson"),
        )
        .unwrap();
        let lines: Vec<String> = recording
            .steps
            .iter()
            .filter(|step| step.dir == crate::replay::Direction::Out)
            .map(|step| step.msg.to_string())
            .collect();
        let usage = recording
            .steps
            .iter()
            .find(|step| step.msg["response"]["response"]["rate_limits_available"] == true)
            .map(|step| step.msg["response"]["response"].to_string())
            .unwrap();
        (lines, usage)
    }

    #[tokio::test]
    async fn usage_inputs_are_recorded_as_they_arrive() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let id = Uuid::new_v4();
        let cwd = dir.path().join("project");
        store.session_started(id, &cwd, Utc::now(), None);
        let (lines, usage) = basic_recording();
        for (seq, raw) in lines.iter().enumerate() {
            let body = EventBody::Line(Box::new(Line {
                raw: raw.clone(),
                parsed: serde_json::from_str(raw).ok(),
            }));
            store.record_event(id, &event(seq as u64 + 1, body));
        }
        store.record_usage(Utc::now(), Some(id), usage);

        let recorded_at = parse_time("2026-09-27T11:05:00Z").unwrap();
        let context = SessionContext {
            id,
            cwd,
            created_at: recorded_at,
            permission_mode: Some("bypassPermissions".into()),
        };
        let snapshot = store
            .usage_snapshot(vec![context], recorded_at)
            .await
            .unwrap();
        let five_hour = snapshot.five_hour.expect("a 5h window");
        assert_eq!(five_hour.used, 7.0);
        assert_eq!(
            five_hour.resets_at,
            parse_time("2026-09-27T13:00:00Z").unwrap()
        );
        assert!(five_hour.projected >= 7.0);
        let seven_day = snapshot.seven_day.expect("a 7d window");
        assert_eq!(seven_day.used, 51.0);
        assert_eq!(snapshot.history.readings, 2, "the sample repeats the event");
        let session = &snapshot.sessions[0];
        assert!(session.bypass);
        assert_eq!(session.model_mix.len(), 1);
        assert_eq!(session.model_mix[0].share, 1.0);
    }

    /// Stores the `out` lines of a recording for `id`, each at `start` plus its offset.
    fn record_recording(store: &Store, id: Uuid, name: &str, start: DateTime<Utc>) {
        let recording = crate::replay::Recording::load(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("spike/runs")
                .join(format!("{name}.ndjson")),
        )
        .unwrap();
        let outs = recording
            .steps
            .iter()
            .filter(|step| step.dir == crate::replay::Direction::Out);
        for (seq, step) in outs.enumerate() {
            let raw = step.msg.to_string();
            let body = EventBody::Line(Box::new(Line {
                parsed: serde_json::from_str(&raw).ok(),
                raw,
            }));
            store.record_event(
                id,
                &Event {
                    seq: seq as u64 + 1,
                    time: start + chrono::Duration::milliseconds(step.t as i64),
                    body,
                },
            );
        }
    }

    #[tokio::test]
    async fn gauges_follow_the_main_conversation_and_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let store = Store::open(&path).unwrap();
        let id = Uuid::new_v4();
        store.session_started(id, dir.path(), Utc::now(), None);
        let start = parse_time("2026-09-27T10:00:00Z").unwrap();
        record_recording(&store, id, "cd", start);
        let row = store.session(id).await.unwrap().unwrap();
        // The last response streams two lines, at 7557 and 7579 ms.
        assert_eq!(
            row.cache,
            Some(Cache {
                last_request_at: start + chrono::Duration::milliseconds(7557),
                ttl_secs: gauges::ONE_HOUR_TTL_SECS,
            })
        );
        let context = row.context.expect("a context estimate");
        // input 2, cache read 28990, cache creation 2056.
        assert_eq!(context.used_tokens, 31_048);
        assert_eq!(context.max_tokens, 1_000_000);

        // A response that writes nothing keeps the TTL; its time still counts.
        let empty_write = r#"{"type":"assistant","parent_tool_use_id":null,"session_id":"s","uuid":"u",
            "message":{"id":"msg_x","model":"claude-opus-5-5","content":[],
            "usage":{"input_tokens":5,"cache_read_input_tokens":40000,"cache_creation_input_tokens":0,
            "cache_creation":{"ephemeral_1h_input_tokens":0,"ephemeral_5m_input_tokens":0}}}}"#;
        let later = start + chrono::Duration::minutes(3);
        store.record_event(
            id,
            &Event {
                seq: 1000,
                time: later,
                body: EventBody::Line(Box::new(Line {
                    raw: empty_write.into(),
                    parsed: serde_json::from_str(empty_write).ok(),
                })),
            },
        );
        store.flush().await.unwrap();
        drop(store);

        let reopened = Store::open(&path).unwrap();
        let row = reopened.session(id).await.unwrap().unwrap();
        assert_eq!(
            row.cache,
            Some(Cache {
                last_request_at: later,
                ttl_secs: gauges::ONE_HOUR_TTL_SECS,
            })
        );
        assert_eq!(row.context.map(|context| context.used_tokens), Some(40_005));
    }

    fn context_reply(total_tokens: u64) -> ContextUsageResponse {
        serde_json::from_value(serde_json::json!({
            "totalTokens": total_tokens, "maxTokens": 967_000, "rawMaxTokens": 1_000_000,
            "percentage": 2, "autoCompactThreshold": 900_000, "apiUsage": null,
            "categories": [{"name": "Messages", "tokens": total_tokens, "color": "x", "kind": "used"}],
            "skills": [{"name": "unread"}]
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn cli_context_until_the_next_request_and_across_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let store = Store::open(&path).unwrap();
        let id = Uuid::new_v4();
        store.session_started(id, dir.path(), Utc::now(), None);
        assert!(
            store
                .record_context_usage(id, context_reply(12_000))
                .await
                .unwrap()
        );
        assert!(
            !store
                .record_context_usage(id, context_reply(12_000))
                .await
                .unwrap()
        );
        let context = store.session(id).await.unwrap().unwrap().context.unwrap();
        assert_eq!(context.source, ContextSource::Cli);
        assert_eq!(context.used_tokens, 12_000);
        assert_eq!(context.max_tokens, 967_000);
        assert_eq!(context.autocompact_threshold, Some(900_000));
        assert_eq!(
            context.categories,
            [crate::protocol::ContextCategory {
                name: "Messages".into(),
                tokens: 12_000,
                kind: "used".into(),
            }]
        );

        // The `cd` recording ends on a prompt of 31048 over a 1000000 window.
        record_recording(&store, id, "cd", Utc::now());
        let context = store.session(id).await.unwrap().unwrap().context.unwrap();
        assert_eq!(context.source, ContextSource::Estimate);
        assert_eq!(context.used_tokens, 31_048);
        assert_eq!(context.max_tokens, 967_000);
        assert_eq!(context.autocompact_threshold, Some(900_000));

        assert!(
            store
                .record_context_usage(id, context_reply(31_048))
                .await
                .unwrap()
        );
        drop(store);
        let reopened = Store::open(&path).unwrap();
        let context = reopened
            .session(id)
            .await
            .unwrap()
            .unwrap()
            .context
            .unwrap();
        assert_eq!(context.source, ContextSource::Cli);
        assert_eq!(context.used_tokens, 31_048);
        assert_eq!(context.max_tokens, 967_000);
        assert_eq!(context.categories[0].tokens, 31_048);
    }

    #[tokio::test]
    async fn a_session_without_requests_has_no_gauges() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let id = Uuid::new_v4();
        store.session_started(id, dir.path(), Utc::now(), None);
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.cache, None);
        assert_eq!(row.context, None);
    }

    #[tokio::test]
    async fn migration_from_a_v1_file_keeps_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let id = Uuid::new_v4();
        {
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch(MIGRATIONS[0]).unwrap();
            connection.pragma_update(None, "user_version", 1).unwrap();
            connection
                .execute(
                    "INSERT INTO sessions (id, cwd, created_at, updated_at, state)
                     VALUES (?1, 'C:\\p', ?2, ?2, 'stopped')",
                    params![id.to_string(), format_time(Utc::now())],
                )
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.state, SessionState::Stopped);
        assert_eq!(row.permission_mode, None);
        store.set_permission_mode(id, "plan".into()).await.unwrap();
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.permission_mode.as_deref(), Some("plan"));
    }

    #[tokio::test]
    async fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("history.db");
        let store = Store::open(&path).unwrap();
        let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
        let cwd = dir.path().join("project");
        store.session_started(first, &cwd, Utc::now(), Some("default".into()));
        store.session_started(second, &cwd, Utc::now(), None);
        let started = event(
            1,
            EventBody::Host(HostEvent::Started {
                launch: Launch::New(first),
                cwd: cwd.clone(),
            }),
        );
        let raw = r#"{"type":"system","subtype":"init","b":1,  "a":"é"}"#;
        store.record_event(first, &started);
        store.record_event(first, &event(2, line(raw)));
        store.record_event(first, &event(3, line("not json")));
        let reason = GateFailure::ApiKeySource {
            source: "ANTHROPIC_API_KEY".into(),
        };
        store.set_state(
            first,
            SessionState::GateFailed {
                reason: reason.clone(),
            },
        );
        store.record_event(second, &event(1, line("{}")));
        store.set_state(second, SessionState::Exited { code: Some(3) });
        store.record_usage(Utc::now(), Some(first), r#"{"n":1}"#.into());
        store.record_usage(Utc::now(), None, r#"{"n":2}"#.into());

        let sessions = store.sessions().await.unwrap();
        assert_eq!(
            sessions.iter().map(|row| row.id).collect::<Vec<_>>(),
            [second, first]
        );
        let row = store.session(first).await.unwrap().unwrap();
        assert_eq!(row.cwd, cwd);
        assert_eq!(row.last_seq, 3);
        assert_eq!(row.state, SessionState::GateFailed { reason });
        assert_eq!(sessions[0].state, SessionState::Exited { code: Some(3) });
        assert_eq!(row.permission_mode.as_deref(), Some("default"));
        assert_eq!(sessions[0].permission_mode, None);
        assert_eq!(store.session(Uuid::new_v4()).await.unwrap(), None);

        let events = store.events_after(first, 0, 100).await.unwrap();
        assert_eq!(
            events.iter().map(|event| event.seq).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(events[0].kind, EventKind::Host);
        assert_eq!(
            serde_json::from_str::<HostEvent>(&events[0].payload).unwrap(),
            match &started.body {
                EventBody::Host(host) => host.clone(),
                EventBody::Line(_) => unreachable!(),
            }
        );
        assert_eq!(
            events[0].time,
            parse_time(&format_time(started.time)).unwrap()
        );
        assert_eq!(events[1].kind, EventKind::Line);
        assert_eq!(events[1].payload, raw);
        assert_eq!(events[2].payload, "not json");
        let tail = store.events_after(first, 1, 1).await.unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].seq, 2);

        let usage = store.latest_usage().await.unwrap().unwrap();
        assert_eq!(usage.session_id, None);
        assert_eq!(usage.response, r#"{"n":2}"#);

        let mode_event = event(
            4,
            EventBody::Host(HostEvent::PermissionMode {
                mode: "plan".into(),
            }),
        );
        store.record_event(first, &mode_event);
        store.session_started(first, &cwd, Utc::now(), None);
        store.stop_running().await.unwrap();
        let row = store.session(first).await.unwrap().unwrap();
        assert_eq!(row.state, SessionState::Stopped);
        assert_eq!(row.last_seq, 4);
        assert_eq!(row.permission_mode.as_deref(), Some("plan"));
        store
            .set_permission_mode(first, "acceptEdits".into())
            .await
            .unwrap();
        let row = store.session(first).await.unwrap().unwrap();
        assert_eq!(row.permission_mode.as_deref(), Some("acceptEdits"));
        assert!(
            store
                .set_permission_mode(Uuid::new_v4(), "plan".into())
                .await
                .is_err()
        );

        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.sessions().await.unwrap().len(), 2);
        let connection = Connection::open(&path).unwrap();
        let mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }
}
