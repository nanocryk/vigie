import { readFileSync } from "node:fs";
import path from "node:path";

import { describe, expect, it } from "vitest";

import { toolDiff } from "@/diff";
import {
  type Loaded,
  appendEvents,
  dropRequest,
  fold,
  nextRequest,
  openWindow,
  prependPage,
  requested,
  wholeHistory,
} from "@/history";
import {
  type Item,
  RELAY_ACK,
  type TranscriptState,
  applyEvents,
  askContext,
  emptyTranscript,
  parseTaskNotification,
  planFileName,
  quietWork,
  relayFailureText,
  restartLabel,
  toolDisplayName,
  toolSummary,
  toolTimeVisible,
  turnEndBubble,
  wakeStops,
  wakeText,
} from "@/transcript";
import { type WireEvent, isRecord, parseHostEvent } from "@/wire";

const SESSION = "00000000-0000-0000-0000-000000000001";
/** Event times are this plus the recording's `t` (ms). */
const RECORDING_START = Date.parse("2026-09-27T10:00:59.000Z");

/**
 * Turns a spike recording into the events the server would send: every `out`
 * line, plus the host events the session emits: `user_sent` for each message
 * written (unless `legacy`, history from before that event), and `ask_queued` /
 * `ask_resolved` around a `can_use_tool` request and its answer.
 */
function recordingEvents(name: string, legacy = false): WireEvent[] {
  const file = path.resolve(import.meta.dirname, "../../spike/runs", `${name}.ndjson`);
  const events: WireEvent[] = [];
  const queued = new Set<string>();
  let elapsed = 0;
  const push = (kind: "line" | "host", payload: unknown): void => {
    // The server's microsecond precision, to exercise its parsing.
    const time = new Date(RECORDING_START + elapsed).toISOString().replace("Z", "123Z");
    const base = { session: SESSION, seq: events.length + 1, time };
    events.push(
      kind === "line"
        ? { ...base, kind, payload }
        : { ...base, kind, payload: parseHostEvent(payload) },
    );
  };
  for (const text of readFileSync(file, "utf8").split("\n")) {
    if (text.trim() === "") continue;
    const step: unknown = JSON.parse(text);
    if (!isRecord(step) || !isRecord(step["msg"])) throw new Error(`bad step: ${text}`);
    const msg = step["msg"];
    if (typeof step["t"] === "number") elapsed = step["t"];
    if (step["dir"] === "out") {
      push("line", msg);
      const request = msg["request"];
      if (
        msg["type"] === "control_request" &&
        isRecord(request) &&
        request["subtype"] === "can_use_tool" &&
        typeof msg["request_id"] === "string"
      ) {
        queued.add(msg["request_id"]);
        push("host", {
          event: "ask_queued",
          request_id: msg["request_id"],
          tool_name: request["tool_name"],
          tool_use_id: request["tool_use_id"],
          agent_id: request["agent_id"],
        });
      }
    } else if (step["dir"] === "in" && msg["type"] === "user" && !legacy) {
      const message = msg["message"];
      const content = isRecord(message) ? message["content"] : undefined;
      if (typeof content !== "string") throw new Error(`bad user step: ${text}`);
      push("host", { event: "user_sent", text: content, client_id: `c${events.length}` });
    } else if (step["dir"] === "in" && msg["type"] === "control_response") {
      const response = msg["response"];
      const requestId = isRecord(response) ? response["request_id"] : undefined;
      if (typeof requestId === "string" && queued.delete(requestId)) {
        push("host", {
          event: "ask_resolved",
          request_id: requestId,
          resolution: { kind: "answered", answer: { kind: "allow" } },
        });
      }
    }
  }
  return events;
}

type Shape =
  | ["user" | "claude" | "thinking", string]
  | ["tool", string, string, string]
  | ["notice", string]
  | ["restart", string, string | null]
  | ["agent", string]
  | ["brief", string]
  | ["relay_failed", string]
  | ["task_end", string, string]
  | ["changes", number, number, number];

function shape(item: Item): Shape {
  switch (item.kind) {
    case "changes":
      return ["changes", item.files, item.added, item.removed];
    case "task_end":
      return ["task_end", item.status, item.summary];
    case "agent":
      return ["agent", item.name];
    case "brief":
      return ["brief", item.text];
    case "relay_failed":
      return ["relay_failed", item.reason];
    case "user":
    case "thinking":
      return [item.kind, item.text];
    case "claude":
      return [item.kind, item.text];
    case "tool":
      return ["tool", item.name, item.summary, item.status];
    case "notice":
      return ["notice", item.text];
    case "restart":
      return ["restart", restartLabel(item), item.seed];
  }
}

function deliveries(items: readonly Item[]): string[] {
  return items.flatMap((item) => (item.kind === "user" ? [item.delivery] : []));
}

function transcript(name: string) {
  return applyEvents(emptyTranscript, recordingEvents(name));
}

