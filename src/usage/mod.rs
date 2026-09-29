//! Account usage: the 5h and 7d rate-limit windows, projected to their reset.
//!
//! Readings come from `get_usage` samples and `rate_limit_event` lines, and
//! per-message tokens from `assistant` lines; the store keeps them as they
//! arrive. A snapshot folds the elapsed hours into the activity profile, the
//! finished windows into the intensity priors, and projects both windows.

pub mod db;
pub mod estimate;
pub mod facts;
pub mod history;
pub mod inputs;
pub mod mix;
pub mod profile;
pub mod slots;
pub mod units;
pub mod window;

use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::claude::protocol::PermissionMode;
use crate::usage::db::UsageDb;
use crate::usage::estimate::Confidence;
use crate::usage::facts::{Fact, Moment};
use crate::usage::mix::{Family, Mix};
use crate::usage::profile::Profile;
use crate::usage::slots::{DAYS_PER_WEEK, HOURS_PER_DAY, SlotId};
use crate::usage::units::{Pct, Timestamp};
use crate::usage::window::{WindowKind, WindowState};

/// Days of raw readings kept; the profile ages by decay instead.
pub const RETENTION_DAYS: u32 = 14;

/// Body of `GET /api/usage`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    pub generated_at: DateTime<Utc>,
    /// Newest reading of either window.
    pub newest_reading: Option<DateTime<Utc>>,
    /// `None` without a reading, or once the window of the newest reading has reset.
    pub five_hour: Option<WindowReport>,
    pub seven_day: Option<WindowReport>,
    /// Account-wide facts.
    pub facts: Vec<Fact>,
    /// One per session given to the snapshot.
    pub sessions: Vec<SessionReport>,
    pub history: HistoryReport,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowReport {
    /// Percent of the window's budget spent, as of `reading_at`.
    pub used: f64,
    pub resets_at: DateTime<Utc>,
    /// Percent expected at reset.
    pub projected: f64,
    pub confidence: Confidence,
    /// Intensity over the one that spends exactly one budget per window; 1 is on budget.
    pub rate: f64,
    /// 5h only, while projected over 100: hours of work the budget still buys.
    pub work_left_hours: Option<f64>,
    /// 7d only: when usage is projected to reach 100, if before the reset.
    pub deadline: Option<DateTime<Utc>>,
    /// Time of the newest reading of this window.
    pub reading_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionReport {
    pub id: Uuid,
    /// The session runs with permission prompts skipped.
    pub bypass: bool,
    /// Share of the session's tokens per model family, in family order.
    pub model_mix: Vec<FamilyShare>,
    pub subagents: Subagents,
    pub facts: Vec<Fact>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FamilyShare {
    pub family: Family,
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Subagents {
    pub count: u32,
    /// Share of the session's tokens spent inside subagents.
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryReport {
    pub readings: u64,
    pub oldest_reading: Option<DateTime<Utc>>,
    pub since: Option<DateTime<Utc>>,
    pub five_hour: HistoryWindow,
    pub seven_day: HistoryWindow,
    /// Decayed slot occurrences behind the profile.
    pub observed_occurrences: f64,
    /// Probability of working per local weekday (Monday first) and hour.
    pub occupancy: Vec<[f64; HOURS_PER_DAY]>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryWindow {
    /// Window instances seen in the retained readings.
    pub instances: u64,
    /// Percent of the budget per active hour carried from finished windows.
    pub prior_intensity: Option<f64>,
    /// Active hours behind that intensity.
    pub prior_weight: Option<f64>,
}

/// What the snapshot needs to know about one session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionContext {
    pub id: Uuid,
    pub cwd: PathBuf,
    pub created_at: DateTime<Utc>,
    pub permission_mode: Option<PermissionMode>,
}

/// Fold what has elapsed, prune old readings, and project both windows.
pub fn snapshot<Tz: TimeZone>(
    db: &UsageDb<'_>,
    sessions: &[SessionContext],
    now: Timestamp,
    zone: &Tz,
) -> Result<Snapshot> {
    db.ensure_started(now)?;
    let profile = history::refresh(db, now, zone)?;
    db.prune_if_due(now, RETENTION_DAYS)?;

    let mut newest: Option<Timestamp> = None;
    let mut five_hour_state = None;
    let mut reports = [None, None];
    for (slot, kind) in reports.iter_mut().zip(WindowKind::ALL) {
        let Some(latest) = db.latest_reading(kind)? else {
            continue;
        };
        newest = Some(newest.map_or(latest.at, |seen| seen.later_of(latest.at)));
        if latest.resets_at.get() <= now.get() {
            continue;
        }
        let state = WindowState {
            used: latest.pct,
            resets_at: latest.resets_at,
        };
        if kind == WindowKind::FiveHour {
            five_hour_state = Some(state);
        }
        *slot = window_report(db, &profile, kind, state, latest.at, now, zone)?;
    }
    let [five_hour, seven_day] = reports;

    let sessions = sessions
        .iter()
        .map(|context| session_report(db, context, now))
        .collect::<Result<Vec<_>>>()?;

    Ok(Snapshot {
        generated_at: to_datetime(now),
        newest_reading: newest.and_then(Timestamp::to_datetime),
        five_hour,
        seven_day,
        facts: facts::account(db, &profile, five_hour_state, now, zone)?,
        sessions,
        history: history_report(db, &profile)?,
    })
}

fn window_report<Tz: TimeZone>(
    db: &UsageDb<'_>,
    profile: &Profile,
    kind: WindowKind,
    state: WindowState,
    reading_at: Timestamp,
    now: Timestamp,
    zone: &Tz,
) -> Result<Option<WindowReport>> {
    let samples = db.window_samples(kind, state.resets_at)?;
    // Evidence starts at the first reading of this window: what was spent
    // before it was watched says nothing about the pace of the work.
    let watched_from = samples.first().map(|sample| sample.at).unwrap_or(now);
    let observed_used = match (samples.first(), samples.last()) {
        (Some(first), Some(last)) => last.pct - first.pct,
        _ => Pct::new(0.0),
    };
    let active_observed = history::active_hours_since(db, watched_from, now, profile, zone)?;
    let estimate = estimate::project(
        &estimate::Inputs {
            kind,
            used: state.used,
            resets_at: state.resets_at,
            now,
            profile,
            prior: db.prior(kind)?,
            observed_used,
            active_observed,
        },
        zone,
    );
    // Each window says what is left in the unit its length makes readable:
    // five hours hold no night, so the work itself is the answer there, while
    // a week holds several and the date is what a plan hangs on.
    let over_budget = estimate.projected.get() > 100.0;
    let (work_left_hours, deadline) = match kind {
        WindowKind::FiveHour => (
            estimate
                .work_remaining
                .filter(|_| over_budget)
                .map(|work| work.get()),
            None,
        ),
        WindowKind::SevenDay => (
            None,
            estimate
                .seconds_to_limit
                .and_then(|seconds| Timestamp::new(now.get() + seconds).to_datetime()),
        ),
    };
    let (Some(resets_at), Some(reading_at)) =
        (state.resets_at.to_datetime(), reading_at.to_datetime())
    else {
        return Ok(None);
    };
    Ok(Some(WindowReport {
        used: state.used.get(),
        resets_at,
        projected: estimate.projected.get(),
        confidence: estimate.confidence,
        rate: estimate.relative_rate(),
        work_left_hours,
        deadline,
        reading_at,
    }))
}

fn session_report(
    db: &UsageDb<'_>,
    context: &SessionContext,
    now: Timestamp,
) -> Result<SessionReport> {
    let mix = Mix::from_messages(&db.session_messages(context.id)?);
    let moment = Moment {
        cwd: Some(context.cwd.clone()),
        session_started: Some(Timestamp::from_datetime(context.created_at)),
    };
    Ok(SessionReport {
        id: context.id,
        bypass: context.permission_mode == Some(PermissionMode::BypassPermissions),
        model_mix: mix
            .shares
            .iter()
            .map(|(family, share)| FamilyShare {
                family: *family,
                share: *share,
            })
            .collect(),
        subagents: Subagents {
            count: mix.subagent_count,
            share: mix.subagent_share,
        },
        facts: facts::session(&mix, &moment, now),
    })
}

fn history_report(db: &UsageDb<'_>, profile: &Profile) -> Result<HistoryReport> {
    let summary = db.summary()?;
    let window = |kind| {
        let prior = summary.priors.get(&kind);
        HistoryWindow {
            instances: summary.instances.get(&kind).copied().unwrap_or(0),
            prior_intensity: prior.map(|prior| prior.lambda),
            prior_weight: prior.map(|prior| prior.weight),
        }
    };
    Ok(HistoryReport {
        readings: summary.readings,
        oldest_reading: summary.oldest.and_then(Timestamp::to_datetime),
        since: summary.history_started.and_then(Timestamp::to_datetime),
        five_hour: window(WindowKind::FiveHour),
        seven_day: window(WindowKind::SevenDay),
        observed_occurrences: profile.observed_occurrences(),
        occupancy: (0..DAYS_PER_WEEK as u8)
            .map(|weekday| {
                std::array::from_fn(|hour| profile.occupancy(SlotId::new(weekday, hour as u8)))
            })
            .collect(),
    })
}

fn to_datetime(at: Timestamp) -> DateTime<Utc> {
    at.to_datetime().unwrap_or_default()
}
