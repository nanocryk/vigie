// The commands panel: its cards, their filter, and the state machine of the
// panel and its bottom field. Pure, so the flow is testable without a DOM.

import type { CommandInfo } from "@/wire";

/** vigie2's own commands, run by the app instead of being sent to Claude. */
export type OwnCommand = "clear" | "model" | "effort";

export interface CommandCard {
  name: string;
  /** Shown instead of `/name` for vigie2's own commands; null for Claude's. */
  title: string | null;
  description: string;
  argumentHint: string;
  own: OwnCommand | null;
}

export const OWN_COMMANDS: readonly CommandCard[] = [
  {
    name: "clear",
    title: "Clear",
    description: "Fresh Claude session with no context, marked by a Cleared divider.",
    argumentHint: "",
    own: "clear",
  },
  {
    name: "model",
    title: "Change model",
    description: "Stop here and pick another model; the cache reloads.",
    argumentHint: "",
    own: "model",
  },
  {
    name: "effort",
    title: "Change effort",
    description: "Stop here and pick another effort level; the cache reloads.",
    argumentHint: "",
    own: "effort",
  },
];

/**
 * vigie2's own commands first, then Claude's commands and skills, minus the
 * terminal-only ones and the ones vigie2's own replace.
 */
export function commandCards(
  commands: readonly CommandInfo[],
  terminal: readonly string[],
): CommandCard[] {
  const hidden = new Set([...terminal, ...OWN_COMMANDS.map((card) => card.name)]);
  const claude = commands
    .filter((command) => !hidden.has(command.name))
    .map(
      (command): CommandCard => ({
        name: command.name,
        title: null,
        description: command.description,
        argumentHint: command.argument_hint,
        own: null,
      }),
    );
  return [...OWN_COMMANDS, ...claude];
}

/** The command name a filter asks for: without its leading `/`, up to the first space, lowercased. */
export function commandQuery(filter: string): string {
  const word = filter.trimStart().replace(/^\//, "").split(/\s/, 1)[0] ?? "";
  return word.toLowerCase();
}

/** Cards whose name or title contains the query, in their order. */
export function filterCards(cards: readonly CommandCard[], filter: string): CommandCard[] {
  const query = commandQuery(filter);
  if (query === "") return [...cards];
  return cards.filter(
    (card) =>
      card.name.toLowerCase().includes(query) ||
      (card.title?.toLowerCase().includes(query) ?? false),
  );
}

/** Typing `/` as the composer's first character opens the panel. */
export function slashOpens(previous: string, next: string): boolean {
  return !previous.startsWith("/") && next.startsWith("/");
}

/**
 * `tab`: opened from the commands tab, the composer draft stays untouched.
 * `slash`: opened by typing `/` in the composer, whose text is the filter and is
 * consumed by the command.
 */
export type PanelSource = "tab" | "slash";

export type PanelState =
  | { kind: "closed" }
  | { kind: "browsing"; source: PanelSource; filter: string }
  /** The bottom field holds the command and its arguments. */
  | { kind: "argument"; source: PanelSource; text: string };

export type PanelEvent =
  | { type: "open_tab" }
  | { type: "open_slash"; text: string }
  | { type: "filter"; text: string }
  | { type: "use"; card: CommandCard }
  | { type: "argument"; text: string }
  | { type: "send" }
  | { type: "close" };

/** `consumeDraft`: the composer text was the command and goes away with it. */
export type PanelEffect =
  | { kind: "none" }
  | { kind: "send"; text: string; consumeDraft: boolean }
  | { kind: "own"; command: OwnCommand; consumeDraft: boolean }
  /** The filter of a `/` typed in the composer is the composer text. */
  | { kind: "draft"; text: string };

export const closedPanel: PanelState = { kind: "closed" };

const NONE: PanelEffect = { kind: "none" };

export function panelStep(
  state: PanelState,
  event: PanelEvent,
): { state: PanelState; effect: PanelEffect } {
  const stay = { state, effect: NONE };
  switch (event.type) {
    case "open_tab":
      return { state: { kind: "browsing", source: "tab", filter: "" }, effect: NONE };
    case "open_slash":
      return { state: { kind: "browsing", source: "slash", filter: event.text }, effect: NONE };
    case "close":
      return { state: closedPanel, effect: NONE };
    case "filter": {
      if (state.kind !== "browsing") return stay;
      if (state.source === "tab") return { state: { ...state, filter: event.text }, effect: NONE };
      const effect: PanelEffect = { kind: "draft", text: event.text };
      if (!event.text.startsWith("/")) return { state: closedPanel, effect };
      return { state: { ...state, filter: event.text }, effect };
    }
    case "use": {
      if (state.kind !== "browsing") return stay;
      const consumeDraft = state.source === "slash";
      const { card } = event;
      if (card.own !== null) {
        return { state: closedPanel, effect: { kind: "own", command: card.own, consumeDraft } };
      }
      if (card.argumentHint.trim() === "") {
        return {
          state: closedPanel,
          effect: { kind: "send", text: `/${card.name}`, consumeDraft },
        };
      }
      return {
        state: { kind: "argument", source: state.source, text: `/${card.name} ` },
        effect: NONE,
      };
    }
    case "argument":
      if (state.kind !== "argument") return stay;
      return { state: { ...state, text: event.text }, effect: NONE };
    case "send":
      if (state.kind !== "argument" || state.text.trim() === "") return stay;
      return {
        state: closedPanel,
        effect: { kind: "send", text: state.text, consumeDraft: state.source === "slash" },
      };
  }
}
