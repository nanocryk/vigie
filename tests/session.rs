//! Drives a real `Session` against the fake claude replaying the spike recordings.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::broadcast;
use uuid::Uuid;
use vigie::claude::child::{Effort, Launch, ModelSettings};
use vigie::claude::protocol::{Delta, Inbound, PermissionMode, ResultMessage, StreamEventBody};
use vigie::claude::replay::Recording;
use vigie::session::grant::GrantKind;
use vigie::session::{
    Activity, Answer, Config, Event, EventBody, GateFailure, HostEvent, Resolution, Session,
};

use common::{TIMEOUT, fake};

/// Copies a recording into `dir` with `edit` applied to every CLI stdout line.
fn doctored(dir: &TempDir, name: &str, edit: impl Fn(&mut Value)) -> PathBuf {
    let text = std::fs::read_to_string(Recording::path(name)).expect("read recording");
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
        Self::start_with_uploads(recording, None)
    }

    fn start_with_uploads(recording: &Path, uploads: Option<std::path::PathBuf>) -> Self {
        let cwd = tempfile::tempdir().expect("tempdir");
        let claude_session = Uuid::new_v4();
        let config = Config {
            id: Uuid::new_v4(),
            cwd: cwd.path().to_path_buf(),
            launch: Launch::New(claude_session),
            program: fake(recording),
            history: None,
            server_restart: None,
            last_seq: 0,
            permission_mode: None,
            settings: ModelSettings::default(),
            uploads,
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
            self.modes.push(mode.to_string());
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
    let mut harness = Harness::start(&Recording::path("basic"));
    let started = harness.next().await;
    assert_eq!(started.seq, 1);
    assert!(matches!(
        &started.body,
        EventBody::Host(HostEvent::Started { launch: Launch::New(id), .. }) if *id == harness.claude_session
    ));
    let HostEvent::Catalog { models, commands } = harness
        .until_host(|host| matches!(host, HostEvent::Catalog { .. }))
        .await
    else {
        unreachable!()
    };
    let opus = models.iter().find(|model| model.value == "opus").unwrap();
    assert_eq!(opus.display_name, "Opus 5.5");
    assert_eq!(
        opus.effort_levels,
        ["low", "medium", "high", "xhigh", "max"]
    );
    let haiku = models.iter().find(|model| model.value == "haiku").unwrap();
    assert!(haiku.effort_levels.is_empty());
    let design = commands
        .iter()
        .find(|command| command.name == "design")
        .unwrap();
    assert_eq!(design.argument_hint, "consent | revoke");
    assert!(commands.iter().any(|command| command.name == "canif"));
    harness
        .session
        .send("Reply with exactly: ok", Some("c1".into()))
        .await
        .unwrap();
    let mut sent_seq = None;
    let echo_seq = harness
        .until(|event| match &event.body {
            EventBody::Host(HostEvent::UserSent {
                text,
                attachments,
                client_id,
                mid_turn,
            }) => {
                assert!(attachments.is_empty());
                assert!(sent_seq.is_none(), "user_sent emitted twice");
                assert!(!mid_turn, "no turn ran before the first message");
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
    assert_eq!(harness.session.status().borrow().activity, Activity::Idle);
    assert!(!harness.session.status().borrow().turn);

    harness
        .session
        .set_model(Some("sonnet".into()))
        .await
        .unwrap();
    harness.session.set_effort(Effort::Low).await.unwrap();
    let applied = harness.session.applied_settings().await.unwrap();
    assert_eq!(applied.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(applied.effort.as_deref(), Some("low"));

    let usage = harness.session.usage().await.unwrap();
    assert!(usage.rate_limits_available);
    // The first prompt: input 2, cache read 13397, cache creation 15607.
    let context = harness.session.context_usage().await.unwrap();
    assert_eq!(context.total_tokens, 29_006);

    harness
        .session
        .send("Reply with exactly: ok again", None)
        .await
        .unwrap();
    harness.until_result().await;
    harness.finish().await;
}

#[tokio::test]
async fn settings_and_context_replies_are_not_emitted() {
    let mut harness = Harness::start(&Recording::path("basic"));
    harness
        .until_host(|host| matches!(host, HostEvent::Catalog { .. }))
        .await;
    harness.session.applied_settings().await.unwrap();
    harness.session.context_usage().await.unwrap();
    harness.session.stop().await;
    loop {
        let event = harness.next().await;
        if let EventBody::Line(line) = &event.body {
            assert!(!line.raw.contains("\"applied\""), "emitted: {}", line.raw);
            assert!(!line.raw.contains("totalTokens"), "emitted: {}", line.raw);
        }
        if matches!(event.body, EventBody::Host(HostEvent::ChildExited { .. })) {
            break;
        }
    }
}

#[tokio::test]
async fn permissions_deny_and_question_answer() {
    let mut harness = Harness::start(&Recording::path("permissions"));
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
        for_session: false,
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
                    accept_edits: false,
                    folder: None,
                    note: None,
                    always_for_file: false,
                    for_session: false,
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
    let mut harness = Harness::start(&Recording::path("interrupt"));
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
                accept_edits: false,
                folder: None,
                note: None,
                always_for_file: false,
                for_session: false,
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
    let mut harness = Harness::start(&Recording::path("background"));
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
    assert_eq!(harness.session.status().borrow().activity, Activity::Idle);
    // The replay has no gap between the turns, so only the end state is certain.
    assert!(harness.session.background().borrow().is_empty());
    harness.finish().await;
}

/// `initialize` reports default, `set_permission_mode` switches to plan, and
/// approving the plan moves the CLI back to default (a `system/status`).
#[tokio::test]
async fn permission_mode_changes_are_announced_once_each() {
    let mut harness = Harness::start(&Recording::path("plan"));
    let changed = harness.session.set_permission_mode("plan").await.unwrap();
    assert_eq!(changed.mode, PermissionMode::Plan);
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
        accept_edits: false,
        folder: None,
        note: None,
        always_for_file: false,
        for_session: false,
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

/// The grants recording: a parent folder granted from a Read ask covers a
/// sibling, Accept edits from a Write ask covers the next Write, and every
/// outside-folder ask offers its folder in the kind the CLI suggested.
#[tokio::test]
async fn folder_grants_and_accept_edits_reach_the_cli() {
    const OUTSIDE: &str = r"C:\Users\user\AppData\Local\Temp\vigie2-spike\outside";
    let mut harness = Harness::start(&Recording::path("grants"));
    let deny = Answer::Deny {
        message: "Refused by the spike driver.".into(),
        for_session: false,
    };
    let read = |name: &str| {
        format!(
            "Use the Read tool on exactly {OUTSIDE}\\{name}. Reply with its content, or refused \
             if refused, and do not retry."
        )
    };
    let tail =
        "Reply with the result, or refused if refused, and do not retry or use another tool.";
    /// An ask a prompt raises, what it offers, and the answer given.
    struct Expected {
        tool: &'static str,
        accept_edits: bool,
        folder: Option<(GrantKind, String)>,
        answer: Answer,
    }
    let refused = |tool, kind, folder: &str| Expected {
        tool,
        accept_edits: false,
        folder: Some((kind, folder.to_string())),
        answer: deny.clone(),
    };
    let allow = |accept_edits, folder: Option<String>| Answer::Allow {
        updated_input: None,
        note: None,
        accept_edits,
        folder,
        always_for_file: false,
        for_session: false,
    };
    let script = vec![
        (
            read(r"a\b\one.txt"),
            Some(Expected {
                tool: "Read",
                accept_edits: false,
                folder: Some((GrantKind::Read, format!(r"{OUTSIDE}\a\b"))),
                answer: allow(false, Some(format!(r"{OUTSIDE}\a"))),
            }),
        ),
        (read(r"a\c\two.txt"), None),
        (
            read("other.txt"),
            Some(refused("Read", GrantKind::Read, OUTSIDE)),
        ),
        (
            "Use the Write tool to create edit1.txt containing x. Then reply done.".into(),
            Some(Expected {
                tool: "Write",
                accept_edits: true,
                folder: None,
                answer: allow(true, None),
            }),
        ),
        (
            "Use the Write tool to create edit2.txt containing y. If refused, reply refused and \
             do not retry."
                .into(),
            None,
        ),
        (
            format!("Use the Write tool to create {OUTSIDE}\\w\\new.txt containing z. {tail}"),
            Some(refused(
                "Write",
                GrantKind::Directory,
                &format!(r"{OUTSIDE}\w"),
            )),
        ),
        (
            format!("Use the Glob tool to find *.txt files under {OUTSIDE}. {tail}"),
            Some(refused("Glob", GrantKind::Read, OUTSIDE)),
        ),
        (
            format!("Use the Grep tool to search for the word other under {OUTSIDE}. {tail}"),
            Some(refused("Grep", GrantKind::Read, OUTSIDE)),
        ),
        (
            format!("Use the Bash tool to run exactly: cat \"{OUTSIDE}\\other.txt\". {tail}"),
            Some(refused("Bash", GrantKind::Read, OUTSIDE)),
        ),
    ];
    for (prompt, expected) in script {
        harness.session.send(prompt, None).await.unwrap();
        if let Some(expected) = expected {
            let HostEvent::AskQueued {
                request_id,
                tool_name,
                accept_edits,
                folder,
                ..
            } = harness
                .until_host(|host| matches!(host, HostEvent::AskQueued { .. }))
                .await
            else {
                unreachable!()
            };
            let tool = expected.tool;
            assert_eq!(tool_name, tool);
            assert_eq!(accept_edits, expected.accept_edits, "{tool}");
            assert_eq!(
                folder.map(|offer| (offer.kind, offer.folder)),
                expected.folder,
                "{tool}"
            );
            harness
                .session
                .answer(request_id, expected.answer)
                .await
                .unwrap();
        }
        harness.until_result().await;
    }
    let modes = harness.modes.clone();
    harness.finish().await;
    assert!(modes.iter().any(|mode| mode == "acceptEdits"), "{modes:?}");
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
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "error", "request_id": "cli-1", "error": "vigie does not handle control request hook_callback"}}}
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
        accept_edits: false,
        folder: None,
        note: None,
        always_for_file: false,
        for_session: false,
    };
    assert!(
        harness
            .session
            .answer("cli-2", allow.clone())
            .await
            .is_err()
    );
    harness.session.answer("cli-3", allow).await.unwrap();
    assert_eq!(harness.session.status().borrow().asks, 0);
    harness.finish().await;
}

/// An Agent call the fanout hook sends to an ask, allowed for the session; the
/// next one of the same type is allowed by the hook, and one from a subagent denied.
const FANOUT_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-1", "request": {"subtype": "hook_callback", "callback_id": "vigie_fanout", "tool_use_id": "t1", "input": {"hook_event_name": "PreToolUse", "tool_name": "Agent", "tool_input": {"subagent_type": "Explore", "model": "haiku", "prompt": "p"}, "tool_use_id": "t1"}}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-1", "response": {"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "ask", "permissionDecisionReason": "Starts agents or unattended work"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-2", "request": {"subtype": "can_use_tool", "tool_name": "Agent", "input": {"subagent_type": "Explore", "model": "haiku", "prompt": "p"}, "tool_use_id": "t1"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-2", "response": {"behavior": "allow", "updatedInput": {"subagent_type": "Explore", "model": "haiku", "prompt": "p"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-3", "request": {"subtype": "hook_callback", "callback_id": "vigie_fanout", "tool_use_id": "t2", "input": {"hook_event_name": "PreToolUse", "tool_name": "Agent", "tool_input": {"subagent_type": "Explore", "prompt": "q"}, "tool_use_id": "t2"}}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-3", "response": {"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "permissionDecisionReason": "Allowed by the user for this session"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-4", "request": {"subtype": "hook_callback", "callback_id": "vigie_fanout", "tool_use_id": "t3", "input": {"hook_event_name": "PreToolUse", "tool_name": "Agent", "agent_id": "a1", "tool_input": {"subagent_type": "Explore", "prompt": "r"}, "tool_use_id": "t3"}}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-4", "response": {"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "Nested agents and unattended work are disabled inside a subagent. Finish the task yourself, or report back to the agent that started you."}}}}}
{"t": 0, "dir": "out", "msg": {"type": "result", "subtype": "success", "is_error": false, "result": "done", "duration_ms": 1, "duration_api_ms": 1, "num_turns": 1, "stop_reason": "end_turn", "total_cost_usd": 0, "usage": {}, "session_id": "s", "uuid": "r1"}}
"#;

#[tokio::test]
async fn agent_calls_ask_until_allowed_for_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fanout.ndjson");
    std::fs::write(&path, FANOUT_SCRIPT.trim()).unwrap();
    let mut harness = Harness::start(&path);
    let HostEvent::AskQueued { fanout, .. } = harness
        .until_host(|host| matches!(host, HostEvent::AskQueued { .. }))
        .await
    else {
        unreachable!()
    };
    let fanout = fanout.expect("the hook's call carries its offer");
    assert_eq!(fanout.key.tool, "Agent");
    assert_eq!(fanout.key.agent_type.as_deref(), Some("Explore"));
    assert_eq!(fanout.model.as_deref(), Some("haiku"));
    let allow = Answer::Allow {
        updated_input: None,
        accept_edits: false,
        folder: None,
        note: None,
        always_for_file: false,
        for_session: true,
    };
    harness.session.answer("cli-2", allow).await.unwrap();
    // The fake sends the result once vigie answered both later hooks as scripted.
    harness.until_result().await;
    harness.finish().await;
}

/// A dangerous ask allowed with a note, which follows as a queued user message,
/// then a safe ask allowed with a blank note, which sends nothing more.
const NOTE_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "system", "subtype": "status", "status": "requesting", "uuid": "u1", "session_id": "s"}}
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
        accept_edits: false,
        folder: None,
        note: Some("Then run the tests.".into()),
        always_for_file: false,
        for_session: false,
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
            attachments: Vec::new(),
            client_id: None,
            mid_turn: true,
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
        accept_edits: false,
        folder: None,
        note: Some("  ".into()),
        always_for_file: false,
        for_session: false,
    };
    harness.session.answer("cli-2", blank).await.unwrap();
    harness.finish().await;
}

