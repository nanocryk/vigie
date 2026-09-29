//! The live sessions and the history store behind them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, TimeDelta, Utc};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::{Mutex, watch};
use tokio::task::AbortHandle;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

use crate::claude::child::{Effort, Launch, ModelSettings, Program};
use crate::claude::protocol::{Inbound, PermissionMode, System};
use crate::http::wire;
use crate::session::advice::RestartMeasure;
use crate::session::agents::AgentView;
use crate::session::background::BackgroundView;
use crate::session::gauges::{self, MainRequest};
use crate::session::seed;
use crate::session::{
    Activity, Config, Event, EventBody, HostEvent, LiveStatus, RestartMode, Session, StopReason,
};
use crate::store::{SessionRow, SessionState, SharedCatalog, Store, StoredEvent, Unread};
use crate::usage::{SessionContext, Snapshot};

/// Period of `get_usage` samples while at least one session is live.
pub const USAGE_INTERVAL: Duration = Duration::from_secs(5 * 60);
const CHANGES_CAPACITY: usize = 256;
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
    signals: Signals,
    sampler: AbortHandle,
    cache_watch: AbortHandle,
    /// What each client connection shows, by connection.
    presence: parking_lot::Mutex<HashMap<Uuid, Presence>>,
    notices: broadcast::Sender<Notice>,
    seen: broadcast::Sender<Uuid>,
}

/// Something a discussion wants the user for, raised while no active client has it on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notice {
    pub session: Uuid,
    pub kind: NoticeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// An ask was queued, main or subagent.
    Ask,
    /// A turn ended and nothing runs.
    Finished,
    /// The child stopped without being asked to.
    Stopped,
    /// The 1 hour cache goes cold in `CACHE_WARNING_BEFORE`.
    CacheSoonCold,
}

/// How long before a 1 hour cache goes cold its warning comes.
pub const CACHE_WARNING_BEFORE: TimeDelta = TimeDelta::minutes(10);
const CACHE_WATCH_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
struct Presence {
    session: Option<Uuid>,
    /// Page visible and recent input.
    active: bool,
}

impl Inner {
    /// An active client has discussion `id` on screen.
    fn watched(&self, id: Uuid) -> bool {
        self.presence
            .lock()
            .values()
            .any(|presence| presence.active && presence.session == Some(id))
    }
}

/// The store of a manager still alive.
fn store_of(inner: &Weak<Inner>) -> Option<Store> {
    inner.upgrade().map(|inner| inner.store.clone())
}

/// Session `id` of `live` if its child is still running.
fn running(live: &HashMap<Uuid, Session>, id: Uuid) -> Option<&Session> {
    live.get(&id).filter(|session| !session.is_closed())
}

/// The broadcasts announcing what changed in the store; sent after the write
/// is queued, so a read on receipt sees it.
#[derive(Clone)]
struct Signals {
    sessions: broadcast::Sender<Uuid>,
    usage: broadcast::Sender<()>,
    catalog: broadcast::Sender<()>,
}

impl Signals {
    fn new() -> Self {
        Self {
            sessions: broadcast::channel(CHANGES_CAPACITY).0,
            usage: broadcast::channel(CHANGES_CAPACITY).0,
            catalog: broadcast::channel(CHANGES_CAPACITY).0,
        }
    }

    fn session(&self, id: Uuid) {
        let _ = self.sessions.send(id);
    }

    fn usage(&self) {
        let _ = self.usage.send(());
    }

    fn catalog(&self) {
        let _ = self.catalog.send(());
    }
}

/// Runs `announce` when a store write changed something; logs a failed write.
fn after_write(written: Result<bool>, what: std::fmt::Arguments<'_>, announce: impl FnOnce()) {
    match written {
        Ok(true) => announce(),
        Ok(false) => {}
        Err(err) => tracing::warn!("{what}: {err:#}"),
    }
}

/// What a running child reports.
#[derive(Debug, Clone)]
pub struct LiveView {
    pub status: LiveStatus,
    pub agents: Vec<AgentView>,
    pub background: Vec<BackgroundView>,
}

