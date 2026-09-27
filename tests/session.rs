//! Drives a real `Session` against the fake claude replaying the spike recordings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::broadcast;
use uuid::Uuid;
use vigie2::child::{Launch, Program};
use vigie2::protocol::{Delta, Inbound, ResultMessage, StreamEventBody};
use vigie2::session::{
    Activity, Answer, Config, Event, EventBody, GateFailure, HostEvent, Resolution, Session,
};

const TIMEOUT: Duration = Duration::from_secs(20);

fn recording(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("spike/runs")
        .join(format!("{name}.ndjson"))
}

/// Copies a recording into `dir` with `edit` applied to every CLI stdout line.
fn doctored(dir: &TempDir, name: &str, edit: impl Fn(&mut Value)) -> PathBuf {
    let text = std::fs::read_to_string(recording(name)).expect("read recording");
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            let mut record: Value = serde_json::from_str(line).expect("recording line");
            if record["dir"] == "out" {
                edit(&mut record["msg"]);
            }
            record.to_string()
        })
        .collect();
    let path = dir.path().join(format!("{name}.doctored.ndjson"));
    std::fs::write(&path, lines.join("\n")).expect("write recording");
    path
}

struct Harness {
    session: Session,
    events: broadcast::Receiver<Arc<Event>>,
    last_seq: u64,
    claude_session: Uuid,
    /// Every `PermissionMode` event seen, in order.
    modes: Vec<String>,
    _cwd: TempDir,
}

impl Harness {
    fn start(recording: &Path) -> Self {
        let cwd = tempfile::tempdir().expect("tempdir");
        let claude_session = Uuid::new_v4();
        let config = Config {
            id: Uuid::new_v4(),
            cwd: cwd.path().to_path_buf(),
            launch: Launch::New(claude_session),
            program: Program {
                path: env!("CARGO_BIN_EXE_fake-claude").into(),
                leading_args: vec![recording.into()],
            },
            history: None,
            last_seq: 0,
            permission_mode: None,
        };
        let (session, events) = Session::spawn(config).expect("spawn session");
        Self {
            session,
            events,
            last_seq: 0,
            claude_session,
            modes: Vec::new(),
            _cwd: cwd,
        }
    }

    /// Next event, checking the sequence and the session id the fake substituted.
    async fn next(&mut self) -> Arc<Event> {
        let event = tokio::time::timeout(TIMEOUT, self.events.recv())
            .await
            .expect("session went quiet")
            .expect("event stream ended");
        assert_eq!(event.seq, self.last_seq + 1, "sequence gap");
        self.last_seq = event.seq;
        if let EventBody::Host(HostEvent::PermissionMode { mode }) = &event.body {
            self.modes.push(mode.clone());
        }
        if let EventBody::Line(line) = &event.body {
            let parsed = line.parsed.as_ref().expect("line parses");
            let raw: Value = serde_json::from_str(&line.raw).expect("raw line is JSON");
            if let Some(session_id) = raw.get("session_id") {
                assert_eq!(session_id, &self.claude_session.to_string(), "{parsed:?}");
            }
        }
        event
    }

    async fn until<T>(&mut self, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        loop {
            let event = self.next().await;
            if let Some(found) = pick(&event) {
                return found;
            }
        }
    }

    async fn until_result(&mut self) -> ResultMessage {
        self.until(|event| match line(event) {
            Some(Inbound::Result(result)) => Some(result.clone()),
            _ => None,
        })
        .await
    }

    async fn until_host(&mut self, mut pick: impl FnMut(&HostEvent) -> bool) -> HostEvent {
        self.until(|event| match &event.body {
            EventBody::Host(host) if pick(host) => Some(host.clone()),
            _ => None,
        })
        .await
    }

