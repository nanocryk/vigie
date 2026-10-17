import { fetchBuild } from "@/version";

/**
 * Whether the server can restart itself, whether it runs a previous build because
 * copying the latest failed, whether a build differing from the running binary
 * waits for a restart, whether a restart waits for every turn to end, and how
 * many turns run.
 */
export interface ServerStatus {
  supervised: boolean;
  staleCopy: boolean;
  newBuild: boolean;
  pending: boolean;
  turns: number;
}

export function parseServerStatus(raw: unknown, label: string): ServerStatus {
  if (typeof raw !== "object" || raw === null) {
    throw new Error(`${label}: expected an object: ${JSON.stringify(raw)}`);
  }
  const supervised: unknown = Reflect.get(raw, "supervised");
  const staleCopy: unknown = Reflect.get(raw, "stale_copy");
  const newBuild: unknown = Reflect.get(raw, "new_build");
  const pending: unknown = Reflect.get(raw, "pending");
  const turns: unknown = Reflect.get(raw, "turns");
  if (typeof supervised !== "boolean") {
    throw new Error(`${label}.supervised: expected a boolean: ${JSON.stringify(supervised)}`);
  }
  if (typeof staleCopy !== "boolean") {
    throw new Error(`${label}.stale_copy: expected a boolean: ${JSON.stringify(staleCopy)}`);
  }
  if (typeof newBuild !== "boolean") {
    throw new Error(`${label}.new_build: expected a boolean: ${JSON.stringify(newBuild)}`);
  }
  if (typeof pending !== "boolean") {
    throw new Error(`${label}.pending: expected a boolean: ${JSON.stringify(pending)}`);
  }
  if (typeof turns !== "number" || !Number.isInteger(turns) || turns < 0) {
    throw new Error(`${label}.turns: expected a count: ${JSON.stringify(turns)}`);
  }
  return { supervised, staleCopy, newBuild, pending, turns };
}

async function check(response: Response): Promise<void> {
  if (response.ok) return;
  const text = await response.text().catch(() => "");
  throw new Error(`HTTP ${response.status}${text === "" ? "" : `: ${text}`}`);
}

export async function fetchServerStatus(): Promise<ServerStatus> {
  const response = await fetch("api/server", { cache: "no-store" });
  await check(response);
  return parseServerStatus(await response.json(), "server status");
}

const BACK_POLL_MS = 1_000;
const BACK_TIMEOUT_MS = 60_000;

/** Resolves once the server answers with a build other than `before`; rejects after a minute. */
export async function waitForNewBuild(before: string | null): Promise<void> {
  const deadline = Date.now() + BACK_TIMEOUT_MS;
  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, BACK_POLL_MS));
    const build = await fetchBuild();
    if (build !== null && build !== before) return;
  }
  throw new Error("The server did not come back within a minute");
}

/**
 * Stops the server now, running turns included; the shim runs it again.
 * Resolves once the new process answers.
 */
export async function restartServer(): Promise<void> {
  const before = await fetchBuild();
  await check(await fetch("api/server/restart", { method: "POST" }));
  await waitForNewBuild(before);
}

/** Drops a restart waiting for every turn to end. */
export async function cancelRestart(): Promise<void> {
  await check(await fetch("api/server/restart/cancel", { method: "POST" }));
}
