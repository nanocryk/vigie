import { describe, expect, it } from "vitest";

import { parseCoolingSeen, seeCooling } from "@/coolingSeen";
import type { SessionView } from "@/wire";

const NOW = Date.parse("2026-09-27T11:00:00Z");

const session = (id: string, lastRequest: string | null): SessionView => ({
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
  cache: lastRequest === null ? null : { last_request_at: lastRequest, ttl_secs: 3600 },
  context: null,
  calls_per_message: null,
  restart: null,
  name: null,
  archived_at: null,
  scratch: false,
  model: null,
  effort: null,
  settings_changed_at: null,
  applied_model: null,
  applied_effort: null,
  agents: [],
  background: [],
  unread: null,
  held: [],
});

describe("parseCoolingSeen", () => {
  it("keeps string spells and drops the rest", () => {
    expect(parseCoolingSeen('{"a":"t","b":3}')).toEqual({ a: "t" });
    expect(parseCoolingSeen("not json")).toEqual({});
    expect(parseCoolingSeen(null)).toEqual({});
  });
});

describe("seeCooling", () => {
  it("adds cooling spells, drops ended ones and leaves other sessions alone", () => {
    const seen = { a: "2026-09-27T09:00:00Z", b: "2026-09-27T10:05:00Z", other: "x" };
    const sessions = [
      session("a", "2026-09-27T10:30:00Z"),
      session("b", "2026-09-27T10:05:00Z"),
      session("c", "2026-09-27T10:02:00Z"),
    ];
    expect(seeCooling(seen, sessions, NOW)).toEqual({
      b: "2026-09-27T10:05:00Z",
      c: "2026-09-27T10:02:00Z",
      other: "x",
    });
  });
});
