import { type ReactNode, useEffect, useRef, useState } from "react";

import { useClock } from "@/components/useNow";
import {
  type CacheGauge,
  cacheGauge,
  cacheTickStep,
  contextTone,
  formatPercent,
  formatSpan,
  formatTokens,
  isStale,
  needsYou,
  readingAge,
  type Tone,
  tween,
  type WindowKind,
  windowBar,
} from "@/gauges";
import { type RowHighlight, strongestHighlight } from "@/home";
import { goHome, navigate } from "@/route";
import type {
  CacheView,
  Confidence,
  ContextView,
  SessionView,
  UsageSnapshot,
  WindowReport,
} from "@/wire";

/** Border pulse per highlight; work is dimmed so it does not draw the eye. */
export const PULSE: Record<RowHighlight, string> = {
  ask: "var(--gauge-glow)",
  stopped: "var(--destructive)",
  finished: "var(--gauge-ok)",
  working: "color-mix(in oklch, var(--gauge-cold) 35%, transparent)",
};

/** A custom property, which React's style type does not list. */
export function pulseStyle(color: string): Record<string, string> {
  return { "--row-pulse": color };
}

/**
 * The top bar's left slot on every screen but Home: back to Home. It pulses
 * like a Home row, in the strongest state among the sessions not on screen, work aside.
 */
export function HomeButton({
  sessions,
  onScreen = null,
  onClick = goHome,
}: {
  sessions: readonly SessionView[] | null;
  /** The session this screen shows, left out of the pulse. */
  onScreen?: string | null;
  onClick?: () => void;
}) {
  const strongest = sessions === null ? null : strongestHighlight(sessions, onScreen);
  // Work in progress wants nothing from the user, so only the other states light it.
  const highlight = strongest === "working" ? null : strongest;
  return (
    <button
      type="button"
      onClick={() => onClick()}
      aria-label="Home"
      title="Home"
      style={highlight === null ? undefined : pulseStyle(PULSE[highlight])}
      className={`text-primary hover:bg-muted shrink-0 rounded-[5px] p-1.5 ${highlight === null ? "" : "row-pulse"}`}
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth={1.6}
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden
        className="size-6"
      >
        <path d="M3 11 12 4l9 7" />
        <path d="M5 10v10h14V10" />
      </svg>
    </button>
  );
}

const TWEEN_MS = 700;
const REDUCED_MOTION = "(prefers-reduced-motion: reduce)";
const TRACK_SHAPE = "relative h-2 min-w-0 flex-1 overflow-hidden rounded-[3px]";
const TRACK = `${TRACK_SHAPE} bg-muted bar-ticks`;
/** An empty track: nothing known, so nothing that could read as a value. */
const EMPTY_TRACK = `${TRACK_SHAPE} border border-dashed`;
const CONTEXT_FILL = { ok: "bg-foreground", high: "bg-gauge-glow", full: "bg-destructive" };
/** A track's notch spacing, a custom property React's style type does not list. */
function tickStyle(percent: number): Record<string, string> {
  return { "--tick-step": `${percent}%` };
}

const CONTEXT_TEXT ={ ok: "", high: "text-gauge-glow", full: "text-destructive" };
const TONE_FILL: Record<Tone, string> = {
  ok: "bg-gauge-ok",
  warning: "bg-gauge-glow",
  critical: "bg-destructive",
};
const TONE_TEXT: Record<Tone, string> = {
  ok: "text-gauge-ok",
  warning: "text-gauge-glow",
  critical: "text-destructive",
};
const CONFIDENCE_EMPHASIS: Record<Confidence, string> = {
  low: "opacity-60",
  medium: "",
  high: "font-bold",
};

/** Follows `target` with an ease-out, or jumps when the system asks for reduced motion. */
function useTweened(target: number): number {
  const [value, setValue] = useState(target);
  const current = useRef(target);
  useEffect(() => {
    const from = current.current;
    if (from === target) return;
    if (window.matchMedia(REDUCED_MOTION).matches) {
      current.current = target;
      setValue(target);
      return;
    }
    const start = performance.now();
    let frame = 0;
    const step = (time: number) => {
      const progress = (time - start) / TWEEN_MS;
      current.current = tween(from, target, progress);
      setValue(current.current);
      if (progress < 1) frame = requestAnimationFrame(step);
    };
    frame = requestAnimationFrame(step);
    return () => cancelAnimationFrame(frame);
  }, [target]);
  return value;
}

