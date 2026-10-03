// Mirror of `src/http/wire.rs`: the websocket protocol at `api/ws` and the bodies of
// `api/sessions` and `api/catalog`, and the part of `api/usage` the app reads. Envelopes are validated strictly; CLI lines and host events of
// unknown shape are kept as raw values instead of failing.

export type GateFailure =
  | { kind: "api_provider"; provider: string | null }
  | { kind: "initialize"; error: string }
  | { kind: "api_key_source"; source: string }
  | { kind: "overage" }
  | { kind: "unknown"; raw: unknown };

export type SessionState =
  | { kind: "running" }
  | { kind: "stopped" }
  | { kind: "exited"; code: number | null }
  | { kind: "gate_failed"; reason: GateFailure };

export interface SessionView {
  id: string;
  cwd: string;
  created_at: string;
  updated_at: string;
  state: SessionState;
  last_seq: number;
  live: boolean;
  busy: boolean;
  /** Asks of the running child awaiting an answer; 0 when not live. */
  asks: number;
  /** Stored mode the next launch passes; null when never recorded. */
  permission_mode: string | null;
  /** Null until a main request reported the TTL of its cache write. */
  cache: CacheView | null;
  context: ContextView | null;
  /** Main-conversation requests per finished turn; null before the first turn. */
  calls_per_message: number | null;
  /** What a restart would keep, measured after each turn; null before. */
  restart: RestartMeasure | null;
  /** Display name; null shows the folder name. */
  name: string | null;
  /** Null while the discussion is not archived. */
  archived_at: string | null;
  /** Its folder sits directly in the scratch root. */
  scratch: boolean;
  /** Stored model and effort every launch passes; null lets the CLI pick. */
  model: string | null;
  effort: string | null;
  /** When a model or effort change made the cache count cold; null once a main request or a restart followed. */
  settings_changed_at: string | null;
  /** What the CLI last reported applying, e.g. `claude-opus-5-5[1m]` and `medium`; null while unknown. */
  applied_model: string | null;
  applied_effort: string | null;
  /** Subagents of the running child, oldest first; empty when not live. */
  agents: AgentView[];
  /** Background tasks of the running child, oldest first; empty when not live. */
  background: BackgroundView[];
  /** Null once the user saw the discussion. */
  unread: Unread | null;
  /** Messages kept until sent, in queue order. */
  held: HeldMessage[];
}

/** `held` waits for the user; `turn_end` is sent when the main agent's turn ends normally. */
export type HeldMode = "held" | "turn_end";

export interface HeldMessage {
  id: string;
  text: string;
  /** Absolute paths of the attached files. */
  attachments: string[];
  mode: HeldMode;
  created_at: string;
}

/** A background task of the running child, such as a command run in the background. */
export interface BackgroundView {
  task_id: string;
  /** Such as `local_bash`. */
  task_type: string | null;
  description: string | null;
  started_at: string;
}

export type Unread = "finished" | "stopped";

export type AgentState = "running" | "completed" | "stopped" | "failed";

/** A subagent of the running child. */
export interface AgentView {
  task_id: string;
  /** The Agent call; the agent's lines carry it as `parent_tool_use_id`. */
  tool_use_id: string;
  /** The Agent call's description. */
  name: string;
  subagent_type: string | null;
  /** The Agent call of the agent that started it; null for the main agent. */
  parent_tool_use_id: string | null;
  state: AgentState;
  /** Latest progress, then the final summary. */
  activity: string | null;
  /** From the agent's own last request. */
  cache: CacheView | null;
  /** That request's prompt: what a relay to a cold agent caches again. */
  prompt_tokens: number;
  started_at: string;
}

/** A model the CLI accepts; `effort_levels` is empty for one without effort. */
export interface ModelChoice {
  value: string;
  /** The model id `value` stands for, e.g. `claude-opus-5-5` for `default`. */
  resolved_model: string | null;
  display_name: string;
  description: string | null;
  effort_levels: string[];
}

/** A slash command or skill; `argument_hint` is empty for one without arguments. */
export interface CommandInfo {
  name: string;
  description: string;
  argument_hint: string;
}

/** `GET api/catalog`: what all discussions share; a field is null until a Claude start reported it. */
export interface SharedCatalog {
  models: ModelChoice[] | null;
  commands: CommandInfo[] | null;
  /** Commands that only work in the terminal. */
  terminal_commands: string[] | null;
  /** What the CLI applies with no model and no effort picked. */
  defaults: { model: string | null; effort: string | null } | null;
}

/** Prompt cache of the main conversation: cold `ttl_secs` after `last_request_at`. */
export interface CacheView {
  last_request_at: string;
  ttl_secs: number;
}

export interface ContextView {
  used_tokens: number;
  max_tokens: number;
  percentage: number;
  source: "cli" | "estimate";
  /** From the latest CLI reply; null while unknown. */
  autocompact_threshold: number | null;
  /** From the latest CLI reply; empty while unknown. */
  categories: ContextCategory[];
}

/** See `notes/restart-advice.md`. */
export interface RestartMeasure {
  /** Prompt tokens when the oldest exchange a seed carries started. */
  window_start_tokens: number;
  seed_tokens: number;
  /** The files the seeded exchanges touched, which a fresh session reads again. */
  reread_tokens: number;
  /** Mean cost of the seeded exchanges' main calls besides cache reads, in base input tokens. */
  other_cost_per_call: number;
}

export interface ContextCategory {
  name: string;
  tokens: number;
  /** `used`, `deferred`, `buffer` or `free`, as the CLI reports it. */
  kind: string;
}

export type Confidence = "low" | "medium" | "high";

/** One rate-limit window of `GET api/usage`; percents are 0 to 100. */
export interface WindowReport {
  used: number;
  resets_at: string;
  projected: number;
  confidence: Confidence;
  /** 1 spends exactly one budget per window. */
  rate: number;
  /** 5h only, while projected over 100. */
  work_left_hours: number | null;
  /** 7d only: when usage reaches 100, if before the reset. */
  deadline: string | null;
  reading_at: string;
}

/** The parts of `GET api/usage` the top bar reads. */
export interface UsageSnapshot {
  generated_at: string;
  newest_reading: string | null;
  five_hour: WindowReport | null;
  seven_day: WindowReport | null;
}

