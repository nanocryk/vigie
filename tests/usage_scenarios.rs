//! Scripted timelines through the whole snapshot path: readings in, recorded,
//! folded, projected. Each test is one situation the usage module has to survive.

use chrono::{DateTime, FixedOffset, Utc};
use rusqlite::Connection;
use uuid::Uuid;
use vigie::usage::db::{MessageTokens, UsageDb};
use vigie::usage::mix::Family;
use vigie::usage::units::{Pct, Timestamp};
use vigie::usage::window::{WindowKind, WindowState};
use vigie::usage::{self, SessionContext, Snapshot, WindowReport};

/// 2026-09-14 00:00:00 UTC, a Monday.
const MONDAY: f64 = 1_789_344_000.0;
const HOUR: f64 = 3600.0;
const MAIN: Uuid = Uuid::from_u128(1);
const COLD: Uuid = Uuid::from_u128(2);
const FIRST: Uuid = Uuid::from_u128(3);
const SECOND: Uuid = Uuid::from_u128(4);

fn utc() -> FixedOffset {
    FixedOffset::east_opt(0).expect("utc offset")
}

struct Harness {
    connection: Connection,
}

impl Harness {
    fn new() -> Self {
        let connection = Connection::open_in_memory().expect("in-memory database");
        vigie::store::migrate(&connection).expect("migrate");
        Self { connection }
    }

    fn db(&self) -> UsageDb<'_> {
        UsageDb::new(&self.connection)
    }

    /// One reading of both windows, then a snapshot at the same instant.
    fn refresh(
        &self,
        at: f64,
        session: Uuid,
        five_hour: (f64, f64),
        seven_day: (f64, f64),
    ) -> Snapshot {
        for (kind, (used, resets_at)) in [
            (WindowKind::FiveHour, five_hour),
            (WindowKind::SevenDay, seven_day),
        ] {
            let state = WindowState {
                used: Pct::new(used),
                resets_at: Timestamp::new(resets_at),
            };
            self.db()
                .record(kind, state, Some(session), Timestamp::new(at))
                .expect("record");
        }
        self.snapshot(at, &[])
    }

    fn snapshot(&self, at: f64, sessions: &[SessionContext]) -> Snapshot {
        usage::snapshot(&self.db(), sessions, Timestamp::new(at), &utc()).expect("snapshot")
    }

    fn stored_samples(&self, kind: WindowKind, resets_at: f64) -> usize {
        self.db()
            .window_samples(kind, Timestamp::new(resets_at))
            .expect("window samples")
            .len()
    }

    /// A stretch of steady work, one reading every five minutes.
    fn work(&self, from: f64, to: f64, five: &mut (f64, f64), seven: &mut (f64, f64)) {
        let mut at = from;
        while at < to {
            five.0 += 14.0 * (300.0 / HOUR);
            seven.0 += 1.33 * (300.0 / HOUR);
            self.refresh(at, MAIN, *five, *seven);
            at += 300.0;
        }
    }
}

fn window(snapshot: &Snapshot, kind: WindowKind) -> &WindowReport {
    match kind {
        WindowKind::FiveHour => snapshot.five_hour.as_ref(),
        WindowKind::SevenDay => snapshot.seven_day.as_ref(),
    }
    .expect("a window report")
}

fn projected(snapshot: &Snapshot, kind: WindowKind) -> f64 {
    window(snapshot, kind).projected
}

fn instant(at: f64) -> DateTime<Utc> {
    Timestamp::new(at).to_datetime().expect("an instant")
}

#[test]
fn a_first_refresh_projects_both_windows() {
    let harness = Harness::new();
    let snapshot = harness.refresh(
        MONDAY + 9.0 * HOUR,
        MAIN,
        (2.0, MONDAY + 14.0 * HOUR),
        (1.0, MONDAY + 7.0 * 86400.0),
    );
    assert!(projected(&snapshot, WindowKind::FiveHour) >= 2.0);
    assert!(projected(&snapshot, WindowKind::SevenDay) >= 1.0);
    let five_hour = window(&snapshot, WindowKind::FiveHour);
    assert_eq!(five_hour.used, 2.0);
    assert_eq!(five_hour.resets_at, instant(MONDAY + 14.0 * HOUR));
    assert_eq!(five_hour.reading_at, instant(MONDAY + 9.0 * HOUR));
    assert_eq!(snapshot.newest_reading, Some(instant(MONDAY + 9.0 * HOUR)));
}

