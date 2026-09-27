import { type ReactNode, useEffect, useRef, useState } from "react";

import { ReloadButton } from "@/components/Reload";
import { useClock } from "@/components/useNow";
import {
  type CacheGauge,
  cacheGauge,
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
import type { Confidence, ContextView, SessionView, UsageSnapshot, WindowReport } from "@/wire";

const TWEEN_MS = 700;
const REDUCED_MOTION = "(prefers-reduced-motion: reduce)";
const TRACK_SHAPE = "relative h-2 min-w-0 flex-1 overflow-hidden rounded-[3px]";
const TRACK = `${TRACK_SHAPE} bg-muted`;
/** An empty track: nothing known, so nothing that could read as a value. */
const EMPTY_TRACK = `${TRACK_SHAPE} border border-dashed`;
const CONTEXT_FILL = { ok: "bg-foreground", high: "bg-gauge-glow", full: "bg-destructive" };
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

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex h-4 items-center gap-2">
      <span className="text-muted-foreground w-[5ch] shrink-0">{label}</span>
      {children}
    </div>
  );
}

function Figures({
  main,
  side,
  className = "",
}: {
  main: ReactNode;
  side: string;
  className?: string;
}) {
  return (
    <>
      <span className={`w-[8ch] shrink-0 text-right tabular-nums ${className}`}>{main}</span>
      <span className="text-muted-foreground w-[4ch] shrink-0 text-right tabular-nums">{side}</span>
    </>
  );
}

function EmptyRow({ label }: { label: string }) {
  return (
    <Row label={label}>
      <div className={EMPTY_TRACK} />
      <Figures main="--" side="" />
    </Row>
  );
}

function WindowRow({ kind, report, now }: { kind: WindowKind; report: WindowReport; now: number }) {
  const used = useTweened(report.used);
  const projected = useTweened(report.projected);
  const bar = windowBar(kind, { ...report, used, projected });
  return (
    <Row label={kind}>
      <div className={TRACK}>
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
        side={formatSpan(Date.parse(report.resets_at) - now)}
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
      {fiveHour === null ? <EmptyRow label="5h" /> : <WindowRow kind="5h" report={fiveHour} now={now} />}
      {sevenDay === null ? <EmptyRow label="7d" /> : <WindowRow kind="7d" report={sevenDay} now={now} />}
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
      <Figures
        main={formatPercent(percentage)}
        side={formatTokens(context.used_tokens)}
        className={tone === "full" ? "text-destructive" : ""}
      />
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
          <Figures main="cold" side="" className="text-gauge-cold" />
        </Row>
      );
    case "warm":
      return (
        <Row label="cache">
          <div className={TRACK}>
            <div
              className={`absolute inset-y-0 left-0 transition-[width] duration-1000 ease-linear motion-reduce:transition-none ${gauge.low ? "bg-gauge-glow" : "bg-foreground"}`}
              style={{ width: `${gauge.fraction * 100}%` }}
            />
          </div>
          <Figures
            main={formatSpan(gauge.remainingMs)}
            side=""
            className={gauge.low ? "text-gauge-glow" : ""}
          />
        </Row>
      );
  }
}

export function SessionColumn({ session }: { session: SessionView | null }) {
  const now = useClock(1000);
  const gauge = cacheGauge(session?.cache ?? null, now);
  return (
    <div className="min-w-0">
      <ContextRow context={session?.context ?? null} />
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
 * The top bar, then a thin bar. The side slots keep fixed widths so the account
 * column sits at the same place on every screen. Gauge columns are side by side
 * from 600 px, stacked below.
 */
export function Header({
  left,
  right,
  usage,
  column,
  info,
  label,
}: {
  left: ReactNode;
  right: ReactNode;
  usage: UsageSnapshot | null;
  /** Second gauge column, beside or under the account column. */
  column: ReactNode;
  /** Left of the thin bar. */
  info: ReactNode;
  /** Device label, right of the thin bar. */
  label: string | null;
}) {
  const now = useClock(1000);
  return (
    <header className="shrink-0">
      <div className="flex items-start gap-2 border-b px-2 py-1.5">
        <div className="flex w-24 shrink-0 items-center self-center overflow-hidden whitespace-nowrap">
          {left}
        </div>
        <div
          aria-label="Usage, context and cache"
          className="voice-code grid min-w-0 flex-1 grid-cols-1 gap-x-4 gap-y-0.5 text-[11px] leading-4 select-none min-[600px]:grid-cols-2"
        >
          <AccountColumn usage={usage} now={now} />
          {column}
        </div>
        <div className="flex w-6 shrink-0 items-center justify-end gap-1.5 self-center">
          {right}
        </div>
      </div>
      <div className="voice-ui flex h-7 items-center gap-2 border-b pr-1 pl-3 text-xs">
        <div className="min-w-0 flex-1 truncate">{info}</div>
        {label !== null && (
          <span className="text-muted-foreground max-w-[50%] min-w-0 truncate">{label}</span>
        )}
        <ReloadButton />
      </div>
    </header>
  );
}