export type Answer =
  | {
      kind: "allow";
      updated_input?: unknown;
      note?: string;
      /** Also switches to Accept edits; only on an ask offering it. */
      accept_edits?: true;
      /** Also grants this folder for the session: the offered one or a parent. */
      folder?: string;
      /** Also allows the later asks on the ask's file until the Claude process stops. */
      always_for_file?: true;
    }
  | { kind: "deny"; message: string }
  | { kind: "questions"; answers: Record<string, string> };

export type ClientMessage =
  /** With `exchanges` and `after_seq` 0: only the last exchanges, after `window`. */
  | { type: "subscribe"; session: string; after_seq: number; exchanges?: number }
  /** Answered by `page`. */
  | { type: "history"; session: string; before_seq: number; exchanges: number }
  | { type: "unsubscribe"; session: string }
  /** The discussion on screen and whether the user is there; not acked. */
  | { type: "presence"; session: string | null; active: boolean }
  | {
      type: "send";
      session: string;
      text: string;
      /** Paths of the attached files. */
      attachments?: string[];
      rewarm?: true;
      client_id: string;
    }
  | { type: "interrupt"; session: string; client_id: string }
  /** Queues a message; a `turn_end` one with no turn running is sent at once. */
  | {
      type: "hold";
      session: string;
      text: string;
      attachments?: string[];
      mode: HeldMode;
      client_id: string;
    }
  /** An absent field keeps its value. */
  | {
      type: "edit_held";
      session: string;
      held_id: string;
      text?: string;
      attachments?: string[];
      mode?: HeldMode;
      client_id: string;
    }
  | { type: "drop_held"; session: string; held_id: string; client_id: string }
  /** Takes the message out of the queue and sends it like `send`. */
  | { type: "send_held"; session: string; held_id: string; rewarm?: true; client_id: string }
  /** A message for subagent `task_id`, relayed by the main agent. */
  | {
      type: "relay";
      session: string;
      task_id: string;
      text: string;
      attachments?: string[];
      rewarm?: true;
      client_id: string;
    }
  | { type: "stop_task"; session: string; task_id: string; client_id: string }
  /** Stops every running agent. */
  | { type: "stop_agents"; session: string; client_id: string }
  | {
      type: "answer";
      session: string;
      request_id: string;
      answer: Answer;
      rewarm?: true;
      client_id: string;
    }
  | { type: "set_permission_mode"; session: string; mode: string; client_id: string }
  | {
      type: "set_settings";
      session: string;
      /** Absent keeps the setting; null lets the CLI pick. */
      model?: string | null;
      effort?: string | null;
      client_id: string;
    }
  | {
      type: "restart";
      session: string;
      mode: RestartMode;
      text?: string;
      attachments?: string[];
      client_id: string;
    }
  /** `reason` defaults to `kill`, a stop from Home. */
  | { type: "kill"; session: string; reason?: StopReason; client_id: string }
  | { type: "archive"; session: string; client_id: string }
  | { type: "restore"; session: string; client_id: string }
  /** Null or blank clears the name. */
  | { type: "rename"; session: string; name: string | null; client_id: string };

export type RestartMode = "continue" | "blank" | "clear";

/** Why a live child was stopped: from Home, to drop the granted folders, or the server stopping. */
export type StopReason = "kill" | "revoke" | "shutdown";

function stopReason(raw: unknown): StopReason | null {
  return raw === "kill" || raw === "revoke" || raw === "shutdown" ? raw : null;
}

export type Command = Extract<ClientMessage, { client_id: string }>;

/** Ack error of a `send` or an `answer` refused on a cold cache without `rewarm`. */
export const CACHE_COLD = "cache_cold";
/** Ack error of an `archive` refused while the child runs. */
export const SESSION_LIVE = "session_live";
/** Ack error of an action on a held message no longer queued. */
export const HELD_GONE = "held_gone";

/** Why an ask deserves a second look; `reasons` has one line per matched rule. */
export interface Danger {
  dangerous: boolean;
  reasons: string[];
}

/** A `Read` rule for Read, Glob, Grep and Bash; an added working folder for Write. */
export type GrantKind = "read" | "directory";

/** The folder an ask suggests granting for the session. */
export interface FolderOffer {
  kind: GrantKind;
  /** `C:\a\b` on a drive, `/a/b` otherwise. */
  folder: string;
  /** The folder and its parents too broad to grant, in the same form. */
  broad: string[];
}

/** Reading, or any edit tool. */
export type FileKind = "read" | "edit";

/** The file of a Read or edit-tool ask, which an allow can grant for good. */
export interface FileOffer {
  kind: FileKind;
  /** Absolute: `C:\a\b.md` on a drive, `/a/b.md` otherwise. */
  path: string;
}

/** The plan an `ExitPlanMode` ask submits. */
export interface PlanReview {
  /** The plan file, when known. */
  path: string | null;
  text: string;
  /** The text the previous review of `path` showed, since its last approval. */
  previous: string | null;
}

/** `unknown` stands for a reason this client does not know. */
export type RelayFailure =
  "altered" | "other_recipient" | "not_forwarded" | "no_acknowledgement" | "ended" | "unknown";

/** `send_message` is the tool use id of the SendMessage call that forwarded the text. */
export type RelayOutcome =
  { kind: "faithful"; send_message: string } | { kind: "failed"; reason: RelayFailure };

const RELAY_FAILURES: readonly RelayFailure[] = [
  "altered",
  "other_recipient",
  "not_forwarded",
  "no_acknowledgement",
  "ended",
];

function parseRelayOutcome(raw: unknown): RelayOutcome | null {
  if (!isRecord(raw)) return null;
  if (raw["kind"] === "faithful") {
    const sendMessage = stringOrNull(raw["send_message"]);
    return sendMessage === null ? null : { kind: "faithful", send_message: sendMessage };
  }
  if (raw["kind"] !== "failed") return null;
  const reason = RELAY_FAILURES.find((known) => known === raw["reason"]) ?? "unknown";
  return { kind: "failed", reason };
}

