//! vigie's MCP server, hosted in each session actor and reached through `mcp_message`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use regex::Regex;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::paths::is_network_path;
use crate::session::server_restart::ServerRestart;
use crate::store::{
    BashAsk, BashAskFilter, MessageQuery, SessionRow, Store, StoredMessage, snippet,
};

pub const SERVER_NAME: &str = "vigie";
/// The CLI's name for `set_attachments`, allowed without asking. The other tools ask.
pub const SET_ATTACHMENTS_TOOL: &str = "mcp__vigie__set_attachments";

const SET_ATTACHMENTS: &str = "set_attachments";
const BASH_ASKS: &str = "bash_asks";
const BASH_ASKS_CLEAR: &str = "bash_asks_clear";
const BASH_ASKS_FILTER: &str = "bash_asks_filter";
const RESTART_SERVER: &str = "restart_server";
const HISTORY_SEARCH: &str = "history_search";
const HISTORY_READ: &str = "history_read";
/// Answered when the client asks for none.
const PROTOCOL_VERSION: &str = "2025-06-18";
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

const SET_ATTACHMENTS_DESCRIPTION: &str = "Attach files to your reply as chips the user can open, under your last message of this turn. Use it when this turn produced a file meant to be viewed rather than read as code: a generated image, a rendered document, an export, a report. Call it before ending the turn, without being asked. Do not attach source or config files you edited: the user reviews those as diffs. Attach them only if the user asks for the file itself. Each call replaces the whole list for the current turn: call again without a file to drop it, or with an empty list to attach nothing. The chips appear when the turn ends.";

const BASH_ASKS_DESCRIPTION: &str = "List the Bash commands the user was asked to approve, across all sessions and projects, to find the ones worth turning into canif scripts. Rows are grouped by session folder; each gives an id, how many times the command was allowed and denied, when it was asked, the command, its last description, and the user's typed deny messages. Identical commands asked in one folder share a row. Filters combine. Rows matching a `bash_asks_filter` rule are hidden unless `filtered` asks for them. Results come in pages: the last line names the offset of the next one.";

const BASH_ASKS_FILTER_DESCRIPTION: &str = "Manage the rules hiding rows from `bash_asks`: commands that are not canif script candidates, such as ones the user approves one by one on purpose (`git add <paths>`) or one-off exploration. A rule is a regex matched anywhere in the command (anchor it with `^`), for one project folder and below it or for all projects, with a reason saying what it hides and why. Hidden rows are kept: removing a rule shows them again. `list` gives each rule and how many rows it matches.";

const BASH_ASKS_CLEAR_DESCRIPTION: &str = "Delete rows from the list `bash_asks` returns, for instance the ones a canif script now covers, so later reviews show only new commands. Session transcripts are not touched. Rows matching every given filter are deleted; at least one filter is required.";

const RESTART_SERVER_DESCRIPTION: &str = "Restart the vigie server on the last built binary, for instance after `server/build` built a server change. The restart waits until no turn runs in any session, this one included, so end the turn after calling it. Messages the user sends meanwhile are held and show in their discussion. Only works when the server was started by `serve.sh`. It never builds.";

const HISTORY_SEARCH_DESCRIPTION: &str = "Search earlier vigie discussions for what was said: decisions, past fixes, the user's wording. Only message text is searched: the user's messages and Claude's text replies, never tool calls or their output. A message matches when it contains every word of `query`, ignoring case; no regex. Searches this session's folder and below it unless `project` or `all_projects` says otherwise. Hits come newest first, grouped by session, each with its seq and a snippet; `history_read` gives the exchanges around one. Results come in pages: the last line names the offset of the next one.";

const HISTORY_READ_DESCRIPTION: &str = "Read the exchanges around a message `history_search` found: the exchange holding it (a user message and the replies until the next one), with `before` exchanges before it and `after` exchanges after it. The hit is marked and cut past 2000 characters, the other messages past 600. A cut message ends with the seq and `from` that read on: with `from`, only that message is returned, from that character on.";

