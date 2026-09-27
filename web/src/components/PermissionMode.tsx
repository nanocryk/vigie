import { useEffect, useLayoutEffect, useRef, useState } from "react";

import type { Client } from "@/client";

const CHOICES: readonly { mode: string; label: string }[] = [
  { mode: "default", label: "Manual" },
  { mode: "acceptEdits", label: "Accept edits" },
  { mode: "plan", label: "Plan" },
];

/** Tab colors per mode; modes outside the picker use the manual look. */
const TAB_STYLE: Record<string, string> = {
  acceptEdits: "bg-mode-accept text-mode-accept-foreground border-mode-accept",
  plan: "bg-mode-plan text-mode-plan-foreground border-mode-plan",
};
const MANUAL_TAB = "bg-mode-manual text-mode-manual-foreground border-mode-manual";
/** Fits the longest label plus its marker; the font is monospaced, so `ch` is exact. */
const TAB_WIDTH = "w-[calc(14ch+1.25rem+2px)]";

/** Label of a CLI permission mode; modes outside the picker show their raw name. */
export function modeLabel(mode: string | null): string {
  if (mode === null) return "mode unknown";
  return CHOICES.find((choice) => choice.mode === mode)?.label ?? mode;
}

/** Shift+Tab order, as in the TUI; a mode outside the picker cycles as manual. */
export function nextMode(mode: string | null): string {
  const index = CHOICES.findIndex((choice) => choice.mode === mode);
  const next = CHOICES[(Math.max(index, 0) + 1) % CHOICES.length];
  if (next === undefined) throw new Error(`nextMode: no choice after ${JSON.stringify(mode)}`);
  return next.mode;
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Current mode as a small label; tapping it opens the choices upward. */
export function PermissionMode({
  client,
  session,
  mode,
  enabled,
}: {
  client: Client;
  session: string;
  mode: string | null;
  /** False until the session is known; a stopped session stores the mode for its next launch. */
  enabled: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onPointer = (event: PointerEvent) => {
      if (event.target instanceof Node && root.current?.contains(event.target) !== true) {
        setOpen(false);
      }
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", onPointer, { passive: true });
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  useEffect(() => {
    if (!enabled) setOpen(false);
  }, [enabled]);

  const pick = (next: string) => {
    setOpen(false);
    if (next === mode) return;
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

  return (
    <div ref={root} className="pointer-events-auto relative flex items-end gap-2">
      <button
        type="button"
        disabled={!enabled || pending}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
        className={`voice-ui flex ${TAB_WIDTH} justify-between rounded-t-[5px] border border-b-0 px-2.5 py-1 text-xs whitespace-nowrap shadow-sm disabled:opacity-60 ${(mode !== null && TAB_STYLE[mode]) || MANUAL_TAB}`}
      >
        <span>{modeLabel(mode)}</span>
        <span>&#x25B4;</span>
      </button>
      {error !== null && <span className="text-destructive truncate text-xs">{error}</span>}
      {open && (
        <ul role="listbox" className="absolute bottom-full left-0 z-20 mb-1 flex flex-col gap-0.5">
          {CHOICES.filter((choice) => choice.mode !== mode).map((choice) => (
            <li key={choice.mode} role="option" aria-selected={false}>
              <button
                type="button"
                onClick={() => pick(choice.mode)}
                className={`voice-ui flex ${TAB_WIDTH} border px-2.5 py-1 text-xs whitespace-nowrap shadow-sm ${TAB_STYLE[choice.mode] ?? MANUAL_TAB}`}
              >
                {choice.label}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
