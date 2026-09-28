//! Drives `SessionManager` against the fake claude and checks what history keeps.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::RecvError};
use uuid::Uuid;
use vigie2::child::{Launch, Program};
use vigie2::gauges::ContextSource;
use vigie2::manager::{Probe, SessionManager};
use vigie2::protocol::{AppliedSettings, Inbound};
use vigie2::replay::{
    CANNED_AUTOCOMPACT_THRESHOLD, CANNED_CONTEXT_MAX, CANNED_CONTEXT_START, CANNED_DEFAULT_EFFORT,
    CANNED_DEFAULT_MODEL, Direction, Recording,
};
use vigie2::session::{Answer, Event, EventBody, HostEvent, RestartMode, Session, StopReason};
use vigie2::store::{EventKind, SessionState, SharedCatalog, Store, StoredEvent, Unread};

const TIMEOUT: Duration = Duration::from_secs(20);
const ALL: u32 = 100_000;

/// Initialization only; the fake then waits for stdin to close.
const INIT_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
"#;

fn recording(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("spike/runs")
        .join(format!("{name}.ndjson"))
}

fn fake(recording: &Path) -> Program {
    Program {
        path: env!("CARGO_BIN_EXE_fake-claude").into(),
        leading_args: vec![recording.into()],
    }
}

struct Env {
    dir: TempDir,
    db: PathBuf,
    cwd: PathBuf,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("state").join("history.db");
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        Self { dir, db, cwd }
    }

    fn script(&self, name: &str, text: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, text.trim()).unwrap();
        path
    }

    async fn manager(&self, recording: &Path) -> SessionManager {
        let store = Store::open(&self.db).expect("open store");
        SessionManager::start(store, fake(recording))
            .await
            .expect("start manager")
    }
}

async fn until_result(events: &mut broadcast::Receiver<Arc<Event>>) {
    loop {
        let event = tokio::time::timeout(TIMEOUT, events.recv())
            .await
            .expect("session went quiet")
            .expect("event stream");
        if let EventBody::Line(line) = &event.body
            && matches!(line.parsed, Some(Inbound::Result(_)))
        {
            return;
        }
    }
}

async fn stop(session: &Session) {
    tokio::time::timeout(TIMEOUT, session.stop())
        .await
        .expect("session did not stop");
}

fn host(event: &StoredEvent) -> HostEvent {
    assert_eq!(event.kind, EventKind::Host);
    serde_json::from_str(&event.payload).expect("host event")
}

fn assert_contiguous(events: &[StoredEvent], first: u64) {
    let seqs: Vec<u64> = events.iter().map(|event| event.seq).collect();
    let expected: Vec<u64> = (first..first + events.len() as u64).collect();
    assert_eq!(seqs, expected);
}

fn assert_clean_exit(event: &StoredEvent) {
    match host(event) {
        HostEvent::ChildExited {
            code, stderr_tail, ..
        } => {
            assert_eq!(code, Some(0), "fake claude failed: {stderr_tail:?}")
        }
        other => panic!("expected ChildExited, got {other:?}"),
    }
}

fn is_usage_answer(line: &Value) -> bool {
    line["type"] == "control_response"
        && line["response"]["subtype"] == "success"
        && line["response"]["response"]["rate_limits_available"] == true
}

/// Every stdout line of the recording, as the fake writes it for `session`,
/// must appear byte for byte and in order among `stored`; the only other lines
/// allowed are answers to unscripted `get_usage` requests.
fn assert_lines_match_recording(stored: &[StoredEvent], name: &str, session: Uuid) {
    let recorded: Vec<Value> = Recording::load(&recording(name))
        .unwrap()
        .steps
        .into_iter()
        .filter(|step| step.dir == Direction::Out)
        .map(|mut step| {
            if let Some(slot) = step.msg.get_mut("session_id") {
                *slot = Value::String(session.to_string());
            }
            step.msg
        })
        .collect();
    let mut expected = recorded.iter().peekable();
    for event in stored.iter().filter(|event| event.kind == EventKind::Line) {
        let got: Value = serde_json::from_str(&event.payload).expect("stored line is JSON");
        if let Some(next) = expected.peek() {
            let mut want = (*next).clone();
            if want["type"] == "control_response"
                && let Some(slot) = want["response"].get_mut("request_id")
            {
                *slot = got["response"]["request_id"].clone();
            }
            if serde_json::to_string(&want).unwrap() == event.payload {
                expected.next();
                continue;
            }
        }
        assert!(
            is_usage_answer(&got),
            "seq {}: unexpected line {}",
            event.seq,
            event.payload
        );
    }
    assert_eq!(expected.count(), 0, "recorded lines missing from history");
}

