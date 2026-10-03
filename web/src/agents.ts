import type { CacheGauge } from "@/gauges";
import type { AgentInfo, PendingAsk } from "@/transcript";
import type { AgentState, AgentView, CacheView } from "@/wire";

/** Name of the main agent where an agent's name goes. */
export const MAIN_AGENT = "main";

/** An agent as the panel lists it: from the server while its process runs, else from the lines. */
export interface AgentEntry {
  /** The Agent call; the key of its discussion. */
  toolUseId: string;
  taskId: string | null;
  name: string;
  parentToolUseId: string | null;
  /** Null when no line said. */
  state: AgentState | null;
  activity: string | null;
  cache: CacheView | null;
  promptTokens: number;
  /** Known to the running Claude process: it can take a relay and be stopped. */
  live: boolean;
  /** Latest time a line or the server saw it act; null when none did. */
  lastActive: number | null;
}

/** The panel hides agents idle for longer. */
export const RECENT_AGENT_MS = 60 * 60 * 1000;

function latest(...times: (number | null)[]): number | null {
  const known = times.filter((time): time is number => time !== null && !Number.isNaN(time));
  return known.length === 0 ? null : Math.max(...known);
}

/** The server's agents first, then those only the loaded lines know, oldest first. */
export function agentEntries(
  live: readonly AgentView[],
  known: ReadonlyMap<string, AgentInfo>,
): AgentEntry[] {
  const entries: AgentEntry[] = live.map((agent) => ({
    toolUseId: agent.tool_use_id,
    taskId: agent.task_id,
    name: agent.name,
    parentToolUseId: agent.parent_tool_use_id,
    state: agent.state,
    activity: agent.activity,
    cache: agent.cache,
    promptTokens: agent.prompt_tokens,
    live: true,
    lastActive: latest(
      Date.parse(agent.started_at),
      agent.cache === null ? null : Date.parse(agent.cache.last_request_at),
      known.get(agent.tool_use_id)?.lastActive ?? null,
    ),
  }));
  const listed = new Set(entries.map((entry) => entry.toolUseId));
  for (const info of known.values()) {
    if (listed.has(info.toolUseId)) continue;
    entries.push({
      toolUseId: info.toolUseId,
      taskId: info.taskId,
      name: info.name,
      parentToolUseId: info.parentToolUseId,
      state: info.state,
      activity: null,
      cache: null,
      promptTokens: 0,
      live: false,
      lastActive: info.lastActive,
    });
  }
  return entries;
}

/** What the panel lists: agents active within the hour, running ones, and the one on screen. */
export function recentAgents(
  entries: readonly AgentEntry[],
  viewing: string | null,
  now: number,
): AgentEntry[] {
  return entries.filter(
    (entry) =>
      entry.toolUseId === viewing ||
      (entry.live && entry.state === "running") ||
      entry.lastActive === null ||
      now - entry.lastActive <= RECENT_AGENT_MS,
  );
}

/** Depth-first: each agent under the one that started it; an unknown parent counts as the main agent. */
export function agentTree(entries: readonly AgentEntry[]): { entry: AgentEntry; depth: number }[] {
  const ids = new Set(entries.map((entry) => entry.toolUseId));
  const rootOf = (entry: AgentEntry) =>
    entry.parentToolUseId !== null && ids.has(entry.parentToolUseId) ? entry.parentToolUseId : null;
  const rows: { entry: AgentEntry; depth: number }[] = [];
  const visit = (parent: string | null, depth: number) => {
    for (const entry of entries) {
      if (rootOf(entry) !== parent) continue;
      rows.push({ entry, depth });
      visit(entry.toolUseId, depth + 1);
    }
  };
  visit(null, 0);
  return rows;
}

/** The thin bar's glance: `2 agents working`; null while the running process has no agent. */
export function workingLabel(live: readonly AgentView[]): string | null {
  if (live.length === 0) return null;
  const working = live.filter((agent) => agent.state === "running").length;
  return `${working} ${working === 1 ? "agent" : "agents"} working`;
}

/** The discussion the ask belongs to, null for the main one, and its agent's name. */
export function askOwner(
  ask: PendingAsk,
  entries: readonly AgentEntry[],
): { toolUseId: string | null; name: string } {
  if (ask.agentId === null) return { toolUseId: null, name: MAIN_AGENT };
  const entry = entries.find((candidate) => candidate.taskId === ask.agentId);
  return entry === undefined
    ? { toolUseId: null, name: ask.agentId }
    : { toolUseId: entry.toolUseId, name: entry.name };
}

/**
 * Where "Go to agent" leads for an ask seen from `viewing` (null: the main
 * discussion); null when the ask belongs to the discussion on screen, or to an
 * agent no discussion is known for.
 */
export function goToAgent(
  ask: PendingAsk,
  viewing: string | null,
  entries: readonly AgentEntry[],
): { toolUseId: string | null; name: string } | null {
  const owner = askOwner(ask, entries);
  if (ask.agentId !== null && owner.toolUseId === null) return null;
  return owner.toolUseId === viewing ? null : owner;
}

/**
 * When a relay to this agent started reloading its cache, given its cache `gauge`:
 * the TTL of its last request passed while it is not running. Null while a relay
 * costs nothing extra, a running agent's included: its next request reads it anyway.
 */
export function agentColdAt(entry: AgentEntry, gauge: CacheGauge): number | null {
  return entry.live && entry.state !== "running" && gauge.kind === "cold" ? gauge.coldAt : null;
}

const STATE_LABEL: Record<AgentState, string> = {
  running: "working",
  completed: "done",
  stopped: "stopped",
  failed: "failed",
};

export function agentStateLabel(entry: AgentEntry): string {
  if (entry.state === null) return entry.live ? "starting" : "ended";
  if (!entry.live && entry.state === "running") return "ended";
  return STATE_LABEL[entry.state];
}