export type HostEvent =
  | { event: "started"; cwd: string }
  | { event: "user_sent"; text: string; attachments: string[]; client_id: string | null }
  | {
      event: "ask_queued";
      request_id: string;
      tool_name: string;
      tool_use_id: string;
      danger: Danger;
      /** An edit ask offering a switch to Accept edits. */
      accept_edits: boolean;
      folder: FolderOffer | null;
      file: FileOffer | null;
      plan: PlanReview | null;
      /** Task id of the subagent asking; null for the main agent. */
      agent_id: string | null;
      /** When the server denies it if still unanswered (RFC 3339); subagent asks only. */
      deny_at: string | null;
    }
  /** An ask of a subagent whose earlier ask timed out, denied by the server without queueing it. */
  | {
      event: "auto_denied";
      request_id: string;
      tool_name: string;
      tool_use_id: string;
      agent_id: string;
    }
  /** An ask on a file granted for good, allowed by the server without queueing it. */
  | {
      event: "auto_allowed";
      request_id: string;
      tool_name: string;
      tool_use_id: string;
      file: FileOffer;
      agent_id: string | null;
    }
  /** A message for a subagent, written to the main agent as `wrapped`. */
  | {
      event: "relay_sent";
      text: string;
      attachments: string[];
      client_id: string | null;
      task_id: string;
      /** The Agent call that started the agent. */
      agent_tool_use_id: string;
      name: string;
      wrapped: string;
    }
  /** How the main agent handled the relay whose `relay_sent` has seq `relay_seq`. */
  | {
      event: "relay_checked";
      relay_seq: number;
      task_id: string;
      agent_tool_use_id: string;
      outcome: RelayOutcome;
    }
  /**
   * `timed_out`: denied by the server at its `deny_at`, not answered. `note`: the
   * trimmed note of an allow, sent next as a user message; null when none.
   */
  | {
      event: "ask_resolved";
      request_id: string;
      timed_out: boolean;
      allowed: boolean;
      note: string | null;
    }
  /** The files Claude attached to its reply; follows the `result` line ending the turn. */
  | {
      event: "claude_attached";
      attachments: string[];
      /** Content hash of each path at the turn's end; a file that could not be read has none. */
      hashes: Record<string, string>;
    }
  | { event: "permission_mode"; mode: string }
  | { event: "auth_gate_failed"; reason: GateFailure }
  /** A stop for `reason` began; a CLI interrupt that follows comes from it. */
  | { event: "stopping"; reason: StopReason }
  | {
      event: "child_exited";
      code: number | null;
      stderr_tail: string[];
      /** Why a client stopped the child; null for any other exit. */
      reason: StopReason | null;
    }
  | {
      event: "restarted";
      mode: RestartMode;
      exchanges: number;
      total_exchanges: number;
      /** The seed the first message of a continue starts with; null for blank. */
      seed: string | null;
    }
  | { event: "catalog"; models: ModelChoice[]; commands: CommandInfo[] }
  | { event: "unknown"; raw: unknown };

export type WireEvent =
  | { session: string; seq: number; time: string; kind: "line"; payload: unknown }
  | { session: string; seq: number; time: string; kind: "host"; payload: HostEvent };

export type ServerMessage =
  | { type: "event"; event: WireEvent }
  | { type: "ack"; client_id: string; ok: boolean; error: string | null }
  /** Every stored event of `session` up to `seq` was sent. */
  | { type: "synced"; session: string; seq: number }
  /** Opens a windowed `subscribe`: its prelude events, then the events from `first_seq`. */
  | { type: "window"; session: string; first_seq: number; start_reached: boolean }
  /** Answers `history`: the prelude, then the events from `first_seq` on, in seq order. */
  | {
      type: "page";
      session: string;
      first_seq: number;
      start_reached: boolean;
      events: WireEvent[];
    }
  | { type: "sessions_changed" }
  | { type: "usage_changed" }
  | { type: "catalog_changed" }
  | { type: "error"; message: string; session: string | null };

export function isRecord(raw: unknown): raw is Record<string, unknown> {
  return typeof raw === "object" && raw !== null && !Array.isArray(raw);
}

function fail(label: string, what: string, raw: unknown): never {
  throw new Error(`${label}: ${what}: ${JSON.stringify(raw)}`);
}

function record(raw: unknown, label: string): Record<string, unknown> {
  if (!isRecord(raw)) fail(label, "expected an object", raw);
  return raw;
}

function str(raw: Record<string, unknown>, key: string, label: string): string {
  const value = raw[key];
  if (typeof value !== "string") fail(`${label}.${key}`, "expected a string", value);
  return value;
}

function optStr(raw: Record<string, unknown>, key: string, label: string): string | null {
  const value = raw[key];
  if (value === undefined || value === null) return null;
  if (typeof value !== "string") fail(`${label}.${key}`, "expected a string", value);
  return value;
}

function num(raw: Record<string, unknown>, key: string, label: string): number {
  const value = raw[key];
  if (typeof value !== "number" || !Number.isFinite(value)) {
    fail(`${label}.${key}`, "expected a number", value);
  }
  return value;
}

function optNum(raw: Record<string, unknown>, key: string, label: string): number | null {
  const value = raw[key];
  if (value === undefined || value === null) return null;
  if (typeof value !== "number" || !Number.isFinite(value)) {
    fail(`${label}.${key}`, "expected a number", value);
  }
  return value;
}

function bool(raw: Record<string, unknown>, key: string, label: string): boolean {
  const value = raw[key];
  if (typeof value !== "boolean") fail(`${label}.${key}`, "expected a boolean", value);
  return value;
}

export function parseGateFailure(raw: unknown): GateFailure {
  if (!isRecord(raw)) return { kind: "unknown", raw };
  const kind = raw["kind"];
  const text = (key: string): string | null => {
    const value = raw[key];
    return typeof value === "string" ? value : null;
  };
  switch (kind) {
    case "api_provider":
      return { kind, provider: text("provider") };
    case "initialize":
      return { kind, error: text("error") ?? "" };
    case "api_key_source":
      return { kind, source: text("source") ?? "" };
    case "overage":
      return { kind };
    default:
      return { kind: "unknown", raw };
  }
}

export function parseSessionState(raw: unknown, label: string): SessionState {
  const value = record(raw, label);
  const kind = value["kind"];
  switch (kind) {
    case "running":
    case "stopped":
      return { kind };
    case "exited":
      return { kind, code: optNum(value, "code", label) };
    case "gate_failed":
      return { kind, reason: parseGateFailure(value["reason"]) };
    default:
      return fail(`${label}.kind`, "unknown session state", kind);
  }
}

