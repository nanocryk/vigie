//! Session history in SQLite: sessions, their events, `get_usage` samples, and
//! the usage tables fed from both.
//!
//! One writer thread owns the connection and runs every job in submission order,
//! writes and reads alike. A read therefore sees every write queued before it,
//! and nothing blocks the async runtime.

mod bash_asks;
mod catalog;
mod commands;
mod events;
mod held;
mod messages;
mod plans;
pub mod prune;
mod push;
mod samples;
mod sessions;

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;
use tokio::sync::oneshot;
use uuid::Uuid;

pub use bash_asks::{BashAnswer, BashAsk, BashAskFilter, Denial};
pub use catalog::SharedCatalog;
pub use events::{EventKind, PageStart, StoredEvent};
pub use held::{HeldEdit, HeldMessage, HeldMode};
pub use messages::{MessageQuery, Role, StoredMessage};
pub use samples::UsageSample;
pub use sessions::{SessionRow, SessionState, Unread};

use crate::{state_dir, usage};

/// Applied in order; `PRAGMA user_version` counts the ones already applied.
const MIGRATIONS: [&str; 23] = [
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
    // When the discussion was archived; NULL while it is not.
    "ALTER TABLE sessions ADD COLUMN archived_at TEXT;",
    // The model and effort every launch passes (NULL lets the CLI pick), and when
    // a change of either made the cache count cold, until the next main request.
    r#"
ALTER TABLE sessions ADD COLUMN model TEXT;
ALTER TABLE sessions ADD COLUMN effort TEXT;
ALTER TABLE sessions ADD COLUMN settings_changed_at TEXT;
"#,
    // The model and effort the CLI last reported applying, from `get_settings`.
    r#"
ALTER TABLE sessions ADD COLUMN applied_model TEXT;
ALTER TABLE sessions ADD COLUMN applied_effort TEXT;
"#,
    // What all discussions share, see `SharedCatalog`: one JSON value per key.
    r#"
CREATE TABLE cli_catalog (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;
"#,
    // Host events by name, newest first from any seq: where a page of history
    // starts and what precedes it. Queries repeat the expression and
    // `kind = 'host'` verbatim, or SQLite does not use it.
    "CREATE INDEX events_host ON events (session_id, json_extract(payload, '$.event'), seq)
         WHERE kind = 'host';",
    // What the user has not seen yet; NULL once seen.
    "ALTER TABLE sessions ADD COLUMN unread TEXT CHECK (unread IN ('finished', 'stopped'));",
    // One Web Push subscription per paired device that turned notifications on.
    r#"
CREATE TABLE push_subscriptions (
    device_id TEXT PRIMARY KEY,
    endpoint TEXT NOT NULL,
    p256dh TEXT NOT NULL,
    auth TEXT NOT NULL
) STRICT;
"#,
    // Main-conversation requests and finished turns, for the calls a message makes.
    r#"
ALTER TABLE sessions ADD COLUMN main_requests INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN turns INTEGER NOT NULL DEFAULT 0;
"#,
    // What a restart would keep, see `advice::RestartMeasure`; NULL until measured.
    r#"
ALTER TABLE sessions ADD COLUMN restart_window_tokens INTEGER;
ALTER TABLE sessions ADD COLUMN restart_seed_tokens INTEGER;
ALTER TABLE sessions ADD COLUMN restart_reread_tokens INTEGER;
ALTER TABLE sessions ADD COLUMN restart_other_cost REAL;
"#,
    // The plan text each plan file last showed for review, keyed by its path.
    r#"
CREATE TABLE plans (
    path TEXT PRIMARY KEY,
    text TEXT NOT NULL
) STRICT;
"#,
    // Display name of the discussion; NULL shows its folder name.
    "ALTER TABLE sessions ADD COLUMN name TEXT;",
    // A Claude session no message reached has no conversation to resume: marks
    // fresh the sessions with no user message, nor a continue seed, since their
    // latest restart.
    r#"
UPDATE sessions SET claude_session_fresh = 1
WHERE claude_session_fresh = 0 AND NOT EXISTS (
    SELECT 1 FROM events used
    WHERE used.session_id = sessions.id AND used.kind = 'host'
        AND (json_extract(used.payload, '$.event') = 'user_sent'
            OR (json_extract(used.payload, '$.event') = 'restarted'
                AND json_extract(used.payload, '$.mode') = 'continue'))
        AND used.seq >= COALESCE((
            SELECT MAX(restart.seq) FROM events restart
            WHERE restart.session_id = sessions.id AND restart.kind = 'host'
                AND json_extract(restart.payload, '$.event') = 'restarted'
        ), 0)
);
"#,
    // Messages vigie keeps for a discussion until the user or the end of a turn sends them.
    r#"
CREATE TABLE held_messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions (id),
    position INTEGER NOT NULL,
    text TEXT NOT NULL,
    attachments TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('held', 'turn_end')),
    created_at TEXT NOT NULL
) STRICT;
CREATE INDEX held_messages_by_session ON held_messages (session_id, position);
"#,
    bash_asks::MIGRATION,
    bash_asks::FILTERS_MIGRATION,
    messages::MIGRATION,
    messages::SYNTHETIC_MIGRATION,
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