    /// Request id and tool name of the next queued ask.
    async fn until_ask(&mut self) -> (String, String) {
        match self
            .until_host(|host| matches!(host, HostEvent::AskQueued { .. }))
            .await
        {
            HostEvent::AskQueued {
                request_id,
                tool_name,
                ..
            } => (request_id, tool_name),
            _ => unreachable!(),
        }
    }

    async fn until_exit(&mut self) -> Option<i32> {
        match self
            .until_host(|host| matches!(host, HostEvent::ChildExited { .. }))
            .await
        {
            HostEvent::ChildExited { code, .. } => code,
            _ => unreachable!(),
        }
    }

    /// Stops the session; the fake exits 0 only if the whole script was played.
    async fn finish(mut self) {
        tokio::time::timeout(TIMEOUT, self.session.stop())
            .await
            .expect("session did not stop");
        assert_eq!(self.until_exit().await, Some(0), "fake claude failed");
    }
}

fn line(event: &Event) -> Option<&Inbound> {
    match &event.body {
        EventBody::Line(line) => line.parsed.as_ref(),
        EventBody::Host(_) => None,
    }
}

fn is_user_echo(event: &Event) -> bool {
    matches!(line(event), Some(Inbound::User(user)) if user.is_replay == Some(true))
}

fn is_text_delta(event: &Event) -> bool {
    matches!(
        line(event),
        Some(Inbound::StreamEvent(stream)) if matches!(
            stream.event,
            StreamEventBody::ContentBlockDelta { delta: Delta::TextDelta { .. }, .. }
        )
    )
}

#[tokio::test]
async fn basic_turns_and_usage() {
    let mut harness = Harness::start(&recording("basic"));
    let started = harness.next().await;
    assert_eq!(started.seq, 1);
    assert!(matches!(
        &started.body,
        EventBody::Host(HostEvent::Started { launch: Launch::New(id), .. }) if *id == harness.claude_session
    ));
    harness
        .session
        .send("Reply with exactly: ok", Some("c1".into()))
        .await
        .unwrap();
    let mut sent_seq = None;
    let echo_seq = harness
        .until(|event| match &event.body {
            EventBody::Host(HostEvent::UserSent { text, client_id }) => {
                assert!(sent_seq.is_none(), "user_sent emitted twice");
                assert_eq!(text, "Reply with exactly: ok");
                assert_eq!(client_id.as_deref(), Some("c1"));
                sent_seq = Some(event.seq);
                None
            }
            _ => is_user_echo(event).then_some(event.seq),
        })
        .await;
    assert!(
        sent_seq.is_some_and(|seq| seq < echo_seq),
        "user_sent must precede the echo"
    );
    let result = harness.until_result().await;
    assert_eq!(result.result.as_deref(), Some("ok"));
    assert_eq!(*harness.session.activity().borrow(), Activity::Idle);

    let usage = harness.session.usage().await.unwrap();
    assert!(usage.rate_limits_available);

    harness
        .session
        .send("Reply with exactly: ok again", None)
        .await
        .unwrap();
    harness.until_result().await;
    harness.finish().await;
}

