import { ackError } from "@/cold";
import { type TranscriptState, applyEvents, emptyTranscript } from "@/transcript";
import {
  type ClientMessage,
  type Command,
  type ServerMessage,
  type SessionView,
  type UsageSnapshot,
  type WireEvent,
  parseErrorBody,
  parseServerMessage,
  parseSessionList,
  parseSessionView,
  parseUsageSnapshot,
} from "@/wire";

export type ConnectionStatus = "connecting" | "open" | "closed";

export interface Snapshot {
  status: ConnectionStatus;
  sessions: readonly SessionView[] | null;
  sessionsError: string | null;
  usage: UsageSnapshot | null;
  usageError: string | null;
  transcripts: ReadonlyMap<string, TranscriptState>;
  /** Last `error` the server reported for a session. */
  sessionErrors: ReadonlyMap<string, string>;
}

export type CreateResult = { ok: true; session: SessionView } | { ok: false; error: string };

const RETRY_BASE_MS = 500;
const RETRY_MAX_MS = 15_000;
/** A socket that sat through a longer hide may be dead without knowing it. */
const STALE_AFTER_HIDDEN_MS = 15_000;

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function socketUrl(): string {
  const url = new URL("api/ws", location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.hash = "";
  return url.toString();
}

/** Runs `task` once at a time; calls made meanwhile fold into one more run after it. */
function coalesced(task: () => Promise<void>): () => Promise<void> {
  let running: Promise<void> | null = null;
  let again = false;
  return async () => {
    if (running !== null) {
      again = true;
      return running;
    }
    running = (async () => {
      do {
        again = false;
        await task();
      } while (again);
    })();
    try {
      await running;
    } finally {
      running = null;
    }
  };
}

interface PendingAck {
  resolve: () => void;
  reject: (err: Error) => void;
}

/**
 * The one websocket to the server, the session list, and the transcripts of the
 * sessions this client has opened. Snapshots are immutable; `subscribe` follows
 * the `useSyncExternalStore` contract.
 */
export class Client {
  private snapshot: Snapshot = {
    status: "closed",
    sessions: null,
    sessionsError: null,
    usage: null,
    usageError: null,
    transcripts: new Map(),
    sessionErrors: new Map(),
  };
  private listeners = new Set<() => void>();
  private socket: WebSocket | null = null;
  private started = false;
  private attempt = 0;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private hiddenAt: number | null = null;
  /** Sessions to (re)subscribe on every open. */
  private subscribed = new Set<string>();
  /** Highest seq received per session, applied or still queued. */
  private received = new Map<string, number>();
  private queued = new Map<string, WireEvent[]>();
  private frame: number | null = null;
  private acks = new Map<string, PendingAck>();
  private nextId = 0;

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  readonly getSnapshot = (): Snapshot => this.snapshot;

  start(): void {
    if (this.started) return;
    this.started = true;
    const wake = (): void => this.wake();
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "hidden") this.hiddenAt = Date.now();
      else wake();
    });
    window.addEventListener("pagehide", () => {
      this.hiddenAt = Date.now();
    });
    window.addEventListener("pageshow", wake);
    window.addEventListener("focus", wake);
    window.addEventListener("online", wake);
    this.connect();
    void this.refreshSessions();
    void this.refreshUsage();
  }

  openSession(id: string): void {
    this.subscribed.add(id);
    this.send({ type: "subscribe", session: id, after_seq: this.received.get(id) ?? 0 });
  }

  closeSession(id: string): void {
    this.subscribed.delete(id);
    this.send({ type: "unsubscribe", session: id });
  }

  /** Resolves on a positive ack; rejects with the server's error or a lost connection. */
  newClientId(): string {
    this.nextId += 1;
    return `${Date.now().toString(36)}-${this.nextId}`;
  }

  command(message: Command): Promise<void> {
    const socket = this.socket;
    if (socket === null || socket.readyState !== WebSocket.OPEN) {
      return Promise.reject(new Error("Not connected to the server"));
    }
    return new Promise<void>((resolve, reject) => {
      this.acks.set(message.client_id, { resolve, reject });
      socket.send(JSON.stringify(message));
    });
  }

  readonly refreshSessions = coalesced(async () => {
    try {
      const response = await fetch("api/sessions");
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const sessions = parseSessionList(await response.json(), "sessions");
      this.update({ sessions, sessionsError: null });
    } catch (err) {
      this.update({ sessionsError: `Cannot load sessions: ${errorMessage(err)}` });
    }
  });

  readonly refreshUsage = coalesced(async () => {
    try {
      const response = await fetch("api/usage");
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const usage = parseUsageSnapshot(await response.json(), "usage");
      this.update({ usage, usageError: null });
    } catch (err) {
      this.update({ usageError: `Cannot load usage: ${errorMessage(err)}` });
    }
  });

  async createSession(cwd: string): Promise<CreateResult> {
    let response: Response;
    try {
      response = await fetch("api/sessions", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ cwd }),
      });
    } catch (err) {
      return { ok: false, error: `Request failed: ${errorMessage(err)}` };
    }
    let body: unknown = null;
    try {
      body = await response.json();
    } catch {
      body = null;
    }
    if (!response.ok) {
      const error = parseErrorBody(body);
      if (error === "bad_cwd") {
        return { ok: false, error: "Not an existing folder given by its absolute path." };
      }
      return { ok: false, error: error ?? `HTTP ${response.status}` };
    }
    try {
      const session = parseSessionView(body, "created session");
      void this.refreshSessions();
      return { ok: true, session };
    } catch (err) {
      return { ok: false, error: errorMessage(err) };
    }
  }

  private update(patch: Partial<Snapshot>): void {
    this.snapshot = { ...this.snapshot, ...patch };
    for (const listener of this.listeners) listener();
  }

  private send(message: ClientMessage): void {
    const socket = this.socket;
    if (socket !== null && socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify(message));
    }
  }

  private connect(): void {
    if (this.retryTimer !== null) {
      clearTimeout(this.retryTimer);
      this.retryTimer = null;
    }
    if (this.socket !== null) return;
    const socket = new WebSocket(socketUrl());
    this.socket = socket;
    this.update({ status: "connecting" });
    socket.addEventListener("open", () => {
      if (this.socket !== socket) return;
      this.attempt = 0;
      this.update({ status: "open" });
      for (const id of this.subscribed) {
        this.send({ type: "subscribe", session: id, after_seq: this.received.get(id) ?? 0 });
      }
      void this.refreshSessions();
      void this.refreshUsage();
    });
    socket.addEventListener("message", (message) => {
      if (this.socket !== socket || typeof message.data !== "string") return;
      let parsed: ServerMessage;
      try {
        parsed = parseServerMessage(JSON.parse(message.data), "server message");
      } catch (err) {
        console.error(err);
        return;
      }
      this.onMessage(parsed);
    });
    socket.addEventListener("close", () => this.onClose(socket));
  }

  private onClose(socket: WebSocket): void {
    if (this.socket !== socket) return;
    this.socket = null;
    const lost = [...this.acks.values()];
    this.acks.clear();
    for (const ack of lost) {
      ack.reject(new Error("Connection lost before the server confirmed; check the transcript"));
    }
    this.update({ status: "closed" });
    const delay = Math.min(RETRY_BASE_MS * 2 ** this.attempt, RETRY_MAX_MS);
    this.attempt += 1;
    this.retryTimer = setTimeout(() => this.connect(), delay);
  }

  /** Idempotent: several wake signals for one resume cost nothing extra. */
  private wake(): void {
    if (document.visibilityState !== "visible") return;
    const hiddenFor = this.hiddenAt === null ? 0 : Date.now() - this.hiddenAt;
    this.hiddenAt = null;
    const socket = this.socket;
    if (socket === null) {
      this.connect();
    } else if (hiddenFor > STALE_AFTER_HIDDEN_MS && socket.readyState === WebSocket.OPEN) {
      socket.close();
      this.onClose(socket);
      this.connect();
    }
    this.flush();
  }

  private onMessage(message: ServerMessage): void {
    switch (message.type) {
      case "event": {
        const event = message.event;
        const last = this.received.get(event.session) ?? 0;
        if (event.seq <= last) return;
        this.received.set(event.session, event.seq);
        const queue = this.queued.get(event.session);
        if (queue === undefined) this.queued.set(event.session, [event]);
        else queue.push(event);
        this.scheduleFlush();
        return;
      }
      case "ack": {
        const ack = this.acks.get(message.client_id);
        if (ack === undefined) return;
        this.acks.delete(message.client_id);
        if (message.ok) ack.resolve();
        else ack.reject(ackError(message.error));
        return;
      }
      case "sessions_changed":
        void this.refreshSessions();
        return;
      case "usage_changed":
        void this.refreshUsage();
        return;
      case "error": {
        console.error("server error", message.message);
        if (message.session !== null) {
          const sessionErrors = new Map(this.snapshot.sessionErrors);
          sessionErrors.set(message.session, message.message);
          this.update({ sessionErrors });
        }
        return;
      }
    }
  }

  private scheduleFlush(): void {
    if (this.frame !== null) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = null;
      this.flush();
    });
  }

  private flush(): void {
    if (this.queued.size === 0) return;
    const transcripts = new Map(this.snapshot.transcripts);
    for (const [session, events] of this.queued) {
      transcripts.set(session, applyEvents(transcripts.get(session) ?? emptyTranscript, events));
    }
    this.queued.clear();
    this.update({ transcripts });
  }
}

let instance: Client | null = null;

export function getClient(): Client {
  if (instance === null) instance = new Client();
  return instance;
}