export function parseSessionView(raw: unknown, label: string): SessionView {
  const value = record(raw, label);
  return {
    id: str(value, "id", label),
    cwd: str(value, "cwd", label),
    created_at: str(value, "created_at", label),
    updated_at: str(value, "updated_at", label),
    state: parseSessionState(value["state"], `${label}.state`),
    last_seq: num(value, "last_seq", label),
    live: bool(value, "live", label),
    busy: bool(value, "busy", label),
    asks: num(value, "asks", label),
    permission_mode: optStr(value, "permission_mode", label),
    cache: parseCache(value["cache"], `${label}.cache`),
    context: parseContext(value["context"], `${label}.context`),
    calls_per_message: optNum(value, "calls_per_message", label),
    restart: parseRestart(value["restart"], `${label}.restart`),
    name: optStr(value, "name", label),
    archived_at: optStr(value, "archived_at", label),
    scratch: bool(value, "scratch", label),
    model: optStr(value, "model", label),
    effort: optStr(value, "effort", label),
    settings_changed_at: optStr(value, "settings_changed_at", label),
    applied_model: optStr(value, "applied_model", label),
    applied_effort: optStr(value, "applied_effort", label),
    agents: parseAgents(value["agents"], `${label}.agents`),
    background: parseBackground(value["background"], `${label}.background`),
    unread: parseUnread(value["unread"], `${label}.unread`),
    held: parseHeld(value["held"], `${label}.held`),
  };
}

function parseHeld(raw: unknown, label: string): HeldMessage[] {
  if (raw === undefined || raw === null) return [];
  if (!Array.isArray(raw)) fail(label, "expected an array", raw);
  return raw.map((item: unknown, index) => {
    const itemLabel = `${label}[${index}]`;
    const value = record(item, itemLabel);
    const mode = value["mode"];
    if (mode !== "held" && mode !== "turn_end") {
      fail(`${itemLabel}.mode`, 'expected "held" or "turn_end"', mode);
    }
    const attachments = value["attachments"];
    if (!Array.isArray(attachments) || !attachments.every((path) => typeof path === "string")) {
      fail(`${itemLabel}.attachments`, "expected an array of strings", attachments);
    }
    return {
      id: str(value, "id", itemLabel),
      text: str(value, "text", itemLabel),
      attachments,
      mode,
      created_at: str(value, "created_at", itemLabel),
    };
  });
}

function parseBackground(raw: unknown, label: string): BackgroundView[] {
  if (raw === undefined || raw === null) return [];
  if (!Array.isArray(raw)) fail(label, "expected an array", raw);
  return raw.map((item: unknown, index) => {
    const itemLabel = `${label}[${index}]`;
    const value = record(item, itemLabel);
    return {
      task_id: str(value, "task_id", itemLabel),
      task_type: optStr(value, "task_type", itemLabel),
      description: optStr(value, "description", itemLabel),
      started_at: str(value, "started_at", itemLabel),
    };
  });
}

function parseUnread(raw: unknown, label: string): Unread | null {
  if (raw === null || raw === undefined) return null;
  if (raw === "finished" || raw === "stopped") return raw;
  throw new Error(`${label}: expected "finished", "stopped" or null: ${JSON.stringify(raw)}`);
}

const AGENT_STATES: readonly AgentState[] = ["running", "completed", "stopped", "failed"];

function parseAgents(raw: unknown, label: string): AgentView[] {
  if (raw === undefined || raw === null) return [];
  if (!Array.isArray(raw)) fail(label, "expected an array", raw);
  return raw.map((item: unknown, index) => {
    const itemLabel = `${label}[${index}]`;
    const value = record(item, itemLabel);
    const state = AGENT_STATES.find((known) => known === value["state"]);
    if (state === undefined) fail(`${itemLabel}.state`, "unknown agent state", value["state"]);
    return {
      task_id: str(value, "task_id", itemLabel),
      tool_use_id: str(value, "tool_use_id", itemLabel),
      name: str(value, "name", itemLabel),
      subagent_type: optStr(value, "subagent_type", itemLabel),
      parent_tool_use_id: optStr(value, "parent_tool_use_id", itemLabel),
      state,
      activity: optStr(value, "activity", itemLabel),
      cache: parseCache(value["cache"], `${itemLabel}.cache`),
      prompt_tokens: num(value, "prompt_tokens", itemLabel),
      started_at: str(value, "started_at", itemLabel),
    };
  });
}

function parseCache(raw: unknown, label: string): CacheView | null {
  if (raw === undefined || raw === null) return null;
  const value = record(raw, label);
  return {
    last_request_at: str(value, "last_request_at", label),
    ttl_secs: num(value, "ttl_secs", label),
  };
}

function parseContext(raw: unknown, label: string): ContextView | null {
  if (raw === undefined || raw === null) return null;
  const value = record(raw, label);
  const source = value["source"];
  if (source !== "cli" && source !== "estimate") {
    fail(`${label}.source`, "unknown context source", source);
  }
  return {
    used_tokens: num(value, "used_tokens", label),
    max_tokens: num(value, "max_tokens", label),
    percentage: num(value, "percentage", label),
    source,
    autocompact_threshold: optNum(value, "autocompact_threshold", label),
    categories: parseCategories(value["categories"], `${label}.categories`),
  };
}

function parseRestart(raw: unknown, label: string): RestartMeasure | null {
  if (raw === undefined || raw === null) return null;
  const value = record(raw, label);
  return {
    window_start_tokens: num(value, "window_start_tokens", label),
    seed_tokens: num(value, "seed_tokens", label),
    reread_tokens: num(value, "reread_tokens", label),
    other_cost_per_call: num(value, "other_cost_per_call", label),
  };
}

function parseCategories(raw: unknown, label: string): ContextCategory[] {
  if (!Array.isArray(raw)) fail(label, "expected an array", raw);
  return raw.map((item: unknown, index) => {
    const itemLabel = `${label}[${index}]`;
    const value = record(item, itemLabel);
    return {
      name: str(value, "name", itemLabel),
      tokens: num(value, "tokens", itemLabel),
      kind: str(value, "kind", itemLabel),
    };
  });
}

function parseWindowReport(raw: unknown, label: string): WindowReport | null {
  if (raw === undefined || raw === null) return null;
  const value = record(raw, label);
  const confidence = value["confidence"];
  if (confidence !== "low" && confidence !== "medium" && confidence !== "high") {
    fail(`${label}.confidence`, "unknown confidence", confidence);
  }
  return {
    used: num(value, "used", label),
    resets_at: str(value, "resets_at", label),
    projected: num(value, "projected", label),
    confidence,
    rate: num(value, "rate", label),
    work_left_hours: optNum(value, "work_left_hours", label),
    deadline: optStr(value, "deadline", label),
    reading_at: str(value, "reading_at", label),
  };
}

