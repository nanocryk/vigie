// Mirror of `src/wire.rs`: the websocket protocol at `api/ws` and the bodies of
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
  /** Null while the discussion is not archived. */
  archived_at: string | null;
  /** Stored model and effort every launch passes; null lets the CLI pick. */
  model: string | null;
  effort: string | null;
  /** When a model or effort change made the cache count cold; null once a main request or a restart followed. */
  settings_changed_at: string | null;
  /** What the CLI last reported applying, e.g. `claude-opus-5-5[1m]` and `medium`; null while unknown. */
  applied_model: string | null;
  applied_effort: string | null;
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
    }
  | { kind: "deny"; message: string }
  | { kind: "questions"; answers: Record<string, string> };

export type ClientMessage =
  | { type: "subscribe"; session: string; after_seq: number }
  | { type: "unsubscribe"; session: string }
  | { type: "send"; session: string; text: string; rewarm?: true; client_id: string }
  | { type: "interrupt"; session: string; client_id: string }
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
  | { type: "restart"; session: string; mode: RestartMode; text?: string; client_id: string }
  /** `reason` defaults to `kill`, a stop from Home. */
  | { type: "kill"; session: string; reason?: StopReason; client_id: string }
  | { type: "archive"; session: string; client_id: string }
  | { type: "restore"; session: string; client_id: string };

export type RestartMode = "continue" | "blank" | "clear";

/** Why a client stopped a live child: from Home, or to drop the granted folders. */
export type StopReason = "kill" | "revoke";

export type Command = Extract<ClientMessage, { client_id: string }>;

/** Ack error of a `send` or an `answer` refused on a cold cache without `rewarm`. */
export const CACHE_COLD = "cache_cold";
/** Ack error of an `archive` refused while the child runs. */
export const SESSION_LIVE = "session_live";

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

export type HostEvent =
  | { event: "started"; cwd: string }
  | { event: "user_sent"; text: string; client_id: string | null }
  | {
      event: "ask_queued";
      request_id: string;
      tool_name: string;
      tool_use_id: string;
      danger: Danger;
      /** An edit ask offering a switch to Accept edits. */
      accept_edits: boolean;
      folder: FolderOffer | null;
    }
  | { event: "ask_resolved"; request_id: string }
  | { event: "permission_mode"; mode: string }
  | { event: "auth_gate_failed"; reason: GateFailure }
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
    archived_at: optStr(value, "archived_at", label),
    model: optStr(value, "model", label),
    effort: optStr(value, "effort", label),
    settings_changed_at: optStr(value, "settings_changed_at", label),
    applied_model: optStr(value, "applied_model", label),
    applied_effort: optStr(value, "applied_effort", label),
  };
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
  const list = <T,>(key: string, parse: (entry: unknown) => T | null): T[] | null => {
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
        : { event: "user_sent", text: sentText, client_id: text("client_id") };
    }
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
      };
    }
    case "ask_resolved": {
      const requestId = text("request_id");
      return requestId === null
        ? { event: "unknown", raw }
        : { event: "ask_resolved", request_id: requestId };
    }
    case "auth_gate_failed":
      return { event: "auth_gate_failed", reason: parseGateFailure(raw["reason"]) };
    case "child_exited": {
      const code = raw["code"];
      const tail = raw["stderr_tail"];
      return {
        event: "child_exited",
        code: typeof code === "number" ? code : null,
        stderr_tail: Array.isArray(tail)
          ? tail.filter((line): line is string => typeof line === "string")
          : [],
        reason: raw["reason"] === "kill" || raw["reason"] === "revoke" ? raw["reason"] : null,
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

export function parseServerMessage(raw: unknown, label: string): ServerMessage {
  const value = record(raw, label);
  const type = value["type"];
  switch (type) {
    case "event": {
      const base = {
        session: str(value, "session", label),
        seq: num(value, "seq", label),
        time: str(value, "time", label),
      };
      const kind = value["kind"];
      if (kind === "line") return { type, event: { ...base, kind, payload: value["payload"] } };
      if (kind === "host") {
        return { type, event: { ...base, kind, payload: parseHostEvent(value["payload"]) } };
      }
      return fail(`${label}.kind`, "unknown event kind", kind);
    }
    case "ack":
      return {
        type,
        client_id: str(value, "client_id", label),
        ok: bool(value, "ok", label),
        error: optStr(value, "error", label),
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
    }
  | {
      type: "can_use_tool";
      requestId: string;
      toolName: string;
      input: unknown;
      toolUseId: string | null;
    }
  | { type: "raw"; raw: unknown };

function stringOrNull(raw: unknown): string | null {
  return typeof raw === "string" ? raw : null;
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
