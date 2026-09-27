// Mirror of `src/wire.rs`: the websocket protocol at `api/ws` and the bodies of
// `api/sessions`. Envelopes are validated strictly; CLI lines and host events of
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
}

export type Answer =
  | { kind: "allow"; updated_input?: unknown }
  | { kind: "deny"; message: string }
  | { kind: "questions"; answers: Record<string, string> };

export type ClientMessage =
  | { type: "subscribe"; session: string; after_seq: number }
  | { type: "unsubscribe"; session: string }
  | { type: "send"; session: string; text: string; client_id: string }
  | { type: "interrupt"; session: string; client_id: string }
  | { type: "answer"; session: string; request_id: string; answer: Answer; client_id: string }
  | { type: "set_permission_mode"; session: string; mode: string; client_id: string };

export type Command = Extract<ClientMessage, { client_id: string }>;

export type HostEvent =
  | { event: "started"; cwd: string }
  | { event: "user_sent"; text: string; client_id: string | null }
  | { event: "ask_queued"; request_id: string; tool_name: string; tool_use_id: string }
  | { event: "ask_resolved"; request_id: string }
  | { event: "auth_gate_failed"; reason: GateFailure }
  | { event: "child_exited"; code: number | null; stderr_tail: string[] }
  | { event: "unknown"; raw: unknown };

export type WireEvent =
  | { session: string; seq: number; time: string; kind: "line"; payload: unknown }
  | { session: string; seq: number; time: string; kind: "host"; payload: HostEvent };

export type ServerMessage =
  | { type: "event"; event: WireEvent }
  | { type: "ack"; client_id: string; ok: boolean; error: string | null }
  | { type: "sessions_changed" }
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
      };
    }
    default:
      return { event: "unknown", raw };
  }
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
      content: string | ContentBlock[];
    }
  | {
      type: "assistant";
      parentToolUseId: string | null;
      messageId: string | null;
      content: ContentBlock[];
    }
  | { type: "stream_event"; parentToolUseId: string | null; event: StreamEvent }
  | { type: "result"; subtype: string; isError: boolean }
  | { type: "system"; subtype: string }
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
        content: parsed,
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
      return subtype === null ? { type: "raw", raw } : { type: "system", subtype };
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
