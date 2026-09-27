import { describe, expect, it } from "vitest";

import {
  cacheGauge,
  contextTone,
  formatPercent,
  formatSpan,
  formatTokens,
  formatTtl,
  isStale,
  readingAge,
  tween,
  windowBar,
} from "@/gauges";
import type { UsageSnapshot, WindowReport } from "@/wire";

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
});

describe("contextTone", () => {
  it("warns as the context fills", () => {
    const context = (percentage: number) => ({
      used_tokens: 0,
      max_tokens: 1,
      percentage,
      source: "estimate" as const,
    });
    expect(contextTone(context(3))).toBe("ok");
    expect(contextTone(context(75))).toBe("high");
    expect(contextTone(context(95))).toBe("full");
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
