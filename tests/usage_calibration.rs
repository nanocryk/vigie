//! Calibration: fed a working pattern it has never been told about, the
//! estimator has to recover the end-of-window usage that pattern produces.
//!
//! The simulated user works 09:00 to 18:00 on weekdays at a steady intensity,
//! spending exactly 60% of the weekly budget. Nothing tells the estimator any
//! of that: it has to learn the schedule from the readings alone.

use chrono::FixedOffset;
use rusqlite::Connection;
use uuid::Uuid;
use vigie::usage::db::UsageDb;
use vigie::usage::estimate::{Confidence, Estimate, Inputs, project};
use vigie::usage::history;
use vigie::usage::profile::Profile;
use vigie::usage::slots;
use vigie::usage::units::{Pct, Timestamp};
use vigie::usage::window::{WindowKind, WindowState};

/// 2026-09-14 00:00:00 UTC, a Monday.
const MONDAY: f64 = 1_789_344_000.0;
/// The simulated session every reading comes from.
const SIM: Uuid = Uuid::from_u128(1);
const STEP_SEC: f64 = 300.0;
const DAY: f64 = 86400.0;
const WEEK: f64 = 7.0 * DAY;
const FIVE_HOURS: f64 = 5.0 * 3600.0;

/// Nine hours a day, five days a week.
const WORK_HOURS_PER_WEEK: f64 = 45.0;
/// The week the simulated user actually has: 60% of the weekly budget.
const TRUE_WEEK_USAGE: f64 = 60.0;
/// Percent of the weekly budget per active hour.
const WEEKLY_INTENSITY: f64 = TRUE_WEEK_USAGE / WORK_HOURS_PER_WEEK;
/// Percent of a 5h window's budget per active hour, at the same workload.
const FIVE_HOUR_INTENSITY: f64 = 14.0;

fn utc() -> FixedOffset {
    FixedOffset::east_opt(0).expect("utc offset")
}

fn is_working(at: f64) -> bool {
    let slot = slots::slot_of(Timestamp::new(at), &utc());
    slot.weekday < 5 && (9..18).contains(&slot.hour)
}

struct Simulation {
    connection: Connection,
    seven_day_pct: f64,
    five_hour_pct: f64,
    seven_day_reset: f64,
    five_hour_reset: f64,
}

impl Simulation {
    fn new() -> Self {
        let connection = Connection::open_in_memory().expect("in-memory database");
        vigie::store::migrate(&connection).expect("migrate");
        Self {
            connection,
            seven_day_pct: 0.0,
            five_hour_pct: 0.0,
            seven_day_reset: MONDAY + WEEK,
            five_hour_reset: MONDAY + FIVE_HOURS,
        }
    }

    fn db(&self) -> UsageDb<'_> {
        UsageDb::new(&self.connection)
    }

    /// Run the simulated user up to, but not including, `until`.
    fn run_to(&mut self, until: f64) {
        let mut at = MONDAY;
        while at < until {
            while at >= self.seven_day_reset {
                self.seven_day_reset += WEEK;
                self.seven_day_pct = 0.0;
            }
            while at >= self.five_hour_reset {
                self.five_hour_reset += FIVE_HOURS;
                self.five_hour_pct = 0.0;
            }
            if is_working(at) {
                let step_hours = STEP_SEC / 3600.0;
                self.seven_day_pct += WEEKLY_INTENSITY * step_hours;
                self.five_hour_pct += FIVE_HOUR_INTENSITY * step_hours;
                self.report(at);
            }
            at += STEP_SEC;
        }
    }

    /// One reading of both windows, then a fold of what has elapsed.
    fn report(&self, at: f64) {
        let now = Timestamp::new(at);
        for (kind, used, resets_at) in [
            (
                WindowKind::FiveHour,
                self.five_hour_pct,
                self.five_hour_reset,
            ),
            (
                WindowKind::SevenDay,
                self.seven_day_pct,
                self.seven_day_reset,
            ),
        ] {
            let state = WindowState {
                used: Pct::new(used),
                resets_at: Timestamp::new(resets_at),
            };
            self.db()
                .record(kind, state, Some(SIM), now)
                .expect("record");
        }
        history::refresh(&self.db(), now, &utc()).expect("refresh");
    }

    fn profile_at(&self, at: f64) -> Profile {
        history::refresh(&self.db(), Timestamp::new(at), &utc()).expect("refresh")
    }

    fn project(&self, kind: WindowKind, at: f64, profile: &Profile) -> Estimate {
        let (used, resets_at) = match kind {
            WindowKind::FiveHour => (self.five_hour_pct, self.five_hour_reset),
            WindowKind::SevenDay => (self.seven_day_pct, self.seven_day_reset),
        };
        let db = self.db();
        let now = Timestamp::new(at);
        let resets_at = Timestamp::new(resets_at);
        let samples = db.window_samples(kind, resets_at).expect("window samples");
        let watched_from = samples.first().map(|sample| sample.at).unwrap_or(now);
        let observed_used = match (samples.first(), samples.last()) {
            (Some(first), Some(last)) => last.pct - first.pct,
            _ => Pct::new(0.0),
        };
        let active_observed = history::active_hours_since(&db, watched_from, now, profile, &utc())
            .expect("active hours");
        project(
            &Inputs {
                kind,
                used: Pct::new(used),
                resets_at,
                now,
                profile,
                prior: db.prior(kind).expect("prior"),
                observed_used,
                active_observed,
            },
            &utc(),
        )
    }
}

