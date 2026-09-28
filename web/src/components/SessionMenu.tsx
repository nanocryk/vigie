import { type ReactNode, useEffect, useRef, useState } from "react";

import {
  SIDE_BUTTON,
  SPRING_BACK,
  coarsePointer,
  reveal,
  useSwitchOnTap,
} from "@/components/AgentsPanel";
import {
  MODE_CHOICES,
  type PermissionModeControl,
  modeLabel,
  modeOutline,
  modeStyle,
} from "@/components/PermissionMode";
import { useSwipe } from "@/components/Swipe";

export type MenuEntry = "mode" | "model" | "effort";

export type MenuState = { kind: "closed" } | { kind: "open"; entry: MenuEntry | null };

export const closedMenu: MenuState = { kind: "closed" };

/** Toggles opening the menu; a pointer going down on them does not count as outside it. */
export const MENU_TOGGLE = "data-menu-toggle";

export interface SettingOption {
  value: string;
  label: string;
}

/** Model or effort: tapping offers `stopLabel` until the change flow runs, then the list. */
export interface SettingControl {
  label: string;
  stopLabel: string;
  options: readonly SettingOption[];
  selected: string | null;
  onStop: () => void;
  onPick: (value: string) => void;
}

const ENTRY =
  "voice-ui flex w-full items-baseline justify-between gap-3 px-4 py-3 text-left text-base disabled:opacity-50";
const NAME = "text-muted-foreground text-sm";
const CHIP = "voice-ui rounded-[5px] border px-3 py-2 text-sm disabled:opacity-50";

function Entry({ children, divided = true }: { children: ReactNode; divided?: boolean }) {
  return <li className={divided ? "border-t first:border-t-0" : ""}>{children}</li>;
}

function SettingEntry({
  name,
  control,
  expanded,
  enabled,
  picking,
  onToggle,
  onPicked,
}: {
  name: string;
  control: SettingControl;
  expanded: boolean;
  enabled: boolean;
  /** The change flow runs: the entry opens its list. */
  picking: boolean;
  onToggle: () => void;
  onPicked: () => void;
}) {
  return (
    <Entry>
      <button
        type="button"
        disabled={!enabled}
        aria-expanded={expanded}
        onClick={onToggle}
        className={`${ENTRY} hover:bg-muted`}
      >
        <span className={NAME}>{name}</span>
        <span className="truncate">{control.label}</span>
      </button>
      {expanded && !picking && (
        <div className="px-4 pb-3">
          <button
            type="button"
            onClick={control.onStop}
            className={`${CHIP} border-destructive text-destructive bg-card w-full text-left`}
          >
            {control.stopLabel}
          </button>
        </div>
      )}
      {expanded && picking && (
        <div role="listbox" className="flex flex-col gap-1 px-4 pb-3">
          {control.options.length === 0 ? (
            <span className="text-muted-foreground voice-ui text-sm">None for this model.</span>
          ) : (
            control.options.map((option) => (
              <button
                key={option.value}
                type="button"
                role="option"
                aria-selected={option.value === control.selected}
                onClick={() => {
                  control.onPick(option.value);
                  onPicked();
                }}
                className={`${CHIP} text-left ${option.value === control.selected ? "bg-primary text-primary-foreground border-primary" : "bg-card hover:bg-muted"}`}
              >
                {option.label}
              </button>
            ))
          )}
        </div>
      )}
    </Entry>
  );
}

/**
 * Tap opens the agents panel; two swipes left stop every running agent. On
 * desktop, a Stop all button pressed twice. Both work with no agent running,
 * then only say so.
 */
function AgentsEntry({
  summary,
  busy,
  running,
  error,
  onOpen,
  onStopAll,
}: {
  summary: string;
  busy: boolean;
  running: boolean;
  error: string | null;
  onOpen: () => void;
  onStopAll: () => void;
}) {
  // A new attempt starts the gesture over.
  const [attempt, setAttempt] = useState(0);
  const [idle, setIdle] = useState(false);
  const confirm = () => {
    setAttempt((count) => count + 1);
    setIdle(!running);
    if (running) onStopAll();
  };
  const notice = idle ? "No agent running" : error;
  return (
    <>
      <AgentsRow
        key={`${attempt}-${busy}`}
        summary={busy ? "stopping all..." : summary}
        stoppable={!busy}
        onOpen={onOpen}
        onConfirm={confirm}
      />
      {notice !== null && (
        <p className={`voice-ui px-4 pb-2 text-xs ${idle ? "text-muted-foreground" : "text-destructive"}`}>
          {notice}
        </p>
      )}
    </>
  );
}

