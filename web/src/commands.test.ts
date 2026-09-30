import { describe, expect, it } from "vitest";

import {
  type CommandCard,
  OWN_COMMANDS,
  type PanelEvent,
  type PanelState,
  closedPanel,
  commandCards,
  commandQuery,
  filterCards,
  panelStep,
  slashOpens,
} from "@/commands";
import type { CommandInfo } from "@/wire";

const COMMANDS: CommandInfo[] = [
  { name: "canif", description: "Use canif scripts.", argument_hint: "" },
  { name: "design", description: "Grant or revoke access.", argument_hint: "consent | revoke" },
  { name: "doctor", description: "Diagnose the install.", argument_hint: "" },
  { name: "clear", description: "Clear the conversation.", argument_hint: "" },
  { name: "model", description: "Set the model.", argument_hint: "[model]" },
];

function card(name: string): CommandCard {
  const found = commandCards(COMMANDS, []).find((entry) => entry.name === name && entry.own === null);
  if (found === undefined) throw new Error(`card: no Claude command ${JSON.stringify(name)}`);
  return found;
}

const [CLEAR, MODEL] = OWN_COMMANDS;

function run(state: PanelState, ...events: PanelEvent[]) {
  let current = state;
  let effect = panelStep(current, { type: "close" }).effect;
  for (const event of events) {
    ({ state: current, effect } = panelStep(current, event));
  }
  return { state: current, effect };
}

describe("commandCards", () => {
  it("lists vigie's own first, then Claude's without terminal-only or replaced ones", () => {
    const cards = commandCards(COMMANDS, ["doctor"]);
    expect(cards.map((entry) => [entry.name, entry.own])).toEqual([
      ["clear", "clear"],
      ["model", "model"],
      ["effort", "effort"],
      ["canif", null],
      ["design", null],
    ]);
    expect(cards[4]?.argumentHint).toBe("consent | revoke");
  });
});

describe("filterCards", () => {
  it("matches names and titles on the first word, without the slash", () => {
    const cards = commandCards(COMMANDS, []);
    expect(commandQuery("/Des consent")).toBe("des");
    expect(filterCards(cards, "/des").map((entry) => entry.name)).toEqual(["design"]);
    expect(filterCards(cards, "change").map((entry) => entry.name)).toEqual(["model", "effort"]);
    expect(filterCards(cards, "/")).toHaveLength(cards.length);
    expect(filterCards(cards, "")).toHaveLength(cards.length);
    expect(filterCards(cards, "/nothing")).toEqual([]);
  });
});

describe("slashOpens", () => {
  it("fires when a slash becomes the first character", () => {
    expect(slashOpens("", "/")).toBe(true);
    expect(slashOpens("hello", "/hello")).toBe(true);
    expect(slashOpens("/c", "/ca")).toBe(false);
    expect(slashOpens("", "a/")).toBe(false);
  });
});

describe("panelStep", () => {
  it("sends a command without argument at once, leaving a tab draft alone", () => {
    const { state, effect } = run(closedPanel, { type: "open_tab" }, { type: "use", card: card("canif") });
    expect(state).toEqual(closedPanel);
    expect(effect).toEqual({ kind: "send", text: "/canif", consumeDraft: false });
  });

  it("consumes a slash typed in the composer", () => {
    const { effect } = run(
      closedPanel,
      { type: "open_slash", text: "/ca" },
      { type: "use", card: card("canif") },
    );
    expect(effect).toEqual({ kind: "send", text: "/canif", consumeDraft: true });
  });

  it("turns the field into the argument entry for a command with arguments", () => {
    const opened = run(closedPanel, { type: "open_tab" }, { type: "use", card: card("design") });
    expect(opened.state).toEqual({ kind: "argument", source: "tab", text: "/design " });
    expect(opened.effect).toEqual({ kind: "none" });
    expect(run(opened.state, { type: "argument", text: "  " }, { type: "send" }).state).toEqual({
      kind: "argument",
      source: "tab",
      text: "  ",
    });
    const sent = run(opened.state, { type: "argument", text: "/design consent" }, { type: "send" });
    expect(sent.state).toEqual(closedPanel);
    expect(sent.effect).toEqual({ kind: "send", text: "/design consent", consumeDraft: false });
  });

  it("runs vigie's own commands in the app", () => {
    if (CLEAR === undefined || MODEL === undefined) throw new Error("own commands missing");
    expect(run(closedPanel, { type: "open_tab" }, { type: "use", card: CLEAR }).effect).toEqual({
      kind: "own",
      command: "clear",
      consumeDraft: false,
    });
    expect(
      run(closedPanel, { type: "open_slash", text: "/mod" }, { type: "use", card: MODEL }).effect,
    ).toEqual({ kind: "own", command: "model", consumeDraft: true });
  });

  it("keeps a slash filter in the composer and closes once the slash is gone", () => {
    const typing = run(closedPanel, { type: "open_slash", text: "/" }, { type: "filter", text: "/de" });
    expect(typing.state).toEqual({ kind: "browsing", source: "slash", filter: "/de" });
    expect(typing.effect).toEqual({ kind: "draft", text: "/de" });
    const erased = run(typing.state, { type: "filter", text: "de" });
    expect(erased.state).toEqual(closedPanel);
    expect(erased.effect).toEqual({ kind: "draft", text: "de" });
  });

  it("filters a tab panel without touching the draft", () => {
    const { state, effect } = run(closedPanel, { type: "open_tab" }, { type: "filter", text: "can" });
    expect(state).toEqual({ kind: "browsing", source: "tab", filter: "can" });
    expect(effect).toEqual({ kind: "none" });
  });

  it("ignores events that do not fit the state", () => {
    expect(run(closedPanel, { type: "send" }).state).toEqual(closedPanel);
    expect(run(closedPanel, { type: "use", card: card("canif") }).effect).toEqual({ kind: "none" });
    expect(run(closedPanel, { type: "open_tab" }, { type: "close" }).state).toEqual(closedPanel);
  });
});
