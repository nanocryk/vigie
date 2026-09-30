//! vigie2's MCP server, hosted in each session actor and reached through `mcp_message`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};

use crate::store::{BashAsk, Store};

pub const SERVER_NAME: &str = "vigie2";
/// The CLI's name for `set_attachments`, allowed without asking. The other tools ask.
pub const SET_ATTACHMENTS_TOOL: &str = "mcp__vigie2__set_attachments";

const SET_ATTACHMENTS: &str = "set_attachments";
const BASH_ASKS: &str = "bash_asks";
const BASH_ASKS_CLEAR: &str = "bash_asks_clear";
/// Answered when the client asks for none.
const PROTOCOL_VERSION: &str = "2025-06-18";
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

const SET_ATTACHMENTS_DESCRIPTION: &str = "Attach files to your reply as chips the user can open, under your last message of this turn. Use it when this turn produced a file meant to be viewed rather than read as code: a generated image, a rendered document, an export, a report. Call it before ending the turn, without being asked. Do not attach source or config files you edited: the user reviews those as diffs. Attach them only if the user asks for the file itself. Each call replaces the whole list for the current turn: call again without a file to drop it, or with an empty list to attach nothing. The chips appear when the turn ends.";

const BASH_ASKS_DESCRIPTION: &str = "List the Bash commands the user was asked to approve, across all sessions and projects, to find the ones worth turning into canif scripts. Rows are grouped by session folder; each gives an id, how many times the command was allowed and denied, when it was asked, the command, its last description, and the user's typed deny messages. Identical commands asked in one folder share a row. Filters combine. Results come in pages: the last line names the offset of the next one.";

const BASH_ASKS_CLEAR_DESCRIPTION: &str = "Delete rows from the list `bash_asks` returns, for instance the ones a canif script now covers, so later reviews show only new commands. Session transcripts are not touched. Rows matching every given filter are deleted; at least one filter is required.";

/// What one message asks of the session besides its reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    /// The turn's attachments become these absolute paths.
    SetAttachments(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Handled {
    pub reply: Value,
    pub effect: Effect,
}

impl Handled {
    fn reply(reply: Value) -> Self {
        Self {
            reply,
            effect: Effect::None,
        }
    }
}

/// Answers one JSON-RPC `message`; relative paths resolve from `cwd`.
pub async fn handle(message: &Value, cwd: &Path, store: Option<&Store>) -> Handled {
    let Some(id) = message.get("id").cloned() else {
        // A notification: the CLI still waits for a reply, with an empty result.
        return Handled::reply(json!({"jsonrpc": "2.0", "result": {}, "id": 0}));
    };
    let params = message.get("params").unwrap_or(&Value::Null);
    let success = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
    let failure = |code: i64, text: String| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": text}});
    match message.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let version = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION);
            Handled::reply(success(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
            })))
        }
        Some("ping") => Handled::reply(success(json!({}))),
        Some("tools/list") => Handled::reply(success(json!({"tools": [
            set_attachments_tool(),
            bash_asks_tool(),
            bash_asks_clear_tool(),
        ]}))),
        Some("tools/call") => {
            let arguments = params.get("arguments").unwrap_or(&Value::Null);
            match params.get("name").and_then(Value::as_str) {
                Some(SET_ATTACHMENTS) => match set_attachments(arguments, cwd) {
                    Ok(paths) => Handled {
                        reply: success(tool_result(&attached_text(&paths), false)),
                        effect: Effect::SetAttachments(paths),
                    },
                    Err(text) => Handled::reply(success(tool_result(&text, true))),
                },
                Some(BASH_ASKS) => {
                    Handled::reply(success(outcome(bash_asks(arguments, cwd, store).await)))
                }
                Some(BASH_ASKS_CLEAR) => Handled::reply(success(outcome(
                    bash_asks_clear(arguments, cwd, store).await,
                ))),
                name => Handled::reply(failure(
                    INVALID_PARAMS,
                    format!("unknown tool {}", name.unwrap_or("(none)")),
                )),
            }
        }
        method => Handled::reply(failure(
            METHOD_NOT_FOUND,
            format!("unknown method {}", method.unwrap_or("(none)")),
        )),
    }
}

