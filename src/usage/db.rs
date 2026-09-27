//! Usage readings, per-message tokens, and the activity profile's counters.
//!
//! Plumbing only: rows in, rows out. What the numbers mean lives in the
//! profile and estimator modules. Instants are epoch seconds as `REAL`, the
//! unit every computation here works in.

use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, params};

use crate::usage::slots::SlotId;
use crate::usage::units::{Pct, Timestamp};
use crate::usage::window::{Sample, WindowKind, WindowState};

/// The store migration creating the usage tables.
pub const MIGRATION: &str = r#"
CREATE TABLE usage_readings (
    id INTEGER PRIMARY KEY,
    at REAL NOT NULL,
    window_kind TEXT NOT NULL CHECK (window_kind IN ('5h', '7d')),
    pct REAL NOT NULL,
    resets_at REAL NOT NULL,
    session_id TEXT
) STRICT;
CREATE INDEX usage_readings_window ON usage_readings (window_kind, resets_at, at);
CREATE INDEX usage_readings_session ON usage_readings (window_kind, session_id, at);
CREATE INDEX usage_readings_at ON usage_readings (at);
CREATE TABLE usage_slots (
    weekday INTEGER NOT NULL,
    hour INTEGER NOT NULL,
    occurrences REAL NOT NULL DEFAULT 0,
    active REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (weekday, hour)
) STRICT;
CREATE TABLE usage_closed_hours (
    at REAL PRIMARY KEY,
    active REAL NOT NULL
) STRICT;
CREATE TABLE usage_priors (
    window_kind TEXT PRIMARY KEY,
    lambda REAL NOT NULL,
    weight REAL NOT NULL
) STRICT;
CREATE TABLE usage_meta (
    key TEXT PRIMARY KEY,
    value REAL NOT NULL
) STRICT;
CREATE TABLE usage_messages (
    session_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    at REAL NOT NULL,
    model TEXT NOT NULL,
    parent_tool_use_id TEXT,
    tokens INTEGER NOT NULL,
    PRIMARY KEY (session_id, message_id)
) STRICT;
"#;

/// Readings from different sessions inside one bucket describe the same state.
const MERGE_BUCKET_SEC: f64 = 30.0;

pub const META_HISTORY_STARTED: &str = "history_started_at";
pub const META_CLOSED_THROUGH: &str = "slots_closed_through";
pub const META_DECAYED_AT: &str = "slots_decayed_at";
pub const META_PRUNED_AT: &str = "pruned_at";

/// A stored reading, carrying the window instance it belongs to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecordedSample {
    pub at: Timestamp,
    pub pct: Pct,
    pub resets_at: Timestamp,
}

/// Decayed counts for one slot of the local week.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SlotCount {
    /// Times this slot has come round since history began.
    pub occurrences: f64,
    /// How many of those saw usage.
    pub active: f64,
}

/// Intensity carried over from completed windows, in percent per active hour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prior {
    pub lambda: f64,
    /// Active hours behind the estimate.
    pub weight: f64,
}

/// Tokens one API message spent, as its `assistant` lines report them.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageTokens {
    pub message_id: String,
    pub model: String,
    /// The subagent's spawning tool use; `None` on the main conversation.
    pub parent_tool_use_id: Option<String>,
    pub tokens: u64,
}

/// What the database holds, for the snapshot's history section.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub readings: u64,
    pub oldest: Option<Timestamp>,
    /// Window instances tracked per window kind.
    pub instances: BTreeMap<WindowKind, u64>,
    pub priors: BTreeMap<WindowKind, Prior>,
    pub history_started: Option<Timestamp>,
}

/// The usage tables, over a connection the caller owns.
pub struct UsageDb<'a> {
    conn: &'a Connection,
}

