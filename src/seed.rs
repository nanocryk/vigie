//! The seed of a continued discussion: its last exchanges, rendered from vigie2's
//! stored events the way `/tail-session` renders a transcript.
//!
//! An exchange is a user message echoed by the CLI and every main-conversation
//! assistant block until the next one. Each keeps the user text cut at
//! `USER_LIMIT` characters, each text block cut at `TEXT_LIMIT`, and one line
//! per tool call with its argument and result cut at `TOOL_LIMIT`; thinking is
//! dropped. The seed a `restarted` event carries is stripped from the echo of the
//! first message that followed it, so a later seed never nests an earlier one.

use std::collections::HashMap;
use std::path::Path;

use chrono::NaiveDate;
use serde_json::Value;

use crate::protocol::{ContentBlock, Inbound, MessageContent};
use crate::session::HostEvent;
use crate::store::{EventKind, StoredEvent};

pub const SEED_EXCHANGES: usize = 5;
const USER_LIMIT: usize = 1200;
const TEXT_LIMIT: usize = 800;
const TOOL_LIMIT: usize = 100;
/// Ends a seed sent without a typed message.
pub const CLOSING: &str = "Say what you want to do next.";
const INTERRUPT_PREFIX: &str = "[Request interrupted by user";
/// Input keys tried in order for the argument shown on a tool line.
const ARG_KEYS: [&str; 11] = [
    "file_path",
    "path",
    "command",
    "pattern",
    "query",
    "url",
    "description",
    "skill",
    "prompt",
    "old_string",
    "subagent_type",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Seed {
    pub text: String,
    /// Exchanges the seed gives.
    pub exchanges: usize,
    /// Exchanges in the whole discussion.
    pub total_exchanges: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum Block {
    Text(String),
    Tool {
        id: String,
        name: String,
        input: Value,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct Exchange {
    user: String,
    blocks: Vec<Block>,
}

/// The exchanges of a discussion, across every Claude session behind it.
#[derive(Debug, Default)]
pub struct History {
    exchanges: Vec<Exchange>,
    /// Tool result text per tool use id.
    results: HashMap<String, String>,
}

impl History {
    pub fn from_events(events: &[StoredEvent]) -> Self {
        let mut history = Self::default();
        let mut pending_seed: Option<String> = None;
        for event in events {
            match event.kind {
                EventKind::Host => {
                    if let Ok(HostEvent::Restarted {
                        seed: Some(seed), ..
                    }) = serde_json::from_str(&event.payload)
                    {
                        pending_seed = Some(seed);
                    }
                }
                EventKind::Line => match serde_json::from_str::<Inbound>(&event.payload) {
                    Ok(Inbound::User(user)) if user.parent_tool_use_id.is_none() => {
                        let is_meta = user.extra.get("isMeta") == Some(&Value::Bool(true));
                        let Some(text) = history.user_line(user.message.content) else {
                            continue;
                        };
                        if is_meta || text.starts_with(INTERRUPT_PREFIX) {
                            continue;
                        }
                        let text = match pending_seed.take() {
                            Some(seed) => match text.strip_prefix(&format!("{seed}\n\n")) {
                                Some(rest) => rest.to_string(),
                                None => text,
                            },
                            None => text,
                        };
                        history.exchanges.push(Exchange {
                            user: text,
                            blocks: Vec::new(),
                        });
                    }
                    Ok(Inbound::Assistant(assistant)) if assistant.parent_tool_use_id.is_none() => {
                        let Some(exchange) = history.exchanges.last_mut() else {
                            continue;
                        };
                        for block in assistant.message.content {
                            match block {
                                ContentBlock::Text { text, .. } => {
                                    exchange.blocks.push(Block::Text(text))
                                }
                                ContentBlock::ToolUse {
                                    id, name, input, ..
                                } => exchange.blocks.push(Block::Tool { id, name, input }),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                },
            }
        }
        history
    }

    /// Records the tool results of a user line; returns its text, if any.
    fn user_line(&mut self, content: MessageContent) -> Option<String> {
        match content {
            MessageContent::Text(text) => Some(text),
            MessageContent::Blocks(blocks) => {
                let mut texts = Vec::new();
                for block in blocks {
                    match block {
                        ContentBlock::Text { text, .. } => texts.push(text),
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            if let Some(result) = result_text(&content) {
                                self.results.insert(tool_use_id, result);
                            }
                        }
                        _ => {}
                    }
                }
                (!texts.is_empty()).then(|| texts.join("\n"))
            }
        }
    }

    pub fn total_exchanges(&self) -> usize {
        self.exchanges.len()
    }

    /// The seed of a discussion started on `started` in `cwd`, without `CLOSING`.
    pub fn seed(&self, cwd: &Path, started: NaiveDate) -> Seed {
        let total = self.exchanges.len();
        let shown = &self.exchanges[total.saturating_sub(SEED_EXCHANGES)..];
        let mut text = format!(
            "Continuing an earlier vigie2 session in {}, started {}. Its last {} of {} exchanges, oldest first:",
            cwd.display(),
            started.format("%Y-%m-%d"),
            shown.len(),
            total
        );
        for (index, exchange) in shown.iter().enumerate() {
            text.push_str(&format!(
                "\n\n## Exchange {}\nUser: {}\n\nClaude:",
                index + 1,
                truncate(&exchange.user, USER_LIMIT)
            ));
            let mut emitted = false;
            for block in &exchange.blocks {
                match block {
                    Block::Text(body) => {
                        let body = body.trim();
                        if body.is_empty() {
                            continue;
                        }
                        text.push('\n');
                        text.push_str(&truncate(body, TEXT_LIMIT));
                    }
                    Block::Tool { id, name, input } => {
                        text.push('\n');
                        text.push_str(&self.tool_line(id, name, input));
                    }
                }
                emitted = true;
            }
            if !emitted {
                text.push_str("\n(no text or tool output)");
            }
        }
        Seed {
            text,
            exchanges: shown.len(),
            total_exchanges: total,
        }
    }

    fn tool_line(&self, id: &str, name: &str, input: &Value) -> String {
        let arg = match input {
            Value::Object(fields) => ARG_KEYS
                .iter()
                .find_map(|key| fields.get(*key).map(|value| (*key, value)))
                .or_else(|| {
                    fields
                        .iter()
                        .next()
                        .map(|(key, value)| (key.as_str(), value))
                })
                .map(|(key, value)| format!("{key}={}", truncate(&value_text(value), TOOL_LIMIT)))
                .unwrap_or_default(),
            _ => String::new(),
        };
        let result = match self.results.get(id) {
            Some(result) if !result.is_empty() => format!(" → {}", truncate(result, TOOL_LIMIT)),
            _ => String::new(),
        };
        format!("  • {name}({arg}){result}")
    }
}

/// The first user message of a continued discussion: the seed, then the typed
/// text, or `CLOSING` when nothing was typed.
pub fn first_message(seed: &str, text: &str) -> String {
    let next = if text.trim().is_empty() {
        CLOSING
    } else {
        text
    };
    format!("{seed}\n\n{next}")
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A tool result is a string or a list of blocks, of which the first text one counts.
fn result_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => blocks
            .iter()
            .find(|block| block["type"] == "text")
            .and_then(|block| block["text"].as_str())
            .map(str::to_string),
        _ => None,
    }
}

/// Whitespace runs collapse to one space; past `limit` characters the text is
/// cut to `limit - 1` and an ellipsis.
fn truncate(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let mut cut: String = flat.chars().take(limit - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::replay::{Direction, Recording};
    use crate::session::{Event, EventBody, Line, RestartMode};
    use crate::store::Store;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
    }

    /// Stores the `out` lines of the named recordings, one after the other, and
    /// reads them back.
    async fn stored_recordings(names: &[&str]) -> Vec<StoredEvent> {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let id = Uuid::new_v4();
        store.session_started(id, dir.path(), Utc::now(), None);
        let mut seq = 0;
        for name in names {
            let recording = Recording::load(
                &Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("spike/runs")
                    .join(format!("{name}.ndjson")),
            )
            .unwrap();
            for step in recording
                .steps
                .iter()
                .filter(|step| step.dir == Direction::Out)
            {
                seq += 1;
                let raw = step.msg.to_string();
                let body = EventBody::Line(Box::new(Line {
                    parsed: serde_json::from_str(&raw).ok(),
                    raw,
                }));
                store.record_event(
                    id,
                    &Event {
                        seq,
                        time: Utc::now(),
                        body,
                    },
                );
            }
        }
        store.events_after(id, 0, u32::MAX).await.unwrap()
    }

    #[tokio::test]
    async fn seed_of_the_cd_recording() {
        let events = stored_recordings(&["cd"]).await;
        let seed = History::from_events(&events).seed(Path::new("/work/p"), date());
        assert_eq!(seed.exchanges, 1);
        assert_eq!(seed.total_exchanges, 1);
        assert_eq!(
            seed.text,
            "Continuing an earlier vigie2 session in /work/p, started 2026-09-27. \
             Its last 1 of 1 exchanges, oldest first:\n\n\
             ## Exchange 1\n\
             User: Use the Bash tool to run exactly: pwd. Reply with its output.\n\n\
             Claude:\n  • Bash(command=pwd) → /tmp/vigie2-spike/cwd\n\
             ``` /tmp/vigie2-spike/cwd ``` `/cd` didn't run, so the shell is still in `cwd`, \
             not `spike-cd-target`. Separately, the claude.ai Google Drive connector needs \
             authorization in your claude.ai connector settings. It won't work until you do that."
        );
    }

    #[tokio::test]
    async fn exchanges_span_every_recording_and_the_seed_keeps_the_last_five() {
        let names = [
            "basic",
            "cd",
            "interrupt",
            "permissions",
            "plan",
            "background",
        ];
        let events = stored_recordings(&names).await;
        let history = History::from_events(&events);
        let total = history.total_exchanges();
        assert!(total > SEED_EXCHANGES, "only {total} exchanges");
        let seed = history.seed(Path::new("/p"), date());
        assert_eq!(seed.exchanges, SEED_EXCHANGES);
        assert_eq!(seed.total_exchanges, total);
        assert_eq!(seed.text.matches("\n## Exchange ").count(), SEED_EXCHANGES);
        assert!(!seed.text.contains("Reply with exactly: ok"));
        assert!(!seed.text.contains(INTERRUPT_PREFIX));
    }

    fn host(seq: u64, event: HostEvent) -> StoredEvent {
        StoredEvent {
            seq,
            time: Utc::now(),
            kind: EventKind::Host,
            payload: serde_json::to_string(&event).unwrap(),
        }
    }

    fn line(seq: u64, value: Value) -> StoredEvent {
        StoredEvent {
            seq,
            time: Utc::now(),
            kind: EventKind::Line,
            payload: value.to_string(),
        }
    }

    fn user(seq: u64, content: Value) -> StoredEvent {
        line(
            seq,
            serde_json::json!({"type": "user", "message": {"role": "user", "content": content},
                "parent_tool_use_id": null, "session_id": "s", "isReplay": true}),
        )
    }

    fn assistant(seq: u64, content: Value) -> StoredEvent {
        line(
            seq,
            serde_json::json!({"type": "assistant", "message": {"id": "m", "content": content},
                "parent_tool_use_id": null, "session_id": "s", "uuid": "u"}),
        )
    }

    #[test]
    fn truncation_and_tool_lines() {
        let long_user = format!("{}  tail", "u".repeat(1300));
        let long_text = "t ".repeat(500);
        let long_command = "x".repeat(150);
        let events = [
            user(1, Value::String(long_user)),
            assistant(
                2,
                serde_json::json!([
                    {"type": "thinking", "thinking": "hidden"},
                    {"type": "text", "text": long_text},
                    {"type": "tool_use", "id": "t1", "name": "Bash",
                        "input": {"description": "d", "command": long_command}},
                    {"type": "tool_use", "id": "t2", "name": "Custom", "input": {"b": [1], "a": 2}},
                    {"type": "tool_use", "id": "t3", "name": "Empty", "input": {}},
                ]),
            ),
            user(
                3,
                serde_json::json!([
                    {"type": "tool_result", "tool_use_id": "t1", "content": "line one\n\nline two"},
                    {"type": "tool_result", "tool_use_id": "t2",
                        "content": [{"type": "image"}, {"type": "text", "text": "done"}]},
                ]),
            ),
            user(4, Value::String("[Request interrupted by user]".into())),
            line(
                5,
                serde_json::json!({"type": "user", "isMeta": true, "session_id": "s",
                    "message": {"role": "user", "content": "meta"}}),
            ),
        ];
        let seed = History::from_events(&events).seed(Path::new("/p"), date());
        assert_eq!(seed.total_exchanges, 1);
        let lines: Vec<&str> = seed.text.lines().collect();
        let user_line = lines
            .iter()
            .find(|line| line.starts_with("User: "))
            .unwrap();
        assert_eq!(user_line.chars().count(), "User: ".len() + USER_LIMIT);
        assert!(user_line.ends_with("u…"));
        let text_line = lines.iter().find(|line| line.starts_with("t t")).unwrap();
        assert_eq!(text_line.chars().count(), TEXT_LIMIT);
        assert!(!seed.text.contains("hidden"));
        assert!(lines.contains(
            &format!("  • Bash(command={}…) → line one line two", "x".repeat(99)).as_str()
        ));
        assert!(lines.contains(&"  • Custom(a=2) → done"));
        assert!(lines.contains(&"  • Empty()"));
        assert!(!seed.text.contains("meta"));
    }

    #[test]
    fn an_earlier_seed_is_stripped_from_its_message() {
        let earlier = "Continuing an earlier vigie2 session in /p.".to_string();
        let events = [
            user(1, Value::String("first".into())),
            assistant(2, serde_json::json!([{"type": "text", "text": "one"}])),
            host(
                3,
                HostEvent::Restarted {
                    mode: RestartMode::Continue,
                    exchanges: 1,
                    total_exchanges: 1,
                    seed: Some(earlier.clone()),
                },
            ),
            user(4, Value::String(first_message(&earlier, "second"))),
            assistant(5, serde_json::json!([])),
            host(
                6,
                HostEvent::Restarted {
                    mode: RestartMode::Continue,
                    exchanges: 2,
                    total_exchanges: 2,
                    seed: Some(earlier.clone()),
                },
            ),
            user(7, Value::String(first_message(&earlier, ""))),
        ];
        let seed = History::from_events(&events).seed(Path::new("/p"), date());
        assert_eq!(seed.total_exchanges, 3);
        assert!(
            seed.text
                .contains("## Exchange 2\nUser: second\n\nClaude:\n(no text or tool output)")
        );
        assert!(seed.text.ends_with(&format!(
            "## Exchange 3\nUser: {CLOSING}\n\nClaude:\n(no text or tool output)"
        )));
        assert_eq!(seed.text.matches("Continuing").count(), 1);
    }

    #[test]
    fn an_empty_history_seeds_no_exchange() {
        let seed = History::from_events(&[]).seed(Path::new("/p"), date());
        assert_eq!((seed.exchanges, seed.total_exchanges), (0, 0));
        assert!(
            seed.text
                .ends_with("Its last 0 of 0 exchanges, oldest first:")
        );
        assert_eq!(first_message("S", "  "), format!("S\n\n{CLOSING}"));
        assert_eq!(first_message("S", "go on"), "S\n\ngo on");
    }
}
