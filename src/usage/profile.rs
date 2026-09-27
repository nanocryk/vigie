//! The working week, learned from history.
//!
//! Occupancy is the share of a slot's occurrences that saw usage. Pure
//! estimation: counters come in, probabilities come out.

use std::collections::BTreeMap;

use crate::usage::db::{RecordedSample, SlotCount};
use crate::usage::slots::{HOURS_PER_DAY, SLOTS_PER_WEEK, SlotChunk, SlotId};
use crate::usage::units::ActiveHours;

/// Counters age with this half-life, so the profile follows habits as they
/// move rather than averaging over every week ever recorded.
pub const HALF_LIFE_SEC: f64 = 28.0 * 86400.0;

/// Weight of the parent estimate, in occurrences, when a slot has little of
/// its own. A slot seen once therefore cannot read 0 or 1.
const POOLING_STRENGTH: f64 = 4.0;

/// Occupancy assumed before any history exists. Its value cancels out of a
/// first-run projection, which leans entirely on the budget-neutral prior.
const COLD_START_OCCUPANCY: f64 = 0.25;

/// Smoothing over neighbouring hours of the week. Activity is correlated
/// across adjacent hours, and counts are smoothed rather than ratios so the
/// result stays a proper estimate.
const KERNEL: [f64; 3] = [0.25, 0.5, 0.25];

/// Usage seen across a gap longer than this says nothing about which hour the
/// work happened in.
const GAP_MAX_SEC: f64 = 3600.0;

/// Fraction of a counter that survives `elapsed_sec`.
pub fn decay_factor(elapsed_sec: f64, half_life_sec: f64) -> f64 {
    if elapsed_sec <= 0.0 || half_life_sec <= 0.0 {
        return 1.0;
    }
    0.5f64.powf(elapsed_sec / half_life_sec)
}

/// Probability of working, per slot of the local week.
#[derive(Debug, Clone)]
pub struct Profile {
    occupancy: [f64; SLOTS_PER_WEEK],
    /// Total decayed occurrences behind the estimate.
    observed: f64,
}

fn index_of(slot: SlotId) -> usize {
    slot.weekday as usize * HOURS_PER_DAY + slot.hour as usize
}

impl Profile {
    /// A flat profile, used before any slot has been observed.
    pub fn cold_start() -> Self {
        Self {
            occupancy: [COLD_START_OCCUPANCY; SLOTS_PER_WEEK],
            observed: 0.0,
        }
    }

    pub fn from_counts(counts: &BTreeMap<SlotId, SlotCount>) -> Self {
        let mut active = [0.0; SLOTS_PER_WEEK];
        let mut occurrences = [0.0; SLOTS_PER_WEEK];
        for (slot, count) in counts {
            active[index_of(*slot)] = count.active;
            occurrences[index_of(*slot)] = count.occurrences;
        }
        let observed: f64 = occurrences.iter().sum();
        if observed <= 0.0 {
            return Self::cold_start();
        }

        let smooth_active = smooth(&active);
        let smooth_occurrences = smooth(&occurrences);

        let global = active.iter().sum::<f64>() / observed;

        // Hour of day, pooled toward the global mean.
        let mut hour_mean = [0.0; HOURS_PER_DAY];
        for (hour, mean) in hour_mean.iter_mut().enumerate() {
            let mut hour_active = 0.0;
            let mut hour_occurrences = 0.0;
            for weekday in 0..7 {
                let index = weekday * HOURS_PER_DAY + hour;
                hour_active += smooth_active[index];
                hour_occurrences += smooth_occurrences[index];
            }
            *mean =
                (hour_active + POOLING_STRENGTH * global) / (hour_occurrences + POOLING_STRENGTH);
        }

        // Each slot, pooled toward its hour of day.
        let mut occupancy = [0.0; SLOTS_PER_WEEK];
        for (index, value) in occupancy.iter_mut().enumerate() {
            let parent = hour_mean[index % HOURS_PER_DAY];
            *value = ((smooth_active[index] + POOLING_STRENGTH * parent)
                / (smooth_occurrences[index] + POOLING_STRENGTH))
                .clamp(0.0, 1.0);
        }

        Self {
            occupancy,
            observed,
        }
    }

    pub fn occupancy(&self, slot: SlotId) -> f64 {
        self.occupancy[index_of(slot)]
    }

    /// Decayed occurrences behind the profile, as a measure of how much it
    /// rests on observation rather than on pooling.
    pub fn observed_occurrences(&self) -> f64 {
        self.observed
    }

    /// Active hours these chunks are expected to hold.
    pub fn expected_active_hours(&self, chunks: &[SlotChunk]) -> ActiveHours {
        ActiveHours::new(
            chunks
                .iter()
                .map(|chunk| self.occupancy(chunk.slot) * chunk.fraction())
                .sum(),
        )
    }
}

