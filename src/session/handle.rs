//! `Session`, the handle that sends commands to a session actor and reads what it publishes.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, anyhow};
use chrono::Utc;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use uuid::Uuid;

use super::activity::LiveStatus;
use super::actor::{Actor, Command, Views};
use super::agents::{AgentState, AgentView};
use super::background::BackgroundView;
use super::events::{Answer, Event, StopReason};
use crate::claude::child::{Child, Effort, Launch, ModelSettings, Program};
use crate::claude::protocol::{
    AppliedSettings, ContextUsageResponse, GetUsageResponse, HostRequest, InterruptResponse,
    PermissionMode, SetPermissionModeResponse, SettingsResponse,
};
use crate::store::Store;

const EVENT_CAPACITY: usize = 4096;
const COMMAND_CAPACITY: usize = 64;

#[derive(Debug, Clone)]
pub struct Config {
    /// vigie2's id for the session, exported to the child as `VIGIE2_SESSION_ID`.
    pub id: Uuid,
    pub cwd: PathBuf,
    pub launch: Launch,
    pub program: Program,
    /// Receives every event before it is broadcast, and the session's state.
    pub history: Option<Store>,
    /// Seq of the session's latest event; the next one gets `last_seq + 1`.
    pub last_seq: u64,
    /// `--permission-mode` for the child; `None` lets the CLI pick its default.
    pub permission_mode: Option<PermissionMode>,
    /// `--model` and `--effort` for the child.
    pub settings: ModelSettings,
}

/// Handle to a session actor. The actor stops the child when every handle is dropped.
#[derive(Clone)]
pub struct Session {
    id: Uuid,
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<Arc<Event>>,
    status: watch::Receiver<LiveStatus>,
    agents: watch::Receiver<Vec<AgentView>>,
    background: watch::Receiver<Vec<BackgroundView>>,
    launch_settings: ModelSettings,
    /// Set once a model or effort change was sent to the child.
    retuned: Arc<AtomicBool>,
}