#[test]
fn closing_the_editor_for_two_hours_does_not_inflate_the_rate() {
    let harness = Harness::new();
    let mut five = (0.0, MONDAY + 14.0 * HOUR);
    let mut seven = (0.0, MONDAY + 7.0 * 86400.0);

    harness.work(
        MONDAY + 9.0 * HOUR,
        MONDAY + 10.0 * HOUR,
        &mut five,
        &mut seven,
    );
    let before = harness.refresh(MONDAY + 10.0 * HOUR, MAIN, five, seven);

    // Two hours away from the keyboard, then back at the same pace.
    let resume = MONDAY + 12.0 * HOUR;
    harness.work(resume, resume + 600.0, &mut five, &mut seven);
    let after = harness.refresh(resume + 600.0, MAIN, five, seven);

    let before_rate = window(&before, WindowKind::FiveHour).rate;
    let after_rate = window(&after, WindowKind::FiveHour).rate;
    assert!(
        after_rate < before_rate * 2.0,
        "the hourly rate jumped from {before_rate:.1} to {after_rate:.1} across an idle gap"
    );
}

#[test]
fn a_window_reset_carries_nothing_across() {
    let harness = Harness::new();
    let mut five = (0.0, MONDAY + 14.0 * HOUR);
    let mut seven = (0.0, MONDAY + 7.0 * 86400.0);
    harness.work(
        MONDAY + 9.0 * HOUR,
        MONDAY + 13.0 * HOUR,
        &mut five,
        &mut seven,
    );
    let before = harness.refresh(MONDAY + 13.0 * HOUR, MAIN, five, seven);
    assert!(projected(&before, WindowKind::FiveHour) > 40.0);

    // The window resets: usage returns to zero against a new reset instant.
    let fresh = (0.5, MONDAY + 19.0 * HOUR);
    let after = harness.refresh(MONDAY + 14.0 * HOUR + 300.0, MAIN, fresh, seven);

    assert_eq!(
        harness.stored_samples(WindowKind::FiveHour, fresh.1),
        1,
        "the new window starts from its own readings"
    );
    assert!(
        projected(&after, WindowKind::FiveHour) < projected(&before, WindowKind::FiveHour),
        "the spent window's value followed the reset"
    );
}

#[test]
fn a_window_whose_reset_has_passed_is_not_reported() {
    let harness = Harness::new();
    harness.refresh(
        MONDAY + 9.0 * HOUR,
        MAIN,
        (40.0, MONDAY + 10.0 * HOUR),
        (5.0, MONDAY + 7.0 * 86400.0),
    );
    let later = harness.snapshot(MONDAY + 11.0 * HOUR, &[]);
    assert_eq!(later.five_hour, None);
    assert!(later.seven_day.is_some());
    assert_eq!(later.newest_reading, Some(instant(MONDAY + 9.0 * HOUR)));
}

#[test]
fn a_stale_reading_at_session_start_leaves_no_trace() {
    let start = MONDAY + 9.0 * HOUR;
    let five_reset = MONDAY + 14.0 * HOUR;
    let seven = (4.0, MONDAY + 7.0 * 86400.0);

    // A cold-started session reports the previous window's 85% first, then
    // corrects itself on the next reading.
    let stale = Harness::new();
    stale.refresh(start, COLD, (85.0, five_reset), seven);
    let corrected = stale.refresh(start + 60.0, COLD, (3.0, five_reset), seven);

    // The same session without the bad first reading.
    let clean = Harness::new();
    let honest = clean.refresh(start + 60.0, COLD, (3.0, five_reset), seven);

    assert_eq!(stale.stored_samples(WindowKind::FiveHour, five_reset), 1);
    let (corrected, honest) = (
        projected(&corrected, WindowKind::FiveHour),
        projected(&honest, WindowKind::FiveHour),
    );
    assert!(
        (corrected - honest).abs() < 0.5,
        "the stale reading moved the projection from {honest:.1}% to {corrected:.1}%"
    );
}

#[test]
fn a_first_ever_refresh_reads_as_on_budget() {
    // With no history the projection leans on the budget-neutral prior, so a
    // fresh window reads as spending about the whole budget.
    let start = MONDAY + 9.0 * HOUR;
    let harness = Harness::new();
    // Both windows open at this instant, so the whole budget is still ahead.
    let snapshot = harness.refresh(
        start,
        FIRST,
        (0.0, start + 5.0 * HOUR),
        (0.0, start + 7.0 * 86400.0),
    );
    for kind in WindowKind::ALL {
        let projected = projected(&snapshot, kind);
        assert!(
            (99.0..=101.0).contains(&projected),
            "{} projected {projected:.1}% on a first ever run",
            kind.label()
        );
        assert!((window(&snapshot, kind).rate - 1.0).abs() < 0.01);
    }
}

