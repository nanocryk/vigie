//! Pruning of stored lines nobody reads anymore.
//!
//! Streams: while a message streams, its `stream_event` lines build its blocks;
//! once done, its `assistant` lines carry every block again, and the transcript
//! shows those. The block lines (`content_block_start`, `_delta`, `_stop`) of a
//! finished message are deleted when its `assistant` lines carry the same blocks:
//! same count and types, and the same text for text blocks. Tool inputs and
//! thinking are not compared: the CLI fills in tool input defaults, and past tool
//! calls are not read closely. Always kept: the message lines (`message_start`
//! carries `ttft_ms`, `message_delta` the final usage), and every line of a
//! message without `assistant` lines (an interrupted reply: its deltas are its
//! only copy).
//!
//! Tool outputs: in `user` lines older than a cutoff, a tool result over
//! `RESULT_LIMIT` bytes is replaced by a marker giving its size, in both copies
//! the CLI stores (`message.content[].content` and `tool_use_result`). A file
//! edit keeps the `type`, `filePath` and `structuredPatch` of its
//! `tool_use_result`, which the diff view draws. Edits go through SQLite's
//! `json_set`, so the rest of each line keeps its text and key order.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde_json::Value;

use super::format_time;

/// Tool results up to this size, serialized, are kept: errors, test totals,
/// short command outputs.
pub const RESULT_LIMIT: usize = 1024;
/// What a file edit's `tool_use_result` keeps: what the diff view reads.
const PATCH_FIELDS: [&str; 3] = ["type", "filePath", "structuredPatch"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Counts what would be deleted.
    DryRun,
    Delete,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StreamPrune {
    /// Messages whose block lines are deleted.
    pub messages: usize,
    pub rows: usize,
    pub bytes: u64,
    /// Messages kept whole, counted by why.
    pub kept: BTreeMap<String, usize>,
    /// The first kept message of each reason: session id, message id.
    pub examples: BTreeMap<String, (String, String)>,
    /// Stream lines outside any message, or not JSON: kept.
    pub stray_rows: usize,
}

impl StreamPrune {
    fn add(&mut self, other: Self) {
        self.messages += other.messages;
        self.rows += other.rows;
        self.bytes += other.bytes;
        for (reason, count) in other.kept {
            *self.kept.entry(reason).or_default() += count;
        }
        for (reason, example) in other.examples {
            self.examples.entry(reason).or_insert(example);
        }
        self.stray_rows += other.stray_rows;
    }

    fn keep(&mut self, reason: String, session: &str, message: Option<&str>) {
        *self.kept.entry(reason.clone()).or_default() += 1;
        self.examples
            .entry(reason)
            .or_insert_with(|| (session.to_string(), message.unwrap_or_default().to_string()));
    }
}

/// One content block as its stream lines build it.
struct Block {
    /// The `content_block` of its start line.
    start: Value,
    /// The streamed text of a text block.
    text: String,
    seqs: Vec<u64>,
    bytes: u64,
}

impl Block {
    fn kind(&self) -> Option<&str> {
        self.start.get("type").and_then(Value::as_str)
    }

    /// Why `answer` does not carry this block, if it does not.
    fn differs(&self, answer: &Value) -> Option<String> {
        let kind = self.kind().unwrap_or("?");
        let answered = answer.get("type").and_then(Value::as_str).unwrap_or("?");
        if kind != answered {
            return Some(format!("{kind} block streamed, {answered} answered"));
        }
        if kind != "text" {
            return None;
        }
        let start = self.start.get("text").and_then(Value::as_str).unwrap_or("");
        let text = answer.get("text").and_then(Value::as_str);
        (text != Some(&format!("{start}{}", self.text))).then(|| "text differs".into())
    }
}

/// The stream lines of one message, from `message_start` to `message_stop`.
struct Run {
    message_id: Option<String>,
    blocks: BTreeMap<u32, Block>,
    /// Why a line does not fit a block, for the first one.
    unexpected: Option<&'static str>,
}

impl Run {
    /// Adds a block line, or says why it does not fit a block.
    fn take(&mut self, seq: u64, bytes: u64, event: &Value) -> Result<(), &'static str> {
        let index = event
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| u32::try_from(index).ok())
            .ok_or("event without a block index")?;
        let block = match event.get("type").and_then(Value::as_str) {
            Some("content_block_start") => {
                if self.blocks.contains_key(&index) {
                    return Err("block started twice");
                }
                let start = event.get("content_block").cloned().unwrap_or(Value::Null);
                if !start.is_object() {
                    return Err("block start without a block");
                }
                let block = Block {
                    start,
                    text: String::new(),
                    seqs: Vec::new(),
                    bytes: 0,
                };
                self.blocks.entry(index).or_insert(block)
            }
            Some("content_block_delta") => {
                let block = self
                    .blocks
                    .get_mut(&index)
                    .ok_or("delta before its block start")?;
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = delta
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or("text delta without text")?;
                        block.text.push_str(text);
                    }
                    Some("input_json_delta" | "thinking_delta" | "signature_delta") => {}
                    _ => return Err("delta of an unknown type"),
                }
                block
            }
            Some("content_block_stop") => self
                .blocks
                .get_mut(&index)
                .ok_or("stop before its block start")?,
            _ => return Err("unknown stream event"),
        };
        block.seqs.push(seq);
        block.bytes += bytes;
        Ok(())
    }
}