export function parseUsageSnapshot(raw: unknown, label: string): UsageSnapshot {
  const value = record(raw, label);
  return {
    generated_at: str(value, "generated_at", label),
    newest_reading: optStr(value, "newest_reading", label),
    five_hour: parseWindowReport(value["five_hour"], `${label}.five_hour`),
    seven_day: parseWindowReport(value["seven_day"], `${label}.seven_day`),
  };
}

/** Entries of unexpected shape are left out, as in a `catalog` event. */
export function parseSharedCatalog(raw: unknown, label: string): SharedCatalog {
  const value = record(raw, label);
  const list = <T>(key: string, parse: (entry: unknown) => T | null): T[] | null => {
    const entries = value[key];
    if (entries === undefined || entries === null) return null;
    if (!Array.isArray(entries)) fail(`${label}.${key}`, "expected an array", entries);
    return entries.flatMap((entry: unknown) => {
      const parsed = parse(entry);
      return parsed === null ? [] : [parsed];
    });
  };
  const rawDefaults = value["defaults"];
  const defaultsLabel = `${label}.defaults`;
  const defaults =
    rawDefaults === undefined || rawDefaults === null ? null : record(rawDefaults, defaultsLabel);
  return {
    models: list("models", parseModelChoice),
    commands: list("commands", parseCommandInfo),
    terminal_commands: list("terminal_commands", stringOrNull),
    defaults:
      defaults === null
        ? null
        : {
            model: optStr(defaults, "model", defaultsLabel),
            effort: optStr(defaults, "effort", defaultsLabel),
          },
  };
}

export interface HistoryMessage {
  role: "user" | "claude";
  time: string;
  text: string;
}

/** `GET api/history/message`: a session, and its message at the asked seq when it has one. */
export interface HistoryHit {
  name: string | null;
  cwd: string;
  message: HistoryMessage | null;
}

export function parseHistoryHit(raw: unknown, label: string): HistoryHit {
  const hit = record(raw, label);
  const message = hit["message"];
  let parsed: HistoryMessage | null = null;
  if (message !== null && message !== undefined) {
    const fields = record(message, `${label}.message`);
    parsed = {
      role: role(fields, `${label}.message`),
      time: str(fields, "time", `${label}.message`),
      text: str(fields, "text", `${label}.message`),
    };
  }
  return { name: optStr(hit, "name", label), cwd: str(hit, "cwd", label), message: parsed };
}

function role(raw: Record<string, unknown>, label: string): "user" | "claude" {
  const value = raw["role"];
  if (value !== "user" && value !== "claude")
    fail(`${label}.role`, "expected user or claude", value);
  return value;
}

/** A message holding every searched word. */
export interface SearchHit {
  discussion: string;
  name: string | null;
  cwd: string;
  seq: number;
  time: string;
  role: "user" | "claude";
  /** One line of the message, from a little before the first word. */
  snippet: string;
}

/** `GET api/history/search`: how many messages match, and one page of them, newest first. */
export interface SearchPage {
  total: number;
  hits: SearchHit[];
}

export function parseSearchPage(raw: unknown, label: string): SearchPage {
  const page = record(raw, label);
  const hits = page["hits"];
  if (!Array.isArray(hits)) fail(`${label}.hits`, "expected an array", hits);
  return {
    total: num(page, "total", label),
    hits: hits.map((entry: unknown, index) => {
      const hitLabel = `${label}.hits[${index}]`;
      const hit = record(entry, hitLabel);
      return {
        discussion: str(hit, "discussion", hitLabel),
        name: optStr(hit, "name", hitLabel),
        cwd: str(hit, "cwd", hitLabel),
        seq: num(hit, "seq", hitLabel),
        time: str(hit, "time", hitLabel),
        role: role(hit, hitLabel),
        snippet: str(hit, "snippet", hitLabel),
      };
    }),
  };
}

export interface AroundMessage extends HistoryMessage {
  seq: number;
}

/** `GET api/history/around`: the exchanges around a message, and whether more lie past either end. */
export interface Around {
  name: string | null;
  cwd: string;
  messages: AroundMessage[];
  earlier: boolean;
  later: boolean;
}

export function parseAround(raw: unknown, label: string): Around {
  const around = record(raw, label);
  const messages = around["messages"];
  if (!Array.isArray(messages)) fail(`${label}.messages`, "expected an array", messages);
  return {
    name: optStr(around, "name", label),
    cwd: str(around, "cwd", label),
    messages: messages.map((entry: unknown, index) => {
      const messageLabel = `${label}.messages[${index}]`;
      const message = record(entry, messageLabel);
      return {
        seq: num(message, "seq", messageLabel),
        time: str(message, "time", messageLabel),
        role: role(message, messageLabel),
        text: str(message, "text", messageLabel),
      };
    }),
    earlier: bool(around, "earlier", label),
    later: bool(around, "later", label),
  };
}

/** `GET api/commands/recent`: command names, newest first. */
export function parseRecentCommands(raw: unknown, label: string): string[] {
  if (!Array.isArray(raw)) fail(label, "expected an array", raw);
  return raw.map((entry: unknown, index) => {
    if (typeof entry !== "string") fail(`${label}[${index}]`, "expected a string", entry);
    return entry;
  });
}

export function parseSessionList(raw: unknown, label: string): SessionView[] {
  const sessions = record(raw, label)["sessions"];
  if (!Array.isArray(sessions)) fail(`${label}.sessions`, "expected an array", sessions);
  return sessions.map((session, index) => parseSessionView(session, `${label}.sessions[${index}]`));
}

/** The `error` field of an error body, or null when the body has none. */
export function parseErrorBody(raw: unknown): string | null {
  if (!isRecord(raw)) return null;
  const error = raw["error"];
  return typeof error === "string" ? error : null;
}

/** History recorded before danger checks existed has no `danger`: not dangerous. */
export function parseDanger(raw: unknown): Danger {
  if (!isRecord(raw)) return { dangerous: false, reasons: [] };
  const reasons = raw["reasons"];
  return {
    dangerous: raw["dangerous"] === true,
    reasons: Array.isArray(reasons)
      ? reasons.filter((reason): reason is string => typeof reason === "string")
      : [],
  };
}