describe("transcript from recordings", () => {
  it("basic: two exchanges", () => {
    const state = transcript("basic");
    expect(state.items.map(shape)).toEqual([
      ["user", "Reply with exactly: ok"],
      ["claude", "ok"],
      ["user", "Reply with exactly: ok again"],
      ["claude", "ok again"],
    ]);
    expect(state.items.every((item) => item.kind !== "claude" || !item.streaming)).toBe(true);
    expect(state.asks).toEqual([]);
    expect(deliveries(state.items)).toEqual(["received", "received"]);
    expect(state.outbox).toEqual([]);
  });

  it("basic: the bubble comes from user_sent and is on host until the echo", () => {
    const events = recordingEvents("basic");
    const sent = events.findIndex(
      (event) => event.kind === "host" && event.payload.event === "user_sent",
    );
    const before = applyEvents(emptyTranscript, events.slice(0, sent + 1));
    const bubble = before.items.at(-1);
    expect(bubble).toMatchObject({
      kind: "user",
      text: "Reply with exactly: ok",
      delivery: "on_host",
      clientId: `c${sent}`,
    });
    const echo = events.findIndex(
      (event) =>
        event.kind === "line" && isRecord(event.payload) && event.payload["isReplay"] === true,
    );
    const pending = applyEvents(before, events.slice(sent + 1, echo));
    expect(deliveries(pending.items)).toEqual(["on_host"]);
    const received = applyEvents(pending, events.slice(echo, echo + 1));
    expect(received.items).toHaveLength(pending.items.length);
    expect(received.items.at(-1)).toMatchObject({ key: bubble?.key, delivery: "received" });
  });

  it("a message's attachments stay apart from its text, and its composed echo delivers it", () => {
    const files = ["C:\\up\\0a1b2c3d4e5f-shot.png"];
    const state = applyEvents(emptyTranscript, [
      {
        session: SESSION,
        seq: 1,
        time: "t",
        kind: "host",
        payload: { event: "user_sent", text: "look", attachments: files, client_id: "c1" },
      },
      {
        session: SESSION,
        seq: 2,
        time: "t",
        kind: "line",
        payload: {
          type: "user",
          message: { role: "user", content: "look\n\nAttachments:\n- C:\\up\\0a1b2c3d4e5f-shot.png" },
          parent_tool_use_id: null,
          session_id: "s",
          isReplay: true,
        },
      },
    ]);
    expect(state.items).toHaveLength(1);
    expect(state.items[0]).toMatchObject({
      kind: "user",
      text: "look",
      attachments: files,
      delivery: "received",
    });
  });

  it("claude_attached puts the files on the turn's last Claude text only", () => {
    const events = recordingEvents("cd");
    const files = ["C:\\p\\shot.png", "C:\\p\\notes.md"];
    const host = (seq: number, payload: unknown): WireEvent => ({
      session: SESSION,
      seq,
      time: "t",
      kind: "host",
      payload: parseHostEvent(payload),
    });
    const state = applyEvents(emptyTranscript, [
      ...events,
      host(events.length + 1, { event: "claude_attached", attachments: files }),
    ]);
    const replies = state.items.filter((item) => item.kind === "claude");
    expect(replies.map((item) => item.attachments)).toEqual([undefined, files]);
    expect(state.items.at(-1)).toMatchObject({ kind: "claude", attachments: files });

    // A turn with no Claude text since the user's message drops them.
    const next = applyEvents(state, [
      host(events.length + 2, { event: "user_sent", text: "again", client_id: "c9" }),
      host(events.length + 3, { event: "claude_attached", attachments: ["C:\\p\\other.png"] }),
    ]);
    expect(next.items.filter((item) => item.kind === "claude")).toEqual(replies);
    expect(next.items.at(-1)).toMatchObject({ kind: "user", text: "again" });
  });

  it("a set_attachments call is hidden", () => {
    const call = (id: string, name: string): WireEvent => ({
      session: SESSION,
      seq: Number(id),
      time: "t",
      kind: "line",
      payload: {
        type: "assistant",
        message: { id: `m${id}`, content: [{ type: "tool_use", id, name, input: { paths: [] } }] },
        parent_tool_use_id: null,
      },
    });
    const state = applyEvents(emptyTranscript, [
      call("1", "mcp__vigie2__set_attachments"),
      call("2", "Read"),
    ]);
    expect(state.items.map((item) => [item.kind === "tool" && item.name, item.hidden])).toEqual([
      ["mcp__vigie2__set_attachments", true],
      ["Read", undefined],
    ]);
  });

  it("cd: a command the CLI answers locally is marked handled", () => {
    const state = transcript("cd");
    expect(state.items.map(shape)).toEqual([
      ["user", "/cd C:\\Users\\user\\AppData\\Local\\Temp\\vigie2-spike\\spike-cd-target"],
      ["claude", expect.stringContaining("/cd isn't available in this environment.")],
      ["user", "Use the Bash tool to run exactly: pwd. Reply with its output."],
      ["tool", "Bash", "pwd", "ok"],
      ["thinking", ""],
      ["claude", expect.stringContaining("/tmp/vigie2-spike/cwd")],
    ]);
    expect(deliveries(state.items)).toEqual(["handled", "received"]);
    expect(state.outbox).toEqual([]);
  });

  it("legacy history without user_sent renders echoes as bubbles", () => {
    const legacy = applyEvents(emptyTranscript, recordingEvents("interrupt", true));
    const current = transcript("interrupt");
    expect(legacy.items.map(shape)).toEqual(current.items.map(shape));
    expect(deliveries(legacy.items)).toEqual(["received", "received", "received"]);
    expect(legacy.items.filter((item) => item.kind === "user" && item.clientId !== null)).toEqual(
      [],
    );
  });

  it("a message sent during a turn is not taken as handled by that turn's end", () => {
    const events = recordingEvents("basic");
    const echo = events.findIndex(
      (event) =>
        event.kind === "line" && isRecord(event.payload) && event.payload["isReplay"] === true,
    );
    const midTurn = applyEvents(emptyTranscript, events.slice(0, echo + 1));
    const queued = applyEvents(midTurn, [
      {
        session: SESSION,
        seq: midTurn.lastSeq + 1,
        time: "t",
        kind: "host",
        payload: { event: "user_sent", text: "queued", attachments: [], client_id: "q" },
      },
    ]);
    const firstResult = events.findIndex(
      (event) => event.kind === "line" && isRecord(event.payload) && event.payload["type"] === "result",
    );
    const shifted = events
      .slice(echo + 1, firstResult + 1)
      .map((event) => ({ ...event, seq: event.seq + 1 }));
    const ended = applyEvents(queued, shifted);
    const bubble = ended.items.find((item) => item.kind === "user" && item.clientId === "q");
    expect(bubble).toMatchObject({ delivery: "on_host" });
    const echoed = applyEvents(ended, [
      {
        session: SESSION,
        seq: ended.lastSeq + 1,
        time: "t",
        kind: "line",
        payload: { type: "user", message: { role: "user", content: "queued" }, isReplay: true },
      },
    ]);
    expect(echoed.items.find((item) => item.kind === "user" && item.clientId === "q")).toMatchObject(
      { delivery: "received" },
    );
  });

  it("permissions: tool rows paired with their results, asks queued then resolved", () => {
    const events = recordingEvents("permissions");
    const state = applyEvents(emptyTranscript, events);
    const writePath = "C:\\Users\\user\\AppData\\Local\\Temp\\vigie2-spike\\cwd\\denied.txt";
    expect(state.items.map(shape)).toEqual([
      ["user", "Use the Bash tool to run exactly: echo spike-allow. Then reply done."],
      ["tool", "Bash", "echo spike-allow", "ok"],
      ["thinking", ""],
      [
        "claude",
        "Output:\n```\nspike-allow\n```\ndone\n\nThe claude.ai Google Drive connector isn't authorized, so it won't work until you connect it in your claude.ai connector settings.",
      ],
      [
        "user",
        "Use the Write tool to create denied.txt containing x. If refused, reply refused and do not retry.",
      ],
      ["tool", "Write", writePath, "error"],
      ["claude", "refused"],
      [
        "user",
        "Call the AskUserQuestion tool once, with the single question 'Pick a color?' and the options red and blue. Then reply with the answer you got.",
      ],
      ["tool", "AskUserQuestion", "1 question", "ok"],
      ["claude", "You answered blue."],
    ]);
    const bash = state.items[1];
    expect(bash?.kind === "tool" ? bash.output : null).toBe("spike-allow");
    const write = state.items[5];
    expect(write?.kind === "tool" ? write.output : null).toBe("Refused by the spike driver.");
    expect(state.asks).toEqual([]);

    const firstAsk = events.findIndex(
      (event) => event.kind === "host" && event.payload.event === "ask_queued",
    );
    const pending = applyEvents(emptyTranscript, events.slice(0, firstAsk + 1));
    expect(pending.asks).toEqual([
      {
        requestId: "034ae61a-6aad-4d34-8525-060c237cd8b9",
        toolName: "Write",
        toolUseId: "toolu_01MNqvkhhFftiww8c6Yao818",
        input: { file_path: writePath, content: "x" },
        danger: { dangerous: false, reasons: [] },
        acceptEdits: false,
        folder: null,
        file: null,
        plan: null,
        agentId: null,
      },
    ]);
  });

  it("interrupt: partial text kept, interrupts noted, no error notice", () => {
    const state = transcript("interrupt");
    const numbers = Array.from({ length: 83 }, (_, index) => String(index + 1)).join("\n");
    expect(state.items.map(shape)).toEqual([
      ["user", "Write the numbers from 1 to 400, one per line, nothing else."],
      ["thinking", ""],
      ["claude", numbers],
      ["notice", "Request interrupted by user"],
      [
        "user",
        'Use the Bash tool to run exactly: python -c "import time; time.sleep(60)". Run it in the foreground. Then reply done.',
      ],
      ["tool", "Bash", 'python -c "import time; time.sleep(60)"', "error"],
      ["notice", "Request interrupted by user for tool use"],
      ["user", "Reply with exactly: ok"],
      ["claude", "ok"],
    ]);
    expect(state.asks).toEqual([]);
  });
});

