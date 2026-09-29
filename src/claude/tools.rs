//! The Claude Code tools the host treats specially, and where an ask names its file.

use serde_json::Value;

use crate::claude::protocol::CanUseTool;

pub const BASH: &str = "Bash";
pub const READ: &str = "Read";
pub const EDIT: &str = "Edit";
pub const NOTEBOOK_EDIT: &str = "NotebookEdit";
pub const ASK_USER_QUESTION: &str = "AskUserQuestion";
pub const EXIT_PLAN_MODE: &str = "ExitPlanMode";
pub const SEND_MESSAGE: &str = "SendMessage";
/// Tools starting a subagent.
pub const AGENT_TOOLS: [&str; 2] = ["Agent", "Task"];
/// Tools that change a file; an `Edit` rule covers them all.
pub const EDIT_TOOLS: [&str; 4] = [EDIT, "Write", "MultiEdit", NOTEBOOK_EDIT];

pub fn is_edit(tool_name: &str) -> bool {
    EDIT_TOOLS.contains(&tool_name)
}

/// The file a Read or edit-tool ask targets, as its input names it.
pub fn file_target(ask: &CanUseTool) -> Option<&str> {
    let key = match ask.tool_name.as_str() {
        NOTEBOOK_EDIT => "notebook_path",
        READ => "file_path",
        tool if is_edit(tool) => "file_path",
        _ => return None,
    };
    ask.input.get(key).and_then(Value::as_str)
}