/// Prunes the stream lines of every session, one transaction per session.
pub fn prune_all_streams(connection: &Connection, mode: Mode) -> Result<StreamPrune> {
    let sessions: Vec<String> = connection
        .prepare("SELECT id FROM sessions ORDER BY created_at")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut total = StreamPrune::default();
    for session in sessions {
        let transaction = connection.unchecked_transaction()?;
        total.add(prune_streams(&transaction, &session, mode)?);
        transaction.commit()?;
    }
    Ok(total)
}

/// Prunes the block lines of the finished messages of `session` whose
/// `assistant` lines carry the same blocks.
pub fn prune_streams(connection: &Connection, session: &str, mode: Mode) -> Result<StreamPrune> {
    let mut report = StreamPrune::default();
    let mut open: HashMap<Option<String>, Run> = HashMap::new();
    let mut finished = Vec::new();
    let mut answers: HashMap<String, Vec<Value>> = HashMap::new();
    let mut statement = connection.prepare(
        "SELECT seq, payload FROM events
         WHERE session_id = ?1 AND kind = 'line'
             AND json_extract(payload, '$.type') IN ('stream_event', 'assistant')
         ORDER BY seq",
    )?;
    let mut rows = statement.query([session])?;
    while let Some(row) = rows.next()? {
        let seq: u64 = row.get(0)?;
        let payload: String = row.get(1)?;
        let Ok(line) = serde_json::from_str::<Value>(&payload) else {
            report.stray_rows += 1;
            continue;
        };
        if line.get("type").and_then(Value::as_str) == Some("assistant") {
            let message = line.get("message");
            let id = message
                .and_then(|message| message.get("id"))
                .and_then(Value::as_str);
            let content = message
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array);
            if let Some(id) = id {
                let blocks = answers.entry(id.to_string()).or_default();
                blocks.extend(content.into_iter().flatten().cloned());
            }
            continue;
        }
        let scope = line
            .get("parent_tool_use_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let event = line.get("event").unwrap_or(&Value::Null);
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(previous) = open.remove(&scope) {
                    // A new message before the previous one stopped.
                    report.keep("unfinished".into(), session, previous.message_id.as_deref());
                }
                let message_id = event
                    .get("message")
                    .and_then(|message| message.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let run = Run {
                    message_id,
                    blocks: BTreeMap::new(),
                    unexpected: None,
                };
                open.insert(scope, run);
            }
            Some("message_stop") => {
                finished.extend(open.remove(&scope));
            }
            Some("message_delta") => {}
            _ => {
                let Some(run) = open.get_mut(&scope) else {
                    report.stray_rows += 1;
                    continue;
                };
                if let Err(reason) = run.take(seq, payload.len() as u64, event) {
                    run.unexpected.get_or_insert(reason);
                }
            }
        }
    }
    for run in open.into_values() {
        report.keep("unfinished".into(), session, run.message_id.as_deref());
    }

    let mut doomed = Vec::new();
    for run in finished {
        let id = run.message_id.as_deref();
        if run.blocks.is_empty() {
            // Already pruned: nothing to delete.
            continue;
        }
        if let Some(reason) = run.unexpected {
            report.keep(reason.into(), session, id);
            continue;
        }
        let Some(answer) = id.and_then(|id| answers.get(id)) else {
            report.keep("no assistant line".into(), session, id);
            continue;
        };
        if run.blocks.len() != answer.len() {
            let reason = format!(
                "{} blocks streamed, {} answered",
                run.blocks.len(),
                answer.len()
            );
            report.keep(reason, session, id);
            continue;
        }
        let differs = run
            .blocks
            .values()
            .zip(answer)
            .find_map(|(block, answer)| block.differs(answer));
        if let Some(reason) = differs {
            report.keep(reason, session, id);
            continue;
        }
        report.messages += 1;
        for block in run.blocks.into_values() {
            report.rows += block.seqs.len();
            report.bytes += block.bytes;
            doomed.extend(block.seqs);
        }
    }
    if mode == Mode::Delete {
        let mut delete =
            connection.prepare("DELETE FROM events WHERE session_id = ?1 AND seq = ?2")?;
        for seq in doomed {
            delete.execute(params![session, seq])?;
        }
    }
    Ok(report)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ResultPrune {
    /// `user` lines changed.
    pub lines: usize,
    /// Tool result copies replaced or cut.
    pub results: usize,
    /// Payload size of the changed lines, before and after.
    pub bytes_before: u64,
    pub bytes_after: u64,
}

