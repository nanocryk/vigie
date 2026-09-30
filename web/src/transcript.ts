import {
  type AgentState,
  type CliLine,
  type CommandInfo,
  type ContentBlock,
  type ModelChoice,
  type Danger,
  type FileOffer,
  type FolderOffer,
  type GateFailure,
  type HostEvent,
  type PlanReview,
  type RelayFailure,
  type RestartMode,
  type StopReason,
  type WireEvent,
  isRecord,
  parseCliLine,
  stringOrNull,
} from "@/wire";
import { composeText } from "@/attachments";
import { toolDiff } from "@/diff";

/** The `attachments` field of an item: absent when there are none. */
function attached(paths: readonly string[]): { attachments?: readonly string[] } {
  return paths.length === 0 ? {} : { attachments: paths };
}

/** The text Claude got for a user item, its attachments included. */
function composedText(item: { text: string; attachments?: readonly string[] }): string {
  return composeText(item.text, item.attachments ?? []);
}

export type ToolStatus = "running" | "ok" | "error" | "unfinished";

export type TaskEndStatus = "ok" | "stopped" | "error";

const WAKE_ONE = "Stopped background task: ";
const WAKE_MANY = "Stopped background tasks: ";
const WAKE_SEPARATOR = "; ";

/** The message vigie2 sends to wake Claude after stopping background tasks. */
export function wakeText(names: readonly string[]): string {
  return names.length === 1
    ? `${WAKE_ONE}${names[0] ?? ""}`
    : `${WAKE_MANY}${names.join(WAKE_SEPARATOR)}`;
}

/** The stopped tasks a `wakeText` message names, or null for any other message. */
export function wakeStops(text: string): string[] | null {
  if (text.startsWith(WAKE_ONE)) return [text.slice(WAKE_ONE.length)];
  if (text.startsWith(WAKE_MANY)) return text.slice(WAKE_MANY.length).split(WAKE_SEPARATOR);
  return null;
}

const TASK_NOTIFICATION = "task-notification";

/** The fields of a `<task-notification>` message, or null when it is not one. */
export function parseTaskNotification(
  text: string,
): { status: TaskEndStatus; summary: string; outputFile: string | null } | null {
  if (!text.trimStart().startsWith(`<${TASK_NOTIFICATION}>`)) return null;
  const field = (name: string) =>
    new RegExp(`<${name}>([\\s\\S]*?)</${name}>`).exec(text)?.[1]?.trim() ?? null;
  const summary = field("summary");
  const status = field("status");
  if (summary === null || status === null) return null;
  return {
    status: status === "completed" ? "ok" : status === "failed" || status === "error" ? "error" : "stopped",
    summary,
    outputFile: field("output-file"),
  };
}

/**
 * `on_host`: written to the CLI, not echoed yet. `received`: the CLI echoed it.
 * `handled`: the CLI answered without echoing it (a command it runs locally).
 */
export type Delivery = "on_host" | "received" | "handled";

/**
 * `time` (epoch ms, null when the event time does not parse) is when the item
 * began: the server sending a user message, the first text of a Claude bubble,
 * the tool call line.
 */
export type Item = {
  time: number | null;
  /** Seq of the event that added it. */
  seq: number;
  /** Not shown: the parts of a faithful relay in the main discussion, `set_attachments` calls. */
  hidden?: true;
} & ItemBody;

type ItemBody =
  | {
      kind: "user";
      key: string;
      text: string;
      delivery: Delivery;
      clientId: string | null;
      /** Typed in an agent's view and passed on by the main agent. */
      relayed?: true;
      /** Paths of the attached files; absent when none. */
      attachments?: readonly string[];
    }
  | {
      kind: "claude";
      key: string;
      /** Id of the Claude message holding it; null when not reported. */
      messageId: string | null;
      text: string;
      streaming: boolean;
      /** Paths of the attached files; absent when none. */
      attachments?: readonly string[];
    }
  | { kind: "thinking"; key: string; text: string }
  | {
      kind: "tool";
      key: string;
      /** Id of the Claude message holding the call; null when not reported. */
      messageId: string | null;
      toolUseId: string;
      name: string;
      summary: string;
      input: unknown;
      status: ToolStatus;
      output: string | null;
      /** The CLI's `tool_use_result`, undefined until reported. */
      result: unknown;
      /** When its result arrived. */
      endTime: number | null;
      /** Text the CLI injected after the call: a loaded skill's content. */
      attached: string | null;
    }
  | { kind: "notice"; key: string; tone: "info" | "error"; text: string; detail: string | null }
  | { kind: "restart"; key: string; mode: RestartMode; exchanges: number; seed: string | null }
  /** An Agent call: the one line its agent shows in the discussion that started it. */
  | { kind: "agent"; key: string; toolUseId: string; name: string }
  /** The prompt an agent was started with, first in its own view. */
  | { kind: "brief"; key: string; text: string }
  /** A background task ended: the CLI's notification, written in the user's place. */
  | {
      kind: "task_end";
      key: string;
      status: TaskEndStatus;
      summary: string;
      /** Where the task's output was written, when it says. */
      outputFile: string | null;
      /** The notification as the CLI wrote it. */
      raw: string;
    }
  /** The main agent did not pass a relayed message on as asked. */
  | { kind: "relay_failed"; key: string; reason: RelayFailure }
  /** The files a turn's edit tools changed, its subagents' included; lines summed over the calls. */
  | { kind: "changes"; key: string; files: number; added: number; removed: number };

