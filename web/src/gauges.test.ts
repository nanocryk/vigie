import { describe, expect, it } from "vitest";

import {
  cacheGauge,
  cacheTickStep,
  contextTone,
  formatPercent,
  formatRatio,
  formatSpan,
  formatTokens,
  formatTtl,
  isStale,
  needsYou,
  readingAge,
  restartAdvice,
  tween,
  windowBar,
} from "@/gauges";
import type {
  ContextView,
  RestartMeasure,
  SessionView,
  UsageSnapshot,
  WindowReport,
} from "@/wire";

const session = (patch: Partial<SessionView>): SessionView => ({
  id: "s",
  cwd: "/p",
  created_at: "c",
  updated_at: "u",
  state: { kind: "running" },
  last_seq: 1,
  live: true,
  busy: false,
  asks: 0,
  permission_mode: null,
  cache: null,
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
  ...patch,
});

describe("needsYou", () => {
  it("sums the asks and counts the working sessions", () => {
    expect(
      needsYou([
        session({ asks: 2, busy: true }),
        session({ asks: 1 }),
        session({ busy: true }),
        session({ live: false, state: { kind: "stopped" } }),
      ]),
    ).toEqual({ asks: 3, working: 2 });
  });

  it("is zero without sessions", () => {
    expect(needsYou([])).toEqual({ asks: 0, working: 0 });
  });
});

const report = (patch: Partial<WindowReport>): WindowReport => ({
  used: 30,
  resets_at: "2026-09-27T13:00:00Z",
  projected: 60,
  confidence: "medium",
  rate: 1,
  work_left_hours: null,
  deadline: null,
  reading_at: "2026-09-27T11:00:00Z",
  ...patch,
});

describe("windowBar", () => {
  it("extends from the spent edge to the projection", () => {
    expect(windowBar("5h", report({}))).toEqual({
      spent: 30,
      full: false,
      projectionLength: 30,
      projectionOpacity: 0.65,
      projectionTone: "ok",
      usedTone: "ok",
    });
  });

  it("fills to the end when projected beyond the budget", () => {
    const bar = windowBar("5h", report({ used: 20, projected: 160, confidence: "high" }));
    expect(bar.projectionTone).toBe("critical");
    expect(bar.usedTone).toBe("ok");
    expect(bar.spent + bar.projectionLength).toBe(100);
    expect(bar.projectionOpacity).toBe(0.85);
  });

  it("colors the used share at 40 and 70", () => {
    expect(windowBar("5h", report({ used: 39.9, projected: 50 })).usedTone).toBe("ok");
    expect(windowBar("5h", report({ used: 40, projected: 50 })).usedTone).toBe("warning");
    expect(windowBar("7d", report({ used: 70, projected: 80 })).usedTone).toBe("critical");
  });

  it("colors the projection per window", () => {
    expect(windowBar("5h", report({ projected: 75 })).projectionTone).toBe("warning");
    expect(windowBar("5h", report({ projected: 90 })).projectionTone).toBe("critical");
    expect(windowBar("7d", report({ projected: 84.9 })).projectionTone).toBe("ok");
    expect(windowBar("7d", report({ projected: 85 })).projectionTone).toBe("warning");
    expect(windowBar("7d", report({ projected: 95 })).projectionTone).toBe("critical");
  });

  it("marks a fully used window and never extends backward", () => {
    expect(windowBar("5h", report({ used: 100, projected: 100 })).full).toBe(true);
    expect(windowBar("5h", report({ used: 40, projected: 35 })).projectionLength).toBe(0);
    expect(windowBar("5h", report({ confidence: "low" })).projectionOpacity).toBe(0.45);
  });
});

describe("staleness", () => {
  const usage = (newest: string | null): UsageSnapshot => ({
    generated_at: "g",
    newest_reading: newest,
    five_hour: null,
    seven_day: null,
  });
  const now = Date.parse("2026-09-27T12:00:00Z");

  it("dims past ten minutes", () => {
    const fresh = readingAge(usage("2026-09-27T11:55:00Z"), now);
    expect(fresh).toBe(5 * 60_000);
    expect(isStale(fresh)).toBe(false);
    expect(isStale(readingAge(usage("2026-09-27T11:49:59Z"), now))).toBe(true);
  });

  it("has no age without a reading", () => {
    expect(readingAge(usage(null), now)).toBeNull();
    expect(readingAge(null, now)).toBeNull();
    expect(isStale(null)).toBe(false);
  });
});

describe("cacheGauge", () => {
  const cache = { last_request_at: "2026-09-27T12:00:00Z", ttl_secs: 3600 };
  const at = (iso: string) => Date.parse(iso);

  it("drains over the TTL", () => {
    const gauge = cacheGauge(cache, at("2026-09-27T12:15:00Z"));
    expect(gauge).toMatchObject({ kind: "warm", fraction: 0.75, low: false });
    expect(cacheGauge(cache, at("2026-09-27T12:55:00Z"))).toMatchObject({
      kind: "warm",
      remainingMs: 5 * 60_000,
      low: true,
    });
  });

  it("is cold at zero and unknown without a cache", () => {
    expect(cacheGauge(cache, at("2026-09-27T13:00:00Z"))).toEqual({
      kind: "cold",
      coldAt: at("2026-09-27T13:00:00Z"),
      ttlMs: 3_600_000,
    });
    expect(cacheGauge(null, 0)).toEqual({ kind: "unknown" });
    expect(cacheGauge({ last_request_at: "bad", ttl_secs: 300 }, 0)).toEqual({ kind: "unknown" });
  });

  it("takes a discussion idle past any TTL as cold when no request is recorded", () => {
    const idle = "2026-09-27T12:00:00Z";
    expect(cacheGauge(null, at("2026-09-27T12:59:00Z"), idle)).toEqual({ kind: "unknown" });
    expect(cacheGauge(null, at("2026-09-27T13:01:00Z"), idle)).toEqual({
      kind: "cold",
      coldAt: at("2026-09-27T13:00:00Z"),
      ttlMs: 3_600_000,
    });
    expect(cacheGauge(null, at("2026-09-27T13:01:00Z"), "bad")).toEqual({ kind: "unknown" });
  });
});