const RESTART_SCHEDULED: &str =
    "Restart scheduled: it happens once no turn runs in any session, so end this turn now.";

const NO_RESTART: &str = "This session cannot restart the server.";

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
pub async fn handle(
    message: &Value,
    cwd: &Path,
    store: Option<&Store>,
    restart: Option<&ServerRestart>,
) -> Handled {
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
            bash_asks_filter_tool(),
            restart_server_tool(),
            history_search_tool(),
            history_read_tool(),
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
                Some(BASH_ASKS_FILTER) => Handled::reply(success(outcome(
                    bash_asks_filter(arguments, cwd, store).await,
                ))),
                Some(RESTART_SERVER) => Handled::reply(success(outcome(restart_server(restart)))),
                Some(HISTORY_SEARCH) => Handled::reply(success(outcome(
                    history_search(arguments, cwd, store).await,
                ))),
                Some(HISTORY_READ) => {
                    Handled::reply(success(outcome(history_read(arguments, store).await)))
                }
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
                "filtered": {
                    "type": "boolean",
                    "description": "List only the rows the filter rules hide, each with the id of the rule hiding it.",
                },
            },
        },
    })
}

fn bash_asks_filter_tool() -> Value {
    json!({
        "name": BASH_ASKS_FILTER,
        "description": BASH_ASKS_FILTER_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "add", "remove"],
                    "description": "Default `list`.",
                },
                "pattern": {
                    "type": "string",
                    "description": "For `add`: the regex (Rust `regex` syntax) matched anywhere in the command.",
                },
                "project": {
                    "type": "string",
                    "description": "For `add`: the folder the rule applies in, and below it, absolute or relative to the working directory. Absent: all projects.",
                },
                "reason": {
                    "type": "string",
                    "description": "For `add`: what the rule hides and why, in one sentence.",
                },
                "ids": {
                    "type": "array",
                    "items": {"type": "integer"},
                    "description": "For `remove`: the rules, by the id `list` gives.",
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

fn restart_server_tool() -> Value {
    json!({
        "name": RESTART_SERVER,
        "description": RESTART_SERVER_DESCRIPTION,
        "inputSchema": {"type": "object", "properties": {}},
    })
}

fn restart_server(restart: Option<&ServerRestart>) -> Result<String, String> {
    let restart = restart.ok_or_else(|| NO_RESTART.to_string())?;
    restart.when_idle().map_err(|err| format!("{err:#}"))?;
    Ok(RESTART_SCHEDULED.to_string())
}

fn history_search_tool() -> Value {
    json!({
        "name": HISTORY_SEARCH,
        "description": HISTORY_SEARCH_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Words a message must all contain, in any order, ignoring case.",
                },
                "project": {
                    "type": "string",
                    "description": "Search the discussions started in this folder or below it instead, absolute or relative to the working directory.",
                },
                "all_projects": {
                    "type": "boolean",
                    "description": "Search the discussions of every project.",
                },
                "since": {
                    "type": "string",
                    "description": "Only messages at or after this date (YYYY-MM-DD, UTC) or RFC 3339 time.",
                },
                "before": {
                    "type": "string",
                    "description": "Only messages before this date (YYYY-MM-DD, UTC) or RFC 3339 time.",
                },
                "offset": {
                    "type": "integer",
                    "description": "Hits to skip, from the previous page's last line. Default 0.",
                },
                "limit": {
                    "type": "integer",
                    "description": "Hits per page. Default 20.",
                },
            },
            "required": ["query"],
        },
    })
}

fn history_read_tool() -> Value {
    json!({
        "name": HISTORY_READ,
        "description": HISTORY_READ_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "session": {
                    "type": "string",
                    "description": "The session id `history_search` gave.",
                },
                "seq": {
                    "type": "integer",
                    "description": "The hit's seq.",
                },
                "before": {
                    "type": "integer",
                    "description": "Exchanges before the hit's one. Default 1, at most 5.",
                },
                "after": {
                    "type": "integer",
                    "description": "Exchanges after the hit's one. Default 1, at most 5.",
                },
                "from": {
                    "type": "integer",
                    "description": "Read only the hit message, from this character on, as a cut marker names. `before` and `after` are ignored.",
                },
            },
            "required": ["session", "seq"],
        },
    })
}

