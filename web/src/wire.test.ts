import { describe, expect, it } from "vitest";

import { parseQuestions } from "@/components/AskCard";
import { folderName } from "@/format";
import { parseRoute, routeHash } from "@/route";
import {
  parseCliLine,
  parseServerMessage,
  parseSessionList,
  parseSharedCatalog,
  parseUsageSnapshot,
} from "@/wire";

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

  it("reads the danger of a queued ask, defaulting to safe", () => {
    const ask = { event: "ask_queued", request_id: "r", tool_name: "Bash", tool_use_id: "t" };
    const queued = (payload: unknown) =>
      parseServerMessage(
        { type: "event", session: "s", seq: 1, time: "t", kind: "host", payload },
        "msg",
      );
    const danger = { dangerous: true, reasons: ["Hard reset: git reset --hard"] };
    expect(queued({ ...ask, danger })).toMatchObject({ event: { payload: { danger } } });
    expect(queued(ask)).toMatchObject({
      event: { payload: { danger: { dangerous: false, reasons: [] } } },
    });
  });

  it("reads the offers of a queued ask, dropping an odd folder offer", () => {
    const ask = { event: "ask_queued", request_id: "r", tool_name: "Read", tool_use_id: "t" };
    const queued = (payload: unknown) =>
      parseServerMessage(
        { type: "event", session: "s", seq: 1, time: "t", kind: "host", payload },
        "msg",
      );
    const folder = { kind: "read", folder: "C:\\a\\b", broad: ["C:\\"] };
    expect(queued({ ...ask, accept_edits: true, folder })).toMatchObject({
      event: { payload: { accept_edits: true, folder } },
    });
    expect(queued(ask)).toMatchObject({
      event: { payload: { accept_edits: false, folder: null } },
    });
    expect(queued({ ...ask, folder: { kind: "write", folder: "C:\\a" } })).toMatchObject({
      event: { payload: { folder: null } },
    });
    expect(queued({ ...ask, folder: { kind: "directory", folder: "C:\\a" } })).toMatchObject({
      event: { payload: { folder: { kind: "directory", folder: "C:\\a", broad: [] } } },
    });
    const file = { kind: "read", path: "C:\\a\\b.md" };
    expect(queued({ ...ask, file })).toMatchObject({ event: { payload: { file } } });
    expect(queued(ask)).toMatchObject({ event: { payload: { file: null } } });
    expect(queued({ ...ask, file: { kind: "write", path: "C:\\a" } })).toMatchObject({
      event: { payload: { file: null } },
    });
  });

  it("reads an auto-allowed ask, which needs its file", () => {
    const allowed = { event: "auto_allowed", request_id: "r", tool_name: "Edit", tool_use_id: "t" };
    const host = (payload: unknown) =>
      parseServerMessage(
        { type: "event", session: "s", seq: 1, time: "t", kind: "host", payload },
        "msg",
      );
    const file = { kind: "edit", path: "/a/b.md" };
    expect(host({ ...allowed, file })).toMatchObject({
      event: { payload: { event: "auto_allowed", request_id: "r", file } },
    });
    expect(host(allowed)).toMatchObject({ event: { payload: { event: "unknown" } } });
  });

  it("reads an ack without an error", () => {
    expect(parseServerMessage({ type: "ack", client_id: "c", ok: true }, "msg")).toEqual({
      type: "ack",
      client_id: "c",
      ok: true,
      error: null,
    });
  });

  it("reads the end of a history catch-up", () => {
    expect(parseServerMessage({ type: "synced", session: "s", seq: 42 }, "msg")).toEqual({
      type: "synced",
      session: "s",
      seq: 42,
    });
  });

  it("reads a window and a page of earlier history", () => {
    expect(
      parseServerMessage(
        { type: "window", session: "s", first_seq: 7, start_reached: false },
        "msg",
      ),
    ).toEqual({ type: "window", session: "s", first_seq: 7, start_reached: false });
    const page = parseServerMessage(
      {
        type: "page",
        session: "s",
        first_seq: 1,
        start_reached: true,
        events: [
          { session: "s", seq: 1, time: "t", kind: "host", payload: { event: "started", cwd: "/p" } },
          { session: "s", seq: 2, time: "t", kind: "line", payload: "raw" },
        ],
      },
      "msg",
    );
    expect(page).toMatchObject({
      type: "page",
      first_seq: 1,
      start_reached: true,
      events: [
        { seq: 1, kind: "host", payload: { event: "started", cwd: "/p" } },
        { seq: 2, kind: "line", payload: "raw" },
      ],
    });
    expect(() =>
      parseServerMessage(
        { type: "page", session: "s", first_seq: 1, start_reached: true, events: [{ seq: 1 }] },
        "msg",
      ),
    ).toThrow(/msg\.events\[0\]\.session: expected a string/);
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
            live: true,
            busy: false,
            asks: 2,
            permission_mode: "plan",
            cache: { last_request_at: "2026-09-27T12:00:00Z", ttl_secs: 3600 },
            context: {
              used_tokens: 30985,
              max_tokens: 1000000,
              percentage: 3.0985,
              source: "estimate",
              autocompact_threshold: 900000,
              categories: [{ name: "Messages", tokens: 27985, kind: "used" }],
            },
          },
          {
            id: "b",
            cwd: "/old",
            created_at: "c",
            updated_at: "u",
            state: { kind: "stopped" },
            last_seq: 1,
            live: false,
            busy: false,
            asks: 0,
            archived_at: "2026-09-27T13:00:00Z",
          },
        ],
      },
      "list",
    );
    expect(list.map((session) => session.archived_at)).toEqual([null, "2026-09-27T13:00:00Z"]);
    expect(list[0]?.state).toEqual({ kind: "exited", code: 1 });
    expect(list[0]?.permission_mode).toBe("plan");
    expect(list[1]?.permission_mode).toBeNull();
    expect(list.map((session) => session.asks)).toEqual([2, 0]);
    expect(list[0]?.cache?.ttl_secs).toBe(3600);
    expect(list[0]?.context?.source).toBe("estimate");
    expect(list[0]?.context?.autocompact_threshold).toBe(900000);
    expect(list[0]?.context?.categories).toEqual([
      { name: "Messages", tokens: 27985, kind: "used" },
    ]);
    expect(list[1]?.cache).toBeNull();
    expect(list[1]?.context).toBeNull();
  });

  it("names a bad context source", () => {
    const session = {
      id: "a",
      cwd: "/p",
      created_at: "c",
      updated_at: "u",
      state: { kind: "stopped" },
      last_seq: 1,
      live: false,
      busy: false,
      asks: 0,
      context: { used_tokens: 1, max_tokens: 2, percentage: 50, source: "guess" },
    };
    expect(() => parseSessionList({ sessions: [session] }, "list")).toThrow(
      /list\.sessions\[0\]\.context\.source: unknown context source: "guess"/,
    );
  });

  it("names a bad context category", () => {
    const session = {
      id: "a",
      cwd: "/p",
      created_at: "c",
      updated_at: "u",
      state: { kind: "stopped" },
      last_seq: 1,
      live: false,
      busy: false,
      asks: 0,
      context: {
        used_tokens: 1,
        max_tokens: 2,
        percentage: 50,
        source: "cli",
        autocompact_threshold: null,
        categories: [{ name: "Messages", tokens: "1", kind: "used" }],
      },
    };
    expect(() => parseSessionList({ sessions: [session] }, "list")).toThrow(
      /list\.sessions\[0\]\.context\.categories\[0\]\.tokens: expected a number: "1"/,
    );
  });
});

