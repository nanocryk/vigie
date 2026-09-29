import { describe, expect, it } from "vitest";

import {
  homeSections,
  rowAction,
  rowHighlight,
  rowSwipeRules,
  strongestHighlight,
} from "@/home";
import { type SwipeInput, type SwipeState, idle, step } from "@/swipe";
import type { SessionView } from "@/wire";

const session = (id: string, patch: Partial<SessionView> = {}): SessionView => ({
  id,
  cwd: "/p",
  created_at: "2026-09-27T10:00:00Z",
  updated_at: "2026-09-27T10:00:00Z",
  state: { kind: "stopped" },
  last_seq: 1,
  live: false,
  busy: false,
  asks: 0,
  permission_mode: null,
  cache: null,
  context: null,
  archived_at: null,
  model: null,
  effort: null,
  settings_changed_at: null,
  applied_model: null,
  applied_effort: null,
  agents: [],
  unread: null,
  ...patch,
});

describe("rowHighlight", () => {
  it("shows the strongest state: ask, stop, finished turn, then work", () => {
    const all = { asks: 1, unread: "stopped", busy: true } as const;
    expect(rowHighlight(session("s", all))).toBe("ask");
    expect(rowHighlight(session("s", { ...all, asks: 0 }))).toBe("stopped");
    expect(rowHighlight(session("s", { unread: "finished", busy: true }))).toBe("finished");
    expect(rowHighlight(session("s", { busy: true }))).toBe("working");
    expect(rowHighlight(session("s"))).toBeNull();
  });
});

describe("strongestHighlight", () => {
  it("picks the strongest state, leaving out the session on screen", () => {
    const sessions = [
      session("a", { busy: true }),
      session("b", { unread: "finished" }),
      session("c", { asks: 2 }),
    ];
    expect(strongestHighlight(sessions)).toBe("ask");
    expect(strongestHighlight(sessions, "c")).toBe("finished");
    expect(strongestHighlight([session("d")])).toBeNull();
  });
});

describe("homeSections", () => {
  it("keeps active sessions in order and lists the archived ones latest first", () => {
    const sections = homeSections([
      session("a"),
      session("old", { archived_at: "2026-09-27T11:00:00Z" }),
      session("b", { live: true }),
      session("new", { archived_at: "2026-09-27T12:00:00Z" }),
    ]);
    expect(sections.active.map((entry) => entry.id)).toEqual(["a", "b"]);
    expect(sections.archived.map((entry) => entry.id)).toEqual(["new", "old"]);
  });
});

describe("row gestures", () => {
  const context = (live: boolean) => ({
    width: 400,
    ...rowSwipeRules(rowAction(session("s", { live }))),
  });
  const swipeLeft = (t: number): SwipeInput[] => [
    { type: "down", x: 300, y: 20, t },
    { type: "up", x: 150, y: 20, t: t + 100 },
  ];
  const run = (inputs: SwipeInput[], live: boolean): SwipeState =>
    inputs.reduce((state, input) => step(state, input, context(live)), idle);

  it("kills a live row on the second left swipe", () => {
    expect(rowAction(session("s", { live: true }))).toBe("kill");
    expect(run(swipeLeft(0), true)).toEqual({ kind: "armed", side: "left", at: 100 });
    expect(run([...swipeLeft(0), ...swipeLeft(500)], true)).toEqual({
      kind: "fired",
      side: "left",
    });
  });

  it("archives a stopped row on the first left swipe", () => {
    expect(rowAction(session("s"))).toBe("archive");
    expect(run(swipeLeft(0), false)).toEqual({ kind: "fired", side: "left" });
  });

  it("restores an archived row on the first right swipe, never on a left one", () => {
    const archived = session("s", { archived_at: "2026-09-27T11:00:00Z" });
    expect(rowAction(archived)).toBe("restore");
    const rules = { width: 400, ...rowSwipeRules("restore") };
    const swipe = (fromX: number, toX: number): SwipeState =>
      [
        { type: "down", x: fromX, y: 20, t: 0 } as const,
        { type: "up", x: toX, y: 20, t: 100 } as const,
      ].reduce((state: SwipeState, input) => step(state, input, rules), idle);
    expect(swipe(100, 250)).toEqual({ kind: "fired", side: "right" });
    expect(swipe(300, 150)).toEqual(idle);
  });
});