#[test]
fn two_sessions_reporting_the_same_window_agree() {
    let harness = Harness::new();
    let five_reset = MONDAY + 14.0 * HOUR;
    let seven = (4.0, MONDAY + 7.0 * 86400.0);

    harness.refresh(MONDAY + 9.0 * HOUR, FIRST, (10.0, five_reset), seven);
    let both = harness.refresh(MONDAY + 9.0 * HOUR + 5.0, SECOND, (10.0, five_reset), seven);

    assert_eq!(
        harness.stored_samples(WindowKind::FiveHour, five_reset),
        1,
        "concurrent reports of one window merged into one reading"
    );
    assert!(projected(&both, WindowKind::FiveHour) >= 10.0);
}

#[test]
fn a_heavy_window_says_what_is_left_in_its_own_unit() {
    let harness = Harness::new();
    let mut five = (0.0, MONDAY + 14.0 * HOUR);
    let mut seven = (0.0, MONDAY + 7.0 * 86400.0);
    // Four times the steady pace on both windows.
    let mut at = MONDAY + 9.0 * HOUR;
    while at < MONDAY + 11.0 * HOUR {
        five.0 += 40.0 * (300.0 / HOUR);
        seven.0 += 15.0 * (300.0 / HOUR);
        harness.refresh(at, MAIN, five, seven);
        at += 300.0;
    }
    let snapshot = harness.refresh(at, MAIN, five, seven);

    let five_hour = window(&snapshot, WindowKind::FiveHour);
    assert!(five_hour.projected > 100.0, "{}", five_hour.projected);
    let work = five_hour
        .work_left_hours
        .expect("work left on the 5h window");
    assert!(work > 0.0 && work < 3.0, "work left {work}");
    assert_eq!(five_hour.deadline, None);

    let seven_day = window(&snapshot, WindowKind::SevenDay);
    assert!(seven_day.projected > 100.0, "{}", seven_day.projected);
    let deadline = seven_day.deadline.expect("a deadline on the 7d window");
    assert!(deadline > instant(at) && deadline < seven_day.resets_at);
    assert_eq!(seven_day.work_left_hours, None);
}

#[test]
fn the_model_mix_counts_each_message_once() {
    let harness = Harness::new();
    let id = Uuid::new_v4();
    // One message written as three content blocks, all carrying the same usage.
    let message = MessageTokens {
        message_id: "msg_1".into(),
        model: "claude-opus-5-5".into(),
        parent_tool_use_id: None,
        tokens: 110,
    };
    for _ in 0..3 {
        harness
            .db()
            .record_message(id, Timestamp::new(MONDAY), &message)
            .expect("record message");
    }
    let subagent = MessageTokens {
        message_id: "msg_2".into(),
        model: "claude-haiku-4-5".into(),
        parent_tool_use_id: Some("toolu_1".into()),
        tokens: 110,
    };
    harness
        .db()
        .record_message(id, Timestamp::new(MONDAY), &subagent)
        .expect("record message");

    let context = SessionContext {
        id,
        cwd: std::env::temp_dir(),
        created_at: instant(MONDAY),
        permission_mode: Some("default".into()),
    };
    let snapshot = harness.snapshot(MONDAY + 60.0, &[context]);
    let report = &snapshot.sessions[0];
    assert_eq!(report.id, id);
    assert!(!report.bypass);
    let shares: Vec<(Family, f64)> = report
        .model_mix
        .iter()
        .map(|share| (share.family, share.share))
        .collect();
    assert_eq!(shares, [(Family::Opus, 0.5), (Family::Haiku, 0.5)]);
    assert_eq!(report.subagents.count, 1);
    assert_eq!(report.subagents.share, 0.5);
    let texts: Vec<&str> = report.facts.iter().map(|fact| fact.text.as_str()).collect();
    assert!(
        texts.contains(&"50% Opus, 50% Haiku this session."),
        "{texts:?}"
    );
}

#[test]
fn a_days_work_leaves_the_snapshot_well_formed_throughout() {
    let harness = Harness::new();
    let mut five = (0.0, MONDAY + 14.0 * HOUR);
    let mut seven = (0.0, MONDAY + 7.0 * 86400.0);

    let mut at = MONDAY + 9.0 * HOUR;
    while at < MONDAY + 13.0 * HOUR {
        five.0 += 14.0 * (300.0 / HOUR);
        seven.0 += 1.33 * (300.0 / HOUR);
        let snapshot = harness.refresh(at, MAIN, five, seven);
        let projected = projected(&snapshot, WindowKind::SevenDay);
        assert!(
            projected.is_finite() && projected >= seven.0,
            "7d projection {projected} at {at}"
        );
        assert_eq!(snapshot.history.occupancy.len(), 7);
        at += 300.0;
    }
    let snapshot = harness.snapshot(at, &[]);
    assert!(snapshot.history.readings > 90);
    assert!(snapshot.history.observed_occurrences >= 3.0);
    serde_json::to_value(&snapshot).expect("the snapshot serializes");
}
