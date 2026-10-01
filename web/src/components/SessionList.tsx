import {
  type KeyboardEvent,
  type MouseEvent,
  type PointerEvent,
  useEffect,
  useRef,
  useState,
} from "react";

import type { Client, ConnectionStatus } from "@/client";
import { useSwipe } from "@/components/Swipe";
import { changeExplorer, selectLastFree } from "@/files";
import { Header, NeedsYouColumn, PULSE, pulseStyle } from "@/components/TopBar";
import { useSnapshot } from "@/components/useSnapshot";
import { formatAgo, sessionTitle } from "@/format";
import {
  type RowAction,
  actionSide,
  homeSections,
  keepOrder,
  rowAction,
  rowHighlight,
  rowSwipeRules,
} from "@/home";
import { navigate, routeHash } from "@/route";
import { TAP_SLOP_PX } from "@/swipe";
import type { SessionView } from "@/wire";

const CLOCK_TICK_MS = 30_000;

/** Nothing while connected; a dot named after the status otherwise. */
export function ConnectionDot({ status }: { status: ConnectionStatus }) {
  if (status === "open") return null;
  const text = status === "connecting" ? "connecting..." : "offline, retrying";
  return (
    <span
      role="status"
      aria-label={text}
      title={text}
      className={
        status === "connecting"
          ? "bg-gauge-glow inline-block size-2 shrink-0 animate-pulse rounded-full"
          : "bg-destructive inline-block size-2 shrink-0 rounded-full"
      }
    />
  );
}

export function BusyDot({ busy }: { busy: boolean }) {
  return (
    <span
      aria-label={busy ? "busy" : "idle"}
      className={
        busy
          ? "bg-primary inline-block size-2 shrink-0 animate-pulse rounded-full"
          : "bg-muted-foreground/30 inline-block size-2 shrink-0 rounded-full"
      }
    />
  );
}

