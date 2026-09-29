import { useSyncExternalStore } from "react";

import { isRecord } from "@/wire";

/** Per discussion, the context size when its restart advice was hidden. */
export type Dismissals = Readonly<Record<string, number>>;

export function parseDismissals(text: string | null): Dismissals {
  if (text === null) return {};
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return {};
  }
  if (!isRecord(raw)) return {};
  const dismissals: Record<string, number> = {};
  for (const [session, tokens] of Object.entries(raw)) {
    if (typeof tokens === "number" && Number.isFinite(tokens)) dismissals[session] = tokens;
  }
  return dismissals;
}

/** The advice stays hidden until the context shrinks below its size at the tap. */
export function adviceHidden(hiddenAt: number | undefined, usedTokens: number): boolean {
  return hiddenAt !== undefined && usedTokens >= hiddenAt;
}

const STORAGE_KEY = "vigie2.restart-dismissals";

let current: Dismissals | null = null;
const listeners = new Set<() => void>();

function snapshot(): Dismissals {
  if (current === null) {
    try {
      current = parseDismissals(localStorage.getItem(STORAGE_KEY));
    } catch {
      current = {};
    }
  }
  return current;
}

function change(next: Dismissals): void {
  current = next;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // Storage full or disabled: the advice stays hidden for this page only.
  }
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useDismissals(): Dismissals {
  return useSyncExternalStore(subscribe, snapshot);
}

export function dismissAdvice(session: string, usedTokens: number): void {
  change({ ...snapshot(), [session]: usedTokens });
}

export function forgetDismissal(session: string): void {
  if (!(session in snapshot())) return;
  const { [session]: _, ...rest } = snapshot();
  change(rest);
}
