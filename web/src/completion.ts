// The composer's slash drawer: command suggestions for a `/word` under the caret,
// then the argument help of the command the caret follows. Pure, so the flow is
// testable without a DOM.

import fuzzysort from "fuzzysort";

import type { CommandCard } from "@/commands";

export const SUGGESTIONS = 3;

/** A `/word` under the caret, at the start of the text or after whitespace. */
export interface SlashWord {
  /** Index of the slash. */
  start: number;
  /** Index after the word, past the caret. */
  end: number;
  /** Between the slash and the caret. */
  query: string;
}

export interface Suggestion {
  card: CommandCard;
  /** Indexes of the name's letters the query matched. */
  indexes: readonly number[];
}

/** `key` names what the drawer shows, so a dismissal holds until it changes. */
export type Drawer =
  | { kind: "none" }
  | { kind: "suggest"; key: string; word: SlashWord; suggestions: Suggestion[] }
  | { kind: "argument"; key: string; card: CommandCard };

const NONE: Drawer = { kind: "none" };

const isSpace = (char: string | undefined) => char !== undefined && /\s/.test(char);

export function slashWord(text: string, caret: number): SlashWord | null {
  let start = caret;
  while (start > 0 && !isSpace(text[start - 1])) start -= 1;
  if (text[start] !== "/") return null;
  let end = caret;
  while (end < text.length && !isSpace(text[end])) end += 1;
  return { start, end, query: text.slice(start + 1, caret) };
}

/**
 * The best matches of the query on command names. For an empty query, the commands
 * run lately (`recent`, newest first), then the first others in order.
 */
export function suggest(
  cards: readonly CommandCard[],
  query: string,
  recent: readonly string[] = [],
): Suggestion[] {
  if (query === "") {
    const used = recent.flatMap((name) => cards.filter((card) => card.name === name));
    const rest = cards.filter((card) => !used.includes(card));
    return [...used, ...rest].slice(0, SUGGESTIONS).map((card) => ({ card, indexes: [] }));
  }
  return fuzzysort
    .go(query, cards, { key: "name", limit: SUGGESTIONS, threshold: 0 })
    .map((result) => ({ card: result.obj, indexes: result.indexes }));
}

/** The word replaced by `/name` and a space, the caret after the space. */
export function complete(
  text: string,
  word: SlashWord,
  name: string,
): { text: string; caret: number } {
  const head = `${text.slice(0, word.start)}/${name} `;
  return { text: head + text.slice(word.end).replace(/^ /, ""), caret: head.length };
}

/**
 * The command whose arguments the caret is in: the last known `/name` before it on
 * its line, followed by whitespace. Null when that command takes no argument.
 */
function argumentCommand(
  text: string,
  caret: number,
  cards: readonly CommandCard[],
): { start: number; card: CommandCard } | null {
  const lineStart = text.lastIndexOf("\n", caret - 1) + 1;
  const line = text.slice(lineStart, caret);
  const found = [...line.matchAll(/(?<=^|\s)\/(\S+)(?=\s)/g)].reverse();
  for (const match of found) {
    const card = cards.find((entry) => entry.name === match[1]);
    if (card === undefined) continue;
    if (card.argumentHint.trim() === "") return null;
    return { start: lineStart + match.index, card };
  }
  return null;
}

/** What the drawer shows for the text and caret; `caret` is null for a selection. */
export function drawerFor(
  text: string,
  caret: number | null,
  cards: readonly CommandCard[],
  recent: readonly string[] = [],
): Drawer {
  if (caret === null) return NONE;
  const word = slashWord(text, caret);
  if (word !== null) {
    const suggestions = suggest(cards, word.query, recent);
    if (suggestions.length === 0) return NONE;
    return { kind: "suggest", key: `suggest:${word.start}`, word, suggestions };
  }
  const command = argumentCommand(text, caret, cards);
  if (command === null) return NONE;
  return {
    kind: "argument",
    key: `argument:${command.start}:${command.card.name}`,
    card: command.card,
  };
}

/** A dismissal hides the drawer while it shows the same thing, and lapses once it does not. */
export function afterDismissal(
  drawer: Drawer,
  dismissed: string | null,
): { drawer: Drawer; dismissed: string | null } {
  if (dismissed === null) return { drawer, dismissed };
  if (drawer.kind !== "none" && drawer.key === dismissed) return { drawer: NONE, dismissed };
  return { drawer, dismissed: null };
}
