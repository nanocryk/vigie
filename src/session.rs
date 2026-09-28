//! One long-lived `claude` child per session, driven by an actor task.
//!
//! Commands go in through `Session`. Every stdout line and a few host events come
//! out on a broadcast channel, numbered by a per-session sequence.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;
use uuid::Uuid;

use crate::child::{Child, Launch, ModelSettings, Program};
use crate::danger::{self, Danger};
use crate::grant::{self, FileOffer, FolderOffer};
use crate::protocol::{
    AppliedSettings, CanUseTool, CliRequest, ContextUsageResponse, ControlRequest, ControlResponse,
    Extra, GetUsageResponse, HostRequest, Inbound, InitCommand, InitModel, InitializeResponse,
    InterruptResponse, Outbound, PermissionResult, Priority, SetPermissionModeResponse,
    SettingsResponse, System, UserMessage,
};
use crate::store::{SessionState, Store};

const EVENT_CAPACITY: usize = 4096;
const COMMAND_CAPACITY: usize = 64;
const STDERR_TAIL: usize = 50;
/// Time a child gets to exit after its stdin closes before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(10);
const STDERR_DRAIN: Duration = Duration::from_secs(1);
const FIRST_PARTY: &str = "firstParty";
/// `apiKeySource` of a claude.ai OAuth session.
const OAUTH_KEY_SOURCE: &str = "none";
const ASK_USER_QUESTION: &str = "AskUserQuestion";
/// `rateLimitType` of a limit that draws on usage credits.
const OVERAGE_LIMIT: &str = "overage";

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
    pub permission_mode: Option<String>,
    /// `--model` and `--effort` for the child.
    pub settings: ModelSettings,
}

#[derive(Debug, Clone)]
pub struct Event {
    /// Starts at 1 and grows by 1 per event of the session.
    pub seq: u64,
    pub time: DateTime<Utc>,
    pub body: EventBody,
}

#[derive(Debug, Clone)]
pub enum EventBody {
    Line(Box<Line>),
    Host(HostEvent),
}

/// One line of CLI stdout.
#[derive(Debug, Clone)]
pub struct Line {
    pub raw: String,
    /// `None` when the line is not JSON.
    pub parsed: Option<Inbound>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HostEvent {
    Started {
        launch: Launch,
        cwd: PathBuf,
    },
    /// A user message about to be written to the child; `client_id` names the command that sent it.
    UserSent {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        /// Sent while a turn ran, so it starts no exchange of its own.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        mid_turn: bool,
    },
    AskQueued {
        request_id: String,
        tool_name: String,
        tool_use_id: String,
        #[serde(default)]
        danger: Danger,
        /// An edit ask the CLI pairs with a switch to Accept edits.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        accept_edits: bool,
        /// The folder the CLI suggests granting for the session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder: Option<FolderOffer>,
        /// The file of a Read or edit-tool ask, which the allow can grant.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<FileOffer>,
    },
    /// An ask on a granted file, allowed by the host without queueing it.
    AutoAllowed {
        request_id: String,
        tool_name: String,
        tool_use_id: String,
        file: FileOffer,
    },
    AskResolved {
        request_id: String,
        resolution: Resolution,
    },
    /// The CLI's permission mode changed, as reported by `initialize`, `system/init`,
    /// `system/status` or a successful `set_permission_mode`.
    PermissionMode {
        mode: String,
    },
    /// The child was killed; the session ends with the following `ChildExited`.
    AuthGateFailed {
        reason: GateFailure,
    },
    /// A requested stop began: the CLI's interrupt of a running tool that may
    /// follow comes from it, not from the user.
    Stopping {
        reason: StopReason,
    },
    ChildExited {
        code: Option<i32>,
        stderr_tail: Vec<String>,
        /// Why the child was stopped; absent for any other exit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<StopReason>,
    },
    /// The discussion moved to a fresh Claude session; a continue carries the seed
    /// its first message starts with.
    Restarted {
        mode: RestartMode,
        exchanges: usize,
        total_exchanges: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seed: Option<String>,
    },
    /// The models and the commands the CLI offers, from its `initialize` reply.
    Catalog {
        models: Vec<ModelChoice>,
        commands: Vec<CommandInfo>,
    },
}

/// A model the CLI accepts, with the effort levels it supports (none when it has no effort).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    pub value: String,
    /// The model id `value` stands for, such as `claude-opus-5-5` for `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub effort_levels: Vec<String>,
}

