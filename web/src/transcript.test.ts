import { readFileSync } from "node:fs";
import path from "node:path";

import { describe, expect, it } from "vitest";

import { toolDiff } from "@/diff";
import {
  type Item,
  applyEvents,
  emptyTranscript,
  restartLabel,
  toolSummary,
  toolTimeVisible,
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
  | ["restart", string, string | null];

function shape(item: Item): Shape {
  switch (item.kind) {
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
        payload: { event: "user_sent", text: "queued", client_id: "q" },
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
            toolUseId: "t",
            name: "Bash",
            summary: "",
            input: {},
            status: "ok",
            output: null,
            result: undefined,
            endTime: null,
          }
        : { kind, key: String(time), time, tone: "info", text: "", detail: null };
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

describe("restarts", () => {
  const SEED = "Continuing an earlier vigie2 session in /p.\n\n## Exchange 1\nUser: hi";
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
      echo(2, `${SEED}\n\nSay what you want to do next.`),
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

describe("toolSummary", () => {
  it("picks the field that says what the call does", () => {
    expect(toolSummary("Bash", { command: "ls -la" })).toBe("ls -la");
    expect(toolSummary("Read", { file_path: "/a/b.ts" })).toBe("/a/b.ts");
    expect(toolSummary("Glob", { pattern: "**/*.rs" })).toBe("**/*.rs");
    expect(toolSummary("Other", { a: 1, b: "two  words", c: [1, 2], d: { e: 1 } })).toBe(
      "a 1 · b two words · c [2] · d {...}",
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