#[tokio::test]
async fn basic_run_is_persisted_with_usage_samples() {
    let env = Env::new();
    let manager = env.manager(&recording("basic")).await;
    let session = manager.create(env.cwd.clone()).await.unwrap();
    let id = session.id();
    let mut events = session.subscribe();
    let mut changes = manager.subscribe_changes();

    session.send("Reply with exactly: ok", None).await.unwrap();
    until_result(&mut events).await;
    // The main request moves the gauges, and the list announces it.
    tokio::time::timeout(TIMEOUT, async {
        loop {
            assert_eq!(changes.recv().await.unwrap(), id);
            if manager.list().await.unwrap()[0].cache.is_some() {
                return;
            }
        }
    })
    .await
    .expect("no announced cache after the first request");
    // The fake expects its scripted `get_usage` before the next message.
    let sample = tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Some(sample) = manager.store().latest_usage().await.unwrap() {
                return sample;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("no usage sample after the turn");
    assert_eq!(sample.session_id, Some(id));
    let response: Value = serde_json::from_str(&sample.response).unwrap();
    assert_eq!(response["rate_limits_available"], true);
    // The CLI reports the first prompt: input 2, cache read 13397, cache creation 15607.
    let context = tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Some(context) = manager.list().await.unwrap()[0].context.clone()
                && context.source == ContextSource::Cli
                && context.used_tokens == 29_006
            {
                return context;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("no CLI context after the turn");
    assert_eq!(context.max_tokens, CANNED_CONTEXT_MAX);

    session
        .send("Reply with exactly: ok again", None)
        .await
        .unwrap();
    until_result(&mut events).await;
    stop(&session).await;
    manager.shutdown().await.unwrap();

    let stored = manager.events_after(id, 0, ALL).await.unwrap();
    assert_contiguous(&stored, 1);
    assert_eq!(
        host(&stored[0]),
        HostEvent::Started {
            launch: Launch::New(id),
            cwd: env.cwd.clone(),
        }
    );
    assert_clean_exit(stored.last().unwrap());
    assert_lines_match_recording(&stored, "basic", id);

    let rows = manager.list().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].cwd, env.cwd);
    assert_eq!(rows[0].last_seq, stored.len() as u64);
    assert_eq!(rows[0].state, SessionState::Stopped);
    assert_eq!(
        rows[0].cache.as_ref().map(|cache| cache.ttl_secs),
        Some(3600)
    );
    let context = rows[0].context.as_ref().expect("a context");
    // The second prompt: input 2, cache read 29004, cache creation 1979.
    assert_eq!(context.used_tokens, 30_985);
    assert_eq!(context.max_tokens, CANNED_CONTEXT_MAX);
    assert!(
        stored
            .iter()
            .all(|event| !event.payload.contains("totalTokens")),
        "a context usage reply was stored"
    );
}

#[tokio::test]
async fn the_context_is_known_before_the_first_turn() {
    let env = Env::new();
    let script = env.script("init.ndjson", INIT_SCRIPT);
    let manager = env.manager(&script).await;
    let mut changes = manager.subscribe_changes();
    let session = manager.create(env.cwd.clone()).await.unwrap();
    let id = session.id();
    let context = tokio::time::timeout(TIMEOUT, async {
        loop {
            assert_eq!(changes.recv().await.unwrap(), id);
            if let Some(context) = manager.list().await.unwrap()[0].context.clone() {
                return context;
            }
        }
    })
    .await
    .expect("no announced context before the first turn");
    assert_eq!(context.source, ContextSource::Cli);
    assert_eq!(context.used_tokens, CANNED_CONTEXT_START);
    assert_eq!(context.max_tokens, CANNED_CONTEXT_MAX);
    assert_eq!(
        context.autocompact_threshold,
        Some(CANNED_AUTOCOMPACT_THRESHOLD)
    );
    assert_eq!(context.categories.len(), 3);
    stop(&session).await;
    manager.shutdown().await.unwrap();
    let stored = manager.events_after(id, 0, ALL).await.unwrap();
    assert_clean_exit(stored.last().unwrap());
}

