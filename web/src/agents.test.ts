import { describe, expect, it } from "vitest";

import {
  type AgentEntry,
  agentCold,
  agentEntries,
  agentStateLabel,
  agentTree,
  goToAgent,
  recentAgents,
  workingLabel,
} from "@/agents";
import type { AgentInfo, PendingAsk } from "@/transcript";
import type { AgentView } from "@/wire";

const NOW = Date.parse("2026-09-28T12:00:00Z");

function view(taskId: string, toolUseId: string, change: Partial<AgentView> = {}): AgentView {
  return {
    task_id: taskId,
    tool_use_id: toolUseId,
    name: taskId,
    subagent_type: "general-purpose",
    parent_tool_use_id: null,
    state: "running",
    activity: null,
    cache: null,
    prompt_tokens: 0,
    started_at: "2026-09-28T11:00:00Z",
    ...change,
  };
}

function info(toolUseId: string, change: Partial<AgentInfo> = {}): AgentInfo {
  return {
    toolUseId,
    taskId: null,
    name: toolUseId,
    parentToolUseId: null,
    state: null,
    lastActive: null,
    ...change,
  };
}

function ask(agentId: string | null): PendingAsk {
  return {
    requestId: "r",
    toolName: "Bash",
    toolUseId: "tb",
    input: {},
    danger: { dangerous: false, reasons: [] },
    acceptEdits: false,
    folder: null,
    plan: null,
    file: null,
    agentId,
    denyAt: null,
  };
}

describe("agent entries", () => {
  it("take the server's agents first, then those only the lines know", () => {
    const entries = agentEntries(
      [view("a1", "t1", { name: "sleeper" })],
      new Map([
        ["t0", info("t0", { name: "older", state: "completed" })],
        ["t1", info("t1", { name: "stale name" })],
      ]),
    );
    expect(entries.map((entry) => [entry.name, entry.live])).toEqual([
      ["sleeper", true],
      ["older", false],
    ]);
  });

  it("nest under the agent that started them", () => {
    const entries = agentEntries(
      [
        view("a1", "t1"),
        view("a3", "t3", { parent_tool_use_id: "t2" }),
        view("a2", "t2", { parent_tool_use_id: "t1" }),
        view("a4", "t4", { parent_tool_use_id: "gone" }),
      ],
      new Map(),
    );
    expect(agentTree(entries).map(({ entry, depth }) => [entry.taskId, depth])).toEqual([
      ["a1", 0],
      ["a2", 1],
      ["a3", 2],
      ["a4", 0],
    ]);
  });

  it("label their state, an unfinished one of an ended process as ended", () => {
    const [live, old] = agentEntries(
      [view("a1", "t1")],
      new Map([["t0", info("t0", { state: "running" })]]),
    ) as [AgentEntry, AgentEntry];
    expect(agentStateLabel(live)).toBe("working");
    expect(agentStateLabel(old)).toBe("ended");
  });

  it("list only those active within the hour, running or on screen", () => {
    const entries = agentEntries(
      [
        view("a1", "t1", { started_at: "2026-09-28T10:00:00Z" }),
        view("a2", "t2", { started_at: "2026-09-28T10:00:00Z", state: "completed" }),
        view("a3", "t3", {
          started_at: "2026-09-28T10:00:00Z",
          state: "completed",
          cache: { last_request_at: "2026-09-28T11:30:00Z", ttl_secs: 300 },
        }),
      ],
      new Map([
        ["t4", info("t4", { lastActive: Date.parse("2026-09-28T09:00:00Z") })],
        ["t5", info("t5", { lastActive: Date.parse("2026-09-28T11:10:00Z") })],
        ["t6", info("t6", { lastActive: Date.parse("2026-09-28T09:00:00Z") })],
      ]),
    );
    expect(recentAgents(entries, "t6", NOW).map((entry) => entry.toolUseId)).toEqual([
      "t1",
      "t3",
      "t5",
      "t6",
    ]);
  });
});

describe("the working pill", () => {
  it("counts the running agents and hides without any", () => {
    expect(workingLabel([])).toBeNull();
    expect(workingLabel([view("a1", "t1", { state: "completed" })])).toBe("0 agents working");
    expect(workingLabel([view("a1", "t1"), view("a2", "t2", { state: "stopped" })])).toBe(
      "1 agent working",
    );
    expect(workingLabel([view("a1", "t1"), view("a2", "t2")])).toBe("2 agents working");
  });
});

describe("go to agent", () => {
  const entries = agentEntries([view("a1", "t1", { name: "sleeper" })], new Map());

  it("is offered from any other discussion than the ask's own", () => {
    expect(goToAgent(ask("a1"), null, entries)).toEqual({ toolUseId: "t1", name: "sleeper" });
    expect(goToAgent(ask("a1"), "t1", entries)).toBeNull();
    expect(goToAgent(ask(null), "t1", entries)).toEqual({ toolUseId: null, name: "main" });
    expect(goToAgent(ask(null), null, entries)).toBeNull();
  });

  it("is not offered for an agent without a known discussion", () => {
    expect(goToAgent(ask("a9"), null, entries)).toBeNull();
  });
});

describe("agent cache", () => {
  it("goes cold after its own TTL from its last request", () => {
    const [entry] = agentEntries(
      [view("a1", "t1", { cache: { last_request_at: "2026-09-28T11:56:00Z", ttl_secs: 300 } })],
      new Map(),
    ) as [AgentEntry];
    expect(agentCold(entry, NOW)).toBe(false);
    expect(agentCold(entry, NOW + 60_000)).toBe(true);
    expect(agentCold({ ...entry, cache: null }, NOW)).toBe(false);
  });
});
