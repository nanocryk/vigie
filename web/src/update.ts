/** Upstream commits HEAD lacks, from the last `git fetch`. */
export type Check =
  { state: "unchecked" } | { state: "done"; behind: number } | { state: "failed"; message: string };

export type Step = "clean" | "pull" | "install" | "web" | "server";

export type Run =
  | { state: "idle" }
  | { state: "running"; step: Step }
  | { state: "built" }
  | { state: "failed"; step: Step; output: string };

export interface Commit {
  sha: string;
  title: string;
}

export interface UpdateStatus {
  check: Check;
  run: Run;
  /** Commits the latest update pulled, newest first; empty while there is none to show. */
  changelog: Commit[];
}

const STEPS: readonly Step[] = ["clean", "pull", "install", "web", "server"];

function object(raw: unknown, label: string): object {
  if (typeof raw !== "object" || raw === null) {
    throw new Error(`${label}: expected an object: ${JSON.stringify(raw)}`);
  }
  return raw;
}

function string(raw: object, key: string, label: string): string {
  const value: unknown = Reflect.get(raw, key);
  if (typeof value !== "string") {
    throw new Error(`${label}.${key}: expected a string: ${JSON.stringify(value)}`);
  }
  return value;
}

function step(raw: object, label: string): Step {
  const value = string(raw, "step", label);
  const found = STEPS.find((known) => known === value);
  if (found === undefined) {
    throw new Error(`${label}.step: unknown step: ${JSON.stringify(value)}`);
  }
  return found;
}

function parseCheck(raw: unknown, label: string): Check {
  const check = object(raw, label);
  const state = string(check, "state", label);
  switch (state) {
    case "unchecked":
      return { state };
    case "done": {
      const behind: unknown = Reflect.get(check, "behind");
      if (typeof behind !== "number" || !Number.isInteger(behind) || behind < 0) {
        throw new Error(`${label}.behind: expected a count: ${JSON.stringify(behind)}`);
      }
      return { state, behind };
    }
    case "failed":
      return { state, message: string(check, "message", label) };
    default:
      throw new Error(`${label}.state: unknown state: ${JSON.stringify(state)}`);
  }
}

function parseRun(raw: unknown, label: string): Run {
  const run = object(raw, label);
  const state = string(run, "state", label);
  switch (state) {
    case "idle":
    case "built":
      return { state };
    case "running":
      return { state, step: step(run, label) };
    case "failed":
      return { state, step: step(run, label), output: string(run, "output", label) };
    default:
      throw new Error(`${label}.state: unknown state: ${JSON.stringify(state)}`);
  }
}

function parseChangelog(raw: unknown, label: string): Commit[] {
  if (!Array.isArray(raw)) {
    throw new Error(`${label}: expected an array: ${JSON.stringify(raw)}`);
  }
  return raw.map((item: unknown, index) => {
    const commit = object(item, `${label}[${index}]`);
    return {
      sha: string(commit, "sha", `${label}[${index}]`),
      title: string(commit, "title", `${label}[${index}]`),
    };
  });
}

export function parseUpdateStatus(raw: unknown, label: string): UpdateStatus {
  const status = object(raw, label);
  return {
    check: parseCheck(Reflect.get(status, "check"), `${label}.check`),
    run: parseRun(Reflect.get(status, "run"), `${label}.run`),
    changelog: parseChangelog(Reflect.get(status, "changelog"), `${label}.changelog`),
  };
}

async function status(response: Response): Promise<UpdateStatus> {
  if (!response.ok) {
    const text = await response.text().catch(() => "");
    throw new Error(`HTTP ${response.status}${text === "" ? "" : `: ${text}`}`);
  }
  return parseUpdateStatus(await response.json(), "update status");
}

export async function fetchUpdateStatus(): Promise<UpdateStatus> {
  return status(await fetch("api/update", { cache: "no-store" }));
}

/** Fetches upstream now; answers the last status at once while an update runs. */
export async function checkForUpdate(): Promise<UpdateStatus> {
  return status(await fetch("api/update/check", { method: "POST" }));
}

/** Starts pulling and rebuilding; progress comes from `fetchUpdateStatus`. */
export async function startUpdate(): Promise<UpdateStatus> {
  return status(await fetch("api/update/run", { method: "POST" }));
}

export const STEP_LABELS: Record<Step, string> = {
  clean: "Checking for local changes",
  pull: "Pulling",
  install: "Installing web dependencies",
  web: "Building the web app",
  server: "Building the server",
};
