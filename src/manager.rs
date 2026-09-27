//! The live sessions and the history store behind them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::{Mutex, watch};
use tokio::task::AbortHandle;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

use crate::child::{Launch, Program};
use crate::protocol::Inbound;
use crate::session::{Activity, Config, Event, EventBody, HostEvent, Session};
use crate::store::{SessionRow, Store, StoredEvent};

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
            sampler: tokio::spawn(sample_periodically(inner.clone())).abort_handle(),
        });
        Ok(Self { inner })
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    /// Receives the id of a session each time it is spawned, changes activity, or
    /// ends. The store already reflects the change when the id arrives.
    pub fn subscribe_changes(&self) -> broadcast::Receiver<Uuid> {
        self.inner.changes.subscribe()
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

    /// Activity of every session whose child is running.
    pub async fn activities(&self) -> HashMap<Uuid, Activity> {
        self.inner
            .live
            .lock()
            .await
            .iter()
            .filter(|(_, session)| !session.is_closed())
            .map(|(id, session)| (*id, *session.activity().borrow()))
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
            Launch::Resume(id),
            row.last_seq,
            row.permission_mode,
        )?;
        live.insert(id, session.clone());
        let _ = self.inner.changes.send(id);
        Ok(session)
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
            session.activity(),
            events,
            Arc::downgrade(&self.inner),
        ));
        tokio::spawn(announce_changes(
            id,
            session.activity(),
            self.inner.changes.clone(),
        ));
        Ok(session)
    }
}

/// The activity channel closes when the session actor ends, after its final
/// state is queued to the store.
async fn announce_changes(
    id: Uuid,
    mut activity: watch::Receiver<Activity>,
    changes: broadcast::Sender<Uuid>,
) {
    while activity.changed().await.is_ok() {
        let _ = changes.send(id);
    }
    let _ = changes.send(id);
}

/// Samples usage each time the session goes idle after a turn, until it ends.
/// A lagged receiver may have missed that moment, so a lag counts as one. Holds
/// no `Session` handle, so it never keeps a session alive.
async fn sample_after_turns(
    id: Uuid,
    activity: watch::Receiver<Activity>,
    mut events: broadcast::Receiver<Arc<Event>>,
    inner: Weak<Inner>,
) {
    loop {
        let turn_ended = match events.recv().await {
            Ok(event) => match &event.body {
                EventBody::Host(HostEvent::ChildExited { .. }) => return,
                EventBody::Line(line) => matches!(line.parsed, Some(Inbound::Result(_))),
                EventBody::Host(_) => false,
            },
            Err(RecvError::Lagged(_)) => true,
            Err(RecvError::Closed) => return,
        };
        if !turn_ended || *activity.borrow() != Activity::Idle {
            continue;
        }
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let session = inner.live.lock().await.get(&id).cloned();
        let store = inner.store.clone();
        drop(inner);
        if let Some(session) = session {
            sample(&session, &store).await;
        }
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
        drop(inner);
        if let Some(session) = session {
            sample(&session, &store).await;
        }
    }
}

async fn sample(session: &Session, store: &Store) {
    match session.usage_json().await {
        Ok(response) => store.record_usage(Utc::now(), Some(session.id()), response.to_string()),
        Err(err) => tracing::debug!("get_usage for session {}: {err:#}", session.id()),
    }
}