impl LiveView {
    fn of(session: &Session) -> Self {
        Self {
            status: *session.status().borrow(),
            agents: session.agents().borrow().clone(),
            background: session.background().borrow().clone(),
        }
    }
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
        self.cache_watch.abort();
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
            signals: Signals::new(),
            sampler: tokio::spawn(sample_periodically(inner.clone())).abort_handle(),
            cache_watch: tokio::spawn(warn_cold_caches(inner.clone())).abort_handle(),
            presence: parking_lot::Mutex::new(HashMap::new()),
            notices: broadcast::channel(CHANGES_CAPACITY).0,
            seen: broadcast::channel(CHANGES_CAPACITY).0,
        });
        Ok(Self { inner })
    }

    pub fn subscribe_notices(&self) -> broadcast::Receiver<Notice> {
        self.inner.notices.subscribe()
    }

    /// Receives a discussion's id each time an active client starts showing it.
    pub fn subscribe_seen(&self) -> broadcast::Receiver<Uuid> {
        self.inner.seen.subscribe()
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    /// Records what client connection `viewer` shows. An active viewer on a
    /// discussion marks it seen.
    pub fn set_presence(&self, viewer: Uuid, session: Option<Uuid>, active: bool) {
        self.inner
            .presence
            .lock()
            .insert(viewer, Presence { session, active });
        let Some(id) = session.filter(|_| active) else {
            return;
        };
        let _ = self.inner.seen.send(id);
        let (store, signals) = (self.inner.store.clone(), self.inner.signals.clone());
        tokio::spawn(async move {
            after_write(
                store.mark_seen(id).await,
                format_args!("mark session {id} seen"),
                || signals.session(id),
            );
        });
    }

    pub fn drop_presence(&self, viewer: Uuid) {
        self.inner.presence.lock().remove(&viewer);
    }

    /// Receives the id of a session each time it is spawned, changes activity or
    /// its count of pending asks, ends, or moves its cache and context gauges. The store already reflects the
    /// change when the id arrives.
    pub fn subscribe_changes(&self) -> broadcast::Receiver<Uuid> {
        self.inner.signals.sessions.subscribe()
    }

    /// Ticks after a usage reading or a finished turn is queued to the store, so
    /// a snapshot taken on receipt includes it.
    pub fn subscribe_usage(&self) -> broadcast::Receiver<()> {
        self.inner.signals.usage.subscribe()
    }

    /// Ticks after the shared catalog or default settings changed in the store.
    pub fn subscribe_catalog(&self) -> broadcast::Receiver<()> {
        self.inner.signals.catalog.subscribe()
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
            history: None,
            ..self.config(id, dir, Launch::New(id))
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
            self.inner.signals.catalog();
        }
        Ok(Probe::Ran)
    }

    /// The usage snapshot, reporting on every session whose child is running.
    pub async fn usage(&self) -> Result<Snapshot> {
        let live = self.live_views().await;
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
        let session = self.spawn(Config {
            permission_mode: Some(PermissionMode::Default),
            ..self.config(id, cwd, Launch::New(id))
        })?;
        live.insert(id, session.clone());
        self.inner.signals.session(id);
        Ok(session)
    }

    /// Changes the permission mode: through the CLI when the child runs, else in
    /// the stored row, without spawning, for the next launch to pass.
    pub async fn set_permission_mode(&self, id: Uuid, mode: PermissionMode) -> Result<()> {
        let live = self.inner.live.lock().await;
        if let Some(session) = running(&live, id).cloned() {
            drop(live);
            session.set_permission_mode(mode).await?;
            return Ok(());
        }
        if !mode.is_storable() {
            bail!("permission mode {mode} cannot be set on a stopped session");
        }
        // Held while storing, so a concurrent `open` launches with the new mode.
        self.inner.store.set_permission_mode(id, mode).await?;
        drop(live);
        self.inner.signals.session(id);
        Ok(())
    }

    /// The session if its child is running, without spawning one.
    pub async fn live(&self, id: Uuid) -> Option<Session> {
        running(&*self.inner.live.lock().await, id).cloned()
    }

    /// What every running child reports, taken under one lock.
    pub async fn live_views(&self) -> HashMap<Uuid, LiveView> {
        self.inner
            .live
            .lock()
            .await
            .iter()
            .filter(|(_, session)| !session.is_closed())
            .map(|(id, session)| (*id, LiveView::of(session)))
            .collect()
    }

    /// What the session's child reports; `None` when none runs.
    pub async fn live_view(&self, id: Uuid) -> Option<LiveView> {
        running(&*self.inner.live.lock().await, id).map(LiveView::of)
    }

    /// The live session, or a known ended one resumed with `--resume`.
    pub async fn open(&self, id: Uuid) -> Result<Session> {
        let mut live = self.inner.live.lock().await;
        if let Some(session) = running(&live, id) {
            return Ok(session.clone());
        }
        let row = self.inner.store.require_session(id).await?;
        let session = self.spawn(Config {
            last_seq: row.last_seq,
            permission_mode: row.permission_mode,
            settings: row.settings,
            ..self.config(id, row.cwd, row.launch)
        })?;
        live.insert(id, session.clone());
        self.inner.signals.session(id);
        Ok(session)
    }

    /// Fails with `wire::CACHE_COLD` while the discussion's cache is cold, or a
    /// model or effort change awaits its first main request.
    pub async fn ensure_warm(&self, id: Uuid) -> Result<()> {
        let row = self.inner.store.require_session(id).await?;
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
        let effort = effort
            .map(|level| level.as_deref().map(str::parse::<Effort>).transpose())
            .transpose()?;
        if let Some(Some(name)) = &model
            && name.trim().is_empty()
        {
            bail!("blank model name");
        }
        let live = self.inner.live.lock().await;
        let row = self.inner.store.require_session(id).await?;
        let settings = ModelSettings {
            model: model.unwrap_or(row.settings.model.clone()),
            effort: effort.unwrap_or(row.settings.effort),
        };
        // Held while storing, so a concurrent `open` launches with the new settings.
        let changed = self
            .inner
            .store
            .set_model_settings(id, settings.clone(), Utc::now())
            .await?;
        let session = running(&live, id).cloned();
        drop(live);
        if !changed {
            return Ok(());
        }
        self.inner.signals.session(id);
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
        ask_settings(&session, &self.inner.store, &self.inner.signals).await;
        Ok(())
    }

    /// Stops the child the graceful way, as a shutdown does; the discussion stays
    /// listed and the next send resumes it. Does nothing more when not live.
    pub async fn kill(&self, id: Uuid, reason: StopReason) -> Result<()> {
        let mut live = self.inner.live.lock().await;
        let session = live.remove(&id);
        if session.is_none() {
            self.inner.store.require_session(id).await?;
        }
        if let Some(session) = session {
            session.stop_for(reason).await;
        }
        drop(live);
        self.inner.signals.session(id);
        Ok(())
    }

    /// Fails with `wire::SESSION_LIVE` while the child runs.
    pub async fn archive(&self, id: Uuid) -> Result<()> {
        let live = self.inner.live.lock().await;
        if running(&live, id).is_some() {
            bail!(wire::SESSION_LIVE);
        }
        // Held while storing, so a concurrent `open` cannot launch in between.
        self.inner.store.set_archived(id, Some(Utc::now())).await?;
        drop(live);
        self.inner.signals.session(id);
        Ok(())
    }

    pub async fn restore(&self, id: Uuid) -> Result<()> {
        self.inner.store.set_archived(id, None).await?;
        self.inner.signals.session(id);
        Ok(())
    }

    /// Restores the discussion when archived; fails for an unknown one.
    pub async fn unarchive(&self, id: Uuid) -> Result<()> {
        let row = self.inner.store.require_session(id).await?;
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
        let row = store.require_session(id).await?;
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
            let session = self.spawn(Config {
                last_seq: seq,
                permission_mode: row.permission_mode,
                settings: row.settings,
                ..self.config(id, row.cwd, Launch::New(claude_session))
            })?;
            live.insert(id, session.clone());
            Some(session)
        } else {
            None
        };
        drop(live);
        self.inner.signals.session(id);
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

    /// A recorded launch of session `id`, in the CLI's default mode and settings.
    fn config(&self, id: Uuid, cwd: PathBuf, launch: Launch) -> Config {
        Config {
            id,
            cwd,
            launch,
            program: self.inner.program.clone(),
            history: Some(self.inner.store.clone()),
            last_seq: 0,
            permission_mode: None,
            settings: ModelSettings::default(),
        }
    }

    fn spawn(&self, config: Config) -> Result<Session> {
        let id = config.id;
        let (session, events) = Session::spawn(config)?;
        let signals = &self.inner.signals;
        tokio::spawn(sample_after_turns(
            id,
            session.status(),
            events,
            Arc::downgrade(&self.inner),
            signals.clone(),
        ));
        tokio::spawn(announce_changes(&session, signals.clone()));
        tokio::spawn(track_attention(
            id,
            session.status(),
            Arc::downgrade(&self.inner),
        ));
        let (first_ask, store, signals) =
            (session.clone(), self.inner.store.clone(), signals.clone());
        tokio::spawn(async move {
            ask_context(&first_ask, &store, &signals).await;
            ask_settings(&first_ask, &store, &signals).await;
        });
        Ok(session)
    }
}