impl ModelChoice {
    pub fn new(model: &InitModel) -> Self {
        let effort_levels = match (&model.supports_effort, &model.supported_effort_levels) {
            (Some(true), Some(levels)) => levels.clone(),
            _ => Vec::new(),
        };
        Self {
            value: model.value.clone(),
            resolved_model: model.resolved_model.clone(),
            display_name: model
                .display_name
                .clone()
                .unwrap_or_else(|| model.value.clone()),
            description: model.description.clone(),
            effort_levels,
        }
    }
}

/// A slash command or a skill; `argument_hint` is empty for one without arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub argument_hint: String,
}

impl CommandInfo {
    pub fn new(command: &InitCommand) -> Self {
        Self {
            name: command.name.clone(),
            description: command.description.clone().unwrap_or_default(),
            argument_hint: command.argument_hint.clone().unwrap_or_default(),
        }
    }
}

/// How a discussion moves to a fresh Claude session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartMode {
    /// Seeded with the last exchanges of the discussion.
    Continue,
    Blank,
    /// A blank restart asked for as the Clear command.
    Clear,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GateFailure {
    /// The `initialize` response did not report an Anthropic subscription account.
    ApiProvider { provider: Option<String> },
    /// The `initialize` request failed, so the account could not be checked.
    Initialize { error: String },
    /// A `system/init` reported API key billing.
    ApiKeySource { source: String },
    /// A `rate_limit_event` reported usage credits in use.
    Overage,
}

/// Answer to a pending ask.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Answer {
    /// `updated_input` defaults to the input the CLI sent. A non-blank `note`
    /// follows the answer as a user message queued for the running turn.
    Allow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// Also switches to Accept edits, through the CLI's suggestion.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        accept_edits: bool,
        /// Also grants this folder for the session: the suggested one or a parent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder: Option<String>,
        /// Also allows the later asks on the ask's file, until the child stops.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        always_for_file: bool,
    },
    /// The message is shown to the model.
    Deny { message: String },
    /// `AskUserQuestion` answers, keyed by question text.
    Questions { answers: BTreeMap<String, String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolution {
    Answered {
        answer: Answer,
    },
    /// The CLI withdrew the ask with `control_cancel_request`.
    Cancelled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    #[default]
    Idle,
    Busy,
}

/// What the session is doing, republished on every change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveStatus {
    pub activity: Activity,
    /// A turn runs; background tasks alone do not count.
    pub turn: bool,
    /// Asks waiting for an answer.
    pub asks: usize,
}

/// Derives `Activity` from the lines the CLI sends and the messages the host sends.
///
/// A background task counts as running until its `task_notification`, which the
/// CLI follows with a turn of its own, so the session stays busy across that gap.
#[derive(Debug, Default)]
pub struct ActivityTracker {
    turn: bool,
    tasks: BTreeSet<String>,
}

impl ActivityTracker {
    pub fn on_send(&mut self) {
        self.turn = true;
    }

    pub fn on_line(&mut self, line: &Inbound) {
        match line {
            Inbound::System(System::Init(_)) => self.turn = true,
            Inbound::System(System::Status(status)) if status.status.is_some() => self.turn = true,
            Inbound::System(System::BackgroundTasksChanged(changed)) => self
                .tasks
                .extend(changed.tasks.iter().map(|task| task.task_id.clone())),
            Inbound::System(System::TaskStarted(started))
                if started.is_backgrounded == Some(true) =>
            {
                self.tasks.insert(started.task_id.clone());
            }
            Inbound::System(System::TaskNotification(notification)) => {
                if self.tasks.remove(&notification.task_id) {
                    self.turn = true;
                }
            }
            Inbound::Result(_) => self.turn = false,
            _ => {}
        }
    }

    pub fn turn(&self) -> bool {
        self.turn
    }

    pub fn activity(&self) -> Activity {
        if self.turn || !self.tasks.is_empty() {
            Activity::Busy
        } else {
            Activity::Idle
        }
    }
}

/// Why a live child was stopped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Stopped from Home.
    #[default]
    Kill,
    /// Stopped to drop the folders granted from asks.
    Revoke,
    /// The server is shutting down.
    Shutdown,
}

enum Command {
    Send {
        text: String,
        seed: Option<String>,
        client_id: Option<String>,
        reply: oneshot::Sender<Result<()>>,
    },
    Control {
        request: HostRequest,
        reply: oneshot::Sender<Result<ControlResponse>>,
    },
    Answer {
        request_id: String,
        answer: Answer,
        reply: oneshot::Sender<Result<()>>,
    },
    Stop(Option<StopReason>),
}

