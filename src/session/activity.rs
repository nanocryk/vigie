//! Whether a session is busy, derived from the lines it exchanges with its child.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::claude::protocol::{Inbound, System};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    #[default]
    Idle,
    Busy,
}

/// What the session is doing, republished on every change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveStatus {
    pub activity: Activity,
    /// A turn runs; background tasks alone do not count.
    pub turn: bool,
    /// Asks waiting for an answer.
    pub asks: usize,
}

/// Derives `Activity` from the lines the CLI sends and the messages the host sends.
///
/// A background task counts as running until its `task_notification`, which the
/// CLI follows with a turn of its own, so the session stays busy across that gap.
#[derive(Debug, Default)]
pub struct ActivityTracker {
    turn: bool,
    tasks: BTreeSet<String>,
}

impl ActivityTracker {
    pub fn on_send(&mut self) {
        self.turn = true;
    }

    pub fn on_line(&mut self, line: &Inbound) {
        match line {
            Inbound::System(System::Init(_)) => self.turn = true,
            Inbound::System(System::Status(status)) if status.status.is_some() => self.turn = true,
            Inbound::System(System::BackgroundTasksChanged(changed)) => self
                .tasks
                .extend(changed.tasks.iter().map(|task| task.task_id.clone())),
            Inbound::System(System::TaskStarted(started))
                if started.is_backgrounded == Some(true) =>
            {
                self.tasks.insert(started.task_id.clone());
            }
            Inbound::System(System::TaskNotification(notification)) => {
                if self.tasks.remove(&notification.task_id) {
                    self.turn = true;
                }
            }
            Inbound::Result(_) => self.turn = false,
            _ => {}
        }
    }

    pub fn turn(&self) -> bool {
        self.turn
    }

    pub fn activity(&self) -> Activity {
        if self.turn || !self.tasks.is_empty() {
            Activity::Busy
        } else {
            Activity::Idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::replay;

    fn recorded_lines(name: &str) -> Vec<Inbound> {
        replay::recorded_lines(name).expect("recording")
    }

    /// The background recording: a turn starts the task and ends, the task
    /// finishes later, and the CLI reports it in a turn of its own.
    #[test]
    fn background_task_keeps_the_session_busy_until_its_turn_ends() {
        let mut tracker = ActivityTracker::default();
        tracker.on_send();
        let mut results = 0;
        let mut after_first_result = Vec::new();
        for line in recorded_lines("background") {
            tracker.on_line(&line);
            if matches!(line, Inbound::Result(_)) {
                results += 1;
            }
            if results == 1 {
                after_first_result.push(tracker.activity());
            }
        }
        assert_eq!(results, 2);
        assert!(!after_first_result.is_empty());
        assert!(
            after_first_result
                .iter()
                .all(|activity| *activity == Activity::Busy)
        );
        assert_eq!(tracker.activity(), Activity::Idle);
    }

    #[test]
    fn a_plain_turn_goes_idle_on_result() {
        let mut tracker = ActivityTracker::default();
        assert_eq!(tracker.activity(), Activity::Idle);
        let mut seen_busy = false;
        for line in recorded_lines("basic") {
            tracker.on_line(&line);
            seen_busy |= tracker.activity() == Activity::Busy;
            if matches!(line, Inbound::Result(_)) {
                assert_eq!(tracker.activity(), Activity::Idle);
            }
        }
        assert!(seen_busy);
    }
}