function OpenExplorer() {
  return (
    <button
      type="button"
      onClick={() => {
        changeExplorer(selectLastFree);
        navigate({ kind: "files" });
      }}
      className="bg-card hover:bg-muted voice-ui flex w-full items-center gap-3 rounded-[5px] border px-4 py-3 text-left"
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth={1.6}
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden
        className="text-primary size-6 shrink-0"
      >
        <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
      </svg>
      <span className="min-w-0 flex-1">
        <span className="block">File explorer</span>
        <span className="text-muted-foreground block text-xs">
          Browse files and start a session in a folder
        </span>
      </span>
      <span className="text-muted-foreground">&rsaquo;</span>
    </button>
  );
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function coarsePointer(): boolean {
  return window.matchMedia("(pointer: coarse)").matches;
}

const ROW = "bg-card block min-w-0 flex-1 rounded-[5px] border px-3 py-2 text-left";
/** Narrow, on the side the row slides toward, its label written vertically. */
const SIDE_BUTTON =
  "voice-ui shrink-0 rounded-[5px] border px-1 py-2 text-xs [writing-mode:vertical-rl] disabled:opacity-50";
const SPRING_BACK = "transform 200ms ease-out";
const ACTION_LABEL: Record<RowAction, string> = {
  kill: "Kill",
  archive: "Archive",
  restore: "Restore",
};
const ACTION_BUTTON: Record<RowAction, string> = {
  kill: "border-destructive text-destructive",
  archive: "border-gauge-cold text-gauge-cold hover:bg-muted",
  restore: "border-swipe-accept-strong text-swipe-accept-strong hover:bg-muted",
};

const REVEAL: Record<RowAction, { fill: string; latched: string }> = {
  kill: { fill: "var(--swipe-deny)", latched: "var(--swipe-deny-latched)" },
  archive: {
    fill: "var(--gauge-cold)",
    latched: "color-mix(in oklch, var(--gauge-cold), white 30%)",
  },
  restore: { fill: "var(--swipe-accept)", latched: "var(--swipe-accept-latched)" },
};

/** Revealed behind a swiped row: red to kill, blue to archive, green to restore, snapping brighter at the threshold. */
function revealBackground(action: RowAction, lean: number): string {
  const { fill, latched } = REVEAL[action];
  if (Math.abs(lean) >= 1) return latched;
  const share = Math.round(Math.abs(lean) * 100);
  return `color-mix(in oklch, ${fill} ${share}%, var(--muted))`;
}

/** Name and time, then the path; `busyLabel` takes the time's place while an action runs. */
function RowBody({
  session,
  now,
  busyLabel,
}: {
  session: SessionView;
  now: number;
  busyLabel: string | null;
}) {
  // A stopped discussion fades; its row's border turns dashed.
  return (
    <div className={session.live ? "" : "opacity-45"}>
      <div className="flex items-center gap-2">
        <span className="voice-heading min-w-0 flex-1 truncate">{sessionTitle(session)}</span>
        <span className="text-muted-foreground voice-ui shrink-0 text-xs">
          {busyLabel ?? formatAgo(session.updated_at, now)}
        </span>
      </div>
      <div className="text-muted-foreground voice-code truncate text-xs">{session.cwd}</div>
    </div>
  );
}

const MIDDLE_BUTTON = 1;

/**
 * Opens the session on a click or Enter, but not on the tap that cancels an arming or ends a drag.
 * A middle click opens it in a new tab.
 */
function useOpenOnTap(id: string, armed: boolean, blocked: boolean) {
  const down = useRef<{ x: number; y: number; armed: boolean } | null>(null);
  const open = () => navigate({ kind: "session", id });
  return {
    role: "button",
    tabIndex: 0,
    onPointerDown: (event: PointerEvent) => {
      // Keeps the middle button from starting autoscroll.
      if (event.button === MIDDLE_BUTTON) event.preventDefault();
      down.current = { x: event.clientX, y: event.clientY, armed };
    },
    onAuxClick: (event: MouseEvent) => {
      if (event.button !== MIDDLE_BUTTON || blocked) return;
      event.preventDefault();
      window.open(routeHash({ kind: "session", id }), "_blank", "noopener");
    },
    onClick: (event: MouseEvent) => {
      const start = down.current;
      down.current = null;
      if (blocked) return;
      if (start !== null) {
        const travel = Math.hypot(event.clientX - start.x, event.clientY - start.y);
        if (start.armed || travel >= TAP_SLOP_PX) return;
      }
      open();
    },
    onKeyDown: (event: KeyboardEvent) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      open();
    },
  };
}

