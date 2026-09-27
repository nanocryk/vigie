import {
  type CliLine,
  type ContentBlock,
  type GateFailure,
  type HostEvent,
  type WireEvent,
  isRecord,
  parseCliLine,
} from "@/wire";

export type ToolStatus = "running" | "ok" | "error" | "unfinished";

export type Item =
  | { kind: "user"; key: string; text: string }
  | { kind: "claude"; key: string; text: string; streaming: boolean }
  | { kind: "thinking"; key: string; text: string }
  | {
      kind: "tool";
      key: string;
      toolUseId: string;
      name: string;
      summary: string;
      input: unknown;
      status: ToolStatus;
      output: string | null;
    }
  | { kind: "notice"; key: string; tone: "info" | "error"; text: string; detail: string | null };

export interface PendingAsk {
  requestId: string;
  toolName: string;
  toolUseId: string;
  /** Undefined when the matching `can_use_tool` line was not seen. */
  input: unknown;
}

export interface TranscriptState {
  lastSeq: number;
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
}

export const emptyTranscript: TranscriptState = {
  lastSeq: 0,
  items: [],
  asks: [],
  tools: new Map(),
  openText: new Map(),
  liveBlocks: new Map(),
  streamMessage: null,
  requests: new Map(),
  interrupted: false,
};

const INTERRUPT_PREFIX = "[Request interrupted by user";
const SUMMARY_LIMIT = 300;
const UNKNOWN_MESSAGE = "?";

function field(input: unknown, key: string): string | null {
  if (!isRecord(input)) return null;
  const value = input[key];
  return typeof value === "string" ? value : null;
}

function compactJson(input: unknown): string {
  const text = JSON.stringify(input) ?? "";
  return text.length > SUMMARY_LIMIT ? `${text.slice(0, SUMMARY_LIMIT)}...` : text;
}

/** One line describing a tool call from its input. */
export function toolSummary(name: string, input: unknown): string {
  switch (name) {
    case "Bash":
      return field(input, "command") ?? compactJson(input);
    case "Read":
    case "Write":
    case "Edit":
    case "MultiEdit":
      return field(input, "file_path") ?? compactJson(input);
    case "NotebookEdit":
      return field(input, "notebook_path") ?? compactJson(input);
    case "Grep":
    case "Glob":
      return field(input, "pattern") ?? compactJson(input);
    default:
      return compactJson(input);
  }
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
  items: Item[];
  asks: PendingAsk[];
  tools: Map<string, number>;
  openText: Map<string, number[]>;
  liveBlocks: Map<string, number>;
  streamMessage: string | null;
  requests: Map<string, { toolName: string; input: unknown }>;
  interrupted: boolean;

  constructor(state: TranscriptState) {
    this.lastSeq = state.lastSeq;
    this.items = [...state.items];
    this.asks = [...state.asks];
    this.tools = new Map(state.tools);
    this.openText = new Map([...state.openText].map(([id, list]) => [id, [...list]]));
    this.liveBlocks = new Map(state.liveBlocks);
    this.streamMessage = state.streamMessage;
    this.requests = new Map(state.requests);
    this.interrupted = state.interrupted;
  }

  finish(): TranscriptState {
    return {
      lastSeq: this.lastSeq,
      items: this.items,
      asks: this.asks,
      tools: this.tools,
      openText: this.openText,
      liveBlocks: this.liveBlocks,
      streamMessage: this.streamMessage,
      requests: this.requests,
      interrupted: this.interrupted,
    };
  }

  apply(event: WireEvent): void {
    if (event.seq <= this.lastSeq) return;
    this.lastSeq = event.seq;
    const key = String(event.seq);
    if (event.kind === "host") this.host(event.payload, key);
    else this.line(parseCliLine(event.payload), key);
  }

  private host(event: HostEvent, key: string): void {
    switch (event.event) {
      case "started":
        this.asks = [];
        this.endStreaming();
        return;
      case "ask_queued": {
        const request = this.requests.get(event.request_id);
        this.asks.push({
          requestId: event.request_id,
          toolName: event.tool_name,
          toolUseId: event.tool_use_id,
          input: request?.input,
        });
        return;
      }
      case "ask_resolved":
        this.asks = this.asks.filter((ask) => ask.requestId !== event.request_id);
        this.requests.delete(event.request_id);
        return;
      case "auth_gate_failed":
        this.items.push({
          kind: "notice",
          key,
          tone: "error",
          text: gateFailureText(event.reason),
          detail: null,
        });
        return;
      case "child_exited": {
        this.asks = [];
        this.requests.clear();
        this.endStreaming();
        this.items = this.items.map((item) =>
          item.kind === "tool" && item.status === "running"
            ? { ...item, status: "unfinished" }
            : item,
        );
        const code = event.code === null ? "no exit code" : `code ${event.code}`;
        this.items.push({
          kind: "notice",
          key,
          tone: event.code === 0 ? "info" : "error",
          text: `Claude process ended (${code})`,
          detail: event.stderr_tail.length > 0 ? event.stderr_tail.join("\n") : null,
        });
        return;
      }
      case "unknown":
        return;
    }
  }

  private line(line: CliLine, key: string): void {
    switch (line.type) {
      case "user":
        if (line.parentToolUseId === null) this.user(line.content, line.isMeta, key);
        return;
      case "assistant":
        if (line.parentToolUseId === null) this.assistant(line.messageId, line.content, key);
        return;
      case "stream_event":
        if (line.parentToolUseId !== null) return;
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
          this.items.push({
            kind: "notice",
            key,
            tone: "error",
            text: `Turn ended with an error (${line.subtype})`,
            detail: null,
          });
        }
        this.interrupted = false;
        this.endStreaming();
        return;
      case "can_use_tool":
        this.requests.set(line.requestId, { toolName: line.toolName, input: line.input });
        return;
      case "raw":
        return;
    }
  }

  private user(content: string | ContentBlock[], isMeta: boolean, key: string): void {
    const blocks: ContentBlock[] =
      typeof content === "string" ? [{ type: "text", text: content }] : content;
    const texts: string[] = [];
    for (const block of blocks) {
      if (block.type === "text") texts.push(block.text);
      else if (block.type === "tool_result") this.toolResult(block);
    }
    if (texts.length === 0 || isMeta) return;
    const text = texts.join("\n");
    if (text.startsWith(INTERRUPT_PREFIX)) {
      this.interrupted = true;
      this.items.push({
        kind: "notice",
        key,
        tone: "info",
        text: text.replace(/^\[|\]$/g, ""),
        detail: null,
      });
      return;
    }
    this.items.push({ kind: "user", key, text });
  }

  private toolResult(block: Extract<ContentBlock, { type: "tool_result" }>): void {
    const index = this.tools.get(block.toolUseId);
    if (index === undefined) return;
    const item = this.items[index];
    if (item?.kind !== "tool") return;
    this.items[index] = {
      ...item,
      status: block.isError ? "error" : "ok",
      output: block.content,
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
            this.items[index] = { ...item, text: block.text, streaming: false };
          } else {
            this.items.push({ kind: "claude", key: blockKey, text: block.text, streaming: false });
          }
          return;
        }
        case "thinking":
          this.items.push({ kind: "thinking", key: blockKey, text: block.thinking });
          return;
        case "tool_use":
          this.tools.set(block.id, this.items.length);
          this.items.push({
            kind: "tool",
            key: blockKey,
            toolUseId: block.id,
            name: block.name,
            summary: toolSummary(block.name, block.input),
            input: block.input,
            status: "running",
            output: null,
          });
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
    this.items.push({ kind: "claude", key: `${key}:live`, text, streaming: true });
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