impl Session {
    /// Spawns the child and its actor. The returned receiver sees every event from
    /// `Started` on; later subscribers only see events sent after they subscribe.
    pub fn spawn(config: Config) -> Result<(Self, broadcast::Receiver<Arc<Event>>)> {
        let child = Child::spawn(
            &config.program,
            &config.cwd,
            config.launch,
            config.permission_mode.as_ref(),
            &config.settings,
            config.id,
        )?;
        if let Some(history) = &config.history {
            history.session_started(
                config.id,
                &config.cwd,
                Utc::now(),
                config.permission_mode.clone(),
            );
        }
        let id = config.id;
        let launch_settings = config.settings.clone();
        let (events, first_receiver) = broadcast::channel(EVENT_CAPACITY);
        let (status_sender, status) = watch::channel(LiveStatus::default());
        let (agents_sender, agents) = watch::channel(Vec::new());
        let (background_sender, background) = watch::channel(Vec::new());
        let (commands, command_receiver) = mpsc::channel(COMMAND_CAPACITY);
        let views = Views {
            status: status_sender,
            agents: agents_sender,
            background: background_sender,
        };
        let actor = Actor::new(config, child, events.clone(), views);
        tokio::spawn(actor.run(command_receiver));
        let session = Self {
            id,
            commands,
            events,
            status,
            agents,
            background,
            launch_settings,
            retuned: Arc::new(AtomicBool::new(false)),
        };
        Ok((session, first_receiver))
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    /// True while the child runs on the CLI's own model and effort: launched
    /// without `--model` or `--effort`, and neither changed since.
    pub fn runs_without_pick(&self) -> bool {
        self.launch_settings == ModelSettings::default() && !self.retuned.load(Ordering::Relaxed)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.events.subscribe()
    }

    pub fn status(&self) -> watch::Receiver<LiveStatus> {
        self.status.clone()
    }

    /// The subagents the child started, oldest first; republished on every change.
    pub fn agents(&self) -> watch::Receiver<Vec<AgentView>> {
        self.agents.clone()
    }

    /// The running background tasks, oldest first; republished on every change.
    pub fn background(&self) -> watch::Receiver<Vec<BackgroundView>> {
        self.background.clone()
    }

    /// Writes `agents::wrap` of `text` to the main agent for agent `task_id`,
    /// after a `RelaySent` event; fails for an agent the child did not start.
    pub async fn relay(
        &self,
        task_id: impl Into<String>,
        text: impl Into<String>,
        client_id: Option<String>,
    ) -> Result<()> {
        let (task_id, text) = (task_id.into(), text.into());
        self.call(|reply| Command::Relay {
            task_id,
            text,
            client_id,
            reply,
        })
        .await
    }

    pub async fn stop_task(&self, task_id: impl Into<String>) -> Result<()> {
        self.control::<Value>(HostRequest::stop_task(task_id))
            .await?;
        Ok(())
    }

    /// Stops every running agent; the first failure is returned after trying them all.
    pub async fn stop_agents(&self) -> Result<()> {
        let running: Vec<String> = self
            .agents
            .borrow()
            .iter()
            .filter(|agent| agent.state == AgentState::Running)
            .map(|agent| agent.task_id.clone())
            .collect();
        let mut first_error = None;
        for task_id in running {
            if let Err(err) = self.stop_task(&task_id).await {
                tracing::warn!("stop agent {task_id}: {err:#}");
                first_error.get_or_insert(err);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Resolves once the message is written to the child. `client_id` is echoed in
    /// the `UserSent` event.
    pub async fn send(&self, text: impl Into<String>, client_id: Option<String>) -> Result<()> {
        let text = text.into();
        self.call(|reply| Command::Send {
            text,
            seed: None,
            client_id,
            reply,
        })
        .await
    }

    /// Writes `seed::first_message(seed, text)`; the `UserSent` event carries
    /// `text` alone, and none is emitted when `text` is blank.
    pub async fn send_seeded(
        &self,
        seed: String,
        text: impl Into<String>,
        client_id: Option<String>,
    ) -> Result<()> {
        let text = text.into();
        self.call(|reply| Command::Send {
            text,
            seed: Some(seed),
            client_id,
            reply,
        })
        .await
    }

    pub async fn interrupt(&self) -> Result<InterruptResponse> {
        self.control(HostRequest::interrupt(None)).await
    }

    pub async fn set_permission_mode(
        &self,
        mode: impl Into<PermissionMode>,
    ) -> Result<SetPermissionModeResponse> {
        self.control(HostRequest::set_permission_mode(mode.into()))
            .await
    }

    /// `None` goes back to the CLI's default model.
    pub async fn set_model(&self, model: Option<String>) -> Result<()> {
        self.retuned.store(true, Ordering::Relaxed);
        self.control::<Value>(HostRequest::set_model(model)).await?;
        Ok(())
    }

    pub async fn set_effort(&self, effort: Effort) -> Result<()> {
        self.retuned.store(true, Ordering::Relaxed);
        let settings = serde_json::json!({ "effortLevel": effort.as_str() });
        self.control::<Value>(HostRequest::apply_flag_settings(settings))
            .await?;
        Ok(())
    }

    pub async fn usage(&self) -> Result<GetUsageResponse> {
        Ok(serde_json::from_value(self.usage_json().await?)?)
    }

    /// The `get_usage` payload as the CLI sent it.
    pub async fn usage_json(&self) -> Result<Value> {
        self.control(HostRequest::get_usage(Some(true))).await
    }

    /// Its reply line is not emitted as an event: it lists the user's configuration.
    pub async fn context_usage(&self) -> Result<ContextUsageResponse> {
        self.control(HostRequest::get_context_usage()).await
    }

    /// The model and effort the CLI applies. Its reply line is not emitted as an
    /// event: it lists the user's settings.
    pub async fn applied_settings(&self) -> Result<AppliedSettings> {
        let reply: SettingsResponse = self.control(HostRequest::get_settings()).await?;
        Ok(reply.applied)
    }

    pub async fn answer(&self, request_id: impl Into<String>, answer: Answer) -> Result<()> {
        let request_id = request_id.into();
        self.call(|reply| Command::Answer {
            request_id,
            answer,
            reply,
        })
        .await
    }

    /// Closes the child's stdin and waits for the session to end.
    pub async fn stop(&self) {
        self.stop_with(None).await;
    }

    /// `stop` for `reason`, which `Stopping` and `ChildExited` carry.
    pub async fn stop_for(&self, reason: StopReason) {
        self.stop_with(Some(reason)).await;
    }

    async fn stop_with(&self, reason: Option<StopReason>) {
        let _ = self.commands.send(Command::Stop(reason)).await;
        self.closed().await;
    }

    /// Resolves after the `ChildExited` event.
    pub async fn closed(&self) {
        self.commands.closed().await;
    }

    /// True once the `ChildExited` event is out.
    pub fn is_closed(&self) -> bool {
        self.commands.is_closed()
    }

    async fn control<T: DeserializeOwned>(&self, request: HostRequest) -> Result<T> {
        let response = self
            .call(|reply| Command::Control { request, reply })
            .await?;
        response.payload()
    }

    async fn call<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T>>) -> Command,
    ) -> Result<T> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(command(reply))
            .await
            .map_err(|_| session_ended())?;
        response.await.map_err(|_| session_ended())?
    }
}

fn session_ended() -> anyhow::Error {
    anyhow!("session ended")
}