async fn until_applied(
    manager: &SessionManager,
    changes: &mut broadcast::Receiver<Uuid>,
    id: Uuid,
    model: &str,
    effort: &str,
) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let row = manager.store().session(id).await.unwrap().unwrap();
            if row.applied.model.as_deref() == Some(model)
                && row.applied.effort.as_deref() == Some(effort)
            {
                return;
            }
            assert_eq!(changes.recv().await.unwrap(), id);
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no announced {model} and {effort}"));
}

#[tokio::test]
async fn applied_settings_are_asked_at_spawn_and_after_a_change_and_survive_a_reopen() {
    let env = Env::new();
    let script = env.script("init.ndjson", INIT_SCRIPT);
    let manager = env.manager(&script).await;
    let mut changes = manager.subscribe_changes();
    let session = manager.create(env.cwd.clone()).await.unwrap();
    let id = session.id();
    until_applied(
        &manager,
        &mut changes,
        id,
        CANNED_DEFAULT_MODEL,
        CANNED_DEFAULT_EFFORT,
    )
    .await;

    manager
        .set_settings(id, Some(Some("sonnet".into())), Some(Some("high".into())))
        .await
        .unwrap();
    until_applied(&manager, &mut changes, id, "claude-sonnet-5", "high").await;
    stop(&session).await;
    manager.shutdown().await.unwrap();
    let stored = manager.events_after(id, 0, ALL).await.unwrap();
    assert!(
        stored
            .iter()
            .all(|event| !event.payload.contains("\"applied\"")),
        "a settings reply was stored"
    );
    assert_clean_exit(stored.last().unwrap());
    drop((session, manager));

    let manager = env.manager(&script).await;
    let row = manager.store().session(id).await.unwrap().unwrap();
    assert_eq!(row.applied.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(row.applied.effort.as_deref(), Some("high"));
}

/// Initialization, a failed `get_settings`, then a marker line.
const FAILED_SETTINGS_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "set", "request": {"subtype": "get_settings"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "error", "request_id": "set", "error": "unavailable"}}}
{"t": 0, "dir": "out", "msg": {"type": "marker"}}
"#;

#[tokio::test]
async fn a_failed_settings_ask_changes_nothing() {
    let env = Env::new();
    let script = env.script("failed.ndjson", FAILED_SETTINGS_SCRIPT);
    let manager = env.manager(&script).await;
    let session = manager.create(env.cwd.clone()).await.unwrap();
    let id = session.id();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let stored = manager.events_after(id, 0, ALL).await.unwrap();
            if stored.iter().any(|event| event.payload.contains("marker")) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the failed ask was not answered");
    manager.store().flush().await.unwrap();
    let row = manager.store().session(id).await.unwrap().unwrap();
    assert_eq!(row.applied.model, None);
    assert_eq!(row.applied.effort, None);
    stop(&session).await;
    manager.shutdown().await.unwrap();
    let stored = manager.events_after(id, 0, ALL).await.unwrap();
    assert!(
        stored
            .iter()
            .all(|event| !event.payload.contains("unavailable")),
        "the failed reply was stored"
    );
    assert_clean_exit(stored.last().unwrap());
}

/// Initialization, a failed `get_context_usage`, then a marker line.
const FAILED_CONTEXT_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "ctx", "request": {"subtype": "get_context_usage"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "error", "request_id": "ctx", "error": "unavailable"}}}
{"t": 0, "dir": "out", "msg": {"type": "marker"}}
"#;