describe("times", () => {
  const at = (t: number) => RECORDING_START + t;

  it("come from user_sent, the first text, and the tool call and result lines", () => {
    const state = transcript("permissions");
    const [user, bash, , output] = state.items;
    expect(user).toMatchObject({ kind: "user", time: at(874) });
    expect(bash).toMatchObject({ kind: "tool", name: "Bash", time: at(2835), endTime: at(4872) });
    // Streamed: the bubble dates from its first delta, before the final assistant line.
    expect(output?.kind).toBe("claude");
    expect(output?.time).toBeGreaterThan(at(4872));
    expect(output?.time).toBeLessThan(at(6849));
  });

  it("legacy history dates the user bubble from its echo", () => {
    const legacy = applyEvents(emptyTranscript, recordingEvents("interrupt", true));
    expect(legacy.items[0]).toMatchObject({ kind: "user", time: at(1757) });
  });

  it("a tool row shows its time only when the minute changes from the item above", () => {
    const minute = Date.parse("2026-09-27T10:05:00Z");
    const item = (time: number | null, kind: "tool" | "notice" = "tool"): Item =>
      kind === "tool"
        ? {
            kind,
            key: String(time),
            time,
            seq: 0,
            messageId: null,
            toolUseId: "t",
            name: "Bash",
            summary: "",
            input: {},
            status: "ok",
            output: null,
            result: undefined,
            endTime: null,
            attached: null,
          }
        : { kind, key: String(time), time, seq: 0, tone: "info", text: "", detail: null };
    const items = [
      item(minute + 5_000),
      item(minute + 50_000),
      item(minute + 61_000, "notice"),
      item(minute + 62_000),
      item(minute + 130_000),
      item(null),
    ];
    expect(items.map((_, index) => toolTimeVisible(items, index))).toEqual([
      true,
      false,
      true,
      false,
      true,
      false,
    ]);
  });
});

describe("permission mode", () => {
  const hostMode = (seq: number, mode: string): WireEvent => ({
    session: SESSION,
    seq,
    time: "t",
    kind: "host",
    payload: { event: "permission_mode", mode },
  });

  it("follows permission_mode host events", () => {
    expect(emptyTranscript.permissionMode).toBeNull();
    const state = applyEvents(emptyTranscript, [hostMode(1, "default"), hostMode(2, "acceptEdits")]);
    expect(state.permissionMode).toBe("acceptEdits");
  });

  it("falls back to system init and status lines in older history", () => {
    const events = recordingEvents("plan", true);
    const firstInit = events.findIndex(
      (event) =>
        event.kind === "line" &&
        isRecord(event.payload) &&
        event.payload["type"] === "system" &&
        event.payload["subtype"] === "init",
    );
    expect(applyEvents(emptyTranscript, events.slice(0, firstInit + 1)).permissionMode).toBe("plan");
    // Approving the plan puts the CLI back in default mode, reported by a status line.
    expect(applyEvents(emptyTranscript, events).permissionMode).toBe("default");
  });

  it("a tool row keeps the CLI's structured result, which a Write diff reads", () => {
    const writes = transcript("plan").items.filter(
      (item) => item.kind === "tool" && item.name === "Write",
    );
    const hello = writes.at(-1);
    if (hello?.kind !== "tool") throw new Error("no Write row");
    expect(hello.result).toMatchObject({ type: "create", content: "hi", structuredPatch: [] });
    const diff = toolDiff({ ...hello, path: hello.summary });
    expect(diff?.verb).toBe("Write");
    expect(diff?.hunks).toEqual([[{ kind: "added", text: "hi", number: 1 }]]);
    const denied = transcript("permissions").items.find(
      (item) => item.kind === "tool" && item.name === "Write",
    );
    if (denied?.kind !== "tool") throw new Error("no denied Write row");
    expect(toolDiff({ ...denied, path: denied.summary })).toBeNull();
  });
});

describe("synthetic user lines", () => {
  const line = (seq: number, payload: unknown): WireEvent => ({
    session: SESSION,
    seq,
    time: "t",
    kind: "line",
    payload,
  });
  const synthetic = (seq: number, text: string): WireEvent =>
    line(seq, {
      type: "user",
      message: { role: "user", content: [{ type: "text", text }] },
      parent_tool_use_id: null,
      isSynthetic: true,
    });
  const skillCall = [
    line(1, {
      type: "assistant",
      message: {
        id: "m1",
        role: "assistant",
        content: [{ type: "tool_use", id: "s1", name: "Skill", input: { skill: "rust-dev" } }],
      },
      parent_tool_use_id: null,
    }),
    line(2, {
      type: "user",
      message: {
        role: "user",
        content: [{ type: "tool_result", tool_use_id: "s1", content: "Launching skill: rust-dev" }],
      },
      parent_tool_use_id: null,
    }),
  ];

  it("a skill's content folds into its Skill call", () => {
    const state = applyEvents(emptyTranscript, [...skillCall, synthetic(3, "## Rules")]);
    expect(state.items.map(shape)).toEqual([["tool", "Skill", expect.any(String), "ok"]]);
    expect(state.items[0]).toMatchObject({ kind: "tool", attached: "## Rules" });
  });

  it("without a Skill call to fold into, it is a system message", () => {
    const state = applyEvents(emptyTranscript, [
      ...skillCall,
      synthetic(3, "## Rules"),
      synthetic(4, "more"),
    ]);
    expect(state.items.map(shape)).toEqual([
      ["tool", "Skill", expect.any(String), "ok"],
      ["notice", "System message"],
    ]);
    expect(state.items[1]).toMatchObject({ detail: "more" });
  });
});

