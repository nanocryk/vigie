//! The events a session emits, and the answers and outcomes they carry.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::agents::RelayOutcome;
use super::danger::Danger;
use super::grant::{FileOffer, FolderOffer};
use super::plan::PlanReview;
use crate::claude::child::Launch;
use crate::claude::protocol::{Inbound, InitCommand, InitModel, PermissionMode};

#[derive(Debug, Clone)]
pub struct Event {
    /// Starts at 1 and grows by 1 per event of the session.
    pub seq: u64,
    pub time: DateTime<Utc>,
    pub body: EventBody,
}

#[derive(Debug, Clone)]
pub enum EventBody {
    Line(Box<Line>),
    Host(HostEvent),
}

/// One line of CLI stdout.
#[derive(Debug, Clone)]
pub struct Line {
    pub raw: String,
    /// `None` when the line is not JSON.
    pub parsed: Option<Inbound>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HostEvent {
    Started {
        launch: Launch,
        cwd: PathBuf,
    },
    /// A user message about to be written to the child; `client_id` names the command that sent it.
    UserSent {
        text: String,
        /// Paths of the attached files; the child got them after `text`, see `attachments::compose`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        /// Sent while a turn ran, so it starts no exchange of its own.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        mid_turn: bool,
    },
    AskQueued {
        request_id: String,
        tool_name: String,
        tool_use_id: String,
        #[serde(default)]
        danger: Danger,
        /// An edit ask the CLI pairs with a switch to Accept edits.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        accept_edits: bool,
        /// The folder the CLI suggests granting for the session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder: Option<Box<FolderOffer>>,
        /// The file of a Read or edit-tool ask, which the allow can grant.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<FileOffer>,
        /// The plan an `ExitPlanMode` ask submits, when one was found.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan: Option<Box<PlanReview>>,
        /// Task id of the subagent asking; `None` for the main agent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        /// When the host denies the ask if still unanswered; subagent asks only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deny_at: Option<DateTime<Utc>>,
    },
    /// An ask of a subagent whose earlier ask timed out, denied by the host
    /// without queueing it.
    AutoDenied {
        request_id: String,
        tool_name: String,
        tool_use_id: String,
        agent_id: String,
        message: String,
    },
    /// An ask on a granted file, allowed by the host without queueing it.
    AutoAllowed {
        request_id: String,
        tool_name: String,
        tool_use_id: String,
        file: FileOffer,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
    },
    /// A message for a subagent, about to be written to the main agent as `wrapped`.
    RelaySent {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        task_id: String,
        /// The Agent call that started the agent.
        agent_tool_use_id: String,
        name: String,
        wrapped: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        mid_turn: bool,
    },
    /// How the main agent handled the relay whose `RelaySent` has seq `relay_seq`.
    RelayChecked {
        relay_seq: u64,
        task_id: String,
        agent_tool_use_id: String,
        outcome: RelayOutcome,
    },
    AskResolved {
        request_id: String,
        resolution: Resolution,
    },
    /// The files Claude attached to its reply with `set_attachments`, as absolute
    /// paths; follows the `result` line ending the main agent's turn.
    ClaudeAttached {
        attachments: Vec<String>,
    },
    /// The CLI's permission mode changed, as reported by `initialize`, `system/init`,
    /// `system/status` or a successful `set_permission_mode`.
    PermissionMode {
        mode: PermissionMode,
    },
    /// The child was killed; the session ends with the following `ChildExited`.
    AuthGateFailed {
        reason: GateFailure,
    },
    /// A requested stop began: the CLI's interrupt of a running tool that may
    /// follow comes from it, not from the user.
    Stopping {
        reason: StopReason,
    },
    ChildExited {
        code: Option<i32>,
        stderr_tail: Vec<String>,
        /// Why the child was stopped; absent for any other exit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<StopReason>,
    },
    /// The discussion moved to a fresh Claude session; a continue carries the seed
    /// its first message starts with.
    Restarted {
        mode: RestartMode,
        exchanges: usize,
        total_exchanges: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seed: Option<String>,
    },
    /// The models and the commands the CLI offers, from its `initialize` reply.
    Catalog {
        models: Vec<ModelChoice>,
        commands: Vec<CommandInfo>,
    },
}

/// A model the CLI accepts, with the effort levels it supports (none when it has no effort).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    pub value: String,
    /// The model id `value` stands for, such as `claude-opus-5-5` for `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub effort_levels: Vec<String>,
}