#[tokio::test]
async fn a_failed_context_ask_changes_nothing() {
    let env = Env::new();
    let script = env.script("failed.ndjson", FAILED_CONTEXT_SCRIPT);
    let manager = env.manager(&script).await;
    let session = manager.create(env.cwd.clone()).await.unwrap();
    let id = session.id();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let stored = manager.events_after(id, 0, ALL).await.unwrap();
            if stored.iter().any(|event| event.payload.contains("marker")) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the failed ask was not answered");
    manager.store().flush().await.unwrap();
    assert_eq!(manager.list().await.unwrap()[0].context, None);
    stop(&session).await;
    manager.shutdown().await.unwrap();
    let stored = manager.events_after(id, 0, ALL).await.unwrap();
    assert!(
        stored
            .iter()
            .all(|event| !event.payload.contains("unavailable")),
        "the failed reply was stored"
    );
    assert_clean_exit(stored.last().unwrap());
}

#[tokio::test]
async fn a_session_resumes_after_a_restart_and_continues_its_seqs() {
    let env = Env::new();
    let script = env.script("init.ndjson", INIT_SCRIPT);
    let manager = env.manager(&script).await;
    let session = manager.create(env.cwd.clone()).await.unwrap();
    let id = session.id();
    stop(&session).await;
    manager.shutdown().await.unwrap();
    drop((session, manager));

    let manager = env.manager(&script).await;
    let rows = manager.list().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, SessionState::Stopped);
    // A new session launches in default mode, which the CLI reports back.
    assert_eq!(rows[0].permission_mode.as_deref(), Some("default"));
    let first_run = rows[0].last_seq;
    assert_eq!(first_run, 4);

    // A stopped session only stores the mode; the resume launches with it.
    let mut changes = manager.subscribe_changes();
    manager
        .set_permission_mode(id, "plan".into())
        .await
        .unwrap();
    assert_eq!(changes.recv().await.unwrap(), id);
    assert!(manager.live(id).await.is_none(), "setting the mode spawned");
    let row = manager.store().session(id).await.unwrap().unwrap();
    assert_eq!(row.permission_mode.as_deref(), Some("plan"));
    assert_eq!(row.last_seq, first_run);
    assert!(
        manager
            .set_permission_mode(id, "bypassPermissions".into())
            .await
            .is_err()
    );

    let session = manager.open(id).await.unwrap();
    let again = manager.open(id).await.unwrap();
    assert_eq!(again.id(), id);
    stop(&session).await;
    manager.shutdown().await.unwrap();

    let stored = manager.events_after(id, first_run, ALL).await.unwrap();
    assert_contiguous(&stored, first_run + 1);
    assert_eq!(
        host(&stored[0]),
        HostEvent::Started {
            launch: Launch::Resume(id),
            cwd: env.cwd.clone(),
        }
    );
    assert_eq!(
        stored
            .iter()
            .filter(|event| event.kind == EventKind::Host
                && matches!(host(event), HostEvent::Started { .. }))
            .count(),
        1,
        "a live session is not respawned"
    );
    assert!(
        stored
            .iter()
            .filter(|event| event.kind == EventKind::Host)
            .any(|event| host(event)
                == HostEvent::PermissionMode {
                    mode: "plan".into()
                }),
        "the resumed CLI was not launched in plan mode"
    );
    assert_clean_exit(stored.last().unwrap());
    assert!(manager.open(Uuid::new_v4()).await.is_err());
}

/// Initialization, then one `set_permission_mode` exchange.
const MODE_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "mode", "request": {"subtype": "set_permission_mode", "mode": "acceptEdits"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "mode", "response": {"mode": "acceptEdits"}}}}
"#;

#[tokio::test]
async fn the_row_follows_the_mode_of_a_live_session() {
    let env = Env::new();
    let script = env.script("mode.ndjson", MODE_SCRIPT);
    let manager = env.manager(&script).await;
    let session = manager.create(env.cwd.clone()).await.unwrap();
    let id = session.id();
    manager
        .set_permission_mode(id, "acceptEdits".into())
        .await
        .unwrap();
    assert!(manager.live(id).await.is_some());
    manager.store().flush().await.unwrap();
    let row = manager.store().session(id).await.unwrap().unwrap();
    assert_eq!(row.permission_mode.as_deref(), Some("acceptEdits"));
    stop(&session).await;
    manager.shutdown().await.unwrap();
    let stored = manager.events_after(id, 0, ALL).await.unwrap();
    assert_clean_exit(stored.last().unwrap());
}

