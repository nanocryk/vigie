import { type FormEvent, type ReactNode, useEffect, useRef, useState } from "react";

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

export type MenuEntry = "name" | "mode" | "model" | "effort";

/** The session's display name; saving `null` clears it back to `folder`. */
export interface NameControl {
  current: string | null;
  folder: string;
  onSave: (name: string | null) => Promise<void>;
}

const MAX_NAME_CHARS = 80;

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
            className={`${CHIP} w-full border-destructive bg-card text-left text-destructive`}
          >
            {control.stopLabel}
          </button>
        </div>
      )}
      {expanded && picking && (
        <div role="listbox" className="flex flex-col gap-1 px-4 pb-3">
          {control.options.length === 0 ? (
            <span className="text-sm voice-ui text-muted-foreground">None for this model.</span>
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
                className={`${CHIP} text-left ${option.value === control.selected ? "border-primary bg-primary text-primary-foreground" : "bg-card hover:bg-muted"}`}
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

/** Tap edits the name in place; saving it blank brings the folder name back. */
function NameEntry({
  control,
  expanded,
  onToggle,
  onDone,
}: {
  control: NameControl;
  expanded: boolean;
  onToggle: () => void;
  onDone: () => void;
}) {
  return (
    <Entry>
      <button
        type="button"
        aria-expanded={expanded}
        onClick={onToggle}
        className={`${ENTRY} hover:bg-muted`}
      >
        <span className={NAME}>Name</span>
        <span className={`truncate ${control.current === null ? "text-muted-foreground" : ""}`}>
          {control.current ?? control.folder}
        </span>
      </button>
      {expanded && <NameForm control={control} onDone={onDone} />}
    </Entry>
  );
}

function NameForm({ control, onDone }: { control: NameControl; onDone: () => void }) {
  const [text, setText] = useState(control.current ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    const trimmed = text.trim();
    if (trimmed === (control.current ?? "")) {
      onDone();
      return;
    }
    setBusy(true);
    setError(null);
    control
      .onSave(trimmed === "" ? null : trimmed)
      .then(onDone)
      .catch((err: unknown) => {
        setError(err instanceof Error ? err.message : String(err));
        setBusy(false);
      });
  };
  return (
    <form onSubmit={submit} className="space-y-2 px-4 pb-3">
      <div className="flex gap-2">
        <input
          autoFocus
          value={text}
          maxLength={MAX_NAME_CHARS}
          placeholder={control.folder}
          onChange={(event) => setText(event.target.value)}
          onKeyDown={(event) => {
            if (event.key !== "Escape") return;
            event.preventDefault();
            onDone();
          }}
          aria-label="Session name"
          className="min-w-0 flex-1 rounded-[5px] border bg-background px-2 py-1 text-base voice-ui"
        />
        <button
          type="submit"
          disabled={busy}
          className={`${CHIP} border-primary bg-primary text-primary-foreground`}
        >
          Save
        </button>
        <button type="button" onClick={onDone} className={`${CHIP} bg-card hover:bg-muted`}>
          Cancel
        </button>
      </div>
      {error !== null && <p className="text-xs voice-ui text-destructive">{error}</p>}
    </form>
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
        <p
          className={`px-4 pb-2 text-xs voice-ui ${idle ? "text-muted-foreground" : "text-destructive"}`}
        >
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
            className="absolute inset-0 flex items-center justify-end px-4 text-sm voice-ui text-black"
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
          className={`${ENTRY} relative touch-pan-y bg-background select-none hover:bg-muted ${armed ? "text-destructive" : ""}`}
          style={{
            transform: `translateX(${view.offset}px)`,
            transition: view.dragging ? "none" : SPRING_BACK,
          }}
        >
          <span>{armed ? "Stop all?" : "Agents"}</span>
          <span className="truncate text-sm text-muted-foreground">{summary}</span>
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
  name,
  mode,
  modeControl,
  model,
  effort,
  commandsEnabled,
  revoke,
  restartEnabled,
  agents,
  draft,
  onEntry,
  onCommands,
  onRestart,
  onClose,
}: {
  entry: MenuEntry | null;
  /** False until the session is known, and on an archived one. */
  enabled: boolean;
  picking: boolean;
  /** Null until the session is known; works on an archived one. */
  name: NameControl | null;
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
  /**
   * With a message in the composer: Hold queues it until sent by hand; Send now,
   * null outside a turn, writes it to Claude during the turn.
   */
  draft: { onHold: () => void; onSendNow: (() => void) | null } | null;
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
          className="pointer-events-auto max-h-[70dvh] w-full max-w-sm overflow-y-auto overscroll-contain rounded-[5px] border bg-background shadow-lg"
        >
          <ul>
            {name !== null && (
              <NameEntry
                control={name}
                expanded={entry === "name"}
                onToggle={() => toggle("name")}
                onDone={() => onEntry(null)}
              />
            )}
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
                <p className="px-4 pb-2 text-xs voice-ui text-destructive">{modeControl.error}</p>
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
                <span className="text-sm voice-code text-muted-foreground">/</span>
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
                className={`${ENTRY} flex-col items-start gap-0.5 hover:bg-muted`}
              >
                <span>Restart...</span>
                <span className="text-xs text-muted-foreground">
                  Resume as is, or start fresh with the last exchanges or with no context.
                </span>
              </button>
            </Entry>
            <Entry>
              <button
                type="button"
                disabled={!revoke.enabled || revoke.busy}
                onClick={revoke.onRevoke}
                className={`${ENTRY} flex-col items-start gap-0.5 hover:bg-muted`}
              >
                <span>{revoke.busy ? "Revoking..." : "Revoke all grants"}</span>
                <span className="text-xs text-muted-foreground">
                  Restarts Claude on this session without the folders granted from asks.
                </span>
              </button>
              {revoke.error !== null && (
                <p className="px-4 pb-2 text-xs voice-ui text-destructive">{revoke.error}</p>
              )}
            </Entry>
            {draft !== null && (
              <Entry>
                <div className={ENTRY}>
                  <span className={NAME}>Message</span>
                  <div className="flex shrink-0 gap-1.5">
                    <button
                      type="button"
                      onClick={draft.onHold}
                      className={`${CHIP} border-dashed bg-card hover:bg-muted`}
                    >
                      Hold
                    </button>
                    {draft.onSendNow !== null && (
                      <button
                        type="button"
                        onClick={draft.onSendNow}
                        className={`${CHIP} border-primary bg-primary text-primary-foreground`}
                      >
                        Send now
                      </button>
                    )}
                  </div>
                </div>
              </Entry>
            )}
          </ul>
        </div>
      </div>
    </div>
  );
}

const PILL =
  "voice-ui shrink-0 rounded-[5px] border px-2 leading-5 whitespace-nowrap disabled:opacity-60";

/** The thin bar's mode and model pills, each opening the menu on its entry, and the agents glance. */
export function MenuPills({
  mode,
  modelLabel,
  enabled,
  onOpen,
  agents,
  onAgents,
  background,
  onBackground,
}: {
  mode: string | null;
  modelLabel: string;
  enabled: boolean;
  onOpen: (entry: MenuEntry) => void;
  /** `2 agents working`; null hides the pill. */
  agents: string | null;
  onAgents: () => void;
  /** `1 background`; null hides the pill. */
  background: string | null;
  onBackground: () => void;
}) {
  return (
    <>
      <button
        type="button"
        {...{ [MENU_TOGGLE]: "" }}
        disabled={!enabled}
        onClick={() => onOpen("mode")}
        className={`${PILL} ${modeStyle(mode)} enabled:hover:brightness-125`}
      >
        {modeLabel(mode)}
      </button>
      <button
        type="button"
        {...{ [MENU_TOGGLE]: "" }}
        disabled={!enabled}
        onClick={() => onOpen("model")}
        className={`${PILL} max-w-[40vw] min-w-0 truncate bg-card enabled:hover:bg-muted`}
      >
        {modelLabel}
      </button>
      {agents !== null && (
        <button
          type="button"
          onClick={onAgents}
          className={`${PILL} border-primary bg-card text-primary`}
        >
          {agents}
        </button>
      )}
      {background !== null && (
        <button
          type="button"
          onClick={onBackground}
          className={`${PILL} bg-card text-muted-foreground`}
        >
          {background}
        </button>
      )}
    </>
  );
}