#[tokio::test]
async fn permissions_deny_and_question_answer() {
    let mut harness = Harness::start(&recording("permissions"));
    harness
        .session
        .send(
            "Use the Bash tool to run exactly: echo spike-allow. Then reply done.",
            None,
        )
        .await
        .unwrap();
    harness.until_result().await;

    harness
        .session
        .send(
            "Use the Write tool to create denied.txt containing x. If refused, reply refused \
             and do not retry.",
            None,
        )
        .await
        .unwrap();
    let (request_id, tool_name) = harness.until_ask().await;
    assert_eq!(tool_name, "Write");
    let deny = Answer::Deny {
        message: "Refused by the spike driver.".into(),
    };
    harness
        .session
        .answer(request_id.clone(), deny.clone())
        .await
        .unwrap();
    let resolved = harness
        .until_host(|host| matches!(host, HostEvent::AskResolved { .. }))
        .await;
    assert_eq!(
        resolved,
        HostEvent::AskResolved {
            request_id: request_id.clone(),
            resolution: Resolution::Answered { answer: deny },
        }
    );
    assert!(
        harness
            .session
            .answer(
                request_id,
                Answer::Allow {
                    updated_input: None,
                    note: None,
                }
            )
            .await
            .is_err(),
        "an answered ask is no longer pending"
    );
    harness.until_result().await;

    harness
        .session
        .send(
            "Call the AskUserQuestion tool once, with the single question 'Pick a color?' and \
             the options red and blue. Then reply with the answer you got.",
            None,
        )
        .await
        .unwrap();
    let (request_id, tool_name) = harness.until_ask().await;
    assert_eq!(tool_name, "AskUserQuestion");
    let answers = BTreeMap::from([("Pick a color?".to_string(), "blue".to_string())]);
    harness
        .session
        .answer(request_id, Answer::Questions { answers })
        .await
        .unwrap();
    let result = harness.until_result().await;
    assert!(result.result.unwrap_or_default().contains("blue"));
    harness.finish().await;
}

#[tokio::test]
async fn interrupt_while_streaming_and_during_a_tool() {
    let mut harness = Harness::start(&recording("interrupt"));
    harness
        .session
        .send(
            "Write the numbers from 1 to 400, one per line, nothing else.",
            None,
        )
        .await
        .unwrap();
    harness
        .until(|event| is_text_delta(event).then_some(()))
        .await;
    let receipt = harness.session.interrupt().await.unwrap();
    assert_eq!(receipt.still_queued, Some(Vec::new()));
    let result = harness.until_result().await;
    assert_eq!(result.subtype, "error_during_execution");

    harness
        .session
        .send(
            "Use the Bash tool to run exactly: python -c \"import time; time.sleep(60)\". Run it \
             in the foreground. Then reply done.",
            None,
        )
        .await
        .unwrap();
    let (request_id, tool_name) = harness.until_ask().await;
    assert_eq!(tool_name, "Bash");
    harness
        .session
        .answer(
            request_id,
            Answer::Allow {
                updated_input: None,
                note: None,
            },
        )
        .await
        .unwrap();
    harness.session.interrupt().await.unwrap();
    let result = harness.until_result().await;
    assert!(result.is_error);

    harness
        .session
        .send("Reply with exactly: ok", None)
        .await
        .unwrap();
    let result = harness.until_result().await;
    assert_eq!(result.result.as_deref(), Some("ok"));
    harness.finish().await;
}

#[tokio::test]
async fn background_task_starts_its_own_turn() {
    let mut harness = Harness::start(&recording("background"));
    harness
        .session
        .send(
            "Use the Bash tool with run_in_background set to true to run exactly: sleep 8 && \
             echo bg-done. Reply started, without waiting for it.",
            None,
        )
        .await
        .unwrap();
    harness.until_result().await;
    let second = harness.until_result().await;
    assert!(second.result.unwrap_or_default().contains("exit"));
    assert_eq!(*harness.session.activity().borrow(), Activity::Idle);
    harness.finish().await;
}

/// `initialize` reports default, `set_permission_mode` switches to plan, and
/// approving the plan moves the CLI back to default (a `system/status`).
#[tokio::test]
async fn permission_mode_changes_are_announced_once_each() {
    let mut harness = Harness::start(&recording("plan"));
    let changed = harness.session.set_permission_mode("plan").await.unwrap();
    assert_eq!(changed.mode, "plan");
    harness
        .session
        .send(
            "Plan creating hello.txt containing hi. Keep the plan to two lines and submit it \
             with ExitPlanMode. Once approved, create the file, then reply done.",
            None,
        )
        .await
        .unwrap();
    let allow = Answer::Allow {
        updated_input: None,
        note: None,
    };
    for expected in ["ExitPlanMode", "Write"] {
        let (request_id, tool_name) = harness.until_ask().await;
        assert_eq!(tool_name, expected);
        harness
            .session
            .answer(request_id, allow.clone())
            .await
            .unwrap();
    }
    harness.until_result().await;
    let modes = harness.modes.clone();
    harness.finish().await;
    assert_eq!(modes, ["default", "plan", "default"]);
}

