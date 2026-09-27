//! Drives `SessionManager` against the fake claude and checks what history keeps.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::RecvError};
use uuid::Uuid;
use vigie2::child::{Launch, Program};
use vigie2::manager::SessionManager;
use vigie2::protocol::Inbound;
use vigie2::replay::{Direction, Recording};
use vigie2::session::{Event, EventBody, HostEvent, Session};
use vigie2::store::{EventKind, SessionState, Store, StoredEvent};

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
        HostEvent::ChildExited { code, stderr_tail } => {
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

    session.send("Reply with exactly: ok", None).await.unwrap();
    until_result(&mut events).await;
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
    let first_run = rows[0].last_seq;
    assert_eq!(first_run, 3);

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
    assert_clean_exit(stored.last().unwrap());
    assert!(manager.open(Uuid::new_v4()).await.is_err());
}

#[tokio::test]
async fn a_restart_without_shutdown_leaves_no_session_running() {
    let env = Env::new();
    let script = env.script("init.ndjson", INIT_SCRIPT);
    let store = Store::open(&env.db).unwrap();
    let id = Uuid::new_v4();
    store.session_started(id, &env.cwd, chrono::Utc::now());
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
