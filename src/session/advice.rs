//! What a restart would keep of the current context, measured from the stored
//! history after each turn, and whether restarting is worthwhile against the
//! current context. See `notes/restart-advice.md`.

use serde::{Deserialize, Serialize};

use crate::claude::protocol::Usage;
use crate::session::gauges::{Context, FIVE_MINUTE_TTL_SECS, written_ttl};
use crate::session::seed::{Seed, SeedWindow};

/// Prices over the base input price.
const OUTPUT_PRICE: f64 = 5.0;
const ONE_HOUR_WRITE_PRICE: f64 = 2.0;
const FIVE_MINUTE_WRITE_PRICE: f64 = 1.25;
const CACHE_READ_PRICE: f64 = 0.1;
/// A restart is advised once at least this share of the context is conversation it drops.
pub const STALE_MIN_SHARE: f64 = 0.5;
/// A restart is advised when it pays back within this many messages.
pub const ADVICE_MAX_MESSAGES: u32 = 10;
/// Below this share of the window, older messages likely still serve the task.
pub const ADVICE_MIN_FILL: f64 = 0.4;
/// The context category holding the conversation itself, dropped by a restart.
const MESSAGES_CATEGORY: &str = "Messages";
/// Characters per token, for text no request counted on its own.
const CHARS_PER_TOKEN: usize = 4;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestartMeasure {
    /// Prompt tokens when the oldest exchange a seed carries started.
    pub window_start_tokens: u64,
    pub seed_tokens: u64,
    /// The files the seeded exchanges touched, which a fresh session reads again.
    pub reread_tokens: u64,
    /// Mean cost of the seeded exchanges' main calls besides cache reads, in base
    /// input tokens.
    pub other_cost_per_call: f64,
}

impl RestartMeasure {
    /// `None` before any main request.
    pub fn new(window: &SeedWindow, seed: &Seed) -> Option<Self> {
        let window_start_tokens = window.start_prompt?;
        let other_cost_per_call = if window.calls.is_empty() {
            0.0
        } else {
            window.calls.iter().map(other_cost).sum::<f64>() / window.calls.len() as f64
        };
        Some(Self {
            window_start_tokens,
            seed_tokens: (seed.text.len() / CHARS_PER_TOKEN) as u64,
            reread_tokens: (window.reread_chars / CHARS_PER_TOKEN) as u64,
            other_cost_per_call,
        })
    }
}

/// Why restarting is worthwhile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestartAdvice {
    /// Share of the context older than what a restart keeps, 0 to 1.
    pub stale_share: f64,
    /// Messages after which restarting first costs less.
    pub messages: u32,
    /// How much less each call costs after a restart.
    pub ratio: f64,
}

/// Why restarting is not worthwhile.
#[derive(Debug, Clone, PartialEq)]
pub enum NoAdvice {
    /// No context, measure or finished turn yet.
    Unmeasured,
    /// The context fills under `ADVICE_MIN_FILL` of the window.
    SmallContext {
        percentage: f64,
    },
    StaleShare {
        stale_share: f64,
    },
    /// The restarted context would be at least as large.
    NoSaving,
    SlowPayback {
        messages: u32,
    },
}

impl std::fmt::Display for NoAdvice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unmeasured => write!(formatter, "not measured yet, no turn finished"),
            Self::SmallContext { percentage } => write!(
                formatter,
                "the context fills {percentage:.0}% of the window, under {:.0}%: the conversation likely still serves the task",
                ADVICE_MIN_FILL * 100.0
            ),
            Self::StaleShare { stale_share } => write!(
                formatter,
                "only {:.0}% of the context is older than what a restart keeps, under {:.0}%",
                stale_share * 100.0,
                STALE_MIN_SHARE * 100.0
            ),
            Self::NoSaving => write!(formatter, "a restart would not make the context smaller"),
            Self::SlowPayback { messages } => write!(
                formatter,
                "a restart pays back only after {messages} messages, over {ADVICE_MAX_MESSAGES}"
            ),
        }
    }
}