async fn assert_gate_fails(harness: &mut Harness, expected: GateFailure) {
    let failed = harness
        .until_host(|host| matches!(host, HostEvent::AuthGateFailed { .. }))
        .await;
    assert_eq!(failed, HostEvent::AuthGateFailed { reason: expected });
    harness.until_exit().await;
    tokio::time::timeout(TIMEOUT, harness.session.closed())
        .await
        .expect("session still open");
    assert!(harness.session.send("more", None).await.is_err());
}

#[tokio::test]
async fn gate_rejects_a_non_first_party_account() {
    let dir = tempfile::tempdir().unwrap();
    let path = doctored(&dir, "basic", |msg| {
        if msg["type"] == "control_response" && msg["response"]["request_id"] == "spike-1" {
            msg["response"]["response"]["account"]["apiProvider"] = "bedrock".into();
        }
    });
    let mut harness = Harness::start(&path);
    let sent = harness.session.send("Reply with exactly: ok", None).await;
    assert!(sent.is_err(), "user text sent before the gate passed");
    assert_gate_fails(
        &mut harness,
        GateFailure::ApiProvider {
            provider: Some("bedrock".into()),
        },
    )
    .await;
}

#[tokio::test]
async fn gate_rejects_api_key_billing() {
    let dir = tempfile::tempdir().unwrap();
    let path = doctored(&dir, "basic", |msg| {
        if msg["type"] == "system" && msg["subtype"] == "init" {
            msg["apiKeySource"] = "ANTHROPIC_API_KEY".into();
        }
    });
    let mut harness = Harness::start(&path);
    harness
        .session
        .send("Reply with exactly: ok", None)
        .await
        .unwrap();
    assert_gate_fails(
        &mut harness,
        GateFailure::ApiKeySource {
            source: "ANTHROPIC_API_KEY".into(),
        },
    )
    .await;
}

#[tokio::test]
async fn gate_rejects_overage_use() {
    let dir = tempfile::tempdir().unwrap();
    let path = doctored(&dir, "background", |msg| {
        if msg["type"] == "rate_limit_event" {
            msg["rate_limit_info"]["isUsingOverage"] = true.into();
        }
    });
    let mut harness = Harness::start(&path);
    harness
        .session
        .send(
            "Use the Bash tool with run_in_background set to true to run exactly: sleep 8 && \
             echo bg-done. Reply started, without waiting for it.",
            None,
        )
        .await
        .unwrap();
    assert_gate_fails(&mut harness, GateFailure::Overage).await;
}

#[tokio::test]
async fn gate_rejects_an_overage_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = doctored(&dir, "background", |msg| {
        if msg["type"] == "rate_limit_event" {
            assert_eq!(msg["rate_limit_info"]["isUsingOverage"], false);
            msg["rate_limit_info"]["rateLimitType"] = "overage".into();
        }
    });
    let mut harness = Harness::start(&path);
    harness
        .session
        .send(
            "Use the Bash tool with run_in_background set to true to run exactly: sleep 8 && \
             echo bg-done. Reply started, without waiting for it.",
            None,
        )
        .await
        .unwrap();
    assert_gate_fails(&mut harness, GateFailure::Overage).await;
}

/// A script with an unsupported CLI request, then two asks of which the CLI
/// withdraws the first.
const CONTROL_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-1", "request": {"subtype": "hook_callback", "callback_id": "c1"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "error", "request_id": "cli-1", "error": "vigie2 does not handle control request hook_callback"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-2", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls"}, "tool_use_id": "t2"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-3", "request": {"subtype": "can_use_tool", "tool_name": "Read", "input": {"file_path": "a"}, "tool_use_id": "t3"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_cancel_request", "request_id": "cli-2"}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-3", "response": {"behavior": "allow", "updatedInput": {"file_path": "a"}}}}}
"#;