describe("contextTone", () => {
  it("warns as the context fills", () => {
    const context = (percentage: number) => ({
      used_tokens: 0,
      max_tokens: 1,
      percentage,
      source: "estimate" as const,
      autocompact_threshold: null,
      categories: [],
    });
    expect(contextTone(context(3))).toBe("ok");
    expect(contextTone(context(49))).toBe("ok");
    expect(contextTone(context(50))).toBe("high");
    expect(contextTone(context(69))).toBe("high");
    expect(contextTone(context(70))).toBe("full");
  });
});

describe("restartAdvice", () => {
  // 27k fixed, plus a 3k seed and 20k of files read again: a restart starts at 50k.
  const context = (used: number, categories = true): ContextView => ({
    used_tokens: used,
    max_tokens: 1_000_000,
    percentage: used / 10_000,
    source: "cli",
    autocompact_threshold: null,
    categories: categories
      ? [
          { name: "System prompt", tokens: 7_000, kind: "used" },
          { name: "System tools", tokens: 20_000, kind: "used" },
          { name: "MCP tools", tokens: 50_000, kind: "deferred" },
          { name: "Messages", tokens: used - 27_000, kind: "used" },
          { name: "Free space", tokens: 1_000_000 - used, kind: "free" },
        ]
      : [],
  });

  const measure = (windowStart: number, other = 0): RestartMeasure => ({
    window_start_tokens: windowStart,
    seed_tokens: 3_000,
    reread_tokens: 20_000,
    other_cost_per_call: other,
  });

  it("counts the stale share, the messages a restart pays back in, and the gain", () => {
    // 300k of 400k is older than the seed; 2 * 50k / (0.1 * 350k) = 2.9 calls.
    expect(restartAdvice(context(400_000), 1, measure(327_000))).toEqual({
      staleShare: 0.75,
      messages: 3,
      ratio: 8,
    });
    expect(restartAdvice(context(400_000), 5, measure(327_000))?.messages).toBe(1);
  });

  it("weighs what a call costs besides reading the cache", () => {
    // (40k + 5k) / (5k + 5k).
    expect(restartAdvice(context(400_000), 1, measure(327_000, 5_000))?.ratio).toBe(4.5);
  });

  it("stays quiet while under half of the context is stale", () => {
    expect(restartAdvice(context(400_000), 1, measure(227_000))?.staleShare).toBe(0.5);
    expect(restartAdvice(context(400_000), 1, measure(226_000))).toBeNull();
  });

  it("stays quiet past ten messages", () => {
    // 2 * 50k / (0.1 * 100k) = 10 calls.
    expect(restartAdvice(context(150_000), 1, measure(127_000))?.messages).toBe(10);
    expect(restartAdvice(context(140_000), 1, measure(117_000))).toBeNull();
  });

  it("stays quiet without the figures it needs or under 100k of context", () => {
    expect(restartAdvice(context(400_000), 1, null)).toBeNull();
    expect(restartAdvice(context(400_000), null, measure(327_000))).toBeNull();
    expect(restartAdvice(context(400_000, false), 1, measure(327_000))).toBeNull();
    expect(restartAdvice(context(90_000), 1, measure(80_000))).toBeNull();
  });

  it("formats the ratio", () => {
    expect(formatRatio(8)).toBe("~8×");
    expect(formatRatio(3.14)).toBe("~3.1×");
    expect(formatRatio(12.6)).toBe("~13×");
  });
});

describe("cacheTickStep", () => {
  it("notches a long TTL every 10 minutes and a short one every minute", () => {
    expect(cacheTickStep(3_600_000)).toBeCloseTo(100 / 6);
    expect(cacheTickStep(300_000)).toBeCloseTo(20);
  });
});

describe("formatting", () => {
  it("spans", () => {
    expect(formatSpan(45_000)).toBe("45s");
    expect(formatSpan(42 * 60_000 + 59_000)).toBe("42m");
    expect(formatSpan((2 * 60 + 5) * 60_000)).toBe("2h05");
    expect(formatSpan((28 * 60 + 30) * 60_000)).toBe("1d4h");
    expect(formatSpan(-5)).toBe("0s");
  });

  it("percents, tokens and TTLs", () => {
    expect(formatPercent(3.0985)).toBe("3.1%");
    expect(formatPercent(61.4)).toBe("61%");
    expect(formatTokens(850)).toBe("850");
    expect(formatTokens(30_985)).toBe("31k");
    expect(formatTokens(1_000_000)).toBe("1M");
    expect(formatTokens(1_250_000)).toBe("1.3M");
    expect(formatTtl(3_600_000)).toBe("1 h");
    expect(formatTtl(300_000)).toBe("5 min");
  });

  it("tweens with an ease-out", () => {
    expect(tween(10, 20, 0)).toBe(10);
    expect(tween(10, 20, 1)).toBe(20);
    expect(tween(10, 20, 0.5)).toBeGreaterThan(15);
    expect(tween(10, 20, 2)).toBe(20);
  });
});
