//! The `sessions` table: one row per discussion, with its state and gauges.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::held::{HeldMessage, read_held};
use super::{Store, format_time, json_error, known_session, parse_time};
use crate::claude::child::{Effort, Launch, ModelSettings};
use crate::claude::protocol::{AppliedSettings, ContextUsageResponse, PermissionMode};
use crate::session::GateFailure;
use crate::session::advice::RestartMeasure;
use crate::session::gauges::{self, Cache, ContextSource};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionState {
    Running,
    /// The host closed the child's stdin.
    Stopped,
    /// The child exited without being asked to.
    Exited {
        code: Option<i32>,
    },
    GateFailed {
        reason: GateFailure,
    },
}

/// What happened in a discussion since the user last looked at it; a stop outranks a finished turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unread {
    /// A turn ended and nothing runs.
    Finished,
    /// The child stopped without being asked to.
    Stopped,
}

impl SessionState {
    /// The `state` column, as serde names the variant.
    fn name(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Exited { .. } => "exited",
            Self::GateFailed { .. } => "gate_failed",
        }
    }
}

impl Unread {
    const ALL: [Self; 2] = [Self::Finished, Self::Stopped];

    /// The `unread` column, as serde names the variant.
    fn as_str(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Stopped => "stopped",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|unread| unread.as_str() == text)
            .with_context(|| format!("unknown unread state {text}"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: Uuid,
    pub cwd: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Seq of the latest event, 0 when there is none.
    pub last_seq: u64,
    pub state: SessionState,
    /// Last known CLI permission mode; `None` for sessions from before it was stored.
    pub permission_mode: Option<PermissionMode>,
    /// `None` until a main request reported the TTL of its cache write.
    pub cache: Option<Cache>,
    /// `None` until a `get_context_usage` reply, or a main request and a
    /// `result`, reported the total and the window.
    pub context: Option<gauges::Context>,
    /// How the next child picks the discussion's current Claude session.
    pub launch: Launch,
    /// Display name; `None` shows the folder name.
    pub name: Option<String>,
    /// `None` while the discussion is not archived.
    pub archived_at: Option<DateTime<Utc>>,
    /// Passed at every launch.
    pub settings: ModelSettings,
    /// When a model or effort change made the cache count cold; `None` once a
    /// main request or a restart followed.
    pub settings_changed_at: Option<DateTime<Utc>>,
    /// What the CLI last reported applying; kept once the child stops, cleared
    /// by a change of the stored settings until the next report.
    pub applied: AppliedSettings,
    /// `None` once the user saw the discussion.
    pub unread: Option<Unread>,
    /// Main-conversation requests per finished turn; `None` before the first turn.
    pub calls_per_message: Option<f64>,
    /// What a restart would keep, measured after each turn; `None` before.
    pub restart: Option<RestartMeasure>,
    /// In queue order.
    pub held: Vec<HeldMessage>,
}

impl Store {
    /// Inserts the session, or marks a known one running again. A `None` mode keeps
    /// the stored one. The Claude session stays fresh until `claude_session_used`:
    /// the CLI writes no conversation before its first message.
    pub fn session_started(
        &self,
        id: Uuid,
        cwd: &Path,
        time: DateTime<Utc>,
        permission_mode: Option<PermissionMode>,
    ) {
        let cwd = cwd.to_string_lossy().into_owned();
        let permission_mode = permission_mode.map(String::from);
        self.write("session start", move |connection| {
            let time = format_time(time);
            connection.execute(
                "INSERT INTO sessions (id, cwd, created_at, updated_at, state, permission_mode,
                     claude_session_fresh)
                 VALUES (?1, ?2, ?3, ?3, 'running', ?4, 1)
                 ON CONFLICT (id) DO UPDATE SET
                     state = 'running',
                     exit_code = NULL, gate_failure = NULL,
                     permission_mode = COALESCE(excluded.permission_mode, permission_mode)",
                params![id.to_string(), cwd, time, permission_mode],
            )?;
            Ok(())
        });
    }

    /// A message reached the current Claude session: later launches resume it.
    pub fn claude_session_used(&self, id: Uuid) {
        self.write("claude session used", move |connection| {
            connection.execute(
                "UPDATE sessions SET claude_session_fresh = 0
                 WHERE id = ?1 AND claude_session_fresh = 1",
                params![id.to_string()],
            )?;
            Ok(())
        });
    }

    /// Points the discussion at `claude_session`, which no child launched yet, run
    /// in `cwd`, and forgets the gauges of the previous one. Counts as activity, so the
    /// fresh cache is not judged by the old idle time. Fails for an unknown session.
    pub async fn restart_claude_session(
        &self,
        session: Uuid,
        claude_session: Uuid,
        cwd: &Path,
    ) -> Result<()> {
        let cwd = cwd.to_string_lossy().into_owned();
        let now = format_time(Utc::now());
        self.call(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET claude_session_id = ?2, claude_session_fresh = 1, cwd = ?3,
                     updated_at = ?4,
                     cache_message_id = NULL, cache_request_at = NULL, cache_ttl_secs = NULL,
                     context_model = NULL, context_tokens = NULL, context_window = NULL,
                     context_source = NULL, context_max_tokens = NULL,
                     context_autocompact = NULL, context_categories = NULL,
                     settings_changed_at = NULL
                 WHERE id = ?1",
                params![session.to_string(), claude_session.to_string(), cwd, now],
            )?;
            known_session(changed, session)
        })
        .await
    }

    /// Archives the discussion at `at`, or unarchives it for `None`. Resolves once
    /// stored; fails for an unknown session.
    pub async fn set_archived(&self, session: Uuid, at: Option<DateTime<Utc>>) -> Result<()> {
        self.call(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET archived_at = ?2 WHERE id = ?1",
                params![session.to_string(), at.map(format_time)],
            )?;
            known_session(changed, session)
        })
        .await
    }

    /// Sets the display name, or clears it for `None`. Fails for an unknown session.
    pub async fn set_name(&self, session: Uuid, name: Option<String>) -> Result<()> {
        self.call(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET name = ?2 WHERE id = ?1",
                params![session.to_string(), name],
            )?;
            known_session(changed, session)
        })
        .await
    }

    /// Raises the unread state to `unread`, never lowering it. Resolves to whether it changed.
    pub async fn mark_unread(&self, session: Uuid, unread: Unread) -> Result<bool> {
        self.call(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET unread = ?2
                 WHERE id = ?1 AND (unread IS NULL OR (unread = 'finished' AND ?2 = 'stopped'))",
                params![session.to_string(), unread.as_str()],
            )?;
            Ok(changed > 0)
        })
        .await
    }

    /// The user saw the discussion. Resolves to whether it was unread.
    pub async fn mark_seen(&self, session: Uuid) -> Result<bool> {
        self.call(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET unread = NULL WHERE id = ?1 AND unread IS NOT NULL",
                params![session.to_string()],
            )?;
            Ok(changed > 0)
        })
        .await
    }

    /// Resolves once stored; fails for an unknown session.
    pub async fn set_permission_mode(&self, session: Uuid, mode: PermissionMode) -> Result<()> {
        self.call(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET permission_mode = ?2 WHERE id = ?1",
                params![session.to_string(), mode.as_str()],
            )?;
            known_session(changed, session)
        })
        .await
    }

    /// Stores the model and effort; when either differs from the stored one, the
    /// cache counts cold from `at` and the applied values are forgotten. Resolves
    /// to whether they changed; fails for an unknown session.
    pub async fn set_model_settings(
        &self,
        session: Uuid,
        settings: ModelSettings,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        self.call(move |connection| {
            let id = session.to_string();
            let changed = connection.execute(
                "UPDATE sessions SET model = ?2, effort = ?3, settings_changed_at = ?4,
                     applied_model = NULL, applied_effort = NULL
                 WHERE id = ?1 AND (model IS NOT ?2 OR effort IS NOT ?3)",
                params![
                    id,
                    settings.model,
                    settings.effort.map(Effort::as_str),
                    format_time(at)
                ],
            )?;
            if changed > 0 {
                return Ok(true);
            }
            let known = connection
                .query_row("SELECT 1 FROM sessions WHERE id = ?1", [&id], |_| Ok(()))
                .optional()?;
            if known.is_none() {
                bail!("unknown session {session}");
            }
            Ok(false)
        })
        .await
    }

    pub fn set_state(&self, session: Uuid, state: SessionState) {
        self.write("session state", move |connection| {
            let (exit_code, gate_failure) = match &state {
                SessionState::Running | SessionState::Stopped => (None, None),
                SessionState::Exited { code } => (*code, None),
                SessionState::GateFailed { reason } => (
                    None,
                    Some(serde_json::to_string(reason).map_err(json_error)?),
                ),
            };
            connection.execute(
                "UPDATE sessions SET state = ?2, exit_code = ?3, gate_failure = ?4 WHERE id = ?1",
                params![session.to_string(), state.name(), exit_code, gate_failure],
            )?;
            Ok(())
        });
    }

    /// Marks every session still recorded as running as stopped: after a restart
    /// none of them has a child.
    pub async fn stop_running(&self) -> Result<()> {
        self.call(|connection| {
            connection.execute(
                "UPDATE sessions SET state = 'stopped' WHERE state = 'running'",
                [],
            )?;
            Ok(())
        })
        .await
    }

    /// Stores a `get_context_usage` reply as the session's context; resolves to
    /// whether that changed the stored values.
    pub async fn record_context_usage(
        &self,
        session: Uuid,
        reply: ContextUsageResponse,
    ) -> Result<bool> {
        let categories = serde_json::to_string(&reply.categories)?;
        self.call(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET context_source = 'cli', context_tokens = ?2,
                     context_max_tokens = ?3, context_autocompact = ?4, context_categories = ?5
                 WHERE id = ?1 AND NOT (context_source IS 'cli' AND context_tokens IS ?2
                     AND context_max_tokens IS ?3 AND context_autocompact IS ?4
                     AND context_categories IS ?5)",
                params![
                    session.to_string(),
                    reply.total_tokens,
                    reply.max_tokens,
                    reply.auto_compact_threshold,
                    categories
                ],
            )?;
            Ok(changed > 0)
        })
        .await
    }

    /// Stores what a restart would keep; resolves to whether that changed the stored values.
    pub async fn record_restart_measure(
        &self,
        session: Uuid,
        measure: RestartMeasure,
    ) -> Result<bool> {
        self.call(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET restart_window_tokens = ?2, restart_seed_tokens = ?3,
                     restart_reread_tokens = ?4, restart_other_cost = ?5
                 WHERE id = ?1 AND NOT (restart_window_tokens IS ?2 AND restart_seed_tokens IS ?3
                     AND restart_reread_tokens IS ?4 AND restart_other_cost IS ?5)",
                params![
                    session.to_string(),
                    measure.window_start_tokens,
                    measure.seed_tokens,
                    measure.reread_tokens,
                    measure.other_cost_per_call
                ],
            )?;
            Ok(changed > 0)
        })
        .await
    }

    /// Stores the model and effort the CLI reports applying; resolves to whether
    /// that changed the stored values.
    pub async fn record_applied_settings(
        &self,
        session: Uuid,
        applied: AppliedSettings,
    ) -> Result<bool> {
        self.call(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET applied_model = ?2, applied_effort = ?3
                 WHERE id = ?1 AND (applied_model IS NOT ?2 OR applied_effort IS NOT ?3)",
                params![session.to_string(), applied.model, applied.effort],
            )?;
            Ok(changed > 0)
        })
        .await
    }

    /// Most recently updated first.
    pub async fn sessions(&self) -> Result<Vec<SessionRow>> {
        self.call(|connection| {
            let mut held = read_held(connection, "", params![])?;
            let mut statement =
                connection.prepare(&format!("{SESSION_SELECT} ORDER BY updated_at DESC"))?;
            let rows = statement.query_map([], RawSession::read)?;
            rows.map(|row| row?.parse(&mut held)).collect()
        })
        .await
    }

    pub async fn session(&self, id: Uuid) -> Result<Option<SessionRow>> {
        self.call(move |connection| {
            let mut held = read_held(connection, "WHERE session_id = ?1", [id.to_string()])?;
            connection
                .query_row(
                    &format!("{SESSION_SELECT} WHERE id = ?1"),
                    [id.to_string()],
                    RawSession::read,
                )
                .optional()?
                .map(|row| row.parse(&mut held))
                .transpose()
        })
        .await
    }

    /// Like `session`, failing for an unknown one.
    pub async fn require_session(&self, id: Uuid) -> Result<SessionRow> {
        self.session(id)
            .await?
            .with_context(|| format!("unknown session {id}"))
    }
}

