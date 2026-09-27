import type { CacheView, Confidence, ContextView, UsageSnapshot, WindowReport } from "@/wire";

/** Past this age the account readings no longer describe the present. */
export const STALE_AFTER_MS = 10 * 60_000;
/** Under this much TTL left, the cache bar warns. */
export const CACHE_LOW_MS = 10 * 60_000;
const PROJECTION_OPACITY: Record<Confidence, number> = { low: 0.45, medium: 0.65, high: 0.85 };
export type Tone = "ok" | "warning" | "critical";
export type WindowKind = "5h" | "7d";

/** Warning and critical thresholds, as in the status bar. */
type Thresholds = readonly [number, number];
const USED_THRESHOLDS: Thresholds = [40, 70];
const PROJECTED_THRESHOLDS: Record<WindowKind, Thresholds> = { "5h": [75, 90], "7d": [85, 95] };

function tone(pct: number, [warning, critical]: Thresholds): Tone {
  if (pct >= critical) return "critical";
  if (pct >= warning) return "warning";
  return "ok";
}

/** Geometry and colors of a window bar, lengths in percent of the bar's width. */
export interface WindowBar {
  spent: number;
  /** The whole budget is used. */
  full: boolean;
  /** Starts at `spent` and ends at the projection, within the bar. */
  projectionLength: number;
  projectionOpacity: number;
  projectionTone: Tone;
  usedTone: Tone;
}

function clamp(value: number, low: number, high: number): number {
  return Math.min(high, Math.max(low, value));
}

export function windowBar(kind: WindowKind, report: WindowReport): WindowBar {
  const spent = clamp(report.used, 0, 100);
  const reach = clamp(report.projected, spent, 100);
  return {
    spent,
    full: report.used >= 100,
    projectionLength: reach - spent,
    projectionOpacity: PROJECTION_OPACITY[report.confidence],
    projectionTone: tone(report.projected, PROJECTED_THRESHOLDS[kind]),
    usedTone: tone(report.used, USED_THRESHOLDS),
  };
}

/** Age of the newest reading, or null without one. */
export function readingAge(usage: UsageSnapshot | null, now: number): number | null {
  if (usage?.newest_reading == null) return null;
  const time = Date.parse(usage.newest_reading);
  return Number.isNaN(time) ? null : Math.max(0, now - time);
}

export function isStale(age: number | null): boolean {
  return age !== null && age > STALE_AFTER_MS;
}

export type CacheGauge =
  | { kind: "unknown" }
  | {
      kind: "warm";
      /** Share of the TTL left, 0 to 1. */
      fraction: number;
      remainingMs: number;
      coldAt: number;
      ttlMs: number;
      low: boolean;
    }
  | { kind: "cold"; coldAt: number; ttlMs: number };

export function cacheGauge(cache: CacheView | null, now: number): CacheGauge {
  if (cache === null) return { kind: "unknown" };
  const start = Date.parse(cache.last_request_at);
  const ttlMs = cache.ttl_secs * 1000;
  if (Number.isNaN(start) || ttlMs <= 0) return { kind: "unknown" };
  const coldAt = start + ttlMs;
  const remainingMs = coldAt - now;
  if (remainingMs <= 0) return { kind: "cold", coldAt, ttlMs };
  return {
    kind: "warm",
    fraction: clamp(remainingMs / ttlMs, 0, 1),
    remainingMs,
    coldAt,
    ttlMs,
    low: remainingMs < CACHE_LOW_MS,
  };
}

export type ContextTone = "ok" | "high" | "full";

export function contextTone(context: ContextView): ContextTone {
  if (context.percentage >= 90) return "full";
  if (context.percentage >= 70) return "high";
  return "ok";
}

/** Compact span: `45s`, `42m`, `2h10`, `3d4h`. */
export function formatSpan(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h${String(minutes % 60).padStart(2, "0")}`;
  return `${Math.floor(hours / 24)}d${hours % 24}h`;
}

/** One decimal under 10, whole above. */
export function formatPercent(value: number): string {
  const rounded = Math.abs(value) < 10 ? Math.round(value * 10) / 10 : Math.round(value);
  return `${rounded}%`;
}

/** `850`, `31k`, `1.2M`. */
export function formatTokens(value: number): string {
  if (value < 1000) return String(Math.round(value));
  if (value < 1_000_000) return `${Math.round(value / 1000)}k`;
  const millions = Math.round(value / 100_000) / 10;
  return `${millions}M`;
}

export function formatTtl(ttlMs: number): string {
  return ttlMs % 3_600_000 === 0 ? `${ttlMs / 3_600_000} h` : `${Math.round(ttlMs / 60_000)} min`;
}

/** Ease-out cubic over `t` in 0 to 1. */
export function easeOut(t: number): number {
  const clamped = clamp(t, 0, 1);
  return 1 - (1 - clamped) ** 3;
}

export function tween(from: number, to: number, t: number): number {
  return from + (to - from) * easeOut(t);
}