fn set_attachments_tool() -> Value {
    json!({
        "name": SET_ATTACHMENTS,
        "description": SET_ATTACHMENTS_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Existing files, absolute or relative to the working directory.",
                },
            },
            "required": ["paths"],
        },
        // Loaded up front instead of behind the CLI's tool search, so the tool is used unprompted.
        "_meta": {"anthropic/alwaysLoad": true},
    })
}

fn bash_asks_tool() -> Value {
    json!({
        "name": BASH_ASKS,
        "description": BASH_ASKS_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "since": {
                    "type": "string",
                    "description": "Only rows last asked at or after this date (YYYY-MM-DD, UTC) or RFC 3339 time.",
                },
                "project": {
                    "type": "string",
                    "description": "Only rows asked in this folder or below it, absolute or relative to the working directory.",
                },
                "decision": {
                    "type": "string",
                    "enum": ["allowed", "denied"],
                    "description": "Only rows allowed, or denied, at least once.",
                },
                "offset": {
                    "type": "integer",
                    "description": "Rows to skip, from the previous page's last line. Default 0.",
                },
                "limit": {
                    "type": "integer",
                    "description": "Rows per page. Default 100.",
                },
            },
        },
    })
}

fn bash_asks_clear_tool() -> Value {
    json!({
        "name": BASH_ASKS_CLEAR,
        "description": BASH_ASKS_CLEAR_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": {"type": "integer"},
                    "description": "Only these rows, by the `id` `bash_asks` returned.",
                },
                "project": {
                    "type": "string",
                    "description": "Only rows asked in this folder or below it, absolute or relative to the working directory.",
                },
                "before": {
                    "type": "string",
                    "description": "Only rows last asked before this date (YYYY-MM-DD, UTC) or RFC 3339 time.",
                },
            },
        },
    })
}