const SESSION_SELECT: &str = "SELECT id, cwd, created_at, updated_at, last_seq, state,
    exit_code, gate_failure, permission_mode, cache_request_at, cache_ttl_secs, context_tokens,
    context_window, context_source, context_max_tokens, context_autocompact, context_categories,
    claude_session_id, claude_session_fresh, archived_at, model, effort, settings_changed_at,
    applied_model, applied_effort, unread, main_requests, turns, restart_window_tokens,
    restart_seed_tokens, restart_reread_tokens, restart_other_cost, name FROM sessions";

/// A row of `SESSION_SELECT`, read by column name.
struct RawSession {
    id: String,
    cwd: String,
    created_at: String,
    updated_at: String,
    last_seq: u64,
    state: String,
    exit_code: Option<i32>,
    gate_failure: Option<String>,
    permission_mode: Option<String>,
    cache_request_at: Option<String>,
    cache_ttl_secs: Option<u32>,
    context_tokens: Option<u64>,
    context_window: Option<u64>,
    context_source: Option<String>,
    context_max_tokens: Option<u64>,
    context_autocompact: Option<u64>,
    context_categories: Option<String>,
    claude_session_id: Option<String>,
    claude_session_fresh: bool,
    archived_at: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    settings_changed_at: Option<String>,
    applied_model: Option<String>,
    applied_effort: Option<String>,
    unread: Option<String>,
    main_requests: u64,
    turns: u64,
    restart_window_tokens: Option<u64>,
    restart_seed_tokens: Option<u64>,
    restart_reread_tokens: Option<u64>,
    restart_other_cost: Option<f64>,
    name: Option<String>,
}

