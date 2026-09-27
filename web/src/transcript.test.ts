import { readFileSync } from "node:fs";
import path from "node:path";

import { describe, expect, it } from "vitest";

import { type Item, applyEvents, emptyTranscript, toolSummary } from "@/transcript";
import { type WireEvent, isRecord, parseHostEvent } from "@/wire";

const SESSION = "00000000-0000-0000-0000-000000000001";

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
  const push = (kind: "line" | "host", payload: unknown): void => {
    const base = { session: SESSION, seq: events.length + 1, time: "2026-09-27T00:00:00Z" };
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
  | ["notice", string];

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
      ["tool", "AskUserQuestion", expect.stringContaining("Pick a color?"), "ok"],
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
    expect(toolSummary("Other", { a: 1 })).toBe('{"a":1}');
  });
});