/// Two asks, of which only the first gets answered.
const ASKS_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-1", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls"}, "tool_use_id": "t1"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-2", "request": {"subtype": "can_use_tool", "tool_name": "Read", "input": {"file_path": "a"}, "tool_use_id": "t2"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-1", "response": {"behavior": "deny", "message": "no"}}}}
"#;

/// Waits for an announcement after which the live session has `asks` pending.
async fn until_asks(
    manager: &SessionManager,
    changes: &mut broadcast::Receiver<Uuid>,
    id: Uuid,
    asks: usize,
) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            assert_eq!(changes.recv().await.unwrap(), id);
            if manager
                .statuses()
                .await
                .get(&id)
                .is_some_and(|status| status.asks == asks)
            {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{asks} pending asks never announced"));
}

#[tokio::test]
async fn pending_asks_are_counted_and_announced() {
    let env = Env::new();
    let script = env.script("asks.ndjson", ASKS_SCRIPT);
    let manager = env.manager(&script).await;
    let mut changes = manager.subscribe_changes();
    let session = manager.create(env.cwd.clone()).await.unwrap();
    let id = session.id();
    until_asks(&manager, &mut changes, id, 2).await;
    session
        .answer(
            "cli-1",
            Answer::Deny {
                message: "no".into(),
            },
        )
        .await
        .unwrap();
    until_asks(&manager, &mut changes, id, 1).await;

    stop(&session).await;
    assert_eq!(session.status().borrow().asks, 0);
    assert!(!manager.statuses().await.contains_key(&id));
    manager.shutdown().await.unwrap();
    let stored = manager.events_after(id, 0, ALL).await.unwrap();
    assert_clean_exit(stored.last().unwrap());
}

/// Waits until the discussion's stored unread state is `want`.
async fn until_unread(manager: &SessionManager, id: Uuid, want: Option<Unread>) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let row = manager.store().session(id).await.unwrap().unwrap();
            if row.unread == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("unread never became {want:?}"));
}

