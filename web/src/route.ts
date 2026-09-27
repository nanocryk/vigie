import { useSyncExternalStore } from "react";

export type Route = { kind: "list" } | { kind: "session"; id: string };

const SESSION_HASH = /^#\/s\/([^/?#]+)$/;

export function parseRoute(hash: string): Route {
  const id = SESSION_HASH.exec(hash)?.[1];
  return id === undefined ? { kind: "list" } : { kind: "session", id: decodeURIComponent(id) };
}

export function routeHash(route: Route): string {
  return route.kind === "list" ? "#/" : `#/s/${encodeURIComponent(route.id)}`;
}

function subscribeHash(listener: () => void): () => void {
  window.addEventListener("hashchange", listener);
  return () => window.removeEventListener("hashchange", listener);
}

function currentHash(): string {
  return location.hash;
}

export function useRoute(): Route {
  return parseRoute(useSyncExternalStore(subscribeHash, currentHash));
}

let navigatedInApp = false;

/** Pushes a history entry, so the back button returns to the previous screen. */
export function navigate(route: Route): void {
  navigatedInApp = true;
  location.hash = routeHash(route);
}

/** Pops back to the list when the app pushed the current entry, else replaces it. */
export function backToList(): void {
  if (navigatedInApp) history.back();
  else location.replace(routeHash({ kind: "list" }));
}
