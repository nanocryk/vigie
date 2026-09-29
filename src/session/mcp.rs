//! vigie2's MCP server, hosted in each session actor and reached through `mcp_message`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub const SERVER_NAME: &str = "vigie2";
/// The CLI's name for `set_attachments`, allowed without asking.
pub const SET_ATTACHMENTS_TOOL: &str = "mcp__vigie2__set_attachments";

const SET_ATTACHMENTS: &str = "set_attachments";
/// Answered when the client asks for none.
const PROTOCOL_VERSION: &str = "2025-06-18";
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

const SET_ATTACHMENTS_DESCRIPTION: &str = "Attach files to your reply as chips the user can open, under your last message of this turn. Use it when this turn produced a file meant to be viewed rather than read as code: a generated image, a rendered document, an export, a report. Call it before ending the turn, without being asked. Do not attach source or config files you edited: the user reviews those as diffs. Attach them only if the user asks for the file itself. Each call replaces the whole list for the current turn: call again without a file to drop it, or with an empty list to attach nothing. The chips appear when the turn ends.";

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
pub fn handle(message: &Value, cwd: &Path) -> Handled {
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
        Some("tools/list") => Handled::reply(success(json!({"tools": [set_attachments_tool()]}))),
        Some("tools/call") => {
            let name = params.get("name").and_then(Value::as_str);
            if name != Some(SET_ATTACHMENTS) {
                return Handled::reply(failure(
                    INVALID_PARAMS,
                    format!("unknown tool {}", name.unwrap_or("(none)")),
                ));
            }
            match set_attachments(params.get("arguments").unwrap_or(&Value::Null), cwd) {
                Ok(paths) => Handled {
                    reply: success(tool_result(&attached_text(&paths), false)),
                    effect: Effect::SetAttachments(paths),
                },
                Err(text) => Handled::reply(success(tool_result(&text, true))),
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

fn tool_result(text: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
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

    fn call(arguments: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": "set_attachments", "arguments": arguments}})
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