impl ModelChoice {
    pub fn new(model: &InitModel) -> Self {
        let effort_levels = match (&model.supports_effort, &model.supported_effort_levels) {
            (Some(true), Some(levels)) => levels.clone(),
            _ => Vec::new(),
        };
        Self {
            value: model.value.clone(),
            resolved_model: model.resolved_model.clone(),
            display_name: model
                .display_name
                .clone()
                .unwrap_or_else(|| model.value.clone()),
            description: model.description.clone(),
            effort_levels,
        }
    }
}

/// A slash command or a skill; `argument_hint` is empty for one without arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub argument_hint: String,
}

impl CommandInfo {
    pub fn new(command: &InitCommand) -> Self {
        Self {
            name: command.name.clone(),
            description: command.description.clone().unwrap_or_default(),
            argument_hint: command.argument_hint.clone().unwrap_or_default(),
        }
    }
}

/// How a discussion moves to a fresh Claude session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartMode {
    /// Seeded with the last exchanges of the discussion.
    Continue,
    Blank,
    /// A blank restart asked for as the Clear command.
    Clear,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GateFailure {
    /// The `initialize` response did not report an Anthropic subscription account.
    ApiProvider { provider: Option<String> },
    /// The `initialize` request failed, so the account could not be checked.
    Initialize { error: String },
    /// A `system/init` reported API key billing.
    ApiKeySource { source: String },
    /// A `rate_limit_event` reported usage credits in use.
    Overage,
}

/// Answer to a pending ask.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Answer {
    /// `updated_input` defaults to the input the CLI sent. A non-blank `note`
    /// follows the answer as a user message queued for the running turn.
    Allow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// Also switches to Accept edits, through the CLI's suggestion.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        accept_edits: bool,
        /// Also grants this folder for the session: the suggested one or a parent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder: Option<String>,
        /// Also allows the later asks on the ask's file, until the child stops.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        always_for_file: bool,
    },
    /// The message is shown to the model.
    Deny { message: String },
    /// `AskUserQuestion` answers, keyed by question text.
    Questions { answers: BTreeMap<String, String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolution {
    Answered {
        answer: Answer,
    },
    /// The CLI withdrew the ask with `control_cancel_request`.
    Cancelled,
    /// Unanswered at its `deny_at`: the host denied it with `message`.
    TimedOut {
        message: String,
    },
}

/// Why a live child was stopped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Stopped from Home.
    #[default]
    Kill,
    /// Stopped to drop the folders granted from asks.
    Revoke,
    /// The server is shutting down.
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_and_asks_read_the_wire() {
        let with_note: Answer =
            serde_json::from_value(serde_json::json!({"kind": "allow", "note": "then test"}))
                .unwrap();
        assert_eq!(
            with_note,
            Answer::Allow {
                updated_input: None,
                note: Some("then test".into()),
                accept_edits: false,
                folder: None,
                always_for_file: false,
            }
        );
        let bare: Answer = serde_json::from_value(serde_json::json!({"kind": "allow"})).unwrap();
        assert_eq!(
            serde_json::to_value(bare).unwrap(),
            serde_json::json!({"kind": "allow"})
        );
        let granting: Answer = serde_json::from_value(serde_json::json!({"kind": "allow",
            "accept_edits": true, "folder": "C:\\a", "always_for_file": true}))
        .unwrap();
        assert_eq!(
            granting,
            Answer::Allow {
                updated_input: None,
                note: None,
                accept_edits: true,
                folder: Some("C:\\a".into()),
                always_for_file: true,
            }
        );

        let queued = HostEvent::AskQueued {
            request_id: "r".into(),
            tool_name: "Bash".into(),
            tool_use_id: "t".into(),
            danger: Danger::from_reasons(vec!["Hard reset: git reset --hard".into()]),
            accept_edits: false,
            folder: None,
            file: None,
            plan: None,
            agent_id: None,
            deny_at: None,
        };
        assert_eq!(
            serde_json::to_value(&queued).unwrap(),
            serde_json::json!({"event": "ask_queued", "request_id": "r", "tool_name": "Bash",
                "tool_use_id": "t", "danger": {"dangerous": true,
                "reasons": ["Hard reset: git reset --hard"]}}),
        );
        let stored: HostEvent = serde_json::from_value(serde_json::json!({"event": "ask_queued",
            "request_id": "r", "tool_name": "Bash", "tool_use_id": "t"}))
        .unwrap();
        assert!(matches!(stored, HostEvent::AskQueued {
            danger, accept_edits: false, folder: None, file: None, plan: None, agent_id: None,
            deny_at: None, ..
        } if danger == Danger::default()));

        let timed_out = Resolution::TimedOut {
            message: "wrap up".into(),
        };
        assert_eq!(
            serde_json::to_value(&timed_out).unwrap(),
            serde_json::json!({"kind": "timed_out", "message": "wrap up"}),
        );
    }
}
