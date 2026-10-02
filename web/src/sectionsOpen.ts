import { useSyncExternalStore } from "react";

/** Ids of the Device settings sections left open on this device; the rest show closed. */
export type OpenSections = ReadonlySet<string>;

export function parseOpenSections(text: string | null): OpenSections {
  if (text === null) return new Set();
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return new Set();
  }
  if (!Array.isArray(raw)) return new Set();
  return new Set(raw.filter((id): id is string => typeof id === "string"));
}

export function toggledSections(open: OpenSections, id: string): OpenSections {
  const next = new Set(open);
  if (!next.delete(id)) next.add(id);
  return next;
}

const STORAGE_KEY = "vigie.settings-open";

let current: OpenSections | null = null;
const listeners = new Set<() => void>();

function snapshot(): OpenSections {
  if (current === null) {
    try {
      current = parseOpenSections(localStorage.getItem(STORAGE_KEY));
    } catch {
      current = new Set();
    }
  }
  return current;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useOpenSections(): OpenSections {
  return useSyncExternalStore(subscribe, snapshot);
}

export function toggleSection(id: string): void {
  current = toggledSections(snapshot(), id);
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify([...current]));
  } catch {
    // Storage full or disabled: the section stays toggled for this page only.
  }
  for (const listener of listeners) listener();
}