describe("restarts", () => {
  const SEED = "This is a new session. The previous vigie2 session in /p.\n\n## Exchange 1\nUser: hi";
  const event = (seq: number, kind: "host" | "line", payload: unknown): WireEvent =>
    kind === "host"
      ? { session: SESSION, seq, time: "t", kind, payload: parseHostEvent(payload) }
      : { session: SESSION, seq, time: "t", kind, payload };
  const echo = (seq: number, text: string): WireEvent =>
    event(seq, "line", {
      type: "user",
      message: { role: "user", content: text },
      parent_tool_use_id: null,
      isReplay: true,
    });
  const restarted = (seq: number, seed: string | null): WireEvent =>
    event(seq, "host", {
      event: "restarted",
      mode: seed === null ? "blank" : "continue",
      exchanges: seed === null ? 0 : 1,
      total_exchanges: 1,
      ...(seed === null ? {} : { seed }),
    });

  it("a continue shows a divider holding the seed, and the typed message alone", () => {
    const events = [
      restarted(1, SEED),
      event(2, "host", { event: "started", cwd: "/p" }),
      event(3, "host", { event: "user_sent", text: "go on", client_id: "c1" }),
      echo(4, `${SEED}\n\ngo on`),
    ];
    const state = applyEvents(emptyTranscript, events);
    expect(state.items.map(shape)).toEqual([
      ["restart", "Restarted: 1 exchange given as context", SEED],
      ["user", "go on"],
    ]);
    expect(deliveries(state.items)).toEqual(["received"]);
    expect(state.pendingSeed).toBeNull();
    const replayed = events.reduce((acc, one) => applyEvents(acc, [one]), emptyTranscript);
    expect(replayed.items.map(shape)).toEqual(state.items.map(shape));
  });

  it("a continue without a typed message shows no bubble for its closing line", () => {
    const state = applyEvents(emptyTranscript, [
      restarted(1, SEED),
      echo(2, `${SEED}\n\nThe user has not typed a request yet: summarize where things stand and ask what to do next.`),
      echo(3, "later"),
    ]);
    expect(state.items.map(shape)).toEqual([
      ["restart", "Restarted: 1 exchange given as context", SEED],
      ["user", "later"],
    ]);
  });

  it("a blank restart shows a divider without seed", () => {
    const state = applyEvents(emptyTranscript, [restarted(1, null), echo(2, "hello")]);
    expect(state.items.map(shape)).toEqual([
      ["restart", "Restarted: blank", null],
      ["user", "hello"],
    ]);
    expect(restartLabel({ mode: "continue", exchanges: 5 })).toBe(
      "Restarted: 5 exchanges given as context",
    );
  });

  it("a stop a client asked for says why, not how", () => {
    const exited = (seq: number, reason?: string): WireEvent =>
      event(seq, "host", {
        event: "child_exited",
        code: 0,
        stderr_tail: [],
        ...(reason === undefined ? {} : { reason }),
      });
    const state = applyEvents(emptyTranscript, [
      exited(1, "revoke"),
      exited(2, "kill"),
      exited(3),
    ]);
    expect(state.items.map(shape)).toEqual([
      ["notice", "Grants revoked"],
      ["notice", "Stopped from Home"],
      ["notice", "Claude process ended (code 0)"],
    ]);

    const shutdown = applyEvents(emptyTranscript, [
      event(1, "host", { event: "stopping", reason: "shutdown" }),
      echo(2, "[Request interrupted by user for tool use]"),
      exited(3, "shutdown"),
      echo(4, "[Request interrupted by user]"),
    ]);
    expect(shutdown.items.map(shape)).toEqual([
      ["notice", "Server stopped"],
      ["notice", "Request interrupted by user"],
    ]);
  });

  it("an ask offers its file, and an auto-allowed one shows a row", () => {
    const file = { kind: "edit", path: "C:\\work\\notes\\plan.md" };
    const state = applyEvents(emptyTranscript, [
      event(1, "host", { event: "started", cwd: "C:\\work" }),
      event(2, "host", {
        event: "ask_queued",
        request_id: "r1",
        tool_name: "Edit",
        tool_use_id: "t1",
        file,
      }),
      event(3, "host", {
        event: "auto_allowed",
        request_id: "r2",
        tool_name: "Write",
        tool_use_id: "t2",
        file,
      }),
    ]);
    expect(state.asks.map((ask) => ask.file)).toEqual([file]);
    expect(state.items.map(shape)).toEqual([["notice", "Auto-allowed: Write notes\\plan.md"]]);
  });

  it("a clear shows a Cleared divider", () => {
    const state = applyEvents(emptyTranscript, [
      event(1, "host", { event: "restarted", mode: "clear", exchanges: 0, total_exchanges: 3 }),
    ]);
    expect(state.items.map(shape)).toEqual([["restart", "Cleared", null]]);
  });
});

describe("catalog", () => {
  const host = (seq: number, payload: unknown): WireEvent => ({
    session: SESSION,
    seq,
    time: "t",
    kind: "host",
    payload: parseHostEvent(payload),
  });

  it("keeps the latest catalog and the terminal-only commands of the latest init", () => {
    expect(emptyTranscript.catalog).toBeNull();
    const first = host(1, {
      event: "catalog",
      models: [
        {
          value: "opus",
          resolved_model: "claude-opus-5-5",
          display_name: "Opus 5.5",
          effort_levels: ["low", 3],
        },
        { bad: 1 },
      ],
      commands: [{ name: "canif", description: "Use canif." }],
    });
    const state = applyEvents(emptyTranscript, [first]);
    expect(state.catalog).toEqual({
      models: [
        {
          value: "opus",
          resolved_model: "claude-opus-5-5",
          display_name: "Opus 5.5",
          description: null,
          effort_levels: ["low"],
        },
      ],
      commands: [{ name: "canif", description: "Use canif.", argument_hint: "" }],
    });
    const second = host(2, { event: "catalog", models: [], commands: [] });
    const init: WireEvent = {
      session: SESSION,
      seq: 3,
      time: "t",
      kind: "line",
      payload: { type: "system", subtype: "init", terminal_slash_commands: ["doctor", "color"] },
    };
    const later = applyEvents(state, [second, init]);
    expect(later.catalog).toEqual({ models: [], commands: [] });
    expect(later.terminalCommands).toEqual(["doctor", "color"]);
  });

  it("reads the recorded init's terminal-only commands", () => {
    expect(transcript("basic").terminalCommands).toContain("doctor");
  });
});

describe("streaming", () => {
  const events = recordingEvents("interrupt");
  const finalIndex = events.findIndex(
    (event) =>
      event.kind === "line" &&
      isRecord(event.payload) &&
      event.payload["type"] === "assistant" &&
      JSON.stringify(event.payload).includes('"type":"text","text":"1\\n2'),
  );

  it("builds a live bubble from deltas that the assistant line replaces in place", () => {
    expect(finalIndex).toBeGreaterThan(0);
    const before = applyEvents(emptyTranscript, events.slice(0, finalIndex));
    const live = before.items.at(-1);
    expect(live?.kind).toBe("claude");
    expect(live?.kind === "claude" && live.streaming).toBe(true);
    const after = applyEvents(before, events.slice(finalIndex, finalIndex + 1));
    expect(after.items).toHaveLength(before.items.length);
    const final = after.items.at(-1);
    expect(final?.kind === "claude" && !final.streaming).toBe(true);
    expect(final?.key).toBe(live?.key);
  });

  it("is the same applied in one batch or event by event", () => {
    const stepwise = events.reduce((state, event) => applyEvents(state, [event]), emptyTranscript);
    expect(stepwise.items).toEqual(applyEvents(emptyTranscript, events).items);
  });

  it("skips events it already holds", () => {
    const state = applyEvents(emptyTranscript, events);
    expect(applyEvents(state, events.slice(0, 20)).items).toBe(state.items);
  });
});

