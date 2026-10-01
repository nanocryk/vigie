import { useEffect, useSyncExternalStore } from "react";

import { useClock } from "@/components/useNow";
import { coolingSpell } from "@/home";
import { isRecord, type SessionView } from "@/wire";

/** Per discussion, the cooling spell (its cache's last request time) already seen on this device. */
export type CoolingSeen = Readonly<Record<string, string>>;

export function parseCoolingSeen(text: string | null): CoolingSeen {
  if (text === null) return {};
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return {};
  }
  if (!isRecord(raw)) return {};
  const seen: Record<string, string> = {};
  for (const [session, spell] of Object.entries(raw)) {
    if (typeof spell === "string") seen[session] = spell;
  }
  return seen;
}

/** `seen` with the spells cooling in `sessions` added, and the ended spells of `sessions` dropped. */
export function seeCooling(
  seen: CoolingSeen,
  sessions: readonly SessionView[],
  now: number,
): CoolingSeen {
  const next: Record<string, string> = { ...seen };
  for (const session of sessions) {
    const spell = coolingSpell(session, now);
    if (spell === null) delete next[session.id];
    else next[session.id] = spell;
  }
  return next;
}

function sameSeen(a: CoolingSeen, b: CoolingSeen): boolean {
  const keys = Object.keys(a);
  return keys.length === Object.keys(b).length && keys.every((key) => a[key] === b[key]);
}

const STORAGE_KEY = "vigie.cooling-seen";
const CLOCK_MS = 30_000;

let current: CoolingSeen | null = null;
const listeners = new Set<() => void>();

function snapshot(): CoolingSeen {
  if (current === null) {
    try {
      current = parseCoolingSeen(localStorage.getItem(STORAGE_KEY));
    } catch {
      current = {};
    }
  }
  return current;
}

function change(next: CoolingSeen): void {
  current = next;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // Storage full or disabled: the spells count as seen for this page only.
  }
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useCoolingSeen(): CoolingSeen {
  return useSyncExternalStore(subscribe, snapshot);
}

/** Marks the spells cooling in `sessions` as seen as they start, while mounted and visible. */
export function useSeeCooling(sessions: readonly SessionView[]): void {
  const now = useClock(CLOCK_MS);
  useEffect(() => {
    if (document.visibilityState !== "visible") return;
    const next = seeCooling(snapshot(), sessions, now);
    if (!sameSeen(next, snapshot())) change(next);
  }, [sessions, now]);
}
