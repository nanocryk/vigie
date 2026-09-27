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

use crate::child::{Launch, Program};
use crate::gauges::{self, MainRequest};
use crate::protocol::Inbound;
use crate::session::{
    Activity, Config, Event, EventBody, HostEvent, LiveStatus, RestartMode, Session,
};
use crate::store::{SessionRow, Store, StoredEvent};
use crate::usage::{SessionContext, Snapshot};
use crate::{seed, wire};

/// Period of `get_usage` samples while at least one session is live.
pub const USAGE_INTERVAL: Duration = Duration::from_secs(5 * 60);
const CHANGES_CAPACITY: usize = 256;
/// Permission mode of a new session.
const DEFAULT_MODE: &str = "default";
/// Modes a stopped session accepts; any other would only fail at the next launch.
const STORED_MODES: [&str; 3] = [DEFAULT_MODE, "acceptEdits", "plan"];

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
    sampler: AbortHandle,
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
        let session = self.spawn(id, cwd, Launch::New(id), 0, Some(DEFAULT_MODE.into()))?;
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
        let session = self.spawn(id, row.cwd, row.launch, row.last_seq, row.permission_mode)?;
        live.insert(id, session.clone());
        let _ = self.inner.changes.send(id);
        Ok(session)
    }

    /// Fails with `wire::CACHE_COLD` while the discussion's cache is cold.
    pub async fn ensure_warm(&self, id: Uuid) -> Result<()> {
        let row = self
            .inner
            .store
            .session(id)
            .await?
            .with_context(|| format!("unknown session {id}"))?;
        if gauges::is_cold(row.cache.as_ref(), row.updated_at, Utc::now()) {
            bail!(wire::CACHE_COLD);
        }
        Ok(())
    }

    /// Stops the child the graceful way, as a shutdown does; the discussion stays
    /// listed and the next send resumes it. Does nothing more when not live.
    pub async fn kill(&self, id: Uuid) -> Result<()> {
        let mut live = self.inner.live.lock().await;
        let session = live.remove(&id);
        if session.is_none() && self.inner.store.session(id).await?.is_none() {
            bail!("unknown session {id}");
        }
        if let Some(session) = session {
            session.stop().await;
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
            RestartMode::Blank => None,
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
        futures_util::future::join_all(sessions.iter().map(Session::stop)).await;
        self.inner.store.flush().await
    }

    fn spawn(
        &self,
        id: Uuid,
        cwd: PathBuf,
        launch: Launch,
        last_seq: u64,
        permission_mode: Option<String>,
    ) -> Result<Session> {
        let (session, events) = Session::spawn(Config {
            id,
            cwd,
            launch,
            program: self.inner.program.clone(),
            history: Some(self.inner.store.clone()),
            last_seq,
            permission_mode,
        })?;
        tokio::spawn(sample_after_turns(
            id,
            session.status(),
            events,
            Arc::downgrade(&self.inner),
            self.inner.usage_changes.clone(),
            self.inner.changes.clone(),
        ));
        tokio::spawn(announce_changes(
            id,
            session.status(),
            self.inner.changes.clone(),
        ));
        let (first_ask, store, changes) = (
            session.clone(),
            self.inner.store.clone(),
            self.inner.changes.clone(),
        );
        tokio::spawn(async move { ask_context(&first_ask, &store, &changes).await });
        Ok(session)
    }
}

/// The status channel closes when the session actor ends, after its final
/// state is queued to the store.
async fn announce_changes(
    id: Uuid,
    mut status: watch::Receiver<LiveStatus>,
    changes: broadcast::Sender<Uuid>,
) {
    while status.changed().await.is_ok() {
        let _ = changes.send(id);
    }
    let _ = changes.send(id);
}

/// Samples usage and asks the context each time the session goes idle after a
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
) {
    let mut last_request: Option<String> = None;
    loop {
        let turn_ended = match events.recv().await {
            Ok(event) => match &event.body {
                EventBody::Host(HostEvent::ChildExited { .. }) => return,
                EventBody::Line(line) => match &line.parsed {
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
        }
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
