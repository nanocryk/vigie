import type {
  CacheView,
  Confidence,
  ContextView,
  RestartMeasure,
  SessionView,
  UsageSnapshot,
  WindowReport,
} from "@/wire";

/** Past this age the account readings no longer describe the present. */
export const STALE_AFTER_MS = 10 * 60_000;
/** Under this much TTL left, or a sixth of a shorter TTL, the cache bar warns. */
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

/** How long before going cold a cache of this TTL counts as low. */
export function cacheLowMs(ttlMs: number): number {
  return Math.min(CACHE_LOW_MS, ttlMs / 6);
}

/** The longest TTL a cache write uses. */
const MAX_TTL_MS = 60 * 60_000;

/**
 * `lastActivity` is the discussion's last update: without a recorded request, a
 * discussion idle for longer than any TTL is taken as cold.
 */
export function cacheGauge(
  cache: CacheView | null,
  now: number,
  lastActivity: string | null = null,
): CacheGauge {
  if (cache === null) {
    const idleSince = lastActivity === null ? Number.NaN : Date.parse(lastActivity);
    if (Number.isNaN(idleSince) || now - idleSince <= MAX_TTL_MS) return { kind: "unknown" };
    return { kind: "cold", coldAt: idleSince + MAX_TTL_MS, ttlMs: MAX_TTL_MS };
  }
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
    low: remainingMs < cacheLowMs(ttlMs),
  };
}

export type ContextTone = "ok" | "high" | "full";

export function contextTone(context: ContextView): ContextTone {
  if (context.percentage >= 70) return "full";
  if (context.percentage >= 50) return "high";
  return "ok";
}

/** Advice shows once at least this share of the context is conversation a restart drops. */
export const STALE_MIN_SHARE = 0.5;
/** Advice shows when a restart pays back within this many messages. */
export const ADVICE_MAX_MESSAGES = 10;
/** Below this context, a restart saves too little per message to mention. */
export const ADVICE_MIN_CONTEXT = 100_000;
/** Price of a 1 hour cache write, and of a cache read, over the base input price. */
const CACHE_WRITE_PRICE = 2;
const CACHE_READ_PRICE = 0.1;
/** The context category holding the conversation itself, dropped by a restart. */
const MESSAGES_CATEGORY = "Messages";

export interface RestartAdvice {
  /** Share of the context older than what a restart keeps, 0 to 1. */
  staleShare: number;
  /** Messages after which restarting first costs less. */
  messages: number;
  /** How much less each call costs after a restart. */
  ratio: number;
}

/**
 * Continuing reads the context `C` at the cache read price per call; a restart
 * writes the smaller `S` once, then reads it per call. It pays back after
 * `write * S / (read * (C - S))` calls. See `notes/restart-advice.md`.
 */
export function restartAdvice(
  context: ContextView,
  callsPerMessage: number | null,
  restart: RestartMeasure | null,
): RestartAdvice | null {
  if (restart === null || callsPerMessage === null || callsPerMessage <= 0) return null;
  if (context.categories.length === 0 || context.used_tokens < ADVICE_MIN_CONTEXT) return null;
  let fixed = 0;
  for (const category of context.categories) {
    if (category.kind === "used" && category.name !== MESSAGES_CATEGORY) fixed += category.tokens;
  }
  const current = context.used_tokens;
  const staleShare = clamp((restart.window_start_tokens - fixed) / current, 0, 1);
  if (staleShare < STALE_MIN_SHARE) return null;
  const restarted = fixed + restart.seed_tokens + restart.reread_tokens;
  const saved = current - restarted;
  if (saved <= 0) return null;
  const calls = (CACHE_WRITE_PRICE * restarted) / (CACHE_READ_PRICE * saved);
  const messages = Math.max(1, Math.ceil(calls / callsPerMessage));
  if (messages > ADVICE_MAX_MESSAGES) return null;
  const other = restart.other_cost_per_call;
  const ratio = (CACHE_READ_PRICE * current + other) / (CACHE_READ_PRICE * restarted + other);
  return { staleShare, messages, ratio };
}

/** `~8×`, one decimal under 10. */
export function formatRatio(ratio: number): string {
  const rounded = ratio < 10 ? Math.round(ratio * 10) / 10 : Math.round(ratio);
  return `~${rounded}×`;
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

/** The cache bar's notch spacing, in percent: every 10 minutes of a long TTL, every minute of a short one. */
export function cacheTickStep(ttlMs: number): number {
  const section = ttlMs >= 30 * 60_000 ? 10 * 60_000 : 60_000;
  return (section / ttlMs) * 100;
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

/** Pending asks across all sessions, and the number of sessions working. */
export interface NeedsYou {
  asks: number;
  working: number;
}

export function needsYou(sessions: readonly SessionView[]): NeedsYou {
  let asks = 0;
  let working = 0;
  for (const session of sessions) {
    asks += session.asks;
    if (session.busy) working += 1;
  }
  return { asks, working };
}
