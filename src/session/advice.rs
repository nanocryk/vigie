//! What a restart would keep of the current context, measured from the stored
//! history after each turn; clients weigh it against the context to advise a
//! restart. See `notes/restart-advice.md`.

use serde::{Deserialize, Serialize};

use crate::claude::protocol::Usage;
use crate::session::gauges::{FIVE_MINUTE_TTL_SECS, written_ttl};
use crate::session::seed::{Seed, SeedWindow};

/// Prices over the base input price.
const OUTPUT_PRICE: f64 = 5.0;
const ONE_HOUR_WRITE_PRICE: f64 = 2.0;
const FIVE_MINUTE_WRITE_PRICE: f64 = 1.25;
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
}