/// Announces the session on each change of its status, agents or background
/// tasks. The status channel closes when the session actor ends, after its
/// final state is queued to the store.
fn announce_changes(session: &Session, signals: Signals) -> impl Future<Output = ()> + use<> {
    let id = session.id();
    let (mut status, mut agents, mut background) =
        (session.status(), session.agents(), session.background());
    async move {
        loop {
            let changed = tokio::select! {
                changed = status.changed() => changed,
                changed = agents.changed() => changed,
                changed = background.changed() => changed,
            };
            if changed.is_err() {
                break;
            }
            signals.session(id);
        }
        signals.session(id);
    }
}

/// Raises a notice when an ask is queued, when the discussion goes idle after
/// running, or when its child exits unasked. The last two also mark it unread.
async fn track_attention(id: Uuid, mut status: watch::Receiver<LiveStatus>, inner: Weak<Inner>) {
    let mut last = *status.borrow();
    loop {
        let ended = status.changed().await.is_err();
        let kind = if ended {
            // The final state is queued to the store before the channel closes.
            let Some(store) = store_of(&inner) else {
                return;
            };
            match store.session(id).await {
                Ok(Some(row)) => matches!(
                    row.state,
                    SessionState::Exited { .. } | SessionState::GateFailed { .. }
                )
                .then_some(NoticeKind::Stopped),
                Ok(None) => None,
                Err(err) => {
                    tracing::warn!("read session {id}: {err:#}");
                    None
                }
            }
        } else {
            let now = *status.borrow_and_update();
            let kind = if now.asks > last.asks {
                Some(NoticeKind::Ask)
            } else if last.activity == Activity::Busy && now.activity == Activity::Idle {
                Some(NoticeKind::Finished)
            } else {
                None
            };
            last = now;
            kind
        };
        if let Some(kind) = kind {
            raise(&inner, id, kind).await;
        }
        if ended {
            return;
        }
    }
}