/// Ten minutes into the third week, after two weeks of the same pattern.
const MEASURE_AT: f64 = MONDAY + 2.0 * WEEK + 9.0 * 3600.0 + 600.0;

#[test]
fn two_weeks_of_habit_predict_the_third() {
    let mut simulation = Simulation::new();
    simulation.run_to(MEASURE_AT);
    let profile = simulation.profile_at(MEASURE_AT);

    let estimate = simulation.project(WindowKind::SevenDay, MEASURE_AT, &profile);
    let error = (estimate.projected.get() - TRUE_WEEK_USAGE).abs();
    assert!(
        error < 15.0,
        "projected {:.1}% ten minutes into the week, against a true {TRUE_WEEK_USAGE}% \
         (intensity {:.2}%/active-hour, {:.1} active hours expected)",
        estimate.projected.get(),
        estimate.intensity.get(),
        estimate.active_total.get()
    );
}

#[test]
fn the_learned_week_is_about_the_right_size() {
    let mut simulation = Simulation::new();
    simulation.run_to(MEASURE_AT);
    let profile = simulation.profile_at(MEASURE_AT);

    let estimate = simulation.project(WindowKind::SevenDay, MEASURE_AT, &profile);
    assert!(
        (estimate.active_total.get() - WORK_HOURS_PER_WEEK).abs() < 12.0,
        "learned {:.1} active hours a week against a true {WORK_HOURS_PER_WEEK}",
        estimate.active_total.get()
    );
    // The hours actually worked must read higher than the ones slept through.
    assert!(
        profile.occupancy(slots::SlotId::new(2, 14))
            > 3.0 * profile.occupancy(slots::SlotId::new(2, 4))
    );
}

#[test]
fn a_fresh_window_is_not_extrapolated_over_the_clock() {
    let mut simulation = Simulation::new();
    simulation.run_to(MEASURE_AT);
    let profile = simulation.profile_at(MEASURE_AT);
    let estimate = simulation.project(WindowKind::SevenDay, MEASURE_AT, &profile);

    // What extrapolating the observed rate over every remaining minute gives.
    let observed_minutes = 10.0;
    let remaining_minutes = (simulation.seven_day_reset - MEASURE_AT) / 60.0;
    let naive = simulation.seven_day_pct / observed_minutes * remaining_minutes;
    assert!(
        naive > 150.0,
        "the naive projection should be absurd, got {naive:.0}%"
    );
    assert!(
        estimate.projected.get() < naive / 2.0,
        "projected {:.1}% against a naive {naive:.0}%",
        estimate.projected.get()
    );
}

#[test]
fn confidence_follows_how_much_has_been_seen() {
    let mut simulation = Simulation::new();
    // Ten minutes into the very first morning.
    let first_morning = MONDAY + 9.0 * 3600.0 + 600.0;
    simulation.run_to(first_morning);
    let profile = simulation.profile_at(first_morning);
    let early = simulation.project(WindowKind::SevenDay, first_morning, &profile);
    assert_eq!(early.confidence, Confidence::Low);

    simulation.run_to(MEASURE_AT);
    let profile = simulation.profile_at(MEASURE_AT);
    let late = simulation.project(WindowKind::SevenDay, MEASURE_AT, &profile);
    assert_eq!(late.confidence, Confidence::High);
}

#[test]
fn the_five_hour_window_converges_too() {
    let mut simulation = Simulation::new();
    // Midway through a 5h window that opened at 10:00 on the third Monday.
    let at = MONDAY + 2.0 * WEEK + 11.0 * 3600.0;
    simulation.run_to(at);
    let profile = simulation.profile_at(at);

    let estimate = simulation.project(WindowKind::FiveHour, at, &profile);
    // The window holds 10:00 to 15:00, all of it worked, at 14%/hour.
    assert!(
        estimate.projected.get() > 40.0 && estimate.projected.get() < 95.0,
        "projected {:.1}% for the 5h window",
        estimate.projected.get()
    );
    assert!(estimate.projected.get() >= simulation.five_hour_pct);
}