/**
 * Every row has the same label and figures widths, so all tracks line up. The
 * label fits a window's time left, up to `6d23h/7d`. Text on each side hugs the track.
 */
function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex h-4 items-center gap-2">
      <span className="text-muted-foreground w-[8ch] shrink-0 text-right tabular-nums">
        {label}
      </span>
      {children}
    </div>
  );
}

function Figures({ main, className = "" }: { main: ReactNode; className?: string }) {
  return <span className={`w-[8ch] shrink-0 tabular-nums ${className}`}>{main}</span>;
}

function EmptyRow({ label }: { label: string }) {
  return (
    <Row label={label}>
      <div className={EMPTY_TRACK} />
      <Figures main="--" />
    </Row>
  );
}

function EmptyWindowRow({ kind }: { kind: WindowKind }) {
  return (
    <Row label={`--/${kind}`}>
      <div className={EMPTY_TRACK} />
      <Figures main="--" />
    </Row>
  );
}

function WindowRow({ kind, report, now }: { kind: WindowKind; report: WindowReport; now: number }) {
  const used = useTweened(report.used);
  const projected = useTweened(report.projected);
  const bar = windowBar(kind, { ...report, used, projected });
  return (
    <Row label={`${formatSpan(Date.parse(report.resets_at) - now)}/${kind}`}>
      <div className={TRACK} style={kind === "7d" ? tickStyle(100 / 7) : undefined}>
        <div
          className={`absolute inset-y-0 left-0 ${bar.full ? "bg-destructive" : "bg-foreground"}`}
          style={{ width: `${bar.spent}%` }}
        />
        {bar.projectionLength > 0 && (
          <div
            className={`absolute inset-y-0 ${TONE_FILL[bar.projectionTone]}`}
            style={{
              left: `${bar.spent}%`,
              width: `${bar.projectionLength}%`,
              opacity: bar.projectionOpacity,
            }}
          />
        )}
      </div>
      <Figures
        main={
          <>
            <span className={TONE_TEXT[bar.usedTone]}>{Math.round(used)}</span>
            <span className="text-muted-foreground">▸</span>
            <span
              className={`${TONE_TEXT[bar.projectionTone]} ${CONFIDENCE_EMPHASIS[report.confidence]}`}
            >
              {Math.round(projected)}%
            </span>
          </>
        }
      />
    </Row>
  );
}

function AccountColumn({ usage, now }: { usage: UsageSnapshot | null; now: number }) {
  const stale = isStale(readingAge(usage, now));
  const fiveHour = usage?.five_hour ?? null;
  const sevenDay = usage?.seven_day ?? null;
  return (
    <div className={`min-w-0 transition-opacity duration-500 ${stale ? "opacity-50" : ""}`}>
      {fiveHour === null ? (
        <EmptyWindowRow kind="5h" />
      ) : (
        <WindowRow kind="5h" report={fiveHour} now={now} />
      )}
      {sevenDay === null ? (
        <EmptyWindowRow kind="7d" />
      ) : (
        <WindowRow kind="7d" report={sevenDay} now={now} />
      )}
    </div>
  );
}

function ContextRow({ context }: { context: ContextView | null }) {
  const percentage = useTweened(context?.percentage ?? 0);
  if (context === null) return <EmptyRow label="ctx" />;
  const tone = contextTone(context);
  return (
    <Row label="ctx">
      <div className={TRACK}>
        <div
          className={`absolute inset-y-0 left-0 ${CONTEXT_FILL[tone]}`}
          style={{ width: `${Math.min(100, percentage)}%` }}
        />
      </div>
      <Figures main={formatPercent(percentage)} className={CONTEXT_TEXT[tone]} />
    </Row>
  );
}

/** A subagent's context: its token count only, its window being unknown. */
function AgentContextRow({ promptTokens }: { promptTokens: number }) {
  return (
    <Row label="ctx">
      <div className={EMPTY_TRACK} />
      <Figures main={promptTokens > 0 ? formatTokens(promptTokens) : "--"} />
    </Row>
  );
}

