//! The task that owns a session's child: it relays commands to the child and
//! turns the child's lines into events.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use chrono::Utc;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;
use uuid::Uuid;

use super::activity::{ActivityTracker, LiveStatus};
use super::agents::{self, AgentTracker, AgentView, Relay, RelayFilter};
use super::background::{BackgroundTracker, BackgroundView};
use super::events::{
    Answer, CommandInfo, Event, EventBody, GateFailure, HostEvent, Line, ModelChoice, Resolution,
    StopReason,
};
use super::grant::{self, FileOffer};
use super::handle::Config;
use super::plan::PlanTracker;
use super::server_restart::ServerRestart;
use super::{attachments, danger, mcp, seed};
use crate::claude::child::{Child, Launch};
use crate::claude::protocol::{
    CanUseTool, CliRequest, ControlRequest, ControlResponse, HostRequest, Inbound,
    InitializeResponse, Outbound, PermissionMode, PermissionResult, Priority,
    SetPermissionModeResponse, System, UserMessage,
};
use crate::claude::tools::{self, ASK_USER_QUESTION, EXIT_PLAN_MODE};
use crate::store::{BashAnswer, SessionState, Store};

const STDERR_TAIL: usize = 50;
/// Time a child gets to exit after its stdin closes before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(10);
const STDERR_DRAIN: Duration = Duration::from_secs(1);
const FIRST_PARTY: &str = "firstParty";
/// `apiKeySource` of a claude.ai OAuth session.
const OAUTH_KEY_SOURCE: &str = "none";
/// `rateLimitType` of a limit that draws on usage credits.
const OVERAGE_LIMIT: &str = "overage";

pub enum Command {
    Send {
        message: attachments::Message,
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
    Relay {
        task_id: String,
        message: attachments::Message,
        client_id: Option<String>,
        reply: oneshot::Sender<Result<()>>,
    },
    Stop(Option<StopReason>),
}

/// The actor's ends of the watch channels the handle reads.
pub struct Views {
    pub status: watch::Sender<LiveStatus>,
    pub agents: watch::Sender<Vec<AgentView>>,
    pub background: watch::Sender<Vec<BackgroundView>>,
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
    /// The plan file an `ExitPlanMode` ask submits, when known.
    plan_path: Option<String>,
}

/// A host control request awaiting its `control_response`.
struct PendingRequest {
    reply: oneshot::Sender<Result<ControlResponse>>,
    kind: RequestKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestKind {
    Plain,
    /// A `set_permission_mode`, whose success announces the new mode.
    SetsMode,
    /// Its reply line is kept out of the events.
    Unrecorded,
}

impl RequestKind {
    fn of(request: &HostRequest) -> Self {
        match request {
            HostRequest::SetPermissionMode { .. } => Self::SetsMode,
            HostRequest::GetContextUsage { .. } | HostRequest::GetSettings { .. } => {
                Self::Unrecorded
            }
            _ => Self::Plain,
        }
    }
}

pub struct Actor {
    id: Uuid,
    cwd: PathBuf,
    launch: Launch,
    child: Child,
    events: broadcast::Sender<Arc<Event>>,
    history: Option<Store>,
    server_restart: Option<ServerRestart>,
    /// How the session ends, once decided; an unrequested exit leaves it `None`.
    end: Option<SessionState>,
    /// Set when the stop was requested for a reason.
    stop_reason: Option<StopReason>,
    seq: u64,
    views: Views,
    tracker: ActivityTracker,
    agents: AgentTracker,
    background: BackgroundTracker,
    relays: RelayFilter,
    plans: PlanTracker,
    /// In arrival order.
    asks: Vec<PendingAsk>,
    /// Files allowed with `always_for_file`; their later asks are answered here.
    file_grants: Vec<FileOffer>,
    /// Reads of files in it are answered here.
    uploads: Option<PathBuf>,
    /// Set by `set_attachments`, announced and cleared when the turn ends.
    turn_attachments: Vec<String>,
    /// Keyed by request id.
    pending: HashMap<String, PendingRequest>,
    /// Last mode announced with `HostEvent::PermissionMode`.
    permission_mode: Option<PermissionMode>,
    next_request: u64,
    state: State,
    kill_at: Option<Instant>,
    stderr_tail: VecDeque<String>,
}

impl Actor {
    pub fn new(
        config: Config,
        child: Child,
        events: broadcast::Sender<Arc<Event>>,
        views: Views,
    ) -> Self {
        Self {
            id: config.id,
            cwd: config.cwd,
            launch: config.launch,
            child,
            events,
            history: config.history,
            server_restart: config.server_restart,
            end: None,
            stop_reason: None,
            seq: config.last_seq,
            views,
            tracker: ActivityTracker::default(),
            agents: AgentTracker::default(),
            background: BackgroundTracker::default(),
            relays: RelayFilter::default(),
            plans: PlanTracker::from_env(),
            asks: Vec::new(),
            file_grants: Vec::new(),
            uploads: config.uploads,
            turn_attachments: Vec::new(),
            pending: HashMap::new(),
            permission_mode: None,
            next_request: 0,
            state: State::Ready,
            kill_at: None,
            stderr_tail: VecDeque::new(),
        }
    }

    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        self.emit_host(HostEvent::Started {
            launch: self.launch,
            cwd: self.cwd.clone(),
        });
        let request_id = self.next_request_id();
        let initialize = ControlRequest::new(
            request_id.clone(),
            HostRequest::initialize(vec![mcp::SERVER_NAME.into()]),
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
                .and_then(|id| self.pending.get(id))
                .is_some_and(|pending| pending.kind == RequestKind::Unrecorded)
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
            if self.agents.on_line(parsed, event.time) {
                self.publish_agents();
            }
            if self.background.on_line(parsed, event.time) {
                self.publish_background();
            }
            for verdict in self.relays.on_line(parsed) {
                self.emit_verdict(verdict);
            }
            self.plans.on_line(parsed);
            self.handle_line(parsed).await;
        }
    }