const SEARCH_LIMIT: usize = 20;
const READ_EXCHANGES: u32 = 1;
const READ_MAX_EXCHANGES: u32 = 5;
const READ_CHARS: usize = 2000;
/// Messages around the hit only give context, so they are cut sooner.
const CONTEXT_CHARS: usize = 600;

async fn history_search(
    arguments: &Value,
    cwd: &Path,
    store: Option<&Store>,
) -> Result<String, String> {
    let history = store.ok_or(NO_HISTORY)?;
    let query = string_argument(arguments, "query")?.ok_or("`query` is required.")?;
    let words: Vec<String> = query.split_whitespace().map(str::to_owned).collect();
    if words.is_empty() {
        return Err("`query` has no word.".into());
    }
    let query = words.join(" ");
    let folder = match (
        bool_argument(arguments, "all_projects")?,
        project_argument(arguments, cwd)?,
    ) {
        (true, Some(_)) => return Err("Give `project` or `all_projects`, not both.".into()),
        (true, None) => None,
        (false, project) => Some(project.unwrap_or_else(|| cwd.to_path_buf())),
    };
    let scope = folder
        .as_ref()
        .map_or("all projects".into(), |folder| folder.display().to_string());
    let sessions: HashMap<Uuid, SessionRow> = history
        .sessions()
        .await
        .map_err(|err| format!("{err:#}"))?
        .into_iter()
        .filter(|row| {
            folder
                .as_ref()
                .is_none_or(|folder| row.cwd.starts_with(folder))
        })
        .map(|row| (row.id, row))
        .collect();
    let message_query = MessageQuery {
        sessions: sessions.keys().copied().collect(),
        words: words.clone(),
        since: time_argument(arguments, "since")?,
        before: time_argument(arguments, "before")?,
    };
    let offset = count_argument(arguments, "offset")?.unwrap_or(0);
    let limit = count_argument(arguments, "limit")?.unwrap_or(SEARCH_LIMIT);
    let (total, hits) = history
        .search_messages(message_query, offset, limit)
        .await
        .map_err(|err| format!("{err:#}"))?;
    if total == 0 {
        return Ok(format!("No message matches {query:?} in {scope}."));
    }
    if hits.is_empty() {
        return Ok(format!("No hits past offset {offset}: {total} match."));
    }
    let end = offset + hits.len();
    let mut groups: Vec<(Uuid, Vec<&StoredMessage>)> = Vec::new();
    for hit in &hits {
        match groups
            .iter_mut()
            .find(|(session, _)| *session == hit.session)
        {
            Some((_, group)) => group.push(hit),
            None => groups.push((hit.session, vec![hit])),
        }
    }
    let mut lines = vec![format!(
        "Hits {}-{end} of {total} for {query:?} in {scope}, newest first, grouped by session. \
         A hit: seq, date, who spoke, snippet. `history_read` gives the exchanges around one.",
        offset + 1
    )];
    for (session, group) in groups {
        if let Some(row) = sessions.get(&session) {
            lines.push(session_line(row));
        }
        for hit in group {
            lines.push(format!(
                "  {} {} {}: {}",
                hit.seq,
                hit.time.format(DAY),
                hit.role.as_str(),
                snippet(&hit.text, &words)
            ));
        }
    }
    if end < total {
        lines.push(format!("More hits: call again with offset {end}."));
    }
    Ok(lines.join("\n"))
}