describe("paged history", () => {
  const SEED = "This is a new session. The previous vigie2 session in /p.";
  const host = (payload: unknown): WireEvent => ({
    session: SESSION,
    seq: 0,
    time: "t",
    kind: "host",
    payload: parseHostEvent(payload),
  });
  const started = host({ event: "started", cwd: "C:\\Users\\user\\AppData\\Local\\Temp" });

  /** Recordings played one after the other, with the state events a discussion collects. */
  function discussion(): WireEvent[] {
    const events = [
      started,
      host({ event: "catalog", models: [], commands: [{ name: "one" }] }),
      host({ event: "permission_mode", mode: "default" }),
      ...recordingEvents("basic"),
      host({ event: "permission_mode", mode: "plan" }),
      ...recordingEvents("permissions"),
      host({ event: "restarted", mode: "continue", exchanges: 1, total_exchanges: 3, seed: SEED }),
      started,
      host({ event: "catalog", models: [], commands: [{ name: "two" }] }),
      host({ event: "user_sent", text: "go on", client_id: "g" }),
      {
        session: SESSION,
        seq: 0,
        time: "t",
        kind: "line",
        payload: { type: "user", message: { role: "user", content: `${SEED}\n\ngo on` } },
      } satisfies WireEvent,
      ...recordingEvents("interrupt"),
      ...recordingEvents("cd"),
    ];
    return events.map((event, index) => ({ ...event, seq: index + 1 }));
  }

  const isHost = (event: WireEvent, name: string): boolean =>
    event.kind === "host" && event.payload.event === name;

  /** What the server sends for the last `exchanges` exchanges below `before`. */
  function serverPage(all: readonly WireEvent[], before: number, exchanges: number) {
    const sent = all.filter((event) => event.seq < before && isHost(event, "user_sent"));
    const firstSeq = sent.at(-exchanges)?.seq ?? 1;
    const prelude = ["started", "permission_mode", "catalog", "restarted"]
      .map((name) => all.filter((event) => event.seq < firstSeq && isHost(event, name)).at(-1))
      .filter((event) => event !== undefined)
      .sort((left, right) => left.seq - right.seq);
    const events = all.filter((event) => event.seq >= firstSeq && event.seq < before);
    return { firstSeq, startReached: firstSeq <= 1, events: [...prelude, ...events] };
  }

  const shownFrom = (state: TranscriptState, firstSeq: number): Item[] =>
    state.items.filter((item) => Number.parseInt(item.key, 10) >= firstSeq);

  it("a window and every page before it fold as the whole history does", () => {
    const all = discussion();
    const full = applyEvents(emptyTranscript, all);
    const end = (all.at(-1)?.seq ?? 0) + 1;
    const window = serverPage(all, end, 2);
    let loaded = appendEvents(openWindow(window.firstSeq, window.startReached), window.events);
    const opened = fold(loaded);
    expect(opened.items).toEqual(shownFrom(full, window.firstSeq));
    for (const key of [
      "asks",
      "outbox",
      "turnOpen",
      "interrupted",
      "stopping",
      "cwd",
      "permissionMode",
      "pendingSeed",
      "catalog",
      "terminalCommands",
      "lastSeq",
    ] as const) {
      expect(opened[key], key).toEqual(full[key]);
    }

    let pages = 0;
    for (let before = nextRequest(loaded); before !== null; before = nextRequest(loaded)) {
      loaded = requested(loaded);
      expect(nextRequest(loaded)).toBeNull();
      const page = serverPage(all, before, 2);
      loaded = prependPage(loaded, page);
      pages += 1;
      expect(fold(loaded).items).toEqual(shownFrom(full, page.firstSeq));
    }
    expect(pages).toBeGreaterThan(1);
    expect(loaded.beginning).toBe("loaded");
    expect({ ...fold(loaded), firstSeq: 0 }).toEqual(full);
  });

  it("the prelude sets state without showing its restart divider", () => {
    const all = discussion();
    const sent = all.find(
      (event) =>
        event.kind === "host" && event.payload.event === "user_sent" && event.payload.text === "go on",
    );
    const firstSeq = sent?.seq ?? 0;
    // The message and its echo, carrying the seed of the restart before them.
    const page = serverPage(all, firstSeq + 2, 1);
    expect(page.firstSeq).toBe(firstSeq);
    const state = fold(appendEvents(openWindow(page.firstSeq, false), page.events));
    expect(state.items.map(shape)[0]).toEqual(["user", "go on"]);
    expect(state.items.some((item) => item.kind === "restart")).toBe(false);
    expect(deliveries(state.items)).toEqual(["received"]);
    expect(state.catalog?.commands.map((command) => command.name)).toEqual(["two"]);
  });

  it("asks one page at a time, and a stale page only ends the request", () => {
    let loaded: Loaded = openWindow(40, false);
    expect(nextRequest(loaded)).toBe(40);
    loaded = requested(loaded);
    expect(nextRequest(loaded)).toBeNull();
    expect(prependPage(loaded, { firstSeq: 40, startReached: false, events: [] })).toEqual({
      ...loaded,
      request: "idle",
    });
    expect(dropRequest(loaded).request).toBe("idle");
    const reached = prependPage(loaded, { firstSeq: 1, startReached: true, events: [] });
    expect(reached).toMatchObject({ firstSeq: 1, beginning: "loaded", request: "idle" });
    expect(nextRequest(reached)).toBeNull();
    expect(nextRequest(wholeHistory)).toBeNull();
  });
});

