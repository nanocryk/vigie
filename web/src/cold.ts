import { attachmentsField, sendableText } from "@/attachments";
import { type CacheGauge, formatPercent, formatTokens } from "@/gauges";
import {
  CACHE_COLD,
  type ContextView,
  FOLDER_MISSING,
  HELD_GONE,
  type RestartMode,
  SESSION_LIVE,
} from "@/wire";

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

/** The held message acted on was already sent or deleted. */
export class HeldGoneError extends Error {
  constructor() {
    super("That queued message was already sent or deleted");
    this.name = "HeldGoneError";
  }
}

export function ackError(error: string | null | undefined): Error {
  if (error === CACHE_COLD) return new CacheColdError();
  if (error === SESSION_LIVE) return new Error("The session is running: kill it first");
  if (error === HELD_GONE) return new HeldGoneError();
  if (error === FOLDER_MISSING) return new Error("That folder no longer exists");
  return new Error(error ?? "Refused by the server");
}

/**
 * The latest cold spell known: the gauge's, the one a model or effort change
 * started at `settingsAt`, or the one the server reported by refusing at
 * `refusedAt` while the client still counted the cache warm.
 */
export function coldSpell(
  gauge: CacheGauge,
  refusedAt: number | null,
  settingsAt: number | null,
): number | null {
  const spells = [gauge.kind === "cold" ? gauge.coldAt : null, refusedAt, settingsAt].filter(
    (spell): spell is number => spell !== null,
  );
  return spells.length === 0 ? null : Math.max(...spells);
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
  | { kind: "send"; text: string; attachments?: string[] }
  | { kind: "unblock" }
  | { kind: "restore" }
  | { kind: "restart"; mode: RestartMode; text: string | null; attachments?: string[] };

/**
 * The cold-cache panel replaces the composer while the cache is cold and no turn
 * runs, and replaces the ask card whenever an ask waits on a cold cache. A server
 * refusal shows it even during a turn. A choice made for one cold spell holds
 * until the next one.
 */
export function coldPanelShown(input: {
  gauge: CacheGauge;
  refusedAt: number | null;
  settingsAt: number | null;
  busy: boolean;
  asks: number;
  chosen: ColdChoiceMade | null;
}): boolean {
  const spell = coldSpell(input.gauge, input.refusedAt, input.settingsAt);
  if (spell === null || input.chosen?.spell === spell) return false;
  return spell === input.refusedAt || !input.busy || input.asks > 0;
}

/** What a choice does with the composer draft: its text and attached files. */
export function coldAction(
  choice: ColdChoice,
  draft: string,
  attachments: readonly string[] = [],
): ColdAction {
  const text = sendableText(draft, attachments);
  const files = attachmentsField(attachments);
  switch (choice) {
    case "rewarm":
      return text === null ? { kind: "unblock" } : { kind: "send", text, ...files };
    case "continue":
    case "blank":
      return { kind: "restart", mode: choice, text, ...files };
  }
}

/** A choice from an archived discussion's panel: the server unarchives on the send or the restart; a bare rewarm restores it. */
export function archivedAction(
  choice: ColdChoice,
  draft: string,
  attachments: readonly string[] = [],
): ColdAction {
  const action = coldAction(choice, draft, attachments);
  return action.kind === "unblock" ? { kind: "restore" } : action;
}

/** `Context 23% (230k of 1M tokens)`, or a note that it is unknown. */
export function contextSummary(context: ContextView | null): string {
  if (context === null) return "Context usage unknown";
  return `Context ${formatPercent(context.percentage)} (${formatTokens(context.used_tokens)} of ${formatTokens(context.max_tokens)} tokens)`;
}