/// An agent starts and asks through the parent's channel; a relay to it is
/// forwarded verbatim and acknowledged; then the agent is stopped.
fn relay_script(wrapped: &str) -> String {
    let steps = [
        serde_json::json!({"dir": "in", "msg": {"type": "control_request", "request_id": "init",
            "request": {"subtype": "initialize"}}}),
        serde_json::json!({"dir": "out", "msg": {"type": "control_response", "response": {
            "subtype": "success", "request_id": "init",
            "response": {"account": {"apiProvider": "firstParty"}}}}}),
        serde_json::json!({"dir": "out", "msg": {"type": "system", "subtype": "task_started",
            "task_id": "a1", "tool_use_id": "t1", "task_type": "local_agent",
            "description": "sleeper", "is_backgrounded": true, "session_id": "s", "uuid": "u1"}}),
        serde_json::json!({"dir": "out", "msg": {"type": "control_request", "request_id": "cli-1",
            "request": {"subtype": "can_use_tool", "tool_name": "Bash",
                "input": {"command": "ls"}, "tool_use_id": "t5", "agent_id": "a1"}}}),
        serde_json::json!({"dir": "in", "msg": {"type": "control_response", "response": {
            "subtype": "success", "request_id": "cli-1",
            "response": {"behavior": "allow", "updatedInput": {"command": "ls"}}}}}),
        serde_json::json!({"dir": "in", "msg": {"type": "user",
            "message": {"role": "user", "content": wrapped}, "parent_tool_use_id": null,
            "session_id": "s"}}),
        serde_json::json!({"dir": "out", "msg": {"type": "user", "isReplay": true,
            "message": {"role": "user", "content": wrapped}, "parent_tool_use_id": null,
            "session_id": "s", "uuid": "u2"}}),
        serde_json::json!({"dir": "out", "msg": {"type": "assistant", "parent_tool_use_id": null,
            "session_id": "s", "uuid": "u3", "message": {"id": "m1", "content": [
                {"type": "tool_use", "id": "s1", "name": "SendMessage",
                 "input": {"to": "a1", "message": "Say PINEAPPLE.", "summary": "relay"}}]}}}),
        serde_json::json!({"dir": "out", "msg": {"type": "assistant", "parent_tool_use_id": null,
            "session_id": "s", "uuid": "u4", "message": {"id": "m2", "content": [
                {"type": "text", "text": "[Relay acknowledged]"}]}}}),
        serde_json::json!({"dir": "out", "msg": {"type": "result", "subtype": "success",
            "is_error": false, "duration_ms": 1, "duration_api_ms": 1, "num_turns": 1,
            "stop_reason": null, "total_cost_usd": 0, "usage": {}, "session_id": "s",
            "uuid": "u5"}}),
        serde_json::json!({"dir": "in", "msg": {"type": "control_request", "request_id": "stop-1",
            "request": {"subtype": "stop_task", "task_id": "a1"}}}),
        serde_json::json!({"dir": "out", "msg": {"type": "control_response", "response": {
            "subtype": "success", "request_id": "stop-1", "response": {}}}}),
        serde_json::json!({"dir": "out", "msg": {"type": "system", "subtype": "task_notification",
            "task_id": "a1", "tool_use_id": "t1", "status": "stopped", "summary": "sleeper",
            "session_id": "s", "uuid": "u6"}}),
    ];
    steps
        .iter()
        .map(|step| step.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn a_subagent_asks_through_the_parent_and_takes_a_faithful_relay() {
    use vigie::session::agents::{self, AgentState, RelayOutcome};

    let text = "Say PINEAPPLE.";
    let wrapped = agents::wrap("sleeper", "a1", text);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("relay.ndjson");
    std::fs::write(&path, relay_script(&wrapped)).unwrap();
    let mut harness = Harness::start(&path);

    let HostEvent::AskQueued {
        request_id,
        agent_id,
        ..
    } = harness
        .until_host(|host| matches!(host, HostEvent::AskQueued { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(agent_id.as_deref(), Some("a1"));
    let allow = Answer::Allow {
        updated_input: None,
        note: None,
        accept_edits: false,
        folder: None,
        always_for_file: false,
        for_session: false,
    };
    harness.session.answer(request_id, allow).await.unwrap();
    let mut agents_view = harness.session.agents();
    tokio::time::timeout(
        TIMEOUT,
        agents_view.wait_for(|agents| agents.iter().any(|agent| agent.task_id == "a1")),
    )
    .await
    .expect("agent listed")
    .unwrap();

    assert!(
        harness.session.relay("a9", text, None).await.is_err(),
        "an agent the child did not start"
    );
    harness
        .session
        .relay("a1", text, Some("c1".into()))
        .await
        .unwrap();
    let sent_seq = harness
        .until(|event| match &event.body {
            EventBody::Host(HostEvent::RelaySent {
                text: sent,
                client_id,
                task_id,
                agent_tool_use_id,
                name,
                wrapped: written,
                ..
            }) => {
                assert_eq!(
                    (sent.as_str(), client_id.as_deref(), task_id.as_str()),
                    (text, Some("c1"), "a1")
                );
                assert_eq!(
                    (agent_tool_use_id.as_str(), name.as_str()),
                    ("t1", "sleeper")
                );
                assert_eq!(written, &wrapped);
                Some(event.seq)
            }
            _ => None,
        })
        .await;
    let checked = harness
        .until_host(|host| matches!(host, HostEvent::RelayChecked { .. }))
        .await;
    assert_eq!(
        checked,
        HostEvent::RelayChecked {
            relay_seq: sent_seq,
            task_id: "a1".into(),
            agent_tool_use_id: "t1".into(),
            outcome: RelayOutcome::Faithful {
                send_message: "s1".into()
            },
        }
    );

    harness.session.stop_task("a1").await.unwrap();
    tokio::time::timeout(
        TIMEOUT,
        agents_view.wait_for(|agents| {
            agents
                .iter()
                .any(|agent| agent.task_id == "a1" && agent.state == AgentState::Stopped)
        }),
    )
    .await
    .expect("agent stopped")
    .unwrap();
    harness.finish().await;
    assert!(
        agents_view.borrow().is_empty(),
        "the agents end with their process"
    );
}

/// A safety-check edit allowed for good: its allow carries no rule, and the next
/// edit of the file, spelled in another case, is allowed by the host. A read of
/// the same file is another kind: it asks, and its grant does send a Read rule.
/// An edit of another file still asks.
const FILE_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-1", "request": {"subtype": "can_use_tool", "tool_name": "Edit", "input": {"file_path": "C:\\x\\plans\\p.md"}, "tool_use_id": "t1", "decision_reason_type": "safetyCheck"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-1", "response": {"behavior": "allow", "updatedInput": {"file_path": "C:\\x\\plans\\p.md"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-2", "request": {"subtype": "can_use_tool", "tool_name": "Write", "input": {"file_path": "c:\\X\\plans\\P.md"}, "tool_use_id": "t2", "decision_reason_type": "safetyCheck"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-2", "response": {"behavior": "allow", "updatedInput": {"file_path": "c:\\X\\plans\\P.md"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-3", "request": {"subtype": "can_use_tool", "tool_name": "Read", "input": {"file_path": "C:\\x\\plans\\p.md"}, "tool_use_id": "t3", "decision_reason_type": "workingDir"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-3", "response": {"behavior": "allow", "updatedInput": {"file_path": "C:\\x\\plans\\p.md"}, "updatedPermissions": [{"type": "addRules", "rules": [{"toolName": "Read", "ruleContent": "//c/x/plans/p.md"}], "behavior": "allow", "destination": "session"}]}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-4", "request": {"subtype": "can_use_tool", "tool_name": "Read", "input": {"file_path": "C:\\x\\plans\\p.md"}, "tool_use_id": "t4"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-4", "response": {"behavior": "allow", "updatedInput": {"file_path": "C:\\x\\plans\\p.md"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-5", "request": {"subtype": "can_use_tool", "tool_name": "Edit", "input": {"file_path": "C:\\x\\plans\\q.md"}, "tool_use_id": "t5"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-5", "response": {"behavior": "allow", "updatedInput": {"file_path": "C:\\x\\plans\\q.md"}}}}}
"#;

#[cfg(windows)]
#[tokio::test]
async fn a_file_granted_for_good_is_allowed_by_the_host() {
    use vigie::session::grant::{FileKind, FileOffer};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.ndjson");
    std::fs::write(&path, FILE_SCRIPT.trim()).unwrap();
    let mut harness = Harness::start(&path);
    let plan = |kind| FileOffer {
        kind,
        path: r"C:\x\plans\p.md".into(),
    };
    let always = Answer::Allow {
        updated_input: None,
        note: None,
        accept_edits: false,
        folder: None,
        always_for_file: true,
        for_session: false,
    };
    async fn queued_file(harness: &mut Harness) -> (String, Option<FileOffer>) {
        match harness
            .until_host(|host| matches!(host, HostEvent::AskQueued { .. }))
            .await
        {
            HostEvent::AskQueued {
                request_id, file, ..
            } => (request_id, file),
            _ => unreachable!(),
        }
    }
    async fn auto_allowed(harness: &mut Harness) -> (String, FileOffer) {
        match harness
            .until_host(|host| matches!(host, HostEvent::AutoAllowed { .. }))
            .await
        {
            HostEvent::AutoAllowed {
                request_id, file, ..
            } => (request_id, file),
            _ => unreachable!(),
        }
    }

    let (request_id, file) = queued_file(&mut harness).await;
    assert_eq!(file, Some(plan(FileKind::Edit)));
    harness
        .session
        .answer(request_id, always.clone())
        .await
        .unwrap();
    assert_eq!(
        auto_allowed(&mut harness).await,
        (
            "cli-2".to_string(),
            FileOffer {
                kind: FileKind::Edit,
                path: r"C:\X\plans\P.md".into()
            }
        ),
        "the ask's own spelling"
    );

    let (request_id, file) = queued_file(&mut harness).await;
    assert_eq!(
        (request_id.as_str(), file),
        ("cli-3", Some(plan(FileKind::Read)))
    );
    harness.session.answer(request_id, always).await.unwrap();
    assert_eq!(
        auto_allowed(&mut harness).await,
        ("cli-4".to_string(), plan(FileKind::Read))
    );

    let (request_id, _) = queued_file(&mut harness).await;
    assert_eq!(request_id, "cli-5");
    let allow = Answer::Allow {
        updated_input: None,
        note: None,
        accept_edits: false,
        folder: None,
        always_for_file: false,
        for_session: false,
    };
    harness.session.answer(request_id, allow).await.unwrap();
    harness.finish().await;
}

/// Four asks queue before any answer. Granting `C:\x\a` on the first also
/// allows the queued read below it; the Bash ask and the read outside stay queued.
const QUEUED_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-1", "request": {"subtype": "can_use_tool", "tool_name": "Read", "input": {"file_path": "C:\\x\\a\\b\\1.md"}, "tool_use_id": "t1", "permission_suggestions": [{"type": "addRules", "rules": [{"toolName": "Read", "ruleContent": "//c/x/a/b/**"}], "behavior": "allow", "destination": "session"}]}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-2", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "cat C:\\x\\a\\2.md"}, "tool_use_id": "t2"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-3", "request": {"subtype": "can_use_tool", "tool_name": "Read", "input": {"file_path": "C:\\x\\c\\3.md"}, "tool_use_id": "t3"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-4", "request": {"subtype": "can_use_tool", "tool_name": "Read", "input": {"file_path": "c:\\X\\a\\4.md"}, "tool_use_id": "t4"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-1", "response": {"behavior": "allow", "updatedInput": {"file_path": "C:\\x\\a\\b\\1.md"}, "updatedPermissions": [{"type": "addRules", "rules": [{"toolName": "Read", "ruleContent": "//c/x/a/**"}], "behavior": "allow", "destination": "session"}]}}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-4", "response": {"behavior": "allow", "updatedInput": {"file_path": "c:\\X\\a\\4.md"}}}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-2", "response": {"behavior": "allow", "updatedInput": {"command": "cat C:\\x\\a\\2.md"}}}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-3", "response": {"behavior": "allow", "updatedInput": {"file_path": "C:\\x\\c\\3.md"}}}}}
"#;

#[cfg(windows)]
#[tokio::test]
async fn a_folder_grant_allows_the_queued_reads_it_covers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queued.ndjson");
    std::fs::write(&path, QUEUED_SCRIPT.trim()).unwrap();
    let mut harness = Harness::start(&path);
    let allow = |folder: Option<&str>| Answer::Allow {
        updated_input: None,
        note: None,
        accept_edits: false,
        folder: folder.map(str::to_string),
        always_for_file: false,
        for_session: false,
    };
    let mut queued = Vec::new();
    while queued.len() < 4 {
        if let HostEvent::AskQueued { request_id, .. } = harness
            .until_host(|host| matches!(host, HostEvent::AskQueued { .. }))
            .await
        {
            queued.push(request_id);
        }
    }
    assert_eq!(queued, ["cli-1", "cli-2", "cli-3", "cli-4"]);

    harness
        .session
        .answer("cli-1".to_string(), allow(Some(r"C:\x\a")))
        .await
        .unwrap();
    let mut resolved = Vec::new();
    while resolved.len() < 2 {
        if let HostEvent::AskResolved {
            request_id,
            resolution,
        } = harness
            .until_host(|host| matches!(host, HostEvent::AskResolved { .. }))
            .await
        {
            resolved.push((request_id, resolution));
        }
    }
    assert_eq!(
        resolved[1],
        (
            "cli-4".to_string(),
            Resolution::Answered {
                answer: allow(None)
            }
        ),
        "resolved as a plain allow, as if tapped"
    );

    for request_id in ["cli-2", "cli-3"] {
        harness
            .session
            .answer(request_id.to_string(), allow(None))
            .await
            .unwrap();
    }
    harness.finish().await;
}

/// A subagent's ask and a main agent's ask both wait; once the subagent's times
/// out, its next ask is denied on arrival, and the main agent's is still answered.
const TIMEOUT_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-1", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls"}, "tool_use_id": "t1", "agent_id": "a1"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-2", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "pwd"}, "tool_use_id": "t2"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-1", "response": {"behavior": "deny", "message": "No answer within 4 minutes. Wrap up now: report what is done, what is left, and what needed permission. No other tool use will be approved."}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-3", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls -la"}, "tool_use_id": "t3", "agent_id": "a1"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-3", "response": {"behavior": "deny", "message": "No answer within 4 minutes. Wrap up now: report what is done, what is left, and what needed permission. No other tool use will be approved."}}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-2", "response": {"behavior": "allow", "updatedInput": {"command": "pwd"}}}}}
"#;

#[tokio::test]
async fn an_unanswered_subagent_ask_times_out_and_the_agent_wraps_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timeout.ndjson");
    std::fs::write(&path, TIMEOUT_SCRIPT.trim()).unwrap();
    let mut harness = Harness::start(&path);
    let mut deny_ats = Vec::new();
    for _ in 0..2 {
        let HostEvent::AskQueued {
            request_id,
            deny_at,
            ..
        } = harness
            .until_host(|host| matches!(host, HostEvent::AskQueued { .. }))
            .await
        else {
            unreachable!()
        };
        deny_ats.push((request_id, deny_at));
    }
    let wait = deny_ats[0].1.expect("the subagent's ask has a deny time") - chrono::Utc::now();
    assert!(
        (230..=240).contains(&wait.num_seconds()),
        "4 minutes: {wait}"
    );
    assert_eq!(
        deny_ats[1],
        ("cli-2".to_string(), None),
        "the main agent's ask waits"
    );

    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(241)).await;
    tokio::time::resume();
    let resolved = harness
        .until_host(|host| matches!(host, HostEvent::AskResolved { .. }))
        .await;
    assert!(
        matches!(&resolved, HostEvent::AskResolved {
            request_id,
            resolution: Resolution::TimedOut { message },
        } if request_id == "cli-1" && message.starts_with("No answer within 4 minutes.")),
        "{resolved:?}"
    );
    let denied = harness
        .until_host(|host| matches!(host, HostEvent::AutoDenied { .. }))
        .await;
    assert!(
        matches!(&denied, HostEvent::AutoDenied { request_id, agent_id, .. }
            if request_id == "cli-3" && agent_id == "a1"),
        "{denied:?}"
    );

    let allow = Answer::Allow {
        updated_input: None,
        note: None,
        accept_edits: false,
        folder: None,
        always_for_file: false,
        for_session: false,
    };
    harness
        .session
        .answer("cli-2".to_string(), allow)
        .await
        .unwrap();
    harness.finish().await;
}