async fn history_read(arguments: &Value, store: Option<&Store>) -> Result<String, String> {
    let history = store.ok_or(NO_HISTORY)?;
    let session: Uuid = string_argument(arguments, "session")?
        .ok_or("`session` is required.")?
        .parse()
        .map_err(|_| "`session` must be a session id `history_search` gave.")?;
    let seq = count_argument(arguments, "seq")?.ok_or("`seq` is required.")? as u64;
    let from = count_argument(arguments, "from")?;
    let (before, after) = match from {
        Some(_) => (0, 0),
        None => (
            exchanges_argument(arguments, "before")?,
            exchanges_argument(arguments, "after")?,
        ),
    };
    let row = history
        .session(session)
        .await
        .map_err(|err| format!("{err:#}"))?
        .ok_or_else(|| format!("No session {session}."))?;
    let messages = history
        .messages_around(session, seq, before, after)
        .await
        .map_err(|err| format!("{err:#}"))?;
    let Some(hit) = messages.iter().find(|message| message.seq == seq) else {
        return Err(format!("Session {session} has no message {seq}."));
    };
    if let Some(from) = from {
        let Some((start, _)) = hit.text.char_indices().nth(from) else {
            return Err(format!(
                "Message {seq} has only {} characters.",
                hit.text.chars().count()
            ));
        };
        return Ok(format!(
            "{}\n--- {} {} {}, from character {from}\n{}",
            session_line(&row),
            hit.seq,
            hit.time.format("%Y-%m-%d %H:%M"),
            hit.role.as_str(),
            cut_message(&hit.text[start..], hit.seq, from, READ_CHARS)
        ));
    }
    let mut text = format!(
        "{}\n{} messages, oldest first, the hit marked. A message: seq, time (UTC), who spoke, \
         then its text. The hit is cut past {READ_CHARS} characters, the others past \
         {CONTEXT_CHARS}.",
        session_line(&row),
        messages.len()
    );
    for message in &messages {
        let (mark, limit) = if message.seq == seq {
            (" (hit)", READ_CHARS)
        } else {
            ("", CONTEXT_CHARS)
        };
        text.push_str(&format!(
            "\n\n--- {} {} {}{mark}\n{}",
            message.seq,
            message.time.format("%Y-%m-%d %H:%M"),
            message.role.as_str(),
            cut_message(&message.text, message.seq, 0, limit)
        ));
    }
    Ok(text)
}

/// `session <id> "<name>" <folder>`; the name defaults to the folder's.
fn session_line(row: &SessionRow) -> String {
    let name = row.name.clone().unwrap_or_else(|| {
        row.cwd
            .file_name()
            .map_or(row.cwd.display().to_string(), |name| {
                name.to_string_lossy().into_owned()
            })
    });
    format!("session {} {name:?} {}", row.id, row.cwd.display())
}

/// `text` cut to `limit` characters, followed by how many were cut.
fn cut(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!(
            "{} [{} more characters]",
            &text[..end],
            text[end..].chars().count()
        ),
        None => text.to_string(),
    }
}

/// `text`, which starts at character `from` of message `seq`, cut to `limit`
/// characters, followed by how many were cut and where to read on.
fn cut_message(text: &str, seq: u64, from: usize, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!(
            "{} [{} more characters: seq {seq} from {}]",
            &text[..end],
            text[end..].chars().count(),
            from + limit
        ),
        None => text.to_string(),
    }
}

