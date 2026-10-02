import { useSyncExternalStore } from "react";

export type Route =
  { kind: "list" } | { kind: "session"; id: string } | { kind: "settings" } | { kind: "files" };

const SESSION_HASH = /^#\/s\/([^/?#]+)$/;
const SETTINGS_HASH = "#/settings";
const FILES_HASH = "#/files";

export function parseRoute(hash: string): Route {
  if (hash === SETTINGS_HASH) return { kind: "settings" };
  if (hash === FILES_HASH) return { kind: "files" };
  const id = SESSION_HASH.exec(hash)?.[1];
  return id === undefined ? { kind: "list" } : { kind: "session", id: decodeURIComponent(id) };
}

export function routeHash(route: Route): string {
  switch (route.kind) {
    case "list":
      return "#/";
    case "session":
      return `#/s/${encodeURIComponent(route.id)}`;
    case "settings":
      return SETTINGS_HASH;
    case "files":
      return FILES_HASH;
  }
}

/** `pushState` changes the hash without a `hashchange`, so pushes notify these. */
const pushListeners = new Set<() => void>();

function subscribeHash(listener: () => void): () => void {
  window.addEventListener("hashchange", listener);
  window.addEventListener("popstate", listener);
  pushListeners.add(listener);
  return () => {
    window.removeEventListener("hashchange", listener);
    window.removeEventListener("popstate", listener);
    pushListeners.delete(listener);
  };
}

function currentHash(): string {
  return location.hash;
}

export function useRoute(): Route {
  return parseRoute(useSyncExternalStore(subscribeHash, currentHash));
}

/** History state key: how many entries separate this one from the Home entry below it. */
const DEPTH = "vigie-depth";

/** Entries above the Home entry; null when none is behind (opened on a screen directly). */
function homeDistance(): number | null {
  const state: unknown = history.state;
  const depth: unknown =
    typeof state === "object" && state !== null ? Reflect.get(state, DEPTH) : undefined;
  if (typeof depth === "number" && Number.isInteger(depth) && depth >= 0) return depth;
  return parseRoute(location.hash).kind === "list" ? 0 : null;
}

/**
 * Pushes a history entry carrying `extra` state, at `hash` or the current URL.
 * It records its distance from Home, which `goHome` jumps back over.
 */
export function pushEntry(extra: Record<string, unknown>, hash?: string): void {
  const distance = homeDistance();
  history.pushState({ ...extra, [DEPTH]: distance === null ? null : distance + 1 }, "", hash);
  for (const listener of pushListeners) listener();
}

/** Pushes a history entry, so the back button returns to the previous screen. */
export function navigate(route: Route): void {
  if (route.kind === "list") goHome();
  else pushEntry({}, routeHash(route));
}

/**
 * Back to the Home entry, dropping every screen above it. Replaces the entry
 * when none is behind, or when the browser dropped it: history keeps a bounded
 * number of entries (50 in Chrome), and going back further does nothing.
 */
export function goHome(): void {
  const distance = homeDistance();
  if (distance === 0) return;
  if (distance === null || distance >= history.length) {
    location.replace(routeHash({ kind: "list" }));
  } else {
    history.go(-distance);
  }
}
