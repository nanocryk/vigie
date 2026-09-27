import { type CacheGauge, formatPercent, formatTokens } from "@/gauges";
import { CACHE_COLD, type ContextView, type RestartMode } from "@/wire";

export type ColdChoice = "rewarm" | "continue" | "blank";

/** A choice and the cold spell (its `coldAt`) it was made for. */
export interface ColdChoiceMade {
  spell: number;
  choice: ColdChoice;
}

/** The server refused a send or an answer because the cache is cold. */
export class CacheColdError extends Error {
  constructor() {
    super("The cache is cold: pick rewarm, continue or blank first");
    this.name = "CacheColdError";
  }
}

export function ackError(error: string | null | undefined): Error {
  if (error === CACHE_COLD) return new CacheColdError();
  return new Error(error ?? "Refused by the server");
}

/**
 * The latest cold spell known: the gauge's, or the one the server reported by
 * refusing at `refusedAt` while the client still counted the cache warm.
 */
export function coldSpell(gauge: CacheGauge, refusedAt: number | null): number | null {
  const own = gauge.kind === "cold" ? gauge.coldAt : null;
  if (own === null) return refusedAt;
  if (refusedAt === null) return own;
  return Math.max(own, refusedAt);
}

/** Spread into a send or an answer: `rewarm` once Rewarm was picked for the spell in force. */
export function rewarmField(
  spell: number | null,
  chosen: ColdChoiceMade | null,
): { rewarm?: true } {
  if (spell === null || chosen === null) return {};
  return chosen.spell === spell && chosen.choice === "rewarm" ? { rewarm: true } : {};
}

export type ColdAction =
  | { kind: "send"; text: string }
  | { kind: "unblock" }
  | { kind: "restart"; mode: RestartMode; text: string | null };

/**
 * The cold-cache panel replaces the composer while the cache is cold and no turn
 * runs, and replaces the ask card whenever an ask waits on a cold cache. A server
 * refusal shows it even during a turn. A choice made for one cold spell holds
 * until the next one.
 */
export function coldPanelShown(input: {
  gauge: CacheGauge;
  refusedAt: number | null;
  busy: boolean;
  asks: number;
  chosen: ColdChoiceMade | null;
}): boolean {
  const spell = coldSpell(input.gauge, input.refusedAt);
  if (spell === null || input.chosen?.spell === spell) return false;
  return spell === input.refusedAt || !input.busy || input.asks > 0;
}

/** What a choice does with the composer draft. */
export function coldAction(choice: ColdChoice, draft: string): ColdAction {
  const text = draft.trim() === "" ? null : draft;
  switch (choice) {
    case "rewarm":
      return text === null ? { kind: "unblock" } : { kind: "send", text };
    case "continue":
    case "blank":
      return { kind: "restart", mode: choice, text };
  }
}

/** `Context 23% (230k of 1M tokens)`, or a note that it is unknown. */
export function contextSummary(context: ContextView | null): string {
  if (context === null) return "Context usage unknown";
  return `Context ${formatPercent(context.percentage)} (${formatTokens(context.used_tokens)} of ${formatTokens(context.max_tokens)} tokens)`;
}
