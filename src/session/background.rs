//! Background tasks of the running child, such as commands run in the
//! background, as `background_tasks_changed` lists them. Subagents are left to
//! `agents`, which shows them already.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::agents::LOCAL_AGENT;
use crate::claude::protocol::{Inbound, System};

/// A background task as `GET /api/sessions` lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackgroundView {
    pub task_id: String,
    /// Such as `local_bash`.
    pub task_type: Option<String>,
    pub description: Option<String>,
    /// When it first showed in a list.
    pub started_at: DateTime<Utc>,
}

/// Folds CLI lines into the running background tasks, oldest first.
#[derive(Debug, Default)]
pub struct BackgroundTracker {
    tasks: Vec<BackgroundView>,
}

impl BackgroundTracker {
    pub fn tasks(&self) -> &[BackgroundView] {
        &self.tasks
    }

    /// True when the list changed. `now` is the line's arrival.
    pub fn on_line(&mut self, line: &Inbound, now: DateTime<Utc>) -> bool {
        match line {
            // Each list is complete: tasks missing from it ended.
            Inbound::System(System::BackgroundTasksChanged(changed)) => {
                let next: Vec<BackgroundView> = changed
                    .tasks
                    .iter()
                    .filter(|task| task.task_type.as_deref() != Some(LOCAL_AGENT))
                    .map(|task| BackgroundView {
                        task_id: task.task_id.clone(),
                        task_type: task.task_type.clone(),
                        description: task.description.clone(),
                        started_at: self
                            .tasks
                            .iter()
                            .find(|known| known.task_id == task.task_id)
                            .map_or(now, |known| known.started_at),
                    })
                    .collect();
                let changed = next != self.tasks;
                self.tasks = next;
                changed
            }
            Inbound::System(System::TaskNotification(notification)) => {
                let before = self.tasks.len();
                self.tasks
                    .retain(|task| task.task_id != notification.task_id);
                self.tasks.len() != before
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(json: serde_json::Value) -> Inbound {
        serde_json::from_value(json).expect("inbound line")
    }

    fn changed(tasks: serde_json::Value) -> Inbound {
        line(serde_json::json!({
            "type": "system", "subtype": "background_tasks_changed",
            "tasks": tasks, "uuid": "u", "session_id": "s",
        }))
    }

    #[test]
    fn follows_the_listed_tasks_and_leaves_agents_out() {
        let mut tracker = BackgroundTracker::default();
        let start = Utc::now();
        assert!(tracker.on_line(
            &changed(serde_json::json!([
                {"task_id": "b1", "task_type": "local_bash", "description": "Wait 8 seconds"},
                {"task_id": "a1", "task_type": "local_agent", "description": "explore"},
            ])),
            start,
        ));
        let ids: Vec<&str> = tracker
            .tasks()
            .iter()
            .map(|task| task.task_id.as_str())
            .collect();
        assert_eq!(ids, ["b1"]);

        let later = start + chrono::Duration::seconds(5);
        assert!(!tracker.on_line(
            &changed(serde_json::json!([
                {"task_id": "b1", "task_type": "local_bash", "description": "Wait 8 seconds"},
            ])),
            later,
        ));
        assert_eq!(
            tracker.tasks()[0].started_at,
            start,
            "a listed task keeps its start"
        );

        assert!(tracker.on_line(&changed(serde_json::json!([])), later));
        assert!(tracker.tasks().is_empty());
    }

    #[test]
    fn a_notification_ends_its_task() {
        let mut tracker = BackgroundTracker::default();
        tracker.on_line(
            &changed(serde_json::json!([{"task_id": "b1", "task_type": "local_bash"}])),
            Utc::now(),
        );
        let done = line(serde_json::json!({
            "type": "system", "subtype": "task_notification", "task_id": "b1",
            "status": "completed", "uuid": "u", "session_id": "s",
        }));
        assert!(tracker.on_line(&done, Utc::now()));
        assert!(tracker.tasks().is_empty());
    }
}
