//! Serde types for the Claude Code stream-json wire.
//!
//! `Inbound` is one line of CLI stdout, `Outbound` one line of CLI stdin. Fields
//! without a typed slot land in each struct's flattened `extra` map, and unknown
//! `type`s, subtypes and blocks land in raw variants, so a line re-serializes
//! to the JSON it came from.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

pub type Extra = Map<String, Value>;

/// The CLI's permission mode. `Other` keeps a mode vigie2 does not know as it
/// was written, so any mode the CLI accepts or reports goes through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    BypassPermissions,
    Other(String),
}

impl PermissionMode {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
            Self::BypassPermissions => "bypassPermissions",
            Self::Other(mode) => mode,
        }
    }

    /// Whether a stopped session accepts the mode; any other would only fail
    /// at the next launch.
    pub fn is_storable(&self) -> bool {
        matches!(self, Self::Default | Self::AcceptEdits | Self::Plan)
    }
}

impl From<String> for PermissionMode {
    fn from(mode: String) -> Self {
        match mode.as_str() {
            "default" => Self::Default,
            "acceptEdits" => Self::AcceptEdits,
            "plan" => Self::Plan,
            "bypassPermissions" => Self::BypassPermissions,
            _ => Self::Other(mode),
        }
    }
}

impl From<&str> for PermissionMode {
    fn from(mode: &str) -> Self {
        Self::from(mode.to_string())
    }
}

