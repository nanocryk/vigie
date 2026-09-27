import {
  type KeyboardEvent,
  type ReactNode,
  useEffect,
  useRef,
  useState,
} from "react";

import { useClock } from "@/components/useNow";
import {
  type CacheGauge,
  cacheGauge,
  contextTone,
  formatPercent,
  formatSpan,
  formatTokens,
  formatTtl,
  isStale,
  readingAge,
  type Tone,
  tween,
  type WindowKind,
  windowBar,
} from "@/gauges";
import { formatExact, formatStamp } from "@/format";
import type { Confidence, ContextView, SessionView, UsageSnapshot, WindowReport } from "@/wire";

const TWEEN_MS = 700;
const REDUCED_MOTION = "(prefers-reduced-motion: reduce)";
/** For a child of a wrapping header row: its own row below 600 px, the remaining width of the row above. */
export const TOP_BAR_IN_ROW =
  "order-last basis-full min-[600px]:order-none min-[600px]:basis-0 min-[600px]:flex-1";
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

function windowDetails(label: string, report: WindowReport | null, now: number): string[] {
  if (report === null) return [`${label}: no reading in this window`];
  const resets = Date.parse(report.resets_at);
  const lines = [
    `${label}: ${formatPercent(report.used)} used, ${formatPercent(report.projected)} projected (${report.confidence}), rate ×${report.rate.toFixed(2)}`,
    `    resets ${formatStamp(resets, now)} (in ${formatSpan(resets - now)})`,
  ];
  if (report.work_left_hours !== null) {
    lines.push(`    work left ${report.work_left_hours.toFixed(1)} h`);
  }
  if (report.deadline !== null) {
    const deadline = Date.parse(report.deadline);
    lines.push(`    limit reached ${formatStamp(deadline, now)} (in ${formatSpan(deadline - now)})`);
  }
  return lines;
}

function AccountColumn({
  usage,
  error,
  now,
  expanded,
}: {
  usage: UsageSnapshot | null;
  error: string | null;
  now: number;
  expanded: boolean;
}) {
  const age = readingAge(usage, now);
  const stale = isStale(age);
  const fiveHour = usage?.five_hour ?? null;
  const sevenDay = usage?.seven_day ?? null;
  return (
    <div className={`min-w-0 transition-opacity duration-500 ${stale ? "opacity-50" : ""}`}>
      {fiveHour === null ? <EmptyRow label="5h" /> : <WindowRow kind="5h" report={fiveHour} now={now} />}
      {sevenDay === null ? <EmptyRow label="7d" /> : <WindowRow kind="7d" report={sevenDay} now={now} />}
      {stale && age !== null && (
        <div className="text-muted-foreground">readings {formatSpan(age)} old</div>
      )}
      {expanded && (
        <div className="text-muted-foreground mt-1 whitespace-pre-wrap">
          {[
            ...windowDetails("5h", fiveHour, now),
            ...windowDetails("7d", sevenDay, now),
            age === null
              ? "no reading yet"
              : `newest reading ${formatStamp(now - age, now)} (${formatSpan(age)} ago)`,
            ...(error === null ? [] : [error]),
          ].join("\n")}
        </div>
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

function sessionDetails(session: SessionView | null, gauge: CacheGauge, now: number): string[] {
  const context = session?.context ?? null;
  const lines = [
    context === null
      ? "context: unknown until a turn ends"
      : `context: ${context.used_tokens.toLocaleString()} / ${context.max_tokens.toLocaleString()} tokens (${formatPercent(context.percentage)}, ${context.source})`,
  ];
  const cache = session?.cache ?? null;
  switch (gauge.kind) {
    case "unknown":
      lines.push("cache: no request with a known TTL yet");
      break;
    case "warm":
      lines.push(
        `cache: ${formatTtl(gauge.ttlMs)} TTL, last request ${cache === null ? "?" : formatExact(Date.parse(cache.last_request_at))}`,
        `    cold at ${formatStamp(gauge.coldAt, now)} (in ${formatSpan(gauge.remainingMs)})`,
      );
      break;
    case "cold":
      lines.push(
        `cache: ${formatTtl(gauge.ttlMs)} TTL, cold since ${formatStamp(gauge.coldAt, now)} (${formatSpan(now - gauge.coldAt)} ago)`,
      );
      break;
  }
  return lines;
}

/**
 * Account usage windows, then the session's context and cache when `session` is
 * given (undefined on Home). Side by side from 600 px, stacked below; a tap
 * shows every detail.
 */
export function TopBar({
  usage,
  usageError,
  session,
  className = "",
}: {
  usage: UsageSnapshot | null;
  usageError: string | null;
  session?: SessionView | null;
  className?: string;
}) {
  const now = useClock(1000);
  const [expanded, setExpanded] = useState(false);
  const toggle = () => setExpanded((current) => !current);
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      toggle();
    }
  };
  const gauge = cacheGauge(session?.cache ?? null, now);

  return (
    <div
      role="button"
      tabIndex={0}
      aria-expanded={expanded}
      aria-label="Usage, context and cache"
      onClick={toggle}
      onKeyDown={onKeyDown}
      className={`voice-code grid cursor-pointer grid-cols-1 gap-x-4 gap-y-0.5 text-[11px] leading-4 select-none min-[600px]:grid-cols-2 ${className}`}
    >
      <AccountColumn usage={usage} error={usageError} now={now} expanded={expanded} />
      {session !== undefined && (
        <div className="min-w-0">
          <ContextRow context={session?.context ?? null} />
          <CacheRow gauge={gauge} />
          {expanded && (
            <div className="text-muted-foreground mt-1 whitespace-pre-wrap">
              {sessionDetails(session, gauge, now).join("\n")}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
