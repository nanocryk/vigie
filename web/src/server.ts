import { fetchBuild } from "@/version";

/** Whether the server can restart itself, and how many turns a restart would stop. */
export interface ServerStatus {
  supervised: boolean;
  turns: number;
}

export function parseServerStatus(raw: unknown, label: string): ServerStatus {
  if (typeof raw !== "object" || raw === null) {
    throw new Error(`${label}: expected an object: ${JSON.stringify(raw)}`);
  }
  const supervised: unknown = Reflect.get(raw, "supervised");
  const turns: unknown = Reflect.get(raw, "turns");
  if (typeof supervised !== "boolean") {
    throw new Error(`${label}.supervised: expected a boolean: ${JSON.stringify(supervised)}`);
  }
  if (typeof turns !== "number" || !Number.isInteger(turns) || turns < 0) {
    throw new Error(`${label}.turns: expected a count: ${JSON.stringify(turns)}`);
  }
  return { supervised, turns };
}

async function check(response: Response): Promise<void> {
  if (response.ok) return;
  const text = await response.text().catch(() => "");
  throw new Error(`HTTP ${response.status}${text === "" ? "" : `: ${text}`}`);
}

export async function fetchServerStatus(): Promise<ServerStatus> {
  const response = await fetch("api/server");
  await check(response);
  return parseServerStatus(await response.json(), "server status");
}

const BACK_POLL_MS = 1_000;
const BACK_TIMEOUT_MS = 60_000;

/**
 * Stops the server now, running turns included; the shim runs it again.
 * Resolves once the new process answers, rejects if it does not within a minute.
 */
export async function restartServer(): Promise<void> {
  const before = await fetchBuild();
  await check(await fetch("api/server/restart", { method: "POST" }));
  const deadline = Date.now() + BACK_TIMEOUT_MS;
  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, BACK_POLL_MS));
    const build = await fetchBuild();
    if (build !== null && build !== before) return;
  }
  throw new Error("The server did not come back within a minute");
}