/** The strings of a list; anything else, an absent field included, is empty. */
function stringList(raw: unknown): string[] {
  return Array.isArray(raw) ? raw.filter((item): item is string => typeof item === "string") : [];
}

/** The string-valued entries of `raw`; empty when it is not an object. */
function stringMap(raw: unknown): Record<string, string> {
  if (!isRecord(raw)) return {};
  return Object.fromEntries(
    Object.entries(raw).filter((entry): entry is [string, string] => typeof entry[1] === "string"),
  );
}

/** An offer of unexpected shape is dropped: the ask stays answerable without it. */
export function parseFolderOffer(raw: unknown): FolderOffer | null {
  if (!isRecord(raw)) return null;
  const kind = raw["kind"];
  const folder = raw["folder"];
  const broad = raw["broad"];
  if ((kind !== "read" && kind !== "directory") || typeof folder !== "string") return null;
  return {
    kind,
    folder,
    broad: Array.isArray(broad)
      ? broad.filter((path): path is string => typeof path === "string")
      : [],
  };
}

/** An offer of unexpected shape is dropped: the ask stays answerable without it. */
export function parseFileOffer(raw: unknown): FileOffer | null {
  if (!isRecord(raw)) return null;
  const kind = raw["kind"];
  const path = raw["path"];
  if ((kind !== "read" && kind !== "edit") || typeof path !== "string") return null;
  return { kind, path };
}

export function parsePlanReview(raw: unknown): PlanReview | null {
  if (!isRecord(raw)) return null;
  const text = raw["text"];
  if (typeof text !== "string") return null;
  return { path: stringOrNull(raw["path"]), text, previous: stringOrNull(raw["previous"]) };
}

export function parseHostEvent(raw: unknown): HostEvent {
  if (!isRecord(raw)) return { event: "unknown", raw };
  const text = (key: string): string | null => {
    const value = raw[key];
    return typeof value === "string" ? value : null;
  };
  switch (raw["event"]) {
    case "started": {
      const cwd = text("cwd");
      return cwd === null ? { event: "unknown", raw } : { event: "started", cwd };
    }
    case "user_sent": {
      const sentText = text("text");
      return sentText === null
        ? { event: "unknown", raw }
        : {
            event: "user_sent",
            text: sentText,
            attachments: stringList(raw["attachments"]),
            client_id: text("client_id"),
          };
    }
    case "claude_attached":
      return {
        event: "claude_attached",
        attachments: stringList(raw["attachments"]),
        hashes: stringMap(raw["hashes"]),
      };
    case "permission_mode": {
      const mode = text("mode");
      return mode === null ? { event: "unknown", raw } : { event: "permission_mode", mode };
    }
    case "ask_queued": {
      const requestId = text("request_id");
      const toolName = text("tool_name");
      const toolUseId = text("tool_use_id");
      if (requestId === null || toolName === null || toolUseId === null) {
        return { event: "unknown", raw };
      }
      return {
        event: "ask_queued",
        request_id: requestId,
        tool_name: toolName,
        tool_use_id: toolUseId,
        danger: parseDanger(raw["danger"]),
        accept_edits: raw["accept_edits"] === true,
        folder: parseFolderOffer(raw["folder"]),
        file: parseFileOffer(raw["file"]),
        plan: parsePlanReview(raw["plan"]),
        agent_id: text("agent_id"),
        deny_at: text("deny_at"),
      };
    }
    case "auto_denied": {
      const requestId = text("request_id");
      const toolName = text("tool_name");
      const toolUseId = text("tool_use_id");
      const agentId = text("agent_id");
      if (requestId === null || toolName === null || toolUseId === null || agentId === null) {
        return { event: "unknown", raw };
      }
      return {
        event: "auto_denied",
        request_id: requestId,
        tool_name: toolName,
        tool_use_id: toolUseId,
        agent_id: agentId,
      };
    }
    case "relay_sent": {
      const sentText = text("text");
      const taskId = text("task_id");
      const agentToolUseId = text("agent_tool_use_id");
      const name = text("name");
      const wrapped = text("wrapped");
      if (
        sentText === null ||
        taskId === null ||
        agentToolUseId === null ||
        name === null ||
        wrapped === null
      ) {
        return { event: "unknown", raw };
      }
      return {
        event: "relay_sent",
        text: sentText,
        attachments: stringList(raw["attachments"]),
        client_id: text("client_id"),
        task_id: taskId,
        agent_tool_use_id: agentToolUseId,
        name,
        wrapped,
      };
    }
    case "relay_checked": {
      const relaySeq = raw["relay_seq"];
      const taskId = text("task_id");
      const agentToolUseId = text("agent_tool_use_id");
      const outcome = parseRelayOutcome(raw["outcome"]);
      if (
        typeof relaySeq !== "number" ||
        taskId === null ||
        agentToolUseId === null ||
        outcome === null
      ) {
        return { event: "unknown", raw };
      }
      return {
        event: "relay_checked",
        relay_seq: relaySeq,
        task_id: taskId,
        agent_tool_use_id: agentToolUseId,
        outcome,
      };
    }
    case "auto_allowed": {
      const requestId = text("request_id");
      const toolName = text("tool_name");
      const toolUseId = text("tool_use_id");
      const file = parseFileOffer(raw["file"]);
      if (requestId === null || toolName === null || toolUseId === null || file === null) {
        return { event: "unknown", raw };
      }
      return {
        event: "auto_allowed",
        request_id: requestId,
        tool_name: toolName,
        tool_use_id: toolUseId,
        file,
        agent_id: text("agent_id"),
      };
    }
    case "ask_resolved": {
      const requestId = text("request_id");
      const resolution = raw["resolution"];
      const timedOut = isRecord(resolution) && resolution["kind"] === "timed_out";
      const answer = isRecord(resolution) ? resolution["answer"] : undefined;
      const allowed = isRecord(answer) && answer["kind"] === "allow";
      const note = allowed && typeof answer["note"] === "string" ? answer["note"].trim() : "";
      return requestId === null
        ? { event: "unknown", raw }
        : {
            event: "ask_resolved",
            request_id: requestId,
            timed_out: timedOut,
            allowed,
            note: note === "" ? null : note,
          };
    }
    case "auth_gate_failed":
      return { event: "auth_gate_failed", reason: parseGateFailure(raw["reason"]) };
    case "stopping": {
      const reason = stopReason(raw["reason"]);
      return reason === null ? { event: "unknown", raw } : { event: "stopping", reason };
    }
    case "child_exited": {
      const code = raw["code"];
      const tail = raw["stderr_tail"];
      return {
        event: "child_exited",
        code: typeof code === "number" ? code : null,
        stderr_tail: Array.isArray(tail)
          ? tail.filter((line): line is string => typeof line === "string")
          : [],
        reason: stopReason(raw["reason"]),
      };
    }
    case "restarted": {
      const mode = raw["mode"];
      const exchanges = raw["exchanges"];
      const total = raw["total_exchanges"];
      if (
        (mode !== "continue" && mode !== "blank" && mode !== "clear") ||
        typeof exchanges !== "number" ||
        typeof total !== "number"
      ) {
        return { event: "unknown", raw };
      }
      return {
        event: "restarted",
        mode,
        exchanges,
        total_exchanges: total,
        seed: text("seed"),
      };
    }
    case "catalog": {
      const models = raw["models"];
      const commands = raw["commands"];
      if (!Array.isArray(models) || !Array.isArray(commands)) return { event: "unknown", raw };
      return {
        event: "catalog",
        models: models.flatMap((model: unknown) => {
          const parsed = parseModelChoice(model);
          return parsed === null ? [] : [parsed];
        }),
        commands: commands.flatMap((command: unknown) => {
          const parsed = parseCommandInfo(command);
          return parsed === null ? [] : [parsed];
        }),
      };
    }
    default:
      return { event: "unknown", raw };
  }
}