/// Circular smoothing over the 168 hours of the week.
fn smooth(values: &[f64; SLOTS_PER_WEEK]) -> [f64; SLOTS_PER_WEEK] {
    let mut out = [0.0; SLOTS_PER_WEEK];
    for (index, slot) in out.iter_mut().enumerate() {
        for (offset, weight) in KERNEL.iter().enumerate() {
            let neighbour = (index + SLOTS_PER_WEEK + offset - 1) % SLOTS_PER_WEEK;
            *slot += values[neighbour] * weight;
        }
    }
    out
}

/// How much of each chunk was spent working, in 0..=1 per chunk.
///
/// A rise between two readings close together places the work in the chunks
/// they span. A rise across a longer gap cannot say when the work happened, so
/// those chunks keep the profile's own expectation and the estimate learns
/// nothing from them.
pub fn activity_weights(
    chunks: &[SlotChunk],
    samples: &[RecordedSample],
    profile: &Profile,
) -> Vec<f64> {
    let mut weights: Vec<f64> = vec![0.0; chunks.len()];
    for pair in samples.windows(2) {
        let (before, after) = (pair[0], pair[1]);
        // Readings from different window instances are not comparable.
        if before.resets_at != after.resets_at || after.pct.get() <= before.pct.get() {
            continue;
        }
        let observed = after.at.seconds_since(before.at) <= GAP_MAX_SEC;
        for (index, chunk) in chunks.iter().enumerate() {
            if chunk.to.get() <= before.at.get() || chunk.from.get() >= after.at.get() {
                continue;
            }
            let weight = if observed {
                1.0
            } else {
                profile.occupancy(chunk.slot)
            };
            weights[index] = weights[index].max(weight);
        }
    }
    weights
}

/// Active hours actually observed across these chunks.
pub fn observed_active_hours(
    chunks: &[SlotChunk],
    samples: &[RecordedSample],
    profile: &Profile,
) -> ActiveHours {
    let weights = activity_weights(chunks, samples, profile);
    ActiveHours::new(
        chunks
            .iter()
            .zip(&weights)
            .map(|(chunk, weight)| chunk.fraction() * weight)
            .sum(),
    )
}

#[cfg(test)]
mod tests {
    use super::{Profile, activity_weights, decay_factor, observed_active_hours};
    use crate::usage::db::{RecordedSample, SlotCount};
    use crate::usage::slots::{self, SlotId};
    use crate::usage::units::{Pct, Timestamp};
    use chrono::FixedOffset;
    use std::collections::BTreeMap;