/** What an agent's view knows of it from the lines: its Agent call and task. */
export interface AgentInfo {
  /** The Agent call; the agent's lines carry it as `parent_tool_use_id`. */
  toolUseId: string;
  /** Null until its `task_started`. */
  taskId: string | null;
  name: string;
  /** The Agent call of the agent that started it; null for the main agent. */
  parentToolUseId: string | null;
  /** Null until a task line reported it. */
  state: AgentState | null;
  /** Time of its latest line, its Agent call's or a task line's included. */
  lastActive: number | null;
}

/** Tools starting a subagent. */
const AGENT_TOOLS: ReadonlySet<string> = new Set(["Agent", "Task"]);
const LOCAL_AGENT = "local_agent";
/** The main agent's whole reply once it forwarded a relayed message. */
export const RELAY_ACK = "[Relay acknowledged]";

export function agentStateOf(status: string | null): AgentState | null {
  switch (status) {
    case "running":
    case "pending":
      return "running";
    case "completed":
      return "completed";
    case "stopped":
    case "killed":
      return "stopped";
    case "failed":
    case "error":
      return "failed";
    default:
      return null;
  }
}

const RELAY_FAILURE_TEXT: Record<RelayFailure, string> = {
  altered: "the main agent passed on a different text",
  other_recipient: "the main agent sent it to another agent",
  not_forwarded: "the main agent did not pass it on",
  no_acknowledgement: "the main agent did not confirm it",
  ended: "the Claude process stopped first",
  unknown: "the main agent did not relay it as asked",
};

export function relayFailureText(reason: RelayFailure): string {
  return `Relay failed: ${RELAY_FAILURE_TEXT[reason]}. See the main discussion.`;
}

export function restartLabel(item: { mode: RestartMode; exchanges: number }): string {
  if (item.mode === "clear") return "Cleared";
  if (item.mode === "blank") return "Restarted: blank";
  const noun = item.exchanges === 1 ? "exchange" : "exchanges";
  return `Restarted: ${item.exchanges} ${noun} given as context`;
}

/** Epoch ms of an RFC 3339 time; fractions beyond milliseconds are dropped. */
export function parseTime(text: string): number | null {
  const time = Date.parse(text.replace(/(\.\d{3})\d+/, "$1"));
  return Number.isNaN(time) ? null : time;
}

/** A tool row shows its time only when the minute differs from the item above. */
export function toolTimeVisible(items: readonly Item[], index: number): boolean {
  const item = items[index];
  if (item?.time === null || item === undefined) return false;
  const previous = items[index - 1]?.time ?? null;
  return previous === null || Math.floor(previous / 60_000) !== Math.floor(item.time / 60_000);
}

/**
 * A turn runs with nothing on screen showing it: no tool running, no live
 * thinking, no text streaming. The discussion then ends with animated dots.
 */
export function quietWork(items: readonly Item[], turnOpen: boolean): boolean {
  if (!turnOpen) return false;
  if (items.some((item) => item.kind === "tool" && item.status === "running")) return false;
  const last = items.at(-1);
  if (last?.kind === "thinking") return false;
  return !(last?.kind === "claude" && last.streaming);
}

/**
 * The text Claude wrote before tool call `toolUseId` in the same message, in
 * whichever discussion holds it; null when that message has none.
 */
export function askContext(state: TranscriptState, toolUseId: string): string | null {
  for (const items of [state.items, ...[...state.scopes.values()].map((scope) => scope.items)]) {
    const at = items.findIndex((item) => item.kind === "tool" && item.toolUseId === toolUseId);
    const call = items[at];
    if (call?.kind !== "tool") continue;
    if (call.messageId === null) return null;
    for (let index = at - 1; index >= 0; index -= 1) {
      const item = items[index];
      if (item?.kind === "thinking") continue;
      if (item?.kind !== "claude" && item?.kind !== "tool") return null;
      if (item.messageId !== call.messageId) return null;
      if (item.kind !== "claude" || item.hidden === true) continue;
      const text = item.text.trim();
      if (text !== "") return text;
    }
    return null;
  }
  return null;
}

/**
 * Key of the Claude bubble ending a turn: the last item, past hidden ones and
 * the changes row, when it is Claude text. Null when the turn ended otherwise
 * (a tool call, an interrupt, an error).
 */
export function turnEndBubble(items: readonly Item[]): string | null {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item === undefined) return null;
    if (item.hidden === true || item.kind === "changes") continue;
    return item.kind === "claude" && item.text.trim() !== "" ? item.key : null;
  }
  return null;
}

export interface PendingAsk {
  requestId: string;
  toolName: string;
  toolUseId: string;
  /** Undefined when the matching `can_use_tool` line was not seen. */
  input: unknown;
  danger: Danger;
  /** An edit ask offering a switch to Accept edits. */
  acceptEdits: boolean;
  folder: FolderOffer | null;
  /** The file an allow can grant for good. */
  file: FileOffer | null;
  /** The plan of an `ExitPlanMode` ask, when one was found. */
  plan: PlanReview | null;
  /** Task id of the subagent asking; null for the main agent. */
  agentId: string | null;
}

/** The plan an ask carries, else the one its input names (older CLIs). */
function askPlan(toolName: string, plan: PlanReview | null, input: unknown): PlanReview | null {
  if (toolName !== "ExitPlanMode") return null;
  if (plan !== null) return plan;
  if (!isRecord(input) || typeof input["plan"] !== "string") return null;
  return { path: stringOrNull(input["planFilePath"]), text: input["plan"], previous: null };
}