describe("parseUsageSnapshot", () => {
  it("reads both windows, a missing one as null", () => {
    const usage = parseUsageSnapshot(
      {
        generated_at: "g",
        newest_reading: "2026-09-27T11:05:00Z",
        five_hour: {
          used: 7,
          resets_at: "2026-09-27T13:00:00Z",
          projected: 40.5,
          confidence: "low",
          rate: 0.8,
          work_left_hours: null,
          deadline: null,
          reading_at: "2026-09-27T11:05:00Z",
        },
        seven_day: null,
        facts: [],
      },
      "usage",
    );
    expect(usage.five_hour?.projected).toBe(40.5);
    expect(usage.five_hour?.confidence).toBe("low");
    expect(usage.seven_day).toBeNull();
  });
});

describe("parseSharedCatalog", () => {
  it("reads each field, an unreported one as null", () => {
    expect(
      parseSharedCatalog(
        { models: null, commands: null, terminal_commands: null, defaults: null },
        "catalog",
      ),
    ).toEqual({ models: null, commands: null, terminal_commands: null, defaults: null });
    const catalog = parseSharedCatalog(
      {
        models: [{ value: "opus", resolved_model: "claude-opus-5-5", display_name: "Opus 5.5" }, 3],
        commands: [{ name: "review", description: "Review", argument_hint: "" }],
        terminal_commands: ["doctor", 1],
        defaults: { model: "claude-opus-5-5[1m]", effort: "medium" },
      },
      "catalog",
    );
    expect(catalog.models?.map((model) => model.value)).toEqual(["opus"]);
    expect(catalog.commands?.[0]?.name).toBe("review");
    expect(catalog.terminal_commands).toEqual(["doctor"]);
    expect(catalog.defaults).toEqual({ model: "claude-opus-5-5[1m]", effort: "medium" });
    expect(() => parseSharedCatalog({ models: "x" }, "catalog")).toThrow("catalog.models");
  });

  it("parses the catalog_changed notification", () => {
    expect(parseServerMessage({ type: "catalog_changed" }, "message")).toEqual({
      type: "catalog_changed",
    });
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