/// Continuing reads the context `C` at the cache read price per call; a restart
/// writes the smaller `S` once, then reads it per call. It pays back after
/// `write * S / (read * (C - S))` calls.
pub fn restart_advice(
    context: Option<&Context>,
    calls_per_message: Option<f64>,
    measure: Option<&RestartMeasure>,
) -> Result<RestartAdvice, NoAdvice> {
    let (Some(context), Some(calls_per_message), Some(measure)) =
        (context, calls_per_message, measure)
    else {
        return Err(NoAdvice::Unmeasured);
    };
    if calls_per_message <= 0.0 || context.categories.is_empty() || context.used_tokens == 0 {
        return Err(NoAdvice::Unmeasured);
    }
    let current = context.used_tokens as f64;
    if current < ADVICE_MIN_FILL * context.max_tokens as f64 {
        return Err(NoAdvice::SmallContext {
            percentage: context.percentage,
        });
    }
    let fixed: u64 = context
        .categories
        .iter()
        .filter(|category| category.kind == "used" && category.name != MESSAGES_CATEGORY)
        .map(|category| category.tokens)
        .sum();
    let stale_share =
        ((measure.window_start_tokens as f64 - fixed as f64) / current).clamp(0.0, 1.0);
    if stale_share < STALE_MIN_SHARE {
        return Err(NoAdvice::StaleShare { stale_share });
    }
    let restarted = (fixed + measure.seed_tokens + measure.reread_tokens) as f64;
    let saved = current - restarted;
    if saved <= 0.0 {
        return Err(NoAdvice::NoSaving);
    }
    let calls = ONE_HOUR_WRITE_PRICE * restarted / (CACHE_READ_PRICE * saved);
    let messages = ((calls / calls_per_message).ceil() as u32).max(1);
    if messages > ADVICE_MAX_MESSAGES {
        return Err(NoAdvice::SlowPayback { messages });
    }
    let other = measure.other_cost_per_call;
    Ok(RestartAdvice {
        stale_share,
        messages,
        ratio: (CACHE_READ_PRICE * current + other) / (CACHE_READ_PRICE * restarted + other),
    })
}

