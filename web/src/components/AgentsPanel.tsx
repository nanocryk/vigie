import { type MouseEvent, type PointerEvent, useRef, useState } from "react";

import { type AgentEntry, agentStateLabel, agentTree } from "@/agents";
import { useSwipe } from "@/components/Swipe";
import { formatTokens } from "@/gauges";
import { type Side, TAP_SLOP_PX } from "@/swipe";

export const SPRING_BACK = "transform 200ms ease-out";
const ROW = "bg-card relative w-full touch-pan-y rounded-[5px] border px-3 py-2 text-left select-none";
export const SIDE_BUTTON = "voice-ui shrink-0 rounded-[5px] border px-3 text-xs disabled:opacity-50";
/** Indent per level of the tree. */
const INDENT_REM = 1;

export function coarsePointer(): boolean {
  return window.matchMedia("(pointer: coarse)").matches;
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Revealed behind a swiped row: green toward opening it, red toward stopping it. */
export function reveal(lean: number, armedLeft: boolean): string {
  if (armedLeft || lean <= -1) return "var(--swipe-deny-latched)";
  if (lean >= 1) return "var(--swipe-accept-latched)";
  const fill = lean < 0 ? "var(--swipe-deny)" : "var(--swipe-accept)";
  return `color-mix(in oklch, ${fill} ${Math.round(Math.abs(lean) * 100)}%, var(--muted))`;
}

/** Switches on a click, but not on the tap ending a drag or cancelling an arming. */
export function useSwitchOnTap(armed: boolean, onSwitch: () => void) {
  const down = useRef<{ x: number; y: number; armed: boolean } | null>(null);
  return {
    onPointerDown: (event: PointerEvent) => {
      down.current = { x: event.clientX, y: event.clientY, armed };
    },
    onClick: (event: MouseEvent) => {
      const start = down.current;
      down.current = null;
      if (start !== null) {
        const travel = Math.hypot(event.clientX - start.x, event.clientY - start.y);
        if (start.armed || travel >= TAP_SLOP_PX) return;
      }
      onSwitch();
    },
  };
}

function RowBody({
  name,
  status,
  activity,
  current,
}: {
  name: string;
  status: string;
  activity: string | null;
  current: boolean;
}) {
  return (
    <>
      <div className="flex items-baseline gap-2">
        <span className="voice-heading min-w-0 flex-1 truncate text-sm">{name}</span>
        {current && <span className="text-primary voice-ui shrink-0 text-xs">on screen</span>}
        <span className="text-muted-foreground voice-ui shrink-0 text-xs">{status}</span>
      </div>
      {activity !== null && (
        <div className="text-muted-foreground voice-ui truncate text-xs">{activity}</div>
      )}
    </>
  );
}

/**
 * Tap or one swipe right switches to the agent; two swipes left stop a running
 * one. On desktop, an Open button and a Stop button pressed twice.
 */
function AgentRow(props: {
  entry: AgentEntry;
  depth: number;
  current: boolean;
  onSwitch: () => void;
  onStop: () => Promise<void>;
}) {
  // A failed stop starts the row's gesture over.
  const [attempt, setAttempt] = useState(0);
  const [error, setError] = useState<string | null>(null);
  return (
    <li className="space-y-1" style={{ paddingLeft: `${props.depth * INDENT_REM}rem` }}>
      <SwipeRow
        key={attempt}
        {...props}
        onStop={() => {
          setError(null);
          return props.onStop().catch((err: unknown) => {
            setError(errorMessage(err));
            setAttempt((count) => count + 1);
          });
        }}
      />
      {error !== null && <p className="text-destructive voice-ui text-xs">{error}</p>}
    </li>
  );
}

function SwipeRow({
  entry,
  current,
  onSwitch,
  onStop,
}: {
  entry: AgentEntry;
  current: boolean;
  onSwitch: () => void;
  onStop: () => Promise<void>;
}) {
  const stoppable = entry.live && entry.state === "running";
  const [stopping, setStopping] = useState(false);
  const stop = () => {
    setStopping(true);
    void onStop().finally(() => setStopping(false));
  };
  const { surface, view, press } = useSwipe(
    {
      dangerous: false,
      sides: stoppable ? ["left", "right"] : ["right"],
      swipes: { right: 1, left: 2 },
      presses: 2,
    },
    (side: Side) => (side === "right" ? onSwitch() : stop()),
  );
  const coarse = coarsePointer();
  const armedLeft = view.armed?.side === "left";
  const tap = useSwitchOnTap(view.armed !== null, onSwitch);
  const state = stopping ? "stopping..." : agentStateLabel(entry);
  const status =
    entry.promptTokens > 0 ? `${state} · ${formatTokens(entry.promptTokens)}` : state;
  return (
    <div className="flex items-stretch gap-2">
      <div className="relative min-w-0 flex-1 overflow-hidden rounded-[5px]">
        {coarse && (
          <div
            aria-hidden
            className="voice-ui absolute inset-0 flex items-center justify-between rounded-[5px] px-4 text-sm text-black"
            style={{ background: reveal(view.lean, armedLeft) }}
          >
            <span>Open</span>
            {stoppable && <span>Stop</span>}
          </div>
        )}
        <div
          ref={coarse ? surface : undefined}
          role="button"
          tabIndex={0}
          {...tap}
          onKeyDown={(event) => {
            if (event.key !== "Enter" && event.key !== " ") return;
            event.preventDefault();
            onSwitch();
          }}
          className={`${ROW} hover:bg-muted ${current ? "border-primary" : ""} ${armedLeft ? "border-destructive" : ""}`}
          style={{
            transform: `translateX(${view.offset}px)`,
            transition: view.dragging ? "none" : SPRING_BACK,
          }}
        >
          <RowBody name={entry.name} status={status} activity={entry.activity} current={current} />
        </div>
        {coarse && armedLeft && (
          <span className="bg-card voice-heading text-destructive pointer-events-none absolute top-1.5 left-2 rounded-[5px] px-2 py-0.5 text-sm shadow-sm">
            Stop?
          </span>
        )}
      </div>
      {!coarse && (
        <>
          <button
            type="button"
            onClick={onSwitch}
            className={`${SIDE_BUTTON} border-swipe-accept-strong text-swipe-accept-strong hover:bg-muted`}
          >
            Open
          </button>
          {stoppable && (
            <button
              type="button"
              disabled={view.fired !== null || stopping}
              onClick={() => press("left")}
              className={`${SIDE_BUTTON} border-destructive ${armedLeft ? "bg-destructive text-black" : "text-destructive hover:bg-muted"}`}
            >
              {armedLeft ? "Stop?" : "Stop"}
            </button>
          )}
        </>
      )}
    </div>
  );
}

/** In front of the discussion: the main discussion, then every agent under the one that started it. */
export function AgentsPanel({
  entries,
  viewing,
  onSwitch,
  onStop,
  onClose,
}: {
  entries: readonly AgentEntry[];
  /** The agent on screen; null for the main discussion. */
  viewing: string | null;
  onSwitch: (toolUseId: string | null) => void;
  onStop: (taskId: string) => Promise<void>;
  onClose: () => void;
}) {
  const coarse = coarsePointer();
  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-2 z-30 px-3">
      <div className="bg-background pointer-events-auto mx-auto flex max-h-[60dvh] max-w-5xl flex-col rounded-[5px] border shadow-lg">
        <div className="flex items-center justify-between gap-2 border-b px-3 py-1.5">
          <span className="voice-heading text-sm">Agents</span>
          <span className="text-muted-foreground voice-ui truncate text-xs">
            {coarse ? "‹‹ stop · open ›" : ""}
          </span>
          <button
            type="button"
            onClick={onClose}
            className="voice-ui hover:bg-muted shrink-0 rounded-[5px] px-2 py-0.5 text-xs"
          >
            Close
          </button>
        </div>
        <div className="min-h-0 overflow-y-auto overscroll-contain p-2">
          <ul className="space-y-1.5">
            <li>
              <button
                type="button"
                onClick={() => onSwitch(null)}
                className={`${ROW} hover:bg-muted ${viewing === null ? "border-primary" : ""}`}
              >
                <RowBody
                  name="Main discussion"
                  status=""
                  activity={null}
                  current={viewing === null}
                />
              </button>
            </li>
            {agentTree(entries).map(({ entry, depth }) => (
              <AgentRow
                key={`${entry.toolUseId}-${entry.state ?? "unknown"}`}
                entry={entry}
                depth={depth}
                current={entry.toolUseId === viewing}
                onSwitch={() => onSwitch(entry.toolUseId)}
                onStop={() =>
                  entry.taskId === null
                    ? Promise.reject(new Error("The agent's task is not known yet"))
                    : onStop(entry.taskId)
                }
              />
            ))}
          </ul>
          {entries.length === 0 && (
            <p className="text-muted-foreground voice-ui px-1 py-2 text-sm">No agent yet.</p>
          )}
        </div>
      </div>
    </div>
  );
}