function AgentsRow({
  summary,
  stoppable,
  onOpen,
  onConfirm,
}: {
  summary: string;
  stoppable: boolean;
  onOpen: () => void;
  onConfirm: () => void;
}) {
  const { surface, view, press } = useSwipe(
    { dangerous: false, sides: ["left"], swipes: 2, presses: 2 },
    onConfirm,
  );
  const coarse = coarsePointer();
  const swipeable = coarse && stoppable;
  const armed = view.armed !== null;
  const tap = useSwitchOnTap(armed, onOpen);
  return (
    <div className="flex items-stretch">
      <div className="relative min-w-0 flex-1 overflow-hidden">
        {swipeable && (
          <div
            aria-hidden
            className="voice-ui absolute inset-0 flex items-center justify-end px-4 text-sm text-black"
            style={{ background: reveal(view.lean, armed) }}
          >
            Stop all
          </div>
        )}
        <div
          ref={swipeable ? surface : undefined}
          role="button"
          tabIndex={0}
          {...tap}
          onKeyDown={(event) => {
            if (event.key !== "Enter" && event.key !== " ") return;
            event.preventDefault();
            onOpen();
          }}
          className={`${ENTRY} bg-background hover:bg-muted relative touch-pan-y select-none ${armed ? "text-destructive" : ""}`}
          style={{
            transform: `translateX(${view.offset}px)`,
            transition: view.dragging ? "none" : SPRING_BACK,
          }}
        >
          <span>{armed ? "Stop all?" : "Agents"}</span>
          <span className="text-muted-foreground truncate text-sm">{summary}</span>
        </div>
      </div>
      {!coarse && stoppable && (
        <div className="flex py-2 pr-3">
          <button
            type="button"
            disabled={view.fired !== null}
            onClick={() => press("left")}
            className={`${SIDE_BUTTON} border-destructive ${armed ? "bg-destructive text-black" : "text-destructive hover:bg-muted"}`}
          >
            {armed ? "Stop all?" : "Stop all"}
          </button>
        </div>
      )}
    </div>
  );
}

/**
 * The session menu, opening upward over the discussion, anchored right. A tap
 * outside it (toggles excepted) closes it; Escape is the owner's to handle.
 */
