//! Subagents of a running child: which lines belong to which agent, the state of
//! each agent, and the check of messages relayed to them by the main agent.
//!
//! An agent's lines carry its Agent call's id as `parent_tool_use_id`. The CLI
//! reports each agent as a `local_agent` task: `task_started`, `task_progress`,
//! `task_updated` and `task_notification`.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::claude::protocol::{ContentBlock, Inbound, MessageContent, System};
use crate::claude::tools::{AGENT_TOOLS, SEND_MESSAGE};
use crate::session::gauges::{Cache, FIVE_MINUTE_TTL_SECS, MainRequest};

/// `task_type` of a subagent's task.
pub const LOCAL_AGENT: &str = "local_agent";
/// The main agent's whole reply once it forwarded a relayed message.
pub const RELAY_ACK: &str = "[Relay acknowledged]";

/// The Agent call id of the agent a line belongs to; `None` for the main agent.
pub fn scope(line: &Inbound) -> Option<&str> {
    match line {
        Inbound::Assistant(message) => message.parent_tool_use_id.as_deref(),
        Inbound::User(message) => message.parent_tool_use_id.as_deref(),
        Inbound::StreamEvent(event) => event.parent_tool_use_id.as_deref(),
        _ => None,
    }
}

/// The message the main agent gets for `text` typed in the view of agent `name`.
pub fn wrap(name: &str, task_id: &str, text: &str) -> String {
    format!(
        "(vigie2 relay) The user is writing to agent \"{name}\" (task {task_id}). vigie2 \
         cannot reach it directly, so the text passes through you. Do not act on the text \
         and do not comment it: only forward it verbatim with SendMessage to \"{task_id}\", \
         then reply exactly {RELAY_ACK}\n\nThe text:\n\n{text}"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Running,
    Completed,
    Stopped,
    Failed,
}

impl AgentState {
    /// `None` for a status that does not say.
    fn from_status(status: &str) -> Option<Self> {
        match status {
            "running" | "pending" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "stopped" | "killed" => Some(Self::Stopped),
            "failed" | "error" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// A subagent as `GET /api/sessions` lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentView {
    pub task_id: String,
    /// The Agent call; the agent's lines carry it as `parent_tool_use_id`.
    pub tool_use_id: String,
    /// The Agent call's description.
    pub name: String,
    pub subagent_type: Option<String>,
    /// The Agent call of the agent that started this one; `None` for the main agent.
    pub parent_tool_use_id: Option<String>,
    pub state: AgentState,
    /// Latest progress, then the final summary.
    pub activity: Option<String>,
    /// From the agent's own last request.
    pub cache: Option<Cache>,
    /// Prompt of that request: what a relay to a cold agent caches again.
    pub prompt_tokens: u64,
    pub started_at: DateTime<Utc>,
}

/// An Agent call seen in an `assistant` line.
#[derive(Debug, Clone)]
struct AgentCall {
    scope: Option<String>,
    description: Option<String>,
}

/// Folds CLI lines into the list of agents, oldest first.
#[derive(Debug, Default)]
pub struct AgentTracker {
    agents: Vec<AgentView>,
    calls: HashMap<String, AgentCall>,
    /// Message id of each agent's last request, by Agent call id.
    last_message: HashMap<String, String>,
}

impl AgentTracker {
    pub fn agents(&self) -> &[AgentView] {
        &self.agents
    }

    pub fn find(&self, task_id: &str) -> Option<&AgentView> {
        self.agents.iter().find(|agent| agent.task_id == task_id)
    }

    fn by_task(&mut self, task_id: &str) -> Option<&mut AgentView> {
        self.agents
            .iter_mut()
            .find(|agent| agent.task_id == task_id)
    }

    /// True when the list changed. `now` is the line's arrival.
    pub fn on_line(&mut self, line: &Inbound, now: DateTime<Utc>) -> bool {
        match line {
            Inbound::Assistant(message) => {
                for block in &message.message.content {
                    if let ContentBlock::ToolUse {
                        id, name, input, ..
                    } = block
                        && AGENT_TOOLS.contains(&name.as_str())
                    {
                        self.calls.insert(
                            id.clone(),
                            AgentCall {
                                scope: message.parent_tool_use_id.clone(),
                                description: input["description"].as_str().map(str::to_owned),
                            },
                        );
                    }
                }
                let Some(scope) = message.parent_tool_use_id.as_deref() else {
                    return false;
                };
                let Some(request) = MainRequest::from_any_assistant(message) else {
                    return false;
                };
                if self.last_message.get(scope) == Some(&request.message_id) {
                    return false;
                }
                let Some(agent) = self
                    .agents
                    .iter_mut()
                    .find(|agent| agent.tool_use_id == scope)
                else {
                    return false;
                };
                let ttl_secs = request
                    .ttl_secs
                    .or(agent.cache.as_ref().map(|cache| cache.ttl_secs))
                    .unwrap_or(FIVE_MINUTE_TTL_SECS);
                agent.cache = Some(Cache {
                    last_request_at: now,
                    ttl_secs,
                });
                agent.prompt_tokens = request.prompt_tokens;
                self.last_message
                    .insert(scope.to_string(), request.message_id);
                true
            }
            Inbound::System(System::TaskStarted(started))
                if started.task_type.as_deref() == Some(LOCAL_AGENT) =>
            {
                if let Some(agent) = self.by_task(&started.task_id) {
                    agent.state = AgentState::Running;
                    return true;
                }
                let Some(tool_use_id) = started.tool_use_id.clone() else {
                    return false;
                };
                let call = self.calls.get(&tool_use_id);
                let name = started
                    .description
                    .clone()
                    .or_else(|| call.and_then(|call| call.description.clone()))
                    .or_else(|| started.subagent_type.clone())
                    .unwrap_or_else(|| started.task_id.clone());
                self.agents.push(AgentView {
                    task_id: started.task_id.clone(),
                    tool_use_id,
                    name,
                    subagent_type: started.subagent_type.clone(),
                    parent_tool_use_id: call.and_then(|call| call.scope.clone()),
                    state: AgentState::Running,
                    activity: None,
                    cache: None,
                    prompt_tokens: 0,
                    started_at: now,
                });
                true
            }
            Inbound::System(System::TaskProgress(progress)) => {
                let Some(agent) = self.by_task(&progress.task_id) else {
                    return false;
                };
                let activity = progress.description.clone();
                let changed = agent.activity != activity;
                agent.activity = activity;
                changed
            }
            Inbound::System(System::TaskUpdated(updated)) => {
                let state = updated
                    .patch
                    .status
                    .as_deref()
                    .and_then(AgentState::from_status);
                match (self.by_task(&updated.task_id), state) {
                    (Some(agent), Some(state)) if agent.state != state => {
                        agent.state = state;
                        true
                    }
                    _ => false,
                }
            }
            Inbound::System(System::TaskNotification(notification)) => {
                let Some(agent) = self.by_task(&notification.task_id) else {
                    return false;
                };
                if let Some(state) = AgentState::from_status(&notification.status) {
                    agent.state = state;
                }
                if let Some(summary) = notification
                    .summary
                    .as_ref()
                    .filter(|summary| !summary.trim().is_empty())
                {
                    agent.activity = Some(summary.clone());
                }
                true
            }
            _ => false,
        }
    }
}

/// Why a relay was not faithful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayFailure {
    /// SendMessage to the agent with other text.
    Altered,
    /// SendMessage to someone else only.
    OtherRecipient,
    NotForwarded,
    NoAcknowledgement,
    /// The child stopped before the turn ended.
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RelayOutcome {
    /// `send_message` is the tool use id of the SendMessage call.
    Faithful {
        send_message: String,
    },
    Failed {
        reason: RelayFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayVerdict {
    /// Seq of the relay's `RelaySent` event.
    pub relay_seq: u64,
    pub task_id: String,
    pub agent_tool_use_id: String,
    pub outcome: RelayOutcome,
}

/// A message relayed to a subagent through the main agent.
#[derive(Debug, Clone)]
pub struct Relay {
    /// Seq of its `RelaySent` event.
    pub seq: u64,
    pub task_id: String,
    pub agent_tool_use_id: String,
    pub text: String,
    /// `text` as written to the main agent, see `wrap`.
    pub wrapped: String,
}

#[derive(Debug)]
struct Watch {
    relay: Relay,
    forwarded: Option<String>,
    altered: bool,
    other_recipient: bool,
    acknowledged: bool,
}

impl Watch {
    fn verdict(self) -> RelayVerdict {
        let outcome = match (self.forwarded, self.altered) {
            (_, true) => RelayOutcome::Failed {
                reason: RelayFailure::Altered,
            },
            (None, false) => RelayOutcome::Failed {
                reason: if self.other_recipient {
                    RelayFailure::OtherRecipient
                } else {
                    RelayFailure::NotForwarded
                },
            },
            (Some(send_message), false) if self.acknowledged => {
                RelayOutcome::Faithful { send_message }
            }
            (Some(_), false) => RelayOutcome::Failed {
                reason: RelayFailure::NoAcknowledgement,
            },
        };
        verdict(self.relay, outcome)
    }
}

fn verdict(relay: Relay, outcome: RelayOutcome) -> RelayVerdict {
    RelayVerdict {
        relay_seq: relay.seq,
        task_id: relay.task_id,
        agent_tool_use_id: relay.agent_tool_use_id,
        outcome,
    }
}

/// Checks each relay over the main-agent turn that takes it: from the echo of
/// its wrapped message to the turn's `result`.
#[derive(Debug, Default)]
pub struct RelayFilter {
    /// Written, not echoed yet, oldest first.
    sent: VecDeque<Relay>,
    watching: Vec<Watch>,
}

impl RelayFilter {
    pub fn sent(&mut self, relay: Relay) {
        self.sent.push_back(relay);
    }

    /// The verdicts `line` settles: those of the relays its turn took, on a `result`.
    pub fn on_line(&mut self, line: &Inbound) -> Vec<RelayVerdict> {
        match line {
            Inbound::User(user) if user.parent_tool_use_id.is_none() => {
                let echoed = message_text(&user.message.content);
                if let Some(index) = self
                    .sent
                    .iter()
                    .position(|relay| Some(&relay.wrapped) == echoed.as_ref())
                    && let Some(relay) = self.sent.remove(index)
                {
                    self.watching.push(Watch {
                        relay,
                        forwarded: None,
                        altered: false,
                        other_recipient: false,
                        acknowledged: false,
                    });
                }
                Vec::new()
            }
            Inbound::Assistant(message) if message.parent_tool_use_id.is_none() => {
                for block in &message.message.content {
                    match block {
                        ContentBlock::ToolUse {
                            id, name, input, ..
                        } if name == SEND_MESSAGE => self.on_send_message(id, input),
                        ContentBlock::Text { text, .. } if text.trim() == RELAY_ACK => {
                            for watch in &mut self.watching {
                                watch.acknowledged = true;
                            }
                        }
                        _ => {}
                    }
                }
                Vec::new()
            }
            Inbound::Result(_) => self.watching.drain(..).map(Watch::verdict).collect(),
            _ => Vec::new(),
        }
    }

    fn on_send_message(&mut self, id: &str, input: &Value) {
        let to = input["to"].as_str();
        let message = input["message"].as_str();
        for watch in &mut self.watching {
            if to != Some(watch.relay.task_id.as_str()) {
                watch.other_recipient = true;
            } else if message == Some(watch.relay.text.as_str()) {
                watch.forwarded = Some(id.to_string());
            } else {
                watch.altered = true;
            }
        }
    }

    /// The child stopped: every relay not settled failed.
    pub fn end(&mut self) -> Vec<RelayVerdict> {
        let ended = || RelayOutcome::Failed {
            reason: RelayFailure::Ended,
        };
        let watched = self.watching.drain(..).map(|watch| watch.relay);
        watched
            .chain(self.sent.drain(..))
            .map(|relay| verdict(relay, ended()))
            .collect()
    }
}

fn message_text(content: &MessageContent) -> Option<String> {
    match content {
        MessageContent::Text(text) => Some(text.clone()),
        MessageContent::Blocks(blocks) => {
            let texts: Vec<&str> = blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            (!texts.is_empty()).then(|| texts.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::replay::recorded_lines;

    const SLEEPER: &str = "toolu_012QcBdgY7ftdrny3oJtHMt5";
    const STOPPER: &str = "toolu_015G4gKRa122iYWaHqcCUYmB";
    const SLEEPER_TASK: &str = "a26cdafd895afd968";
    const CHANGE: &str = "Change of plan: when you finish, reply with the single word PINEAPPLE.";

    fn agents_lines() -> Vec<Inbound> {
        recorded_lines("agents").expect("agents recording")
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_596_400 + secs, 0).unwrap()
    }

    #[test]
    fn subagent_lines_route_to_their_agent_only() {
        let mut routed: HashMap<&str, Vec<&'static str>> = HashMap::new();
        let lines = agents_lines();
        for line in &lines {
            let kind = match line {
                Inbound::Assistant(_) => "assistant",
                Inbound::User(_) => "user",
                Inbound::StreamEvent(_) => "stream_event",
                _ => "other",
            };
            routed
                .entry(scope(line).unwrap_or("main"))
                .or_default()
                .push(kind);
        }
        assert_eq!(
            routed[SLEEPER],
            ["assistant", "user", "assistant", "assistant"]
        );
        assert_eq!(routed[STOPPER], ["assistant", "user", "user"]);
        assert_eq!(routed.len(), 3, "{:?}", routed.keys());
        let system = Inbound::System(System::Other(Default::default()));
        assert_eq!(scope(&system), None);
    }

    #[test]
    fn tasks_track_the_recorded_agents() {
        let mut tracker = AgentTracker::default();
        let lines = agents_lines();
        let mut states = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if tracker.on_line(line, at(index as i64)) {
                states.push(
                    tracker
                        .agents()
                        .iter()
                        .map(|agent| (agent.name.clone(), agent.state))
                        .collect::<Vec<_>>(),
                );
            }
        }
        assert!(states.contains(&vec![("sleeper".into(), AgentState::Running)]));
        let agents = tracker.agents();
        assert_eq!(agents.len(), 2);
        let sleeper = &agents[0];
        assert_eq!(sleeper.task_id, SLEEPER_TASK);
        assert_eq!(sleeper.tool_use_id, SLEEPER);
        assert_eq!(sleeper.subagent_type.as_deref(), Some("general-purpose"));
        assert_eq!(sleeper.parent_tool_use_id, None);
        assert_eq!(sleeper.state, AgentState::Completed);
        assert_eq!(sleeper.activity.as_deref(), Some("PINEAPPLE"));
        let cache = sleeper.cache.as_ref().expect("the sleeper made requests");
        assert_eq!(cache.ttl_secs, FIVE_MINUTE_TTL_SECS);
        assert!(sleeper.prompt_tokens > 20_000, "{}", sleeper.prompt_tokens);
        let stopper = &agents[1];
        assert_eq!(
            (stopper.name.as_str(), stopper.state),
            ("stopper", AgentState::Stopped)
        );
        assert_eq!(
            stopper.activity.as_deref(),
            Some("stopper"),
            "the stop notification's summary"
        );
        assert_eq!(
            tracker
                .find(SLEEPER_TASK)
                .map(|agent| agent.tool_use_id.as_str()),
            Some(SLEEPER)
        );
    }

    #[test]
    fn an_agent_started_by_an_agent_has_it_as_parent() {
        let mut tracker = AgentTracker::default();
        let lines: Vec<Inbound> = [
            serde_json::json!({"type": "system", "subtype": "task_started", "task_id": "a1",
                "tool_use_id": "t1", "task_type": "local_agent", "description": "outer",
                "session_id": "s", "uuid": "u1"}),
            serde_json::json!({"type": "assistant", "parent_tool_use_id": "t1",
                "session_id": "s", "uuid": "u2", "message": {"id": "m1", "content": [
                    {"type": "tool_use", "id": "t2", "name": "Agent",
                     "input": {"description": "inner", "prompt": "p"}}]}}),
            serde_json::json!({"type": "system", "subtype": "task_started", "task_id": "a2",
                "tool_use_id": "t2", "task_type": "local_agent",
                "session_id": "s", "uuid": "u3"}),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();
        for line in &lines {
            tracker.on_line(line, at(0));
        }
        let inner = tracker.find("a2").unwrap();
        assert_eq!(inner.name, "inner", "named from its Agent call");
        assert_eq!(inner.parent_tool_use_id.as_deref(), Some("t1"));
    }

    /// The recording's message reached the agent verbatim through SendMessage,
    /// but the main agent was not asked to acknowledge it.
    #[test]
    fn the_recorded_relay_was_forwarded_without_acknowledgement() {
        let mut filter = RelayFilter::default();
        filter.sent(Relay {
            seq: 7,
            task_id: SLEEPER_TASK.into(),
            agent_tool_use_id: SLEEPER.into(),
            text: CHANGE.into(),
            wrapped: CHANGE.into(),
        });
        let verdicts: Vec<RelayVerdict> = agents_lines()
            .iter()
            .flat_map(|line| filter.on_line(line))
            .collect();
        assert_eq!(
            verdicts,
            [RelayVerdict {
                relay_seq: 7,
                task_id: SLEEPER_TASK.into(),
                agent_tool_use_id: SLEEPER.into(),
                outcome: RelayOutcome::Failed {
                    reason: RelayFailure::NoAcknowledgement
                },
            }]
        );
        assert!(filter.end().is_empty());
    }

    fn user(text: &str) -> Inbound {
        serde_json::from_value(
            serde_json::json!({"type": "user", "parent_tool_use_id": null,
            "session_id": "s", "isReplay": true, "message": {"role": "user", "content": text}}),
        )
        .unwrap()
    }

    fn assistant(blocks: Value) -> Inbound {
        serde_json::from_value(
            serde_json::json!({"type": "assistant", "parent_tool_use_id": null,
            "session_id": "s", "uuid": "u", "message": {"id": "m", "content": blocks}}),
        )
        .unwrap()
    }

    fn send_message(id: &str, to: &str, message: &str) -> Inbound {
        assistant(
            serde_json::json!([{"type": "tool_use", "id": id, "name": "SendMessage",
            "input": {"to": to, "message": message, "summary": "relay"}}]),
        )
    }

    fn text(text: &str) -> Inbound {
        assistant(serde_json::json!([{"type": "text", "text": text}]))
    }

    /// "hello" for agent `a1`, written to the main agent as `wrapped`.
    fn hello(wrapped: &str) -> Relay {
        Relay {
            seq: 3,
            task_id: "a1".into(),
            agent_tool_use_id: "t1".into(),
            text: "hello".into(),
            wrapped: wrapped.into(),
        }
    }

    fn result() -> Inbound {
        serde_json::from_value(serde_json::json!({"type": "result", "subtype": "success",
            "is_error": false, "duration_ms": 1, "duration_api_ms": 1, "num_turns": 1,
            "stop_reason": null, "total_cost_usd": 0, "usage": {}, "session_id": "s",
            "uuid": "u"}))
        .unwrap()
    }

    fn outcome_of(lines: &[Inbound]) -> RelayOutcome {
        let wrapped = wrap("sleeper", "a1", "hello");
        let mut filter = RelayFilter::default();
        filter.sent(hello(&wrapped));
        assert!(filter.on_line(&user(&wrapped)).is_empty());
        let mut verdicts: Vec<RelayVerdict> =
            lines.iter().flat_map(|line| filter.on_line(line)).collect();
        verdicts.extend(filter.end());
        assert_eq!(verdicts.len(), 1, "{verdicts:?}");
        assert_eq!(verdicts[0].relay_seq, 3);
        verdicts.remove(0).outcome
    }

    #[test]
    fn a_relay_is_faithful_only_verbatim_to_its_agent_and_acknowledged() {
        let failed = |reason| RelayOutcome::Failed { reason };
        assert_eq!(
            outcome_of(&[
                send_message("s1", "a1", "hello"),
                text("  [Relay acknowledged]\n"),
                result()
            ]),
            RelayOutcome::Faithful {
                send_message: "s1".into()
            }
        );
        assert_eq!(
            outcome_of(&[
                send_message("s1", "a1", "hello!"),
                text(RELAY_ACK),
                result()
            ]),
            failed(RelayFailure::Altered)
        );
        assert_eq!(
            outcome_of(&[send_message("s1", "a2", "hello"), text(RELAY_ACK), result()]),
            failed(RelayFailure::OtherRecipient)
        );
        assert_eq!(
            outcome_of(&[
                send_message("s0", "a2", "other news"),
                send_message("s1", "a1", "hello"),
                text(RELAY_ACK),
                result()
            ]),
            RelayOutcome::Faithful {
                send_message: "s1".into()
            },
            "another SendMessage alongside does not fail it"
        );
        assert_eq!(
            outcome_of(&[text("Sure, done."), result()]),
            failed(RelayFailure::NotForwarded)
        );
        assert_eq!(
            outcome_of(&[
                send_message("s1", "a1", "hello"),
                text("Forwarded."),
                result()
            ]),
            failed(RelayFailure::NoAcknowledgement)
        );
        assert_eq!(
            outcome_of(&[send_message("s1", "a1", "hello")]),
            failed(RelayFailure::Ended)
        );
    }

    #[test]
    fn a_relay_not_echoed_yet_waits_for_its_own_turn() {
        let wrapped = wrap("sleeper", "a1", "hello");
        let mut filter = RelayFilter::default();
        filter.sent(hello(&wrapped));
        assert!(filter.on_line(&text(RELAY_ACK)).is_empty());
        assert!(
            filter.on_line(&result()).is_empty(),
            "a turn before the echo"
        );
        filter.on_line(&user(&wrapped));
        filter.on_line(&send_message("s1", "a1", "hello"));
        filter.on_line(&text(RELAY_ACK));
        let verdicts = filter.on_line(&result());
        assert_eq!(verdicts.len(), 1);
        assert!(matches!(verdicts[0].outcome, RelayOutcome::Faithful { .. }));
    }

    #[test]
    fn the_wrapped_message_names_the_agent_and_ends_with_the_text() {
        let wrapped = wrap("sleeper", "a1", "hello\nthere");
        assert!(wrapped.contains("agent \"sleeper\" (task a1)"), "{wrapped}");
        assert!(wrapped.contains("SendMessage to \"a1\""), "{wrapped}");
        assert!(wrapped.contains(RELAY_ACK), "{wrapped}");
        assert!(wrapped.ends_with("\n\nhello\nthere"), "{wrapped}");
    }
}