/// Handle to a session actor. The actor stops the child when every handle is dropped.
#[derive(Clone)]
pub struct Session {
    id: Uuid,
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<Arc<Event>>,
    status: watch::Receiver<LiveStatus>,
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
            config.permission_mode.as_deref(),
            &config.settings,
            config.id,
        )?;
        if let Some(history) = &config.history {
            history.session_started(config.id, &config.cwd, Utc::now(), config.permission_mode);
        }
        let launch_settings = config.settings.clone();
        let (events, first_receiver) = broadcast::channel(EVENT_CAPACITY);
        let (status_sender, status) = watch::channel(LiveStatus::default());
        let (commands, command_receiver) = mpsc::channel(COMMAND_CAPACITY);
        let actor = Actor {
            id: config.id,
            cwd: config.cwd,
            launch: config.launch,
            child,
            events: events.clone(),
            history: config.history,
            end: None,
            stop_reason: None,
            seq: config.last_seq,
            status: status_sender,
            tracker: ActivityTracker::default(),
            asks: Vec::new(),
            file_grants: Vec::new(),
            requests: HashMap::new(),
            mode_requests: HashSet::new(),
            unrecorded_requests: HashSet::new(),
            permission_mode: None,
            next_request: 0,
            state: State::Ready,
            kill_at: None,
            stderr_tail: VecDeque::new(),
        };
        tokio::spawn(actor.run(command_receiver));
        let session = Self {
            id: config.id,
            commands,
            events,
            status,
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
        self.control(HostRequest::Interrupt {
            cancel_queued: None,
            extra: Extra::new(),
        })
        .await
    }

    pub async fn set_permission_mode(
        &self,
        mode: impl Into<String>,
    ) -> Result<SetPermissionModeResponse> {
        self.control(HostRequest::SetPermissionMode {
            mode: mode.into(),
            extra: Extra::new(),
        })
        .await
    }

    /// `None` goes back to the CLI's default model.
    pub async fn set_model(&self, model: Option<String>) -> Result<()> {
        self.retuned.store(true, Ordering::Relaxed);
        self.control::<Value>(HostRequest::SetModel {
            model,
            extra: Extra::new(),
        })
        .await?;
        Ok(())
    }

    pub async fn set_effort(&self, effort: String) -> Result<()> {
        self.retuned.store(true, Ordering::Relaxed);
        self.control::<Value>(HostRequest::ApplyFlagSettings {
            settings: serde_json::json!({ "effortLevel": effort }),
            extra: Extra::new(),
        })
        .await?;
        Ok(())
    }

    pub async fn usage(&self) -> Result<GetUsageResponse> {
        Ok(serde_json::from_value(self.usage_json().await?)?)
    }

    /// The `get_usage` payload as the CLI sent it.
    pub async fn usage_json(&self) -> Result<Value> {
        self.control(HostRequest::GetUsage {
            skip_behaviors: Some(true),
            extra: Extra::new(),
        })
        .await
    }

    /// Its reply line is not emitted as an event: it lists the user's configuration.
    pub async fn context_usage(&self) -> Result<ContextUsageResponse> {
        self.control(HostRequest::GetContextUsage {
            extra: Extra::new(),
        })
        .await
    }

    /// The model and effort the CLI applies. Its reply line is not emitted as an
    /// event: it lists the user's settings.
    pub async fn applied_settings(&self) -> Result<AppliedSettings> {
        let reply: SettingsResponse = self
            .control(HostRequest::GetSettings {
                extra: Extra::new(),
            })
            .await?;
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

#[derive(Debug, PartialEq, Eq)]
enum State {
    /// Waiting for the `initialize` response; commands wait in the queue.
    Initializing {
        request_id: String,
    },
    Ready,
    /// Stdin closed or child killed; draining stdout until it closes.
    Stopping,
}

struct PendingAsk {
    request_id: String,
    request: CanUseTool,
}

struct Actor {
    id: Uuid,
    cwd: PathBuf,
    launch: Launch,
    child: Child,
    events: broadcast::Sender<Arc<Event>>,
    history: Option<Store>,
    /// How the session ends, once decided; an unrequested exit leaves it `None`.
    end: Option<SessionState>,
    /// Set when the stop was requested for a reason.
    stop_reason: Option<StopReason>,
    seq: u64,
    status: watch::Sender<LiveStatus>,
    tracker: ActivityTracker,
    /// In arrival order.
    asks: Vec<PendingAsk>,
    /// Files allowed with `always_for_file`; their later asks are answered here.
    file_grants: Vec<FileOffer>,
    /// Host control requests awaiting their `control_response`.
    requests: HashMap<String, oneshot::Sender<Result<ControlResponse>>>,
    /// Ids among `requests` that are `set_permission_mode` requests.
    mode_requests: HashSet<String>,
    /// Ids among `requests` whose reply line is kept out of the events.
    unrecorded_requests: HashSet<String>,
    /// Last mode announced with `HostEvent::PermissionMode`.
    permission_mode: Option<String>,
    next_request: u64,
    state: State,
    kill_at: Option<Instant>,
    stderr_tail: VecDeque<String>,
}

impl Actor {
    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        self.emit_host(HostEvent::Started {
            launch: self.launch,
            cwd: self.cwd.clone(),
        });
        let request_id = self.next_request_id();
        let initialize = ControlRequest::new(
            request_id.clone(),
            HostRequest::Initialize {
                extra: Extra::new(),
            },
        );
        self.state = State::Initializing { request_id };
        if let Err(err) = self.write(Outbound::ControlRequest(initialize)).await {
            tracing::warn!("send initialize: {err:#}");
        }
        let mut stderr_open = true;
        loop {
            let ready = self.state == State::Ready;
            let kill_at = self.kill_at;
            tokio::select! {
                line = self.child.stdout.recv() => match line {
                    Some(line) => self.on_line(line).await,
                    None => break,
                },
                line = self.child.stderr.recv(), if stderr_open => match line {
                    Some(line) => self.on_stderr(line),
                    None => stderr_open = false,
                },
                command = commands.recv(), if ready => match command {
                    Some(command) => self.on_command(command).await,
                    None => self.begin_stop(None),
                },
                _ = tokio::time::sleep_until(kill_at.unwrap_or_else(Instant::now)), if kill_at.is_some() => {
                    self.child.kill();
                    self.kill_at = None;
                }
            }
        }
        self.finish().await;
        drop(commands);
    }

    async fn on_line(&mut self, raw: String) {
        let parsed = serde_json::from_str::<Inbound>(&raw).ok();
        if let Some(Inbound::ControlResponse(response)) = &parsed
            && response
                .request_id()
                .is_some_and(|id| self.unrecorded_requests.remove(id))
        {
            self.on_control_response(response);
            return;
        }
        if let Some(line) = &parsed {
            self.tracker.on_line(line);
            self.publish_status();
        }
        let event = self.emit(EventBody::Line(Box::new(Line { raw, parsed })));
        if let EventBody::Line(line) = &event.body
            && let Some(parsed) = &line.parsed
        {
            self.handle_line(parsed).await;
        }
    }

    async fn handle_line(&mut self, line: &Inbound) {
        match line {
            Inbound::System(System::Init(init)) => {
                if init.api_key_source != OAUTH_KEY_SOURCE {
                    self.fail_gate(GateFailure::ApiKeySource {
                        source: init.api_key_source.clone(),
                    });
                } else {
                    self.set_mode(&init.permission_mode);
                }
            }
            Inbound::System(System::Status(status)) => {
                if let Some(mode) = &status.permission_mode {
                    self.set_mode(mode);
                }
            }
            Inbound::RateLimitEvent(event)
                if event.rate_limit_info.is_using_overage == Some(true)
                    || event.rate_limit_info.rate_limit_type.as_deref() == Some(OVERAGE_LIMIT) =>
            {
                self.fail_gate(GateFailure::Overage);
            }
            Inbound::ControlRequest(request) => self.on_cli_request(request).await,
            Inbound::ControlResponse(response) => self.on_control_response(response),
            Inbound::Unknown(value) if value["type"] == "control_cancel_request" => {
                if let Some(request_id) = value["request_id"].as_str() {
                    self.on_cancel(request_id);
                }
            }
            _ => {}
        }
    }

    async fn on_cli_request(&mut self, request: &ControlRequest<CliRequest>) {
        match &request.request {
            CliRequest::CanUseTool(ask) => {
                let file = FileOffer::of(ask, &self.cwd);
                if let Some(granted) = file
                    .as_ref()
                    .filter(|file| self.file_grants.iter().any(|grant| grant.same(file)))
                {
                    self.auto_allow(&request.request_id, ask, granted.clone())
                        .await;
                    return;
                }
                self.asks.push(PendingAsk {
                    request_id: request.request_id.clone(),
                    request: (**ask).clone(),
                });
                self.emit_host(HostEvent::AskQueued {
                    request_id: request.request_id.clone(),
                    tool_name: ask.tool_name.clone(),
                    tool_use_id: ask.tool_use_id.clone(),
                    danger: danger::classify(ask, &self.cwd),
                    accept_edits: grant::offers_accept_edits(ask),
                    folder: grant::folder_offer(ask, dirs::home_dir().as_deref()),
                    file,
                });
                self.publish_status();
            }
            CliRequest::Other(fields) => {
                let subtype = fields
                    .get("subtype")
                    .and_then(Value::as_str)
                    .unwrap_or("(none)");
                let response = ControlResponse::error(
                    request.request_id.clone(),
                    format!("vigie2 does not handle control request {subtype}"),
                );
                if let Err(err) = self.write(Outbound::ControlResponse(response)).await {
                    tracing::warn!("answer control request {subtype}: {err:#}");
                }
            }
        }
    }

    fn on_control_response(&mut self, response: &ControlResponse) {
        let Some(request_id) = response.request_id() else {
            return;
        };
        if matches!(&self.state, State::Initializing { request_id: id } if id == request_id) {
            self.check_initialize(response);
        } else if let Some(reply) = self.requests.remove(request_id) {
            if self.mode_requests.remove(request_id)
                && let Ok(changed) = response.payload::<SetPermissionModeResponse>()
            {
                self.set_mode(&changed.mode);
            }
            let _ = reply.send(Ok(response.clone()));
        }
    }

    /// Announces `mode` when it differs from the last one announced.
    fn set_mode(&mut self, mode: &str) {
        if self.permission_mode.as_deref() == Some(mode) {
            return;
        }
        self.permission_mode = Some(mode.to_string());
        self.emit_host(HostEvent::PermissionMode {
            mode: mode.to_string(),
        });
    }

    fn check_initialize(&mut self, response: &ControlResponse) {
        match response.payload::<InitializeResponse>() {
            Ok(initialize) => {
                let provider = initialize.account.and_then(|account| account.api_provider);
                if provider.as_deref() == Some(FIRST_PARTY) {
                    self.state = State::Ready;
                    if let Some(mode) = &initialize.current_permission_mode {
                        self.set_mode(mode);
                    }
                    if initialize.models.is_some() || initialize.commands.is_some() {
                        self.emit_host(HostEvent::Catalog {
                            models: initialize
                                .models
                                .iter()
                                .flatten()
                                .map(ModelChoice::new)
                                .collect(),
                            commands: initialize
                                .commands
                                .iter()
                                .flatten()
                                .map(CommandInfo::new)
                                .collect(),
                        });
                    }
                } else {
                    self.fail_gate(GateFailure::ApiProvider { provider });
                }
            }
            Err(err) => self.fail_gate(GateFailure::Initialize {
                error: format!("{err:#}"),
            }),
        }
    }

    fn on_cancel(&mut self, request_id: &str) {
        let Some(index) = self.ask_index(request_id) else {
            return;
        };
        let ask = self.asks.remove(index);
        self.emit_host(HostEvent::AskResolved {
            request_id: ask.request_id,
            resolution: Resolution::Cancelled,
        });
        self.publish_status();
    }

    fn on_stderr(&mut self, line: String) {
        tracing::debug!("claude stderr: {line}");
        if self.stderr_tail.len() == STDERR_TAIL {
            self.stderr_tail.pop_front();
        }
        self.stderr_tail.push_back(line);
    }

    async fn on_command(&mut self, command: Command) {
        match command {
            Command::Send {
                text,
                seed,
                client_id,
                reply,
            } => {
                let _ = reply.send(self.send_user(text, seed, client_id, None).await);
            }
            Command::Control { request, reply } => {
                let request_id = self.next_request_id();
                let sets_mode = matches!(request, HostRequest::SetPermissionMode { .. });
                let unrecorded = matches!(
                    request,
                    HostRequest::GetContextUsage { .. } | HostRequest::GetSettings { .. }
                );
                let line =
                    Outbound::ControlRequest(ControlRequest::new(request_id.clone(), request));
                match self.write(line).await {
                    Ok(()) => {
                        if sets_mode {
                            self.mode_requests.insert(request_id.clone());
                        }
                        if unrecorded {
                            self.unrecorded_requests.insert(request_id.clone());
                        }
                        self.requests.insert(request_id, reply);
                    }
                    Err(err) => {
                        let _ = reply.send(Err(err));
                    }
                }
            }
            Command::Answer {
                request_id,
                answer,
                reply,
            } => {
                let _ = reply.send(self.answer(request_id, answer).await);
            }
            Command::Stop(reason) => self.begin_stop(reason),
        }
    }

    async fn send_user(
        &mut self,
        text: String,
        seed: Option<String>,
        client_id: Option<String>,
        priority: Option<Priority>,
    ) -> Result<()> {
        // Emitted before the write, so it precedes the CLI's echo in seq order.
        if seed.is_none() || !text.trim().is_empty() {
            self.emit_host(HostEvent::UserSent {
                text: text.clone(),
                client_id,
                mid_turn: self.tracker.turn(),
            });
        }
        let written = match &seed {
            Some(seed) => crate::seed::first_message(seed, &text),
            None => text,
        };
        let session_id = self.launch.claude_session_id().to_string();
        let mut message = UserMessage::text(session_id, written);
        if let Some(priority) = priority {
            message = message.with_priority(priority);
        }
        self.write(Outbound::User(message)).await?;
        self.tracker.on_send();
        self.publish_status();
        Ok(())
    }

    async fn answer(&mut self, request_id: String, answer: Answer) -> Result<()> {
        let index = self
            .ask_index(&request_id)
            .ok_or_else(|| anyhow!("no pending ask {request_id}"))?;
        let result = permission_result(&self.asks[index].request, &answer, &self.cwd)?;
        let response = ControlResponse::permission(request_id.clone(), &result);
        self.write(Outbound::ControlResponse(response)).await?;
        let ask = self.asks.remove(index);
        if let Answer::Allow {
            always_for_file: true,
            ..
        } = &answer
            && let Some(file) = FileOffer::of(&ask.request, &self.cwd)
            && !self.file_grants.iter().any(|grant| grant.same(&file))
        {
            self.file_grants.push(file);
        }
        let note = match &answer {
            Answer::Allow {
                note: Some(note), ..
            } if !note.trim().is_empty() => Some(allow_note(&ask.request.tool_name, note)),
            _ => None,
        };
        self.emit_host(HostEvent::AskResolved {
            request_id,
            resolution: Resolution::Answered { answer },
        });
        self.publish_status();
        if let Some(note) = note {
            self.send_user(note, None, None, Some(Priority::Next))
                .await?;
        }
        Ok(())
    }

    async fn auto_allow(&mut self, request_id: &str, ask: &CanUseTool, file: FileOffer) {
        let result = PermissionResult::allow(ask.input.clone());
        let response = ControlResponse::permission(request_id.to_string(), &result);
        if let Err(err) = self.write(Outbound::ControlResponse(response)).await {
            tracing::warn!("auto-allow {}: {err:#}", ask.tool_name);
            return;
        }
        self.emit_host(HostEvent::AutoAllowed {
            request_id: request_id.to_string(),
            tool_name: ask.tool_name.clone(),
            tool_use_id: ask.tool_use_id.clone(),
            file,
        });
    }

    fn ask_index(&self, request_id: &str) -> Option<usize> {
        self.asks
            .iter()
            .position(|ask| ask.request_id == request_id)
    }

    fn begin_stop(&mut self, reason: Option<StopReason>) {
        if self.state == State::Stopping {
            return;
        }
        self.state = State::Stopping;
        self.stop_reason = reason;
        if let Some(reason) = reason {
            self.emit_host(HostEvent::Stopping { reason });
        }
        self.end = Some(SessionState::Stopped);
        self.child.close_stdin();
        self.kill_at = Some(Instant::now() + STOP_GRACE);
    }

    fn fail_gate(&mut self, reason: GateFailure) {
        if self.state == State::Stopping {
            return;
        }
        tracing::warn!("auth gate failed: {reason:?}");
        self.state = State::Stopping;
        self.end = Some(SessionState::GateFailed {
            reason: reason.clone(),
        });
        self.child.kill();
        self.kill_at = None;
        self.emit_host(HostEvent::AuthGateFailed { reason });
    }

    async fn finish(&mut self) {
        let status = match tokio::time::timeout(STOP_GRACE, self.child.wait()).await {
            Ok(status) => status,
            Err(_) => {
                self.child.kill();
                self.child.wait().await
            }
        };
        while let Ok(Some(line)) =
            tokio::time::timeout(STDERR_DRAIN, self.child.stderr.recv()).await
        {
            self.on_stderr(line);
        }
        let code = match status {
            Ok(status) => status.code(),
            Err(err) => {
                tracing::warn!("wait for child: {err:#}");
                None
            }
        };
        self.tracker = ActivityTracker::default();
        self.asks.clear();
        self.publish_status();
        self.requests.clear();
        self.mode_requests.clear();
        let stderr_tail = std::mem::take(&mut self.stderr_tail).into();
        let reason = self.stop_reason.take();
        self.emit_host(HostEvent::ChildExited {
            code,
            stderr_tail,
            reason,
        });
        let end = self.end.take().unwrap_or(SessionState::Exited { code });
        if let Some(history) = &self.history {
            history.set_state(self.id, end);
        }
    }

    async fn write(&self, line: Outbound) -> Result<()> {
        self.child.send(serde_json::to_string(&line)?).await
    }

    fn next_request_id(&mut self) -> String {
        self.next_request += 1;
        format!("vigie2-{}", self.next_request)
    }

    fn publish_status(&mut self) {
        let next = LiveStatus {
            activity: self.tracker.activity(),
            turn: self.tracker.turn(),
            asks: self.asks.len(),
        };
        self.status.send_if_modified(|current| {
            let changed = *current != next;
            *current = next;
            changed
        });
    }

    fn emit_host(&mut self, event: HostEvent) {
        self.emit(EventBody::Host(event));
    }

    fn emit(&mut self, body: EventBody) -> Arc<Event> {
        self.seq += 1;
        let event = Arc::new(Event {
            seq: self.seq,
            time: Utc::now(),
            body,
        });
        // Queued to the store first, so history holds every event a subscriber could see.
        if let Some(history) = &self.history {
            history.record_event(self.id, &event);
        }
        let _ = self.events.send(event.clone());
        event
    }
}