#[tokio::test]
async fn unknown_requests_are_refused_and_cancelled_asks_resolve() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.ndjson");
    std::fs::write(&path, CONTROL_SCRIPT.trim()).unwrap();
    let mut harness = Harness::start(&path);
    assert_eq!(
        harness.until_ask().await,
        ("cli-2".to_string(), "Bash".to_string())
    );
    assert_eq!(
        harness.until_ask().await,
        ("cli-3".to_string(), "Read".to_string())
    );
    let cancelled = harness
        .until_host(|host| matches!(host, HostEvent::AskResolved { .. }))
        .await;
    assert_eq!(
        cancelled,
        HostEvent::AskResolved {
            request_id: "cli-2".into(),
            resolution: Resolution::Cancelled,
        }
    );
    let allow = Answer::Allow {
        updated_input: None,
        note: None,
    };
    assert!(
        harness
            .session
            .answer("cli-2", allow.clone())
            .await
            .is_err()
    );
    harness.session.answer("cli-3", allow).await.unwrap();
    harness.finish().await;
}

/// A dangerous ask allowed with a note, which follows as a queued user message,
/// then a safe ask allowed with a blank note, which sends nothing more.
const NOTE_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-1", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "git reset --hard"}, "tool_use_id": "t1"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-1", "response": {"behavior": "allow", "updatedInput": {"command": "git reset --hard"}}}}}
{"t": 0, "dir": "in", "msg": {"type": "user", "message": {"role": "user", "content": "(Note attached to my approval of your Bash call) Then run the tests."}, "parent_tool_use_id": null, "session_id": "s", "priority": "next"}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-2", "request": {"subtype": "can_use_tool", "tool_name": "Read", "input": {"file_path": "a"}, "tool_use_id": "t2"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-2", "response": {"behavior": "allow", "updatedInput": {"file_path": "a"}}}}}
"#;

#[tokio::test]
async fn an_allow_note_follows_as_a_queued_user_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.ndjson");
    std::fs::write(&path, NOTE_SCRIPT.trim()).unwrap();
    let mut harness = Harness::start(&path);
    let HostEvent::AskQueued { danger, .. } = harness
        .until_host(|host| matches!(host, HostEvent::AskQueued { .. }))
        .await
    else {
        unreachable!()
    };
    assert!(danger.dangerous);
    assert_eq!(danger.reasons, ["Hard reset: git reset --hard"]);

    let allow = Answer::Allow {
        updated_input: None,
        note: Some("Then run the tests.".into()),
    };
    harness
        .session
        .answer("cli-1", allow.clone())
        .await
        .unwrap();
    let resolved = harness
        .until_host(|host| matches!(host, HostEvent::AskResolved { .. }))
        .await;
    assert_eq!(
        resolved,
        HostEvent::AskResolved {
            request_id: "cli-1".into(),
            resolution: Resolution::Answered { answer: allow },
        }
    );
    let sent = harness
        .until_host(|host| matches!(host, HostEvent::UserSent { .. }))
        .await;
    assert_eq!(
        sent,
        HostEvent::UserSent {
            text: "(Note attached to my approval of your Bash call) Then run the tests.".into(),
            client_id: None,
        }
    );

    let HostEvent::AskQueued { danger, .. } = harness
        .until_host(|host| matches!(host, HostEvent::AskQueued { .. }))
        .await
    else {
        unreachable!()
    };
    assert!(!danger.dangerous);
    let blank = Answer::Allow {
        updated_input: None,
        note: Some("  ".into()),
    };
    harness.session.answer("cli-2", blank).await.unwrap();
    harness.finish().await;
}
