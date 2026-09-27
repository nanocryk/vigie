import { describe, expect, it } from "vitest";

import {
  type SettingsFlow,
  appliedModelName,
  appliedOutcome,
  defaultEffort,
  effortLevels,
  effortTabLabel,
  firstButton,
  modelOptionLabel,
  modelTabLabel,
  pickEffort,
  pickModel,
  settingsPatch,
  settingsPlan,
} from "@/settings";
import type { ModelChoice } from "@/wire";

const LEVELS = ["low", "medium", "high", "xhigh", "max"];
const model = (
  value: string,
  resolved_model: string | null,
  display_name: string,
  effort_levels: string[],
): ModelChoice => ({ value, resolved_model, display_name, description: null, effort_levels });

const MODELS: ModelChoice[] = [
  model("default", "claude-opus-5-5", "Default (recommended)", LEVELS),
  model("opus", "claude-opus-5-5", "Opus 5.5", LEVELS),
  model("haiku", "claude-haiku-4-5-20251001", "Haiku 4.5", []),
  model("odd", null, "Odd", ["low"]),
];

const current = { model: "opus", effort: "high" };

describe("labels and levels", () => {
  it("names an applied model after its catalog entry, never the CLI's default one", () => {
    expect(appliedModelName(MODELS, "claude-opus-5-5[1m]")).toBe("Opus 5.5");
    expect(appliedModelName(MODELS, "claude-haiku-4-5-20251001")).toBe("Haiku 4.5");
    expect(appliedModelName(MODELS, "claude-x[1m]")).toBe("claude-x[1m]");
    const defaultOnly = [model("default", "claude-opus-5-5", "Default (recommended)", LEVELS)];
    expect(appliedModelName(defaultOnly, "claude-opus-5-5")).toBe("claude-opus-5-5");
    expect(appliedModelName([], "claude-opus-5-5[1m]")).toBe("claude-opus-5-5[1m]");
  });

  it("shows what the CLI applies, else the pick, else an ellipsis", () => {
    expect(modelTabLabel(MODELS, "claude-opus-5-5[1m]", null, false, null)).toBe("Opus 5.5");
    expect(modelTabLabel(MODELS, "claude-opus-5-5[1m]", "haiku", false, null)).toBe("Opus 5.5");
    expect(modelTabLabel(MODELS, null, "haiku", false, null)).toBe("Haiku 4.5");
    expect(modelTabLabel(MODELS, null, "claude-x", false, null)).toBe("claude-x");
    expect(modelTabLabel(MODELS, null, null, false, null)).toBe("Opus 5.5");
    expect(modelTabLabel([], null, null, false, null)).toBe("...");
    expect(modelTabLabel(MODELS, "claude-opus-5-5[1m]", "haiku", true, null)).toBe("Haiku 4.5");
    expect(modelTabLabel(MODELS, "claude-haiku-4-5-20251001", null, true, null)).toBe("Opus 5.5");
    expect(effortTabLabel("medium", null, false, null)).toBe("medium effort");
    expect(effortTabLabel("medium", "max", false, null)).toBe("medium effort");
    expect(effortTabLabel(null, "max", false, null)).toBe("max effort");
    expect(effortTabLabel(null, null, false, null)).toBe("...");
    expect(effortTabLabel("medium", "max", true, null)).toBe("max effort");
    expect(effortTabLabel("medium", null, true, null)).toBe("medium effort");
  });

  it("falls back on the shared defaults for a discussion without applied values or a pick", () => {
    const shared = "claude-opus-5-5[1m]";
    expect(modelTabLabel(MODELS, null, null, false, shared)).toBe("Opus 5.5");
    expect(modelTabLabel([], null, null, false, shared)).toBe(shared);
    expect(modelTabLabel(MODELS, null, "haiku", false, shared)).toBe("Haiku 4.5");
    expect(modelTabLabel(MODELS, "claude-haiku-4-5-20251001", null, false, shared)).toBe("Haiku 4.5");
    expect(modelTabLabel(MODELS, null, null, true, "claude-haiku-4-5-20251001")).toBe("Haiku 4.5");
    expect(effortTabLabel(null, null, false, "medium")).toBe("medium effort");
    expect(effortTabLabel(null, "max", false, "medium")).toBe("max effort");
    expect(effortTabLabel("high", null, false, "medium")).toBe("high effort");
    expect(effortTabLabel(null, null, true, "medium")).toBe("medium effort");
  });

  it("keeps the shared default effort only where the model offers it", () => {
    expect(defaultEffort(MODELS, null, "medium")).toBe("medium");
    expect(defaultEffort(MODELS, "opus", "medium")).toBe("medium");
    expect(defaultEffort(MODELS, "haiku", "medium")).toBeNull();
    expect(defaultEffort(MODELS, "odd", "medium")).toBeNull();
    expect(defaultEffort(MODELS, "opus", null)).toBeNull();
  });

  it("names the model the CLI's default entry stands for in the list", () => {
    const defaultEntry = model("default", "claude-opus-5-5", "Default (recommended)", LEVELS);
    const opus = model("opus", "claude-opus-5-5", "Opus 5.5", LEVELS);
    expect(modelOptionLabel(MODELS, defaultEntry)).toBe("Default (Opus 5.5)");
    expect(modelOptionLabel(MODELS, opus)).toBe("Opus 5.5");
    const unresolved = model("default", null, "Default (recommended)", LEVELS);
    expect(modelOptionLabel([unresolved], unresolved)).toBe("Default (recommended)");
  });

  it("lists the effort levels of the picked model", () => {
    expect(effortLevels(MODELS, null)).toEqual(LEVELS);
    expect(effortLevels(MODELS, "haiku")).toEqual([]);
    expect(effortLevels(MODELS, "unknown")).toEqual([]);
  });
});