impl From<PermissionMode> for String {
    fn from(mode: PermissionMode) -> Self {
        match mode {
            PermissionMode::Other(mode) => mode,
            known => known.as_str().to_string(),
        }
    }
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Inbound {
    System(System),
    StreamEvent(StreamEvent),
    Assistant(AssistantMessage),
    User(UserMessage),
    Result(ResultMessage),
    RateLimitEvent(RateLimitEvent),
    ControlRequest(ControlRequest<CliRequest>),
    ControlResponse(ControlResponse),
    ControlCancelRequest(ControlCancelRequest),
    #[serde(untagged)]
    Unknown(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outbound {
    User(UserMessage),
    ControlRequest(ControlRequest<HostRequest>),
    ControlResponse(ControlResponse),
    #[serde(untagged)]
    Unknown(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum System {
    Init(Init),
    Status(Status),
    HookStarted(HookStarted),
    HookResponse(HookResponse),
    TaskStarted(TaskStarted),
    TaskProgress(TaskProgress),
    TaskUpdated(TaskUpdated),
    TaskNotification(TaskNotification),
    BackgroundTasksChanged(BackgroundTasksChanged),
    ThinkingTokens(ThinkingTokens),
    #[serde(untagged)]
    Other(Extra),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Init {
    pub session_id: String,
    pub uuid: String,
    pub cwd: String,
    pub model: String,
    #[serde(rename = "permissionMode")]
    pub permission_mode: PermissionMode,
    /// `"none"` for claude.ai OAuth; any other value means API key billing.
    #[serde(rename = "apiKeySource")]
    pub api_key_source: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub slash_commands: Vec<String>,
    #[serde(default)]
    pub terminal_slash_commands: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_code_version: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub status: Option<String>,
    #[serde(
        rename = "permissionMode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub permission_mode: Option<PermissionMode>,
    pub session_id: String,
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookStarted {
    pub hook_id: String,
    pub hook_name: String,
    pub hook_event: String,
    pub session_id: String,
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookResponse {
    pub hook_id: String,
    pub hook_name: String,
    pub hook_event: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    pub session_id: String,
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskStarted {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_backgrounded: Option<bool>,
    /// Set on a subagent (`task_type` `local_agent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    /// A subagent's brief.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub session_id: String,
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A running task's progress, such as a subagent's latest tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskProgress {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool_name: Option<String>,
    pub session_id: String,
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskUpdated {
    pub task_id: String,
    pub patch: TaskPatch,
    pub session_id: String,
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<u64>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskNotification {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub session_id: String,
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackgroundTasksChanged {
    pub tasks: Vec<BackgroundTask>,
    pub session_id: String,
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackgroundTask {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingTokens {
    pub estimated_tokens: u64,
    pub estimated_tokens_delta: i64,
    pub session_id: String,
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamEvent {
    pub event: StreamEventBody,
    pub parent_tool_use_id: Option<String>,
    pub session_id: String,
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEventBody {
    MessageStart {
        message: ApiMessage,
        #[serde(flatten)]
        extra: Extra,
    },
    ContentBlockStart {
        index: u32,
        content_block: ContentBlock,
        #[serde(flatten)]
        extra: Extra,
    },
    ContentBlockDelta {
        index: u32,
        delta: Delta,
        #[serde(flatten)]
        extra: Extra,
    },
    ContentBlockStop {
        index: u32,
        #[serde(flatten)]
        extra: Extra,
    },
    MessageDelta {
        delta: MessageDeltaBody,
        #[serde(flatten)]
        extra: Extra,
    },
    MessageStop {
        #[serde(flatten)]
        extra: Extra,
    },
    #[serde(untagged)]
    Other(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    TextDelta {
        text: String,
        #[serde(flatten)]
        extra: Extra,
    },
    ThinkingDelta {
        thinking: String,
        #[serde(flatten)]
        extra: Extra,
    },
    InputJsonDelta {
        partial_json: String,
        #[serde(flatten)]
        extra: Extra,
    },
    SignatureDelta {
        signature: String,
        #[serde(flatten)]
        extra: Extra,
    },
    #[serde(untagged)]
    Other(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageDeltaBody {
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// An Anthropic API message as carried by `assistant` and `message_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(flatten)]
        extra: Extra,
    },
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        #[serde(flatten)]
        extra: Extra,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(flatten)]
        extra: Extra,
    },
    ToolResult {
        tool_use_id: String,
        /// A string or an array of blocks.
        content: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        #[serde(flatten)]
        extra: Extra,
    },
    #[serde(untagged)]
    Other(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub message: ApiMessage,
    pub parent_tool_use_id: Option<String>,
    pub session_id: String,
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A `user` line in either direction: a host prompt, its replay, or tool results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    pub message: UserContent,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(rename = "isReplay", default, skip_serializing_if = "Option::is_none")]
    pub is_replay: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// When a host message sent during a turn reaches the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Now,
    /// At the next point the turn can take input, without interrupting it.
    Next,
    Later,
}

impl UserMessage {
    pub fn text(session_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            message: UserContent {
                role: "user".into(),
                content: MessageContent::Text(text.into()),
                extra: Extra::new(),
            },
            parent_tool_use_id: None,
            session_id: session_id.into(),
            uuid: None,
            timestamp: None,
            is_replay: None,
            tool_use_result: None,
            priority: None,
            extra: Extra::new(),
        }
    }

    pub fn with_priority(self, priority: Priority) -> Self {
        Self {
            priority: Some(priority),
            ..self
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserContent {
    pub role: String,
    pub content: MessageContent,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultMessage {
    /// `success`, `error_during_execution`, `error_max_turns`, ...
    pub subtype: String,
    pub is_error: bool,
    /// Absent when the turn was interrupted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<String>>,
    pub duration_ms: u64,
    pub duration_api_ms: u64,
    pub num_turns: u32,
    pub stop_reason: Option<String>,
    /// Client-side estimate at list price.
    pub total_cost_usd: Number,
    pub usage: Usage,
    #[serde(rename = "modelUsage", default)]
    pub model_usage: BTreeMap<String, ModelUsage>,
    #[serde(default)]
    pub permission_denials: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    pub session_id: String,
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<CacheCreation>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheCreation {
    #[serde(default)]
    pub ephemeral_1h_input_tokens: u64,
    #[serde(default)]
    pub ephemeral_5m_input_tokens: u64,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(rename = "costUSD", default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitEvent {
    pub rate_limit_info: RateLimitInfo,
    pub session_id: String,
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitInfo {
    /// `allowed`, `allowed_warning` or `rejected`.
    pub status: String,
    /// Epoch seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utilization: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surpassed_threshold: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overage_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_using_overage: Option<bool>,
    /// Keyed by window (`five_hour`, `seven_day`); utilization is a fraction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_windows: Option<BTreeMap<String, RateWindow>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utilization: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Either side can send one; the answer echoes `request_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlRequest<R> {
    pub request_id: String,
    pub request: R,
    #[serde(flatten)]
    pub extra: Extra,
}

impl<R> ControlRequest<R> {
    pub fn new(request_id: impl Into<String>, request: R) -> Self {
        Self {
            request_id: request_id.into(),
            request,
            extra: Extra::new(),
        }
    }
}

/// Withdraws the control request `request_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlCancelRequest {
    pub request_id: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Requests the CLI sends to the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum CliRequest {
    CanUseTool(Box<CanUseTool>),
    McpMessage(McpMessage),
    #[serde(untagged)]
    Other(Extra),
}

/// One MCP JSON-RPC message for the SDK-hosted server `server_name`, answered by `McpResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpMessage {
    pub server_name: String,
    pub message: Value,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Success payload answering an `mcp_message`: the server's JSON-RPC reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpResponse {
    pub mcp_response: Value,
}

/// Only the three identifying fields are typed strictly; the rest accept any
/// JSON shape so a new CLI version never turns an ask into an unknown request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanUseTool {
    pub tool_name: String,
    pub input: Value,
    pub tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_reason: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_reason_type: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_suggestions: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_path: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_to_no: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier_approvable: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppress_always_allow_rule: Option<Value>,
    /// Set on `AskUserQuestion` and `ExitPlanMode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_user_interaction: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Requests the host sends to the CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum HostRequest {
    Initialize {
        /// SDK-hosted MCP servers, reached through `mcp_message`.
        #[serde(
            rename = "sdkMcpServers",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        sdk_mcp_servers: Vec<String>,
        #[serde(flatten)]
        extra: Extra,
    },
    Interrupt {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cancel_queued: Option<bool>,
        #[serde(flatten)]
        extra: Extra,
    },
    SetPermissionMode {
        mode: PermissionMode,
        #[serde(flatten)]
        extra: Extra,
    },
    GetUsage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skip_behaviors: Option<bool>,
        #[serde(flatten)]
        extra: Extra,
    },
    GetContextUsage {
        #[serde(flatten)]
        extra: Extra,
    },
    GetSettings {
        #[serde(flatten)]
        extra: Extra,
    },
    /// `None` goes back to the CLI's default model.
    SetModel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(flatten)]
        extra: Extra,
    },
    /// `settings` holds settings keys, such as `effortLevel`.
    ApplyFlagSettings {
        settings: Value,
        #[serde(flatten)]
        extra: Extra,
    },
    /// Stops one task, such as a subagent.
    StopTask {
        task_id: String,
        #[serde(flatten)]
        extra: Extra,
    },
    #[serde(untagged)]
    Other(Extra),
}

impl HostRequest {
    pub fn initialize(sdk_mcp_servers: Vec<String>) -> Self {
        Self::Initialize {
            sdk_mcp_servers,
            extra: Extra::new(),
        }
    }

    pub fn interrupt(cancel_queued: Option<bool>) -> Self {
        Self::Interrupt {
            cancel_queued,
            extra: Extra::new(),
        }
    }

    pub fn set_permission_mode(mode: PermissionMode) -> Self {
        Self::SetPermissionMode {
            mode,
            extra: Extra::new(),
        }
    }

    pub fn get_usage(skip_behaviors: Option<bool>) -> Self {
        Self::GetUsage {
            skip_behaviors,
            extra: Extra::new(),
        }
    }

    pub fn get_context_usage() -> Self {
        Self::GetContextUsage {
            extra: Extra::new(),
        }
    }

    pub fn get_settings() -> Self {
        Self::GetSettings {
            extra: Extra::new(),
        }
    }

    pub fn set_model(model: Option<String>) -> Self {
        Self::SetModel {
            model,
            extra: Extra::new(),
        }
    }

    pub fn apply_flag_settings(settings: Value) -> Self {
        Self::ApplyFlagSettings {
            settings,
            extra: Extra::new(),
        }
    }

    pub fn stop_task(task_id: impl Into<String>) -> Self {
        Self::StopTask {
            task_id: task_id.into(),
            extra: Extra::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlResponse {
    pub response: ControlResponseBody,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlResponseBody {
    Success {
        request_id: String,
        /// Shape depends on the request: see the `*Response` types and `PermissionResult`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<Value>,
        #[serde(flatten)]
        extra: Extra,
    },
    Error {
        request_id: String,
        error: String,
        #[serde(flatten)]
        extra: Extra,
    },
    #[serde(untagged)]
    Other(Extra),
}

impl ControlResponse {
    pub fn success(request_id: impl Into<String>, payload: impl Serialize) -> Result<Self> {
        Ok(Self::from_body(ControlResponseBody::Success {
            request_id: request_id.into(),
            response: Some(serde_json::to_value(payload)?),
            extra: Extra::new(),
        }))
    }

    pub fn permission(request_id: impl Into<String>, result: &PermissionResult) -> Self {
        Self::from_body(ControlResponseBody::Success {
            request_id: request_id.into(),
            response: Some(serde_json::to_value(result).expect("permission result serializes")),
            extra: Extra::new(),
        })
    }

    pub fn mcp(request_id: impl Into<String>, reply: Value) -> Self {
        Self::from_body(ControlResponseBody::Success {
            request_id: request_id.into(),
            response: Some(
                serde_json::to_value(McpResponse {
                    mcp_response: reply,
                })
                .expect("mcp response serializes"),
            ),
            extra: Extra::new(),
        })
    }

    pub fn error(request_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self::from_body(ControlResponseBody::Error {
            request_id: request_id.into(),
            error: error.into(),
            extra: Extra::new(),
        })
    }

    fn from_body(response: ControlResponseBody) -> Self {
        Self {
            response,
            extra: Extra::new(),
        }
    }

    pub fn request_id(&self) -> Option<&str> {
        match &self.response {
            ControlResponseBody::Success { request_id, .. }
            | ControlResponseBody::Error { request_id, .. } => Some(request_id),
            ControlResponseBody::Other(extra) => extra.get("request_id").and_then(Value::as_str),
        }
    }

    /// Decodes a success payload; an error response becomes an `Err` carrying its message.
    pub fn payload<T: DeserializeOwned>(&self) -> Result<T> {
        match &self.response {
            ControlResponseBody::Success { response, .. } => Ok(serde_json::from_value(
                response.clone().unwrap_or(Value::Null),
            )?),
            ControlResponseBody::Error {
                request_id, error, ..
            } => Err(anyhow!("control request {request_id} failed: {error}")),
            ControlResponseBody::Other(extra) => {
                Err(anyhow!("unrecognized control response: {extra:?}"))
            }
        }
    }
}

/// Answer to `can_use_tool`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "snake_case")]
pub enum PermissionResult {
    Allow {
        /// For `AskUserQuestion`: the input plus `answers` keyed by question text.
        #[serde(rename = "updatedInput")]
        updated_input: Value,
        #[serde(
            rename = "updatedPermissions",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        updated_permissions: Option<Vec<PermissionUpdate>>,
        #[serde(flatten)]
        extra: Extra,
    },
    Deny {
        /// Shown to the model.
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interrupt: Option<bool>,
        #[serde(flatten)]
        extra: Extra,
    },
}

impl PermissionResult {
    pub fn allow(updated_input: Value) -> Self {
        Self::allow_with(updated_input, Vec::new())
    }

    /// An allow that also applies `updates`; none leaves `updatedPermissions` out.
    pub fn allow_with(updated_input: Value, updates: Vec<PermissionUpdate>) -> Self {
        Self::Allow {
            updated_input,
            updated_permissions: (!updates.is_empty()).then_some(updates),
            extra: Extra::new(),
        }
    }

    pub fn deny(message: impl Into<String>) -> Self {
        Self::Deny {
            message: message.into(),
            interrupt: None,
            extra: Extra::new(),
        }
    }
}

/// A permission change: suggested in `can_use_tool.permission_suggestions`,
/// applied when sent back in an allow's `updatedPermissions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PermissionUpdate {
    AddRules {
        rules: Vec<PermissionRule>,
        /// `allow`, `deny` or `ask`.
        behavior: String,
        /// `session`, `localSettings`, `projectSettings`, `userSettings`, ...
        destination: String,
        #[serde(flatten)]
        extra: Extra,
    },
    AddDirectories {
        directories: Vec<String>,
        destination: String,
        #[serde(flatten)]
        extra: Extra,
    },
    SetMode {
        mode: String,
        destination: String,
        #[serde(flatten)]
        extra: Extra,
    },
    #[serde(untagged)]
    Other(Value),
}

/// `Read(//c/a/**)` is `{toolName: "Read", ruleContent: "//c/a/**"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRule {
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_content: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl CanUseTool {
    /// The suggestions, typed; empty when absent or not a list.
    pub fn suggestions(&self) -> Vec<PermissionUpdate> {
        self.permission_suggestions
            .clone()
            .and_then(|raw| serde_json::from_value(raw).ok())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_permission_mode: Option<PermissionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Slash commands and skills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<InitCommand>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<InitModel>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitModel {
    /// Passed to `--model` and `set_model`.
    pub value: String,
    /// The model id `value` stands for, such as `claude-opus-5-5` for `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_effort: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_effort_levels: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_source: Option<String>,
    /// `firstParty` for Anthropic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_provider: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterruptResponse {
    /// Present when `system/init.capabilities` has `interrupt_receipt_v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub still_queued: Option<Vec<Value>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetPermissionModeResponse {
    pub mode: PermissionMode,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetUsageResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_type: Option<String>,
    /// False for API key sessions.
    pub rate_limits_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limits: Option<UsageRateLimits>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The parts of a `get_context_usage` reply vigie2 reads; the rest describes the
/// user's configuration and is dropped unread.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsageResponse {
    pub total_tokens: u64,
    pub max_tokens: u64,
    #[serde(default)]
    pub auto_compact_threshold: Option<u64>,
    #[serde(default)]
    pub categories: Vec<ContextCategory>,
}

/// The part of a `get_settings` reply vigie2 reads; the rest holds the user's
/// settings and is dropped unread.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SettingsResponse {
    pub applied: AppliedSettings,
}

/// The model and effort the CLI runs on, such as `claude-opus-5-5[1m]` and `medium`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedSettings {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCategory {
    pub name: String,
    pub tokens: u64,
    /// `used`, `deferred`, `buffer` or `free`.
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRateLimits {
    #[serde(default)]
    pub five_hour: Option<UsageWindow>,
    #[serde(default)]
    pub seven_day: Option<UsageWindow>,
    #[serde(default)]
    pub extra_usage: Option<ExtraUsage>,
    #[serde(default)]
    pub limits: Vec<UsageLimit>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    /// Percent (7 means 7 %).
    #[serde(default)]
    pub utilization: Option<Number>,
    /// ISO 8601.
    #[serde(default)]
    pub resets_at: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtraUsage {
    pub is_enabled: bool,
    #[serde(default)]
    pub monthly_limit: Option<Number>,
    #[serde(default)]
    pub used_credits: Option<Number>,
    #[serde(default)]
    pub utilization: Option<Number>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageLimit {
    /// `session`, `weekly_all`, `weekly_scoped`, ...
    pub kind: String,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub percent: Option<Number>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub resets_at: Option<String>,
    #[serde(default)]
    pub scope: Option<Value>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;
    use crate::claude::replay::runs_dir;

    #[test]
    fn permission_modes_keep_their_wire_string() {
        for (text, mode) in [
            ("default", PermissionMode::Default),
            ("acceptEdits", PermissionMode::AcceptEdits),
            ("plan", PermissionMode::Plan),
            ("bypassPermissions", PermissionMode::BypassPermissions),
            ("dontAsk", PermissionMode::Other("dontAsk".into())),
        ] {
            let json = Value::String(text.into());
            assert_eq!(
                serde_json::from_value::<PermissionMode>(json.clone()).unwrap(),
                mode
            );
            assert_eq!(serde_json::to_value(&mode).unwrap(), json);
        }
    }

    fn recordings(suffix: &str) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(runs_dir())
            .expect("read spike/runs")
            .map(|entry| entry.expect("dir entry").path())
            .filter(|path| {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name.ends_with(suffix) && (suffix != ".ndjson" || !name.ends_with(".stdout.ndjson"))
            })
            .collect();
        paths.sort();
        assert!(!paths.is_empty(), "no {suffix} recordings");
        paths
    }

    fn blocks_fallback(blocks: &[ContentBlock]) -> Option<&'static str> {
        blocks
            .iter()
            .any(|block| matches!(block, ContentBlock::Other(_)))
            .then_some("content block")
    }

    fn content_fallback(content: &MessageContent) -> Option<&'static str> {
        match content {
            MessageContent::Text(_) => None,
            MessageContent::Blocks(blocks) => blocks_fallback(blocks),
        }
    }

    /// Names the raw fallback a typed line landed in, if any.
    fn inbound_fallback(line: &Inbound) -> Option<&'static str> {
        match line {
            Inbound::Unknown(_) => Some("line"),
            Inbound::System(System::Other(_)) => Some("system subtype"),
            Inbound::System(_) => None,
            Inbound::StreamEvent(event) => match &event.event {
                StreamEventBody::Other(_) => Some("stream event"),
                StreamEventBody::MessageStart { message, .. } => blocks_fallback(&message.content),
                StreamEventBody::ContentBlockStart {
                    content_block: ContentBlock::Other(_),
                    ..
                } => Some("content block"),
                StreamEventBody::ContentBlockDelta {
                    delta: Delta::Other(_),
                    ..
                } => Some("delta"),
                _ => None,
            },
            Inbound::Assistant(message) => blocks_fallback(&message.message.content),
            Inbound::User(message) => content_fallback(&message.message.content),
            Inbound::Result(_) | Inbound::RateLimitEvent(_) | Inbound::ControlCancelRequest(_) => {
                None
            }
            Inbound::ControlRequest(request) => {
                matches!(request.request, CliRequest::Other(_)).then_some("cli request")
            }
            Inbound::ControlResponse(response) => {
                matches!(response.response, ControlResponseBody::Other(_))
                    .then_some("control response")
            }
        }
    }

    fn outbound_fallback(line: &Outbound) -> Option<&'static str> {
        match line {
            Outbound::Unknown(_) => Some("line"),
            Outbound::User(message) => content_fallback(&message.message.content),
            Outbound::ControlRequest(request) => {
                matches!(request.request, HostRequest::Other(_)).then_some("host request")
            }
            Outbound::ControlResponse(response) => {
                matches!(response.response, ControlResponseBody::Other(_))
                    .then_some("control response")
            }
        }
    }

    fn assert_round_trip<T>(original: &Value, context: &str) -> T
    where
        T: Serialize + DeserializeOwned + std::fmt::Debug,
    {
        let typed: T = serde_json::from_value(original.clone())
            .unwrap_or_else(|err| panic!("{context}: parse failed: {err}"));
        let back = serde_json::to_value(&typed).expect("serialize");
        assert_eq!(&back, original, "{context}: round trip changed the line");
        typed
    }

    /// What a control request asks for, so its response payload can be typed.
    #[derive(Clone, Copy, Debug)]
    enum Answers {
        Initialize,
        Interrupt,
        SetPermissionMode,
        GetUsage,
        StopTask,
        Permission,
    }

    fn check_payload(response: &ControlResponse, answers: Answers, context: &str) {
        let ControlResponseBody::Success {
            response: Some(payload),
            ..
        } = &response.response
        else {
            panic!("{context}: not a success with a payload");
        };
        match answers {
            Answers::Initialize => {
                let typed: InitializeResponse = assert_round_trip(payload, context);
                assert!(typed.account.is_some(), "{context}: no account");
            }
            Answers::Interrupt => {
                let typed: InterruptResponse = assert_round_trip(payload, context);
                assert!(typed.still_queued.is_some(), "{context}: no still_queued");
            }
            Answers::SetPermissionMode => {
                assert_round_trip::<SetPermissionModeResponse>(payload, context);
            }
            Answers::GetUsage => {
                let typed: GetUsageResponse = assert_round_trip(payload, context);
                assert!(typed.rate_limits.is_some(), "{context}: no rate_limits");
            }
            Answers::StopTask => {
                assert_eq!(payload, &serde_json::json!({}), "{context}");
            }
            Answers::Permission => {
                assert_round_trip::<PermissionResult>(payload, context);
            }
        }
    }

    #[test]
    fn every_recorded_line_is_typed_and_round_trips() {
        for path in recordings(".ndjson") {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).expect("read recording");
            let mut answers: HashMap<String, Answers> = HashMap::new();
            for (index, line) in text.lines().enumerate() {
                let context = format!("{name}:{}", index + 1);
                let record: Value = serde_json::from_str(line).expect("recording line is JSON");
                let msg = &record["msg"];
                match record["dir"].as_str() {
                    Some("out") => {
                        let typed: Inbound = assert_round_trip(msg, &context);
                        if let Some(fallback) = inbound_fallback(&typed) {
                            panic!("{context}: landed in the raw {fallback} fallback");
                        }
                        match &typed {
                            Inbound::ControlRequest(request) => {
                                answers.insert(request.request_id.clone(), Answers::Permission);
                            }
                            Inbound::ControlResponse(response) => {
                                let id = response.request_id().expect("request id");
                                let kind = answers[id];
                                check_payload(response, kind, &context);
                            }
                            _ => {}
                        }
                    }
                    Some("in") => {
                        let typed: Outbound = assert_round_trip(msg, &context);
                        if let Some(fallback) = outbound_fallback(&typed) {
                            panic!("{context}: landed in the raw {fallback} fallback");
                        }
                        match &typed {
                            Outbound::ControlRequest(request) => {
                                let kind = match request.request {
                                    HostRequest::Initialize { .. } => Answers::Initialize,
                                    HostRequest::Interrupt { .. } => Answers::Interrupt,
                                    HostRequest::SetPermissionMode { .. } => {
                                        Answers::SetPermissionMode
                                    }
                                    HostRequest::GetUsage { .. } => Answers::GetUsage,
                                    HostRequest::StopTask { .. } => Answers::StopTask,
                                    HostRequest::GetContextUsage { .. }
                                    | HostRequest::GetSettings { .. }
                                    | HostRequest::SetModel { .. }
                                    | HostRequest::ApplyFlagSettings { .. }
                                    | HostRequest::Other(_) => unreachable!(),
                                };
                                answers.insert(request.request_id.clone(), kind);
                            }
                            Outbound::ControlResponse(response) => {
                                let id = response.request_id().expect("request id");
                                let kind = answers[id];
                                check_payload(response, kind, &context);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn every_raw_stdout_line_is_typed_and_round_trips() {
        for path in recordings(".stdout.ndjson") {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).expect("read recording");
            for (index, line) in text.lines().enumerate() {
                let context = format!("{name}:{}", index + 1);
                let original: Value = serde_json::from_str(line).expect("stdout line is JSON");
                let typed: Inbound = assert_round_trip(&original, &context);
                if let Some(fallback) = inbound_fallback(&typed) {
                    panic!("{context}: landed in the raw {fallback} fallback");
                }
            }
        }
    }

    #[test]
    fn typed_fields_carry_the_recorded_values() {
        let text = std::fs::read_to_string(runs_dir().join("basic.stdout.ndjson")).unwrap();
        let lines: Vec<Inbound> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let init = lines
            .iter()
            .find_map(|line| match line {
                Inbound::System(System::Init(init)) => Some(init),
                _ => None,
            })
            .expect("init line");
        assert_eq!(init.api_key_source, "none");
        assert!(
            init.capabilities
                .iter()
                .any(|c| c == "interrupt_receipt_v1")
        );
        let result = lines
            .iter()
            .find_map(|line| match line {
                Inbound::Result(result) => Some(result),
                _ => None,
            })
            .expect("result line");
        let cache = result
            .usage
            .cache_creation
            .as_ref()
            .expect("cache_creation");
        assert_eq!(cache.ephemeral_1h_input_tokens, 15607);
        assert_eq!(cache.ephemeral_5m_input_tokens, 0);
        assert_eq!(result.result.as_deref(), Some("ok"));
    }

    #[test]
    fn unknown_kinds_are_kept_raw() {
        let line = serde_json::json!({"type": "brand_new", "x": 1});
        let typed: Inbound = serde_json::from_value(line.clone()).unwrap();
        assert!(matches!(typed, Inbound::Unknown(_)));
        assert_eq!(serde_json::to_value(&typed).unwrap(), line);

        let line = serde_json::json!({"type": "system", "subtype": "brand_new", "x": 1});
        let typed: Inbound = serde_json::from_value(line.clone()).unwrap();
        assert!(matches!(typed, Inbound::System(System::Other(_))));
        assert_eq!(serde_json::to_value(&typed).unwrap(), line);
    }

    #[test]
    fn cancel_requests_are_typed() {
        let line = serde_json::json!({"type": "control_cancel_request", "request_id": "cli-2"});
        let typed: Inbound = serde_json::from_value(line.clone()).unwrap();
        assert!(
            matches!(&typed, Inbound::ControlCancelRequest(cancel) if cancel.request_id == "cli-2")
        );
        assert_eq!(serde_json::to_value(&typed).unwrap(), line);
    }

    #[test]
    fn odd_shaped_ask_fields_stay_an_ask() {
        let line = serde_json::json!({"type": "control_request", "request_id": "r1",
            "request": {"subtype": "can_use_tool", "tool_name": "Bash",
                "input": {"command": "ls"}, "tool_use_id": "t1",
                "decision_reason": {"type": "rule", "rule": {"source": "project"}},
                "default_to_no": "yes", "display_name": 3, "brand_new": [1]}});
        let typed: Inbound = serde_json::from_value(line.clone()).unwrap();
        let Inbound::ControlRequest(request) = &typed else {
            panic!("not a control request: {typed:?}");
        };
        let CliRequest::CanUseTool(ask) = &request.request else {
            panic!("fell back to Other: {:?}", request.request);
        };
        assert_eq!(ask.tool_name, "Bash");
        assert_eq!(ask.decision_reason.as_ref().unwrap()["type"], "rule");
        assert_eq!(serde_json::to_value(&typed).unwrap(), line);
    }

    #[test]
    fn host_constructors_match_the_wire() {
        let deny = Outbound::ControlResponse(ControlResponse::permission(
            "abc",
            &PermissionResult::deny("no"),
        ));
        assert_eq!(
            serde_json::to_value(&deny).unwrap(),
            serde_json::json!({"type": "control_response", "response": {
                "subtype": "success", "request_id": "abc",
                "response": {"behavior": "deny", "message": "no"}}}),
        );
        let error = Outbound::ControlResponse(ControlResponse::error("abc", "nope"));
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({"type": "control_response", "response": {
                "subtype": "error", "request_id": "abc", "error": "nope"}}),
        );
        let user = Outbound::User(UserMessage::text("s", "hi"));
        assert_eq!(
            serde_json::to_value(&user).unwrap(),
            serde_json::json!({"type": "user", "message": {"role": "user", "content": "hi"},
                "parent_tool_use_id": null, "session_id": "s"}),
        );
        let queued = Outbound::User(UserMessage::text("s", "hi").with_priority(Priority::Next));
        assert_eq!(
            serde_json::to_value(&queued).unwrap(),
            serde_json::json!({"type": "user", "message": {"role": "user", "content": "hi"},
                "parent_tool_use_id": null, "session_id": "s", "priority": "next"}),
        );
        let interrupt =
            Outbound::ControlRequest(ControlRequest::new("r1", HostRequest::interrupt(None)));
        assert_eq!(
            serde_json::to_value(&interrupt).unwrap(),
            serde_json::json!({"type": "control_request", "request_id": "r1",
                "request": {"subtype": "interrupt"}}),
        );
        let model = Outbound::ControlRequest(ControlRequest::new(
            "r2",
            HostRequest::set_model(Some("sonnet".into())),
        ));
        assert_eq!(
            serde_json::to_value(&model).unwrap(),
            serde_json::json!({"type": "control_request", "request_id": "r2",
                "request": {"subtype": "set_model", "model": "sonnet"}}),
        );
        let effort = Outbound::ControlRequest(ControlRequest::new(
            "r3",
            HostRequest::apply_flag_settings(serde_json::json!({"effortLevel": "high"})),
        ));
        assert_eq!(
            serde_json::to_value(&effort).unwrap(),
            serde_json::json!({"type": "control_request", "request_id": "r3",
                "request": {"subtype": "apply_flag_settings", "settings": {"effortLevel": "high"}}}),
        );
        let stop =
            Outbound::ControlRequest(ControlRequest::new("r4", HostRequest::stop_task("a1")));
        assert_eq!(
            serde_json::to_value(&stop).unwrap(),
            serde_json::json!({"type": "control_request", "request_id": "r4",
                "request": {"subtype": "stop_task", "task_id": "a1"}}),
        );
    }

    #[test]
    fn initialize_lists_models_and_commands() {
        let text = std::fs::read_to_string(runs_dir().join("basic.ndjson")).unwrap();
        let initialize = text
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|record| record["msg"]["response"]["response"]["models"].is_array())
            .expect("initialize reply");
        let reply: InitializeResponse =
            serde_json::from_value(initialize["msg"]["response"]["response"].clone()).unwrap();
        let models = reply.models.expect("models");
        let opus = models.iter().find(|model| model.value == "opus").unwrap();
        assert_eq!(opus.supports_effort, Some(true));
        assert!(
            opus.supported_effort_levels
                .as_ref()
                .is_some_and(|levels| levels.iter().any(|level| level == "max"))
        );
        assert_eq!(opus.resolved_model.as_deref(), Some("claude-opus-5-5"));
        let haiku = models.iter().find(|model| model.value == "haiku").unwrap();
        assert_eq!(haiku.supports_effort, None);
        let commands = reply.commands.expect("commands");
        let design = commands
            .iter()
            .find(|command| command.name == "design")
            .unwrap();
        assert_eq!(design.argument_hint.as_deref(), Some("consent | revoke"));
    }

    #[test]
    fn recorded_permission_updates_are_typed() {
        let text = std::fs::read_to_string(runs_dir().join("grants.ndjson")).unwrap();
        let mut suggestions = 0;
        let mut sent = 0;
        for line in text.lines() {
            let record: Value = serde_json::from_str(line).unwrap();
            let msg = &record["msg"];
            let updates = match record["dir"].as_str() {
                Some("out") => match serde_json::from_value::<Inbound>(msg.clone()).unwrap() {
                    Inbound::ControlRequest(ControlRequest {
                        request: CliRequest::CanUseTool(ask),
                        ..
                    }) => {
                        suggestions += 1;
                        ask.suggestions()
                    }
                    _ => continue,
                },
                _ => match serde_json::from_value::<PermissionResult>(
                    msg["response"]["response"].clone(),
                ) {
                    Ok(PermissionResult::Allow {
                        updated_permissions: Some(updates),
                        ..
                    }) => {
                        sent += 1;
                        updates
                    }
                    _ => continue,
                },
            };
            assert!(!updates.is_empty(), "{line}");
            assert!(
                !updates
                    .iter()
                    .any(|update| matches!(update, PermissionUpdate::Other(_))),
                "{line}"
            );
        }
        assert_eq!((suggestions, sent), (7, 2));
    }

    #[test]
    fn settings_reply_keeps_only_the_applied_values() {
        let reply: SettingsResponse = serde_json::from_value(serde_json::json!({
            "applied": {"model": "claude-opus-5-5[1m]", "effort": "medium", "other": 1},
            "effective": {"theme": "dark"},
            "sources": [{"source": "userSettings", "settings": {}}],
        }))
        .unwrap();
        assert_eq!(
            reply.applied,
            AppliedSettings {
                model: Some("claude-opus-5-5[1m]".into()),
                effort: Some("medium".into()),
            }
        );
    }
}