/// The marker replacing a tool result of `bytes` serialized bytes.
pub fn pruned_marker(bytes: usize) -> String {
    format!("[output pruned, {} KB]", bytes.div_ceil(1024))
}

/// What replaces a tool result copy, or `None` to keep it.
fn pruned(result: &Value) -> Option<Value> {
    let bytes = result.to_string().len();
    if bytes <= RESULT_LIMIT {
        return None;
    }
    let patch = result
        .as_object()
        .filter(|fields| fields.contains_key("structuredPatch"));
    let replacement = match patch {
        Some(fields) => Value::Object(
            fields
                .iter()
                .filter(|(key, _)| PATCH_FIELDS.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        None => Value::String(pruned_marker(bytes)),
    };
    (replacement != *result).then_some(replacement)
}

/// The tool result copies to replace in one line: `json_set` paths and values.
struct LineEdit {
    session: String,
    seq: u64,
    changes: Vec<(String, Value)>,
}

/// Replaces the tool results over `RESULT_LIMIT` in `user` lines stored before
/// `before`. The caller owns the transaction: a dry run rolls it back.
pub fn prune_tool_results(connection: &Connection, before: DateTime<Utc>) -> Result<ResultPrune> {
    let mut edits = Vec::new();
    {
        let mut statement = connection.prepare(
            "SELECT session_id, seq, payload FROM events
             WHERE kind = 'line' AND time < ?1 AND json_extract(payload, '$.type') = 'user'",
        )?;
        let mut rows = statement.query([format_time(before)])?;
        while let Some(row) = rows.next()? {
            let payload: String = row.get(2)?;
            let Ok(line) = serde_json::from_str::<Value>(&payload) else {
                continue;
            };
            let mut changes = Vec::new();
            let blocks = line
                .pointer("/message/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate();
            for (index, block) in blocks {
                if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                if let Some(replacement) = block.get("content").and_then(pruned) {
                    changes.push((format!("$.message.content[{index}].content"), replacement));
                }
            }
            if let Some(replacement) = line.get("tool_use_result").and_then(pruned) {
                changes.push(("$.tool_use_result".to_string(), replacement));
            }
            if !changes.is_empty() {
                edits.push(LineEdit {
                    session: row.get(0)?,
                    seq: row.get(1)?,
                    changes,
                });
            }
        }
    }
    let mut report = ResultPrune::default();
    let mut size = connection
        .prepare("SELECT length(payload) FROM events WHERE session_id = ?1 AND seq = ?2")?;
    let mut update = connection.prepare(
        "UPDATE events SET payload = json_set(payload, ?3, json(?4))
         WHERE session_id = ?1 AND seq = ?2",
    )?;
    for LineEdit {
        session,
        seq,
        changes,
    } in edits
    {
        report.lines += 1;
        report.bytes_before += size.query_row(params![session, seq], |row| row.get::<_, u64>(0))?;
        for (path, replacement) in changes {
            update.execute(params![session, seq, path, replacement.to_string()])?;
            report.results += 1;
        }
        report.bytes_after += size.query_row(params![session, seq], |row| row.get::<_, u64>(0))?;
    }
    Ok(report)
}

/// Rows per event type: `line` rows by CLI line type, stream lines also by
/// event and block or delta type, `host` rows by event name.
pub fn event_counts(connection: &Connection) -> Result<BTreeMap<String, u64>> {
    let mut statement = connection.prepare(
        "SELECT kind || '/' || COALESCE(CASE kind
                 WHEN 'line' THEN json_extract(payload, '$.type')
                     || COALESCE('/' || json_extract(payload, '$.event.type'), '')
                     || COALESCE('/' || json_extract(payload, '$.event.content_block.type'),
                         '/' || json_extract(payload, '$.event.delta.type'), '')
                 ELSE json_extract(payload, '$.event') END, '?'),
             count(*)
         FROM events GROUP BY 1",
    )?;
    let counts = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::store::migrate;

    const SESSION: &str = "s";

    fn connection(lines: &[Value]) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO sessions (id, cwd, created_at, updated_at, state)
                 VALUES (?1, 'c', 't', 't', 'stopped')",
                [SESSION],
            )
            .unwrap();
        for (index, line) in lines.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO events (session_id, seq, time, kind, payload)
                     VALUES (?1, ?2, 't', 'line', ?3)",
                    params![SESSION, index + 1, line.to_string()],
                )
                .unwrap();
        }
        connection
    }

    fn stream(event: Value) -> Value {
        json!({"type": "stream_event", "event": event, "parent_tool_use_id": null,
            "session_id": "x", "uuid": "u"})
    }

    fn start(id: &str) -> Value {
        stream(json!({"type": "message_start", "message": {"id": id, "content": []}}))
    }

    fn block_start(index: u32, block: Value) -> Value {
        stream(json!({"type": "content_block_start", "index": index, "content_block": block}))
    }

    fn text_start(index: u32) -> Value {
        block_start(index, json!({"type": "text", "text": ""}))
    }

    fn delta(index: u32, delta: Value) -> Value {
        stream(json!({"type": "content_block_delta", "index": index, "delta": delta}))
    }

    fn text(index: u32, text: &str) -> Value {
        delta(index, json!({"type": "text_delta", "text": text}))
    }

    fn block_stop(index: u32) -> Value {
        stream(json!({"type": "content_block_stop", "index": index}))
    }

    fn message_delta() -> Value {
        stream(
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 5}}),
        )
    }

    fn stop() -> Value {
        stream(json!({"type": "message_stop"}))
    }

    fn assistant(id: &str, block: Value) -> Value {
        json!({"type": "assistant", "message": {"id": id, "content": [block]},
            "parent_tool_use_id": null, "session_id": "x", "uuid": "u"})
    }

    fn said(id: &str, text: &str) -> Value {
        assistant(id, json!({"type": "text", "text": text}))
    }

    fn seqs(connection: &Connection) -> Vec<u64> {
        connection
            .prepare("SELECT seq FROM events ORDER BY seq")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn block_lines_go_and_message_lines_stay() {
        let connection = connection(&[
            start("m1"),
            block_start(
                0,
                json!({"type": "thinking", "thinking": "", "signature": ""}),
            ),
            delta(
                0,
                json!({"type": "thinking_delta", "thinking": "hmm", "estimated_tokens": 2}),
            ),
            delta(0, json!({"type": "signature_delta", "signature": "sig"})),
            block_stop(0),
            assistant(
                "m1",
                json!({"type": "thinking", "thinking": "hmm", "signature": "sig"}),
            ),
            text_start(1),
            text(1, "Hel"),
            text(1, "lo"),
            block_stop(1),
            said("m1", "Hello"),
            block_start(
                2,
                json!({"type": "tool_use", "id": "t1", "name": "Edit", "input": {}}),
            ),
            delta(
                2,
                json!({"type": "input_json_delta", "partial_json": "{\"a\": 1}"}),
            ),
            block_stop(2),
            // The CLI fills in a default the model did not send.
            assistant(
                "m1",
                json!({"type": "tool_use", "id": "t1", "name": "Edit",
                    "input": {"a": 1, "replace_all": false}}),
            ),
            message_delta(),
            stop(),
        ]);
        let dry = prune_streams(&connection, SESSION, Mode::DryRun).unwrap();
        assert_eq!((dry.messages, dry.rows), (1, 11));
        assert_eq!(seqs(&connection).len(), 17);

        let report = prune_streams(&connection, SESSION, Mode::Delete).unwrap();
        assert_eq!(report, dry);
        assert_eq!(seqs(&connection), vec![1, 6, 11, 15, 16, 17]);
        let again = prune_streams(&connection, SESSION, Mode::DryRun).unwrap();
        assert_eq!(again, StreamPrune::default());
    }

    #[test]
    fn unverified_messages_keep_every_line() {
        let connection = connection(&[
            // Interrupted: stopped, but no assistant line.
            start("m1"),
            text_start(0),
            text(0, "partial"),
            stop(),
            // Text differs from its assistant line.
            start("m2"),
            text_start(0),
            text(0, "one"),
            said("m2", "two"),
            stop(),
            // A delta of a type the pruning does not know.
            start("m3"),
            text_start(0),
            delta(0, json!({"type": "citations_delta"})),
            said("m3", ""),
            stop(),
            // A block the assistant lines do not carry.
            start("m4"),
            text_start(0),
            text(0, "x"),
            block_start(
                1,
                json!({"type": "tool_use", "id": "t", "name": "n", "input": {}}),
            ),
            said("m4", "x"),
            stop(),
            // Still streaming.
            start("m5"),
            text_start(0),
            text(0, "live"),
        ]);
        let report = prune_streams(&connection, SESSION, Mode::Delete).unwrap();
        assert_eq!(report.messages, 0);
        let kept: Vec<(&str, usize)> = report
            .kept
            .iter()
            .map(|(reason, count)| (reason.as_str(), *count))
            .collect();
        assert_eq!(
            kept,
            vec![
                ("2 blocks streamed, 1 answered", 1),
                ("delta of an unknown type", 1),
                ("no assistant line", 1),
                ("text differs", 1),
                ("unfinished", 1),
            ]
        );
        assert_eq!(
            report.examples["text differs"],
            (SESSION.into(), "m2".into())
        );
        assert_eq!(seqs(&connection).len(), 23);
    }

    #[test]
    fn old_big_tool_outputs_become_markers_and_edits_keep_their_patch() {
        let big = "x".repeat(3000);
        let patch = json!([{"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1,
            "lines": ["-a", "+b"]}]);
        let result_line = |id: &str, content: Value, tool_use_result: Value| {
            // Built by hand to keep the CLI's key order, "type" first.
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"{id}","type":"tool_result","content":{content}}}]}},"parent_tool_use_id":null,"session_id":"x","uuid":"u","tool_use_result":{tool_use_result}}}"#
            )
        };
        let lines = [
            (
                "2026-09-01T00:00:00.000000Z",
                result_line("t1", json!(big), json!({"stdout": big})),
            ),
            (
                "2026-09-01T00:00:01.000000Z",
                result_line(
                    "t2",
                    json!("The file was updated."),
                    json!({"filePath": "a.rs", "oldString": "a", "originalFile": big,
                        "structuredPatch": patch}),
                ),
            ),
            (
                "2026-09-01T00:00:02.000000Z",
                result_line("t3", json!("ok"), json!({"stdout": "ok"})),
            ),
            (
                "2026-10-02T00:00:00.000000Z",
                result_line("t4", json!(big), json!({"stdout": big})),
            ),
        ];
        let connection = connection(&[]);
        for (seq, (time, payload)) in lines.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO events (session_id, seq, time, kind, payload)
                     VALUES (?1, ?2, ?3, 'line', ?4)",
                    params![SESSION, seq + 1, time, payload],
                )
                .unwrap();
        }
        let cutoff = "2026-10-01T00:00:00Z".parse().unwrap();
        let report = prune_tool_results(&connection, cutoff).unwrap();
        assert_eq!((report.lines, report.results), (2, 3));
        assert!(report.bytes_after < report.bytes_before / 4, "{report:?}");

        let payload = |seq: u64| -> String {
            connection
                .query_row("SELECT payload FROM events WHERE seq = ?1", [seq], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        let parsed = |seq: u64| serde_json::from_str::<Value>(&payload(seq)).unwrap();
        assert!(
            payload(1).starts_with(r#"{"type":"user","message":"#),
            "{}",
            payload(1)
        );
        assert_eq!(
            parsed(1)["message"]["content"][0]["content"],
            "[output pruned, 3 KB]"
        );
        assert_eq!(parsed(1)["message"]["content"][0]["tool_use_id"], "t1");
        assert_eq!(parsed(1)["tool_use_result"], "[output pruned, 3 KB]");
        assert_eq!(
            parsed(2)["tool_use_result"],
            json!({"filePath": "a.rs", "structuredPatch": patch})
        );
        assert_eq!(
            parsed(2)["message"]["content"][0]["content"],
            "The file was updated."
        );
        assert_eq!(payload(3), lines[2].1);
        assert_eq!(payload(4), lines[3].1);

        let again = prune_tool_results(&connection, cutoff).unwrap();
        assert_eq!(again, ResultPrune::default());
    }

    #[test]
    fn agent_and_main_streams_interleave_without_mixing() {
        let in_agent = |mut line: Value| {
            line["parent_tool_use_id"] = json!("t1");
            line
        };
        let connection = connection(&[
            start("m1"),
            in_agent(start("a1")),
            text_start(0),
            in_agent(text_start(0)),
            text(0, "main"),
            in_agent(text(0, "agent")),
            in_agent(stop()),
            assistant("a1", json!({"type": "text", "text": "agent"})),
            said("m1", "main"),
            stop(),
        ]);
        let report = prune_streams(&connection, SESSION, Mode::Delete).unwrap();
        assert_eq!((report.messages, report.rows), (2, 4));
        assert_eq!(seqs(&connection), vec![1, 2, 7, 8, 9, 10]);
    }
}