fn exchanges_argument(arguments: &Value, key: &str) -> Result<u32, String> {
    match count_argument(arguments, key)? {
        None => Ok(READ_EXCHANGES),
        Some(count) => u32::try_from(count)
            .ok()
            .filter(|count| *count <= READ_MAX_EXCHANGES)
            .ok_or_else(|| format!("`{key}` is at most {READ_MAX_EXCHANGES}.")),
    }
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
    let filtered = bool_argument(arguments, "filtered")?;
    let offset = count_argument(arguments, "offset")?.unwrap_or(0);
    let limit = count_argument(arguments, "limit")?.unwrap_or(DEFAULT_LIMIT);
    let rules = rules(store).await?;
    let (hidden, shown): (Vec<_>, Vec<_>) = selected(store, &filter)
        .await?
        .into_iter()
        .map(|ask| (hiding(&rules, &ask), ask))
        .partition(|(rule, _)| rule.is_some());
    let (asks, set) = if filtered {
        (hidden, " hidden by filters".to_string())
    } else {
        let set = match hidden.len() {
            0 => String::new(),
            count => format!(" ({count} more hidden by filters, see `filtered`)"),
        };
        (shown, set)
    };
    if asks.is_empty() {
        return Ok(format!("No asked Bash command matches{set}."));
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
        "Rows {}-{end} of {}{set}, by folder then command. A row: id, allowed/denied, last \
         asked (first..last when on different days), command, `# description`. Typed deny \
         messages follow their row. A newline in a command shows as \\n; commands past \
         {COMMAND_CHARS} characters are cut.",
        offset + 1,
        asks.len(),
    )];
    let mut folder = None;
    for (rule, ask) in page {
        if folder != Some(&ask.cwd) {
            folder = Some(&ask.cwd);
            lines.push(ask.cwd.display().to_string());
        }
        let mut line = ask_line(ask);
        if let Some(rule) = rule {
            line.push_str(&format!("  [filter {rule}]"));
        }
        lines.push(line);
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
    let command = cut(&one_line(&ask.command), COMMAND_CHARS);
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
    let filter = AskFilter {
        ids: ids_argument(arguments)?,
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

const NO_HISTORY: &str = "vigie keeps no history for this session.";

async fn selected(store: Option<&Store>, filter: &AskFilter) -> Result<Vec<BashAsk>, String> {
    let asks = store
        .ok_or(NO_HISTORY)?
        .bash_asks()
        .await
        .map_err(|err| format!("{err:#}"))?;
    Ok(asks.into_iter().filter(|ask| filter.keeps(ask)).collect())
}

/// A `BashAskFilter` ready to match.
struct Rule {
    filter: BashAskFilter,
    regex: Regex,
}

async fn rules(store: Option<&Store>) -> Result<Vec<Rule>, String> {
    let filters = store
        .ok_or(NO_HISTORY)?
        .bash_ask_filters()
        .await
        .map_err(|err| format!("{err:#}"))?;
    filters
        .into_iter()
        .map(|filter| {
            let regex = Regex::new(&filter.pattern)
                .map_err(|err| format!("filter {} does not compile: {err}", filter.id))?;
            Ok(Rule { filter, regex })
        })
        .collect()
}

/// The id of the first rule hiding `ask`.
fn hiding(rules: &[Rule], ask: &BashAsk) -> Option<i64> {
    rules
        .iter()
        .find(|rule| {
            rule.filter
                .project
                .as_ref()
                .is_none_or(|project| ask.cwd.starts_with(project))
                && rule.regex.is_match(&ask.command)
        })
        .map(|rule| rule.filter.id)
}

async fn bash_asks_filter(
    arguments: &Value,
    cwd: &Path,
    store: Option<&Store>,
) -> Result<String, String> {
    let history = store.ok_or(NO_HISTORY)?;
    match string_argument(arguments, "action")? {
        Some("add") => {
            let pattern = string_argument(arguments, "pattern")?
                .ok_or("`add` needs a `pattern`.")?
                .to_owned();
            let regex = Regex::new(&pattern).map_err(|err| format!("Nothing added: {err}"))?;
            let reason = string_argument(arguments, "reason")?
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .ok_or("`add` needs a `reason`.")?
                .to_owned();
            let project = project_argument(arguments, cwd)?;
            let filter = history
                .add_bash_ask_filter(pattern, project, reason)
                .await
                .map_err(|err| format!("{err:#}"))?;
            let rule = [Rule { filter, regex }];
            let hides = hidden_counts(&rule, store).await?[0];
            Ok(format!(
                "Added filter {}, hiding {hides} rows.",
                rule[0].filter.id
            ))
        }
        Some("remove") => {
            let ids = ids_argument(arguments)?.ok_or("`remove` needs `ids`.")?;
            let deleted = history
                .delete_bash_ask_filters(ids)
                .await
                .map_err(|err| format!("{err:#}"))?;
            Ok(format!("Removed {deleted} filters."))
        }
        None | Some("list") => {
            let rules = rules(store).await?;
            if rules.is_empty() {
                return Ok("No filters.".into());
            }
            let counts = hidden_counts(&rules, store).await?;
            let lines: Vec<String> = rules
                .iter()
                .zip(counts)
                .map(|(rule, hides)| {
                    let filter = &rule.filter;
                    let scope = filter
                        .project
                        .as_ref()
                        .map_or("all projects".into(), |project| {
                            project.display().to_string()
                        });
                    format!(
                        "{} /{}/ in {scope}, hiding {hides} rows  # {}",
                        filter.id,
                        filter.pattern,
                        one_line(&filter.reason)
                    )
                })
                .collect();
            Ok(lines.join("\n"))
        }
        Some(other) => Err(format!(
            "`action` must be \"add\", \"remove\" or \"list\", not {other:?}."
        )),
    }
}

/// How many rows each rule matches, rows another rule hides included.
async fn hidden_counts(rules: &[Rule], store: Option<&Store>) -> Result<Vec<usize>, String> {
    let asks = selected(store, &AskFilter::default()).await?;
    Ok(rules
        .iter()
        .map(|rule| {
            asks.iter()
                .filter(|ask| hiding(std::slice::from_ref(rule), ask).is_some())
                .count()
        })
        .collect())
}

fn ids_argument(arguments: &Value) -> Result<Option<Vec<i64>>, String> {
    match arguments.get("ids") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_array()
            .and_then(|ids| ids.iter().map(Value::as_i64).collect::<Option<Vec<_>>>())
            .map(Some)
            .ok_or_else(|| "`ids` must be a list of integers.".into()),
    }
}

fn string_argument<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("`{key}` must be a string.")),
    }
}