/// Raises notice `kind` for discussion `id` unless an active client has it on screen.
async fn raise(inner: &Weak<Inner>, id: Uuid, kind: NoticeKind) {
    let Some(inner) = inner.upgrade().filter(|inner| !inner.watched(id)) else {
        return;
    };
    let (store, signals, notices) = (
        inner.store.clone(),
        inner.signals.clone(),
        inner.notices.clone(),
    );
    drop(inner);
    let unread = match kind {
        NoticeKind::Finished => Some(Unread::Finished),
        NoticeKind::Stopped => Some(Unread::Stopped),
        NoticeKind::Ask | NoticeKind::CacheSoonCold => None,
    };
    if let Some(unread) = unread {
        after_write(
            store.mark_unread(id, unread).await,
            format_args!("mark session {id} unread"),
            || signals.session(id),
        );
    }
    let _ = notices.send(Notice { session: id, kind });
}

/// The discussion's latest child was stopped from Home.
async fn killed(store: &Store, id: Uuid) -> bool {
    match store.latest_stop_reason(id).await {
        Ok(reason) => reason == Some(StopReason::Kill),
        Err(err) => {
            tracing::warn!("read the stop reason of session {id}: {err:#}");
            false
        }
    }
}

/// Every minute, raises `CacheSoonCold` once per request for each unarchived
/// discussion whose 1 hour cache goes cold within `CACHE_WARNING_BEFORE`,
/// unless it was killed from Home: the user let it go.
async fn warn_cold_caches(inner: Weak<Inner>) {
    let mut warned: HashMap<Uuid, DateTime<Utc>> = HashMap::new();
    let mut ticks = tokio::time::interval(CACHE_WATCH_INTERVAL);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let Some(store) = store_of(&inner) else {
            return;
        };
        let rows = match store.sessions().await {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!("list sessions for the cache warning: {err:#}");
                continue;
            }
        };
        let now = Utc::now();
        for row in rows {
            let Some(cache) = row
                .cache
                .filter(|cache| cache.ttl_secs == gauges::ONE_HOUR_TTL_SECS)
                .filter(|_| row.archived_at.is_none())
            else {
                continue;
            };
            let cold_at = cache.last_request_at + TimeDelta::seconds(cache.ttl_secs.into());
            let due = now >= cold_at - CACHE_WARNING_BEFORE && now < cold_at;
            if !due || warned.get(&row.id) == Some(&cache.last_request_at) {
                continue;
            }
            warned.insert(row.id, cache.last_request_at);
            if row.state != SessionState::Running && killed(&store, row.id).await {
                continue;
            }
            raise(&inner, row.id, NoticeKind::CacheSoonCold).await;
        }
    }
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
    signals: Signals,
) {
    let mut last_request: Option<String> = None;
    loop {
        let turn_ended = match events.recv().await {
            Ok(event) => match &event.body {
                EventBody::Host(HostEvent::ChildExited { .. }) => return,
                EventBody::Host(HostEvent::Catalog { models, commands }) => {
                    let Some(store) = store_of(&inner) else {
                        return;
                    };
                    after_write(
                        store.record_catalog(models, commands).await,
                        format_args!("store the shared catalog"),
                        || signals.catalog(),
                    );
                    false
                }
                EventBody::Line(line) => match &line.parsed {
                    Some(Inbound::System(System::Init(init))) => {
                        let Some(store) = store_of(&inner) else {
                            return;
                        };
                        after_write(
                            store
                                .record_terminal_commands(&init.terminal_slash_commands)
                                .await,
                            format_args!("store the shared terminal-only commands"),
                            || signals.catalog(),
                        );
                        false
                    }
                    Some(Inbound::Result(_)) => {
                        signals.usage();
                        signals.session(id);
                        true
                    }
                    Some(Inbound::RateLimitEvent(_)) => {
                        signals.usage();
                        false
                    }
                    Some(Inbound::Assistant(assistant)) => {
                        if let Some(request) = MainRequest::from_assistant(assistant)
                            && last_request.as_ref() != Some(&request.message_id)
                        {
                            last_request = Some(request.message_id);
                            signals.session(id);
                        }
                        false
                    }
                    _ => false,
                },
                EventBody::Host(_) => false,
            },
            Err(RecvError::Lagged(_)) => {
                signals.usage();
                signals.session(id);
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
            sample(&session, &store, &signals).await;
            ask_context(&session, &store, &signals).await;
            ask_settings(&session, &store, &signals).await;
            measure_restart(id, &store, &signals).await;
        }
    }
}

/// Stores the CLI's context usage and announces the session when it changed.
async fn ask_context(session: &Session, store: &Store, signals: &Signals) {
    let id = session.id();
    let reply = match session.context_usage().await {
        Ok(reply) => reply,
        Err(err) => {
            tracing::debug!("get_context_usage for session {id}: {err:#}");
            return;
        }
    };
    after_write(
        store.record_context_usage(id, reply).await,
        format_args!("store the context of session {id}"),
        || signals.session(id),
    );
}

/// Measures what a restart would keep, stores it and announces the session when it changed.
async fn measure_restart(id: Uuid, store: &Store, signals: &Signals) {
    let measured = async {
        let row = store.require_session(id).await?;
        let events = store.events_after(id, 0, u32::MAX).await?;
        let started = std::time::Instant::now();
        let measure = tokio::task::spawn_blocking(move || {
            let history = seed::History::from_events(&events);
            let seed = history.seed(&row.cwd, row.created_at.with_timezone(&Local).date_naive());
            RestartMeasure::new(&history.seed_window(), &seed)
        })
        .await?;
        tracing::debug!("restart measure of session {id} in {:?}", started.elapsed());
        anyhow::Ok(measure)
    };
    match measured.await {
        Ok(Some(measure)) => after_write(
            store.record_restart_measure(id, measure).await,
            format_args!("store the restart measure of session {id}"),
            || signals.session(id),
        ),
        Ok(None) => {}
        Err(err) => tracing::warn!("measure a restart of session {id}: {err:#}"),
    }
}

/// Stores the model and effort the CLI applies and announces the session when
/// they changed. A child launched without a pick, whose discussion still has
/// none, also reports the shared default settings.
async fn ask_settings(session: &Session, store: &Store, signals: &Signals) {
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
                after_write(
                    store.record_default_settings(&applied).await,
                    format_args!("store the shared default settings"),
                    || signals.catalog(),
                );
            }
            Ok(_) => {}
            Err(err) => tracing::warn!("read session {id}: {err:#}"),
        }
    }
    after_write(
        store.record_applied_settings(id, applied).await,
        format_args!("store the applied settings of session {id}"),
        || signals.session(id),
    );
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
        let (store, signals) = (inner.store.clone(), inner.signals.clone());
        drop(inner);
        if let Some(session) = session {
            sample(&session, &store, &signals).await;
        }
    }
}

async fn sample(session: &Session, store: &Store, signals: &Signals) {
    match session.usage_json().await {
        Ok(response) => {
            store.record_usage(Utc::now(), Some(session.id()), response.to_string());
            signals.usage();
        }
        Err(err) => tracing::debug!("get_usage for session {}: {err:#}", session.id()),
    }
}
