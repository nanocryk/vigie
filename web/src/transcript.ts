import {
  type CliLine,
  type CommandInfo,
  type ContentBlock,
  type ModelChoice,
  type Danger,
  type FolderOffer,
  type GateFailure,
  type HostEvent,
  type RestartMode,
  type WireEvent,
  isRecord,
  parseCliLine,
} from "@/wire";

export type ToolStatus = "running" | "ok" | "error" | "unfinished";

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
export type Item = { time: number | null } & ItemBody;

type ItemBody =
  | { kind: "user"; key: string; text: string; delivery: Delivery; clientId: string | null }
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
      /** The CLI's `tool_use_result`, undefined until reported. */
      result: unknown;
      /** When its result arrived. */
      endTime: number | null;
      /** Text the CLI injected after the call: a loaded skill's content. */
      attached: string | null;
    }
  | { kind: "notice"; key: string; tone: "info" | "error"; text: string; detail: string | null }
  | { kind: "restart"; key: string; mode: RestartMode; exchanges: number; seed: string | null };

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
  /** A turn has started and its `result` has not arrived. */
  turnOpen: boolean;
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
  items: [],
  asks: [],
  tools: new Map(),
  openText: new Map(),
  liveBlocks: new Map(),
  streamMessage: null,
  requests: new Map(),
  interrupted: false,
  turnOpen: false,
  outbox: [],
  cwd: null,
  permissionMode: null,
  pendingSeed: null,
  catalog: null,
  terminalCommands: [],
};

const INTERRUPT_PREFIX = "[Request interrupted by user";
const SKILL_TOOL = "Skill";
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

function shortValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return `[${value.length}]`;
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
  items: Item[];
  asks: PendingAsk[];
  tools: Map<string, number>;
  openText: Map<string, number[]>;
  liveBlocks: Map<string, number>;
  streamMessage: string | null;
  requests: Map<string, { toolName: string; input: unknown }>;
  interrupted: boolean;
  turnOpen: boolean;
  outbox: Outgoing[];
  cwd: string | null;
  permissionMode: string | null;
  pendingSeed: string | null;
  catalog: Catalog | null;
  terminalCommands: readonly string[];

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
    this.turnOpen = state.turnOpen;
    this.outbox = state.outbox.map((entry) => ({ ...entry }));
    this.cwd = state.cwd;
    this.permissionMode = state.permissionMode;
    this.pendingSeed = state.pendingSeed;
    this.catalog = state.catalog;
    this.terminalCommands = state.terminalCommands;
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
      turnOpen: this.turnOpen,
      outbox: this.outbox,
      cwd: this.cwd,
      permissionMode: this.permissionMode,
      pendingSeed: this.pendingSeed,
      catalog: this.catalog,
      terminalCommands: this.terminalCommands,
    };
  }

  /** Time of the event being applied. */
  private time: number | null = null;

  private add(body: ItemBody): void {
    this.items.push({ ...body, time: this.time });
  }

  apply(event: WireEvent): void {
    if (event.seq <= this.lastSeq) return;
    this.lastSeq = event.seq;
    this.time = parseTime(event.time);
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
        });
        return;
      }
      case "ask_resolved":
        this.asks = this.asks.filter((ask) => ask.requestId !== event.request_id);
        this.requests.delete(event.request_id);
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
      case "child_exited": {
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
        const code = event.code === null ? "no exit code" : `code ${event.code}`;
        this.add({
          kind: "notice",
          key,
          tone: event.code === 0 ? "info" : "error",
          text: `Claude process ended (${code})`,
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
    switch (line.type) {
      case "user":
        if (line.parentToolUseId === null) {
          this.user(line.content, line.isMeta, line.isSynthetic, line.toolUseResult, key);
        }
        return;
      case "assistant":
        if (line.parentToolUseId !== null) return;
        this.answered(false);
        this.turnOpen = true;
        this.assistant(line.messageId, line.content, key);
        return;
      case "system":
        if (line.subtype === "init") this.turnOpen = true;
        if (line.subtype === "init" && line.terminalCommands !== null) {
          this.terminalCommands = line.terminalCommands;
        }
        // History recorded before `permission_mode` host events only has these.
        if ((line.subtype === "init" || line.subtype === "status") && line.permissionMode !== null) {
          this.permissionMode = line.permissionMode;
        }
        return;
      case "stream_event":
        if (line.parentToolUseId !== null) return;
        this.turnOpen = true;
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
    if (isSynthetic) {
      this.synthetic(text, key);
      return;
    }
    if (text.startsWith(INTERRUPT_PREFIX)) {
      this.interrupted = true;
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
    this.turnOpen = true;
    const seed = this.pendingSeed;
    this.pendingSeed = null;
    const prefix = seed === null ? null : `${seed}\n\n`;
    const seeded = prefix !== null && echoed.startsWith(prefix);
    const text = seeded ? echoed.slice(prefix.length) : echoed;
    const matches = (entry: Outgoing, delivery: Delivery): boolean => {
      const item = this.items[entry.index];
      return item?.kind === "user" && item.delivery === delivery && item.text === text;
    };
    const onHost = this.outbox.findIndex((entry) => matches(entry, "on_host"));
    const position =
      onHost >= 0 ? onHost : this.outbox.findIndex((entry) => matches(entry, "handled"));
    const entry = this.outbox[position];
    const item = entry === undefined ? undefined : this.items[entry.index];
    if (entry === undefined || item?.kind !== "user") {
      if (seeded) return;
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

  /**
   * Text the CLI wrote in the user's place. It carries no link to its call on
   * stdout, so a skill's content folds into the latest Skill call.
   */
  private synthetic(text: string, key: string): void {
    for (let index = this.items.length - 1; index >= 0; index -= 1) {
      const item = this.items[index];
      if (item?.kind !== "tool" || item.name !== SKILL_TOOL) continue;
      if (item.attached !== null) break;
      this.items[index] = { ...item, attached: text };
      return;
    }
    this.add({ kind: "notice", key, tone: "info", text: "System message", detail: text });
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
            this.items[index] = { ...item, text: block.text, streaming: false };
          } else {
            this.add({ kind: "claude", key: blockKey, text: block.text, streaming: false });
          }
          return;
        }
        case "thinking":
          this.add({ kind: "thinking", key: blockKey, text: block.thinking });
          return;
        case "tool_use":
          this.tools.set(block.id, this.items.length);
          this.add({
            kind: "tool",
            key: blockKey,
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
    this.add({ kind: "claude", key: `${key}:live`, text, streaming: true });
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