describe("picks", () => {
  it("drops an effort level the new model does not support", () => {
    expect(pickModel(current, "haiku", MODELS)).toEqual({ model: "haiku", effort: null });
    expect(pickModel(current, "odd", MODELS)).toEqual({ model: "odd", effort: null });
    expect(pickModel(current, "default", MODELS)).toEqual({ model: null, effort: "high" });
    expect(pickEffort(current, "max")).toEqual({ model: "opus", effort: "max" });
  });
});

describe("firstButton", () => {
  it("resumes for free when nothing changed, else rewarms", () => {
    expect(firstButton(current, current)).toBe("resume");
    expect(firstButton({ ...current, effort: "max" }, current)).toBe("rewarm");
    expect(firstButton({ model: null, effort: null }, { model: null, effort: null })).toBe("resume");
  });
});

describe("settingsPatch and settingsPlan", () => {
  it("carries only the changed fields", () => {
    expect(settingsPatch(current, current)).toBeNull();
    expect(settingsPatch({ model: "haiku", effort: null }, current)).toEqual({
      model: "haiku",
      effort: null,
    });
    expect(settingsPatch({ ...current, effort: "low" }, current)).toEqual({ effort: "low" });
  });

  it("plans each choice", () => {
    const picks = { ...current, model: "default" };
    expect(settingsPlan(current, current, "rewarm", "hi")).toEqual({
      patch: null,
      then: { kind: "close" },
    });
    expect(settingsPlan(picks, current, "rewarm", " ")).toEqual({
      patch: { model: "default" },
      then: { kind: "rewarm", text: null },
    });
    expect(settingsPlan(picks, current, "rewarm", "go")).toEqual({
      patch: { model: "default" },
      then: { kind: "rewarm", text: "go" },
    });
    expect(settingsPlan(picks, current, "continue", "")).toEqual({
      patch: { model: "default" },
      then: { kind: "restart", mode: "continue", text: null },
    });
    expect(settingsPlan(current, current, "blank", "next")).toEqual({
      patch: null,
      then: { kind: "restart", mode: "blank", text: "next" },
    });
  });
});

describe("appliedOutcome", () => {
  const picks = { model: "haiku", effort: null };
  const rewarming: SettingsFlow = { kind: "applying", picks, choice: "rewarm" };
  const restarting: SettingsFlow = { kind: "applying", picks, choice: "blank" };

  it("waits until the session list shows the picks", () => {
    expect(appliedOutcome(rewarming, { ...current, settingsChangedAt: 5 })).toEqual({ done: false });
    expect(appliedOutcome({ kind: "choosing", picks }, { ...picks, settingsChangedAt: 5 })).toEqual({
      done: false,
    });
  });

  it("answers the spell of the change with the rewarm", () => {
    expect(appliedOutcome(rewarming, { ...picks, settingsChangedAt: 5 })).toEqual({
      done: true,
      rewarmSpell: 5,
    });
    expect(appliedOutcome(rewarming, { ...picks, settingsChangedAt: null })).toEqual({
      done: true,
      rewarmSpell: null,
    });
  });

  it("ends a restart once the change is gone", () => {
    expect(appliedOutcome(restarting, { ...picks, settingsChangedAt: 5 })).toEqual({ done: false });
    expect(appliedOutcome(restarting, { ...picks, settingsChangedAt: null })).toEqual({
      done: true,
      rewarmSpell: null,
    });
  });
});
