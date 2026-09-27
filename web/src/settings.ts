// The model and effort change flow: picks, labels, the choice panel's first
// button, and what each choice sends. Pure, so the flow is testable without a DOM.

import type { ColdChoice } from "@/cold";
import type { ModelChoice, RestartMode } from "@/wire";

/** Null lets the CLI pick. */
export interface Settings {
  model: string | null;
  effort: string | null;
}

/** The CLI's entry for its own pick, used when no model is stored. */
export const DEFAULT_MODEL = "default";

export type SettingsFlow =
  | { kind: "closed" }
  /** The choice panel shows with both tabs open on their lists. */
  | { kind: "choosing"; picks: Settings }
  /** A choice was sent; the flow ends once the session list reflects it. */
  | { kind: "applying"; picks: Settings; choice: ColdChoice };

export const closedFlow: SettingsFlow = { kind: "closed" };

export function sameSettings(a: Settings, b: Settings): boolean {
  return a.model === b.model && a.effort === b.effort;
}

export function modelEntry(models: readonly ModelChoice[], model: string | null): ModelChoice | null {
  return models.find((entry) => entry.value === (model ?? DEFAULT_MODEL)) ?? null;
}

/** Effort levels of `model`; empty for a model without effort or an unknown one. */
export function effortLevels(models: readonly ModelChoice[], model: string | null): string[] {
  return modelEntry(models, model)?.effort_levels ?? [];
}

/** Shown while nothing tells which model or effort applies. */
export const UNKNOWN_LABEL = "...";

/** Display name without its parenthesized note: `Default (recommended)` reads `Default`. */
function shortName(entry: ModelChoice): string {
  return entry.display_name.replace(/\s*\(.*\)\s*$/, "") || entry.value;
}

/**
 * Catalog name of a model id the CLI reports, its `[1m]` suffix dropped, a
 * named entry preferred over the CLI's default one; the raw id when none matches.
 */
export function appliedModelName(models: readonly ModelChoice[], applied: string): string {
  const id = applied.replace(/\[[^\]]*\]$/, "");
  const entry = models.find((model) => model.value !== DEFAULT_MODEL && model.resolved_model === id);
  return entry === undefined ? applied : shortName(entry);
}

/** Name of a stored or picked model; null for the CLI's pick when the catalog does not resolve it. */
function pickedModelName(models: readonly ModelChoice[], model: string | null): string | null {
  const entry = modelEntry(models, model);
  if (entry === null) return model;
  if (entry.value !== DEFAULT_MODEL) return shortName(entry);
  return entry.resolved_model === null ? null : appliedModelName(models, entry.resolved_model);
}

/** A model list entry; the CLI's default one names the model it stands for: `Default (Opus 5.5)`. */
export function modelOptionLabel(models: readonly ModelChoice[], entry: ModelChoice): string {
  if (entry.value !== DEFAULT_MODEL || entry.resolved_model === null) return entry.display_name;
  return `${shortName(entry)} (${appliedModelName(models, entry.resolved_model)})`;
}

/**
 * The model tab: the pick while choosing, else what the CLI applies, else the
 * stored pick, else `fallback`, what the CLI applies to any discussion without
 * a pick; never the CLI's "Default".
 */
export function modelTabLabel(
  models: readonly ModelChoice[],
  applied: string | null,
  pick: string | null,
  choosing: boolean,
  fallback: string | null,
): string {
  if (!choosing && applied !== null) return appliedModelName(models, applied);
  if (pick === null && fallback !== null) return appliedModelName(models, fallback);
  return pickedModelName(models, pick) ?? UNKNOWN_LABEL;
}

/**
 * The effort tab, e.g. `medium effort`: the pick while choosing, else what the
 * CLI applies, else the stored pick, else `fallback`.
 */
export function effortTabLabel(
  applied: string | null,
  pick: string | null,
  choosing: boolean,
  fallback: string | null,
): string {
  const effort = (choosing ? (pick ?? applied) : (applied ?? pick)) ?? fallback;
  return effort === null ? UNKNOWN_LABEL : `${effort} effort`;
}

/**
 * The effort the CLI applies with no effort picked, as it applies it without
 * any pick (`shared`): kept for no model pick or a model that offers that
 * level, else null.
 */
export function defaultEffort(
  models: readonly ModelChoice[],
  model: string | null,
  shared: string | null,
): string | null {
  if (shared === null || model === null) return shared;
  return effortLevels(models, model).includes(shared) ? shared : null;
}

/**
 * The CLI's default entry stores no model. A model without the picked effort
 * level drops it, back to the CLI's pick.
 */
export function pickModel(picks: Settings, value: string, models: readonly ModelChoice[]): Settings {
  const model = value === DEFAULT_MODEL ? null : value;
  const levels = effortLevels(models, model);
  const effort = picks.effort !== null && levels.includes(picks.effort) ? picks.effort : null;
  return { model, effort };
}

export function pickEffort(picks: Settings, effort: string): Settings {
  return { ...picks, effort };
}

/**
 * Green "Resume, same model" when nothing changed (no cache cost), red
 * "Rewarm with new settings" when something did.
 */
export type FirstButton = "resume" | "rewarm";

export function firstButton(picks: Settings, current: Settings): FirstButton {
  return sameSettings(picks, current) ? "resume" : "rewarm";
}

/** Fields of a `set_settings` carrying what changed; null when nothing did. */
export function settingsPatch(
  picks: Settings,
  current: Settings,
): { model?: string | null; effort?: string | null } | null {
  if (sameSettings(picks, current)) return null;
  return {
    ...(picks.model === current.model ? {} : { model: picks.model }),
    ...(picks.effort === current.effort ? {} : { effort: picks.effort }),
  };
}

/** What a choice of the panel does, in order: store the settings, then `then`. */
export interface SettingsPlan {
  patch: { model?: string | null; effort?: string | null } | null;
  then:
    | { kind: "close" }
    /** Sends carry `rewarm` once the change shows in the session list; the draft, if any, goes now. */
    | { kind: "rewarm"; text: string | null }
    | { kind: "restart"; mode: RestartMode; text: string | null };
}

export function settingsPlan(
  picks: Settings,
  current: Settings,
  choice: ColdChoice,
  draft: string,
): SettingsPlan {
  const patch = settingsPatch(picks, current);
  const text = draft.trim() === "" ? null : draft;
  switch (choice) {
    case "rewarm":
      return patch === null
        ? { patch, then: { kind: "close" } }
        : { patch, then: { kind: "rewarm", text } };
    case "continue":
    case "blank":
      return { patch, then: { kind: "restart", mode: choice, text } };
  }
}

/**
 * While applying, once the session list shows the picks: the flow ends, and a
 * rewarm answers the cold spell the change started, if the server reports one.
 */
export function appliedOutcome(
  flow: SettingsFlow,
  view: Settings & { settingsChangedAt: number | null },
): { done: false } | { done: true; rewarmSpell: number | null } {
  if (flow.kind !== "applying" || !sameSettings(flow.picks, view)) return { done: false };
  if (flow.choice !== "rewarm") {
    return view.settingsChangedAt === null ? { done: true, rewarmSpell: null } : { done: false };
  }
  return { done: true, rewarmSpell: view.settingsChangedAt };
}