fn tool_result(text: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

fn outcome(result: Result<String, String>) -> Value {
    match result {
        Ok(text) => tool_result(&text, false),
        Err(text) => tool_result(&text, true),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Allowed,
    Denied,
}

/// The rows a `bash_asks` or `bash_asks_clear` call selects; an absent filter keeps every row.
#[derive(Debug, Default)]
struct AskFilter {
    ids: Option<Vec<i64>>,
    project: Option<PathBuf>,
    since: Option<DateTime<Utc>>,
    before: Option<DateTime<Utc>>,
    decision: Option<Decision>,
}

impl AskFilter {
    fn is_empty(&self) -> bool {
        self.ids.is_none()
            && self.project.is_none()
            && self.since.is_none()
            && self.before.is_none()
            && self.decision.is_none()
    }

    fn keeps(&self, ask: &BashAsk) -> bool {
        self.ids.as_ref().is_none_or(|ids| ids.contains(&ask.id))
            && self
                .project
                .as_ref()
                .is_none_or(|project| ask.cwd.starts_with(project))
            && self.since.is_none_or(|since| ask.last_at >= since)
            && self.before.is_none_or(|before| ask.last_at < before)
            && self.decision.is_none_or(|decision| match decision {
                Decision::Allowed => ask.allowed > 0,
                Decision::Denied => ask.denied > 0,
            })
    }
}

async fn bash_asks(arguments: &Value, cwd: &Path, store: Option<&Store>) -> Result<String, String> {
    let filter = AskFilter {
        project: project_argument(arguments, cwd)?,
        since: time_argument(arguments, "since")?,
        decision: match string_argument(arguments, "decision")? {
            None => None,
            Some("allowed") => Some(Decision::Allowed),
            Some("denied") => Some(Decision::Denied),
            Some(other) => {
                return Err(format!(
                    "`decision` must be \"allowed\" or \"denied\", not {other:?}."
                ));
            }
        },
        ..AskFilter::default()
    };
    let offset = count_argument(arguments, "offset")?.unwrap_or(0);
    let limit = count_argument(arguments, "limit")?.unwrap_or(DEFAULT_LIMIT);
    let asks = selected(store, &filter).await?;
    if asks.is_empty() {
        return Ok("No asked Bash command matches.".into());
    }
    let page = asks.iter().skip(offset).take(limit);
    let end = offset + page.len();
    if end == offset {
        return Ok(format!(
            "No rows past offset {offset}: {} match.",
            asks.len()
        ));
    }
    let mut lines = vec![format!(
        "Rows {}-{end} of {}, by folder then command. A row: id, allowed/denied, last asked \
         (first..last when on different days), command, `# description`. Typed deny \
         messages follow their row. A newline in a command shows as \\n; commands past \
         {COMMAND_CHARS} characters are cut.",
        offset + 1,
        asks.len(),
    )];
    let mut folder = None;
    for ask in page {
        if folder != Some(&ask.cwd) {
            folder = Some(&ask.cwd);
            lines.push(ask.cwd.display().to_string());
        }
        lines.push(ask_line(ask));
        for denial in &ask.denials {
            lines.push(format!(
                "      denied {}: {}",
                denial.at.format(DAY),
                one_line(&denial.message)
            ));
        }
    }
    if end < asks.len() {
        lines.push(format!("More rows: call again with offset {end}."));
    }
    Ok(lines.join("\n"))
}

const DEFAULT_LIMIT: usize = 100;
/// Longer commands are cut, keeping a page within the CLI's tool result limit.
const COMMAND_CHARS: usize = 400;
const DAY: &str = "%Y-%m-%d";

fn ask_line(ask: &BashAsk) -> String {
    let (first, last) = (ask.first_at.format(DAY), ask.last_at.format(DAY));
    let days = if first.to_string() == last.to_string() {
        last.to_string()
    } else {
        format!("{first}..{last}")
    };
    let mut command = one_line(&ask.command);
    if let Some((cut, _)) = command.char_indices().nth(COMMAND_CHARS) {
        let rest = command[cut..].chars().count();
        command.truncate(cut);
        command.push_str(&format!(" [{rest} more characters]"));
    }
    let description = ask
        .description
        .as_deref()
        .map(|text| format!("  # {}", one_line(text)))
        .unwrap_or_default();
    format!(
        "  {} {}/{} {days} {command}{description}",
        ask.id, ask.allowed, ask.denied
    )
}

fn one_line(text: &str) -> String {
    text.replace("\r\n", "\\n").replace('\n', "\\n")
}

async fn bash_asks_clear(
    arguments: &Value,
    cwd: &Path,
    store: Option<&Store>,
) -> Result<String, String> {
    let ids = match arguments.get("ids") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_array()
                .and_then(|ids| ids.iter().map(Value::as_i64).collect::<Option<Vec<_>>>())
                .ok_or("`ids` must be a list of integers.")?,
        ),
    };
    let filter = AskFilter {
        ids,
        project: project_argument(arguments, cwd)?,
        before: time_argument(arguments, "before")?,
        ..AskFilter::default()
    };
    if filter.is_empty() {
        return Err("Nothing deleted: give `ids`, `project` or `before`.".into());
    }
    let ids = selected(store, &filter)
        .await?
        .into_iter()
        .map(|ask| ask.id)
        .collect();
    let deleted = store
        .ok_or(NO_HISTORY)?
        .delete_bash_asks(ids)
        .await
        .map_err(|err| format!("{err:#}"))?;
    Ok(format!("Deleted {deleted} rows."))
}

const NO_HISTORY: &str = "vigie2 keeps no history for this session.";

