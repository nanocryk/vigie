//! Folding what has been observed into the stored profile and priors.
//!
//! The seam between the pure estimation in [`crate::usage::profile`] and the
//! rows in [`crate::usage::db`].

use anyhow::Result;
use chrono::TimeZone;

use crate::usage::db::{META_CLOSED_THROUGH, META_DECAYED_AT, Prior, UsageDb};
use crate::usage::profile::{self, HALF_LIFE_SEC, Profile};
use crate::usage::slots;
use crate::usage::units::{ActiveHours, Timestamp};
use crate::usage::window::WindowKind;

/// How far before a slot to look for the reading that precedes it, so a rise
/// spanning the boundary is not lost.
const LOOKBACK_SEC: f64 = 2.0 * 3600.0;

/// Activity is read from the 5h window: its percentages move in larger steps
/// than the weekly ones, so an hour of work is visible in them.
const ACTIVITY_WINDOW: WindowKind = WindowKind::FiveHour;

/// Bring the profile and the priors up to date, and return the profile.
pub fn refresh<Tz: TimeZone>(db: &UsageDb<'_>, now: Timestamp, zone: &Tz) -> Result<Profile> {
    close_elapsed_slots(db, now, zone)?;
    fold_completed_windows(db, now)?;
    Ok(Profile::from_counts(&db.slot_counts()?))
}

/// Fold every slot that has fully elapsed into the profile's counters.
///
/// Slots are only closed once the hour is over, so the current hour never
/// counts as idle merely because it has just begun.
fn close_elapsed_slots<Tz: TimeZone>(db: &UsageDb<'_>, now: Timestamp, zone: &Tz) -> Result<()> {
    let boundary = slots::slot_start(now, zone);
    let Some(closed_through) = db.meta(META_CLOSED_THROUGH)?.map(Timestamp::new) else {
        // Nothing is known about the hours before the first reading.
        let first = db
            .earliest_reading_at()?
            .filter(|earliest| earliest.get() < now.get())
            .map_or(boundary, |earliest| slots::slot_start(earliest, zone));
        db.set_meta(META_CLOSED_THROUGH, first.get())?;
        return if first.get() < boundary.get() {
            close_elapsed_slots(db, now, zone)
        } else {
            Ok(())
        };
    };
    if closed_through.get() >= boundary.get() {
        return Ok(());
    }

    let profile = Profile::from_counts(&db.slot_counts()?);
    let samples = db.samples_between(
        ACTIVITY_WINDOW,
        Timestamp::new(closed_through.get() - LOOKBACK_SEC),
        boundary,
    )?;
    let chunks = slots::chunks(closed_through, boundary, zone);
    let weights = profile::activity_weights(&chunks, &samples, &profile);

    // Age what is already counted before adding to it.
    let decayed_at = db.meta(META_DECAYED_AT)?.unwrap_or(now.get());
    let factor = profile::decay_factor(now.get() - decayed_at, HALF_LIFE_SEC);
    if factor < 1.0 {
        db.decay_slots(factor)?;
    }
    db.set_meta(META_DECAYED_AT, now.get())?;

    for (chunk, weight) in chunks.iter().zip(&weights) {
        db.add_slot_occurrence(chunk.slot, *weight)?;
        db.record_closed_hour(chunk.from, *weight)?;
    }
    db.set_meta(META_CLOSED_THROUGH, boundary.get())?;
    Ok(())
}

/// Active hours between an instant and now.
///
/// Hours that have closed were judged once and are read back; only the hour in
/// progress is measured from the readings themselves.
pub fn active_hours_since<Tz: TimeZone>(
    db: &UsageDb<'_>,
    from: Timestamp,
    now: Timestamp,
    profile: &Profile,
    zone: &Tz,
) -> Result<ActiveHours> {
    if now.get() <= from.get() {
        return Ok(ActiveHours::new(0.0));
    }
    let closed_through = db
        .meta(META_CLOSED_THROUGH)?
        .map(Timestamp::new)
        .unwrap_or(now);
    let closed = db.active_hours_between(from, closed_through)?;

    let in_progress_from = closed_through.later_of(from);
    let in_progress = if now.get() > in_progress_from.get() {
        let samples = db.samples_between(
            ACTIVITY_WINDOW,
            Timestamp::new(in_progress_from.get() - LOOKBACK_SEC),
            now,
        )?;
        let chunks = slots::chunks(in_progress_from, now, zone);
        profile::observed_active_hours(&chunks, &samples, profile).get()
    } else {
        0.0
    };

    Ok(ActiveHours::new(closed + in_progress))
}

