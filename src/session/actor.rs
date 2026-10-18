//! The task that owns a session's child: it relays commands to the child and
//! turns the child's lines into events.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;
use uuid::Uuid;

use super::activity::{ActivityTracker, LiveStatus};
use super::agents::{self, AgentTracker, AgentView, Relay, RelayFilter};
use super::background::{BackgroundTracker, BackgroundView};
use super::danger::Danger;
use super::events::{
    Answer, CommandInfo, Event, EventBody, GateFailure, HostEvent, Line, ModelChoice, Resolution,
    StopReason,
};
use super::fanout::{self, FanoutGate, FanoutOffer, Remember};
use super::gauges::FIVE_MINUTE_TTL_SECS;
use super::grant::{self, FileOffer, Granted, Steps};
use super::handle::Config;
use super::plan::PlanTracker;
use super::server_restart::ServerRestart;
use super::{attachments, danger, mcp, seed};
use crate::claude::child::{Child, Launch};
use crate::claude::protocol::{
    CanUseTool, CliRequest, ControlRequest, ControlResponse, HookCallback, HookOutput, HostRequest,
    Inbound, InitializeResponse, Outbound, PermissionMode, PermissionResult, Priority,
    SetPermissionModeResponse, System, UserMessage,
};
use crate::claude::tools::{self, ASK_USER_QUESTION, EXIT_PLAN_MODE, SEND_MESSAGE};
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
/// A subagent's unanswered ask is denied this long before its cache expires, so
/// the request wrapping up still reads the cache.
const ASK_DENY_MARGIN_SECS: u32 = 60;

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

struct FileGrant {
    file: FileOffer,
    /// Tool use ids of the step the grant was given in.
    step: Vec<String>,
}

/// Whether an auto-allowed ask shows in the discussion.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cue {
    Row,
    /// Same step as the grant: resolved like a tapped Allow.
    Silent,
}

struct PendingAsk {
    request_id: String,
    request: CanUseTool,
    /// The plan file an `ExitPlanMode` ask submits, when known.
    plan_path: Option<String>,
    /// The plan text an `ExitPlanMode` ask showed, when one was found.
    plan_text: Option<String>,
    /// A subagent's ask: when it is denied if still unanswered.
    timeout: Option<AskTimeout>,
    /// A call the fanout hook sent to this ask.
    fanout: Option<FanoutOffer>,
}