async fn selected(store: Option<&Store>, filter: &AskFilter) -> Result<Vec<BashAsk>, String> {
    let asks = store
        .ok_or(NO_HISTORY)?
        .bash_asks()
        .await
        .map_err(|err| format!("{err:#}"))?;
    Ok(asks.into_iter().filter(|ask| filter.keeps(ask)).collect())
}

fn string_argument<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("`{key}` must be a string.")),
    }
}

fn count_argument(arguments: &Value, key: &str) -> Result<Option<usize>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|count| usize::try_from(count).ok())
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a non-negative integer.")),
    }
}

fn project_argument(arguments: &Value, cwd: &Path) -> Result<Option<PathBuf>, String> {
    Ok(string_argument(arguments, "project")?.map(|text| cwd.join(text).components().collect()))
}

/// A `YYYY-MM-DD` date, as midnight UTC, or an RFC 3339 time.
fn time_argument(arguments: &Value, key: &str) -> Result<Option<DateTime<Utc>>, String> {
    let Some(text) = string_argument(arguments, key)? else {
        return Ok(None);
    };
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Ok(Some(date.and_time(Default::default()).and_utc()));
    }
    DateTime::parse_from_rfc3339(text)
        .map(|time| Some(time.with_timezone(&Utc)))
        .map_err(|_| format!("`{key}` must be YYYY-MM-DD or an RFC 3339 time, not {text:?}."))
}

fn attached_text(paths: &[String]) -> String {
    if paths.is_empty() {
        return "No files will be attached to this turn's reply.".into();
    }
    let list: Vec<String> = paths.iter().map(|path| format!("- {path}")).collect();
    format!(
        "These files will be attached to this turn's reply:\n{}",
        list.join("\n")
    )
}

