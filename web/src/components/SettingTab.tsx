import { useEffect, useRef, useState } from "react";

const TAB =
  "voice-ui flex min-w-0 items-center justify-between gap-1.5 rounded-t-[5px] border border-b-0 px-2.5 py-1 text-xs whitespace-nowrap shadow-sm disabled:opacity-60";
const IDLE_TAB = "bg-card text-foreground border-border";
const OPEN_TAB = "bg-primary text-primary-foreground border-primary";
const CHIP = "voice-ui flex w-full min-w-full border px-2.5 py-1 text-left text-xs whitespace-nowrap shadow-sm";

export interface SettingOption {
  value: string;
  label: string;
}

/**
 * A session setting (model or effort) as a tab over the discussion. Tapping it
 * offers one entry, `stopLabel`, which starts the change flow and opens this
 * tab's list; during the flow (`picking`), tapping a tab opens its list of
 * `options`. A pick, a tap elsewhere or Escape closes it.
 */
export function SettingTab({
  label,
  stopLabel,
  enabled,
  picking,
  options,
  selected,
  onStop,
  onPick,
}: {
  label: string;
  stopLabel: string;
  enabled: boolean;
  picking: boolean;
  options: readonly SettingOption[];
  selected: string | null;
  onStop: () => void;
  onPick: (value: string) => void;
}) {
  const [open, setOpen] = useState(false);
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
    if (!enabled || !picking) setOpen(false);
  }, [enabled, picking]);

  const listed = open && picking && options.length > 0;

  return (
    <div ref={root} className="pointer-events-auto relative flex min-w-0 items-end">
      <button
        type="button"
        disabled={!enabled || (picking && options.length === 0)}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
        className={`${TAB} ${picking ? OPEN_TAB : IDLE_TAB}`}
      >
        <span className="truncate">{label}</span>
        <span>&#x25B4;</span>
      </button>
      {open && !picking && (
        <ul role="listbox" className="absolute bottom-full left-0 z-20 mb-1 flex flex-col gap-0.5">
          <li role="option" aria-selected={false}>
            <button
              type="button"
              onClick={() => {
                onStop();
                setOpen(true);
              }}
              className={`${CHIP} border-destructive bg-card text-destructive`}
            >
              {stopLabel}
            </button>
          </li>
        </ul>
      )}
      {listed && (
        <ul
          role="listbox"
          className="absolute bottom-full left-0 z-20 mb-1 flex max-h-[50dvh] flex-col gap-0.5 overflow-y-auto"
        >
          {options.map((option) => (
            <li key={option.value} role="option" aria-selected={option.value === selected}>
              <button
                type="button"
                onClick={() => {
                  onPick(option.value);
                  setOpen(false);
                }}
                className={`${CHIP} ${option.value === selected ? OPEN_TAB : IDLE_TAB}`}
              >
                {option.label}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
