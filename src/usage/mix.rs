//! Where a session's tokens went: model families and subagents.
//!
//! Counted from the `assistant` lines the CLI writes: one per content block,
//! each carrying its message's usage, so tokens are counted once per message
//! id. A line with a `parent_tool_use_id` comes from the subagent that tool use
//! spawned.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::usage::db::MessageTokens;

/// A model family, in the order a mix lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Opus,
    Sonnet,
    Haiku,
    Fable,
    Other,
}

const FAMILY_PATTERNS: [(&str, Family); 4] = [
    ("opus", Family::Opus),
    ("sonnet", Family::Sonnet),
    ("haiku", Family::Haiku),
    ("fable", Family::Fable),
];

impl Family {
    /// The family of a model id; `None` for the CLI's `<synthetic>` messages.
    pub fn of(model: &str) -> Option<Self> {
        if model.is_empty() || model.starts_with('<') {
            return None;
        }
        let model = model.to_ascii_lowercase();
        FAMILY_PATTERNS
            .iter()
            .find(|(needle, _)| model.contains(needle))
            .map(|(_, family)| *family)
            .or(Some(Family::Other))
    }

    pub fn name(self) -> &'static str {
        match self {
            Family::Opus => "Opus",
            Family::Sonnet => "Sonnet",
            Family::Haiku => "Haiku",
            Family::Fable => "Fable",
            Family::Other => "other",
        }
    }
}

/// Token shares across the session and what its subagents accounted for.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Mix {
    /// Share of session tokens per model family, summing to 1.
    pub shares: BTreeMap<Family, f64>,
    pub subagent_count: u32,
    /// Share of session tokens spent inside subagents.
    pub subagent_share: f64,
}

impl Mix {
    pub fn from_messages(messages: &[MessageTokens]) -> Self {
        let mut totals: BTreeMap<Family, u64> = BTreeMap::new();
        let mut subagents: BTreeSet<&str> = BTreeSet::new();
        let mut subagent_tokens = 0;
        let mut grand = 0;
        for message in messages {
            if let Some(parent) = &message.parent_tool_use_id {
                subagents.insert(parent);
            }
            let Some(family) = Family::of(&message.model) else {
                continue;
            };
            if message.tokens == 0 {
                continue;
            }
            *totals.entry(family).or_insert(0) += message.tokens;
            grand += message.tokens;
            if message.parent_tool_use_id.is_some() {
                subagent_tokens += message.tokens;
            }
        }
        let subagent_count = subagents.len() as u32;
        if grand == 0 {
            return Self {
                subagent_count,
                ..Self::default()
            };
        }
        Self {
            shares: totals
                .into_iter()
                .map(|(family, tokens)| (family, tokens as f64 / grand as f64))
                .collect(),
            subagent_count,
            subagent_share: subagent_tokens as f64 / grand as f64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Family, Mix};
    use crate::usage::db::MessageTokens;

    fn message(id: &str, model: &str, parent: Option<&str>, tokens: u64) -> MessageTokens {
        MessageTokens {
            message_id: id.into(),
            model: model.into(),
            parent_tool_use_id: parent.map(Into::into),
            tokens,
        }
    }

    #[test]
    fn model_names_map_to_families() {
        assert_eq!(Family::of("claude-opus-5-5"), Some(Family::Opus));
        assert_eq!(Family::of("claude-opus-5-5[1m]"), Some(Family::Opus));
        assert_eq!(Family::of("claude-sonnet-5"), Some(Family::Sonnet));
        assert_eq!(Family::of("claude-haiku-4-5-20251001"), Some(Family::Haiku));
        assert_eq!(Family::of("claude-fable-5-1"), Some(Family::Fable));
        assert_eq!(Family::of("some-other-model"), Some(Family::Other));
        assert_eq!(Family::of("<synthetic>"), None);
        assert_eq!(Family::of(""), None);
    }

    #[test]
    fn shares_split_by_family() {
        let mix = Mix::from_messages(&[
            message("m1", "claude-opus-5", None, 100),
            message("m2", "claude-haiku-4-5", None, 100),
        ]);
        assert_eq!(mix.shares.get(&Family::Opus), Some(&0.5));
        assert_eq!(mix.shares.get(&Family::Haiku), Some(&0.5));
        assert_eq!(mix.subagent_count, 0);
    }

    #[test]
    fn subagent_messages_count_toward_the_session() {
        let mix = Mix::from_messages(&[
            message("m1", "claude-opus-5", None, 300),
            message("s1", "claude-sonnet-5", Some("toolu_1"), 30),
            message("s2", "claude-sonnet-5", Some("toolu_1"), 20),
            message("s3", "claude-sonnet-5", Some("toolu_2"), 50),
        ]);
        assert_eq!(mix.subagent_count, 2);
        assert_eq!(mix.shares.get(&Family::Sonnet), Some(&0.25));
        assert_eq!(mix.subagent_share, 0.25);
    }

    #[test]
    fn unusable_messages_are_skipped_rather_than_counted() {
        let mix = Mix::from_messages(&[
            message("m1", "claude-opus-5", None, 10),
            message("m2", "<synthetic>", None, 10),
            message("m3", "claude-haiku-4-5", None, 0),
        ]);
        assert_eq!(mix.shares.len(), 1);
        assert_eq!(mix.shares.get(&Family::Opus), Some(&1.0));
    }

    #[test]
    fn a_session_without_tokens_reports_no_shares() {
        assert_eq!(Mix::from_messages(&[]), Mix::default());
    }
}