/// Absent means false.
fn bool_argument(arguments: &Value, key: &str) -> Result<bool, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("`{key}` must be a boolean.")),
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
        // Allowed without asking, so a network path must never be touched.
        if is_network_path(&path) {
            bad.push(format!("- {text}: on another machine, not allowed"));
            continue;
        }
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
    use crate::store::{BashAnswer, SNIPPET_CHARS};

    /// `super::handle` with no history, which the tools that need none never reach.
    fn handle(message: &Value, cwd: &Path) -> Handled {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(super::handle(message, cwd, None, None))
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
            let handled =
                super::handle(&call_tool(name, arguments), &vigie, Some(&store), None).await;
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

        let no_history = super::handle(&call_tool(BASH_ASKS, json!({})), &vigie, None, None).await;
        assert_eq!(no_history.reply["result"]["isError"], true);
        assert_eq!(no_history.reply["result"]["content"][0]["text"], NO_HISTORY);
    }

    #[tokio::test]
    async fn filter_rules_hide_rows_until_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let (vigie, other) = (dir.path().join("vigie"), dir.path().join("other"));
        let at = "2026-09-20T10:00:00Z".parse().unwrap();
        for (command, cwd) in [
            ("git add src", &vigie),
            ("git add web", &vigie),
            ("cargo test", &vigie),
            ("git add x", &other),
        ] {
            store.record_bash_ask(command.into(), cwd, None, BashAnswer::Allowed, at);
        }
        let run = async |name: &str, arguments: Value| {
            let handled =
                super::handle(&call_tool(name, arguments), &vigie, Some(&store), None).await;
            let result = &handled.reply["result"];
            (
                result["content"][0]["text"].as_str().unwrap().to_owned(),
                result["isError"].as_bool().unwrap(),
            )
        };

        let (text, _) = run(BASH_ASKS_FILTER, json!({})).await;
        assert_eq!(text, "No filters.");
        for bad in [
            json!({"action": "add", "pattern": "(", "reason": "x"}),
            json!({"action": "add", "pattern": "^git add ", "reason": " "}),
            json!({"action": "drop"}),
        ] {
            let (text, failed) = run(BASH_ASKS_FILTER, bad).await;
            assert!(failed, "{text}");
        }
        let (text, failed) = run(
            BASH_ASKS_FILTER,
            json!({"action": "add", "pattern": "^git add ", "project": ".",
                "reason": "Paths differ per commit."}),
        )
        .await;
        assert!(!failed, "{text}");
        assert_eq!(text, "Added filter 1, hiding 2 rows.");
        let (text, _) = run(BASH_ASKS_FILTER, json!({"action": "list"})).await;
        assert_eq!(
            text,
            format!(
                "1 /^git add / in {}, hiding 2 rows  # Paths differ per commit.",
                vigie.display()
            )
        );

        let (text, _) = run(BASH_ASKS, json!({})).await;
        assert!(
            text.starts_with("Rows 1-2 of 2 (2 more hidden by filters, see `filtered`),"),
            "{text}"
        );
        assert!(
            text.contains("git add x") && !text.contains("git add src"),
            "{text}"
        );
        let (text, _) = run(BASH_ASKS, json!({"filtered": true})).await;
        assert!(
            text.starts_with("Rows 1-2 of 2 hidden by filters,"),
            "{text}"
        );
        assert!(
            text.contains("git add src  [filter 1]") && !text.contains("cargo test"),
            "{text}"
        );

        let (text, _) = run(BASH_ASKS_FILTER, json!({"action": "remove", "ids": [1]})).await;
        assert_eq!(text, "Removed 1 filters.");
        let (text, _) = run(BASH_ASKS, json!({})).await;
        assert!(text.starts_with("Rows 1-4 of 4,"), "{text}");
    }

    #[tokio::test]
    async fn history_tools_search_this_project_and_read_around_a_hit() {
        use crate::session::{Event, EventBody, Line};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let (vigie, other) = (dir.path().join("vigie"), dir.path().join("other"));
        let (here, there) = (Uuid::new_v4(), Uuid::new_v4());
        store.session_started(here, &vigie, Utc::now(), None);
        store.session_started(there, &other, Utc::now(), None);
        let say = |session: Uuid, seq: u64, day: u32, user: bool, text: &str| {
            let raw = if user {
                json!({"type": "user", "message": {"role": "user", "content": text},
                    "parent_tool_use_id": null, "session_id": "s"})
            } else {
                json!({"type": "assistant", "parent_tool_use_id": null, "session_id": "s",
                    "uuid": "u", "message": {"id": "m", "content": [{"type": "text", "text": text}]}})
            }
            .to_string();
            let body = EventBody::Line(Box::new(Line {
                parsed: serde_json::from_str(&raw).ok(),
                raw,
            }));
            let time = format!("2026-09-{day:02}T10:00:00Z").parse().unwrap();
            store.record_event(session, &Event { seq, time, body });
        };
        say(here, 1, 1, true, "Set up prettier");
        say(here, 2, 1, false, "Prettier is set up.");
        say(
            here,
            3,
            2,
            true,
            &format!(
                "{} the prettier config {}",
                "a ".repeat(100),
                "b".repeat(300)
            ),
        );
        say(here, 4, 2, false, &"c".repeat(READ_CHARS + 5));
        say(there, 1, 3, true, "prettier elsewhere");
        let run = async |name: &str, arguments: Value| {
            let handled =
                super::handle(&call_tool(name, arguments), &vigie, Some(&store), None).await;
            let result = &handled.reply["result"];
            (
                result["content"][0]["text"].as_str().unwrap().to_owned(),
                result["isError"].as_bool().unwrap(),
            )
        };

        let (text, failed) = run(HISTORY_SEARCH, json!({"query": " PRETTIER "})).await;
        assert!(!failed, "{text}");
        // The 60 characters before the word are 28 a's and " the ".
        let snippet = format!(
            "…{} the prettier config {}…",
            "a ".repeat(28).trim_end(),
            "b".repeat(SNIPPET_CHARS - 76)
        );
        assert_eq!(
            text.lines().skip(1).collect::<Vec<_>>(),
            [
                format!("session {here} \"vigie\" {}", vigie.display()).as_str(),
                &format!("  3 2026-09-02 user: {snippet}"),
                "  2 2026-09-01 claude: Prettier is set up.",
                "  1 2026-09-01 user: Set up prettier",
            ]
        );
        assert!(text.starts_with("Hits 1-3 of 3 for \"PRETTIER\""), "{text}");
        let (text, _) = run(
            HISTORY_SEARCH,
            json!({"query": "prettier", "all_projects": true, "limit": 1}),
        )
        .await;
        assert!(
            text.starts_with("Hits 1-1 of 4 for \"prettier\" in all projects"),
            "{text}"
        );
        assert!(
            text.contains(&format!("session {there} \"other\"")),
            "{text}"
        );
        assert!(
            text.ends_with("More hits: call again with offset 1."),
            "{text}"
        );
        let (text, _) = run(HISTORY_SEARCH, json!({"query": "elsewhere"})).await;
        assert!(text.starts_with("No message matches"), "{text}");
        for bad in [
            json!({}),
            json!({"query": "  "}),
            json!({"query": "x", "project": ".", "all_projects": true}),
        ] {
            let (text, failed) = run(HISTORY_SEARCH, bad).await;
            assert!(failed, "{text}");
        }

        let (text, failed) = run(
            HISTORY_READ,
            json!({"session": here.to_string(), "seq": 2, "before": 0}),
        )
        .await;
        assert!(!failed, "{text}");
        let headers: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with("--- "))
            .collect();
        assert_eq!(
            headers,
            [
                "--- 1 2026-09-01 10:00 user",
                "--- 2 2026-09-01 10:00 claude (hit)",
                "--- 3 2026-09-02 10:00 user",
                "--- 4 2026-09-02 10:00 claude",
            ]
        );
        assert!(text.ends_with(&format!(
            "{} [1405 more characters: seq 4 from {CONTEXT_CHARS}]",
            "c".repeat(CONTEXT_CHARS)
        )));

        let (text, failed) = run(
            HISTORY_READ,
            json!({"session": here.to_string(), "seq": 4, "from": CONTEXT_CHARS, "before": 9}),
        )
        .await;
        assert!(!failed, "{text}");
        assert!(text.contains("\n--- 4 2026-09-02 10:00 claude, from character 600\n"));
        assert_eq!(
            text.lines().filter(|line| line.starts_with("--- ")).count(),
            1
        );
        assert!(text.ends_with(&format!("\n{}", "c".repeat(1405))));
        let (text, failed) =
            run(HISTORY_READ, json!({"session": here.to_string(), "seq": 4})).await;
        assert!(!failed, "{text}");
        assert!(text.contains(&format!(
            "{} [5 more characters: seq 4 from {READ_CHARS}]",
            "c".repeat(READ_CHARS)
        )));
        for bad in [
            json!({"session": here.to_string(), "seq": 4, "from": 2005}),
            json!({"session": here.to_string(), "seq": 9}),
            json!({"session": "nope", "seq": 1}),
            json!({"session": Uuid::new_v4().to_string(), "seq": 1}),
            json!({"session": here.to_string(), "seq": 1, "after": 6}),
        ] {
            let (text, failed) = run(HISTORY_READ, bad).await;
            assert!(failed, "{text}");
        }
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

    #[cfg(windows)]
    #[test]
    fn a_network_path_is_refused_without_being_touched() {
        let dir = tempfile::tempdir().unwrap();
        let handled = handle(
            &call(json!({"paths": [r"\\attacker.invalid\s\x.png"]})),
            dir.path(),
        );
        assert_eq!(handled.effect, Effect::None);
        assert!(text(&handled).contains("on another machine"));
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

    #[tokio::test]
    async fn restart_server_schedules_a_restart_when_idle() {
        use crate::session::server_restart::{RestartState, Supervision};

        let cwd = Path::new(".");
        let message = call_tool(RESTART_SERVER, json!({}));
        let restart = ServerRestart::new(Supervision::Shim);
        let handled = super::handle(&message, cwd, None, Some(&restart)).await;
        assert_eq!(handled.reply["result"]["isError"], false);
        assert_eq!(restart.state(), RestartState::Pending);

        let alone = ServerRestart::new(Supervision::None);
        let refused = super::handle(&message, cwd, None, Some(&alone)).await;
        assert_eq!(refused.reply["result"]["isError"], true);
        assert_eq!(alone.state(), RestartState::Idle);
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
