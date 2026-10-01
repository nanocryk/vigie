import { describe, expect, it } from "vitest";

import { type CommandCard } from "@/commands";
import { afterDismissal, complete, drawerFor, slashWord, suggest } from "@/completion";

function card(name: string, argumentHint = ""): CommandCard {
  return { name, title: null, description: `${name} does things.`, argumentHint, own: null };
}

const CARDS = [
  card("canif-suggest"),
  card("code-review", "[low|high]"),
  card("compact", "[instructions]"),
  card("context"),
  card("review", "[pr]"),
];

const names = (cards: readonly { card: CommandCard }[]) => cards.map((entry) => entry.card.name);

describe("slashWord", () => {
  it("finds a slash word at the start or after whitespace", () => {
    expect(slashWord("/ca", 3)).toEqual({ start: 0, end: 3, query: "ca" });
    expect(slashWord("then /rev it", 8)).toEqual({ start: 5, end: 9, query: "re" });
    expect(slashWord("a\n/x", 4)).toEqual({ start: 2, end: 4, query: "x" });
  });

  it("ignores a slash inside a word or a caret past the word", () => {
    expect(slashWord("a/b", 3)).toBeNull();
    expect(slashWord("/ca ", 4)).toBeNull();
    expect(slashWord("plain", 5)).toBeNull();
  });
});

describe("suggest", () => {
  it("ranks fuzzy matches and caps them", () => {
    expect(names(suggest(CARDS, "cs"))[0]).toBe("canif-suggest");
    expect(names(suggest(CARDS, "rev"))).toEqual(["review", "code-review"]);
    expect(suggest(CARDS, "co")).toHaveLength(3);
    expect(suggest(CARDS, "zzz")).toEqual([]);
  });

  it("lists the first commands for a bare slash", () => {
    expect(names(suggest(CARDS, ""))).toEqual(["canif-suggest", "code-review", "compact"]);
  });

  it("puts recent commands first for a bare slash, skipping unknown ones", () => {
    expect(names(suggest(CARDS, "", ["review", "gone", "compact"]))).toEqual([
      "review",
      "compact",
      "canif-suggest",
    ]);
    expect(names(suggest(CARDS, "re", ["code-review"]))[0]).toBe("review");
  });

  it("gives the matched letters", () => {
    expect(suggest(CARDS, "review")[0]?.indexes).toEqual([0, 1, 2, 3, 4, 5]);
  });
});

describe("complete", () => {
  it("replaces the word and puts the caret after one space", () => {
    expect(complete("run /ca now", { start: 4, end: 7, query: "ca" }, "canif-suggest")).toEqual({
      text: "run /canif-suggest now",
      caret: 19,
    });
    expect(complete("/re", { start: 0, end: 3, query: "re" }, "review")).toEqual({
      text: "/review ",
      caret: 8,
    });
  });
});

describe("drawerFor", () => {
  it("suggests while the caret is in a slash word", () => {
    const drawer = drawerFor("/rev", 4, CARDS);
    expect(drawer).toMatchObject({ kind: "suggest", key: "suggest:0" });
    expect(drawerFor("/zzz", 4, CARDS)).toEqual({ kind: "none" });
  });

  it("shows argument help after a known command on the caret's line", () => {
    expect(drawerFor("/review ", 8, CARDS)).toMatchObject({
      kind: "argument",
      key: "argument:0:review",
      card: { name: "review" },
    });
    expect(drawerFor("go /code-review hi", 18, CARDS)).toMatchObject({
      kind: "argument",
      card: { name: "code-review" },
    });
    expect(drawerFor("/review\nnext", 12, CARDS)).toEqual({ kind: "none" });
    expect(drawerFor("/context ", 9, CARDS)).toEqual({ kind: "none" });
    expect(drawerFor("/unknown ", 9, CARDS)).toEqual({ kind: "none" });
  });

  it("shows nothing for a selection", () => {
    expect(drawerFor("/re", null, CARDS)).toEqual({ kind: "none" });
  });
});

describe("afterDismissal", () => {
  it("hides the same drawer and lapses on another", () => {
    const argument = drawerFor("/review x", 9, CARDS);
    expect(afterDismissal(argument, "argument:0:review")).toEqual({
      drawer: { kind: "none" },
      dismissed: "argument:0:review",
    });
    expect(afterDismissal(drawerFor("/re", 3, CARDS), "suggest:0").drawer).toEqual({
      kind: "none",
    });
    const moved = drawerFor("a /re", 5, CARDS);
    expect(afterDismissal(moved, "suggest:0")).toEqual({ drawer: moved, dismissed: null });
    expect(afterDismissal({ kind: "none" }, "suggest:0").dismissed).toBeNull();
  });
});