/// Frames an allow note so the model reads it as tied to the approval, not as a stray mid-turn message.
fn allow_note(tool_name: &str, note: &str) -> String {
    let what = if tool_name == "ExitPlanMode" {
        "plan".to_string()
    } else {
        format!("{tool_name} call")
    };
    format!(
        "(Note attached to my approval of your {what}) {}",
        note.trim()
    )
}

/// `cwd` is the session folder, which relative tool paths resolve against.
fn permission_result(ask: &CanUseTool, answer: &Answer, cwd: &Path) -> Result<PermissionResult> {
    Ok(match answer {
        Answer::Allow {
            updated_input,
            accept_edits,
            folder,
            always_for_file,
            ..
        } => {
            let file = if *always_for_file {
                Some(
                    FileOffer::of(ask, cwd)
                        .ok_or_else(|| anyhow!("{} offers no file", ask.tool_name))?,
                )
            } else {
                None
            };
            PermissionResult::allow_with(
                updated_input.clone().unwrap_or_else(|| ask.input.clone()),
                grant::updates(ask, *accept_edits, folder.as_deref(), file.as_ref())?,
            )
        }
        Answer::Deny { message } => PermissionResult::deny(message.clone()),
        Answer::Questions { answers } => {
            if ask.tool_name != ASK_USER_QUESTION {
                bail!("{} is not {ASK_USER_QUESTION}", ask.tool_name);
            }
            let Value::Object(mut input) = ask.input.clone() else {
                bail!("{ASK_USER_QUESTION} input is not an object");
            };
            input.insert("answers".into(), serde_json::to_value(answers)?);
            PermissionResult::allow(Value::Object(input))
        }
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::replay::{Direction, Recording};

    fn recorded_lines(name: &str) -> Vec<Inbound> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("spike/runs")
            .join(format!("{name}.ndjson"));
        Recording::load(&path)
            .expect("load recording")
            .steps
            .into_iter()
            .filter(|step| step.dir == Direction::Out)
            .map(|step| serde_json::from_value(step.msg).expect("typed line"))
            .collect()
    }

    /// The background recording: a turn starts the task and ends, the task
    /// finishes later, and the CLI reports it in a turn of its own.
    #[test]
    fn background_task_keeps_the_session_busy_until_its_turn_ends() {
        let mut tracker = ActivityTracker::default();
        tracker.on_send();
        let mut results = 0;
        let mut after_first_result = Vec::new();
        for line in recorded_lines("background") {
            tracker.on_line(&line);
            if matches!(line, Inbound::Result(_)) {
                results += 1;
            }
            if results == 1 {
                after_first_result.push(tracker.activity());
            }
        }
        assert_eq!(results, 2);
        assert!(!after_first_result.is_empty());
        assert!(
            after_first_result
                .iter()
                .all(|activity| *activity == Activity::Busy)
        );
        assert_eq!(tracker.activity(), Activity::Idle);
    }

    #[test]
    fn a_plain_turn_goes_idle_on_result() {
        let mut tracker = ActivityTracker::default();
        assert_eq!(tracker.activity(), Activity::Idle);
        let mut seen_busy = false;
        for line in recorded_lines("basic") {
            tracker.on_line(&line);
            seen_busy |= tracker.activity() == Activity::Busy;
            if matches!(line, Inbound::Result(_)) {
                assert_eq!(tracker.activity(), Activity::Idle);
            }
        }
        assert!(seen_busy);
    }

    fn ask(tool_name: &str, input: Value) -> CanUseTool {
        serde_json::from_value(serde_json::json!({
            "tool_name": tool_name, "input": input, "tool_use_id": "t1"}))
        .unwrap()
    }

    #[test]
    fn answers_and_asks_read_the_wire() {
        let with_note: Answer =
            serde_json::from_value(serde_json::json!({"kind": "allow", "note": "then test"}))
                .unwrap();
        assert_eq!(
            with_note,
            Answer::Allow {
                updated_input: None,
                note: Some("then test".into()),
                accept_edits: false,
                folder: None,
                always_for_file: false,
            }
        );
        let bare: Answer = serde_json::from_value(serde_json::json!({"kind": "allow"})).unwrap();
        assert_eq!(
            serde_json::to_value(bare).unwrap(),
            serde_json::json!({"kind": "allow"})
        );
        let granting: Answer = serde_json::from_value(serde_json::json!({"kind": "allow",
            "accept_edits": true, "folder": "C:\\a", "always_for_file": true}))
        .unwrap();
        assert_eq!(
            granting,
            Answer::Allow {
                updated_input: None,
                note: None,
                accept_edits: true,
                folder: Some("C:\\a".into()),
                always_for_file: true,
            }
        );

        let queued = HostEvent::AskQueued {
            request_id: "r".into(),
            tool_name: "Bash".into(),
            tool_use_id: "t".into(),
            danger: Danger::from_reasons(vec!["Hard reset: git reset --hard".into()]),
            accept_edits: false,
            folder: None,
            file: None,
        };
        assert_eq!(
            serde_json::to_value(&queued).unwrap(),
            serde_json::json!({"event": "ask_queued", "request_id": "r", "tool_name": "Bash",
                "tool_use_id": "t", "danger": {"dangerous": true,
                "reasons": ["Hard reset: git reset --hard"]}}),
        );
        let stored: HostEvent = serde_json::from_value(serde_json::json!({"event": "ask_queued",
            "request_id": "r", "tool_name": "Bash", "tool_use_id": "t"}))
        .unwrap();
        assert!(matches!(stored, HostEvent::AskQueued {
            danger, accept_edits: false, folder: None, file: None, ..
        } if danger == Danger::default()));
    }

    fn cwd() -> PathBuf {
        std::env::temp_dir().join("vigie2-session").join("project")
    }

    #[test]
    fn answers_map_to_permission_results() {
        let bash = ask("Bash", serde_json::json!({"command": "ls"}));
        assert_eq!(
            permission_result(
                &bash,
                &Answer::Allow {
                    updated_input: None,
                    note: Some("then stop".into()),
                    accept_edits: false,
                    folder: None,
                    always_for_file: false,
                },
                &cwd(),
            )
            .unwrap(),
            PermissionResult::allow(serde_json::json!({"command": "ls"})),
        );
        let grant = Answer::Allow {
            updated_input: None,
            note: None,
            accept_edits: false,
            folder: Some("C:\\Users".into()),
            always_for_file: false,
        };
        assert!(
            permission_result(&bash, &grant, &cwd()).is_err(),
            "no folder was suggested"
        );
        let always = Answer::Allow {
            updated_input: None,
            note: None,
            accept_edits: false,
            folder: None,
            always_for_file: true,
        };
        assert!(
            permission_result(&bash, &always, &cwd()).is_err(),
            "Bash offers no file"
        );
        let edit = ask("Edit", serde_json::json!({"file_path": "notes.md"}));
        let PermissionResult::Allow {
            updated_permissions: Some(updates),
            ..
        } = permission_result(&edit, &always, &cwd()).unwrap()
        else {
            panic!("an always-for-file allow carries the file rule");
        };
        let rule = serde_json::to_value(&updates).unwrap();
        assert_eq!(rule[0]["rules"][0]["toolName"], "Edit");
        assert!(
            rule[0]["rules"][0]["ruleContent"]
                .as_str()
                .unwrap()
                .ends_with("/vigie2-session/project/notes.md")
        );
        let questions = Answer::Questions {
            answers: BTreeMap::from([("Pick?".to_string(), "blue".to_string())]),
        };
        assert!(permission_result(&bash, &questions, &cwd()).is_err());
        let question = ask(ASK_USER_QUESTION, serde_json::json!({"questions": []}));
        assert_eq!(
            permission_result(&question, &questions, &cwd()).unwrap(),
            PermissionResult::allow(
                serde_json::json!({"questions": [], "answers": {"Pick?": "blue"}})
            ),
        );
    }
}
