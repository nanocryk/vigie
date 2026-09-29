//! What the CLI reports, turned into readings and per-message tokens.
//!
//! `get_usage` gives utilization in percent with ISO reset instants;
//! `rate_limit_event` gives it as a fraction with epoch reset instants. Both
//! become percent readings keyed by a reset instant rounded to the minute, since
//! `get_usage` stamps the same reset with a varying sub-second part.

use chrono::DateTime;
use serde_json::Number;

use crate::claude::protocol::{AssistantMessage, GetUsageResponse, RateLimitInfo};
use crate::usage::db::MessageTokens;
use crate::usage::units::{Pct, Timestamp};
use crate::usage::window::{WindowKind, WindowState};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    pub kind: WindowKind,
    pub state: WindowState,
}

/// The windows a `get_usage` response reports.
pub fn from_get_usage(response: &GetUsageResponse) -> Vec<Reading> {
    let Some(limits) = &response.rate_limits else {
        return Vec::new();
    };
    [
        (WindowKind::FiveHour, &limits.five_hour),
        (WindowKind::SevenDay, &limits.seven_day),
    ]
    .into_iter()
    .filter_map(|(kind, window)| {
        let window = window.as_ref()?;
        let percent = window.utilization.as_ref().and_then(Number::as_f64)?;
        let resets_at = DateTime::parse_from_rfc3339(window.resets_at.as_deref()?).ok()?;
        Some(reading(
            kind,
            percent,
            resets_at.timestamp_micros() as f64 / 1_000_000.0,
        ))
    })
    .collect()
}

/// The windows a `rate_limit_event` reports: every entry of `unifiedWindows`,
/// or the event's own window when it carries none.
pub fn from_rate_limit_event(info: &RateLimitInfo) -> Vec<Reading> {
    if let Some(windows) = &info.unified_windows {
        return windows
            .iter()
            .filter_map(|(key, window)| {
                let kind = WindowKind::from_cli(key)?;
                let fraction = window.utilization.as_ref().and_then(Number::as_f64)?;
                Some(reading(kind, fraction * 100.0, window.resets_at? as f64))
            })
            .collect();
    }
    let own = || {
        let kind = WindowKind::from_cli(info.rate_limit_type.as_deref()?)?;
        let fraction = info.utilization.as_ref().and_then(Number::as_f64)?;
        Some(reading(kind, fraction * 100.0, info.resets_at? as f64))
    };
    own().into_iter().collect()
}

/// Tokens an `assistant` line reports for its message, when it names one.
pub fn from_assistant(line: &AssistantMessage) -> Option<MessageTokens> {
    let usage = line.message.usage.as_ref()?;
    Some(MessageTokens {
        message_id: line.message.id.clone()?,
        model: line.message.model.clone()?,
        parent_tool_use_id: line.parent_tool_use_id.clone(),
        tokens: usage.input_tokens
            + usage.output_tokens
            + usage.cache_read_input_tokens
            + usage.cache_creation_input_tokens,
    })
}

