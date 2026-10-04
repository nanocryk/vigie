//! The gate on calls that start agents or unattended work: a `PreToolUse` hook
//! turns each into an ask, unless an earlier answer for the session settles it.
//! Calls from inside a subagent are denied while `NESTED` is `Nested::Deny`.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::claude::protocol::HookOutput;
use crate::claude::tools::AGENT_TOOLS;

/// Callback id of the `PreToolUse` hook on `GATED_TOOLS`.
pub const FANOUT_HOOK: &str = "vigie_fanout";
/// Tools starting agents or unattended work.
pub const GATED_TOOLS: [&str; 5] = ["Agent", "Task", "Workflow", "CronCreate", "ScheduleWakeup"];

/// What a gated call made from inside a subagent gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nested {
    /// Denied at once.
    Deny,
    /// Asked like a main agent's call, keyed by its parent agent.
    Gate,
}

pub const NESTED: Nested = Nested::Deny;

const NESTED_DENIED: &str = "Nested agents and unattended work are disabled inside a subagent. \
                             Finish the task yourself, or report back to the agent that started you.";

/// What a session answer covers: one tool from one parent, and for Agent one agent type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FanoutKey {
    /// `Task` is filed as `Agent`.
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// Task id of the calling subagent; `None` for the main agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

impl FanoutKey {
    fn of(tool_name: &str, input: &Value, parent: Option<&str>) -> Self {
        let is_agent = AGENT_TOOLS.contains(&tool_name);
        Self {
            tool: if is_agent { "Agent" } else { tool_name }.to_owned(),
            agent_type: is_agent.then(|| {
                input["subagent_type"]
                    .as_str()
                    .unwrap_or("general-purpose")
                    .to_owned()
            }),
            parent: parent.map(str::to_owned),
        }
    }

    fn denied_message(&self) -> String {
        match &self.agent_type {
            Some(agent_type) => format!(
                "The user denied {} ({agent_type}) for this session.",
                self.tool
            ),
            None => format!("The user denied {} for this session.", self.tool),
        }
    }
}

/// The card's view of a gated call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanoutOffer {
    pub key: FanoutKey,
    /// The model the call sets; `None` leaves the agent type's default or the session's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remember {
    Allow,
    Deny,
}

/// Answers for the session and the calls sent to an ask, until the child stops.
#[derive(Debug, Default)]
pub struct FanoutGate {
    allowed: HashSet<FanoutKey>,
    denied: HashSet<FanoutKey>,
    /// Calls the hook sent to an ask, by tool use id, until their ask arrives.
    asked: HashMap<String, FanoutOffer>,
}

impl FanoutGate {
    /// The hook's answer to a gated call. `input` is the hook input, carrying
    /// `agent_id` when the call comes from inside a subagent.
    pub fn decide(&mut self, input: &Value, tool_use_id: Option<&str>) -> HookOutput {
        let tool_name = input["tool_name"].as_str().unwrap_or_default();
        let tool_input = &input["tool_input"];
        let parent = input["agent_id"].as_str();
        if parent.is_some() && NESTED == Nested::Deny {
            return HookOutput::deny(NESTED_DENIED);
        }
        let key = FanoutKey::of(tool_name, tool_input, parent);
        if self.denied.contains(&key) {
            return HookOutput::deny(key.denied_message());
        }
        if self.allowed.contains(&key) {
            return HookOutput::allow("Allowed by the user for this session");
        }
        if let Some(tool_use_id) = tool_use_id {
            let model = tool_input["model"].as_str().map(str::to_owned);
            self.asked
                .insert(tool_use_id.to_owned(), FanoutOffer { key, model });
        }
        HookOutput::ask("Starts agents or unattended work")
    }

    /// The offer of the call an ask is for, when the hook sent it there.
    pub fn take(&mut self, tool_use_id: &str) -> Option<FanoutOffer> {
        self.asked.remove(tool_use_id)
    }

    pub fn remember(&mut self, key: FanoutKey, remember: Remember) {
        match remember {
            Remember::Allow => {
                self.denied.remove(&key);
                self.allowed.insert(key);
            }
            Remember::Deny => {
                self.allowed.remove(&key);
                self.denied.insert(key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn decision(output: &HookOutput) -> Option<&str> {
        output
            .hook_specific_output
            .as_ref()
            .map(|output| output.permission_decision.as_str())
    }

    fn agent(agent_type: &str) -> Value {
        json!({"tool_name": "Agent", "tool_input": {"subagent_type": agent_type, "prompt": "p"}})
    }

    #[test]
    fn a_call_asks_until_a_session_answer_covers_it() {
        let mut gate = FanoutGate::default();
        assert_eq!(
            decision(&gate.decide(&agent("Explore"), Some("t1"))),
            Some("ask")
        );
        let offer = gate.take("t1").unwrap();
        assert_eq!(offer.key.agent_type.as_deref(), Some("Explore"));
        assert_eq!(gate.take("t1"), None);

        gate.remember(offer.key, Remember::Allow);
        assert_eq!(
            decision(&gate.decide(&agent("Explore"), Some("t2"))),
            Some("allow")
        );
        assert_eq!(gate.take("t2"), None);
        assert_eq!(
            decision(&gate.decide(&agent("general-purpose"), Some("t3"))),
            Some("ask"),
            "another agent type asks"
        );
    }

    #[test]
    fn a_session_deny_names_the_tool_and_type() {
        let mut gate = FanoutGate::default();
        gate.decide(&agent("Plan"), Some("t1"));
        let key = gate.take("t1").unwrap().key;
        gate.remember(key, Remember::Deny);
        let output = gate.decide(&agent("Plan"), Some("t2"));
        let output = output.hook_specific_output.unwrap();
        assert_eq!(output.permission_decision, "deny");
        assert_eq!(
            output.permission_decision_reason,
            "The user denied Agent (Plan) for this session."
        );
    }

    #[test]
    fn task_is_filed_as_agent_and_other_tools_have_no_type() {
        let task = FanoutKey::of("Task", &json!({"subagent_type": "Explore"}), None);
        assert_eq!(
            task,
            FanoutKey::of("Agent", &json!({"subagent_type": "Explore"}), None)
        );
        let cron = FanoutKey::of("CronCreate", &json!({"cron": "* * * * *"}), None);
        assert_eq!(cron.agent_type, None);
        assert_eq!(
            cron.denied_message(),
            "The user denied CronCreate for this session."
        );
    }

    #[test]
    fn calls_from_a_subagent_are_denied() {
        let mut gate = FanoutGate::default();
        let mut nested = agent("Explore");
        nested["agent_id"] = json!("a1");
        gate.remember(
            FanoutKey::of("Agent", &nested["tool_input"], Some("a1")),
            Remember::Allow,
        );
        let output = gate.decide(&nested, Some("t1"));
        assert_eq!(decision(&output), Some("deny"));
        assert_eq!(gate.take("t1"), None);
    }
}
