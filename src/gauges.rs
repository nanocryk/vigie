//! The prompt cache and the context of a session's main conversation, read from
//! its CLI lines.
//!
//! Each main-conversation request refreshes the cache for the TTL its write
//! used: `usage.cache_creation` splits the written tokens between the 1 hour and
//! the 5 minute TTLs. A request that wrote nothing keeps the last known TTL.
//! The context is the prompt of the newest main request (input, cache read and
//! cache creation tokens) over the model's `contextWindow` from the `result`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::protocol::{AssistantMessage, ResultMessage, Usage};

pub const ONE_HOUR_TTL_SECS: u32 = 60 * 60;
pub const FIVE_MINUTE_TTL_SECS: u32 = 5 * 60;

/// Key of `modelUsage.<model>` naming the model the entry belongs to.
const CANONICAL_MODEL: &str = "canonicalModel";

/// A request of the main conversation, as its `assistant` line reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainRequest {
    /// Shared by the lines of one response, one per content block.
    pub message_id: String,
    pub model: Option<String>,
    /// `None` when the response wrote nothing to the cache.
    pub ttl_secs: Option<u32>,
    pub prompt_tokens: u64,
}

impl MainRequest {
    /// `None` for subagent lines and for lines no API request stands behind
    /// (the CLI's own synthetic messages report no tokens).
    pub fn from_assistant(line: &AssistantMessage) -> Option<Self> {
        if line.parent_tool_use_id.is_some() {
            return None;
        }
        let usage: Usage = serde_json::from_value(line.message.extra.get("usage")?.clone()).ok()?;
        let prompt_tokens =
            usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens;
        if prompt_tokens == 0 {
            return None;
        }
        Some(Self {
            message_id: line.message.id.clone()?,
            model: line.message.model.clone(),
            ttl_secs: written_ttl(&usage),
            prompt_tokens,
        })
    }
}

/// The TTL a response's cache write used; the longer one when it used both.
pub fn written_ttl(usage: &Usage) -> Option<u32> {
    let creation = usage.cache_creation.as_ref()?;
    if creation.ephemeral_1h_input_tokens > 0 {
        Some(ONE_HOUR_TTL_SECS)
    } else if creation.ephemeral_5m_input_tokens > 0 {
        Some(FIVE_MINUTE_TTL_SECS)
    } else {
        None
    }
}

/// The context window of each model a `result` line reports on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Windows {
    entries: Vec<ModelWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelWindow {
    /// Key of `modelUsage`, e.g. `claude-opus-5-5[1m]`.
    key: String,
    canonical: Option<String>,
    tokens: u64,
}

impl Windows {
    pub fn from_result(result: &ResultMessage) -> Self {
        Self {
            entries: result
                .model_usage
                .iter()
                .filter_map(|(key, usage)| {
                    Some(ModelWindow {
                        key: key.clone(),
                        canonical: usage
                            .extra
                            .get(CANONICAL_MODEL)
                            .and_then(|value| value.as_str())
                            .map(str::to_owned),
                        tokens: usage.context_window.filter(|tokens| *tokens > 0)?,
                    })
                })
                .collect(),
        }
    }

    /// The window of `model` (as `assistant` lines name it), else the largest
    /// reported: the main conversation runs on the largest model of a turn.
    pub fn for_model(&self, model: Option<&str>) -> Option<u64> {
        let matching = model.and_then(|model| {
            self.entries.iter().find(|entry| {
                entry.canonical.as_deref() == Some(model)
                    || entry.key == model
                    || entry
                        .key
                        .strip_prefix(model)
                        .is_some_and(|rest| rest.starts_with('['))
            })
        });
        matching
            .or_else(|| self.entries.iter().max_by_key(|entry| entry.tokens))
            .map(|entry| entry.tokens)
    }
}

/// The prompt cache of the main conversation; it is cold `ttl_secs` after
/// `last_request_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cache {
    pub last_request_at: DateTime<Utc>,
    pub ttl_secs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    /// Reported by the CLI's `get_context_usage`.
    Cli,
    /// The newest main prompt over the model's context window.
    Estimate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Context {
    pub used_tokens: u64,
    pub max_tokens: u64,
    pub percentage: f64,
    pub source: ContextSource,
}

