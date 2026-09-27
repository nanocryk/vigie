//! The two rate-limit windows the account reports.

use crate::usage::units::{Pct, Timestamp};

/// A rate-limit window, identified by the period it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WindowKind {
    FiveHour,
    SevenDay,
}

impl WindowKind {
    pub const ALL: [WindowKind; 2] = [WindowKind::FiveHour, WindowKind::SevenDay];

    /// Label in stored rows.
    pub fn label(self) -> &'static str {
        match self {
            WindowKind::FiveHour => "5h",
            WindowKind::SevenDay => "7d",
        }
    }

    /// The window a CLI key names: `five_hour` or `seven_day`, as used by
    /// `get_usage` and `rate_limit_event`.
    pub fn from_cli(key: &str) -> Option<Self> {
        match key {
            "five_hour" => Some(WindowKind::FiveHour),
            "seven_day" => Some(WindowKind::SevenDay),
            _ => None,
        }
    }

    /// Full length of the window in seconds.
    pub fn length_sec(self) -> f64 {
        match self {
            WindowKind::FiveHour => 5.0 * 3600.0,
            WindowKind::SevenDay => 7.0 * 86400.0,
        }
    }

    /// Instant the window opened, where usage was zero by definition.
    pub fn started_at(self, resets_at: Timestamp) -> Timestamp {
        Timestamp::new(resets_at.get() - self.length_sec())
    }
}

/// What one reading says about one window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowState {
    pub used: Pct,
    pub resets_at: Timestamp,
}

/// One recorded usage reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub at: Timestamp,
    pub pct: Pct,
}