fn covered_key(kind: WindowKind) -> String {
    format!("prior_covered_{}", kind.label())
}

fn updated_key(kind: WindowKind) -> String {
    format!("prior_updated_{}", kind.label())
}

/// Carry each finished window's intensity into the prior for its kind.
fn fold_completed_windows(db: &UsageDb<'_>, now: Timestamp) -> Result<()> {
    for kind in WindowKind::ALL {
        let covered = Timestamp::new(db.meta(&covered_key(kind))?.unwrap_or(0.0));
        let instances = db.completed_instances(kind, covered, now)?;
        if instances.is_empty() {
            continue;
        }

        let stored = db.prior(kind)?;
        let updated_at = db.meta(&updated_key(kind))?.unwrap_or(now.get());
        let decay = profile::decay_factor(now.get() - updated_at, HALF_LIFE_SEC);
        let mut intensity = stored.map(|prior| prior.lambda).unwrap_or(0.0);
        let mut weight = stored.map(|prior| prior.weight * decay).unwrap_or(0.0);
        let mut latest = covered;

        for (resets_at, final_pct) in instances {
            latest = resets_at;
            let opened_at = kind.started_at(resets_at);
            let active = db.active_hours_between(opened_at, resets_at)?;
            if active <= 0.0 {
                continue;
            }
            let mass = intensity * weight + final_pct.get();
            weight += active;
            intensity = mass / weight;
        }

        if weight > 0.0 {
            db.set_prior(
                kind,
                Prior {
                    lambda: intensity,
                    weight,
                },
            )?;
        }
        db.set_meta(&covered_key(kind), latest.get())?;
        db.set_meta(&updated_key(kind), now.get())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{covered_key, refresh};
    use crate::usage::db::tests::connection;
    use crate::usage::db::{META_CLOSED_THROUGH, UsageDb};
    use crate::usage::profile::Profile;
    use crate::usage::slots::SlotId;
    use crate::usage::units::{Pct, Timestamp};
    use crate::usage::window::{WindowKind, WindowState};
    use chrono::FixedOffset;

    /// 2026-09-14 00:00:00 UTC, a Monday.
    const MONDAY_MIDNIGHT: f64 = 1_789_344_000.0;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).expect("utc offset")
    }

    fn record(db: &UsageDb<'_>, at: f64, pct: f64, resets_at: f64) {
        db.record(
            WindowKind::FiveHour,
            WindowState {
                used: Pct::new(pct),
                resets_at: Timestamp::new(resets_at),
            },
            Some(uuid::Uuid::from_u128(1)),
            Timestamp::new(at),
        )
        .expect("record");
    }

    fn run(db: &UsageDb<'_>, at: f64) {
        refresh(db, Timestamp::new(at), &utc()).expect("refresh");
    }

    #[test]
    fn the_first_run_claims_no_knowledge_of_earlier_hours() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        run(&db, MONDAY_MIDNIGHT + 9.5 * 3600.0);
        assert!(db.slot_counts().expect("counts").is_empty());
        assert_eq!(
            db.meta(META_CLOSED_THROUGH).expect("meta"),
            Some(MONDAY_MIDNIGHT + 9.0 * 3600.0)
        );
    }

    #[test]
    fn the_first_run_folds_readings_already_in_history() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let start = MONDAY_MIDNIGHT + 9.0 * 3600.0;
        let resets = start + 5.0 * 3600.0;
        record(&db, start + 300.0, 1.0, resets);
        record(&db, start + 900.0, 4.0, resets);

        run(&db, start + 2.5 * 3600.0);
        let counts = db.slot_counts().expect("counts");
        assert_eq!(counts.len(), 2, "both closed hours are counted");
        assert_eq!(counts[&SlotId::new(0, 9)].active, 1.0);
        assert_eq!(counts[&SlotId::new(0, 10)].active, 0.0);
    }

    #[test]
    fn a_worked_hour_is_counted_once_the_hour_is_over() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let start = MONDAY_MIDNIGHT + 9.0 * 3600.0;
        run(&db, start);

        let resets = start + 5.0 * 3600.0;
        record(&db, start + 300.0, 1.0, resets);
        record(&db, start + 900.0, 4.0, resets);

        // Half past the next hour, so the worked hour has closed.
        run(&db, start + 5400.0);

        let counts = db.slot_counts().expect("counts");
        let worked = counts[&SlotId::new(0, 9)];
        assert_eq!(worked.occurrences, 1.0);
        assert_eq!(worked.active, 1.0);
        assert_eq!(counts.len(), 1, "only the closed hour is counted");
    }

    #[test]
    fn an_hour_without_usage_counts_as_idle() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let start = MONDAY_MIDNIGHT + 3.0 * 3600.0;
        run(&db, start);
        run(&db, start + 2.0 * 3600.0);

        let counts = db.slot_counts().expect("counts");
        assert_eq!(counts[&SlotId::new(0, 3)].occurrences, 1.0);
        assert_eq!(counts[&SlotId::new(0, 3)].active, 0.0);
        assert_eq!(counts[&SlotId::new(0, 4)].occurrences, 1.0);
    }

    #[test]
    fn a_fortnight_away_ages_the_profile_rather_than_freezing_it() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let start = MONDAY_MIDNIGHT + 9.0 * 3600.0;
        run(&db, start);
        let resets = start + 5.0 * 3600.0;
        record(&db, start + 300.0, 1.0, resets);
        record(&db, start + 900.0, 4.0, resets);
        run(&db, start + 5400.0);
        let before = db.slot_counts().expect("counts")[&SlotId::new(0, 9)];

        // Four weeks later: one half-life.
        run(&db, start + 28.0 * 86400.0 + 5400.0);
        let after = db.slot_counts().expect("counts")[&SlotId::new(0, 9)];

        assert!(
            after.active < before.active,
            "active {} did not decay from {}",
            after.active,
            before.active
        );
        assert!((after.active - 0.5).abs() < 0.01, "active {}", after.active);
        // The four idle weeks were counted, so the slot is no longer certain.
        let profile = Profile::from_counts(&db.slot_counts().expect("counts"));
        assert!(profile.occupancy(SlotId::new(0, 9)) < 0.5);
    }

    fn a_finished_window(db: &UsageDb<'_>) -> f64 {
        let start = MONDAY_MIDNIGHT + 9.0 * 3600.0;
        run(db, start);
        let resets = start + 5.0 * 3600.0;
        for step in 0..8 {
            record(
                db,
                start + f64::from(step) * 1800.0,
                f64::from(step) * 4.0,
                resets,
            );
        }
        resets
    }

    #[test]
    fn a_finished_window_becomes_the_prior() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let resets = a_finished_window(&db);

        run(&db, resets + 1800.0);

        let prior = db
            .prior(WindowKind::FiveHour)
            .expect("prior")
            .expect("a prior after a finished window");
        // Four hours of work spending 28% of the budget.
        assert!(
            prior.weight >= 3.5 && prior.weight <= 4.5,
            "weight {}",
            prior.weight
        );
        assert!(
            prior.lambda > 5.0 && prior.lambda < 9.0,
            "intensity {}",
            prior.lambda
        );
        assert_eq!(
            db.meta(&covered_key(WindowKind::FiveHour)).expect("meta"),
            Some(resets)
        );
    }

    #[test]
    fn a_finished_window_is_folded_in_only_once() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let resets = a_finished_window(&db);
        run(&db, resets + 1800.0);
        let first = db
            .prior(WindowKind::FiveHour)
            .expect("prior")
            .expect("prior");

        run(&db, resets + 5400.0);
        let second = db
            .prior(WindowKind::FiveHour)
            .expect("prior")
            .expect("prior");

        assert!(
            (second.weight - first.weight).abs() < 1e-9,
            "weight moved from {} to {}",
            first.weight,
            second.weight
        );
    }
}