impl RawSession {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            cwd: row.get("cwd")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            last_seq: row.get("last_seq")?,
            state: row.get("state")?,
            exit_code: row.get("exit_code")?,
            gate_failure: row.get("gate_failure")?,
            permission_mode: row.get("permission_mode")?,
            cache_request_at: row.get("cache_request_at")?,
            cache_ttl_secs: row.get("cache_ttl_secs")?,
            context_tokens: row.get("context_tokens")?,
            context_window: row.get("context_window")?,
            context_source: row.get("context_source")?,
            context_max_tokens: row.get("context_max_tokens")?,
            context_autocompact: row.get("context_autocompact")?,
            context_categories: row.get("context_categories")?,
            claude_session_id: row.get("claude_session_id")?,
            claude_session_fresh: row.get("claude_session_fresh")?,
            archived_at: row.get("archived_at")?,
            model: row.get("model")?,
            effort: row.get("effort")?,
            settings_changed_at: row.get("settings_changed_at")?,
            applied_model: row.get("applied_model")?,
            applied_effort: row.get("applied_effort")?,
            unread: row.get("unread")?,
            main_requests: row.get("main_requests")?,
            turns: row.get("turns")?,
            restart_window_tokens: row.get("restart_window_tokens")?,
            restart_seed_tokens: row.get("restart_seed_tokens")?,
            restart_reread_tokens: row.get("restart_reread_tokens")?,
            restart_other_cost: row.get("restart_other_cost")?,
            name: row.get("name")?,
        })
    }

    /// Moves the row's messages out of `held`.
    fn parse(self, held: &mut HashMap<Uuid, Vec<HeldMessage>>) -> Result<SessionRow> {
        let state = match self.state.as_str() {
            "running" => SessionState::Running,
            "stopped" => SessionState::Stopped,
            "exited" => SessionState::Exited {
                code: self.exit_code,
            },
            "gate_failed" => SessionState::GateFailed {
                reason: serde_json::from_str(
                    self.gate_failure
                        .as_deref()
                        .context("gate_failed without a reason")?,
                )?,
            },
            other => bail!("unknown session state {other}"),
        };
        let cache = match (self.cache_request_at, self.cache_ttl_secs) {
            (Some(at), Some(ttl_secs)) => Some(Cache {
                last_request_at: parse_time(&at)?,
                ttl_secs,
            }),
            _ => None,
        };
        let source = match self.context_source.as_deref() {
            Some("cli") => ContextSource::Cli,
            _ => ContextSource::Estimate,
        };
        let categories = self
            .context_categories
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        let context = self
            .context_tokens
            .zip(self.context_max_tokens.or(self.context_window))
            .and_then(|(used, max)| gauges::Context::new(used, max, source))
            .map(|context| gauges::Context {
                autocompact_threshold: self.context_autocompact,
                categories,
                ..context
            });
        let id = Uuid::parse_str(&self.id)?;
        let claude_session = match self.claude_session_id.as_deref() {
            Some(text) => Uuid::parse_str(text)?,
            None => id,
        };
        let launch = if self.claude_session_fresh {
            Launch::New(claude_session)
        } else {
            Launch::Resume(claude_session)
        };
        Ok(SessionRow {
            id,
            cwd: PathBuf::from(self.cwd),
            created_at: parse_time(&self.created_at)?,
            updated_at: parse_time(&self.updated_at)?,
            last_seq: self.last_seq,
            state,
            permission_mode: self.permission_mode.map(PermissionMode::from),
            cache,
            context,
            launch,
            name: self.name,
            archived_at: self.archived_at.as_deref().map(parse_time).transpose()?,
            settings: ModelSettings {
                model: self.model,
                effort: self.effort.as_deref().map(str::parse).transpose()?,
            },
            settings_changed_at: self
                .settings_changed_at
                .as_deref()
                .map(parse_time)
                .transpose()?,
            applied: AppliedSettings {
                model: self.applied_model,
                effort: self.applied_effort,
            },
            unread: self.unread.as_deref().map(Unread::parse).transpose()?,
            calls_per_message: (self.turns > 0)
                .then(|| self.main_requests as f64 / self.turns as f64),
            restart: match (
                self.restart_window_tokens,
                self.restart_seed_tokens,
                self.restart_reread_tokens,
                self.restart_other_cost,
            ) {
                (Some(window), Some(seed), Some(reread), Some(other)) => Some(RestartMeasure {
                    window_start_tokens: window,
                    seed_tokens: seed,
                    reread_tokens: reread,
                    other_cost_per_call: other,
                }),
                _ => None,
            },
            held: held.remove(&id).unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Fixture, fixture};
    use super::*;

    #[test]
    fn stored_names_are_the_serde_names() {
        for unread in Unread::ALL {
            assert_eq!(serde_json::to_value(unread).unwrap(), unread.as_str());
            assert_eq!(Unread::parse(unread.as_str()).unwrap(), unread);
        }
        let states = [
            SessionState::Running,
            SessionState::Stopped,
            SessionState::Exited { code: Some(1) },
            SessionState::GateFailed {
                reason: GateFailure::Overage,
            },
        ];
        for state in states {
            assert_eq!(serde_json::to_value(&state).unwrap()["kind"], state.name());
        }
    }

    #[tokio::test]
    async fn a_restart_measure_survives_a_reopen() {
        let Fixture {
            dir: _dir,
            path,
            store,
            id,
        } = fixture();
        assert_eq!(store.require_session(id).await.unwrap().restart, None);
        let measure = RestartMeasure {
            window_start_tokens: 90_000,
            seed_tokens: 2_000,
            reread_tokens: 10_000,
            other_cost_per_call: 1_234.5,
        };
        assert!(
            store
                .record_restart_measure(id, measure.clone())
                .await
                .unwrap()
        );
        assert!(
            !store
                .record_restart_measure(id, measure.clone())
                .await
                .unwrap()
        );
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(
            reopened.require_session(id).await.unwrap().restart,
            Some(measure)
        );
    }

    fn context_reply(total_tokens: u64) -> ContextUsageResponse {
        serde_json::from_value(serde_json::json!({
            "totalTokens": total_tokens, "maxTokens": 967_000, "rawMaxTokens": 1_000_000,
            "percentage": 2, "autoCompactThreshold": 900_000, "apiUsage": null,
            "categories": [{"name": "Messages", "tokens": total_tokens, "color": "x", "kind": "used"}],
            "skills": [{"name": "unread"}]
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn cli_context_until_the_next_request_and_across_a_reopen() {
        let Fixture {
            dir: _dir,
            path,
            store,
            id,
        } = fixture();
        assert!(
            store
                .record_context_usage(id, context_reply(12_000))
                .await
                .unwrap()
        );
        assert!(
            !store
                .record_context_usage(id, context_reply(12_000))
                .await
                .unwrap()
        );
        let context = store.session(id).await.unwrap().unwrap().context.unwrap();
        assert_eq!(context.source, ContextSource::Cli);
        assert_eq!(context.used_tokens, 12_000);
        assert_eq!(context.max_tokens, 967_000);
        assert_eq!(context.autocompact_threshold, Some(900_000));
        assert_eq!(
            context.categories,
            [crate::claude::protocol::ContextCategory {
                name: "Messages".into(),
                tokens: 12_000,
                kind: "used".into(),
            }]
        );

        // The `cd` recording ends on a prompt of 31048 over a 1000000 window.
        store.record_recording(id, "cd", 0, Utc::now());
        let context = store.session(id).await.unwrap().unwrap().context.unwrap();
        assert_eq!(context.source, ContextSource::Estimate);
        assert_eq!(context.used_tokens, 31_048);
        assert_eq!(context.max_tokens, 967_000);
        assert_eq!(context.autocompact_threshold, Some(900_000));

        assert!(
            store
                .record_context_usage(id, context_reply(31_048))
                .await
                .unwrap()
        );
        drop(store);
        let reopened = Store::open(&path).unwrap();
        let context = reopened
            .session(id)
            .await
            .unwrap()
            .unwrap()
            .context
            .unwrap();
        assert_eq!(context.source, ContextSource::Cli);
        assert_eq!(context.used_tokens, 31_048);
        assert_eq!(context.max_tokens, 967_000);
        assert_eq!(context.categories[0].tokens, 31_048);
    }

    #[tokio::test]
    async fn a_stop_outranks_a_finished_turn_until_seen() {
        let Fixture { store, id, .. } = fixture();
        let unread = async || store.session(id).await.unwrap().unwrap().unread;
        assert_eq!(unread().await, None);
        assert!(store.mark_unread(id, Unread::Finished).await.unwrap());
        assert!(store.mark_unread(id, Unread::Stopped).await.unwrap());
        assert!(!store.mark_unread(id, Unread::Finished).await.unwrap());
        assert_eq!(unread().await, Some(Unread::Stopped));
        assert!(store.mark_seen(id).await.unwrap());
        assert!(!store.mark_seen(id).await.unwrap());
        assert_eq!(unread().await, None);
    }

    #[tokio::test]
    async fn a_session_without_requests_has_no_gauges() {
        let Fixture { store, id, .. } = fixture();
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.cache, None);
        assert_eq!(row.context, None);
    }

    #[tokio::test]
    async fn model_settings_mark_a_change_until_the_next_request_and_survive_a_reopen() {
        let Fixture {
            dir: _dir,
            path,
            store,
            id,
        } = fixture();
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.settings, ModelSettings::default());
        assert_eq!(row.settings_changed_at, None);

        let settings = ModelSettings {
            model: Some("sonnet".into()),
            effort: Some(Effort::High),
        };
        let at = parse_time("2026-09-28T10:00:00Z").unwrap();
        assert!(
            store
                .set_model_settings(id, settings.clone(), at)
                .await
                .unwrap()
        );
        let later = at + chrono::Duration::minutes(1);
        assert!(
            !store
                .set_model_settings(id, settings.clone(), later)
                .await
                .unwrap(),
            "the same settings changed nothing"
        );
        assert!(
            store
                .set_model_settings(Uuid::new_v4(), settings.clone(), at)
                .await
                .is_err()
        );
        drop(store);

        let store = Store::open(&path).unwrap();
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.settings, settings);
        assert_eq!(row.settings_changed_at, Some(at));

        store.record_recording(id, "cd", 0, Utc::now());
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.settings_changed_at, None, "a main request followed");
        assert_eq!(row.settings, settings);

        let effort_only = ModelSettings {
            effort: Some(Effort::Low),
            ..settings.clone()
        };
        assert!(store.set_model_settings(id, effort_only, at).await.unwrap());
        store
            .restart_claude_session(id, Uuid::new_v4(), Path::new("/moved"))
            .await
            .unwrap();
        let row = store.session(id).await.unwrap().unwrap();
        assert_eq!(row.settings_changed_at, None, "a restart followed");
        assert_eq!(row.cwd, Path::new("/moved"));
        assert_eq!(row.settings.effort, Some(Effort::Low));
    }

    #[tokio::test]
    async fn applied_settings_survive_a_reopen_and_clear_on_a_change() {
        let Fixture {
            dir: _dir,
            path,
            store,
            id,
        } = fixture();
        assert_eq!(
            store.session(id).await.unwrap().unwrap().applied,
            AppliedSettings::default()
        );
        let applied = AppliedSettings {
            model: Some("claude-opus-5-5[1m]".into()),
            effort: Some("medium".into()),
        };
        assert!(
            store
                .record_applied_settings(id, applied.clone())
                .await
                .unwrap()
        );
        assert!(
            !store
                .record_applied_settings(id, applied.clone())
                .await
                .unwrap(),
            "the same values changed nothing"
        );
        drop(store);

        let store = Store::open(&path).unwrap();
        assert_eq!(store.session(id).await.unwrap().unwrap().applied, applied);
        let settings = ModelSettings {
            model: Some("sonnet".into()),
            effort: None,
        };
        assert!(
            store
                .set_model_settings(id, settings, Utc::now())
                .await
                .unwrap()
        );
        assert_eq!(
            store.session(id).await.unwrap().unwrap().applied,
            AppliedSettings::default()
        );
    }
}
