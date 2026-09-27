//! Newtypes for the quantities the usage module deals in.
//!
//! Percentages, instants and the estimator's rates are all bare `f64` at the
//! arithmetic level; wrapping them keeps a rate from being multiplied by the
//! wrong kind of time.

use chrono::{DateTime, Utc};

/// Percentage of a rate-limit window's budget.
///
/// Observed values sit in 0..100; projections can exceed 100.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Pct(f64);

impl Pct {
    pub fn new(value: f64) -> Self {
        Self(value)
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

/// Unix timestamp in seconds.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Timestamp(f64);

impl Timestamp {
    pub fn new(seconds: f64) -> Self {
        Self(seconds)
    }

    pub fn from_datetime(time: DateTime<Utc>) -> Self {
        Self(time.timestamp_micros() as f64 / 1_000_000.0)
    }

    pub fn get(self) -> f64 {
        self.0
    }

    /// `None` outside the range chrono represents.
    pub fn to_datetime(self) -> Option<DateTime<Utc>> {
        DateTime::from_timestamp_micros((self.0 * 1_000_000.0).round() as i64)
    }

    /// Seconds from `earlier` to `self`, negative when `self` is in the past.
    pub fn seconds_since(self, earlier: Self) -> f64 {
        self.0 - earlier.0
    }

    pub fn later_of(self, other: Self) -> Self {
        Self(self.0.max(other.0))
    }
}

/// Hours spent working, as opposed to hours on the clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, PartialOrd)]
pub struct ActiveHours(f64);

impl ActiveHours {
    pub fn new(hours: f64) -> Self {
        Self(hours.max(0.0))
    }

    pub fn get(self) -> f64 {
        self.0
    }

    pub fn at_least(self, floor: f64) -> Self {
        Self(self.0.max(floor))
    }
}

/// Percent of a window's budget consumed per active hour: the intensity of
/// the work, with idle time already factored out.
#[derive(Debug, Clone, Copy, Default, PartialEq, PartialOrd)]
pub struct PctPerActiveHour(f64);

impl PctPerActiveHour {
    pub fn new(rate: f64) -> Self {
        Self(rate.max(0.0))
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl std::ops::Add for Pct {
    type Output = Pct;

    fn add(self, other: Pct) -> Pct {
        Pct(self.0 + other.0)
    }
}

impl std::ops::Sub for Pct {
    type Output = Pct;

    fn sub(self, other: Pct) -> Pct {
        Pct(self.0 - other.0)
    }
}

impl std::ops::Add for ActiveHours {
    type Output = ActiveHours;

    fn add(self, other: ActiveHours) -> ActiveHours {
        ActiveHours(self.0 + other.0)
    }
}

/// Working for a stretch of active hours at a given intensity consumes budget.
impl std::ops::Mul<ActiveHours> for PctPerActiveHour {
    type Output = Pct;

    fn mul(self, hours: ActiveHours) -> Pct {
        Pct(self.0 * hours.0)
    }
}

/// Budget spread over the active hours that consumed it is an intensity.
impl std::ops::Div<ActiveHours> for Pct {
    type Output = PctPerActiveHour;

    fn div(self, hours: ActiveHours) -> PctPerActiveHour {
        PctPerActiveHour(if hours.0 > 0.0 { self.0 / hours.0 } else { 0.0 })
    }
}

/// Budget spent at a given intensity buys a stretch of active hours.
impl std::ops::Div<PctPerActiveHour> for Pct {
    type Output = ActiveHours;

    fn div(self, rate: PctPerActiveHour) -> ActiveHours {
        ActiveHours::new(if rate.0 > 0.0 { self.0 / rate.0 } else { 0.0 })
    }
}