impl Store {
    /// `~/.vigie/history.db`.
    pub fn default_path() -> Result<PathBuf> {
        Ok(state_dir::path()?.join("history.db"))
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
        // A checkpoint keeps the WAL file at its peak size (a VACUUM writes the whole db into it).
        connection.pragma_update(None, "journal_size_limit", 32 * 1024 * 1024)?;
        migrate(&connection).with_context(|| format!("migrate {}", path.display()))?;
        // Nothing is pruned unless the backup succeeds; the next launch tries again.
        if let Err(err) = prune::first_prune(&connection, path) {
            tracing::warn!("first prune of {}: {err:#}", path.display());
        }
        let (jobs, queue) = mpsc::channel();
        std::thread::Builder::new()
            .name("vigie-history".into())
            .spawn(move || run_writer(connection, queue))
            .context("spawn history writer")?;
        Ok(Self { jobs })
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
        if *sql == messages::MIGRATION {
            messages::backfill(&transaction)?;
        }
        transaction.pragma_update(None, "user_version", index + 1)?;
        transaction.commit()?;
    }
    Ok(())
}

/// Fails for an `UPDATE` of `session` that matched no row.
fn known_session(changed: usize, session: Uuid) -> Result<()> {
    if changed == 0 {
        bail!("unknown session {session}");
    }
    Ok(())
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
mod testing {
    use std::path::PathBuf;

    use chrono::Utc;
    use uuid::Uuid;

    use super::Store;
    use crate::session::{Event, EventBody, Line};

    /// A store in a fresh folder, with one session started in that folder.
    pub struct Fixture {
        pub dir: tempfile::TempDir,
        pub path: PathBuf,
        pub store: Store,
        pub id: Uuid,
    }

    pub fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let store = Store::open(&path).unwrap();
        let id = Uuid::new_v4();
        store.session_started(id, dir.path(), Utc::now(), None);
        Fixture {
            dir,
            path,
            store,
            id,
        }
    }

    pub fn event(seq: u64, body: EventBody) -> Event {
        Event {
            seq,
            time: Utc::now(),
            body,
        }
    }

    pub fn line(raw: &str) -> EventBody {
        EventBody::Line(Box::new(Line {
            raw: raw.into(),
            parsed: None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::testing::{event, line};
    use super::*;
    use crate::claude::child::Launch;
    use crate::claude::protocol::PermissionMode;
    use crate::session::{EventBody, GateFailure, HostEvent};

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
                "bash_ask_filters",
                "bash_asks",
                "cli_catalog",
                "events",
                "held_messages",
                "history_messages",
                "plans",
                "push_subscriptions",
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

    #[tokio::test]
    async fn a_claude_session_resumes_only_once_a_message_reached_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let id = Uuid::new_v4();
        store.session_started(id, dir.path(), Utc::now(), None);
        store.session_started(id, dir.path(), Utc::now(), None);
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.launch, Launch::New(id));
        store.claude_session_used(id);
        store.session_started(id, dir.path(), Utc::now(), None);
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.launch, Launch::Resume(id));
    }

    #[tokio::test]
    async fn migration_marks_fresh_the_claude_sessions_no_message_reached() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let [unused, used, blank, continued] = [(); 4].map(|_| Uuid::new_v4());
        {
            let connection = Connection::open(&path).unwrap();
            for migration in &MIGRATIONS[..17] {
                connection.execute_batch(migration).unwrap();
            }
            connection.pragma_update(None, "user_version", 17).unwrap();
            let now = format_time(Utc::now());
            for id in [unused, used, blank, continued] {
                connection
                    .execute(
                        "INSERT INTO sessions (id, cwd, created_at, updated_at, state)
                         VALUES (?1, 'C:\\p', ?2, ?2, 'stopped')",
                        params![id.to_string(), now],
                    )
                    .unwrap();
            }
            let events = [
                (used, 1, r#"{"event":"user_sent"}"#),
                (blank, 1, r#"{"event":"user_sent"}"#),
                (blank, 2, r#"{"event":"restarted","mode":"blank"}"#),
                (continued, 1, r#"{"event":"restarted","mode":"continue"}"#),
            ];
            for (id, seq, payload) in events {
                connection
                    .execute(
                        "INSERT INTO events (session_id, seq, time, kind, payload)
                         VALUES (?1, ?2, ?3, 'host', ?4)",
                        params![id.to_string(), seq, now, payload],
                    )
                    .unwrap();
            }
        }
        let store = Store::open(&path).unwrap();
        for (id, fresh) in [
            (unused, true),
            (used, false),
            (blank, true),
            (continued, false),
        ] {
            let row = store.session(id).await.unwrap().unwrap();
            assert_eq!(matches!(row.launch, Launch::New(_)), fresh, "{id}");
        }
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
        assert_eq!(row.permission_mode, Some(PermissionMode::Plan));
        assert_eq!(row.archived_at, None);
        let at = parse_time(&format_time(Utc::now())).unwrap();
        store.set_archived(id, Some(at)).await.unwrap();
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.archived_at, Some(at));
        store.set_archived(id, None).await.unwrap();
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.archived_at, None);
        assert!(store.set_archived(Uuid::new_v4(), None).await.is_err());
        assert_eq!(row.name, None);
        store
            .set_name(id, Some("parser idea".into()))
            .await
            .unwrap();
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.name.as_deref(), Some("parser idea"));
        store.set_name(id, None).await.unwrap();
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.name, None);
        assert!(store.set_name(Uuid::new_v4(), None).await.is_err());
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
        assert_eq!(row.permission_mode, Some(PermissionMode::Default));
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
        assert_eq!(row.permission_mode, Some(PermissionMode::Plan));
        store
            .set_permission_mode(first, "acceptEdits".into())
            .await
            .unwrap();
        let row = store.session(first).await.unwrap().unwrap();
        assert_eq!(row.permission_mode, Some(PermissionMode::AcceptEdits));
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