/** Null for an entry of unexpected shape, which the catalog then leaves out. */
function parseModelChoice(raw: unknown): ModelChoice | null {
  if (!isRecord(raw)) return null;
  const value = stringOrNull(raw["value"]);
  if (value === null) return null;
  const levels = raw["effort_levels"];
  return {
    value,
    resolved_model: stringOrNull(raw["resolved_model"]),
    display_name: stringOrNull(raw["display_name"]) ?? value,
    description: stringOrNull(raw["description"]),
    effort_levels: Array.isArray(levels)
      ? levels.filter((level): level is string => typeof level === "string")
      : [],
  };
}

function parseCommandInfo(raw: unknown): CommandInfo | null {
  if (!isRecord(raw)) return null;
  const name = stringOrNull(raw["name"]);
  if (name === null) return null;
  return {
    name,
    description: stringOrNull(raw["description"]) ?? "",
    argument_hint: stringOrNull(raw["argument_hint"]) ?? "",
  };
}

export function parseWireEvent(raw: unknown, label: string): WireEvent {
  const value = record(raw, label);
  const base = {
    session: str(value, "session", label),
    seq: num(value, "seq", label),
    time: str(value, "time", label),
  };
  const kind = value["kind"];
  if (kind === "line") return { ...base, kind, payload: value["payload"] };
  if (kind === "host") return { ...base, kind, payload: parseHostEvent(value["payload"]) };
  return fail(`${label}.kind`, "unknown event kind", kind);
}

export function parseServerMessage(raw: unknown, label: string): ServerMessage {
  const value = record(raw, label);
  const type = value["type"];
  switch (type) {
    case "event":
      return { type, event: parseWireEvent(value, label) };
    case "window":
      return {
        type,
        session: str(value, "session", label),
        first_seq: num(value, "first_seq", label),
        start_reached: bool(value, "start_reached", label),
      };
    case "page": {
      const events = value["events"];
      if (!Array.isArray(events)) fail(`${label}.events`, "expected an array", events);
      return {
        type,
        session: str(value, "session", label),
        first_seq: num(value, "first_seq", label),
        start_reached: bool(value, "start_reached", label),
        events: events.map((event: unknown, index: number) =>
          parseWireEvent(event, `${label}.events[${index}]`),
        ),
      };
    }
    case "ack":
      return {
        type,
        client_id: str(value, "client_id", label),
        ok: bool(value, "ok", label),
        error: optStr(value, "error", label),
      };
    case "synced":
      return {
        type,
        session: str(value, "session", label),
        seq: num(value, "seq", label),
      };
    case "sessions_changed":
    case "usage_changed":
    case "catalog_changed":
      return { type };
    case "error":
      return {
        type,
        message: str(value, "message", label),
        session: optStr(value, "session", label),
      };
    default:
      return fail(`${label}.type`, "unknown server message", type);
  }
}

// CLI stdout lines, narrowed to the parts the transcript reads.

export type ContentBlock =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string }
  | { type: "tool_use"; id: string; name: string; input: unknown }
  | { type: "tool_result"; toolUseId: string; content: string; isError: boolean }
  | { type: "other"; raw: unknown };

export type StreamEvent =
  | { type: "message_start"; messageId: string | null }
  | { type: "text_delta"; index: number; text: string }
  | { type: "message_stop" }
  | { type: "other" };

export type CliLine =
  | {
      type: "user";
      parentToolUseId: string | null;
      isReplay: boolean;
      isMeta: boolean;
      /** Written by the CLI, not typed: a loaded skill's text, a subagent's brief. */
      isSynthetic: boolean;
      /** What wrote the message when not the user, such as `task-notification`. */
      origin: string | null;
      content: string | ContentBlock[];
      /** The CLI's structured result of the tool call answered by this line, if any. */
      toolUseResult: unknown;
    }
  | {
      type: "assistant";
      parentToolUseId: string | null;
      messageId: string | null;
      content: ContentBlock[];
    }
  | { type: "stream_event"; parentToolUseId: string | null; event: StreamEvent }
  | { type: "result"; subtype: string; isError: boolean }
  | {
      type: "system";
      subtype: string;
      permissionMode: string | null;
      /** `terminal_slash_commands` of an `init`: commands that only work in the TUI. Null when absent. */
      terminalCommands: string[] | null;
      /** Set on the `task_*` subtypes. */
      task: TaskLine | null;
    }
  | {
      type: "can_use_tool";
      requestId: string;
      toolName: string;
      input: unknown;
      toolUseId: string | null;
    }
  | { type: "raw"; raw: unknown };