#[tokio::test]
async fn a_finished_turn_is_unread_until_an_active_viewer_sees_it() {
    let env = Env::new();
    let manager = env.manager(&recording("basic")).await;
    let session = manager.create(env.cwd.clone()).await.unwrap();
    let id = session.id();
    session.send("Reply with exactly: ok", None).await.unwrap();
    until_unread(&manager, id, Some(Unread::Finished)).await;

    let viewer = Uuid::new_v4();
    manager.set_presence(viewer, Some(id), false);
    tokio::time::sleep(Duration::from_millis(100)).await;
    until_unread(&manager, id, Some(Unread::Finished)).await;
    manager.set_presence(viewer, Some(id), true);
    until_unread(&manager, id, None).await;
    manager.drop_presence(viewer);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_unasked_exit_is_unread_and_a_kill_is_not() {
    let env = Env::new();
    let script = env.script("init.ndjson", INIT_SCRIPT);
    let manager = env.manager(&script).await;

    let killed = manager.create(env.cwd.clone()).await.unwrap();
    manager.kill(killed.id(), StopReason::Kill).await.unwrap();
    // The fake exits on a line past the end of its script.
    let crashed = manager.create(env.cwd.clone()).await.unwrap();
    crashed.send("unscripted", None).await.unwrap();
    until_unread(&manager, crashed.id(), Some(Unread::Stopped)).await;

    let row = manager.store().session(killed.id()).await.unwrap().unwrap();
    assert_eq!(row.unread, None);
    manager.shutdown().await.unwrap();
}

/// Stores the `out` lines of the `cd` recording as the history of a stopped
/// discussion in plan mode.
async fn discussion_from_cd(env: &Env) -> (Uuid, u64) {
    let store = Store::open(&env.db).unwrap();
    let id = Uuid::new_v4();
    store.session_started(id, &env.cwd, chrono::Utc::now(), Some("plan".into()));
    let lines = Recording::load(&recording("cd"))
        .unwrap()
        .steps
        .into_iter()
        .filter(|step| step.dir == Direction::Out);
    let mut seq = 0;
    for step in lines {
        seq += 1;
        let raw = step.msg.to_string();
        let body = EventBody::Line(Box::new(vigie2::session::Line {
            parsed: serde_json::from_str(&raw).ok(),
            raw,
        }));
        store.record_event(
            id,
            &Event {
                seq,
                time: chrono::Utc::now(),
                body,
            },
        );
    }
    store.set_state(id, SessionState::Stopped);
    store.flush().await.unwrap();
    assert!(store.session(id).await.unwrap().unwrap().cache.is_some());
    (id, seq)
}

fn user_script(text: &str) -> String {
    let user = serde_json::json!({"t": 0, "dir": "in", "msg": {"type": "user",
        "message": {"role": "user", "content": text}, "parent_tool_use_id": null,
        "session_id": "s"}});
    format!("{}\n{user}", INIT_SCRIPT.trim())
}

#[tokio::test]
async fn continue_and_blank_move_the_discussion_to_fresh_claude_sessions() {
    let env = Env::new();
    let (id, first_run) = discussion_from_cd(&env).await;
    let store = Store::open(&env.db).unwrap();
    let row = store.session(id).await.unwrap().unwrap();
    let expected_seed =
        vigie2::seed::History::from_events(&store.events_after(id, 0, ALL).await.unwrap()).seed(
            &env.cwd,
            row.created_at.with_timezone(&chrono::Local).date_naive(),
        );
    drop(store);
    let script = env.script(
        "seeded.ndjson",
        &user_script(&vigie2::seed::first_message(
            &expected_seed.text,
            "next step",
        )),
    );

    let manager = env.manager(&script).await;
    let mut changes = manager.subscribe_changes();
    manager
        .restart(
            id,
            RestartMode::Continue,
            "next step".into(),
            Some("c1".into()),
        )
        .await
        .unwrap();
    assert_eq!(changes.recv().await.unwrap(), id);
    let session = manager.live(id).await.expect("a continue launches");
    stop(&session).await;
    manager.shutdown().await.unwrap();

    let stored = manager.events_after(id, first_run, ALL).await.unwrap();
    assert_contiguous(&stored, first_run + 1);
    assert_eq!(
        host(&stored[0]),
        HostEvent::Restarted {
            mode: RestartMode::Continue,
            exchanges: 1,
            total_exchanges: 1,
            seed: Some(expected_seed.text.clone()),
        }
    );
    let HostEvent::Started { launch, .. } = host(&stored[1]) else {
        panic!("expected Started, got {:?}", host(&stored[1]));
    };
    let claude_session = launch.claude_session_id();
    assert_eq!(launch, Launch::New(claude_session));
    assert_ne!(claude_session, id);
    let sent: Vec<HostEvent> = stored
        .iter()
        .filter(|event| event.kind == EventKind::Host)
        .map(host)
        .filter(|event| matches!(event, HostEvent::UserSent { .. }))
        .collect();
    assert_eq!(
        sent,
        [HostEvent::UserSent {
            text: "next step".into(),
            client_id: Some("c1".into()),
            mid_turn: false,
        }]
    );
    assert!(
        stored
            .iter()
            .filter(|event| event.kind == EventKind::Host)
            .any(|event| host(event)
                == HostEvent::PermissionMode {
                    mode: "plan".into()
                })
    );
    assert_clean_exit(stored.last().unwrap());
    let row = manager.store().session(id).await.unwrap().unwrap();
    assert_eq!(row.launch, Launch::Resume(claude_session));
    assert_eq!(row.cache, None);
    drop(manager);

    let script = env.script("init.ndjson", INIT_SCRIPT);
    let manager = env.manager(&script).await;
    let before = manager.store().session(id).await.unwrap().unwrap().last_seq;
    manager
        .restart(id, RestartMode::Blank, " ".into(), None)
        .await
        .unwrap();
    assert!(
        manager.live(id).await.is_none(),
        "a blank restart without text launched"
    );
    let row = manager.store().session(id).await.unwrap().unwrap();
    let Launch::New(blank_session) = row.launch else {
        panic!("a blank restart left {:?}", row.launch);
    };
    assert_ne!(blank_session, claude_session);
    assert_eq!((row.cache, row.context), (None, None));
    let stored = manager.events_after(id, before, ALL).await.unwrap();
    assert_eq!(
        stored.iter().map(host).collect::<Vec<_>>(),
        [HostEvent::Restarted {
            mode: RestartMode::Blank,
            exchanges: 0,
            total_exchanges: 1,
            seed: None,
        }]
    );
    let session = manager.open(id).await.unwrap();
    stop(&session).await;
    manager.shutdown().await.unwrap();
    let stored = manager.events_after(id, before + 1, ALL).await.unwrap();
    assert_eq!(
        host(&stored[0]),
        HostEvent::Started {
            launch: Launch::New(blank_session),
            cwd: env.cwd.clone(),
        }
    );
    assert_clean_exit(stored.last().unwrap());
    let row = manager.store().session(id).await.unwrap().unwrap();
    assert_eq!(row.launch, Launch::Resume(blank_session));
}

#[tokio::test]
async fn a_restart_without_shutdown_leaves_no_session_running() {
    let env = Env::new();
    let script = env.script("init.ndjson", INIT_SCRIPT);
    let store = Store::open(&env.db).unwrap();
    let id = Uuid::new_v4();
    store.session_started(id, &env.cwd, chrono::Utc::now(), None);
    store.flush().await.unwrap();
    drop(store);

    let manager = env.manager(&script).await;
    let rows = manager.list().await.unwrap();
    assert_eq!(rows[0].state, SessionState::Stopped);
}

#[tokio::test]
async fn a_lagging_subscriber_does_not_lose_history() {
    const NOISE: usize = 6000;
    let env = Env::new();
    let mut text = INIT_SCRIPT.trim().to_string();
    for index in 0..NOISE {
        text.push_str(&format!(
            "\n{{\"t\": 0, \"dir\": \"out\", \"msg\": {{\"type\": \"noise\", \"n\": {index}}}}}"
        ));
    }
    let script = env.script("noise.ndjson", &text);
    let manager = env.manager(&script).await;
    let session = manager.create(env.cwd.clone()).await.unwrap();
    let mut lagging = session.subscribe();
    stop(&session).await;
    manager.shutdown().await.unwrap();

    assert!(matches!(lagging.recv().await, Err(RecvError::Lagged(_))));
    let stored = manager.events_after(session.id(), 0, ALL).await.unwrap();
    assert_contiguous(&stored, 1);
    let noise: Vec<&StoredEvent> = stored
        .iter()
        .filter(|event| event.payload.contains("\"noise\""))
        .collect();
    assert_eq!(noise.len(), NOISE);
    for (index, event) in noise.iter().enumerate() {
        let expected = serde_json::json!({"type": "noise", "n": index});
        assert_eq!(event.payload, expected.to_string());
    }
    assert_clean_exit(stored.last().unwrap());
}

/// Initialization with a catalog; the fake then waits for stdin to close.
const CATALOG_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}, "models": [{"value": "opus", "resolvedModel": "claude-opus-5-5", "displayName": "Opus 5.5", "supportsEffort": true, "supportedEffortLevels": ["low", "medium"]}], "commands": [{"name": "review", "description": "Review a change", "argumentHint": ""}]}}}}
"#;