/** Killed by two left swipes or presses, archived by one left, restored by one right. */
function ActionRow({
  client,
  session,
  now,
  action,
  onRun,
  onFailed,
}: {
  client: Client;
  session: SessionView;
  now: number;
  action: RowAction;
  onRun: () => void;
  onFailed: (error: string) => void;
}) {
  const [pending, setPending] = useState(false);
  const run = () => {
    setPending(true);
    onRun();
    client
      .command({ type: action, session: session.id, client_id: client.newClientId() })
      .catch((err: unknown) => onFailed(errorMessage(err)));
  };
  const { surface, view, press } = useSwipe(rowSwipeRules(action), run);
  const coarse = coarsePointer();
  const side = actionSide(action);
  const armed = view.armed !== null;
  const slidOut = action !== "kill" && view.fired !== null;
  const tap = useOpenOnTap(session.id, armed, view.fired !== null);
  const highlight = armed ? null : rowHighlight(session, now);
  const busyLabel = !pending
    ? null
    : action === "restore"
      ? "Restoring..."
      : action === "kill"
        ? "Stopping..."
        : null;
  const button = !coarse && (
    <button
      type="button"
      disabled={view.fired !== null}
      onClick={() => press(side)}
      className={`${SIDE_BUTTON} ${side === "left" ? "rotate-180" : ""} ${ACTION_BUTTON[action]} ${armed ? "bg-destructive text-black" : ""}`}
    >
      {armed ? "Kill?" : ACTION_LABEL[action]}
    </button>
  );

  return (
    <div className="flex items-stretch gap-2">
      {side === "left" && button}
      {/* The pulse sits here: the clipping below would cut a shadow drawn by the row. */}
      <div
        className={`relative min-w-0 flex-1 overflow-hidden rounded-[5px] ${highlight !== null ? "row-pulse" : ""}`}
        style={highlight !== null ? pulseStyle(PULSE[highlight]) : undefined}
      >
        {coarse && (
          <div
            aria-hidden
            className={`voice-ui absolute inset-0 flex items-center rounded-[5px] px-4 text-sm text-black ${side === "left" ? "justify-end" : "justify-start"}`}
            style={{
              background: slidOut ? REVEAL[action].latched : revealBackground(action, view.lean),
            }}
          >
            {ACTION_LABEL[action]}
          </div>
        )}
        <div
          ref={coarse ? surface : undefined}
          {...tap}
          className={`${ROW} hover:bg-muted relative w-full touch-pan-y select-none ${armed ? "border-destructive" : ""} ${highlight !== null ? "border-transparent" : ""} ${session.live || highlight !== null ? "" : "border-long-dash"}`}
          style={{
            transform: slidOut
              ? `translateX(${side === "left" ? "-110%" : "110%"})`
              : `translateX(${view.offset}px)`,
            transition: view.dragging ? "none" : SPRING_BACK,
          }}
        >
          <RowBody session={session} now={now} busyLabel={busyLabel} />
        </div>
        {coarse && armed && (
          <span className="bg-card voice-heading text-destructive pointer-events-none absolute top-1.5 right-2 rounded-[5px] px-2 py-0.5 text-sm shadow-sm">
            Kill?
          </span>
        )}
      </div>
      {side === "right" && button}
    </div>
  );
}

function Entry({ client, session, now }: { client: Client; session: SessionView; now: number }) {
  const action = rowAction(session);
  const [attempt, setAttempt] = useState(0);
  const [error, setError] = useState<string | null>(null);
  return (
    <li className="space-y-1">
      <ActionRow
        key={`${action}-${attempt}`}
        client={client}
        session={session}
        now={now}
        action={action}
        onRun={() => setError(null)}
        onFailed={(message) => {
          setError(message);
          setAttempt((count) => count + 1);
        }}
      />
      {error !== null && <p className="text-destructive voice-ui text-xs">{error}</p>}
    </li>
  );
}

/** Once above a list on touch screens, on the side its rows slide toward. */
function SwipeHint({ side, text }: { side: "left" | "right"; text: string }) {
  if (!coarsePointer()) return null;
  return (
    <div
      className={`text-muted-foreground voice-ui text-xs ${side === "left" ? "text-left" : "text-right"}`}
    >
      {side === "left" ? `‹ ${text}` : `${text} ›`}
    </div>
  );
}

function NewScratch({ client }: { client: Client }) {
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const start = async () => {
    setStarting(true);
    setError(null);
    const result = await client.createScratch();
    setStarting(false);
    if (result.ok) navigate({ kind: "session", id: result.session.id });
    else setError(result.error);
  };
  return (
    <>
      <button
        type="button"
        disabled={starting}
        onClick={() => void start()}
        className="bg-card hover:bg-muted voice-ui rounded-[5px] border px-3 py-1 text-sm disabled:opacity-50"
      >
        {starting ? "Starting..." : "New scratch"}
      </button>
      {error !== null && <p className="text-destructive voice-ui w-full text-xs">{error}</p>}
    </>
  );
}

