//! The local week, divided into hour slots.
//!
//! A slot is one (weekday, hour) pair in the host's own timezone, because a
//! working day is a local-time thing. Everything here is pure arithmetic over
//! an injected timezone, so tests can pick one.

use chrono::{DateTime, Datelike as _, TimeZone, Timelike as _};

use crate::usage::units::Timestamp;

pub const HOURS_PER_DAY: usize = 24;
pub const DAYS_PER_WEEK: usize = 7;
pub const SLOTS_PER_WEEK: usize = DAYS_PER_WEEK * HOURS_PER_DAY;
const SECONDS_PER_HOUR: f64 = 3600.0;

/// One hour of the local week. `weekday` is 0 for Monday.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SlotId {
    pub weekday: u8,
    pub hour: u8,
}

impl SlotId {
    pub fn new(weekday: u8, hour: u8) -> Self {
        Self {
            weekday: weekday % DAYS_PER_WEEK as u8,
            hour: hour % HOURS_PER_DAY as u8,
        }
    }

    pub fn is_weekend(self) -> bool {
        self.weekday >= 5
    }
}

/// One hour chunk of a time range, labelled by the slot it falls in.
///
/// A chunk is at most one hour but can be shorter at either end of the range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotChunk {
    pub slot: SlotId,
    pub from: Timestamp,
    pub to: Timestamp,
}

impl SlotChunk {
    /// How much of a whole hour this chunk covers, in 0..=1.
    pub fn fraction(self) -> f64 {
        ((self.to.get() - self.from.get()) / SECONDS_PER_HOUR).clamp(0.0, 1.0)
    }
}

pub fn local<Tz: TimeZone>(at: Timestamp, zone: &Tz) -> DateTime<Tz> {
    let seconds = at.get().trunc() as i64;
    DateTime::from_timestamp(seconds, 0)
        .unwrap_or_else(|| DateTime::from_timestamp_nanos(0))
        .with_timezone(zone)
}

pub fn slot_of<Tz: TimeZone>(at: Timestamp, zone: &Tz) -> SlotId {
    let moment = local(at, zone);
    SlotId::new(
        moment.weekday().num_days_from_monday() as u8,
        moment.hour() as u8,
    )
}

/// Start of the local hour containing `at`.
///
/// Subtracting the time already spent in the local hour, rather than
/// rebuilding an instant from a local wall clock, keeps this well defined at a
/// daylight-saving change, where one local hour reading maps to two instants.
pub fn slot_start<Tz: TimeZone>(at: Timestamp, zone: &Tz) -> Timestamp {
    let moment = local(at, zone);
    let into_hour = i64::from(moment.minute()) * 60 + i64::from(moment.second());
    Timestamp::new((at.get().trunc() as i64 - into_hour) as f64)
}

/// A backstop against a clock jump turning slot iteration into a long walk.
const MAX_CHUNKS: usize = 90 * HOURS_PER_DAY;