function CacheRow({ gauge }: { gauge: CacheGauge }) {
  switch (gauge.kind) {
    case "unknown":
      return <EmptyRow label="cache" />;
    case "cold":
      return (
        <Row label="cache">
          <div className={`${EMPTY_TRACK} border-gauge-cold`} />
          <Figures main="cold" className="text-gauge-cold" />
        </Row>
      );
    case "warm":
      return (
        <Row label="cache">
          <div className={TRACK} style={tickStyle(cacheTickStep(gauge.ttlMs))}>
            <div
              className={`absolute inset-y-0 left-0 transition-[width] duration-1000 ease-linear motion-reduce:transition-none ${gauge.low ? "bg-gauge-glow" : "bg-foreground"}`}
              style={{ width: `${gauge.fraction * 100}%` }}
            />
          </div>
          <Figures
            main={formatSpan(gauge.remainingMs)}
            className={gauge.low ? "text-gauge-glow" : ""}
          />
        </Row>
      );
  }
}

/** `agent` set: the context and cache bars are that agent's, from its own last request. */
export function SessionColumn({
  session,
  agent,
}: {
  session: SessionView | null;
  agent?: { cache: CacheView | null; promptTokens: number };
}) {
  const now = useClock(1000);
  const gauge =
    agent === undefined
      ? cacheGauge(session?.cache ?? null, now, session?.updated_at ?? null)
      : cacheGauge(agent.cache, now);
  return (
    <div className="min-w-0">
      {agent === undefined ? (
        <ContextRow context={session?.context ?? null} />
      ) : (
        <AgentContextRow promptTokens={agent.promptTokens} />
      )}
      <CacheRow gauge={gauge} />
    </div>
  );
}

/** `sessions` is null while the list is loading. */
export function NeedsYouColumn({ sessions }: { sessions: readonly SessionView[] | null }) {
  const counts = sessions === null ? null : needsYou(sessions);
  const asksTone = counts !== null && counts.asks > 0 ? "text-gauge-glow" : "text-muted-foreground";
  return (
    <div className="min-w-0">
      <div className="flex h-4 items-center gap-4">
        <span className={`tabular-nums ${asksTone}`}>asks {counts?.asks ?? "--"}</span>
        <span className="text-muted-foreground tabular-nums">working {counts?.working ?? "--"}</span>
      </div>
      <div className="h-4" />
    </div>
  );
}

/**
 * The top bar, then a thin bar. The gauges grow to the discussion's width first,
 * then the side slots share what is left equally, keeping the gauges centered.
 * Gauge columns are side by side from 600 px, stacked below.
 */
export function Header({
  left,
  right,
  usage,
  column,
  info,
  pills,
  end,
}: {
  left: ReactNode;
  right: ReactNode;
  usage: UsageSnapshot | null;
  /** Second gauge column, beside or under the account column. */
  column: ReactNode;
  /** Left of the thin bar. */
  info: ReactNode;
  /** Right after `info`, in a session: the mode and model pills. */
  pills?: ReactNode;
  /** Right of the thin bar, in a session: the project folder button. */
  end?: ReactNode;
}) {
  const now = useClock(1000);
  return (
    <header className="shrink-0">
      <div className="grid grid-cols-[minmax(6rem,1fr)_minmax(0,64rem)_minmax(3.5rem,1fr)] items-start gap-4 border-b px-2 py-1.5">
        {/* Not clipped: the Home button's pulse glows past it. */}
        <div className="flex items-center self-center whitespace-nowrap">{left}</div>
        <div
          aria-label="Usage, context and cache"
          className="voice-code grid min-w-0 grid-cols-1 gap-x-6 gap-y-0.5 text-[11px] leading-4 select-none min-[600px]:grid-cols-2 min-[600px]:self-center"
        >
          <AccountColumn usage={usage} now={now} />
          {column}
        </div>
        <div className="flex items-center justify-end gap-1.5 self-center">
          {right}
          <button
            type="button"
            aria-label="Device settings"
            title="Device settings"
            onClick={() => navigate({ kind: "settings" })}
            className="text-muted-foreground hover:bg-muted hover:text-foreground flex size-10 shrink-0 items-center justify-center rounded-[5px] text-3xl leading-none"
          >
            ⚙
          </button>
        </div>
      </div>
      <div className="voice-ui flex h-7 items-center gap-2 border-b pr-1 pl-3 text-xs">
        <div className="min-w-0 truncate">{info}</div>
        {pills}
        <div className="flex-1" />
        {end}
      </div>
    </header>
  );
}