describe("subagents", () => {
  const SLEEPER = "toolu_012QcBdgY7ftdrny3oJtHMt5";
  const STOPPER = "toolu_015G4gKRa122iYWaHqcCUYmB";
  const SLEEPER_TASK = "a26cdafd895afd968";
  const SLEEPER_BRIEF =
    'Use the Bash tool to run exactly: python -c "import time; time.sleep(20)". Then reply with the single word DONE, unless a message told you another word; then reply with that word instead.';
  const scope = (state: TranscriptState, id: string): Item[] => [
    ...(state.scopes.get(id)?.items ?? []),
  ];

  it("each agent's lines fold into its own discussion, the main one shows its creation line", () => {
    const state = transcript("agents");
    const main = state.items.map(shape);
    expect(main).toContainEqual(["agent", "sleeper"]);
    expect(main).toContainEqual(["agent", "stopper"]);
    expect(main.filter((item) => item[0] === "tool" && item[1] === "Bash")).toEqual([]);
    expect(main).not.toContainEqual(["claude", "PINEAPPLE"]);
    expect(scope(state, SLEEPER).map(shape)).toEqual([
      ["brief", SLEEPER_BRIEF],
      ["tool", "Bash", 'python -c "import time; time.sleep(20)"', "ok"],
      ["thinking", ""],
      ["claude", "PINEAPPLE"],
      ["notice", "Agent finished"],
    ]);
    const stopper = scope(state, STOPPER).map(shape);
    expect(stopper[0]?.[0]).toBe("brief");
    expect(stopper).toContainEqual(["notice", "Agent stopped"]);
    expect(stopper.filter((item) => item[0] === "tool")).toHaveLength(1);
    expect([...state.agents.values()]).toEqual([
      {
        toolUseId: SLEEPER,
        taskId: SLEEPER_TASK,
        name: "sleeper",
        parentToolUseId: null,
        state: "completed",
        lastActive: Date.parse("2026-09-27T10:01:28.724Z"),
      },
      {
        toolUseId: STOPPER,
        taskId: "a053c040a86fec5a7",
        name: "stopper",
        parentToolUseId: null,
        state: "stopped",
        lastActive: Date.parse("2026-09-27T10:02:12.681Z"),
      },
    ]);
  });

  it("an agent's ask names it and stays on the shared ask list", () => {
    const events = recordingEvents("agents");
    const firstAsk = events.findIndex(
      (event) => event.kind === "host" && event.payload.event === "ask_queued",
    );
    const pending = applyEvents(emptyTranscript, events.slice(0, firstAsk + 1));
    expect(pending.asks.map((ask) => [ask.toolName, ask.agentId])).toEqual([
      ["Bash", SLEEPER_TASK],
    ]);
    expect(pending.scopes.get(SLEEPER)?.asks).toEqual([]);
  });

  it("folds the same over pages", () => {
    const events = recordingEvents("agents");
    const full = applyEvents(emptyTranscript, events);
    const stepwise = events.reduce((state, event) => applyEvents(state, [event]), emptyTranscript);
    expect(stepwise).toEqual(full);
  });

  const line = (seq: number, payload: unknown): WireEvent => ({
    session: SESSION,
    seq,
    time: "t",
    kind: "line",
    payload,
  });
  const host = (seq: number, payload: unknown): WireEvent => ({
    session: SESSION,
    seq,
    time: "t",
    kind: "host",
    payload: parseHostEvent(payload),
  });
  const started = (seq: number): WireEvent =>
    line(seq, {
      type: "system",
      subtype: "task_started",
      task_id: "a1",
      tool_use_id: "t1",
      task_type: "local_agent",
      description: "sleeper",
      prompt: "Sleep.",
    });
  const WRAPPED = "(vigie2 relay) ... The text:\n\nSay PINEAPPLE.";

  function relayed(sendMessageTo: string, reply: string, outcome: unknown): TranscriptState {
    return applyEvents(emptyTranscript, [
      started(1),
      host(2, {
        event: "relay_sent",
        text: "Say PINEAPPLE.",
        client_id: "c1",
        task_id: "a1",
        agent_tool_use_id: "t1",
        name: "sleeper",
        wrapped: WRAPPED,
      }),
      line(3, { type: "user", message: { role: "user", content: WRAPPED }, isReplay: true }),
      line(4, {
        type: "assistant",
        message: {
          id: "m1",
          content: [
            {
              type: "tool_use",
              id: "s1",
              name: "SendMessage",
              input: { to: sendMessageTo, message: "Say PINEAPPLE." },
            },
          ],
        },
      }),
      line(5, { type: "assistant", message: { id: "m2", content: [{ type: "text", text: reply }] } }),
      line(6, { type: "result", subtype: "success", is_error: false }),
      host(7, {
        event: "relay_checked",
        relay_seq: 2,
        task_id: "a1",
        agent_tool_use_id: "t1",
        outcome,
      }),
    ]);
  }

  it("a faithful relay is hidden from the main discussion and shows in the agent's view", () => {
    const state = relayed("a1", RELAY_ACK, { kind: "faithful", send_message: "s1" });
    expect(state.items.filter((item) => item.hidden !== true)).toEqual([]);
    expect(state.items.map((item) => item.kind)).toEqual(["user", "tool", "claude"]);
    const view = scope(state, "t1");
    expect(view.map(shape)).toEqual([
      ["brief", "Sleep."],
      ["user", "Say PINEAPPLE."],
    ]);
    expect(view[1]).toMatchObject({ relayed: true, delivery: "received", clientId: "c1" });
  });

  it("a failed relay stays in the main discussion and warns in the agent's view", () => {
    const state = relayed("a2", "Done.", { kind: "failed", reason: "other_recipient" });
    expect(state.items.filter((item) => item.hidden === true)).toEqual([]);
    expect(state.items.map(shape)).toEqual([
      ["user", WRAPPED],
      ["tool", "SendMessage", "to a2 · message Say PINEAPPLE.", "running"],
      ["claude", "Done."],
    ]);
    expect(scope(state, "t1").map(shape)).toEqual([
      ["brief", "Sleep."],
      ["user", "Say PINEAPPLE."],
      ["relay_failed", "other_recipient"],
    ]);
    expect(relayFailureText("other_recipient")).toContain("See the main discussion");
  });

  it("a synthetic brief line replaces the brief of task_started, a later one does not", () => {
    const brief = (seq: number, text: string): WireEvent =>
      line(seq, {
        type: "user",
        parent_tool_use_id: "t1",
        isSynthetic: true,
        message: { role: "user", content: [{ type: "text", text }] },
      });
    const state = applyEvents(emptyTranscript, [
      started(1),
      brief(2, "Sleep, in full."),
      line(3, {
        type: "assistant",
        parent_tool_use_id: "t1",
        message: { id: "m1", content: [{ type: "text", text: "Sleeping." }] },
      }),
      brief(4, "Skill text"),
    ]);
    expect(scope(state, "t1").map(shape)).toEqual([
      ["brief", "Sleep, in full."],
      ["claude", "Sleeping."],
      ["notice", "System message"],
    ]);
  });

  it("an auto-allowed ask of an agent shows in its view, and a process exit stops its agents", () => {
    const state = applyEvents(emptyTranscript, [
      started(1),
      host(2, {
        event: "auto_allowed",
        request_id: "r",
        tool_name: "Read",
        tool_use_id: "t9",
        file: { kind: "read", path: "/a.md" },
        agent_id: "a1",
      }),
      host(3, { event: "child_exited", code: 0, stderr_tail: [] }),
    ]);
    expect(scope(state, "t1").map(shape)).toEqual([
      ["brief", "Sleep."],
      ["notice", "Auto-allowed: Read /a.md"],
      ["notice", "Agent stopped"],
    ]);
    expect(state.agents.get("t1")?.state).toBe("stopped");
  });
});