export function SessionMenu({
  entry,
  enabled,
  picking,
  mode,
  modeControl,
  model,
  effort,
  commandsEnabled,
  revoke,
  restartEnabled,
  agents,
  onEntry,
  onCommands,
  onRestart,
  onClose,
}: {
  entry: MenuEntry | null;
  /** False until the session is known, and on an archived one. */
  enabled: boolean;
  picking: boolean;
  mode: string | null;
  modeControl: PermissionModeControl;
  model: SettingControl;
  effort: SettingControl;
  commandsEnabled: boolean;
  revoke: { enabled: boolean; busy: boolean; error: string | null; onRevoke: () => void };
  restartEnabled: boolean;
  /** `summary` sits beside the entry; Stop all acts on every running agent at once. */
  agents: {
    summary: string;
    onOpen: () => void;
    stopAll: { enabled: boolean; busy: boolean; error: string | null; onStop: () => void };
  };
  onEntry: (entry: MenuEntry | null) => void;
  onCommands: () => void;
  /** Opens the restart choices: resume, continue or blank. */
  onRestart: () => void;
  onClose: () => void;
}) {
  const root = useRef<HTMLDivElement>(null);
  const closeRef = useRef(onClose);
  useEffect(() => {
    closeRef.current = onClose;
  });

  useEffect(() => {
    const onPointer = (event: PointerEvent) => {
      if (!(event.target instanceof Element)) return;
      if (root.current?.contains(event.target) === true) return;
      if (event.target.closest(`[${MENU_TOGGLE}]`) !== null) return;
      closeRef.current();
    };
    document.addEventListener("pointerdown", onPointer, { passive: true });
    return () => document.removeEventListener("pointerdown", onPointer);
  }, []);

  const toggle = (next: MenuEntry) => onEntry(entry === next ? null : next);

  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-2 z-30 px-3">
      <div className="mx-auto flex max-w-5xl justify-end">
        <div
          ref={root}
          role="menu"
          className="bg-background pointer-events-auto max-h-[70dvh] w-full max-w-sm overflow-y-auto overscroll-contain rounded-[5px] border shadow-lg"
        >
          <ul>
            <Entry>
              <div className={ENTRY}>
                <span className={NAME}>Mode</span>
                <div className="flex shrink-0 gap-1.5">
                  {mode !== null && !MODE_CHOICES.some((choice) => choice.mode === mode) && (
                    <span className={`${CHIP} ${modeStyle(mode)}`}>{modeLabel(mode)}</span>
                  )}
                  {MODE_CHOICES.map((choice) =>
                    choice.mode === mode ? (
                      <span key={choice.mode} className={`${CHIP} ${modeStyle(choice.mode)}`}>
                        {choice.label}
                      </span>
                    ) : (
                      <button
                        key={choice.mode}
                        type="button"
                        disabled={!enabled || modeControl.pending}
                        onClick={() => modeControl.pick(choice.mode)}
                        className={`${CHIP} ${modeOutline(choice.mode)}`}
                      >
                        {choice.label}
                      </button>
                    ),
                  )}
                </div>
              </div>
              {modeControl.error !== null && (
                <p className="text-destructive voice-ui px-4 pb-2 text-xs">{modeControl.error}</p>
              )}
            </Entry>
            <SettingEntry
              name="Model"
              control={model}
              expanded={entry === "model"}
              enabled={enabled && !(picking && model.options.length === 0)}
              picking={picking}
              onToggle={() => toggle("model")}
              onPicked={() => onEntry(null)}
            />
            <SettingEntry
              name="Effort"
              control={effort}
              expanded={entry === "effort"}
              enabled={enabled}
              picking={picking}
              onToggle={() => toggle("effort")}
              onPicked={() => onEntry(null)}
            />
            <Entry>
              <button
                type="button"
                disabled={!commandsEnabled}
                onClick={onCommands}
                className={`${ENTRY} hover:bg-muted`}
              >
                <span>Commands</span>
                <span className="text-muted-foreground voice-code text-sm">/</span>
              </button>
            </Entry>
            <Entry>
              <AgentsEntry
                summary={agents.summary}
                busy={agents.stopAll.busy}
                running={agents.stopAll.enabled}
                error={agents.stopAll.error}
                onOpen={agents.onOpen}
                onStopAll={agents.stopAll.onStop}
              />
            </Entry>
            <Entry>
              <button
                type="button"
                disabled={!restartEnabled}
                onClick={onRestart}
                className={`${ENTRY} hover:bg-muted flex-col items-start gap-0.5`}
              >
                <span>Restart...</span>
                <span className="text-muted-foreground text-xs">
                  Resume as is, or start fresh with the last exchanges or with no context.
                </span>
              </button>
            </Entry>
            <Entry>
              <button
                type="button"
                disabled={!revoke.enabled || revoke.busy}
                onClick={revoke.onRevoke}
                className={`${ENTRY} hover:bg-muted flex-col items-start gap-0.5`}
              >
                <span>{revoke.busy ? "Revoking..." : "Revoke all grants"}</span>
                <span className="text-muted-foreground text-xs">
                  Restarts Claude on this session without the folders granted from asks.
                </span>
              </button>
              {revoke.error !== null && (
                <p className="text-destructive voice-ui px-4 pb-2 text-xs">{revoke.error}</p>
              )}
            </Entry>
          </ul>
        </div>
      </div>
    </div>
  );
}

const PILL = "voice-ui shrink-0 rounded-[5px] border px-2 leading-5 whitespace-nowrap disabled:opacity-60";

/** The thin bar's mode and model pills, each opening the menu on its entry, and the agents glance. */
export function MenuPills({
  mode,
  modelLabel,
  enabled,
  onOpen,
  agents,
  onAgents,
}: {
  mode: string | null;
  modelLabel: string;
  enabled: boolean;
  onOpen: (entry: MenuEntry) => void;
  /** `2 agents working`; null hides the pill. */
  agents: string | null;
  onAgents: () => void;
}) {
  return (
    <>
      <button
        type="button"
        {...{ [MENU_TOGGLE]: "" }}
        disabled={!enabled}
        onClick={() => onOpen("mode")}
        className={`${PILL} ${modeStyle(mode)}`}
      >
        {modeLabel(mode)}
      </button>
      <button
        type="button"
        {...{ [MENU_TOGGLE]: "" }}
        disabled={!enabled}
        onClick={() => onOpen("model")}
        className={`${PILL} bg-card max-w-[40vw] min-w-0 truncate`}
      >
        {modelLabel}
      </button>
      {agents !== null && (
        <button
          type="button"
          onClick={onAgents}
          className={`${PILL} bg-card border-primary text-primary`}
        >
          {agents}
        </button>
      )}
    </>
  );
}