fn canned_defaults() -> AppliedSettings {
    AppliedSettings {
        model: Some(CANNED_DEFAULT_MODEL.into()),
        effort: Some(CANNED_DEFAULT_EFFORT.into()),
    }
}

/// Waits, through the catalog announcements, for the shared catalog to satisfy `done`.
async fn until_catalog(
    manager: &SessionManager,
    catalog_changes: &mut broadcast::Receiver<()>,
    done: impl Fn(&SharedCatalog) -> bool,
) -> SharedCatalog {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let catalog = manager.catalog().await.unwrap();
            if done(&catalog) {
                return catalog;
            }
            catalog_changes.recv().await.unwrap();
        }
    })
    .await
    .expect("the shared catalog was not announced")
}

#[tokio::test]
async fn the_catalog_probe_runs_only_when_nothing_is_stored() {
    let env = Env::new();
    let script = env.script("catalog.ndjson", CATALOG_SCRIPT);
    let manager = env.manager(&script).await;
    let mut catalog_changes = manager.subscribe_catalog();
    let probe_dir = env.dir.path().join("state").join("probe");
    assert_eq!(
        manager.probe_catalog(probe_dir.clone()).await.unwrap(),
        Probe::Ran
    );
    catalog_changes
        .try_recv()
        .expect("the probe announced nothing");
    let catalog = manager.catalog().await.unwrap();
    let models = catalog.models.expect("models stored");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].value, "opus");
    assert_eq!(models[0].effort_levels, ["low", "medium"]);
    let commands = catalog.commands.expect("commands stored");
    assert_eq!(commands[0].name, "review");
    assert_eq!(catalog.defaults, Some(canned_defaults()));
    assert_eq!(catalog.terminal_commands, None);
    assert!(
        manager.list().await.unwrap().is_empty(),
        "the probe is no discussion"
    );
    drop(manager);

    let manager = env.manager(&script).await;
    assert_eq!(
        manager.probe_catalog(probe_dir).await.unwrap(),
        Probe::Skipped
    );
}

