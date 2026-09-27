import { describe, expect, it } from "vitest";

import { parseQuestions } from "@/components/AskCard";
import { folderName } from "@/format";
import { parseRoute, routeHash } from "@/route";
import { parseCliLine, parseServerMessage, parseSessionList } from "@/wire";

describe("parseServerMessage", () => {
  it("reads events, keeping unknown host events", () => {
    const message = parseServerMessage(
      {
        type: "event",
        session: "s",
        seq: 3,
        time: "t",
        kind: "host",
        payload: { event: "future_thing", x: 1 },
      },
      "msg",
    );
    expect(message).toEqual({
      type: "event",
      event: {
        session: "s",
        seq: 3,
        time: "t",
        kind: "host",
        payload: { event: "unknown", raw: { event: "future_thing", x: 1 } },
      },
    });
  });

  it("reads an ack without an error", () => {
    expect(parseServerMessage({ type: "ack", client_id: "c", ok: true }, "msg")).toEqual({
      type: "ack",
      client_id: "c",
      ok: true,
      error: null,
    });
  });

  it("names the field and value on a bad envelope", () => {
    expect(() => parseServerMessage({ type: "event", session: "s", seq: "3" }, "msg")).toThrow(
      /msg\.seq: expected a number: "3"/,
    );
  });
});

describe("parseSessionList", () => {
  it("reads sessions and their state", () => {
    const list = parseSessionList(
      {
        sessions: [
          {
            id: "a",
            cwd: "C:\\work\\vigie2",
            created_at: "c",
            updated_at: "u",
            state: { kind: "exited", code: 1 },
            last_seq: 9,
            live: false,
            busy: false,
          },
        ],
      },
      "list",
    );
    expect(list[0]?.state).toEqual({ kind: "exited", code: 1 });
  });
});

describe("parseCliLine", () => {
  it("keeps lines of unknown shape as raw", () => {
    expect(parseCliLine("not json")).toEqual({ type: "raw", raw: "not json" });
    expect(parseCliLine({ type: "assistant", message: {} })).toEqual({
      type: "raw",
      raw: { type: "assistant", message: {} },
    });
    expect(parseCliLine({ type: "rate_limit_event" }).type).toBe("raw");
  });
});

describe("routes", () => {
  it("round-trips session routes and defaults to the list", () => {
    expect(parseRoute(routeHash({ kind: "session", id: "abc-1" }))).toEqual({
      kind: "session",
      id: "abc-1",
    });
    expect(parseRoute("")).toEqual({ kind: "list" });
    expect(parseRoute("#auth=token")).toEqual({ kind: "list" });
  });
});

describe("folderName", () => {
  it("takes the last segment of either path style", () => {
    expect(folderName("C:\\work\\vigie2\\")).toBe("vigie2");
    expect(folderName("/home/me/proj")).toBe("proj");
  });
});

describe("parseQuestions", () => {
  it("reads AskUserQuestion input and rejects other shapes", () => {
    const questions = parseQuestions({
      questions: [
        {
          question: "Pick a color?",
          header: "Color",
          multiSelect: false,
          options: [{ label: "red", description: "Selects red." }, { label: "blue" }],
        },
      ],
    });
    expect(questions?.[0]?.options).toEqual([
      { label: "red", description: "Selects red." },
      { label: "blue", description: null },
    ]);
    expect(parseQuestions({ questions: [{ question: 1 }] })).toBeNull();
  });
});
