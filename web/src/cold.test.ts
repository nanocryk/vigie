import { describe, expect, it } from "vitest";

import {
  CacheColdError,
  type ColdChoiceMade,
  ackError,
  archivedAction,
  coldAction,
  coldPanelShown,
  coldSpell,
  contextSummary,
  rewarmField,
} from "@/cold";
import { cacheGauge } from "@/gauges";

const HOUR = 3_600_000;
const start = Date.parse("2026-09-27T10:00:00Z");
const cache = { last_request_at: "2026-09-27T10:00:00Z", ttl_secs: 3600 };
const cold = cacheGauge(cache, start + HOUR + 1);
const warm = cacheGauge(cache, start + HOUR - 1);
const coldAt = start + HOUR;

function shown(input: {
  gauge: ReturnType<typeof cacheGauge>;
  busy?: boolean;
  asks?: number;
  chosen?: ColdChoiceMade | null;
  refusedAt?: number | null;
  settingsAt?: number | null;
}): boolean {
  return coldPanelShown({
    gauge: input.gauge,
    refusedAt: input.refusedAt ?? null,
    settingsAt: input.settingsAt ?? null,
    busy: input.busy ?? false,
    asks: input.asks ?? 0,
    chosen: input.chosen ?? null,
  });
}

describe("coldPanelShown", () => {
  it("blocks the composer of an idle session on a cold cache", () => {
    expect(shown({ gauge: cold })).toBe(true);
    expect(shown({ gauge: warm })).toBe(false);
    expect(shown({ gauge: cacheGauge(null, start) })).toBe(false);
  });

  it("leaves a running turn alone unless an ask waits", () => {
    expect(shown({ gauge: cold, busy: true })).toBe(false);
    expect(shown({ gauge: cold, busy: true, asks: 1 })).toBe(true);
  });

  it("holds a choice until the next cold spell", () => {
    expect(shown({ gauge: cold, asks: 1, chosen: { spell: coldAt, choice: "blank" } })).toBe(false);
    const later = cacheGauge(
      { last_request_at: "2026-09-27T12:00:00Z", ttl_secs: 3600 },
      start + 4 * HOUR,
    );
    expect(shown({ gauge: later, chosen: { spell: coldAt, choice: "rewarm" } })).toBe(true);
  });

  it("comes back when the server refuses on a cache counted warm, even during a turn", () => {
    const refusedAt = start + HOUR - 10;
    expect(shown({ gauge: warm, refusedAt, busy: true })).toBe(true);
    expect(shown({ gauge: warm, refusedAt, chosen: { spell: refusedAt, choice: "rewarm" } })).toBe(
      false,
    );
  });

  it("shows for a model or effort change on a warm cache, until a choice for it", () => {
    const settingsAt = start + 10;
    expect(shown({ gauge: warm, settingsAt })).toBe(true);
    expect(
      shown({ gauge: warm, settingsAt, chosen: { spell: settingsAt, choice: "rewarm" } }),
    ).toBe(false);
  });
});

describe("coldSpell", () => {
  it("takes the latest of the gauge's spell, a refusal and a settings change", () => {
    expect(coldSpell(warm, null, null)).toBe(null);
    expect(coldSpell(cold, null, null)).toBe(coldAt);
    expect(coldSpell(warm, 5, null)).toBe(5);
    expect(coldSpell(cold, coldAt - 1, null)).toBe(coldAt);
    expect(coldSpell(cold, coldAt + 1, null)).toBe(coldAt + 1);
    expect(coldSpell(warm, null, 7)).toBe(7);
    expect(coldSpell(cold, coldAt + 1, coldAt + 2)).toBe(coldAt + 2);
  });
});

describe("rewarmField", () => {
  it("marks requests only after Rewarm was picked for the spell in force", () => {
    expect(rewarmField(null, null)).toEqual({});
    expect(rewarmField(coldAt, null)).toEqual({});
    expect(rewarmField(coldAt, { spell: coldAt, choice: "rewarm" })).toEqual({ rewarm: true });
    expect(rewarmField(coldAt, { spell: coldAt, choice: "continue" })).toEqual({});
    expect(rewarmField(coldAt, { spell: coldAt, choice: "blank" })).toEqual({});
    expect(rewarmField(coldAt + HOUR, { spell: coldAt, choice: "rewarm" })).toEqual({});
    expect(rewarmField(null, { spell: coldAt, choice: "rewarm" })).toEqual({});
  });
});

describe("ackError", () => {
  it("names a cold-cache refusal and passes other errors through", () => {
    const refusal = ackError("cache_cold");
    expect(refusal).toBeInstanceOf(CacheColdError);
    expect(refusal.message).toMatch(/cache is cold/);
    const other = ackError("session x is not running");
    expect(other).not.toBeInstanceOf(CacheColdError);
    expect(other.message).toBe("session x is not running");
    expect(ackError(undefined).message).toBe("Refused by the server");
    expect(ackError("session_live").message).toMatch(/kill it first/);
  });
});

describe("coldAction", () => {
  it("rewarm sends the draft, or only unblocks without one", () => {
    expect(coldAction("rewarm", "go on")).toEqual({ kind: "send", text: "go on" });
    expect(coldAction("rewarm", "  \n")).toEqual({ kind: "unblock" });
  });

  it("continue and blank restart with the draft, if any", () => {
    expect(coldAction("continue", "next")).toEqual({
      kind: "restart",
      mode: "continue",
      text: "next",
    });
    expect(coldAction("blank", "")).toEqual({ kind: "restart", mode: "blank", text: null });
  });

  it("a draft of files alone is not blank, and its files go along", () => {
    const files = ["C:\\up\\a.png"];
    expect(coldAction("rewarm", " ", files)).toEqual({
      kind: "send",
      text: " ",
      attachments: files,
    });
    expect(coldAction("blank", "", files)).toEqual({
      kind: "restart",
      mode: "blank",
      text: "",
      attachments: files,
    });
  });
});

describe("archivedAction", () => {
  it("restores on a bare rewarm and otherwise acts as on a cold cache", () => {
    expect(archivedAction("rewarm", " ")).toEqual({ kind: "restore" });
    expect(archivedAction("rewarm", "go on")).toEqual({ kind: "send", text: "go on" });
    expect(archivedAction("blank", "")).toEqual({ kind: "restart", mode: "blank", text: null });
  });
});

describe("contextSummary", () => {
  it("reads the percentage and the tokens", () => {
    expect(
      contextSummary({
        used_tokens: 230_000,
        max_tokens: 1_000_000,
        percentage: 23,
        source: "cli",
        autocompact_threshold: null,
        categories: [],
      }),
    ).toBe("Context 23% (230k of 1M tokens)");
    expect(contextSummary(null)).toBe("Context usage unknown");
  });
});
