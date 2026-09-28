//! The live sessions and the history store behind them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{Local, Utc};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::{Mutex, watch};
use tokio::task::AbortHandle;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

use crate::agents::AgentView;
use crate::child::{Launch, ModelSettings, Program};
use crate::gauges::{self, MainRequest};
use crate::protocol::{Inbound, System};
use crate::session::{
    Activity, Config, Event, EventBody, HostEvent, LiveStatus, RestartMode, Session, StopReason,
};
use crate::store::{SessionRow, SharedCatalog, Store, StoredEvent};
use crate::usage::{SessionContext, Snapshot};
use crate::{seed, wire};

/// Period of `get_usage` samples while at least one session is live.
pub const USAGE_INTERVAL: Duration = Duration::from_secs(5 * 60);
const CHANGES_CAPACITY: usize = 256;
/// Permission mode of a new session.
const DEFAULT_MODE: &str = "default";
/// Modes a stopped session accepts; any other would only fail at the next launch.
const STORED_MODES: [&str; 3] = [DEFAULT_MODE, "acceptEdits", "plan"];
/// Values `--effort` accepts.
const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// Time the catalog probe gets to initialize and answer `get_settings`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// Cheap to clone; every clone shares the same sessions.
#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    program: Program,
    /// Sessions spawned by this manager, closed ones included until reopened.
    live: Mutex<HashMap<Uuid, Session>>,
    changes: broadcast::Sender<Uuid>,
    usage_changes: broadcast::Sender<()>,
    catalog_changes: broadcast::Sender<()>,
    sampler: AbortHandle,
}

/// Whether `SessionManager::probe_catalog` started a Claude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// The catalog and the default settings were already stored.
    Skipped,
    Ran,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.sampler.abort();
    }
}