fn reading(kind: WindowKind, percent: f64, resets_at: f64) -> Reading {
    Reading {
        kind,
        state: WindowState {
            used: Pct::new((percent * 1e6).round() / 1e6),
            resets_at: Timestamp::new((resets_at / 60.0).round() * 60.0),
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{Reading, from_assistant, from_get_usage, from_rate_limit_event};
    use crate::claude::protocol::{GetUsageResponse, Inbound};
    use crate::claude::replay::{self, Recording};
    use crate::usage::mix::{Family, Mix};
    use crate::usage::window::WindowKind;

    /// 2026-09-27T13:00:00Z and 2026-09-28T16:00:00Z, the resets in the recordings.
    const FIVE_HOUR_RESET: f64 = 1_790_514_000.0;
    const SEVEN_DAY_RESET: f64 = 1_790_611_200.0;

    fn lines(name: &str) -> Vec<Inbound> {
        replay::recorded_lines(name).expect("recording")
    }

    fn summary(readings: &[Reading]) -> Vec<(WindowKind, f64, f64)> {
        readings
            .iter()
            .map(|reading| {
                (
                    reading.kind,
                    reading.state.used.get(),
                    reading.state.resets_at.get(),
                )
            })
            .collect()
    }

    #[test]
    fn a_rate_limit_event_reports_both_windows_in_percent() {
        let readings: Vec<Reading> = lines("basic")
            .iter()
            .filter_map(|line| match line {
                Inbound::RateLimitEvent(event) => Some(&event.rate_limit_info),
                _ => None,
            })
            .flat_map(from_rate_limit_event)
            .collect();
        assert_eq!(
            summary(&readings),
            [
                (WindowKind::FiveHour, 7.0, FIVE_HOUR_RESET),
                (WindowKind::SevenDay, 51.0, SEVEN_DAY_RESET),
            ]
        );
    }

    #[test]
    fn every_recording_reports_its_windows() {
        for name in ["background", "cd", "interrupt", "permissions", "plan"] {
            let events: Vec<Reading> = lines(name)
                .iter()
                .filter_map(|line| match line {
                    Inbound::RateLimitEvent(event) => Some(&event.rate_limit_info),
                    _ => None,
                })
                .flat_map(from_rate_limit_event)
                .collect();
            assert!(!events.is_empty(), "{name} has no reading");
            for reading in &events {
                let used = reading.state.used.get();
                assert!((0.0..=100.0).contains(&used), "{name}: {used}%");
                assert_eq!(reading.state.resets_at.get() % 60.0, 0.0);
            }
        }
    }

    #[test]
    fn a_get_usage_answer_agrees_with_the_event_of_the_same_moment() {
        let recording = Recording::named("basic").expect("recording");
        let response: Value = recording
            .out()
            .find(|msg| msg["response"]["response"]["rate_limits_available"] == true)
            .map(|msg| msg["response"]["response"].clone())
            .expect("a get_usage answer");
        let response: GetUsageResponse = serde_json::from_value(response).expect("typed answer");
        assert_eq!(
            summary(&from_get_usage(&response)),
            [
                (WindowKind::FiveHour, 7.0, FIVE_HOUR_RESET),
                (WindowKind::SevenDay, 51.0, SEVEN_DAY_RESET),
            ]
        );
    }

    #[test]
    fn a_rate_limit_event_without_unified_windows_reports_its_own() {
        let info = serde_json::from_value(serde_json::json!({
            "status": "allowed_warning", "resetsAt": 1_790_514_000u64,
            "rateLimitType": "seven_day", "utilization": 0.9}))
        .expect("info");
        assert_eq!(
            summary(&from_rate_limit_event(&info)),
            [(WindowKind::SevenDay, 90.0, FIVE_HOUR_RESET)]
        );
        let other = serde_json::from_value(serde_json::json!({
            "status": "allowed", "resetsAt": 1_790_514_000u64,
            "rateLimitType": "seven_day_opus", "utilization": 0.9}))
        .expect("info");
        assert!(from_rate_limit_event(&other).is_empty());
    }

    #[test]
    fn an_unavailable_get_usage_reports_nothing() {
        let response: GetUsageResponse =
            serde_json::from_value(serde_json::json!({"rate_limits_available": false}))
                .expect("typed answer");
        assert!(from_get_usage(&response).is_empty());
    }

    #[test]
    fn assistant_lines_carry_each_message_s_tokens() {
        let messages: Vec<_> = lines("basic")
            .iter()
            .filter_map(|line| match line {
                Inbound::Assistant(message) => from_assistant(message),
                _ => None,
            })
            .collect();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].model, "claude-opus-5-5");
        assert_eq!(messages[0].parent_tool_use_id, None);
        // input 2, cache creation 15607, cache read 13397, output 4.
        assert_eq!(messages[0].tokens, 29_010);
    }

    #[test]
    fn a_recorded_session_mixes_its_models() {
        for name in ["background", "cd", "permissions", "plan"] {
            let messages: Vec<_> = lines(name)
                .iter()
                .filter_map(|line| match line {
                    Inbound::Assistant(message) => from_assistant(message),
                    _ => None,
                })
                .collect();
            assert!(!messages.is_empty(), "{name} has no message");
            let mix = Mix::from_messages(&messages);
            assert_eq!(mix.shares.get(&Family::Opus), Some(&1.0), "{name}");
            assert_eq!(mix.subagent_count, 0, "{name}");
        }
    }
}