function Sessions({
  client,
  sessions,
  now,
}: {
  client: Client;
  sessions: readonly SessionView[];
  now: number;
}) {
  const [showArchived, setShowArchived] = useState(false);
  const sections = homeSections(sessions);
  // Rows keep their order while Home is visible, so none moves under a tap.
  const activeOrder = useRef<string[]>([]);
  const scratchOrder = useRef<string[]>([]);
  const archivedOrder = useRef<string[]>([]);
  const [, setHidden] = useState(0);
  useEffect(() => {
    const onHide = () => {
      if (document.visibilityState === "hidden") setHidden((count) => count + 1);
    };
    document.addEventListener("visibilitychange", onHide, { passive: true });
    window.addEventListener("pagehide", onHide, { passive: true });
    return () => {
      document.removeEventListener("visibilitychange", onHide);
      window.removeEventListener("pagehide", onHide);
    };
  }, []);
  const frozen = document.visibilityState === "visible";
  const active = frozen ? keepOrder(activeOrder.current, sections.active) : sections.active;
  const scratch = frozen ? keepOrder(scratchOrder.current, sections.scratch) : sections.scratch;
  const archived = frozen ? keepOrder(archivedOrder.current, sections.archived) : sections.archived;
  activeOrder.current = active.map((session) => session.id);
  scratchOrder.current = scratch.map((session) => session.id);
  archivedOrder.current = archived.map((session) => session.id);
  return (
    <>
      {active.length === 0 ? (
        <p className="text-muted-foreground text-sm">No active sessions.</p>
      ) : (
        <div className="space-y-1">
          <SwipeHint side="left" text="Kill / Archive" />
          <ul className="space-y-2">
            {active.map((session) => (
              <Entry key={session.id} client={client} session={session} now={now} />
            ))}
          </ul>
        </div>
      )}
      <section className="space-y-2">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 className="text-muted-foreground voice-ui px-2 text-sm">Scratch</h2>
          <NewScratch client={client} />
        </div>
        {scratch.length > 0 && (
          <div className="space-y-1">
            {active.length === 0 && <SwipeHint side="left" text="Kill / Archive" />}
            <ul className="space-y-2">
              {scratch.map((session) => (
                <Entry key={session.id} client={client} session={session} now={now} />
              ))}
            </ul>
          </div>
        )}
      </section>
      {archived.length > 0 && (
        <section className="space-y-2">
          <button
            type="button"
            aria-expanded={showArchived}
            onClick={() => setShowArchived((shown) => !shown)}
            className="text-muted-foreground voice-ui hover:bg-muted rounded-[5px] px-2 py-1 text-sm"
          >
            {showArchived ? "▾" : "▸"} Archived ({archived.length})
          </button>
          {showArchived && (
            <div className="space-y-1">
              <SwipeHint side="right" text="Restore" />
              <ul className="space-y-2">
                {archived.map((session) => (
                  <Entry key={session.id} client={client} session={session} now={now} />
                ))}
              </ul>
            </div>
          )}
        </section>
      )}
    </>
  );
}

export function SessionList({ client }: { client: Client }) {
  const snapshot = useSnapshot(client);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), CLOCK_TICK_MS);
    return () => clearInterval(timer);
  }, []);

  return (
    <>
      <Header
        left={<span className="text-primary voice-heading px-2 text-lg">vigie</span>}
        right={<ConnectionDot status={snapshot.status} />}
        usage={snapshot.usage}
        column={<NeedsYouColumn sessions={snapshot.sessions} />}
        info={null}
      />
      <main className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
        <div className="mx-auto max-w-3xl space-y-6 p-4">
          <OpenExplorer />
          {snapshot.sessionsError !== null && (
            <p className="text-destructive text-sm">{snapshot.sessionsError}</p>
          )}
          {snapshot.sessions === null ? (
            <p className="text-muted-foreground text-sm">Loading sessions...</p>
          ) : (
            <Sessions client={client} sessions={snapshot.sessions} now={now} />
          )}
        </div>
      </main>
    </>
  );
}