struct AskTimeout {
    agent_id: String,
    at: Instant,
    message: String,
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
    /// Stream lines up to this seq were handed to pruning.
    pruned_seq: u64,
    views: Views,
    tracker: ActivityTracker,
    agents: AgentTracker,
    background: BackgroundTracker,
    relays: RelayFilter,
    plans: PlanTracker,
    /// In arrival order.
    asks: Vec<PendingAsk>,
    /// Subagents whose ask timed out, with the message denying their later asks.
    wrapping_up: HashMap<String, String>,
    /// SendMessage calls to a cold agent the hook sent to an ask, by tool use id.
    cold_sends: HashMap<String, agents::ColdSend>,
    fanout: FanoutGate,
    /// Files allowed with `always_for_file`; their later asks are answered here.
    file_grants: Vec<FileGrant>,
    steps: Steps,
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
            pruned_seq: config.last_seq,
            views,
            tracker: ActivityTracker::default(),
            agents: AgentTracker::default(),
            background: BackgroundTracker::default(),
            relays: RelayFilter::default(),
            plans: PlanTracker::from_env(),
            asks: Vec::new(),
            wrapping_up: HashMap::new(),
            cold_sends: HashMap::new(),
            fanout: FanoutGate::default(),
            file_grants: Vec::new(),
            steps: Steps::default(),
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
        let hooks: Vec<(&str, &str)> = std::iter::once((SEND_MESSAGE, agents::SEND_MESSAGE_HOOK))
            .chain(
                fanout::GATED_TOOLS
                    .iter()
                    .map(|tool| (*tool, fanout::FANOUT_HOOK)),
            )
            .collect();
        let initialize = ControlRequest::new(
            request_id.clone(),
            HostRequest::initialize(vec![mcp::SERVER_NAME.into()], &hooks),
        );
        self.state = State::Initializing { request_id };
        if let Err(err) = self.write(Outbound::ControlRequest(initialize)).await {
            tracing::warn!("send initialize: {err:#}");
        }
        let mut stderr_open = true;
        loop {
            let ready = self.state == State::Ready;
            let kill_at = self.kill_at;
            let deny_at = self
                .asks
                .iter()
                .filter_map(|ask| ask.timeout.as_ref().map(|timeout| timeout.at))
                .min();
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
                _ = tokio::time::sleep_until(deny_at.unwrap_or_else(Instant::now)), if deny_at.is_some() => {
                    self.time_out_asks().await;
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
            self.steps.on_line(parsed);
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
                if let Some(history) = &self.history {
                    history.prune_turn_streams(self.id, self.pruned_seq + 1..=self.seq);
                    self.pruned_seq = self.seq;
                }
                let attachments = std::mem::take(&mut self.turn_attachments);
                if !attachments.is_empty() {
                    let paths = attachments.clone();
                    let hashes =
                        tokio::task::spawn_blocking(move || attachments::file_hashes(&paths))
                            .await
                            .unwrap_or_default();
                    self.emit_host(HostEvent::ClaudeAttached {
                        attachments,
                        hashes,
                    });
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
                if let Some(file) = &file {
                    let cue = match self.file_grants.iter().find(|grant| grant.file.same(file)) {
                        Some(grant) if grant.step.contains(&ask.tool_use_id) => Some(Cue::Silent),
                        Some(_) => Some(Cue::Row),
                        None => self
                            .uploads
                            .as_deref()
                            .is_some_and(|uploads| file.reads_within(uploads))
                            .then_some(Cue::Row),
                    };
                    if let Some(cue) = cue {
                        self.auto_allow(&request.request_id, ask, file.clone(), agent_id, cue)
                            .await;
                        return;
                    }
                }
                if let Some(agent_id) = &agent_id
                    && let Some(message) = self.wrapping_up.get(agent_id).cloned()
                {
                    self.auto_deny(&request.request_id, ask, agent_id.clone(), message)
                        .await;
                    return;
                }
                let (timeout, deny_at) = agent_id
                    .as_ref()
                    .map(|agent_id| self.ask_timeout(agent_id))
                    .unzip();
                let plan = if ask.tool_name == EXIT_PLAN_MODE {
                    self.plans
                        .review(ask, self.history.as_ref())
                        .await
                        .map(Box::new)
                } else {
                    None
                };
                let fanout = self.fanout.take(&ask.tool_use_id);
                self.asks.push(PendingAsk {
                    request_id: request.request_id.clone(),
                    request: (**ask).clone(),
                    plan_path: plan.as_ref().and_then(|plan| plan.path.clone()),
                    plan_text: plan.as_ref().map(|plan| plan.text.clone()),
                    timeout,
                    fanout: fanout.clone(),
                });
                let fanout = fanout.map(Box::new);
                let danger = match self.cold_sends.remove(&ask.tool_use_id) {
                    Some(cold) => Danger::from_reasons(vec![cold.warning()]),
                    None => danger::classify(ask, &self.cwd),
                };
                self.emit_host(HostEvent::AskQueued {
                    request_id: request.request_id.clone(),
                    tool_name: ask.tool_name.clone(),
                    tool_use_id: ask.tool_use_id.clone(),
                    danger,
                    accept_edits: grant::offers_accept_edits(ask),
                    folder: grant::folder_offer(ask, dirs::home_dir().as_deref()).map(Box::new),
                    file,
                    plan,
                    fanout,
                    agent_id,
                    deny_at,
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
            CliRequest::HookCallback(hook) => {
                let output = match hook.callback_id.as_str() {
                    agents::SEND_MESSAGE_HOOK => self.cold_send_hook(hook),
                    fanout::FANOUT_HOOK => {
                        self.fanout.decide(&hook.input, hook.tool_use_id.as_deref())
                    }
                    _ => HookOutput::default(),
                };
                let response = ControlResponse::success(request.request_id.clone(), output)
                    .expect("hook output serializes");
                if let Err(err) = self.write(Outbound::ControlResponse(response)).await {
                    tracing::warn!("answer hook_callback: {err:#}");
                }
            }
            CliRequest::Other(fields) => {
                let subtype = fields
                    .get("subtype")
                    .and_then(Value::as_str)
                    .unwrap_or("(none)");
                let response = ControlResponse::error(
                    request.request_id.clone(),
                    format!("vigie does not handle control request {subtype}"),
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

    /// Sends a SendMessage resuming a cold agent to an ask; lets any other run.
    fn cold_send_hook(&mut self, hook: &HookCallback) -> HookOutput {
        let Some(cold) = agents::cold_send(
            &self.agents,
            &self.relays,
            &hook.input["tool_input"],
            Utc::now(),
        ) else {
            return HookOutput::default();
        };
        let output = HookOutput::ask(cold.warning());
        if let Some(tool_use_id) = &hook.tool_use_id {
            self.cold_sends.insert(tool_use_id.clone(), cold);
        }
        output
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

    /// Answers one ask. An allow granting a file, a folder or accept edits also
    /// allows the queued asks the grant covers, as a plain allow each: the CLI
    /// asked them before it had the grant.
    async fn answer(&mut self, request_id: String, answer: Answer) -> Result<()> {
        let granted = self
            .ask_index(&request_id)
            .and_then(|index| Granted::of(&answer, &self.asks[index].request, &self.cwd));
        self.answer_one(request_id, answer).await?;
        let Some(granted) = granted else {
            return Ok(());
        };
        let covered: Vec<String> = self
            .asks
            .iter()
            .filter(|ask| granted.covers(&ask.request, &self.cwd))
            .map(|ask| ask.request_id.clone())
            .collect();
        for request_id in covered {
            let allow = Answer::Allow {
                updated_input: None,
                note: None,
                accept_edits: false,
                folder: None,
                always_for_file: false,
                for_session: false,
            };
            self.answer_one(request_id, allow).await?;
        }
        Ok(())
    }

    async fn answer_one(&mut self, request_id: String, answer: Answer) -> Result<()> {
        let index = self
            .ask_index(&request_id)
            .ok_or_else(|| anyhow!("no pending ask {request_id}"))?;
        let pending = &self.asks[index];
        let noted_plan = plan_with_note(&pending.request, pending.plan_text.as_deref(), &answer);
        let result =
            permission_result(&pending.request, &answer, noted_plan.as_deref(), &self.cwd)?;
        let response = ControlResponse::permission(request_id.clone(), &result);
        self.write(Outbound::ControlResponse(response)).await?;
        let ask = self.asks.remove(index);
        if let Answer::Allow {
            always_for_file: true,
            ..
        } = &answer
            && let Some(file) = FileOffer::of(&ask.request, &self.cwd)
            && !self.file_grants.iter().any(|grant| grant.file.same(&file))
        {
            let step = self.steps.of(&ask.request.tool_use_id);
            self.file_grants.push(FileGrant { file, step });
        }
        let remember = match &answer {
            Answer::Allow {
                for_session: true, ..
            } => Some(Remember::Allow),
            Answer::Deny {
                for_session: true, ..
            } => Some(Remember::Deny),
            _ => None,
        };
        if let Some(remember) = remember
            && let Some(offer) = &ask.fanout
        {
            self.fanout.remember(offer.key.clone(), remember);
        }
        // An approved plan is done with: the next plan in its file is new work.
        if let Answer::Allow { .. } = &answer
            && let Some(path) = ask.plan_path
            && let Some(history) = &self.history
        {
            history.forget_plan(path);
        }
        // A plan's note rides in the approved plan itself.
        let note = match &answer {
            Answer::Allow {
                note: Some(note), ..
            } if !note.trim().is_empty() && noted_plan.is_none() => {
                Some(allow_note(&ask.request.tool_name, note))
            }
            _ => None,
        };
        let bash_answer = match &answer {
            Answer::Allow { .. } => Some(BashAnswer::Allowed),
            Answer::Deny { message, .. } => Some(BashAnswer::Denied {
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
        cue: Cue,
    ) {
        let result = PermissionResult::allow(ask.input.clone());
        let response = ControlResponse::permission(request_id.to_string(), &result);
        if let Err(err) = self.write(Outbound::ControlResponse(response)).await {
            tracing::warn!("auto-allow {}: {err:#}", ask.tool_name);
            return;
        }
        if cue == Cue::Silent {
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

    async fn auto_deny(
        &mut self,
        request_id: &str,
        ask: &CanUseTool,
        agent_id: String,
        message: String,
    ) {
        let result = PermissionResult::deny(message.clone());
        let response = ControlResponse::permission(request_id.to_string(), &result);
        if let Err(err) = self.write(Outbound::ControlResponse(response)).await {
            tracing::warn!("auto-deny {}: {err:#}", ask.tool_name);
            return;
        }
        self.emit_host(HostEvent::AutoDenied {
            request_id: request_id.to_string(),
            tool_name: ask.tool_name.clone(),
            tool_use_id: ask.tool_use_id.clone(),
            agent_id,
            message,
        });
    }

    /// When a new ask of `agent_id` is denied if unanswered: its cache TTL from
    /// now, less the margin; as a timer, and as the time clients count down to.
    fn ask_timeout(&self, agent_id: &str) -> (AskTimeout, DateTime<Utc>) {
        let ttl_secs = self
            .agents
            .find(agent_id)
            .and_then(|agent| agent.cache.as_ref())
            .map_or(FIVE_MINUTE_TTL_SECS, |cache| cache.ttl_secs);
        let wait_secs = ttl_secs.saturating_sub(ASK_DENY_MARGIN_SECS);
        let message = format!(
            "No answer within {} minutes. Wrap up now: report what is done, what is left, \
             and what needed permission. No other tool use will be approved.",
            wait_secs / 60
        );
        let timeout = AskTimeout {
            agent_id: agent_id.to_string(),
            at: Instant::now() + Duration::from_secs(wait_secs.into()),
            message,
        };
        (timeout, Utc::now() + TimeDelta::seconds(wait_secs.into()))
    }

    /// Denies the first ask past its time, with every other pending ask of its
    /// agent, whose later asks are then denied on arrival.
    async fn time_out_asks(&mut self) {
        let now = Instant::now();
        let Some(expired) = self
            .asks
            .iter()
            .filter_map(|ask| ask.timeout.as_ref())
            .find(|timeout| timeout.at <= now)
        else {
            return;
        };
        let agent_id = expired.agent_id.clone();
        let message = expired.message.clone();
        let (denied, kept): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.asks).into_iter().partition(|ask| {
                ask.timeout
                    .as_ref()
                    .is_some_and(|timeout| timeout.agent_id == agent_id)
            });
        self.asks = kept;
        self.wrapping_up.insert(agent_id, message.clone());
        for ask in denied {
            let result = PermissionResult::deny(message.clone());
            let response = ControlResponse::permission(ask.request_id.clone(), &result);
            if let Err(err) = self.write(Outbound::ControlResponse(response)).await {
                tracing::warn!("time out {}: {err:#}", ask.request.tool_name);
            }
            self.emit_host(HostEvent::AskResolved {
                request_id: ask.request_id,
                resolution: Resolution::TimedOut {
                    message: message.clone(),
                },
            });
        }
        self.publish_status();
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
        format!("vigie-{}", self.next_request)
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

/// The plan with the allow's note appended, for an `ExitPlanMode` allow carrying one.
/// The CLI writes an allow's `plan` to the plan file and gives it to the model as the approved plan.
fn plan_with_note(ask: &CanUseTool, plan_text: Option<&str>, answer: &Answer) -> Option<String> {
    let Answer::Allow {
        note: Some(note),
        updated_input: None,
        ..
    } = answer
    else {
        return None;
    };
    if ask.tool_name != EXIT_PLAN_MODE || note.trim().is_empty() {
        return None;
    }
    plan_text.map(|text| {
        format!(
            "{}\n\n## Approval note\n\n{}\n",
            text.trim_end(),
            note.trim()
        )
    })
}

/// `cwd` is the session folder, which relative tool paths resolve against. `noted_plan`
/// from `plan_with_note` replaces the input's plan.
fn permission_result(
    ask: &CanUseTool,
    answer: &Answer,
    noted_plan: Option<&str>,
    cwd: &Path,
) -> Result<PermissionResult> {
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
            let input = match noted_plan {
                Some(plan) => {
                    let mut input = match &ask.input {
                        Value::Object(input) => input.clone(),
                        _ => serde_json::Map::new(),
                    };
                    input.insert("plan".into(), Value::String(plan.to_owned()));
                    Value::Object(input)
                }
                None => updated_input.clone().unwrap_or_else(|| ask.input.clone()),
            };
            PermissionResult::allow_with(
                input,
                grant::updates(ask, *accept_edits, folder.as_deref(), file.as_ref())?,
            )
        }
        Answer::Deny { message, .. } => PermissionResult::deny(message.clone()),
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
        std::env::temp_dir().join("vigie-session").join("project")
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
                    for_session: false,
                },
                None,
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
            for_session: false,
        };
        assert!(
            permission_result(&bash, &grant, None, &cwd()).is_err(),
            "no folder was suggested"
        );
        let always = Answer::Allow {
            updated_input: None,
            note: None,
            accept_edits: false,
            folder: None,
            always_for_file: true,
            for_session: false,
        };
        assert!(
            permission_result(&bash, &always, None, &cwd()).is_err(),
            "Bash offers no file"
        );
        let edit = ask("Edit", serde_json::json!({"file_path": "notes.md"}));
        let PermissionResult::Allow {
            updated_permissions: Some(updates),
            ..
        } = permission_result(&edit, &always, None, &cwd()).unwrap()
        else {
            panic!("an always-for-file allow carries the file rule");
        };
        let rule = serde_json::to_value(&updates).unwrap();
        assert_eq!(rule[0]["rules"][0]["toolName"], "Edit");
        assert!(
            rule[0]["rules"][0]["ruleContent"]
                .as_str()
                .unwrap()
                .ends_with("/vigie-session/project/notes.md")
        );
        let questions = Answer::Questions {
            answers: BTreeMap::from([("Pick?".to_string(), "blue".to_string())]),
        };
        assert!(permission_result(&bash, &questions, None, &cwd()).is_err());
        let question = ask(ASK_USER_QUESTION, serde_json::json!({"questions": []}));
        assert_eq!(
            permission_result(&question, &questions, None, &cwd()).unwrap(),
            PermissionResult::allow(
                serde_json::json!({"questions": [], "answers": {"Pick?": "blue"}})
            ),
        );
    }

    #[test]
    fn a_plan_note_rides_in_the_approved_plan() {
        let exit = ask(EXIT_PLAN_MODE, serde_json::json!({}));
        let allow = |note: Option<&str>| Answer::Allow {
            updated_input: None,
            note: note.map(str::to_owned),
            accept_edits: false,
            folder: None,
            always_for_file: false,
            for_session: false,
        };
        let noted = plan_with_note(&exit, Some("# Plan\n"), &allow(Some(" check it \n")));
        assert_eq!(
            noted.as_deref(),
            Some("# Plan\n\n## Approval note\n\ncheck it\n")
        );
        assert_eq!(
            permission_result(&exit, &allow(Some("check it")), noted.as_deref(), &cwd()).unwrap(),
            PermissionResult::allow(serde_json::json!({"plan": noted})),
        );
        assert_eq!(
            plan_with_note(&exit, Some("# Plan"), &allow(Some("  "))),
            None
        );
        assert_eq!(plan_with_note(&exit, None, &allow(Some("check it"))), None);
        let bash = ask("Bash", serde_json::json!({"command": "ls"}));
        assert_eq!(
            plan_with_note(&bash, Some("# Plan"), &allow(Some("check it"))),
            None
        );
    }
}