describe("plan asks", () => {
  it("take the host's plan, else the one an older CLI put in the input", () => {
    const events = recordingEvents("plan", true);
    const queued = events.findIndex(
      (event) => event.kind === "host" && event.payload.event === "ask_queued",
    );
    const state = applyEvents(emptyTranscript, events.slice(0, queued + 1));
    expect(state.asks.map((ask) => ask.plan)).toEqual([
      {
        path: "C:\\Users\\user\\.claude\\plans\\plan-creating-hello-txt-containing-tidy-aurora.md",
        text: "Create `hello.txt` in the working dir with content `hi`.\nVerify by reading it back.\n",
        previous: null,
      },
    ]);

    const plan = { path: "C:\\p\\a.md", text: "# Two", previous: "# One" };
    const hosted = applyEvents(emptyTranscript, [
      {
        session: SESSION,
        seq: 1,
        time: "t",
        kind: "host",
        payload: parseHostEvent({
          event: "ask_queued",
          request_id: "r",
          tool_name: "ExitPlanMode",
          tool_use_id: "t",
          plan,
        }),
      },
    ]);
    expect(hosted.asks.map((ask) => ask.plan)).toEqual([plan]);
  });

  it("name the plan file an edit targets", () => {
    expect(planFileName({ file_path: "C:\\Users\\u\\.claude\\plans\\tidy-aurora.md" })).toBe(
      "tidy-aurora.md",
    );
    expect(planFileName({ file_path: "/home/u/.claude/plans/a.md" })).toBe("a.md");
    expect(planFileName({ file_path: "/home/u/.claude/plans/deep/a.md" })).toBeNull();
    expect(planFileName({ file_path: "/work/plans/a.md" })).toBeNull();
    expect(planFileName({})).toBeNull();
  });
});

describe("toolDisplayName", () => {
  it("titles an MCP tool and names its server", () => {
    expect(toolDisplayName("mcp__vigie2__bash_asks")).toBe("Bash Asks (vigie2)");
    expect(toolDisplayName("mcp__claude-in-chrome__read_page")).toBe("Read Page (claude-in-chrome)");
  });

  it("leaves built-in and malformed names unchanged", () => {
    expect(toolDisplayName("Bash")).toBe("Bash");
    expect(toolDisplayName("mcp__vigie2")).toBe("mcp__vigie2");
    expect(toolDisplayName("mcp____tool")).toBe("mcp____tool");
    expect(toolDisplayName("mcp__vigie2__")).toBe("mcp__vigie2__");
  });
});

describe("toolSummary", () => {
  it("picks the field that says what the call does", () => {
    expect(toolSummary("Bash", { command: "ls -la" })).toBe("ls -la");
    expect(toolSummary("Read", { file_path: "/a/b.ts" })).toBe("/a/b.ts");
    expect(toolSummary("Glob", { pattern: "**/*.rs" })).toBe("**/*.rs");
    expect(toolSummary("Other", { a: 1, b: "two  words", c: [1, 2], d: { e: 1 } })).toBe(
      "a 1 · b two words · c [1, 2] · d {...}",
    );
    expect(toolSummary("Other", { ids: [1, 2, 3, 4, 5, 6, 7], nested: [[1], { a: 1 }, "x"] })).toBe(
      "ids [1, 2, 3, 4, 5; and 2 more...] · nested [[...], {...}, x]",
    );
    expect(toolSummary("EnterPlanMode", {})).toBe("");
    expect(toolSummary("ToolSearch", { query: "select:A,B", max_results: 2 })).toBe("select:A,B");
    expect(toolSummary("ExitPlanMode", { plan: "# Dummy plan\n\n## Context" })).toBe("Dummy plan");
    expect(
      toolSummary("AskUserQuestion", { questions: [{ question: "Color?" }, { question: "Fruits?" }] }),
    ).toBe("2 questions");
    expect(toolSummary("AskUserQuestion", { questions: [{ question: "Color?" }] })).toBe(
      "1 question",
    );
    expect(toolSummary("Grep", { pattern: "fn main", path: "src" })).toBe("fn main in src");
    const cwd = "E:\\work\\vigie2";
    expect(toolSummary("Edit", { file_path: "E:\\work\\vigie2\\src\\main.rs" }, cwd)).toBe(
      "src\\main.rs",
    );
    expect(toolSummary("Read", { file_path: "e:/work/vigie2/web/a.ts" }, cwd)).toBe("web/a.ts");
    expect(toolSummary("Write", { file_path: "E:\\work\\vigie2x\\a.rs" }, cwd)).toBe(
      "E:\\work\\vigie2x\\a.rs",
    );
    expect(toolSummary("Read", { file_path: "C:\\Users\\me\\.claude\\plans\\p.md" }, cwd)).toBe(
      "C:\\Users\\me\\.claude\\plans\\p.md",
    );
    expect(toolSummary("Grep", { pattern: "x", path: "E:\\work\\vigie2\\src" }, cwd)).toBe(
      "x in src",
    );
    expect(toolSummary("Read", { file_path: "/home/me/p/a.ts" }, "/home/me/p/")).toBe("a.ts");
    expect(toolSummary("Bash", { command: "npm test", run_in_background: true })).toBe(
      "npm test (background)",
    );
  });
});

describe("task notifications", () => {
  const notification = (status: string, extra = "") =>
    `<task-notification>\n<task-id>b1</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n${extra}<status>${status}</status>\n<summary>Background command "sleep 5" ${status}</summary>\n</task-notification>`;
  const line = (seq: number, text: string, origin: unknown): WireEvent => ({
    session: SESSION,
    seq,
    time: "t",
    kind: "line",
    payload: {
      type: "user",
      message: { role: "user", content: text },
      parent_tool_use_id: null,
      isReplay: true,
      origin,
    },
  });

  it("shows a notification as a background row, never as the user's message", () => {
    const state = applyEvents(emptyTranscript, [
      line(1, notification("completed", "<output-file>C:\\t\\b1.output</output-file>\n"), {
        kind: "task-notification",
      }),
      line(2, notification("killed"), { kind: "task-notification" }),
    ]);
    expect(state.items.map(shape)).toEqual([
      ["task_end", "ok", 'Background command "sleep 5" completed'],
      ["task_end", "stopped", 'Background command "sleep 5" killed'],
    ]);
    expect(state.items[0]).toMatchObject({ outputFile: "C:\\t\\b1.output" });
    expect(state.items[1]).toMatchObject({ outputFile: null });
  });

  it("falls back to a system message when the notification cannot be read", () => {
    const state = applyEvents(emptyTranscript, [
      line(1, "<task-notification>garbled", { kind: "task-notification" }),
    ]);
    expect(state.items.map(shape)).toEqual([["notice", "System message"]]);
  });

  it("reads back the stopped tasks a wake message names", () => {
    expect(wakeStops(wakeText(["a"]))).toEqual(["a"]);
    expect(wakeStops(wakeText(["a", "b c"]))).toEqual(["a", "b c"]);
    expect(wakeStops("Stopped the build")).toBeNull();
  });

  it("maps failures to errors", () => {
    expect(parseTaskNotification(notification("failed"))?.status).toBe("error");
    expect(parseTaskNotification("hello")).toBeNull();
  });
});