export interface TranscriptState {
  lastSeq: number;
  /**
   * Seq of the first event shown. Events below it, the prelude of a page of
   * history, only set state: they add no item.
   */
  firstSeq: number;
  items: readonly Item[];
  /** In arrival order; the first one is the card on display. */
  asks: readonly PendingAsk[];
  tools: ReadonlyMap<string, number>;
  /** Streamed text items per message id still waiting for their `assistant` line, oldest first. */
  openText: ReadonlyMap<string, readonly number[]>;
  /** Streamed text item per `messageId + block index`. */
  liveBlocks: ReadonlyMap<string, number>;
  streamMessage: string | null;
  requests: ReadonlyMap<string, { toolName: string; input: unknown }>;
  /** An interrupt notice was added since the last `result`. */
  interrupted: boolean;
  /** A stop for a reason began and the child has not exited yet. */
  stopping: StopReason | null;
  /** A turn has started and its `result` has not arrived. */
  turnOpen: boolean;
  /** Seq of the event that opened the latest turn; null before any. */
  turnStartSeq: number | null;
  /** User messages not echoed yet, oldest first. */
  outbox: readonly Outgoing[];
  /** Folder the session runs in, from its `started` event; tool paths inside it show relative. */
  cwd: string | null;
  /** CLI permission mode (`default`, `acceptEdits`, `plan`, ...), null until reported. */
  permissionMode: string | null;
  /** Seed of the latest continue, until the echo of the message starting with it. */
  pendingSeed: string | null;
  /** Models and commands from the latest `catalog`; null until one arrived. */
  catalog: Catalog | null;
  /** Commands the latest `system/init` reported as working only in the TUI. */
  terminalCommands: readonly string[];
  /** Agents seen in the lines, by Agent call id, in the order they appeared. */
  agents: ReadonlyMap<string, AgentInfo>;
  /**
   * Each agent's own discussion, by Agent call id: the lines carrying it as
   * `parent_tool_use_id`, its brief, relays and end. The main discussion is
   * this state's own `items`.
   */
  scopes: ReadonlyMap<string, TranscriptState>;
}

export interface Catalog {
  models: readonly ModelChoice[];
  commands: readonly CommandInfo[];
}

interface Outgoing {
  /** Index of the user item. */
  index: number;
  /** The next `assistant` or `result` line answers this message: no turn was running before it. */
  armed: boolean;
  /** Marked handled before the latest `result`; dropped at the next one. */
  settled: boolean;
}

export const emptyTranscript: TranscriptState = {
  lastSeq: 0,
  firstSeq: 0,
  items: [],
  asks: [],
  tools: new Map(),
  openText: new Map(),
  liveBlocks: new Map(),
  streamMessage: null,
  requests: new Map(),
  interrupted: false,
  stopping: null,
  turnOpen: false,
  turnStartSeq: null,
  outbox: [],
  cwd: null,
  permissionMode: null,
  pendingSeed: null,
  catalog: null,
  terminalCommands: [],
  agents: new Map(),
  scopes: new Map(),
};

const INTERRUPT_PREFIX = "[Request interrupted by user";
const SKILL_TOOL = "Skill";
/** vigie2's attach tool: its effect shows as chips on the reply, so its row is hidden. */
const SET_ATTACHMENTS_TOOL = "mcp__vigie2__set_attachments";

const STOPPED: Record<StopReason, string> = {
  kill: "Stopped from Home",
  revoke: "Grants revoked",
  shutdown: "Server stopped",
};
const SUMMARY_LIMIT = 300;
const UNKNOWN_MESSAGE = "?";

function field(input: unknown, key: string): string | null {
  if (!isRecord(input)) return null;
  const value = input[key];
  return typeof value === "string" ? value : null;
}

function oneLine(text: string): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > SUMMARY_LIMIT ? `${flat.slice(0, SUMMARY_LIMIT)}...` : flat;
}

const LIST_SHOWN = 5;

function shortValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) {
    const shown = value
      .slice(0, LIST_SHOWN)
      .map((item: unknown) => (Array.isArray(item) ? "[...]" : shortValue(item)))
      .join(", ");
    const more = value.length - LIST_SHOWN;
    return more > 0 ? `[${shown}; and ${more} more...]` : `[${shown}]`;
  }
  if (isRecord(value)) return "{...}";
  return String(value);
}

/** `key value · key value`, nested values shortened. */
function keyValues(input: unknown): string {
  if (!isRecord(input)) return input === undefined ? "" : shortValue(input);
  return Object.entries(input)
    .map(([key, value]) => `${key} ${shortValue(value)}`)
    .join(" · ");
}

/** `path` relative to `cwd` when inside it (on a folder boundary), else unchanged. */
export function relativePath(path: string, cwd: string | null): string {
  if (cwd === null) return path;
  const base = cwd.replace(/[\\/]+$/, "");
  // Drive-letter paths are Windows ones, where case and separator style do not matter.
  const windows = /^[a-zA-Z]:/.test(base);
  const fold = (text: string) => (windows ? text.toLowerCase().replace(/\\/g, "/") : text);
  if (fold(path) === fold(base)) return ".";
  const head = path.slice(0, base.length);
  const separator = path.charAt(base.length);
  if (fold(head) !== fold(base) || (separator !== "/" && separator !== "\\")) return path;
  return path.slice(base.length + 1);
}

function pathField(input: unknown, key: string, cwd: string | null): string | null {
  const path = field(input, key);
  return path === null ? null : relativePath(path, cwd);
}

function withScope(main: string | null, input: unknown, cwd: string | null): string | null {
  if (main === null) return null;
  const path = pathField(input, "path", cwd);
  return path === null ? main : `${main} in ${path}`;
}