/** The fields of a `task_started`, `task_progress`, `task_updated` or `task_notification` line. */
export interface TaskLine {
  taskId: string;
  /** The call that started the task: the Agent call of a subagent. */
  toolUseId: string | null;
  /** `local_agent` for a subagent, on `task_started`. */
  taskType: string | null;
  description: string | null;
  /** A subagent's brief, on `task_started`. */
  prompt: string | null;
  /** Of `task_notification`, or of `task_updated`'s patch. */
  status: string | null;
  summary: string | null;
}

function parseTaskLine(raw: Record<string, unknown>): TaskLine | null {
  const taskId = stringOrNull(raw["task_id"]);
  if (taskId === null) return null;
  const patch = raw["patch"];
  return {
    taskId,
    toolUseId: stringOrNull(raw["tool_use_id"]),
    taskType: stringOrNull(raw["task_type"]),
    description: stringOrNull(raw["description"]),
    prompt: stringOrNull(raw["prompt"]),
    status: stringOrNull(raw["status"]) ?? (isRecord(patch) ? stringOrNull(patch["status"]) : null),
    summary: stringOrNull(raw["summary"]),
  };
}

export function stringOrNull(raw: unknown): string | null {
  return typeof raw === "string" ? raw : null;
}

/** The `kind` of a user line's `origin`, such as `task-notification`. */
function originKind(raw: unknown): string | null {
  return isRecord(raw) ? stringOrNull(raw["kind"]) : null;
}

function toolResultText(raw: unknown): string {
  if (typeof raw === "string") return raw;
  if (!Array.isArray(raw)) return raw === undefined || raw === null ? "" : JSON.stringify(raw);
  return raw
    .map((part: unknown) => {
      if (isRecord(part) && part["type"] === "text" && typeof part["text"] === "string") {
        return part["text"];
      }
      if (isRecord(part) && typeof part["type"] === "string") return `[${part["type"]}]`;
      return JSON.stringify(part);
    })
    .join("\n");
}

export function parseContentBlock(raw: unknown): ContentBlock {
  if (!isRecord(raw)) return { type: "other", raw };
  switch (raw["type"]) {
    case "text": {
      const text = stringOrNull(raw["text"]);
      return text === null ? { type: "other", raw } : { type: "text", text };
    }
    case "thinking":
      return { type: "thinking", thinking: stringOrNull(raw["thinking"]) ?? "" };
    case "tool_use": {
      const id = stringOrNull(raw["id"]);
      const name = stringOrNull(raw["name"]);
      if (id === null || name === null) return { type: "other", raw };
      return { type: "tool_use", id, name, input: raw["input"] };
    }
    case "tool_result": {
      const toolUseId = stringOrNull(raw["tool_use_id"]);
      if (toolUseId === null) return { type: "other", raw };
      return {
        type: "tool_result",
        toolUseId,
        content: toolResultText(raw["content"]),
        isError: raw["is_error"] === true,
      };
    }
    default:
      return { type: "other", raw };
  }
}

function parseStreamEvent(raw: unknown): StreamEvent {
  if (!isRecord(raw)) return { type: "other" };
  switch (raw["type"]) {
    case "message_start": {
      const message = raw["message"];
      return {
        type: "message_start",
        messageId: isRecord(message) ? stringOrNull(message["id"]) : null,
      };
    }
    case "content_block_delta": {
      const delta = raw["delta"];
      const index = raw["index"];
      if (
        typeof index === "number" &&
        isRecord(delta) &&
        delta["type"] === "text_delta" &&
        typeof delta["text"] === "string"
      ) {
        return { type: "text_delta", index, text: delta["text"] };
      }
      return { type: "other" };
    }
    case "message_stop":
      return { type: "message_stop" };
    default:
      return { type: "other" };
  }
}

/** Never throws: a line of unexpected shape comes back as `raw`. */
export function parseCliLine(raw: unknown): CliLine {
  if (!isRecord(raw)) return { type: "raw", raw };
  const parentToolUseId = stringOrNull(raw["parent_tool_use_id"]);
  switch (raw["type"]) {
    case "user": {
      const message = raw["message"];
      if (!isRecord(message)) return { type: "raw", raw };
      const content = message["content"];
      let parsed: string | ContentBlock[];
      if (typeof content === "string") parsed = content;
      else if (Array.isArray(content)) parsed = content.map(parseContentBlock);
      else return { type: "raw", raw };
      return {
        type: "user",
        parentToolUseId,
        isReplay: raw["isReplay"] === true,
        isMeta: raw["isMeta"] === true,
        isSynthetic: raw["isSynthetic"] === true,
        origin: originKind(raw["origin"]),
        content: parsed,
        toolUseResult: raw["tool_use_result"],
      };
    }
    case "assistant": {
      const message = raw["message"];
      if (!isRecord(message) || !Array.isArray(message["content"])) return { type: "raw", raw };
      return {
        type: "assistant",
        parentToolUseId,
        messageId: stringOrNull(message["id"]),
        content: message["content"].map(parseContentBlock),
      };
    }
    case "stream_event":
      return { type: "stream_event", parentToolUseId, event: parseStreamEvent(raw["event"]) };
    case "result": {
      const subtype = stringOrNull(raw["subtype"]);
      if (subtype === null) return { type: "raw", raw };
      return { type: "result", subtype, isError: raw["is_error"] === true };
    }
    case "system": {
      const subtype = stringOrNull(raw["subtype"]);
      const terminal = raw["terminal_slash_commands"];
      return subtype === null
        ? { type: "raw", raw }
        : {
            type: "system",
            subtype,
            permissionMode: stringOrNull(raw["permissionMode"]),
            terminalCommands: Array.isArray(terminal)
              ? terminal.filter((name): name is string => typeof name === "string")
              : null,
            task: subtype.startsWith("task_") ? parseTaskLine(raw) : null,
          };
    }
    case "control_request": {
      const requestId = stringOrNull(raw["request_id"]);
      const request = raw["request"];
      if (requestId === null || !isRecord(request) || request["subtype"] !== "can_use_tool") {
        return { type: "raw", raw };
      }
      const toolName = stringOrNull(request["tool_name"]);
      if (toolName === null) return { type: "raw", raw };
      return {
        type: "can_use_tool",
        requestId,
        toolName,
        input: request["input"],
        toolUseId: stringOrNull(request["tool_use_id"]),
      };
    }
    default:
      return { type: "raw", raw };
  }
}