describe("quietWork", () => {
  const user: Item = {
    kind: "user",
    key: "u",
    text: "go",
    delivery: "handled",
    clientId: null,
    time: null,
    seq: 0,
  };
  const tool = (status: "running" | "ok"): Item => ({
    kind: "tool",
    key: "t",
    messageId: null,
    toolUseId: "t",
    name: "Bash",
    summary: "ls",
    input: {},
    status,
    output: null,
    result: undefined,
    endTime: null,
    attached: null,
    time: null,
    seq: 0,
  });
  const claude = (streaming: boolean): Item => ({
    kind: "claude",
    key: "c",
    messageId: null,
    text: "hi",
    streaming,
    time: null,
    seq: 0,
  });

  it("shows while a turn runs with nothing moving on screen", () => {
    expect(quietWork([user], true)).toBe(true);
    expect(quietWork([user, tool("ok")], true)).toBe(true);
    expect(quietWork([user, claude(false)], true)).toBe(true);
    expect(quietWork([user], false)).toBe(false);
  });

  it("hides behind a running tool, live thinking or streaming text", () => {
    expect(quietWork([user, tool("running")], true)).toBe(false);
    expect(quietWork([user, { kind: "thinking", key: "k", text: "", time: null, seq: 0 }], true)).toBe(
      false,
    );
    expect(quietWork([user, claude(true)], true)).toBe(false);
  });
});

describe("ask context", () => {
  const line = (seq: number, payload: unknown): WireEvent => ({
    session: SESSION,
    seq,
    time: "t",
    kind: "line",
    payload,
  });
  const assistant = (
    seq: number,
    content: unknown[],
    parent: string | null = null,
    id = `m${seq}`,
  ) =>
    line(seq, {
      type: "assistant",
      message: { id, role: "assistant", content },
      parent_tool_use_id: parent,
    });
  const bash = (id: string) => ({ type: "tool_use", id, name: "Bash", input: { command: "ls" } });
  const done = (seq: number, id: string) =>
    line(seq, {
      type: "user",
      message: { role: "user", content: [{ type: "tool_result", tool_use_id: id, content: "ok" }] },
      parent_tool_use_id: null,
    });

  it("is the text of the call's own message, shared by parallel calls", () => {
    const state = applyEvents(emptyTranscript, [
      assistant(1, [{ type: "text", text: "First." }]),
      assistant(2, [{ type: "text", text: " Lists the folder. " }], null, "m"),
      assistant(3, [bash("b1")], null, "m"),
      assistant(4, [bash("b2")], null, "m"),
    ]);
    expect(askContext(state, "b1")).toBe("Lists the folder.");
    expect(askContext(state, "b2")).toBe("Lists the folder.");
  });

  it("is null when the call's message has no text, even with text in an earlier round", () => {
    const state = applyEvents(emptyTranscript, [
      assistant(1, [{ type: "text", text: "Earlier round." }, bash("b0")]),
      done(2, "b0"),
      assistant(3, [bash("b1")]),
    ]);
    expect(askContext(state, "b1")).toBeNull();
    expect(askContext(state, "nope")).toBeNull();
  });

  it("is found in a subagent's discussion", () => {
    const state = applyEvents(emptyTranscript, [
      assistant(1, [{ type: "tool_use", id: "a1", name: "Agent", input: { description: "look" } }]),
      assistant(2, [{ type: "text", text: "Agent text." }, bash("b1")], "a1"),
    ]);
    expect(askContext(state, "b1")).toBe("Agent text.");
  });
});

describe("turn changes", () => {
  const line = (seq: number, payload: unknown): WireEvent => ({
    session: SESSION,
    seq,
    time: "t",
    kind: "line",
    payload,
  });
  const call = (seq: number, block: unknown, parent: string | null = null) =>
    line(seq, {
      type: "assistant",
      message: { id: `m${seq}`, role: "assistant", content: [block] },
      parent_tool_use_id: parent,
    });
  const done = (seq: number, id: string, isError = false, parent: string | null = null) =>
    line(seq, {
      type: "user",
      message: {
        role: "user",
        content: [{ type: "tool_result", tool_use_id: id, content: "done", is_error: isError }],
      },
      parent_tool_use_id: parent,
    });
  const edit = (id: string, file: string, from: string, to: string) => ({
    type: "tool_use",
    id,
    name: "Edit",
    input: { file_path: file, old_string: from, new_string: to },
  });
  const echo = (seq: number) =>
    line(seq, { type: "user", message: { role: "user", content: "go" }, parent_tool_use_id: null });
  const result = (seq: number) => line(seq, { type: "result", subtype: "success", is_error: false });

  it("closes a turn with its edits and its subagents', failed calls left out", () => {
    const state = applyEvents(emptyTranscript, [
      echo(1),
      call(2, edit("e1", "/p/a.ts", "x", "y\nz")),
      done(3, "e1"),
      call(4, { type: "tool_use", id: "a1", name: "Agent", input: { description: "help" } }),
      call(
        5,
        { type: "tool_use", id: "w1", name: "Write", input: { file_path: "/p/b.ts", content: "q\n" } },
        "a1",
      ),
      done(6, "w1", false, "a1"),
      call(7, edit("e2", "/p/a.ts", "y", "w")),
      done(8, "e2", true),
      call(9, edit("e3", "/p/a.ts", "z", "v")),
      done(10, "e3"),
      result(11),
    ]);
    expect(state.items.map(shape).at(-1)).toEqual(["changes", 2, 4, 2]);
  });

  it("a turn ending on Claude text names that bubble, past the changes row", () => {
    const state = applyEvents(emptyTranscript, [
      echo(1),
      call(2, edit("e1", "/p/a.ts", "x", "y")),
      done(3, "e1"),
      call(4, { type: "text", text: "Done." }),
      result(5),
    ]);
    expect(turnEndBubble(state.items)).toBe(state.items.find((item) => item.kind === "claude")?.key);
  });

  it("a turn ending on a tool call or an interrupt names no bubble", () => {
    const onTool = applyEvents(emptyTranscript, [
      echo(1),
      call(2, { type: "text", text: "Editing." }),
      call(3, edit("e1", "/p/a.ts", "x", "y")),
      done(4, "e1"),
      result(5),
    ]);
    expect(turnEndBubble(onTool.items)).toBeNull();
    const interrupted = applyEvents(emptyTranscript, [
      echo(1),
      call(2, { type: "text", text: "Partial" }),
      line(3, {
        type: "user",
        message: { role: "user", content: "[Request interrupted by user]" },
        parent_tool_use_id: null,
      }),
      result(4),
    ]);
    expect(turnEndBubble(interrupted.items)).toBeNull();
  });

  it("counts only the turn it closes, and nothing for a turn without edits", () => {
    const state = applyEvents(emptyTranscript, [
      echo(1),
      call(2, edit("e1", "/p/a.ts", "x", "y")),
      done(3, "e1"),
      result(4),
      echo(5),
      call(6, { type: "text", text: "Nothing to change." }),
      result(7),
    ]);
    expect(state.items.filter((item) => item.kind === "changes").map(shape)).toEqual([
      ["changes", 1, 1, 1],
    ]);
  });
});
