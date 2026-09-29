//! The plan an `ExitPlanMode` ask submits for review, and the text the same plan
//! file showed at its previous review.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::grant;
use crate::claude::protocol::{CanUseTool, ContentBlock, Inbound};
use crate::claude::tools;
use crate::store::Store;

/// What a plan approval shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanReview {
    /// The plan file, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub text: String,
    /// The text the previous review of `path` showed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
}

/// Follows the plan file the main agent last wrote or edited.
#[derive(Debug)]
pub struct PlanTracker {
    /// Where the CLI keeps plan files; `None` without a home folder.
    plans_dir: Option<String>,
    last_file: Option<String>,
}

impl PlanTracker {
    /// Plan files in `$CLAUDE_CONFIG_DIR/plans`, else `~/.claude/plans`.
    pub fn from_env() -> Self {
        let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".claude")));
        Self::in_dir(config_dir.map(|dir| dir.join("plans").to_string_lossy().into_owned()))
    }

    fn in_dir(plans_dir: Option<String>) -> Self {
        Self {
            plans_dir,
            last_file: None,
        }
    }

    pub fn on_line(&mut self, line: &Inbound) {
        let Inbound::Assistant(message) = line else {
            return;
        };
        if message.parent_tool_use_id.is_some() {
            return;
        }
        let Some(plans_dir) = &self.plans_dir else {
            return;
        };
        for block in &message.message.content {
            if let ContentBlock::ToolUse { name, input, .. } = block
                && tools::is_edit(name)
                && let Some(file) = input.get("file_path").and_then(Value::as_str)
                && grant::directly_in(file, plans_dir)
            {
                self.last_file = Some(file.to_string());
            }
        }
    }

    /// The plan of an `ExitPlanMode` ask: its input's `plan`, else the last plan
    /// file read from disk. Records the text as the file's latest review.
    pub async fn review(&self, ask: &CanUseTool, history: Option<&Store>) -> Option<PlanReview> {
        let (path, text) = match ask.input.get("plan").and_then(Value::as_str) {
            Some(plan) => {
                let path = ask.input.get("planFilePath").and_then(Value::as_str);
                (path.map(str::to_owned), plan.to_string())
            }
            None => {
                let file = self.last_file.clone()?;
                let text = match tokio::fs::read_to_string(&file).await {
                    Ok(text) => text,
                    Err(err) => {
                        tracing::warn!("read plan {file}: {err}");
                        return None;
                    }
                };
                (Some(file), text)
            }
        };
        let previous = match (&path, history) {
            (Some(path), Some(history)) => history
                .swap_plan(path.clone(), text.clone())
                .await
                .unwrap_or_else(|err| {
                    tracing::warn!("swap plan {path}: {err:#}");
                    None
                }),
            _ => None,
        };
        Some(PlanReview {
            path,
            text,
            previous,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn assistant_write(file: &str, parent: Option<&str>) -> Inbound {
        serde_json::from_value(json!({
            "type": "assistant",
            "parent_tool_use_id": parent,
            "session_id": "s", "uuid": "u",
            "message": {"id": "m", "content": [
                {"type": "tool_use", "id": "t", "name": "Write",
                 "input": {"file_path": file, "content": "# Plan"}}
            ]}
        }))
        .unwrap()
    }

    fn exit_plan(input: Value) -> CanUseTool {
        serde_json::from_value(json!({
            "tool_name": "ExitPlanMode", "input": input, "tool_use_id": "t"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn an_empty_input_reads_the_last_plan_file_and_diffs_against_its_last_review() {
        let dir = tempfile::tempdir().unwrap();
        let plans = dir.path().join("plans");
        std::fs::create_dir(&plans).unwrap();
        let file = plans.join("tidy-aurora.md");
        let file_text = file.to_string_lossy().into_owned();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let mut tracker = PlanTracker::in_dir(Some(plans.to_string_lossy().into_owned()));

        assert_eq!(
            tracker.review(&exit_plan(json!({})), Some(&store)).await,
            None
        );

        tracker.on_line(&assistant_write(
            &dir.path().join("elsewhere.md").to_string_lossy(),
            None,
        ));
        tracker.on_line(&assistant_write(&file_text, Some("agent")));
        assert_eq!(
            tracker.last_file, None,
            "other folders and subagents are ignored"
        );

        tracker.on_line(&assistant_write(&file_text, None));
        std::fs::write(&file, "# One").unwrap();
        assert_eq!(
            tracker.review(&exit_plan(json!({})), Some(&store)).await,
            Some(PlanReview {
                path: Some(file_text.clone()),
                text: "# One".into(),
                previous: None,
            })
        );
        std::fs::write(&file, "# Two").unwrap();
        assert_eq!(
            tracker.review(&exit_plan(json!({})), Some(&store)).await,
            Some(PlanReview {
                path: Some(file_text),
                text: "# Two".into(),
                previous: Some("# One".into()),
            })
        );
    }

    #[tokio::test]
    async fn an_input_plan_wins_over_the_file() {
        let tracker = PlanTracker::in_dir(None);
        let ask = exit_plan(json!({"plan": "# Inline", "planFilePath": "C:\\p\\a.md"}));
        assert_eq!(
            tracker.review(&ask, None).await,
            Some(PlanReview {
                path: Some("C:\\p\\a.md".into()),
                text: "# Inline".into(),
                previous: None,
            })
        );
    }
}
