import { useSyncExternalStore } from "react";

import { isRecord } from "@/wire";

/** Root font size in percent: every rem-sized part of the app follows it. */
export const SCALES = [87.5, 100, 112.5, 125] as const;
export type Scale = (typeof SCALES)[number];

export const THEMES = ["system", "light", "dark"] as const;
export type ThemeChoice = (typeof THEMES)[number];
export type Theme = "light" | "dark";

/** Per device display preferences. */
export interface Display {
  scale: Scale;
  theme: ThemeChoice;
}

export const DEFAULT_DISPLAY: Display = { scale: 100, theme: "system" };

function isScale(value: unknown): value is Scale {
  return SCALES.some((scale) => scale === value);
}

function isThemeChoice(value: unknown): value is ThemeChoice {
  return THEMES.some((theme) => theme === value);
}

/** Each field falls back to its default on its own, so one bad field keeps the other. */
export function parseDisplay(text: string | null): Display {
  if (text === null) return DEFAULT_DISPLAY;
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return DEFAULT_DISPLAY;
  }
  if (!isRecord(raw)) return DEFAULT_DISPLAY;
  return {
    scale: isScale(raw.scale) ? raw.scale : DEFAULT_DISPLAY.scale,
    theme: isThemeChoice(raw.theme) ? raw.theme : DEFAULT_DISPLAY.theme,
  };
}

export function resolveTheme(choice: ThemeChoice, systemDark: boolean): Theme {
  if (choice === "system") return systemDark ? "dark" : "light";
  return choice;
}

/** Browser chrome color per theme; the boot script in `index.html` repeats these. */
const THEME_COLORS: Record<Theme, string> = { light: "#f9f6ee", dark: "#121821" };

/** Also read by the boot script in `index.html`, which applies it before first paint. */
const STORAGE_KEY = "vigie.display";
const SYSTEM_DARK = "(prefers-color-scheme: dark)";

function apply(display: Display): void {
  const theme = resolveTheme(display.theme, window.matchMedia(SYSTEM_DARK).matches);
  const root = document.documentElement;
  root.style.fontSize = `${display.scale}%`;
  root.dataset.theme = theme;
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", THEME_COLORS[theme]);
}

let current: Display | null = null;
const listeners = new Set<() => void>();

function snapshot(): Display {
  if (current === null) {
    try {
      current = parseDisplay(localStorage.getItem(STORAGE_KEY));
    } catch {
      current = DEFAULT_DISPLAY;
    }
  }
  return current;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useDisplay(): Display {
  return useSyncExternalStore(subscribe, snapshot);
}

export function setDisplay(next: Display): void {
  current = next;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // Storage full or disabled: the choice holds for this page only.
  }
  apply(next);
  for (const listener of listeners) listener();
}

/** Applies the stored display, then follows the system theme while it is the choice. */
export function startDisplay(): void {
  apply(snapshot());
  window.matchMedia(SYSTEM_DARK).addEventListener("change", () => apply(snapshot()));
}