/// The absolute paths of `arguments.paths`, or the error text naming each bad one.
fn set_attachments(arguments: &Value, cwd: &Path) -> Result<Vec<String>, String> {
    let Some(raw) = arguments.get("paths").and_then(Value::as_array) else {
        return Err("`paths` must be a list of file paths.".into());
    };
    let mut paths = Vec::new();
    let mut bad = Vec::new();
    for entry in raw {
        let Some(text) = entry.as_str() else {
            bad.push(format!("- {entry}: not a string"));
            continue;
        };
        let path = cwd.join(text);
        if !path.is_file() {
            let why = if path.exists() {
                "not a file"
            } else {
                "not found"
            };
            bad.push(format!("- {text}: {why}"));
            continue;
        }
        // Rebuilt from its components, so it carries the platform's separators only.
        let absolute = path.components().collect::<PathBuf>().display().to_string();
        if !paths.contains(&absolute) {
            paths.push(absolute);
        }
    }
    if bad.is_empty() {
        Ok(paths)
    } else {
        Err(format!(
            "Nothing changed, the attachments stay as they were. Bad paths:\n{}",
            bad.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::BashAnswer;

    /// `super::handle` with no history, which the tools that need none never reach.
    fn handle(message: &Value, cwd: &Path) -> Handled {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(super::handle(message, cwd, None))
    }

    fn call(arguments: Value) -> Value {
        call_tool(SET_ATTACHMENTS, arguments)
    }

    fn call_tool(name: &str, arguments: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}})
    }

    #[tokio::test]
    async fn bash_asks_lists_and_clears_filtered_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let (vigie, other) = (dir.path().join("vigie"), dir.path().join("other"));
        let old = "2026-09-01T10:00:00Z".parse().unwrap();
        let new = "2026-09-20T10:00:00Z".parse().unwrap();
        store.record_bash_ask("cargo test".into(), &vigie, None, BashAnswer::Allowed, old);
        store.record_bash_ask(
            "rm -rf x".into(),
            &vigie,
            Some("Clean".into()),
            BashAnswer::Denied {
                message: "never".into(),
            },
            new,
        );
        store.record_bash_ask("ls".into(), &other, None, BashAnswer::Allowed, new);
        let run = async |name: &str, arguments: Value| {
            let handled = super::handle(&call_tool(name, arguments), &vigie, Some(&store)).await;
            let result = &handled.reply["result"];
            (
                result["content"][0]["text"].as_str().unwrap().to_owned(),
                result["isError"].as_bool().unwrap(),
            )
        };

        let body = |text: &str| text.lines().skip(1).collect::<Vec<_>>().join("\n");
        let (all, failed) = run(BASH_ASKS, json!({})).await;
        assert!(!failed, "{all}");
        assert!(all.starts_with("Rows 1-3 of 3,"), "{all}");
        assert_eq!(
            body(&all),
            format!(
                "{}\n  3 1/0 2026-09-20 ls\n{}\n  1 1/0 2026-09-01 cargo test\n  \
                 2 0/1 2026-09-20 rm -rf x  # Clean\n      denied 2026-09-20: never",
                other.display(),
                vigie.display()
            )
        );
        let (page, _) = run(BASH_ASKS, json!({"offset": 1, "limit": 1})).await;
        assert!(page.starts_with("Rows 2-2 of 3,"), "{page}");
        assert_eq!(
            body(&page),
            format!(
                "{}\n  1 1/0 2026-09-01 cargo test\nMore rows: call again with offset 2.",
                vigie.display()
            )
        );
        let (text, failed) = run(BASH_ASKS, json!({"offset": 3})).await;
        assert!(
            !failed && text.starts_with("No rows past offset 3"),
            "{text}"
        );

        let (text, _) = run(BASH_ASKS, json!({"project": ".", "since": "2026-09-10"})).await;
        assert!(
            text.contains("rm -rf x") && !text.contains("cargo test"),
            "{text}"
        );
        let (text, _) = run(BASH_ASKS, json!({"decision": "denied"})).await;
        assert!(text.contains("rm -rf x") && !text.contains(" ls"), "{text}");
        for bad in [
            json!({"decision": "maybe"}),
            json!({"since": "yesterday"}),
            json!({"limit": -1}),
        ] {
            let (text, failed) = run(BASH_ASKS, bad).await;
            assert!(failed, "{text}");
        }

        let (text, failed) = run(BASH_ASKS_CLEAR, json!({})).await;
        assert!(failed, "{text}");
        let (text, _) = run(
            BASH_ASKS_CLEAR,
            json!({"project": ".", "before": "2026-09-10T00:00:00+00:00"}),
        )
        .await;
        assert_eq!(text, "Deleted 1 rows.");
        let (text, _) = run(BASH_ASKS_CLEAR, json!({"ids": [2, 999]})).await;
        assert_eq!(text, "Deleted 1 rows.");
        let (text, _) = run(BASH_ASKS, json!({})).await;
        assert_eq!(
            body(&text),
            format!("{}\n  3 1/0 2026-09-20 ls", other.display())
        );

        let no_history = super::handle(&call_tool(BASH_ASKS, json!({})), &vigie, None).await;
        assert_eq!(no_history.reply["result"]["isError"], true);
        assert_eq!(no_history.reply["result"]["content"][0]["text"], NO_HISTORY);
    }

    #[test]
    fn a_long_command_shows_on_one_cut_line() {
        let command = format!("python -c \"\nprint(1)\r\n{}\"", "x".repeat(COMMAND_CHARS));
        let ask = BashAsk {
            id: 4,
            command,
            cwd: PathBuf::from("C:\\p"),
            allowed: 2,
            denied: 0,
            first_at: "2026-09-01T23:00:00Z".parse().unwrap(),
            last_at: "2026-09-03T01:00:00Z".parse().unwrap(),
            description: Some("Run\nit".into()),
            denials: Vec::new(),
        };
        let line = ask_line(&ask);
        // 23 characters before the x's once newlines are escaped; the rest is 23 x's and a quote.
        let shown = format!(
            "python -c \"\\nprint(1)\\n{}",
            "x".repeat(COMMAND_CHARS - 23)
        );
        assert_eq!(
            line,
            format!("  4 2/0 2026-09-01..2026-09-03 {shown} [24 more characters]  # Run\\nit")
        );
    }

    fn text(handled: &Handled) -> &str {
        handled.reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
    }

    #[test]
    fn the_handshake_lists_the_tool() {
        let cwd = Path::new(".");
        let init = handle(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2025-03-26"}}),
            cwd,
        );
        assert_eq!(init.reply["id"], 1);
        assert_eq!(init.reply["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init.reply["result"]["serverInfo"]["name"], SERVER_NAME);

        let note = handle(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            cwd,
        );
        assert_eq!(note.reply, json!({"jsonrpc": "2.0", "result": {}, "id": 0}));

        let list = handle(
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            cwd,
        );
        assert_eq!(list.reply["result"]["tools"][0]["name"], SET_ATTACHMENTS);
        assert_eq!(
            list.reply["result"]["tools"][0]["_meta"]["anthropic/alwaysLoad"],
            true
        );
        assert_eq!(
            SET_ATTACHMENTS_TOOL,
            format!("mcp__{SERVER_NAME}__{SET_ATTACHMENTS}")
        );
    }

    #[test]
    fn set_attachments_resolves_relative_paths_and_drops_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("shot.png");
        std::fs::write(&file, b"png").unwrap();
        let absolute = file.display().to_string();

        let handled = handle(&call(json!({"paths": ["shot.png", absolute]})), dir.path());
        assert_eq!(
            handled.effect,
            Effect::SetAttachments(vec![absolute.clone()])
        );
        assert_eq!(handled.reply["id"], 7);
        assert_eq!(handled.reply["result"]["isError"], false);
        assert!(text(&handled).contains(&absolute));

        let cleared = handle(&call(json!({"paths": []})), dir.path());
        assert_eq!(cleared.effect, Effect::SetAttachments(Vec::new()));
    }

    #[test]
    fn set_attachments_writes_paths_with_the_platform_separators() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("notes")).unwrap();
        let file = dir.path().join("notes").join("wire.md");
        std::fs::write(&file, b"md").unwrap();

        let handled = handle(&call(json!({"paths": ["./notes/wire.md"]})), dir.path());
        assert_eq!(
            handled.effect,
            Effect::SetAttachments(vec![file.display().to_string()])
        );
    }

    #[test]
    fn a_bad_path_fails_the_call_without_changing_the_list() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.txt"), b"ok").unwrap();
        std::fs::create_dir(dir.path().join("folder")).unwrap();

        let handled = handle(
            &call(json!({"paths": ["ok.txt", "missing.txt", "folder", 3]})),
            dir.path(),
        );
        assert_eq!(handled.effect, Effect::None);
        assert_eq!(handled.reply["result"]["isError"], true);
        let text = text(&handled);
        assert!(text.contains("- missing.txt: not found"), "{text}");
        assert!(text.contains("- folder: not a file"), "{text}");
        assert!(text.contains("- 3: not a string"), "{text}");
        assert!(!text.contains("ok.txt"), "{text}");

        let no_list = handle(&call(json!({})), dir.path());
        assert_eq!(no_list.effect, Effect::None);
        assert_eq!(no_list.reply["result"]["isError"], true);
    }

    #[test]
    fn unknown_methods_and_tools_are_errors() {
        let cwd = Path::new(".");
        let method = handle(
            &json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list"}),
            cwd,
        );
        assert_eq!(method.reply["error"]["code"], METHOD_NOT_FOUND);
        let tool = handle(
            &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": {"name": "other", "arguments": {}}}),
            cwd,
        );
        assert_eq!(tool.reply["error"]["code"], INVALID_PARAMS);
        assert_eq!(tool.effect, Effect::None);
    }
}
