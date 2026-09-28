import { useEffect, useLayoutEffect, useRef, useState } from "react";

import type { Client } from "@/client";

export const MODE_CHOICES: readonly { mode: string; label: string }[] = [
  { mode: "default", label: "Manual" },
  { mode: "acceptEdits", label: "Accept edits" },
  { mode: "plan", label: "Plan" },
];

/** Colors per mode; modes outside the picker use the manual look. */
const MODE_STYLE: Record<string, string> = {
  acceptEdits: "bg-mode-accept text-mode-accept-foreground border-mode-accept",
  plan: "bg-mode-plan text-mode-plan-foreground border-mode-plan",
};
const MANUAL_STYLE = "bg-mode-manual text-mode-manual-foreground border-mode-manual";

/** Unselected look: the mode's color on its border and text, over black. */
const MODE_OUTLINE: Record<string, string> = {
  acceptEdits: "bg-black text-mode-accept border-mode-accept",
  plan: "bg-black text-mode-plan border-mode-plan",
};
const MANUAL_OUTLINE = "bg-black text-mode-manual border-mode-manual";

/** Background, text and border classes of `mode`. */
export function modeStyle(mode: string | null): string {
  return (mode !== null && MODE_STYLE[mode]) || MANUAL_STYLE;
}

/** Classes of `mode` shown as a choice not currently selected. */
export function modeOutline(mode: string): string {
  return MODE_OUTLINE[mode] ?? MANUAL_OUTLINE;
}

/** Label of a CLI permission mode; modes outside the picker show their raw name. */
export function modeLabel(mode: string | null): string {
  if (mode === null) return "mode unknown";
  return MODE_CHOICES.find((choice) => choice.mode === mode)?.label ?? mode;
}

/** Shift+Tab order, as in the TUI; a mode outside the picker cycles as manual. */
export function nextMode(mode: string | null): string {
  const index = MODE_CHOICES.findIndex((choice) => choice.mode === mode);
  const next = MODE_CHOICES[(Math.max(index, 0) + 1) % MODE_CHOICES.length];
  if (next === undefined) throw new Error(`nextMode: no choice after ${JSON.stringify(mode)}`);
  return next.mode;
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export interface PermissionModeControl {
  pick: (mode: string) => void;
  /** A change was sent and awaits the server. */
  pending: boolean;
  error: string | null;
}

/**
 * Sets the session's permission mode, and cycles it on Shift+Tab. `enabled` is
 * false until the session is known; a stopped session stores the mode for its
 * next launch.
 */
export function usePermissionMode(
  client: Client,
  session: string,
  mode: string | null,
  enabled: boolean,
): PermissionModeControl {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const pick = (next: string) => {
    if (!enabled || pending || next === mode) return;
    setPending(true);
    setError(null);
    client
      .command({ type: "set_permission_mode", session, mode: next, client_id: client.newClientId() })
      .catch((err: unknown) => setError(errorMessage(err)))
      .finally(() => setPending(false));
  };
  const pickRef = useRef(pick);
  useLayoutEffect(() => {
    pickRef.current = pick;
  });

  useEffect(() => {
    if (!enabled || pending) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Tab" || !event.shiftKey || event.ctrlKey || event.altKey || event.metaKey) {
        return;
      }
      event.preventDefault();
      pickRef.current(nextMode(mode));
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [enabled, pending, mode]);

  return { pick, pending, error };
}