#[tokio::test]
async fn a_start_without_a_pick_updates_the_shared_catalog_and_a_pick_does_not() {
    let env = Env::new();
    let script = env.script("catalog.ndjson", CATALOG_SCRIPT);
    let manager = env.manager(&script).await;
    let mut catalog_changes = manager.subscribe_catalog();
    let mut changes = manager.subscribe_changes();
    let first = manager.create(env.cwd.clone()).await.unwrap();
    // Reached once the spawn's own ask stored what it read.
    until_applied(
        &manager,
        &mut changes,
        first.id(),
        CANNED_DEFAULT_MODEL,
        CANNED_DEFAULT_EFFORT,
    )
    .await;
    let catalog = until_catalog(&manager, &mut catalog_changes, |catalog| {
        catalog.models.is_some() && catalog.defaults.is_some()
    })
    .await;
    assert_eq!(catalog.defaults, Some(canned_defaults()));
    assert_eq!(catalog.commands.unwrap()[0].name, "review");

    // A change on the live child is a pick.
    let first_id = first.id();
    manager
        .set_settings(first_id, Some(Some("sonnet".into())), None)
        .await
        .unwrap();
    until_applied(
        &manager,
        &mut changes,
        first_id,
        "claude-sonnet-5",
        CANNED_DEFAULT_EFFORT,
    )
    .await;
    stop(&first).await;

    // So is a launch with one.
    let mut changes = manager.subscribe_changes();
    let second = manager.create(env.cwd.clone()).await.unwrap();
    let second_id = second.id();
    stop(&second).await;
    manager
        .set_settings(second_id, None, Some(Some("high".into())))
        .await
        .unwrap();
    let second = manager.open(second_id).await.unwrap();
    until_applied(
        &manager,
        &mut changes,
        second_id,
        CANNED_DEFAULT_MODEL,
        "high",
    )
    .await;
    assert_eq!(
        manager.catalog().await.unwrap().defaults,
        Some(canned_defaults())
    );
    stop(&second).await;
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_start_stores_the_terminal_only_commands_of_its_init() {
    let env = Env::new();
    let manager = env.manager(&recording("basic")).await;
    let mut catalog_changes = manager.subscribe_catalog();
    let session = manager.create(env.cwd.clone()).await.unwrap();
    let mut events = session.subscribe();
    session.send("Reply with exactly: ok", None).await.unwrap();
    until_result(&mut events).await;
    let catalog = until_catalog(&manager, &mut catalog_changes, |catalog| {
        catalog.terminal_commands.is_some()
    })
    .await;
    assert!(
        catalog
            .terminal_commands
            .unwrap()
            .contains(&"doctor".to_string())
    );
    assert!(
        catalog
            .models
            .unwrap()
            .iter()
            .any(|model| model.value == "opus")
    );
    stop(&session).await;
    manager.shutdown().await.unwrap();
}