impl<'a> UsageDb<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn meta(&self, key: &str) -> Result<Option<f64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM usage_meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: f64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO usage_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Stamps the start of history once.
    pub fn ensure_started(&self, now: Timestamp) -> Result<()> {
        if self.meta(META_HISTORY_STARTED)?.is_none() {
            self.set_meta(META_HISTORY_STARTED, now.get())?;
        }
        Ok(())
    }

    /// Store a reading, unless this session already reported the same one.
    ///
    /// Returns whether a row was written.
    pub fn record(
        &self,
        kind: WindowKind,
        state: WindowState,
        session: Option<&str>,
        now: Timestamp,
    ) -> Result<bool> {
        let last: Option<(f64, f64)> = self
            .conn
            .query_row(
                "SELECT pct, resets_at FROM usage_readings
                 WHERE window_kind = ?1 AND session_id IS ?2 ORDER BY at DESC LIMIT 1",
                params![kind.label(), session],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if last == Some((state.used.get(), state.resets_at.get())) {
            return Ok(false);
        }
        self.conn.execute(
            "INSERT INTO usage_readings (at, window_kind, pct, resets_at, session_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                now.get(),
                kind.label(),
                state.used.get(),
                state.resets_at.get(),
                session
            ],
        )?;
        Ok(true)
    }

    /// The newest reading of a window, whichever session took it.
    pub fn latest_reading(&self, kind: WindowKind) -> Result<Option<RecordedSample>> {
        Ok(self
            .conn
            .query_row(
                "SELECT at, pct, resets_at FROM usage_readings WHERE window_kind = ?1
                 ORDER BY at DESC, id DESC LIMIT 1",
                params![kind.label()],
                |row| {
                    Ok(RecordedSample {
                        at: Timestamp::new(row.get(0)?),
                        pct: Pct::new(row.get(1)?),
                        resets_at: Timestamp::new(row.get(2)?),
                    })
                },
            )
            .optional()?)
    }

    /// The oldest reading of any window.
    pub fn earliest_reading_at(&self) -> Result<Option<Timestamp>> {
        let earliest: Option<f64> =
            self.conn
                .query_row("SELECT MIN(at) FROM usage_readings", [], |row| row.get(0))?;
        Ok(earliest.map(Timestamp::new))
    }

    /// Readings for one window instance, oldest first.
    pub fn window_samples(&self, kind: WindowKind, resets_at: Timestamp) -> Result<Vec<Sample>> {
        let mut statement = self.conn.prepare(
            "SELECT CAST(at / ?3 AS INTEGER) * ?3 AS bucket, MAX(pct) FROM usage_readings
             WHERE window_kind = ?1 AND resets_at = ?2
             GROUP BY bucket ORDER BY bucket",
        )?;
        let rows = statement.query_map(
            params![kind.label(), resets_at.get(), MERGE_BUCKET_SEC],
            |row| {
                Ok(Sample {
                    at: Timestamp::new(row.get(0)?),
                    pct: Pct::new(row.get(1)?),
                })
            },
        )?;
        let mut samples = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        drop_stale_highs(&mut samples, |sample| sample.pct.get());
        Ok(samples)
    }

    /// Readings in a time range across window instances, oldest first.
    ///
    /// Each instance is cleaned on its own, since usage only has to rise
    /// within one window.
    pub fn samples_between(
        &self,
        kind: WindowKind,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<RecordedSample>> {
        let mut statement = self.conn.prepare(
            "SELECT resets_at, CAST(at / ?4 AS INTEGER) * ?4 AS bucket, MAX(pct)
             FROM usage_readings
             WHERE window_kind = ?1 AND at >= ?2 AND at < ?3
             GROUP BY resets_at, bucket ORDER BY bucket",
        )?;
        let rows = statement.query_map(
            params![kind.label(), from.get(), to.get(), MERGE_BUCKET_SEC],
            |row| {
                Ok(RecordedSample {
                    resets_at: Timestamp::new(row.get(0)?),
                    at: Timestamp::new(row.get(1)?),
                    pct: Pct::new(row.get(2)?),
                })
            },
        )?;
        let all = rows.collect::<rusqlite::Result<Vec<_>>>()?;

        let mut per_instance: BTreeMap<u64, Vec<RecordedSample>> = BTreeMap::new();
        for sample in all {
            per_instance
                .entry(sample.resets_at.get().to_bits())
                .or_default()
                .push(sample);
        }
        let mut out = Vec::new();
        for instance in per_instance.values_mut() {
            drop_stale_highs(instance, |sample| sample.pct.get());
            out.append(instance);
        }
        out.sort_by(|left, right| left.at.get().total_cmp(&right.at.get()));
        Ok(out)
    }

    /// Window instances whose reset has passed, with the usage they ended on.
    ///
    /// `after` excludes instances already accounted for.
    pub fn completed_instances(
        &self,
        kind: WindowKind,
        after: Timestamp,
        now: Timestamp,
    ) -> Result<Vec<(Timestamp, Pct)>> {
        let mut statement = self.conn.prepare(
            "SELECT resets_at, MAX(pct) FROM usage_readings
             WHERE window_kind = ?1 AND resets_at > ?2 AND resets_at <= ?3
             GROUP BY resets_at ORDER BY resets_at",
        )?;
        let rows = statement.query_map(params![kind.label(), after.get(), now.get()], |row| {
            Ok((Timestamp::new(row.get(0)?), Pct::new(row.get(1)?)))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn prune(&self, before: Timestamp) -> Result<usize> {
        let removed = self.conn.execute(
            "DELETE FROM usage_readings WHERE at < ?1",
            params![before.get()],
        )?;
        self.conn.execute(
            "DELETE FROM usage_closed_hours WHERE at < ?1",
            params![before.get()],
        )?;
        Ok(removed)
    }

    /// Prune at most once a day, so frequent snapshots do not rewrite the table.
    pub fn prune_if_due(&self, now: Timestamp, retention_days: u32) -> Result<usize> {
        let last = self.meta(META_PRUNED_AT)?.unwrap_or(f64::NEG_INFINITY);
        if now.get() - last < 86400.0 {
            return Ok(0);
        }
        let removed = self.prune(Timestamp::new(
            now.get() - f64::from(retention_days) * 86400.0,
        ))?;
        self.set_meta(META_PRUNED_AT, now.get())?;
        Ok(removed)
    }

    pub fn slot_counts(&self) -> Result<BTreeMap<SlotId, SlotCount>> {
        let mut statement = self
            .conn
            .prepare("SELECT weekday, hour, occurrences, active FROM usage_slots")?;
        let rows = statement.query_map([], |row| {
            Ok((
                SlotId::new(row.get::<_, u8>(0)?, row.get::<_, u8>(1)?),
                SlotCount {
                    occurrences: row.get(2)?,
                    active: row.get(3)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?)
    }

    /// Fold one occurrence of a slot into the profile, `active` of it spent
    /// working.
    pub fn add_slot_occurrence(&self, slot: SlotId, active: f64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO usage_slots (weekday, hour, occurrences, active) VALUES (?1, ?2, 1, ?3)
             ON CONFLICT (weekday, hour) DO UPDATE SET
                occurrences = occurrences + 1,
                active = active + excluded.active",
            params![slot.weekday, slot.hour, active],
        )?;
        Ok(())
    }

    /// Remember how much of one elapsed hour was spent working.
    ///
    /// Judging an hour once, when it closes, keeps every later reader of that
    /// hour in agreement and saves rescanning the readings behind it.
    pub fn record_closed_hour(&self, at: Timestamp, active: f64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO usage_closed_hours (at, active) VALUES (?1, ?2)
             ON CONFLICT (at) DO UPDATE SET active = excluded.active",
            params![at.get(), active],
        )?;
        Ok(())
    }

    /// Every closed hour in a range, with the verdict it was given.
    pub fn closed_hours_between(
        &self,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<(Timestamp, f64)>> {
        let mut statement = self.conn.prepare(
            "SELECT at, active FROM usage_closed_hours WHERE at >= ?1 AND at < ?2 ORDER BY at",
        )?;
        let rows = statement.query_map(params![from.get(), to.get()], |row| {
            Ok((Timestamp::new(row.get(0)?), row.get(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Active hours across the closed hours in a range.
    pub fn active_hours_between(&self, from: Timestamp, to: Timestamp) -> Result<f64> {
        let total: Option<f64> = self.conn.query_row(
            "SELECT SUM(active) FROM usage_closed_hours WHERE at >= ?1 AND at < ?2",
            params![from.get(), to.get()],
            |row| row.get(0),
        )?;
        Ok(total.unwrap_or(0.0))
    }

    /// Scale every counter, ageing the profile toward the present.
    pub fn decay_slots(&self, factor: f64) -> Result<()> {
        self.conn.execute(
            "UPDATE usage_slots SET occurrences = occurrences * ?1, active = active * ?1",
            params![factor],
        )?;
        Ok(())
    }

    pub fn prior(&self, kind: WindowKind) -> Result<Option<Prior>> {
        Ok(self
            .conn
            .query_row(
                "SELECT lambda, weight FROM usage_priors WHERE window_kind = ?1",
                params![kind.label()],
                |row| {
                    Ok(Prior {
                        lambda: row.get(0)?,
                        weight: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn set_prior(&self, kind: WindowKind, prior: Prior) -> Result<()> {
        self.conn.execute(
            "INSERT INTO usage_priors (window_kind, lambda, weight) VALUES (?1, ?2, ?3)
             ON CONFLICT (window_kind) DO UPDATE SET
                lambda = excluded.lambda, weight = excluded.weight",
            params![kind.label(), prior.lambda, prior.weight],
        )?;
        Ok(())
    }

    /// When each vigie2 session was created, oldest first.
    pub fn sessions_created_since(&self, from: Timestamp) -> Result<Vec<Timestamp>> {
        let mut statement = self.conn.prepare(
            "SELECT unixepoch(created_at, 'subsec') AS at FROM sessions
             WHERE at >= ?1 ORDER BY at",
        )?;
        let rows =
            statement.query_map(params![from.get()], |row| Ok(Timestamp::new(row.get(0)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Keeps the largest count seen for a message: the CLI writes one
    /// `assistant` line per content block, each carrying the message's usage.
    pub fn record_message(
        &self,
        session: &str,
        at: Timestamp,
        message: &MessageTokens,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO usage_messages
                (session_id, message_id, at, model, parent_tool_use_id, tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (session_id, message_id) DO UPDATE SET
                tokens = MAX(tokens, excluded.tokens)",
            params![
                session,
                message.message_id,
                at.get(),
                message.model,
                message.parent_tool_use_id,
                message.tokens
            ],
        )?;
        Ok(())
    }

    /// Every message a session spent tokens on, in arrival order.
    pub fn session_messages(&self, session: &str) -> Result<Vec<MessageTokens>> {
        let mut statement = self.conn.prepare(
            "SELECT message_id, model, parent_tool_use_id, tokens FROM usage_messages
             WHERE session_id = ?1 ORDER BY at",
        )?;
        let rows = statement.query_map(params![session], |row| {
            Ok(MessageTokens {
                message_id: row.get(0)?,
                model: row.get(1)?,
                parent_tool_use_id: row.get(2)?,
                tokens: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn summary(&self) -> Result<Summary> {
        let (readings, oldest): (u64, Option<f64>) =
            self.conn
                .query_row("SELECT COUNT(*), MIN(at) FROM usage_readings", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?;
        let mut instances = BTreeMap::new();
        let mut priors = BTreeMap::new();
        for kind in WindowKind::ALL {
            let count: u64 = self.conn.query_row(
                "SELECT COUNT(DISTINCT resets_at) FROM usage_readings WHERE window_kind = ?1",
                params![kind.label()],
                |row| row.get(0),
            )?;
            instances.insert(kind, count);
            if let Some(prior) = self.prior(kind)? {
                priors.insert(kind, prior);
            }
        }
        Ok(Summary {
            readings,
            oldest: oldest.map(Timestamp::new),
            instances,
            priors,
            history_started: self.meta(META_HISTORY_STARTED)?.map(Timestamp::new),
        })
    }
}

/// Usage cannot fall inside one window, so a reading above a later one is a
/// stale snapshot from before the window opened. Drop those.
fn drop_stale_highs<T>(samples: &mut Vec<T>, pct: impl Fn(&T) -> f64) {
    let mut ceiling = f64::INFINITY;
    let mut keep = vec![true; samples.len()];
    for (index, sample) in samples.iter().enumerate().rev() {
        if pct(sample) > ceiling {
            keep[index] = false;
        } else {
            ceiling = pct(sample);
        }
    }
    let mut kept = keep.into_iter();
    samples.retain(|_| kept.next().unwrap_or(true));
}

#[cfg(test)]
pub mod tests {
    use super::{META_PRUNED_AT, MessageTokens, Prior, UsageDb};
    use crate::usage::slots::SlotId;
    use crate::usage::units::{Pct, Timestamp};
    use crate::usage::window::{WindowKind, WindowState};
    use rusqlite::Connection;

    const NOW: f64 = 1_789_128_000.0;

    /// An in-memory database with every store migration applied.
    pub fn connection() -> Connection {
        let connection = Connection::open_in_memory().expect("in-memory database");
        crate::store::migrate(&connection).expect("migrate");
        connection
    }

    fn state(pct: f64, resets_at: f64) -> WindowState {
        WindowState {
            used: Pct::new(pct),
            resets_at: Timestamp::new(resets_at),
        }
    }

    fn record(db: &UsageDb<'_>, kind: WindowKind, pct: f64, resets: f64, session: &str, at: f64) {
        db.record(kind, state(pct, resets), Some(session), Timestamp::new(at))
            .expect("record");
    }

    #[test]
    fn a_reading_is_stored_once_per_change() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let resets = NOW + 3600.0;
        let kind = WindowKind::FiveHour;
        assert!(
            db.record(kind, state(5.0, resets), Some("s1"), Timestamp::new(NOW))
                .expect("record")
        );
        assert!(
            !db.record(
                kind,
                state(5.0, resets),
                Some("s1"),
                Timestamp::new(NOW + 10.0)
            )
            .expect("record")
        );
        // Past the merge bucket, so this stays a second reading.
        assert!(
            db.record(
                kind,
                state(6.0, resets),
                Some("s1"),
                Timestamp::new(NOW + 60.0)
            )
            .expect("record")
        );
        let samples = db
            .window_samples(kind, Timestamp::new(resets))
            .expect("samples");
        assert_eq!(samples.len(), 2);
    }

    #[test]
    fn a_reading_without_a_session_is_deduplicated_too() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let kind = WindowKind::SevenDay;
        let first = db.record(kind, state(5.0, NOW), None, Timestamp::new(NOW));
        let again = db.record(kind, state(5.0, NOW), None, Timestamp::new(NOW + 60.0));
        assert!(first.expect("record"));
        assert!(!again.expect("record"));
    }

    #[test]
    fn two_sessions_reporting_together_merge_into_one_reading() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let resets = NOW + 3600.0;
        record(&db, WindowKind::FiveHour, 5.0, resets, "s1", NOW);
        record(&db, WindowKind::FiveHour, 7.0, resets, "s2", NOW + 5.0);
        let samples = db
            .window_samples(WindowKind::FiveHour, Timestamp::new(resets))
            .expect("samples");
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].pct.get(), 7.0);
    }

    #[test]
    fn a_stale_snapshot_before_the_window_opened_is_dropped() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let resets = NOW + 3600.0;
        // A cold-started session reports the previous window's 80% first.
        for (offset, pct) in [(0.0, 80.0), (60.0, 2.0), (120.0, 3.0), (180.0, 5.0)] {
            record(&db, WindowKind::FiveHour, pct, resets, "s1", NOW + offset);
        }
        let samples = db
            .window_samples(WindowKind::FiveHour, Timestamp::new(resets))
            .expect("samples");
        let percentages: Vec<f64> = samples.iter().map(|sample| sample.pct.get()).collect();
        assert_eq!(percentages, vec![2.0, 3.0, 5.0]);
    }

    #[test]
    fn window_instances_are_kept_apart() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        record(&db, WindowKind::FiveHour, 90.0, NOW + 60.0, "s1", NOW);
        record(
            &db,
            WindowKind::FiveHour,
            1.0,
            NOW + 18060.0,
            "s1",
            NOW + 120.0,
        );
        let fresh = db
            .window_samples(WindowKind::FiveHour, Timestamp::new(NOW + 18060.0))
            .expect("samples");
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].pct.get(), 1.0);
    }

    #[test]
    fn samples_between_spans_instances_in_time_order() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        record(&db, WindowKind::FiveHour, 90.0, NOW + 60.0, "s1", NOW);
        record(
            &db,
            WindowKind::FiveHour,
            1.0,
            NOW + 18060.0,
            "s1",
            NOW + 120.0,
        );
        let spanning = db
            .samples_between(
                WindowKind::FiveHour,
                Timestamp::new(NOW - 10.0),
                Timestamp::new(NOW + 600.0),
            )
            .expect("samples");
        assert_eq!(spanning.len(), 2);
        assert!(spanning[0].at.get() < spanning[1].at.get());
        assert_eq!(spanning[0].pct.get(), 90.0);
    }

    #[test]
    fn the_latest_reading_of_a_window_is_readable() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let resets = NOW + 3600.0;
        record(&db, WindowKind::FiveHour, 5.0, resets, "s1", NOW);
        record(&db, WindowKind::FiveHour, 9.0, resets, "s1", NOW + 60.0);
        record(&db, WindowKind::FiveHour, 9.5, resets, "other", NOW + 70.0);
        let latest = db
            .latest_reading(WindowKind::FiveHour)
            .expect("latest")
            .expect("a reading");
        assert_eq!(latest.pct.get(), 9.5);
        assert_eq!(latest.at.get(), NOW + 70.0);
        assert_eq!(
            db.latest_reading(WindowKind::SevenDay).expect("latest"),
            None
        );
        assert_eq!(
            db.earliest_reading_at().expect("earliest"),
            Some(Timestamp::new(NOW))
        );
    }

    #[test]
    fn pruning_drops_readings_past_retention_and_waits_a_day() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let resets = NOW + 3600.0;
        record(
            &db,
            WindowKind::FiveHour,
            5.0,
            resets,
            "s1",
            NOW - 20.0 * 86400.0,
        );
        record(&db, WindowKind::FiveHour, 6.0, resets, "s1", NOW);

        let removed = db
            .prune_if_due(Timestamp::new(NOW), 14)
            .expect("first prune");
        assert_eq!(removed, 1);
        assert_eq!(
            db.prune_if_due(Timestamp::new(NOW + 60.0), 14)
                .expect("second prune"),
            0
        );
        assert!(db.meta(META_PRUNED_AT).expect("meta").is_some());
    }

    #[test]
    fn slot_counters_accumulate_and_decay() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let slot = SlotId::new(2, 14);
        db.add_slot_occurrence(slot, 1.0).expect("bump");
        db.add_slot_occurrence(slot, 0.0).expect("bump");
        let counts = db.slot_counts().expect("counts");
        assert_eq!(counts[&slot].occurrences, 2.0);
        assert_eq!(counts[&slot].active, 1.0);

        db.decay_slots(0.5).expect("decay");
        let decayed = db.slot_counts().expect("counts");
        assert_eq!(decayed[&slot].occurrences, 1.0);
        assert_eq!(decayed[&slot].active, 0.5);
    }

    #[test]
    fn a_prior_round_trips_per_window() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        assert_eq!(db.prior(WindowKind::SevenDay).expect("prior"), None);
        db.set_prior(
            WindowKind::SevenDay,
            Prior {
                lambda: 1.4,
                weight: 22.0,
            },
        )
        .expect("set prior");
        db.set_prior(
            WindowKind::SevenDay,
            Prior {
                lambda: 1.6,
                weight: 30.0,
            },
        )
        .expect("set prior");
        assert_eq!(
            db.prior(WindowKind::SevenDay).expect("prior"),
            Some(Prior {
                lambda: 1.6,
                weight: 30.0
            })
        );
        assert_eq!(db.prior(WindowKind::FiveHour).expect("prior"), None);
    }

    #[test]
    fn a_message_keeps_its_largest_count() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        let message = |tokens| MessageTokens {
            message_id: "msg_1".into(),
            model: "claude-opus-5-5".into(),
            parent_tool_use_id: None,
            tokens,
        };
        for tokens in [100, 250, 250] {
            db.record_message("s1", Timestamp::new(NOW), &message(tokens))
                .expect("record message");
        }
        assert_eq!(
            db.session_messages("s1").expect("messages"),
            vec![message(250)]
        );
        assert!(db.session_messages("s2").expect("messages").is_empty());
    }

    #[test]
    fn summary_counts_what_is_stored() {
        let connection = connection();
        let db = UsageDb::new(&connection);
        db.ensure_started(Timestamp::new(NOW)).expect("start");
        record(&db, WindowKind::FiveHour, 5.0, NOW + 60.0, "s1", NOW);
        record(&db, WindowKind::SevenDay, 2.0, NOW + 86400.0, "s1", NOW);
        let summary = db.summary().expect("summary");
        assert_eq!(summary.readings, 2);
        assert_eq!(summary.instances[&WindowKind::FiveHour], 1);
        assert_eq!(summary.instances[&WindowKind::SevenDay], 1);
        assert_eq!(summary.oldest.map(Timestamp::get), Some(NOW));
        assert_eq!(summary.history_started.map(Timestamp::get), Some(NOW));
    }
}