    fn publish_status(&self) {
        let next = LiveStatus {
            activity: self.tracker.activity(),
            turn: self.tracker.turn(),
            asks: self.asks.len(),
        };
        publish(&self.views.status, next);
    }

    fn publish_agents(&self) {
        publish(&self.views.agents, self.agents.agents().to_vec());
    }

    fn publish_background(&self) {
        publish(&self.views.background, self.background.tasks().to_vec());
    }

    fn emit_verdict(&mut self, verdict: agents::RelayVerdict) {
        self.emit_host(HostEvent::RelayChecked {
            relay_seq: verdict.relay_seq,
            task_id: verdict.task_id,
            agent_tool_use_id: verdict.agent_tool_use_id,
            outcome: verdict.outcome,
        });
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
            Inbound::Result(_) => {
                let attachments = std::mem::take(&mut self.turn_attachments);
                if !attachments.is_empty() {
                    self.emit_host(HostEvent::ClaudeAttached { attachments });
                }
            }
            Inbound::ControlRequest(request) => self.on_cli_request(request).await,
            Inbound::ControlResponse(response) => self.on_control_response(response),
            Inbound::ControlCancelRequest(cancel) => self.on_cancel(&cancel.request_id),
            _ => {}
        }
    }

    async fn on_cli_request(&mut self, request: &ControlRequest<CliRequest>) {
        match &request.request {
            CliRequest::CanUseTool(ask) => {
                let file = FileOffer::of(ask, &self.cwd);
                let agent_id = ask
                    .agent_id
                    .as_ref()
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(granted) = file.as_ref().filter(|file| {
                    self.file_grants.iter().any(|grant| grant.same(file))
                        || self
                            .uploads
                            .as_deref()
                            .is_some_and(|uploads| file.reads_within(uploads))
                }) {
                    self.auto_allow(&request.request_id, ask, granted.clone(), agent_id)
                        .await;
                    return;
                }
                let plan = if ask.tool_name == EXIT_PLAN_MODE {
                    self.plans
                        .review(ask, self.history.as_ref())
                        .await
                        .map(Box::new)
                } else {
                    None
                };
                self.asks.push(PendingAsk {
                    request_id: request.request_id.clone(),
                    request: (**ask).clone(),
                    plan_path: plan.as_ref().and_then(|plan| plan.path.clone()),
                });
                self.emit_host(HostEvent::AskQueued {
                    request_id: request.request_id.clone(),
                    tool_name: ask.tool_name.clone(),
                    tool_use_id: ask.tool_use_id.clone(),
                    danger: danger::classify(ask, &self.cwd),
                    accept_edits: grant::offers_accept_edits(ask),
                    folder: grant::folder_offer(ask, dirs::home_dir().as_deref()).map(Box::new),
                    file,
                    plan,
                    agent_id,
                });
                self.publish_status();
            }
            CliRequest::McpMessage(message) => {
                let response = if message.server_name == mcp::SERVER_NAME {
                    let handled = mcp::handle(
                        &message.message,
                        &self.cwd,
                        self.history.as_ref(),
                        self.server_restart.as_ref(),
                    )
                    .await;
                    if let mcp::Effect::SetAttachments(paths) = handled.effect {
                        self.turn_attachments = paths;
                    }
                    ControlResponse::mcp(request.request_id.clone(), handled.reply)
                } else {
                    ControlResponse::error(
                        request.request_id.clone(),
                        format!("unknown MCP server {}", message.server_name),
                    )
                };
                if let Err(err) = self.write(Outbound::ControlResponse(response)).await {
                    tracing::warn!("answer mcp_message: {err:#}");
                }
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
        } else if let Some(pending) = self.pending.remove(request_id) {
            if pending.kind == RequestKind::SetsMode
                && let Ok(changed) = response.payload::<SetPermissionModeResponse>()
            {
                self.set_mode(&changed.mode);
            }
            let _ = pending.reply.send(Ok(response.clone()));
        }
    }

    /// Announces `mode` when it differs from the last one announced.
    fn set_mode(&mut self, mode: &PermissionMode) {
        if self.permission_mode.as_ref() == Some(mode) {
            return;
        }
        self.permission_mode = Some(mode.clone());
        self.emit_host(HostEvent::PermissionMode { mode: mode.clone() });
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
                message,
                seed,
                client_id,
                reply,
            } => {
                let _ = reply.send(self.send_user(message, seed, client_id, None).await);
            }
            Command::Control { request, reply } => {
                let request_id = self.next_request_id();
                let kind = RequestKind::of(&request);
                let line =
                    Outbound::ControlRequest(ControlRequest::new(request_id.clone(), request));
                match self.write(line).await {
                    Ok(()) => {
                        self.pending
                            .insert(request_id, PendingRequest { reply, kind });
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
            Command::Relay {
                task_id,
                message,
                client_id,
                reply,
            } => {
                let _ = reply.send(self.relay(task_id, message, client_id).await);
            }
            Command::Stop(reason) => self.begin_stop(reason),
        }
    }

    async fn send_user(
        &mut self,
        message: attachments::Message,
        seed: Option<String>,
        client_id: Option<String>,
        priority: Option<Priority>,
    ) -> Result<()> {
        let composed = message.composed();
        // Emitted before the write, so it precedes the CLI's echo in seq order.
        if seed.is_none() || !message.is_blank() {
            self.emit_host(HostEvent::UserSent {
                text: message.text,
                attachments: message.attachments,
                client_id,
                mid_turn: self.tracker.turn(),
            });
        }
        let written = match &seed {
            Some(seed) => seed::first_message(seed, &composed),
            None => composed,
        };
        self.write_user(written, priority).await
    }

    async fn relay(
        &mut self,
        task_id: String,
        message: attachments::Message,
        client_id: Option<String>,
    ) -> Result<()> {
        let agent = self
            .agents
            .find(&task_id)
            .ok_or_else(|| anyhow!("no agent {task_id} in this Claude process"))?;
        let (name, agent_tool_use_id) = (agent.name.clone(), agent.tool_use_id.clone());
        let text = message.composed();
        let wrapped = agents::wrap(&name, &task_id, &text);
        // Emitted before the write, so it precedes the CLI's echo in seq order.
        let sent = self.emit(EventBody::Host(HostEvent::RelaySent {
            text: message.text,
            attachments: message.attachments,
            client_id,
            task_id: task_id.clone(),
            agent_tool_use_id: agent_tool_use_id.clone(),
            name,
            wrapped: wrapped.clone(),
            mid_turn: self.tracker.turn(),
        }));
        self.relays.sent(Relay {
            seq: sent.seq,
            task_id,
            agent_tool_use_id,
            text,
            wrapped: wrapped.clone(),
        });
        self.write_user(wrapped, None).await
    }

    /// Writes a user message to the child, which starts or joins a turn.
    async fn write_user(&mut self, text: String, priority: Option<Priority>) -> Result<()> {
        let session_id = self.launch.claude_session_id().to_string();
        let mut message = UserMessage::text(session_id, text);
        if let Some(priority) = priority {
            message = message.with_priority(priority);
        }
        self.write(Outbound::User(message)).await?;
        if let Some(history) = &self.history {
            history.claude_session_used(self.id);
        }
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
        // An approved plan is done with: the next plan in its file is new work.
        if let Answer::Allow { .. } = &answer
            && let Some(path) = ask.plan_path
            && let Some(history) = &self.history
        {
            history.forget_plan(path);
        }
        let note = match &answer {
            Answer::Allow {
                note: Some(note), ..
            } if !note.trim().is_empty() => Some(allow_note(&ask.request.tool_name, note)),
            _ => None,
        };
        let bash_answer = match &answer {
            Answer::Allow { .. } => Some(BashAnswer::Allowed),
            Answer::Deny { message } => Some(BashAnswer::Denied {
                message: message.clone(),
            }),
            Answer::Questions { .. } => None,
        };
        let resolved = self.emit(EventBody::Host(HostEvent::AskResolved {
            request_id,
            resolution: Resolution::Answered { answer },
        }));
        if ask.request.tool_name == tools::BASH
            && let Some(bash_answer) = bash_answer
            && let Some(command) = ask.request.input.get("command").and_then(Value::as_str)
            && let Some(history) = &self.history
        {
            let description = ask
                .request
                .input
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned);
            history.record_bash_ask(
                command.to_owned(),
                &self.cwd,
                description,
                bash_answer,
                resolved.time,
            );
        }
        self.publish_status();
        if let Some(note) = note {
            self.send_user(note.into(), None, None, Some(Priority::Next))
                .await?;
        }
        Ok(())
    }

    async fn auto_allow(
        &mut self,
        request_id: &str,
        ask: &CanUseTool,
        file: FileOffer,
        agent_id: Option<String>,
    ) {
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
            agent_id,
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
        for verdict in self.relays.end() {
            self.emit_verdict(verdict);
        }
        self.agents = AgentTracker::default();
        self.publish_agents();
        self.background = BackgroundTracker::default();
        self.publish_background();
        self.pending.clear();
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

/// Sends `next` to the view's readers when it differs from what they hold.
fn publish<T: PartialEq>(view: &watch::Sender<T>, next: T) {
    view.send_if_modified(|current| {
        let changed = *current != next;
        *current = next;
        changed
    });
}

/// Frames an allow note so the model reads it as tied to the approval, not as a stray mid-turn message.
fn allow_note(tool_name: &str, note: &str) -> String {
    let what = if tool_name == EXIT_PLAN_MODE {
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
    use std::collections::BTreeMap;

    use super::*;

    fn ask(tool_name: &str, input: Value) -> CanUseTool {
        serde_json::from_value(serde_json::json!({
            "tool_name": tool_name, "input": input, "tool_use_id": "t1"}))
        .unwrap()
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