function firstLine(text: string | null): string | null {
  if (text === null) return null;
  const line = text.split("\n").find((candidate) => candidate.trim() !== "");
  return line === undefined ? null : line.replace(/^#+\s*/, "");
}

const PLAN_FILE = /[\\/]\.claude[\\/]plans[\\/]([^\\/]+\.md)$/i;

/** The name of the plan file an edit-tool input targets, as `tidy-aurora.md`. */
export function planFileName(input: unknown): string | null {
  return field(input, "file_path")?.match(PLAN_FILE)?.[1] ?? null;
}

function questionCount(input: unknown): string | null {
  if (!isRecord(input) || !Array.isArray(input["questions"])) return null;
  const count = input["questions"].length;
  return count === 1 ? "1 question" : `${count} questions`;
}

function summaryOf(name: string, input: unknown, cwd: string | null): string | null {
  switch (name) {
    case "Bash": {
      const command = field(input, "command");
      const background = isRecord(input) && input["run_in_background"] === true;
      return command === null ? null : background ? `${command} (background)` : command;
    }
    case "Read":
    case "Write":
    case "Edit":
    case "MultiEdit":
      return pathField(input, "file_path", cwd);
    case "NotebookEdit":
      return pathField(input, "notebook_path", cwd);
    case "Grep":
    case "Glob":
      return withScope(field(input, "pattern"), input, cwd);
    case "WebFetch":
      return field(input, "url");
    case "WebSearch":
    case "ToolSearch":
      return field(input, "query");
    case "Task":
    case "Agent":
      return field(input, "description");
    case "Skill":
      return field(input, "skill");
    case "ExitPlanMode":
      return firstLine(field(input, "plan"));
    case "AskUserQuestion":
      return questionCount(input);
    case "TodoWrite":
      return isRecord(input) && Array.isArray(input["todos"])
        ? `${input["todos"].length} items`
        : null;
    default:
      return null;
  }
}

/** `mcp__<server>__<tool_name>` as `Tool Name (server)`; other names unchanged. */
export function toolDisplayName(name: string): string {
  const parts = name.split("__");
  const [prefix, server, ...rest] = parts;
  if (prefix !== "mcp" || server === undefined || server === "" || rest.length === 0) return name;
  const words = rest.join("_").split(/[_-]+/).filter((word) => word !== "");
  if (words.length === 0) return name;
  const tool = words.map((word) => word.charAt(0).toUpperCase() + word.slice(1)).join(" ");
  return `${tool} (${server})`;
}

/**
 * One readable line describing a tool call from its input; empty for a call without
 * input. File paths inside `cwd` show relative to it.
 */
export function toolSummary(name: string, input: unknown, cwd: string | null = null): string {
  return oneLine(summaryOf(name, input, cwd) ?? keyValues(input));
}

export function gateFailureText(reason: GateFailure): string {
  switch (reason.kind) {
    case "api_provider":
      return `Stopped: the account is not an Anthropic subscription (provider ${reason.provider ?? "none"}).`;
    case "initialize":
      return `Stopped: the account check failed (${reason.error}).`;
    case "api_key_source":
      return `Stopped: Claude reported API key billing (${reason.source}).`;
    case "overage":
      return "Stopped: Claude reported usage credits in use.";
    case "unknown":
      return `Stopped by the auth gate: ${JSON.stringify(reason.raw)}`;
  }
}

class Draft {
  lastSeq: number;
  firstSeq: number;
  items: Item[];
  asks: PendingAsk[];
  tools: Map<string, number>;
  openText: Map<string, number[]>;
  liveBlocks: Map<string, number>;
  streamMessage: string | null;
  requests: Map<string, { toolName: string; input: unknown }>;
  interrupted: boolean;
  stopping: StopReason | null;
  turnOpen: boolean;
  turnStartSeq: number | null;
  outbox: Outgoing[];
  cwd: string | null;
  permissionMode: string | null;
  pendingSeed: string | null;
  catalog: Catalog | null;
  terminalCommands: readonly string[];
  agents: Map<string, AgentInfo>;
  /** Scopes not touched by the events being applied. */
  private scopeStates: Map<string, TranscriptState>;
  /** Scopes the events being applied touched. */
  private scopeDrafts = new Map<string, Draft>();

  /** `scope` is the Agent call whose discussion this draft folds; null for the main one. */
  constructor(
    state: TranscriptState,
    private readonly scope: string | null = null,
  ) {
    this.lastSeq = state.lastSeq;
    this.firstSeq = state.firstSeq;
    this.items = [...state.items];
    this.asks = [...state.asks];
    this.tools = new Map(state.tools);
    this.openText = new Map([...state.openText].map(([id, list]) => [id, [...list]]));
    this.liveBlocks = new Map(state.liveBlocks);
    this.streamMessage = state.streamMessage;
    this.requests = new Map(state.requests);
    this.interrupted = state.interrupted;
    this.stopping = state.stopping;
    this.turnOpen = state.turnOpen;
    this.turnStartSeq = state.turnStartSeq;
    this.outbox = state.outbox.map((entry) => ({ ...entry }));
    this.cwd = state.cwd;
    this.permissionMode = state.permissionMode;
    this.pendingSeed = state.pendingSeed;
    this.catalog = state.catalog;
    this.terminalCommands = state.terminalCommands;
    this.agents = new Map(state.agents);
    this.scopeStates = new Map(state.scopes);
  }

  finish(): TranscriptState {
    const scopes = new Map(this.scopeStates);
    for (const [id, draft] of this.scopeDrafts) scopes.set(id, draft.finish());
    return {
      lastSeq: this.lastSeq,
      firstSeq: this.firstSeq,
      items: this.items,
      asks: this.asks,
      tools: this.tools,
      openText: this.openText,
      liveBlocks: this.liveBlocks,
      streamMessage: this.streamMessage,
      requests: this.requests,
      interrupted: this.interrupted,
      stopping: this.stopping,
      turnOpen: this.turnOpen,
      turnStartSeq: this.turnStartSeq,
      outbox: this.outbox,
      cwd: this.cwd,
      permissionMode: this.permissionMode,
      pendingSeed: this.pendingSeed,
      catalog: this.catalog,
      terminalCommands: this.terminalCommands,
      agents: this.agents,
      scopes,
    };
  }

  /** The draft of agent `id`'s discussion, entered at the event being applied. */
  private scoped(id: string): Draft {
    let draft = this.scopeDrafts.get(id);
    if (draft === undefined) {
      draft = new Draft(this.scopeStates.get(id) ?? emptyTranscript, id);
      this.scopeDrafts.set(id, draft);
    }
    draft.enter(this.lastSeq, this.time, this.prelude);
    return draft;
  }

  enter(seq: number, time: number | null, prelude: boolean): void {
    this.lastSeq = seq;
    this.time = time;
    this.prelude = prelude;
  }

  /** The agent of task `taskId`'s Agent call id, if its `task_started` was seen. */
  private agentOfTask(taskId: string | null): string | null {
    if (taskId === null) return null;
    for (const agent of this.agents.values()) {
      if (agent.taskId === taskId) return agent.toolUseId;
    }
    return null;
  }

  /** Records the Agent calls of an `assistant` line, main or subagent. */
  private agentCalls(line: Extract<CliLine, { type: "assistant" }>): void {
    for (const block of line.content) {
      if (block.type !== "tool_use" || !AGENT_TOOLS.has(block.name)) continue;
      if (this.agents.has(block.id)) continue;
      this.agents.set(block.id, {
        toolUseId: block.id,
        taskId: null,
        name: field(block.input, "description") ?? block.name,
        parentToolUseId: line.parentToolUseId,
        state: null,
        lastActive: this.time,
      });
    }
  }

  private touchAgent(id: string): void {
    const agent = this.agents.get(id);
    if (agent === undefined || this.time === null) return;
    if (agent.lastActive !== null && agent.lastActive >= this.time) return;
    this.agents.set(id, { ...agent, lastActive: this.time });
  }

  /** A `task_*` line: the agent's state, its brief on start, a closing line on end. */
  private task(line: Extract<CliLine, { type: "system" }>, key: string): void {
    const task = line.task;
    if (task === null) return;
    if (line.subtype === "task_started") {
      if (task.taskType !== LOCAL_AGENT || task.toolUseId === null) return;
      const known = this.agents.get(task.toolUseId);
      this.agents.set(task.toolUseId, {
        toolUseId: task.toolUseId,
        taskId: task.taskId,
        name: task.description ?? known?.name ?? task.taskId,
        parentToolUseId: known?.parentToolUseId ?? null,
        state: "running",
        lastActive: this.time ?? known?.lastActive ?? null,
      });
      if (task.prompt !== null) this.scoped(task.toolUseId).brief(task.prompt, key);
      return;
    }
    const id = task.toolUseId !== null && this.agents.has(task.toolUseId)
      ? task.toolUseId
      : this.agentOfTask(task.taskId);
    if (id === null) return;
    this.touchAgent(id);
    const agent = this.agents.get(id);
    const state = agentStateOf(task.status);
    if (agent === undefined || state === null) return;
    this.agents.set(id, { ...agent, state });
    if (line.subtype === "task_notification") this.scoped(id).ended(state, key);
  }

  /** Opens the discussion with the agent's brief; a later brief replaces it. */
  brief(text: string, key: string): void {
    const first = this.items[0];
    if (first?.kind === "brief") {
      this.items[0] = { ...first, text };
      return;
    }
    if (this.items.length === 0) this.add({ kind: "brief", key, text });
  }

  /** The agent's task ended. */
  ended(state: AgentState, key: string): void {
    this.turnOpen = false;
    this.endStreaming();
    this.items = this.items.map((item) =>
      item.kind === "tool" && item.status === "running" ? { ...item, status: "unfinished" } : item,
    );
    const text: Record<AgentState, string> = {
      running: "Agent running",
      completed: "Agent finished",
      stopped: "Agent stopped",
      failed: "Agent failed",
    };
    this.add({
      kind: "notice",
      key,
      tone: state === "failed" ? "error" : "info",
      text: text[state],
      detail: null,
    });
  }

  /** A relay typed in this agent's view. */
  private relayed(
    text: string,
    attachments: readonly string[],
    clientId: string | null,
    key: string,
  ): void {
    this.add({
      kind: "user",
      key,
      text,
      delivery: "on_host",
      clientId,
      relayed: true,
      ...attached(attachments),
    });
  }

  /** The verdict on the relay at `key`, in this agent's view. */
  private relayChecked(key: string, failure: RelayFailure | null, at: string): void {
    const index = this.items.findIndex((item) => item.key === key);
    const item = this.items[index];
    if (item?.kind === "user") this.items[index] = { ...item, delivery: "received" };
    if (failure !== null) this.add({ kind: "relay_failed", key: at, reason: failure });
  }

  /** Hides a faithful relay here: its wrapped message, the SendMessage call and the acknowledgement. */
  private hideRelay(key: string, sendMessage: string): void {
    const start = this.items.findIndex((item) => item.key === key);
    if (start < 0) return;
    let ackHidden = false;
    this.items = this.items.map((item, index) => {
      if (index === start) return { ...item, hidden: true };
      if (index < start) return item;
      if (item.kind === "tool" && item.toolUseId === sendMessage) return { ...item, hidden: true };
      if (
        !ackHidden &&
        item.kind === "claude" &&
        item.hidden !== true &&
        item.text.trim() === RELAY_ACK
      ) {
        ackHidden = true;
        return { ...item, hidden: true };
      }
      return item;
    });
  }

  /** Time of the event being applied. */
  private time: number | null = null;
  /** The event being applied is in a prelude. Preludes hold host events only, none indexing an item. */
  private prelude = false;

  private add(body: ItemBody): void {
    if (this.prelude) return;
    this.items.push({ ...body, time: this.time, seq: this.lastSeq });
  }

  private openTurn(): void {
    if (this.turnOpen) return;
    this.turnOpen = true;
    this.turnStartSeq = this.lastSeq;
  }

  /** Every discussion's items: this one's, then each agent's. */
  private allItems(): (readonly Item[])[] {
    const scopes = new Map<string, readonly Item[]>();
    for (const [id, state] of this.scopeStates) scopes.set(id, state.items);
    for (const [id, draft] of this.scopeDrafts) scopes.set(id, draft.items);
    return [this.items, ...scopes.values()];
  }

  /** Closes a turn with the files its edit tools changed, when any. */
  private turnChanges(key: string): void {
    const start = this.turnStartSeq;
    if (start === null) return;
    const files = new Set<string>();
    let added = 0;
    let removed = 0;
    for (const items of this.allItems()) {
      for (const item of items) {
        if (item.kind !== "tool" || item.seq < start || item.status !== "ok") continue;
        const path = field(item.input, "file_path");
        if (path === null) continue;
        const diff = toolDiff({ ...item, path });
        if (diff === null) continue;
        files.add(path);
        added += diff.added;
        removed += diff.removed;
      }
    }
    if (files.size === 0) return;
    this.add({ kind: "changes", key, files: files.size, added, removed });
  }

  /** Hides the item just added, if one was. */
  private hideLast(): void {
    if (this.prelude) return;
    const last = this.items.at(-1);
    if (last !== undefined) this.items[this.items.length - 1] = { ...last, hidden: true };
  }

  apply(event: WireEvent): void {
    if (event.seq <= this.lastSeq) return;
    this.lastSeq = event.seq;
    this.time = parseTime(event.time);
    this.prelude = event.seq < this.firstSeq;
    const key = String(event.seq);
    if (event.kind === "host") this.host(event.payload, key);
    else this.line(parseCliLine(event.payload), key);
  }

  private host(event: HostEvent, key: string): void {
    switch (event.event) {
      case "started":
        this.cwd = event.cwd;
        this.asks = [];
        this.outbox = [];
        this.turnOpen = false;
        this.endStreaming();
        return;
      case "user_sent":
        this.outbox.push({ index: this.items.length, armed: !this.turnOpen, settled: false });
        this.add({
          kind: "user",
          key,
          text: event.text,
          delivery: "on_host",
          clientId: event.client_id,
          ...attached(event.attachments),
        });
        return;
      case "ask_queued": {
        const request = this.requests.get(event.request_id);
        this.asks.push({
          requestId: event.request_id,
          toolName: event.tool_name,
          toolUseId: event.tool_use_id,
          input: request?.input,
          danger: event.danger,
          acceptEdits: event.accept_edits,
          folder: event.folder,
          file: event.file,
          plan: askPlan(event.tool_name, event.plan, request?.input),
          agentId: event.agent_id,
        });
        return;
      }
      case "auto_allowed": {
        this.requests.delete(event.request_id);
        const agent = this.agentOfTask(event.agent_id);
        const target = agent === null ? this : this.scoped(agent);
        target.add({
          kind: "notice",
          key,
          tone: "info",
          text: `Auto-allowed: ${event.tool_name} ${relativePath(event.file.path, this.cwd)}`,
          detail: null,
        });
        return;
      }
      case "relay_sent":
        this.outbox.push({ index: this.items.length, armed: !this.turnOpen, settled: false });
        this.add({ kind: "user", key, text: event.wrapped, delivery: "on_host", clientId: null });
        this.scoped(event.agent_tool_use_id).relayed(
          event.text,
          event.attachments,
          event.client_id,
          key,
        );
        return;
      case "relay_checked": {
        const relayKey = String(event.relay_seq);
        const outcome = event.outcome;
        if (outcome.kind === "faithful") this.hideRelay(relayKey, outcome.send_message);
        this.scoped(event.agent_tool_use_id).relayChecked(
          relayKey,
          outcome.kind === "failed" ? outcome.reason : null,
          key,
        );
        return;
      }
      case "ask_resolved":
        this.asks = this.asks.filter((ask) => ask.requestId !== event.request_id);
        this.requests.delete(event.request_id);
        return;
      case "claude_attached":
        this.attachToReply(event.attachments);
        return;
      case "permission_mode":
        this.permissionMode = event.mode;
        return;
      case "auth_gate_failed":
        this.add({
          kind: "notice",
          key,
          tone: "error",
          text: gateFailureText(event.reason),
          detail: null,
        });
        return;
      case "stopping":
        this.stopping = event.reason;
        return;
      case "child_exited": {
        this.stopping = null;
        this.asks = [];
        this.requests.clear();
        this.outbox = [];
        this.turnOpen = false;
        this.endStreaming();
        this.items = this.items.map((item) =>
          item.kind === "tool" && item.status === "running"
            ? { ...item, status: "unfinished" }
            : item,
        );
        for (const [id, agent] of this.agents) {
          if (agent.state !== "running") continue;
          this.agents.set(id, { ...agent, state: "stopped" });
          this.scoped(id).ended("stopped", key);
        }
        const code = event.code === null ? "no exit code" : `code ${event.code}`;
        this.add({
          kind: "notice",
          key,
          tone: event.reason !== null || event.code === 0 ? "info" : "error",
          text: event.reason === null ? `Claude process ended (${code})` : STOPPED[event.reason],
          detail: event.stderr_tail.length > 0 ? event.stderr_tail.join("\n") : null,
        });
        return;
      }
      case "restarted":
        this.pendingSeed = event.seed;
        this.add({
          kind: "restart",
          key,
          mode: event.mode,
          exchanges: event.exchanges,
          seed: event.seed,
        });
        return;
      case "catalog":
        this.catalog = { models: event.models, commands: event.commands };
        return;
      case "unknown":
        return;
    }
  }

  private line(line: CliLine, key: string): void {
    if (line.type === "assistant" && this.scope === null) this.agentCalls(line);
    // An agent's lines go to its own discussion only.
    if (
      this.scope === null &&
      (line.type === "user" || line.type === "assistant" || line.type === "stream_event") &&
      line.parentToolUseId !== null
    ) {
      this.touchAgent(line.parentToolUseId);
      this.scoped(line.parentToolUseId).line(line, key);
      return;
    }
    switch (line.type) {
      case "user":
        if (line.parentToolUseId === this.scope) {
          this.user(
            line.content,
            line.isMeta,
            line.isSynthetic,
            line.origin,
            line.toolUseResult,
            key,
          );
        }
        return;
      case "assistant":
        if (line.parentToolUseId !== this.scope) return;
        this.answered(false);
        this.openTurn();
        this.assistant(line.messageId, line.content, key);
        return;
      case "system":
        if (line.task !== null) {
          this.task(line, key);
          return;
        }
        if (line.subtype === "init") this.openTurn();
        if (line.subtype === "init" && line.terminalCommands !== null) {
          this.terminalCommands = line.terminalCommands;
        }
        // History recorded before `permission_mode` host events only has these.
        if ((line.subtype === "init" || line.subtype === "status") && line.permissionMode !== null) {
          this.permissionMode = line.permissionMode;
        }
        return;
      case "stream_event":
        if (line.parentToolUseId !== this.scope) return;
        this.openTurn();
        switch (line.event.type) {
          case "message_start":
            this.streamMessage = line.event.messageId;
            return;
          case "message_stop":
            this.streamMessage = null;
            return;
          case "text_delta":
            this.delta(line.event.index, line.event.text, key);
            return;
          case "other":
            return;
        }
        return;
      case "result":
        if (line.isError && !(line.subtype === "error_during_execution" && this.interrupted)) {
          this.add({
            kind: "notice",
            key,
            tone: "error",
            text: `Turn ended with an error (${line.subtype})`,
            detail: null,
          });
        }
        this.interrupted = false;
        this.answered(true);
        if (this.scope === null) this.turnChanges(`${key}:changes`);
        this.turnOpen = false;
        this.endStreaming();
        return;
      case "can_use_tool":
        this.requests.set(line.requestId, { toolName: line.toolName, input: line.input });
        return;
      case "raw":
        return;
    }
  }

  private user(
    content: string | ContentBlock[],
    isMeta: boolean,
    isSynthetic: boolean,
    origin: string | null,
    toolUseResult: unknown,
    key: string,
  ): void {
    const blocks: ContentBlock[] =
      typeof content === "string" ? [{ type: "text", text: content }] : content;
    // The line's structured result belongs to its tool call only when it answers a single one.
    const single = blocks.filter((block) => block.type === "tool_result").length === 1;
    const texts: string[] = [];
    for (const block of blocks) {
      if (block.type === "text") texts.push(block.text);
      else if (block.type === "tool_result") {
        this.toolResult(block, single ? toolUseResult : undefined);
      }
    }
    if (texts.length === 0 || isMeta) return;
    const text = texts.join("\n");
    if (origin === TASK_NOTIFICATION) {
      this.taskEnd(text, key);
      return;
    }
    if (isSynthetic) {
      this.synthetic(text, key);
      return;
    }
    if (text.startsWith(INTERRUPT_PREFIX)) {
      this.interrupted = true;
      // A requested stop interrupts the turn itself; its exit line says why.
      if (this.stopping !== null) return;
      this.add({
        kind: "notice",
        key,
        tone: "info",
        text: text.replace(/^\[|\]$/g, ""),
        detail: null,
      });
      return;
    }
    this.echo(text, key);
  }

  /**
   * The CLI echoed a user message: the oldest matching one sent is received. The
   * first echo after a continue starts with its seed, which the divider row holds;
   * what follows it is the typed message, or the closing line when none was typed.
   */
  private echo(echoed: string, key: string): void {
    this.openTurn();
    const seed = this.pendingSeed;
    this.pendingSeed = null;
    const prefix = seed === null ? null : `${seed}\n\n`;
    const seeded = prefix !== null && echoed.startsWith(prefix);
    const text = seeded ? echoed.slice(prefix.length) : echoed;
    const matches = (entry: Outgoing, delivery: Delivery): boolean => {
      const item = this.items[entry.index];
      return item?.kind === "user" && item.delivery === delivery && composedText(item) === text;
    };
    const onHost = this.outbox.findIndex((entry) => matches(entry, "on_host"));
    const position =
      onHost >= 0 ? onHost : this.outbox.findIndex((entry) => matches(entry, "handled"));
    const entry = this.outbox[position];
    const item = entry === undefined ? undefined : this.items[entry.index];
    if (entry === undefined || item?.kind !== "user") {
      if (seeded) return;
      // A relayed message reaching the agent is already shown.
      const relayed = (candidate: Item) =>
        candidate.kind === "user" && candidate.relayed === true && composedText(candidate) === text;
      if (this.scope !== null && this.items.some(relayed)) return;
      // History recorded before `user_sent` existed only has the echo.
      this.add({ kind: "user", key, text, delivery: "received", clientId: null });
      return;
    }
    this.items[entry.index] = { ...item, delivery: "received" };
    this.outbox.splice(position, 1);
  }

  /**
   * An `assistant` or `result` line answers the armed messages still unechoed:
   * the CLI handled them locally. A `result` also arms the rest, sent during the
   * turn it ends, and drops messages handled before it.
   */
  private answered(isResult: boolean): void {
    for (const entry of this.outbox) {
      const item = this.items[entry.index];
      if (entry.armed && item?.kind === "user" && item.delivery === "on_host") {
        this.items[entry.index] = { ...item, delivery: "handled" };
      }
    }
    if (!isResult) return;
    this.outbox = this.outbox.filter((entry) => !entry.settled);
    for (const entry of this.outbox) {
      const item = this.items[entry.index];
      if (item?.kind === "user" && item.delivery === "handled") entry.settled = true;
      entry.armed = true;
    }
  }

  /** A background task's end, which the CLI writes as a user message. */
  private taskEnd(text: string, key: string): void {
    const parsed = parseTaskNotification(text);
    if (parsed === null) {
      this.add({ kind: "notice", key, tone: "info", text: "System message", detail: text });
      return;
    }
    this.add({ kind: "task_end", key, ...parsed, raw: text });
  }

  /**
   * Text the CLI wrote in the user's place. It carries no link to its call on
   * stdout, so a skill's content folds into the latest Skill call.
   */
  private synthetic(text: string, key: string): void {
    // An agent's first synthetic message, before it did anything, is its brief.
    if (this.scope !== null && this.items.every((item) => item.kind === "brief")) {
      this.brief(text, key);
      return;
    }
    for (let index = this.items.length - 1; index >= 0; index -= 1) {
      const item = this.items[index];
      if (item?.kind !== "tool" || item.name !== SKILL_TOOL) continue;
      if (item.attached !== null) break;
      this.items[index] = { ...item, attached: text };
      return;
    }
    this.add({ kind: "notice", key, tone: "info", text: "System message", detail: text });
  }

  /** Puts `paths` on the last Claude text since the last user message; dropped when there is none. */
  private attachToReply(paths: readonly string[]): void {
    for (let index = this.items.length - 1; index >= 0; index -= 1) {
      const item = this.items[index];
      if (item?.kind === "user") return;
      if (item?.kind !== "claude") continue;
      this.items[index] = { ...item, ...attached(paths) };
      return;
    }
  }

  private toolResult(
    block: Extract<ContentBlock, { type: "tool_result" }>,
    result: unknown,
  ): void {
    const index = this.tools.get(block.toolUseId);
    if (index === undefined) return;
    const item = this.items[index];
    if (item?.kind !== "tool") return;
    this.items[index] = {
      ...item,
      status: block.isError ? "error" : "ok",
      output: block.content,
      result,
      endTime: this.time,
    };
  }

  private assistant(messageId: string | null, content: ContentBlock[], key: string): void {
    const id = messageId ?? UNKNOWN_MESSAGE;
    content.forEach((block, position) => {
      const blockKey = `${key}:${position}`;
      switch (block.type) {
        case "text": {
          const open = this.openText.get(id);
          const index = open?.shift();
          const item = index === undefined ? undefined : this.items[index];
          if (index !== undefined && item?.kind === "claude") {
            this.items[index] = { ...item, messageId, text: block.text, streaming: false };
          } else {
            this.add({ kind: "claude", key: blockKey, messageId, text: block.text, streaming: false });
          }
          return;
        }
        case "thinking":
          this.add({ kind: "thinking", key: blockKey, text: block.thinking });
          return;
        case "tool_use":
          if (AGENT_TOOLS.has(block.name)) {
            this.add({
              kind: "agent",
              key: blockKey,
              toolUseId: block.id,
              name: field(block.input, "description") ?? block.name,
            });
            return;
          }
          this.tools.set(block.id, this.items.length);
          this.add({
            kind: "tool",
            key: blockKey,
            messageId,
            toolUseId: block.id,
            name: block.name,
            summary: toolSummary(block.name, block.input, this.cwd),
            input: block.input,
            status: "running",
            output: null,
            result: undefined,
            endTime: null,
            attached: null,
          });
          if (block.name === SET_ATTACHMENTS_TOOL) this.hideLast();
          return;
        case "tool_result":
        case "other":
          return;
      }
    });
  }

  private delta(blockIndex: number, text: string, key: string): void {
    const id = this.streamMessage ?? UNKNOWN_MESSAGE;
    const liveKey = `${id}\u0000${blockIndex}`;
    const index = this.liveBlocks.get(liveKey);
    const item = index === undefined ? undefined : this.items[index];
    if (index !== undefined && item?.kind === "claude") {
      if (item.streaming) this.items[index] = { ...item, text: item.text + text };
      return;
    }
    const created = this.items.length;
    this.add({
      kind: "claude",
      key: `${key}:live`,
      messageId: this.streamMessage,
      text,
      streaming: true,
    });
    this.liveBlocks.set(liveKey, created);
    const open = this.openText.get(id);
    if (open === undefined) this.openText.set(id, [created]);
    else open.push(created);
  }

  private endStreaming(): void {
    for (const indexes of this.openText.values()) {
      for (const index of indexes) {
        const item = this.items[index];
        if (item?.kind === "claude" && item.streaming) {
          this.items[index] = { ...item, streaming: false };
        }
      }
    }
    this.openText.clear();
    this.liveBlocks.clear();
    this.streamMessage = null;
  }
}

/** Folds events into the transcript; events at or below `lastSeq` are skipped. */
export function applyEvents(state: TranscriptState, events: readonly WireEvent[]): TranscriptState {
  const fresh = events.filter((event) => event.seq > state.lastSeq);
  if (fresh.length === 0) return state;
  const draft = new Draft(state);
  for (const event of fresh) draft.apply(event);
  return draft.finish();
}