    /// 2026-09-11 12:00:00 UTC, a Friday.
    const FRIDAY_NOON: f64 = 1_789_128_000.0;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).expect("utc offset")
    }

    fn counts(entries: &[(SlotId, f64, f64)]) -> BTreeMap<SlotId, SlotCount> {
        entries
            .iter()
            .map(|(slot, active, occurrences)| {
                (
                    *slot,
                    SlotCount {
                        active: *active,
                        occurrences: *occurrences,
                    },
                )
            })
            .collect()
    }

    fn sample(at: f64, pct: f64, resets_at: f64) -> RecordedSample {
        RecordedSample {
            at: Timestamp::new(at),
            pct: Pct::new(pct),
            resets_at: Timestamp::new(resets_at),
        }
    }

    #[test]
    fn an_empty_history_is_flat() {
        let profile = Profile::from_counts(&BTreeMap::new());
        assert_eq!(profile.occupancy(SlotId::new(0, 3)), 0.25);
        assert_eq!(profile.occupancy(SlotId::new(5, 20)), 0.25);
        assert_eq!(profile.observed_occurrences(), 0.0);
    }

    /// A whole week of occurrences, so a slot is judged against context
    /// rather than against itself alone.
    fn week(occurrences: f64, active: impl Fn(SlotId) -> bool) -> BTreeMap<SlotId, SlotCount> {
        let mut out = BTreeMap::new();
        for weekday in 0..7u8 {
            for hour in 0..24u8 {
                let slot = SlotId::new(weekday, hour);
                out.insert(
                    slot,
                    SlotCount {
                        occurrences,
                        active: if active(slot) { occurrences } else { 0.0 },
                    },
                );
            }
        }
        out
    }

    #[test]
    fn one_observation_cannot_pin_a_slot_to_certainty() {
        let lone = SlotId::new(1, 10);
        let profile = Profile::from_counts(&week(1.0, |slot| slot == lone));
        let seen_working = profile.occupancy(lone);
        assert!(
            seen_working > 0.05 && seen_working < 0.9,
            "one active hour in a week read {seen_working}"
        );
        let never_seen = profile.occupancy(SlotId::new(3, 2));
        assert!(
            never_seen > 0.0,
            "an hour never seen active read {never_seen}, which would zero out every projection"
        );
    }

    #[test]
    fn a_repeated_stretch_of_hours_reads_as_a_habit() {
        let profile = Profile::from_counts(&week(40.0, |slot| {
            slot.weekday == 1 && (9..18).contains(&slot.hour)
        }));
        let inside = profile.occupancy(SlotId::new(1, 13));
        let outside = profile.occupancy(SlotId::new(1, 3));
        assert!(inside > 0.8, "the middle of the stretch read {inside}");
        assert!(outside < 0.2, "an hour outside it read {outside}");
    }

    #[test]
    fn an_active_hour_lifts_its_neighbours() {
        let profile = Profile::from_counts(&counts(&[
            (SlotId::new(1, 10), 20.0, 20.0),
            (SlotId::new(1, 9), 0.0, 20.0),
            (SlotId::new(1, 3), 0.0, 20.0),
        ]));
        let neighbour = profile.occupancy(SlotId::new(1, 9));
        let distant = profile.occupancy(SlotId::new(1, 3));
        assert!(
            neighbour > distant,
            "neighbour {neighbour} should outrank the distant hour {distant}"
        );
    }

    #[test]
    fn expected_active_hours_weighs_each_chunk_by_its_length() {
        let profile = Profile::from_counts(&BTreeMap::new());
        let chunks = slots::chunks(
            Timestamp::new(FRIDAY_NOON),
            Timestamp::new(FRIDAY_NOON + 5400.0),
            &utc(),
        );
        // Ninety minutes of a flat quarter-occupancy profile.
        assert!((profile.expected_active_hours(&chunks).get() - 0.375).abs() < 1e-9);
    }

    #[test]
    fn a_rise_between_close_readings_marks_the_hour_active() {
        let profile = Profile::cold_start();
        let chunks = slots::chunks(
            Timestamp::new(FRIDAY_NOON),
            Timestamp::new(FRIDAY_NOON + 7200.0),
            &utc(),
        );
        let samples = vec![
            sample(FRIDAY_NOON + 600.0, 2.0, 0.0),
            sample(FRIDAY_NOON + 1200.0, 3.0, 0.0),
        ];
        let weights = activity_weights(&chunks, &samples, &profile);
        assert_eq!(weights, vec![1.0, 0.0]);
    }

    #[test]
    fn readings_without_a_rise_leave_the_hour_idle() {
        let profile = Profile::cold_start();
        let chunks = slots::chunks(
            Timestamp::new(FRIDAY_NOON),
            Timestamp::new(FRIDAY_NOON + 3600.0),
            &utc(),
        );
        let samples = vec![
            sample(FRIDAY_NOON + 600.0, 2.0, 0.0),
            sample(FRIDAY_NOON + 1200.0, 2.0, 0.0),
        ];
        assert_eq!(activity_weights(&chunks, &samples, &profile), vec![0.0]);
    }

    #[test]
    fn a_window_reset_between_readings_is_not_a_rise() {
        let profile = Profile::cold_start();
        let chunks = slots::chunks(
            Timestamp::new(FRIDAY_NOON),
            Timestamp::new(FRIDAY_NOON + 3600.0),
            &utc(),
        );
        // The second reading belongs to a fresh window, so its lower value is
        // not a fall and the jump back up is not consumption.
        let samples = vec![
            sample(FRIDAY_NOON + 600.0, 80.0, 1.0),
            sample(FRIDAY_NOON + 1200.0, 90.0, 2.0),
        ];
        assert_eq!(activity_weights(&chunks, &samples, &profile), vec![0.0]);
    }

    #[test]
    fn a_rise_across_a_long_gap_teaches_nothing_about_when() {
        let profile = Profile::cold_start();
        let chunks = slots::chunks(
            Timestamp::new(FRIDAY_NOON),
            Timestamp::new(FRIDAY_NOON + 5.0 * 3600.0),
            &utc(),
        );
        let samples = vec![
            sample(FRIDAY_NOON + 600.0, 2.0, 0.0),
            sample(FRIDAY_NOON + 4.0 * 3600.0, 9.0, 0.0),
        ];
        let weights = activity_weights(&chunks, &samples, &profile);
        // Each spanned chunk keeps the profile's own expectation.
        assert_eq!(&weights[0..4], &[0.25, 0.25, 0.25, 0.25]);
        assert_eq!(weights[4], 0.0);
    }

    #[test]
    fn observed_active_hours_counts_partial_chunks() {
        let profile = Profile::cold_start();
        let chunks = slots::chunks(
            Timestamp::new(FRIDAY_NOON),
            Timestamp::new(FRIDAY_NOON + 1800.0),
            &utc(),
        );
        let samples = vec![
            sample(FRIDAY_NOON + 300.0, 2.0, 0.0),
            sample(FRIDAY_NOON + 900.0, 4.0, 0.0),
        ];
        assert_eq!(
            observed_active_hours(&chunks, &samples, &profile).get(),
            0.5
        );
    }

    #[test]
    fn decay_halves_a_counter_every_half_life() {
        assert_eq!(decay_factor(0.0, 100.0), 1.0);
        assert_eq!(decay_factor(100.0, 100.0), 0.5);
        assert!((decay_factor(200.0, 100.0) - 0.25).abs() < 1e-12);
        assert_eq!(decay_factor(-5.0, 100.0), 1.0);
    }
}