/// Split `from..to` into hour chunks, each labelled with its local slot.
///
/// Boundaries follow the local clock, so a daylight-saving change makes a day
/// hold 23 or 25 chunks rather than shifting every later slot by an hour.
pub fn chunks<Tz: TimeZone>(from: Timestamp, to: Timestamp, zone: &Tz) -> Vec<SlotChunk> {
    let mut out = Vec::new();
    let mut cursor = from;
    while cursor.get() < to.get() && out.len() < MAX_CHUNKS {
        let next = Timestamp::new(slot_start(cursor, zone).get() + SECONDS_PER_HOUR);
        let end = Timestamp::new(next.get().min(to.get()));
        if end.get() <= cursor.get() {
            break;
        }
        out.push(SlotChunk {
            slot: slot_of(cursor, zone),
            from: cursor,
            to: end,
        });
        cursor = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{SlotId, chunks, slot_of, slot_start};
    use crate::usage::units::Timestamp;
    use chrono::{FixedOffset, TimeZone as _};

    /// 2026-09-11 12:00:00 UTC, a Friday.
    const FRIDAY_NOON: f64 = 1_789_128_000.0;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).expect("utc offset")
    }

    fn paris_summer() -> FixedOffset {
        FixedOffset::east_opt(2 * 3600).expect("paris summer offset")
    }

    #[test]
    fn a_slot_is_the_local_weekday_and_hour() {
        let at = Timestamp::new(FRIDAY_NOON);
        assert_eq!(slot_of(at, &utc()), SlotId::new(4, 12));
        assert_eq!(slot_of(at, &paris_summer()), SlotId::new(4, 14));
    }

    #[test]
    fn the_timezone_can_move_a_slot_to_another_day() {
        // 23:30 UTC on Friday is 01:30 Saturday in Paris.
        let late = Timestamp::new(FRIDAY_NOON + 11.5 * 3600.0);
        assert_eq!(slot_of(late, &utc()), SlotId::new(4, 23));
        assert_eq!(slot_of(late, &paris_summer()), SlotId::new(5, 1));
        assert!(slot_of(late, &paris_summer()).is_weekend());
    }

    #[test]
    fn slot_start_truncates_to_the_local_hour() {
        let at = Timestamp::new(FRIDAY_NOON + 1800.0);
        assert_eq!(slot_start(at, &utc()).get(), FRIDAY_NOON);
        // A whole-hour offset truncates to the same instant here.
        assert_eq!(slot_start(at, &paris_summer()).get(), FRIDAY_NOON);
    }

    #[test]
    fn chunks_cover_the_range_exactly_once() {
        let from = Timestamp::new(FRIDAY_NOON + 1800.0);
        let to = Timestamp::new(FRIDAY_NOON + 3.0 * 3600.0);
        let covered = chunks(from, to, &utc());
        assert_eq!(covered.len(), 3);
        assert_eq!(covered[0].from.get(), from.get());
        assert_eq!(covered[2].to.get(), to.get());
        let total: f64 = covered
            .iter()
            .map(|chunk| chunk.to.get() - chunk.from.get())
            .sum();
        assert_eq!(total, to.get() - from.get());
        assert_eq!(covered[0].fraction(), 0.5);
        assert_eq!(covered[1].fraction(), 1.0);
    }

    #[test]
    fn chunks_label_each_hour_with_its_own_slot() {
        let from = Timestamp::new(FRIDAY_NOON);
        let to = Timestamp::new(FRIDAY_NOON + 2.0 * 3600.0);
        let covered = chunks(from, to, &paris_summer());
        assert_eq!(covered[0].slot, SlotId::new(4, 14));
        assert_eq!(covered[1].slot, SlotId::new(4, 15));
    }

    #[test]
    fn an_empty_or_reversed_range_yields_nothing() {
        let at = Timestamp::new(FRIDAY_NOON);
        assert!(chunks(at, at, &utc()).is_empty());
        assert!(chunks(at, Timestamp::new(FRIDAY_NOON - 60.0), &utc()).is_empty());
    }

    #[test]
    fn a_daylight_saving_day_holds_twenty_five_hours() {
        // Europe/Paris falls back at 03:00 local on 2026-10-25.
        let day_start = Timestamp::new(1_792_879_200.0); // 2026-10-25 00:00 +02:00
        let day_end = Timestamp::new(day_start.get() + 25.0 * 3600.0);
        let covered = chunks(day_start, day_end, &ParisLike);
        assert_eq!(covered.len(), 25);
        // Hour 2 local happens twice, once in each offset.
        let twos = covered.iter().filter(|chunk| chunk.slot.hour == 2).count();
        assert_eq!(twos, 2);
    }

    /// Europe/Paris without a timezone database: +02:00 until the October
    /// change, +01:00 after it.
    #[derive(Debug, Clone, Copy)]
    struct ParisLike;

    /// 2026-10-25 01:00 UTC, when Paris returns to +01:00.
    const PARIS_FALL_BACK: i64 = 1_792_890_000;

    impl chrono::TimeZone for ParisLike {
        type Offset = FixedOffset;

        fn from_offset(_offset: &FixedOffset) -> Self {
            ParisLike
        }

        fn offset_from_local_date(
            &self,
            _local: &chrono::NaiveDate,
        ) -> chrono::MappedLocalTime<FixedOffset> {
            chrono::MappedLocalTime::Single(FixedOffset::east_opt(3600).expect("offset"))
        }

        fn offset_from_local_datetime(
            &self,
            local: &chrono::NaiveDateTime,
        ) -> chrono::MappedLocalTime<FixedOffset> {
            let summer = FixedOffset::east_opt(2 * 3600).expect("offset");
            let winter = FixedOffset::east_opt(3600).expect("offset");
            let wall_clock = local.and_utc().timestamp();
            // The hour before the change reads the same on the wall clock as
            // the hour after it, so it maps to two instants.
            let summer_holds = wall_clock - 2 * 3600 < PARIS_FALL_BACK;
            let winter_holds = wall_clock - 3600 >= PARIS_FALL_BACK;
            match (summer_holds, winter_holds) {
                (true, true) => chrono::MappedLocalTime::Ambiguous(summer, winter),
                (true, false) => chrono::MappedLocalTime::Single(summer),
                (false, true) => chrono::MappedLocalTime::Single(winter),
                (false, false) => chrono::MappedLocalTime::None,
            }
        }

        fn offset_from_utc_date(&self, _utc: &chrono::NaiveDate) -> FixedOffset {
            FixedOffset::east_opt(3600).expect("offset")
        }

        fn offset_from_utc_datetime(&self, utc: &chrono::NaiveDateTime) -> FixedOffset {
            if utc.and_utc().timestamp() < PARIS_FALL_BACK {
                FixedOffset::east_opt(2 * 3600).expect("offset")
            } else {
                FixedOffset::east_opt(3600).expect("offset")
            }
        }
    }

    #[test]
    fn the_paris_stub_changes_offset_at_the_right_instant() {
        let before = ParisLike.timestamp_opt(PARIS_FALL_BACK - 1, 0).unwrap();
        let after = ParisLike.timestamp_opt(PARIS_FALL_BACK, 0).unwrap();
        assert_eq!(before.offset().local_minus_utc(), 2 * 3600);
        assert_eq!(after.offset().local_minus_utc(), 3600);
    }
}