impl Context {
    pub fn estimate(used_tokens: u64, max_tokens: u64) -> Option<Self> {
        (max_tokens > 0).then(|| Self {
            used_tokens,
            max_tokens,
            percentage: used_tokens as f64 * 100.0 / max_tokens as f64,
            source: ContextSource::Estimate,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::protocol::Inbound;
    use crate::replay::{Direction, Recording};

    const RECORDINGS: [&str; 6] = [
        "basic",
        "background",
        "cd",
        "interrupt",
        "permissions",
        "plan",
    ];

    fn lines(name: &str) -> Vec<Inbound> {
        Recording::load(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("spike/runs")
                .join(format!("{name}.ndjson")),
        )
        .expect("load recording")
        .steps
        .into_iter()
        .filter(|step| step.dir == Direction::Out)
        .map(|step| serde_json::from_value(step.msg).expect("typed line"))
        .collect()
    }

    fn requests(name: &str) -> Vec<MainRequest> {
        lines(name)
            .iter()
            .filter_map(|line| match line {
                Inbound::Assistant(message) => MainRequest::from_assistant(message),
                _ => None,
            })
            .collect()
    }

    fn assistant(value: serde_json::Value) -> AssistantMessage {
        serde_json::from_value(value).expect("assistant line")
    }

    #[test]
    fn every_recorded_main_request_writes_with_the_one_hour_ttl() {
        for name in RECORDINGS {
            let requests = requests(name);
            assert!(!requests.is_empty(), "{name} has no main request");
            for request in &requests {
                assert_eq!(request.ttl_secs, Some(ONE_HOUR_TTL_SECS), "{name}");
                assert_eq!(request.model.as_deref(), Some("claude-opus-5-5"), "{name}");
            }
        }
    }

    #[test]
    fn a_synthetic_message_is_not_a_request() {
        let cd = requests("cd");
        assert!(
            cd.iter()
                .all(|request| request.message_id.starts_with("msg_"))
        );
        assert_eq!(cd.len(), 3);
    }

    #[test]
    fn the_prompt_counts_input_and_both_cache_parts() {
        let basic = requests("basic");
        assert_eq!(basic.len(), 2);
        // input 2, cache read 13397, cache creation 15607.
        assert_eq!(basic[0].prompt_tokens, 29_006);
        // input 2, cache read 29004, cache creation 1979.
        assert_eq!(basic[1].prompt_tokens, 30_985);
    }

    #[test]
    fn a_five_minute_write_and_an_empty_write() {
        let line = |creation: serde_json::Value| {
            assistant(serde_json::json!({
                "type": "assistant", "parent_tool_use_id": null, "session_id": "s", "uuid": "u",
                "message": {"id": "msg_1", "model": "claude-opus-5-5", "content": [],
                    "usage": {"input_tokens": 3, "cache_read_input_tokens": 10,
                        "cache_creation_input_tokens": 7, "cache_creation": creation}}}))
        };
        let five = MainRequest::from_assistant(&line(serde_json::json!(
            {"ephemeral_1h_input_tokens": 0, "ephemeral_5m_input_tokens": 7}
        )))
        .unwrap();
        assert_eq!(five.ttl_secs, Some(FIVE_MINUTE_TTL_SECS));
        let none = MainRequest::from_assistant(&line(serde_json::json!(
            {"ephemeral_1h_input_tokens": 0, "ephemeral_5m_input_tokens": 0}
        )))
        .unwrap();
        assert_eq!(none.ttl_secs, None);
        assert_eq!(none.prompt_tokens, 20);
    }

    #[test]
    fn a_subagent_request_is_not_the_main_conversation() {
        let line = assistant(serde_json::json!({
            "type": "assistant", "parent_tool_use_id": "toolu_1", "session_id": "s", "uuid": "u",
            "message": {"id": "msg_1", "model": "claude-haiku-5", "content": [],
                "usage": {"input_tokens": 3, "cache_creation": {"ephemeral_5m_input_tokens": 3}}}}));
        assert_eq!(MainRequest::from_assistant(&line), None);
    }

    #[test]
    fn results_report_the_main_model_s_window() {
        for name in RECORDINGS {
            let windows: Vec<Option<u64>> = lines(name)
                .iter()
                .filter_map(|line| match line {
                    Inbound::Result(result) => {
                        Some(Windows::from_result(result).for_model(Some("claude-opus-5-5")))
                    }
                    _ => None,
                })
                .collect();
            assert!(windows.contains(&Some(1_000_000)), "{name}: {windows:?}");
            // Interrupted turns report an empty `modelUsage`.
            assert!(
                windows
                    .iter()
                    .all(|window| matches!(window, None | Some(1_000_000))),
                "{name}: {windows:?}"
            );
        }
    }

    #[test]
    fn the_window_matches_the_model_else_the_largest() {
        let result: ResultMessage = serde_json::from_value(serde_json::json!({
            "type": "result", "subtype": "success", "is_error": false, "duration_ms": 1,
            "duration_api_ms": 1, "num_turns": 1, "stop_reason": null, "total_cost_usd": 0,
            "usage": {}, "session_id": "s", "uuid": "u",
            "modelUsage": {
                "claude-haiku-5": {"contextWindow": 200000},
                "claude-opus-5-5[1m]": {"contextWindow": 1000000},
                "claude-sonnet-5": {"contextWindow": 400000, "canonicalModel": "sonnet-x"}
            }
        }))
        .unwrap();
        let windows = Windows::from_result(&result);
        assert_eq!(windows.for_model(Some("claude-haiku-5")), Some(200_000));
        assert_eq!(windows.for_model(Some("claude-opus-5-5")), Some(1_000_000));
        assert_eq!(windows.for_model(Some("sonnet-x")), Some(400_000));
        assert_eq!(windows.for_model(Some("claude-opus")), Some(1_000_000));
        assert_eq!(windows.for_model(None), Some(1_000_000));
        assert_eq!(Windows::default().for_model(None), None);
    }

    #[test]
    fn the_estimate_is_a_percentage_of_the_window() {
        let context = Context::estimate(30_985, 1_000_000).unwrap();
        assert!((context.percentage - 3.0985).abs() < 1e-9);
        assert_eq!(context.source, ContextSource::Estimate);
        assert_eq!(Context::estimate(1, 0), None);
    }
}
