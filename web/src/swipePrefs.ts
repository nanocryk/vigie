import { useSyncExternalStore } from "react";

import type { SwipeTuning } from "@/swipe";
import { isRecord } from "@/wire";

export const DISTANCES = ["short", "default", "long"] as const;
export type Distance = (typeof DISTANCES)[number];

export const CONFIRM_WINDOWS = [2000, 3000, 5000] as const;
export type ConfirmWindow = (typeof CONFIRM_WINDOWS)[number];

/** One swipe or two to answer an ask that is not dangerous; a dangerous one always takes two. */
export const SAFE_ASK_SWIPES = ["two", "one"] as const;
export type SafeAskSwipes = (typeof SAFE_ASK_SWIPES)[number];

/** Per device swipe preferences. */
export interface SwipePrefs {
  distance: Distance;
  confirmWindow: ConfirmWindow;
  safeAsk: SafeAskSwipes;
}

export const DEFAULT_SWIPE_PREFS: SwipePrefs = {
  distance: "default",
  confirmWindow: 3000,
  safeAsk: "two",
};

const DISTANCE_TUNING: Record<Distance, Pick<SwipeTuning, "fraction" | "minPx">> = {
  short: { fraction: 0.12, minPx: 40 },
  default: { fraction: 0.2, minPx: 56 },
  long: { fraction: 0.3, minPx: 80 },
};

export function tuningOf(prefs: SwipePrefs): SwipeTuning {
  return { ...DISTANCE_TUNING[prefs.distance], confirmMs: prefs.confirmWindow };
}

function oneOf<T>(options: readonly T[], value: unknown, fallback: T): T {
  return options.find((option) => option === value) ?? fallback;
}

/** Each field falls back to its default on its own, so one bad field keeps the others. */
export function parseSwipePrefs(text: string | null): SwipePrefs {
  if (text === null) return DEFAULT_SWIPE_PREFS;
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return DEFAULT_SWIPE_PREFS;
  }
  if (!isRecord(raw)) return DEFAULT_SWIPE_PREFS;
  return {
    distance: oneOf(DISTANCES, raw.distance, DEFAULT_SWIPE_PREFS.distance),
    confirmWindow: oneOf(CONFIRM_WINDOWS, raw.confirmWindow, DEFAULT_SWIPE_PREFS.confirmWindow),
    safeAsk: oneOf(SAFE_ASK_SWIPES, raw.safeAsk, DEFAULT_SWIPE_PREFS.safeAsk),
  };
}

const STORAGE_KEY = "vigie.swipes";

let current: SwipePrefs | null = null;
const listeners = new Set<() => void>();

function snapshot(): SwipePrefs {
  if (current === null) {
    try {
      current = parseSwipePrefs(localStorage.getItem(STORAGE_KEY));
    } catch {
      current = DEFAULT_SWIPE_PREFS;
    }
  }
  return current;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useSwipePrefs(): SwipePrefs {
  return useSyncExternalStore(subscribe, snapshot);
}

export function setSwipePrefs(next: SwipePrefs): void {
  current = next;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // Storage full or disabled: the choice holds for this page only.
  }
  for (const listener of listeners) listener();
}