/// A call's cost besides reading the cache: input, cache writes and output.
fn other_cost(usage: &Usage) -> f64 {
    let write_price = match written_ttl(usage) {
        Some(FIVE_MINUTE_TTL_SECS) => FIVE_MINUTE_WRITE_PRICE,
        _ => ONE_HOUR_WRITE_PRICE,
    };
    usage.input_tokens as f64
        + usage.cache_creation_input_tokens as f64 * write_price
        + usage.output_tokens as f64 * OUTPUT_PRICE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::protocol::ContextCategory;
    use crate::session::gauges::ContextSource;

    fn usage(value: serde_json::Value) -> Usage {
        serde_json::from_value(value).unwrap()
    }

    fn seed(text: &str) -> Seed {
        Seed {
            text: text.into(),
            exchanges: 1,
            total_exchanges: 1,
        }
    }

    #[test]
    fn a_call_costs_its_input_writes_and_output() {
        let one_hour = usage(serde_json::json!({"input_tokens": 10, "output_tokens": 100,
            "cache_read_input_tokens": 50_000, "cache_creation_input_tokens": 1_000,
            "cache_creation": {"ephemeral_1h_input_tokens": 1_000, "ephemeral_5m_input_tokens": 0}}));
        assert_eq!(other_cost(&one_hour), 10.0 + 2_000.0 + 500.0);
        let five_minutes = usage(serde_json::json!({"input_tokens": 0, "output_tokens": 0,
            "cache_creation_input_tokens": 1_000,
            "cache_creation": {"ephemeral_1h_input_tokens": 0, "ephemeral_5m_input_tokens": 1_000}}));
        assert_eq!(other_cost(&five_minutes), 1_250.0);
    }

    #[test]
    fn the_measure_averages_the_window_s_calls() {
        let call = |output: u64| usage(serde_json::json!({"output_tokens": output}));
        let window = SeedWindow {
            start_prompt: Some(90_000),
            reread_chars: 40_000,
            calls: vec![call(100), call(300)],
        };
        assert_eq!(
            RestartMeasure::new(&window, &seed(&"x".repeat(8_000))),
            Some(RestartMeasure {
                window_start_tokens: 90_000,
                seed_tokens: 2_000,
                reread_tokens: 10_000,
                other_cost_per_call: 1_000.0,
            })
        );
        let empty = SeedWindow {
            start_prompt: None,
            reread_chars: 0,
            calls: Vec::new(),
        };
        assert_eq!(RestartMeasure::new(&empty, &seed("")), None);
    }

    // 27k fixed, plus a 3k seed and 20k of files read again: a restart starts at 50k.
    fn context(used: u64, max: u64) -> Context {
        let category = |name: &str, tokens: u64, kind: &str| ContextCategory {
            name: name.into(),
            tokens,
            kind: kind.into(),
        };
        Context {
            categories: vec![
                category("System prompt", 7_000, "used"),
                category("System tools", 20_000, "used"),
                category("MCP tools", 50_000, "deferred"),
                category("Messages", used - 27_000, "used"),
                category("Free space", max - used, "free"),
            ],
            ..Context::new(used, max, ContextSource::Cli).unwrap()
        }
    }

    fn measure(window_start: u64, other: f64) -> RestartMeasure {
        RestartMeasure {
            window_start_tokens: window_start,
            seed_tokens: 3_000,
            reread_tokens: 20_000,
            other_cost_per_call: other,
        }
    }

    fn advice(
        context: &Context,
        calls: f64,
        measure: &RestartMeasure,
    ) -> Result<RestartAdvice, NoAdvice> {
        restart_advice(Some(context), Some(calls), Some(measure))
    }

    #[test]
    fn advice_counts_the_stale_share_the_payback_and_the_gain() {
        // 300k of 400k is older than the seed; 2 * 50k / (0.1 * 350k) = 2.9 calls.
        let full = context(400_000, 1_000_000);
        assert_eq!(
            advice(&full, 1.0, &measure(327_000, 0.0)),
            Ok(RestartAdvice {
                stale_share: 0.75,
                messages: 3,
                ratio: 8.0,
            })
        );
        assert_eq!(
            advice(&full, 5.0, &measure(327_000, 0.0)).unwrap().messages,
            1
        );
        // (40k + 5k) / (5k + 5k).
        assert_eq!(
            advice(&full, 1.0, &measure(327_000, 5_000.0))
                .unwrap()
                .ratio,
            4.5
        );
    }

    #[test]
    fn advice_needs_half_of_the_context_stale() {
        let full = context(400_000, 1_000_000);
        assert_eq!(
            advice(&full, 1.0, &measure(227_000, 0.0))
                .unwrap()
                .stale_share,
            0.5
        );
        assert!(matches!(
            advice(&full, 1.0, &measure(226_000, 0.0)),
            Err(NoAdvice::StaleShare { .. })
        ));
    }

    #[test]
    fn advice_needs_a_payback_within_ten_messages() {
        // 2 * 50k / (0.1 * 100k) = 10 calls.
        let half = context(150_000, 300_000);
        assert_eq!(
            advice(&half, 1.0, &measure(127_000, 0.0)).unwrap().messages,
            10
        );
        assert_eq!(
            advice(&context(140_000, 300_000), 1.0, &measure(117_000, 0.0)),
            Err(NoAdvice::SlowPayback { messages: 12 })
        );
    }

    #[test]
    fn advice_needs_forty_percent_of_the_window() {
        assert!(advice(&context(400_000, 1_000_000), 1.0, &measure(327_000, 0.0)).is_ok());
        assert!(matches!(
            advice(&context(399_000, 1_000_000), 1.0, &measure(326_000, 0.0)),
            Err(NoAdvice::SmallContext { .. })
        ));
    }

    #[test]
    fn advice_needs_its_figures() {
        let full = context(400_000, 1_000_000);
        let measured = measure(327_000, 0.0);
        assert_eq!(
            restart_advice(Some(&full), Some(1.0), None),
            Err(NoAdvice::Unmeasured)
        );
        assert_eq!(
            restart_advice(Some(&full), None, Some(&measured)),
            Err(NoAdvice::Unmeasured)
        );
        let uncategorized = Context {
            categories: Vec::new(),
            ..full
        };
        assert_eq!(
            advice(&uncategorized, 1.0, &measured),
            Err(NoAdvice::Unmeasured)
        );
    }
}