/// A read in the session's uploads folder is allowed by the host; an edit there,
/// and a read in another session's folder, still ask.
const UPLOADS_SCRIPT: &str = r#"
{"t": 0, "dir": "in", "msg": {"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}}
{"t": 0, "dir": "out", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-1", "request": {"subtype": "can_use_tool", "tool_name": "Read", "input": {"file_path": "C:\\x\\uploads\\s1\\ab12-shot.png"}, "tool_use_id": "t1"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-1", "response": {"behavior": "allow", "updatedInput": {"file_path": "C:\\x\\uploads\\s1\\ab12-shot.png"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-2", "request": {"subtype": "can_use_tool", "tool_name": "Edit", "input": {"file_path": "C:\\x\\uploads\\s1\\ab12-shot.png"}, "tool_use_id": "t2"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-2", "response": {"behavior": "allow", "updatedInput": {"file_path": "C:\\x\\uploads\\s1\\ab12-shot.png"}}}}}
{"t": 0, "dir": "out", "msg": {"type": "control_request", "request_id": "cli-3", "request": {"subtype": "can_use_tool", "tool_name": "Read", "input": {"file_path": "C:\\x\\uploads\\s2\\cd34-other.png"}, "tool_use_id": "t3"}}}
{"t": 0, "dir": "in", "msg": {"type": "control_response", "response": {"subtype": "success", "request_id": "cli-3", "response": {"behavior": "allow", "updatedInput": {"file_path": "C:\\x\\uploads\\s2\\cd34-other.png"}}}}}
"#;

#[cfg(windows)]
#[tokio::test]
async fn reads_in_the_uploads_folder_are_allowed_by_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("uploads.ndjson");
    std::fs::write(&path, UPLOADS_SCRIPT.trim()).unwrap();
    let mut harness = Harness::start_with_uploads(&path, Some(r"C:\x\uploads\s1".into()));
    let allowed = harness
        .until_host(|host| matches!(host, HostEvent::AutoAllowed { .. }))
        .await;
    assert!(matches!(allowed, HostEvent::AutoAllowed { request_id, .. } if request_id == "cli-1"));
    let allow = Answer::Allow {
        updated_input: None,
        note: None,
        accept_edits: false,
        folder: None,
        always_for_file: false,
        for_session: false,
    };
    for expected in ["cli-2", "cli-3"] {
        let HostEvent::AskQueued { request_id, .. } = harness
            .until_host(|host| matches!(host, HostEvent::AskQueued { .. }))
            .await
        else {
            unreachable!()
        };
        assert_eq!(request_id, expected);
        harness
            .session
            .answer(request_id, allow.clone())
            .await
            .unwrap();
    }
    harness.finish().await;
}

/// `set_attachments` through the SDK MCP channel: a bad path fails and keeps
/// the list, a later call replaces it, and the turn's end announces the last
/// one; a turn without a call announces nothing.
#[tokio::test]
async fn set_attachments_announces_the_last_list_when_the_turn_ends() {
    use serde_json::json;
    use vigie::session::attachments::digest_hex;

    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.png");
    let second = dir.path().join("second.txt");
    std::fs::write(&first, b"png").unwrap();
    std::fs::write(&second, b"txt").unwrap();
    let missing = dir.path().join("missing.txt");
    let [first, second, missing] = [first, second, missing].map(|path| path.display().to_string());

    let record = |dir: &str, msg: Value| json!({"t": 0, "dir": dir, "msg": msg}).to_string();
    let call = |request_id: &str, id: u64, paths: &[&str]| {
        record(
            "out",
            json!({"type": "control_request", "request_id": request_id, "request": {
                "subtype": "mcp_message", "server_name": "vigie", "message": {
                    "jsonrpc": "2.0", "id": id, "method": "tools/call",
                    "params": {"name": "set_attachments", "arguments": {"paths": paths}}}}}),
        )
    };
    let reply = |request_id: &str, id: u64, text: String, is_error: bool| {
        record(
            "in",
            json!({"type": "control_response", "response": {"subtype": "success",
                "request_id": request_id, "response": {"mcp_response": {
                    "jsonrpc": "2.0", "id": id, "result": {
                        "content": [{"type": "text", "text": text}], "isError": is_error}}}}}),
        )
    };
    let result = |uuid: &str| {
        record(
            "out",
            json!({"type": "result", "subtype": "success", "is_error": false, "result": "done",
                "duration_ms": 1, "duration_api_ms": 1, "num_turns": 1, "stop_reason": "end_turn",
                "total_cost_usd": 0, "usage": {}, "session_id": "s", "uuid": uuid}),
        )
    };
    let script = [
        record(
            "in",
            json!({"type": "control_request", "request_id": "init", "request": {
                "subtype": "initialize", "sdkMcpServers": ["vigie"]}}),
        ),
        record(
            "out",
            json!({"type": "control_response", "response": {"subtype": "success",
                "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}),
        ),
        record(
            "out",
            json!({"type": "control_request", "request_id": "mcp-1", "request": {
                "subtype": "mcp_message", "server_name": "vigie",
                "message": {"jsonrpc": "2.0", "method": "notifications/initialized"}}}),
        ),
        record(
            "in",
            json!({"type": "control_response", "response": {"subtype": "success",
                "request_id": "mcp-1", "response": {"mcp_response": {
                    "jsonrpc": "2.0", "result": {}, "id": 0}}}}),
        ),
        call("mcp-2", 1, &[&first]),
        reply(
            "mcp-2",
            1,
            format!("These files will be attached to this turn's reply:\n- {first}"),
            false,
        ),
        call("mcp-3", 2, &[&first, &missing]),
        reply(
            "mcp-3",
            2,
            format!(
                "Nothing changed, the attachments stay as they were. Bad paths:\n- {missing}: not found"
            ),
            true,
        ),
        call("mcp-4", 3, &[&second]),
        reply(
            "mcp-4",
            3,
            format!("These files will be attached to this turn's reply:\n- {second}"),
            false,
        ),
        result("r1"),
        result("r2"),
    ];
    let path = dir.path().join("attach.ndjson");
    std::fs::write(&path, script.join("\n")).unwrap();

    let mut harness = Harness::start(&path);
    let attached = harness
        .until_host(|host| matches!(host, HostEvent::ClaudeAttached { .. }))
        .await;
    assert_eq!(
        attached,
        HostEvent::ClaudeAttached {
            hashes: [(second.clone(), digest_hex(b"txt"))].into(),
            attachments: vec![second],
        }
    );
    harness.until_result().await;
    tokio::time::timeout(TIMEOUT, harness.session.stop())
        .await
        .expect("session did not stop");
    let rest = harness
        .until(|event| match &event.body {
            EventBody::Host(HostEvent::ClaudeAttached { .. }) => Some(false),
            EventBody::Host(HostEvent::ChildExited { code, .. }) => {
                assert_eq!(*code, Some(0), "fake claude failed");
                Some(true)
            }
            _ => None,
        })
        .await;
    assert!(rest, "a turn without a call announced attachments");
}

/// The same path attached in two turns, rewritten in between: each
/// `claude_attached` carries the hash of the content at its own turn's end.
#[tokio::test]
async fn reattaching_a_rewritten_path_records_each_content_hash() {
    use serde_json::json;
    use vigie::session::attachments::digest_hex;

    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("main.png");
    std::fs::write(&image, b"first").unwrap();
    let image = image.display().to_string();

    let record = |dir: &str, msg: Value| json!({"t": 0, "dir": dir, "msg": msg}).to_string();
    let call = |request_id: &str, id: u64| {
        record(
            "out",
            json!({"type": "control_request", "request_id": request_id, "request": {
                "subtype": "mcp_message", "server_name": "vigie", "message": {
                    "jsonrpc": "2.0", "id": id, "method": "tools/call",
                    "params": {"name": "set_attachments", "arguments": {"paths": [&image]}}}}}),
        )
    };
    let reply = |request_id: &str, id: u64| {
        let text = format!("These files will be attached to this turn's reply:\n- {image}");
        record(
            "in",
            json!({"type": "control_response", "response": {"subtype": "success",
                "request_id": request_id, "response": {"mcp_response": {
                    "jsonrpc": "2.0", "id": id, "result": {
                        "content": [{"type": "text", "text": text}], "isError": false}}}}}),
        )
    };
    let result = |uuid: &str| {
        record(
            "out",
            json!({"type": "result", "subtype": "success", "is_error": false, "result": "done",
                "duration_ms": 1, "duration_api_ms": 1, "num_turns": 1, "stop_reason": "end_turn",
                "total_cost_usd": 0, "usage": {}, "session_id": "s", "uuid": uuid}),
        )
    };
    let script = [
        record(
            "in",
            json!({"type": "control_request", "request_id": "init", "request": {
                "subtype": "initialize", "sdkMcpServers": ["vigie"]}}),
        ),
        record(
            "out",
            json!({"type": "control_response", "response": {"subtype": "success",
                "request_id": "init", "response": {"account": {"apiProvider": "firstParty"}}}}),
        ),
        call("mcp-1", 1),
        reply("mcp-1", 1),
        result("r1"),
        record(
            "in",
            json!({"type": "user", "message": {"role": "user", "content": "again"},
                "parent_tool_use_id": null, "session_id": "s"}),
        ),
        call("mcp-2", 2),
        reply("mcp-2", 2),
        result("r2"),
    ];
    let path = dir.path().join("reattach.ndjson");
    std::fs::write(&path, script.join("\n")).unwrap();

    async fn attached(harness: &mut Harness) -> BTreeMap<String, String> {
        let HostEvent::ClaudeAttached { hashes, .. } = harness
            .until_host(|host| matches!(host, HostEvent::ClaudeAttached { .. }))
            .await
        else {
            unreachable!()
        };
        hashes
    }

    let mut harness = Harness::start(&path);
    let first = attached(&mut harness).await;
    std::fs::write(&image, b"second").unwrap();
    harness.session.send("again", None).await.unwrap();
    let second = attached(&mut harness).await;
    assert_eq!(first.get(&image), Some(&digest_hex(b"first")));
    assert_eq!(second.get(&image), Some(&digest_hex(b"second")));
    harness.finish().await;
}