impl SessionManager {
    /// Sessions the store still records as running are marked stopped: none has a
    /// child after a restart. Each resumes on `open`.
    pub async fn start(store: Store, program: Program) -> Result<Self> {
        store.stop_running().await?;
        let inner = Arc::new_cyclic(|inner| Inner {
            store,
            program,
            live: Mutex::new(HashMap::new()),
            changes: broadcast::channel(CHANGES_CAPACITY).0,
            usage_changes: broadcast::channel(CHANGES_CAPACITY).0,
            catalog_changes: broadcast::channel(CHANGES_CAPACITY).0,
            sampler: tokio::spawn(sample_periodically(inner.clone())).abort_handle(),
        });
        Ok(Self { inner })
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    /// Receives the id of a session each time it is spawned, changes activity or
    /// its count of pending asks, ends, or moves its cache and context gauges. The store already reflects the
    /// change when the id arrives.
    pub fn subscribe_changes(&self) -> broadcast::Receiver<Uuid> {
        self.inner.changes.subscribe()
    }

    /// Ticks after a usage reading or a finished turn is queued to the store, so
    /// a snapshot taken on receipt includes it.
    pub fn subscribe_usage(&self) -> broadcast::Receiver<()> {
        self.inner.usage_changes.subscribe()
    }

    /// Ticks after the shared catalog or default settings changed in the store.
    pub fn subscribe_catalog(&self) -> broadcast::Receiver<()> {
        self.inner.catalog_changes.subscribe()
    }

    pub async fn catalog(&self) -> Result<SharedCatalog> {
        self.inner.store.shared_catalog().await
    }

    /// With no catalog or default settings stored, starts a Claude in `dir`
    /// without a pick, stores its `initialize` catalog and its `get_settings`
    /// values, and stops it. It sends no user message.
    pub async fn probe_catalog(&self, dir: PathBuf) -> Result<Probe> {
        let stored = self.inner.store.shared_catalog().await?;
        if stored.models.is_some() && stored.defaults.is_some() {
            return Ok(Probe::Skipped);
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let id = Uuid::new_v4();
        let (session, mut events) = Session::spawn(Config {
            id,
            cwd: dir,
            launch: Launch::New(id),
            program: self.inner.program.clone(),
            history: None,
            last_seq: 0,
            permission_mode: None,
            settings: ModelSettings::default(),
        })?;
        let applied = tokio::time::timeout(PROBE_TIMEOUT, session.applied_settings()).await;
        let mut catalog = None;
        while let Ok(event) = events.try_recv() {
            if let EventBody::Host(HostEvent::Catalog { models, commands }) = &event.body {
                catalog = Some((models.clone(), commands.clone()));
            }
        }
        session.stop().await;
        let applied = applied.context("get_settings timed out")??;
        let store = &self.inner.store;
        let mut changed = store.record_default_settings(&applied).await?;
        if let Some((models, commands)) = catalog {
            changed |= store.record_catalog(&models, &commands).await?;
        }
        if changed {
            let _ = self.inner.catalog_changes.send(());
        }
        Ok(Probe::Ran)
    }

    /// The usage snapshot, reporting on every session whose child is running.
    pub async fn usage(&self) -> Result<Snapshot> {
        let live = self.statuses().await;
        let sessions = self
            .list()
            .await?
            .into_iter()
            .filter(|row| live.contains_key(&row.id))
            .map(|row| SessionContext {
                id: row.id,
                cwd: row.cwd,
                created_at: row.created_at,
                permission_mode: row.permission_mode,
            })
            .collect();
        self.inner.store.usage_snapshot(sessions, Utc::now()).await
    }

    /// Starts a new Claude session in `cwd`, with the same id on both sides.
    pub async fn create(&self, cwd: PathBuf) -> Result<Session> {
        let id = Uuid::new_v4();
        let mut live = self.inner.live.lock().await;
        let session = self.spawn(
            id,
            cwd,
            Launch::New(id),
            0,
            Some(DEFAULT_MODE.into()),
            ModelSettings::default(),
        )?;
        live.insert(id, session.clone());
        let _ = self.inner.changes.send(id);
        Ok(session)
    }

    /// Changes the permission mode: through the CLI when the child runs, else in
    /// the stored row, without spawning, for the next launch to pass.
    pub async fn set_permission_mode(&self, id: Uuid, mode: String) -> Result<()> {
        let live = self.inner.live.lock().await;
        if let Some(session) = live.get(&id).filter(|session| !session.is_closed()) {
            let session = session.clone();
            drop(live);
            session.set_permission_mode(mode).await?;
            return Ok(());
        }
        if !STORED_MODES.contains(&mode.as_str()) {
            bail!("permission mode {mode} cannot be set on a stopped session");
        }
        // Held while storing, so a concurrent `open` launches with the new mode.
        self.inner.store.set_permission_mode(id, mode).await?;
        drop(live);
        let _ = self.inner.changes.send(id);
        Ok(())
    }

    /// The session if its child is running, without spawning one.
    pub async fn live(&self, id: Uuid) -> Option<Session> {
        self.inner
            .live
            .lock()
            .await
            .get(&id)
            .filter(|session| !session.is_closed())
            .cloned()
    }

    /// Status of every session whose child is running.
    pub async fn statuses(&self) -> HashMap<Uuid, LiveStatus> {
        self.inner
            .live
            .lock()
            .await
            .iter()
            .filter(|(_, session)| !session.is_closed())
            .map(|(id, session)| (*id, *session.status().borrow()))
            .collect()
    }

    /// Subagents of every session whose child is running.
    pub async fn agents(&self) -> HashMap<Uuid, Vec<AgentView>> {
        self.inner
            .live
            .lock()
            .await
            .iter()
            .filter(|(_, session)| !session.is_closed())
            .map(|(id, session)| (*id, session.agents().borrow().clone()))
            .collect()
    }

    /// The live session, or a known ended one resumed with `--resume`.
    pub async fn open(&self, id: Uuid) -> Result<Session> {
        let mut live = self.inner.live.lock().await;
        if let Some(session) = live.get(&id).filter(|session| !session.is_closed()) {
            return Ok(session.clone());
        }
        let row = self
            .inner
            .store
            .session(id)
            .await?
            .with_context(|| format!("unknown session {id}"))?;
        let session = self.spawn(
            id,
            row.cwd,
            row.launch,
            row.last_seq,
            row.permission_mode,
            row.settings,
        )?;
        live.insert(id, session.clone());
        let _ = self.inner.changes.send(id);
        Ok(session)
    }

    /// Fails with `wire::CACHE_COLD` while the discussion's cache is cold, or a
    /// model or effort change awaits its first main request.
    pub async fn ensure_warm(&self, id: Uuid) -> Result<()> {
        let row = self
            .inner
            .store
            .session(id)
            .await?
            .with_context(|| format!("unknown session {id}"))?;
        if row.settings_changed_at.is_some()
            || gauges::is_cold(row.cache.as_ref(), row.updated_at, Utc::now())
        {
            bail!(wire::CACHE_COLD);
        }
        Ok(())
    }

    /// Stores the model and effort, `None` fields keeping theirs. When one
    /// changed and the child runs, interrupts its turn if any, then applies the
    /// change through the CLI and reads back what it applies. Going back to the CLI's default effort only takes
    /// effect at the next launch.
    pub async fn set_settings(
        &self,
        id: Uuid,
        model: Option<Option<String>>,
        effort: Option<Option<String>>,
    ) -> Result<()> {
        if let Some(Some(level)) = &effort
            && !EFFORT_LEVELS.contains(&level.as_str())
        {
            bail!("unknown effort level {level}");
        }
        if let Some(Some(name)) = &model
            && name.trim().is_empty()
        {
            bail!("blank model name");
        }
        let live = self.inner.live.lock().await;
        let row = self
            .inner
            .store
            .session(id)
            .await?
            .with_context(|| format!("unknown session {id}"))?;
        let settings = ModelSettings {
            model: model.unwrap_or(row.settings.model.clone()),
            effort: effort.unwrap_or(row.settings.effort.clone()),
        };
        // Held while storing, so a concurrent `open` launches with the new settings.
        let changed = self
            .inner
            .store
            .set_model_settings(id, settings.clone(), Utc::now())
            .await?;
        let session = live
            .get(&id)
            .filter(|session| !session.is_closed())
            .cloned();
        drop(live);
        if !changed {
            return Ok(());
        }
        let _ = self.inner.changes.send(id);
        let Some(session) = session else {
            return Ok(());
        };
        if session.status().borrow().turn {
            session.interrupt().await?;
        }
        if settings.model != row.settings.model {
            session.set_model(settings.model).await?;
        }
        if settings.effort != row.settings.effort
            && let Some(effort) = settings.effort
        {
            session.set_effort(effort).await?;
        }
        ask_settings(
            &session,
            &self.inner.store,
            &self.inner.changes,
            &self.inner.catalog_changes,
        )
        .await;
        Ok(())
    }

    /// Stops the child the graceful way, as a shutdown does; the discussion stays
    /// listed and the next send resumes it. Does nothing more when not live.
    pub async fn kill(&self, id: Uuid, reason: StopReason) -> Result<()> {
        let mut live = self.inner.live.lock().await;
        let session = live.remove(&id);
        if session.is_none() && self.inner.store.session(id).await?.is_none() {
            bail!("unknown session {id}");
        }
        if let Some(session) = session {
            session.stop_for(reason).await;
        }
        drop(live);
        let _ = self.inner.changes.send(id);
        Ok(())
    }

    /// Fails with `wire::SESSION_LIVE` while the child runs.
    pub async fn archive(&self, id: Uuid) -> Result<()> {
        let live = self.inner.live.lock().await;
        if live.get(&id).is_some_and(|session| !session.is_closed()) {
            bail!(wire::SESSION_LIVE);
        }
        // Held while storing, so a concurrent `open` cannot launch in between.
        self.inner.store.set_archived(id, Some(Utc::now())).await?;
        drop(live);
        let _ = self.inner.changes.send(id);
        Ok(())
    }

    pub async fn restore(&self, id: Uuid) -> Result<()> {
        self.inner.store.set_archived(id, None).await?;
        let _ = self.inner.changes.send(id);
        Ok(())
    }

    /// Restores the discussion when archived; fails for an unknown one.
    pub async fn unarchive(&self, id: Uuid) -> Result<()> {
        let row = self
            .inner
            .store
            .session(id)
            .await?
            .with_context(|| format!("unknown session {id}"))?;
        if row.archived_at.is_some() {
            self.restore(id).await?;
        }
        Ok(())
    }

    /// Moves the discussion to a fresh Claude session: stops its child, records a
    /// `Restarted` event, then launches the new session with the seed and `text`
    /// as its first message (continue) or `text` alone (blank). A blank restart
    /// without text launches nothing; the next send does.
    pub async fn restart(
        &self,
        id: Uuid,
        mode: RestartMode,
        text: String,
        client_id: Option<String>,
    ) -> Result<()> {
        let mut live = self.inner.live.lock().await;
        if let Some(session) = live.remove(&id) {
            session.stop().await;
        }
        let store = &self.inner.store;
        let row = store
            .session(id)
            .await?
            .with_context(|| format!("unknown session {id}"))?;
        let history = seed::History::from_events(&store.events_after(id, 0, u32::MAX).await?);
        let seed = match mode {
            RestartMode::Continue => {
                Some(history.seed(&row.cwd, row.created_at.with_timezone(&Local).date_naive()))
            }
            RestartMode::Blank | RestartMode::Clear => None,
        };
        let claude_session = Uuid::new_v4();
        store.restart_claude_session(id, claude_session).await?;
        let seq = row.last_seq + 1;
        store.record_event(
            id,
            &Event {
                seq,
                time: Utc::now(),
                body: EventBody::Host(HostEvent::Restarted {
                    mode,
                    exchanges: seed.as_ref().map_or(0, |seed| seed.exchanges),
                    total_exchanges: history.total_exchanges(),
                    seed: seed.as_ref().map(|seed| seed.text.clone()),
                }),
            },
        );
        let launches = mode == RestartMode::Continue || !text.trim().is_empty();
        let session = if launches {
            let session = self.spawn(
                id,
                row.cwd,
                Launch::New(claude_session),
                seq,
                row.permission_mode,
                row.settings,
            )?;
            live.insert(id, session.clone());
            Some(session)
        } else {
            None
        };
        drop(live);
        let _ = self.inner.changes.send(id);
        match (session, seed) {
            (Some(session), Some(seed)) => session.send_seeded(seed.text, text, client_id).await,
            (Some(session), None) => session.send(text, client_id).await,
            (None, _) => Ok(()),
        }
    }

    /// Most recently updated first.
    pub async fn list(&self) -> Result<Vec<SessionRow>> {
        self.inner.store.sessions().await
    }

    /// Includes every event already broadcast by the session.
    pub async fn events_after(&self, id: Uuid, seq: u64, limit: u32) -> Result<Vec<StoredEvent>> {
        self.inner.store.events_after(id, seq, limit).await
    }

    /// Stops every live session and waits for their history to be written.
    pub async fn shutdown(&self) -> Result<()> {
        let sessions: Vec<Session> = self
            .inner
            .live
            .lock()
            .await
            .drain()
            .map(|(_, session)| session)
            .collect();
        futures_util::future::join_all(
            sessions
                .iter()
                .map(|session| session.stop_for(StopReason::Shutdown)),
        )
        .await;
        self.inner.store.flush().await
    }

    fn spawn(
        &self,
        id: Uuid,
        cwd: PathBuf,
        launch: Launch,
        last_seq: u64,
        permission_mode: Option<String>,
        settings: ModelSettings,
    ) -> Result<Session> {
        let (session, events) = Session::spawn(Config {
            id,
            cwd,
            launch,
            program: self.inner.program.clone(),
            history: Some(self.inner.store.clone()),
            last_seq,
            permission_mode,
            settings,
        })?;
        tokio::spawn(sample_after_turns(
            id,
            session.status(),
            events,
            Arc::downgrade(&self.inner),
            self.inner.usage_changes.clone(),
            self.inner.changes.clone(),
            self.inner.catalog_changes.clone(),
        ));
        tokio::spawn(announce_changes(
            id,
            session.status(),
            session.agents(),
            self.inner.changes.clone(),
        ));
        let (first_ask, store, changes, catalog_changes) = (
            session.clone(),
            self.inner.store.clone(),
            self.inner.changes.clone(),
            self.inner.catalog_changes.clone(),
        );
        tokio::spawn(async move {
            ask_context(&first_ask, &store, &changes).await;
            ask_settings(&first_ask, &store, &changes, &catalog_changes).await;
        });
        Ok(session)
    }
}

/// The status channel closes when the session actor ends, after its final
/// state is queued to the store.
async fn announce_changes(
    id: Uuid,
    mut status: watch::Receiver<LiveStatus>,
    mut agents: watch::Receiver<Vec<AgentView>>,
    changes: broadcast::Sender<Uuid>,
) {
    loop {
        let changed = tokio::select! {
            changed = status.changed() => changed,
            changed = agents.changed() => changed,
        };
        if changed.is_err() {
            break;
        }
        let _ = changes.send(id);
    }
    let _ = changes.send(id);
}

/// Samples usage and asks the context and the applied settings each time the session goes idle after a
/// turn, until it ends. Announces the rate limit readings and finished turns the
/// session reports, and each main-conversation request, which moves its cache and
/// context gauges.
/// A lagged receiver may have missed that moment, so a lag counts as one. Holds
/// no `Session` handle, so it never keeps a session alive.
async fn sample_after_turns(
    id: Uuid,
    status: watch::Receiver<LiveStatus>,
    mut events: broadcast::Receiver<Arc<Event>>,
    inner: Weak<Inner>,
    usage_changes: broadcast::Sender<()>,
    changes: broadcast::Sender<Uuid>,
    catalog_changes: broadcast::Sender<()>,
) {
    let mut last_request: Option<String> = None;
    loop {
        let turn_ended = match events.recv().await {
            Ok(event) => match &event.body {
                EventBody::Host(HostEvent::ChildExited { .. }) => return,
                EventBody::Host(HostEvent::Catalog { models, commands }) => {
                    let Some(store) = inner.upgrade().map(|inner| inner.store.clone()) else {
                        return;
                    };
                    let recorded = store.record_catalog(models, commands).await;
                    announce_catalog(recorded, &catalog_changes, "catalog");
                    false
                }
                EventBody::Line(line) => match &line.parsed {
                    Some(Inbound::System(System::Init(init))) => {
                        let Some(store) = inner.upgrade().map(|inner| inner.store.clone()) else {
                            return;
                        };
                        let recorded = store
                            .record_terminal_commands(&init.terminal_slash_commands)
                            .await;
                        announce_catalog(recorded, &catalog_changes, "terminal-only commands");
                        false
                    }
                    Some(Inbound::Result(_)) => {
                        let _ = usage_changes.send(());
                        let _ = changes.send(id);
                        true
                    }
                    Some(Inbound::RateLimitEvent(_)) => {
                        let _ = usage_changes.send(());
                        false
                    }
                    Some(Inbound::Assistant(assistant)) => {
                        if let Some(request) = MainRequest::from_assistant(assistant)
                            && last_request.as_ref() != Some(&request.message_id)
                        {
                            last_request = Some(request.message_id);
                            let _ = changes.send(id);
                        }
                        false
                    }
                    _ => false,
                },
                EventBody::Host(_) => false,
            },
            Err(RecvError::Lagged(_)) => {
                let _ = usage_changes.send(());
                let _ = changes.send(id);
                true
            }
            Err(RecvError::Closed) => return,
        };
        if !turn_ended || status.borrow().activity != Activity::Idle {
            continue;
        }
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let session = inner.live.lock().await.get(&id).cloned();
        let store = inner.store.clone();
        drop(inner);
        if let Some(session) = session {
            sample(&session, &store, &usage_changes).await;
            ask_context(&session, &store, &changes).await;
            ask_settings(&session, &store, &changes, &catalog_changes).await;
        }
    }
}

fn announce_catalog(recorded: Result<bool>, catalog_changes: &broadcast::Sender<()>, what: &str) {
    match recorded {
        Ok(true) => {
            let _ = catalog_changes.send(());
        }
        Ok(false) => {}
        Err(err) => tracing::warn!("store the shared {what}: {err:#}"),
    }
}

/// Stores the CLI's context usage and announces the session when it changed.
async fn ask_context(session: &Session, store: &Store, changes: &broadcast::Sender<Uuid>) {
    let id = session.id();
    let reply = match session.context_usage().await {
        Ok(reply) => reply,
        Err(err) => {
            tracing::debug!("get_context_usage for session {id}: {err:#}");
            return;
        }
    };
    match store.record_context_usage(id, reply).await {
        Ok(true) => {
            let _ = changes.send(id);
        }
        Ok(false) => {}
        Err(err) => tracing::warn!("store the context of session {id}: {err:#}"),
    }
}

/// Stores the model and effort the CLI applies and announces the session when
/// they changed. A child launched without a pick, whose discussion still has
/// none, also reports the shared default settings.
async fn ask_settings(
    session: &Session,
    store: &Store,
    changes: &broadcast::Sender<Uuid>,
    catalog_changes: &broadcast::Sender<()>,
) {
    let id = session.id();
    let applied = match session.applied_settings().await {
        Ok(applied) => applied,
        Err(err) => {
            tracing::debug!("get_settings for session {id}: {err:#}");
            return;
        }
    };
    if session.runs_without_pick() {
        match store.session(id).await {
            Ok(Some(row)) if row.settings == ModelSettings::default() => {
                let recorded = store.record_default_settings(&applied).await;
                announce_catalog(recorded, catalog_changes, "default settings");
            }
            Ok(_) => {}
            Err(err) => tracing::warn!("read session {id}: {err:#}"),
        }
    }
    match store.record_applied_settings(id, applied).await {
        Ok(true) => {
            let _ = changes.send(id);
        }
        Ok(false) => {}
        Err(err) => tracing::warn!("store the applied settings of session {id}: {err:#}"),
    }
}

async fn sample_periodically(inner: Weak<Inner>) {
    let mut ticks = tokio::time::interval(USAGE_INTERVAL);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticks.tick().await;
    loop {
        ticks.tick().await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let session = inner
            .live
            .lock()
            .await
            .values()
            .find(|session| !session.is_closed())
            .cloned();
        let store = inner.store.clone();
        let usage_changes = inner.usage_changes.clone();
        drop(inner);
        if let Some(session) = session {
            sample(&session, &store, &usage_changes).await;
        }
    }
}

async fn sample(session: &Session, store: &Store, usage_changes: &broadcast::Sender<()>) {
    match session.usage_json().await {
        Ok(response) => {
            store.record_usage(Utc::now(), Some(session.id()), response.to_string());
            let _ = usage_changes.send(());
        }
        Err(err) => tracing::debug!("get_usage for session {}: {err:#}", session.id()),
    }
}
