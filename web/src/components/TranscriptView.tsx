import {
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { DiffBlock } from "@/components/Diff";
import { Markdown } from "@/components/Markdown";
import { useClock, useNow } from "@/components/useNow";
import { DIFF_TOOLS, type FileDiff, changeSummary, toolDiff } from "@/diff";
import { formatDuration, formatExact, formatStamp } from "@/format";
import { changeExplorer, openFileInLastFree } from "@/files";
import { formatSpan } from "@/gauges";
import { navigate } from "@/route";
import type { BackgroundView } from "@/wire";
import type { Beginning, Request } from "@/history";
import {
  type Delivery,
  type Item,
  type TaskEndStatus,
  type ToolStatus,
  quietWork,
  relayFailureText,
  restartLabel,
  toolTimeVisible,
  wakeStops,
} from "@/transcript";

/** Distance from the bottom, in px, still counted as "at the bottom". */
const BOTTOM_SLACK = 24;
/** Earlier history is asked for once the view is within this many screen heights of the top. */
const NEAR_TOP_SCREENS = 1;

export interface PendingSend {
  id: string;
  text: string;
  /** One per attempt; the server's `user_sent` carrying any of them replaces this entry. */
  clientIds: readonly string[];
  status: "sending" | "sent" | "failed";
  error: string | null;
}

const BUBBLE = "min-w-0 rounded-[5px] px-3 py-2 text-sm";

/** One row per background task: "In background" during a turn, "Waiting for" once it ended. */
function BackgroundRows({
  tasks,
  waiting,
  onStop,
}: {
  tasks: readonly BackgroundView[];
  /** The turn ended and Claude waits for these to continue. */
  waiting: boolean;
  onStop: (taskId: string) => Promise<void>;
}) {
  const now = useClock(1000);
  const [stopping, setStopping] = useState<ReadonlySet<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const stop = (taskId: string) => {
    setError(null);
    setStopping((current) => new Set(current).add(taskId));
    onStop(taskId).catch((err: unknown) => {
      setError(`Stop failed: ${err instanceof Error ? err.message : String(err)}`);
      setStopping((current) => {
        const next = new Set(current);
        next.delete(taskId);
        return next;
      });
    });
  };
  // Full-width bars, like the restart divider.
  return (
    <div className="-mx-3 space-y-px pt-2">
      {tasks.map((task) => {
        const started = Date.parse(task.started_at);
        return (
          <div
            key={task.task_id}
            className="bg-muted text-foreground voice-ui flex items-center gap-2 px-3 py-1.5 text-xs"
          >
            <span className="min-w-0 flex-1 truncate">
              <span className="text-muted-foreground">
                {waiting ? "Waiting for: " : "In background: "}
              </span>
              {task.description ?? task.task_id}
              <span className="text-muted-foreground">
                {Number.isNaN(started) ? "" : ` · ${formatSpan(now - started)}`}
              </span>
              {waiting && (
                <span className="ml-1.5">
                  <Dots />
                </span>
              )}
            </span>
            <button
              type="button"
              disabled={stopping.has(task.task_id)}
              onClick={() => stop(task.task_id)}
              className="border-destructive/50 text-destructive hover:bg-destructive/10 shrink-0 rounded-[5px] border px-2 py-0.5 disabled:opacity-50"
            >
              {stopping.has(task.task_id) ? "Stopping..." : "Stop"}
            </button>
          </div>
        );
      })}
      {error !== null && <p className="text-destructive voice-ui px-3 text-xs">{error}</p>}
    </div>
  );
}

/** Three dots animated in turn: work is going on. */
function Dots() {
  return (
    <span className="working-dots" aria-hidden>
      <span>.</span>
      <span>.</span>
      <span>.</span>
    </span>
  );
}

function UserBubble({ children, dim }: { children: ReactNode; dim?: boolean }) {
  return (
    <div
      className={`bg-bubble-user border-ring ml-16 border-r-2 ${BUBBLE} rounded-r-none ${dim === true ? "opacity-60" : ""}`}
    >
      {children}
    </div>
  );
}

const STATUS_TEXT: Record<ToolStatus, string> = {
  running: "...",
  ok: "ok",
  error: "err",
  unfinished: "--",
};

const STATUS_CLASS: Record<ToolStatus, string> = {
  running: "text-primary",
  ok: "text-muted-foreground",
  error: "text-destructive",
  unfinished: "text-muted-foreground",
};

const PANEL =
  "bg-background voice-code max-h-80 overflow-auto rounded-[5px] p-2 text-xs whitespace-pre-wrap [overflow-wrap:anywhere]";

const STAMP = "text-muted-foreground voice-ui text-[0.6875rem] leading-none";

/** Exact start time and, once the result is in, the call's duration. */
function toolTiming(item: Extract<Item, { kind: "tool" }>): string | null {
  if (item.time === null) return null;
  const exact = formatExact(item.time);
  return item.endTime === null ? exact : `${exact} · ${formatDuration(item.endTime - item.time)}`;
}

type ToolItem = Extract<Item, { kind: "tool" }>;

const TOOL_SUMMARY =
  "hover:bg-muted flex cursor-pointer list-none items-baseline gap-2 rounded-r-[5px] px-1 py-0.5";

function StatusMark({ status }: { status: ToolStatus }) {
  return (
    <span className={`voice-code w-7 shrink-0 text-xs ${STATUS_CLASS[status]}`}>
      {status === "running" ? <Dots /> : STATUS_TEXT[status]}
    </span>
  );
}

function ToolOutput({ item }: { item: ToolItem }) {
  return (
    item.output !== null && (
      <pre className={`${PANEL} ${item.status === "error" ? "text-destructive" : ""}`}>
        {item.output === "" ? "(no output)" : item.output}
      </pre>
    )
  );
}

function ToolInput({ item }: { item: ToolItem }) {
  return (
    <details>
      <summary className="text-muted-foreground voice-ui cursor-pointer text-xs">input</summary>
      <pre className={`${PANEL} mt-1`}>{JSON.stringify(item.input, null, 2)}</pre>
    </details>
  );
}

function ToolRow({
  item,
  stamp,
  awaiting,
}: {
  item: ToolItem;
  /** Shown at the row's end; null hides it. */
  stamp: string | null;
  awaiting: boolean;
}) {
  const diff = useMemo(
    () => (DIFF_TOOLS.has(item.name) ? toolDiff({ ...item, path: item.summary }) : null),
    [item],
  );
  if (diff !== null && awaiting) {
    return (
      <div className="border-ring border-l-2 text-sm">
        <div className={TOOL_SUMMARY}>
          <StatusMark status={item.status} />
          <span className="voice-code min-w-0 truncate">
            <span className="voice-ui">{diff.verb}</span>({diff.path})
          </span>
          <span className="text-muted-foreground voice-ui ml-auto shrink-0 pl-2 text-xs">
            awaiting approval
          </span>
        </div>
      </div>
    );
  }
  if (diff !== null) return <DiffRow item={item} diff={diff} stamp={stamp} />;
  const timing = toolTiming(item);
  return (
    <details className="group open:bg-muted border-ring rounded-r-[5px] border-l-2 text-sm open:pb-2">
      <summary className={TOOL_SUMMARY}>
        <StatusMark status={item.status} />
        <span className="voice-ui shrink-0">{item.name}</span>
        <span className="voice-code text-muted-foreground min-w-0 truncate">{item.summary}</span>
        {stamp !== null && <span className={`${STAMP} ml-auto shrink-0 pl-2`}>{stamp}</span>}
      </summary>
      <div className="mt-1 mr-2 ml-9 space-y-2">
        {timing !== null && <div className={STAMP}>{timing}</div>}
        <ToolOutput item={item} />
        {item.attached !== null && (
          <details>
            <summary className="text-muted-foreground voice-ui cursor-pointer text-xs">
              skill content
            </summary>
            <pre className={`${PANEL} mt-1`}>{item.attached}</pre>
          </details>
        )}
        <ToolInput item={item} />
      </div>
    </details>
  );
}

/** The message that woke Claude after background tasks were stopped, shown as a tool row. */
function WakeRow({ stops, pending }: { stops: readonly string[]; pending: boolean }) {
  return (
    <div className={`${TOOL_SUMMARY} border-ring cursor-default border-l-2 text-sm`}>
      <span className="voice-code text-primary w-7 shrink-0 text-xs">
        {pending ? <Dots /> : "↻"}
      </span>
      <span className="voice-ui shrink-0">Wake</span>
      <span className="voice-code text-muted-foreground min-w-0 truncate">
        Stopped: {stops.join("; ")}
      </span>
    </div>
  );
}

const TASK_END_MARK: Record<TaskEndStatus, { text: string; className: string }> = {
  ok: { text: "ok", className: "text-muted-foreground" },
  stopped: { text: "--", className: "text-muted-foreground" },
  error: { text: "err", className: "text-destructive" },
};

/** A background task's end, shown like a tool row; tapping it shows the notification. */
function TaskEndRow({ item }: { item: Extract<Item, { kind: "task_end" }> }) {
  const mark = TASK_END_MARK[item.status];
  const outputFile = item.outputFile;
  return (
    <details className="group open:bg-muted border-ring rounded-r-[5px] border-l-2 text-sm open:pb-2">
      <summary className={TOOL_SUMMARY}>
        <span className={`voice-code w-7 shrink-0 text-xs ${mark.className}`}>{mark.text}</span>
        <span className="voice-ui shrink-0">Background</span>
        <span className="voice-code text-muted-foreground min-w-0 truncate">{item.summary}</span>
      </summary>
      <div className="mt-1 mr-2 ml-9 space-y-2">
        {outputFile !== null && (
          <button
            type="button"
            onClick={() => {
              changeExplorer((state) => openFileInLastFree(state, outputFile));
              navigate({ kind: "files" });
            }}
            className="text-primary voice-ui rounded-[5px] border px-2 py-0.5 text-xs"
          >
            Open output
          </button>
        )}
        <pre className={PANEL}>{item.raw}</pre>
      </div>
    </details>
  );
}

/** A file change, TUI style: open until its header is tapped, open again after a reload. */
function DiffRow({ item, diff, stamp }: { item: ToolItem; diff: FileDiff; stamp: string | null }) {
  const [open, setOpen] = useState(true);
  const timing = toolTiming(item);
  return (
    <details
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
      className="border-ring rounded-r-[5px] border-l-2 text-sm"
    >
      <summary className={TOOL_SUMMARY}>
        <StatusMark status={item.status} />
        <span className="voice-code min-w-0 truncate">
          <span className="voice-ui">{diff.verb}</span>({diff.path})
        </span>
        {stamp !== null && <span className={`${STAMP} ml-auto shrink-0 pl-2`}>{stamp}</span>}
      </summary>
      <div className="mt-0.5 mr-2 ml-2 space-y-1 pb-1">
        <div className="flex items-baseline gap-2 pl-7">
          <span className="text-muted-foreground voice-ui text-xs">
            &#x2514; {changeSummary(diff.added, diff.removed)}
          </span>
          {timing !== null && <span className={`${STAMP} ml-auto`}>{timing}</span>}
        </div>
        <DiffBlock diff={diff} />
        <details className="space-y-2">
          <summary className="text-muted-foreground voice-ui cursor-pointer text-xs">raw</summary>
          <div className="mt-1 space-y-2">
            <ToolOutput item={item} />
            <ToolInput item={item} />
          </div>
        </details>
      </div>
    </details>
  );
}

const DELIVERY_LABEL: Record<Delivery, string | null> = {
  on_host: "on host",
  received: null,
  handled: "handled locally",
};

/** A bubble's text, rendered or as its markdown source, with the toggle and the time under it. */
function BubbleBody({
  text,
  streaming,
  time,
  now,
}: {
  text: string;
  streaming?: boolean;
  time: number | null;
  now: number;
}) {
  const [raw, setRaw] = useState(false);
  const [copy, setCopy] = useState<"idle" | "copied" | "failed">("idle");
  const rendered = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (copy === "idle") return;
    const timer = setTimeout(() => setCopy("idle"), COPY_FEEDBACK_MS);
    return () => clearTimeout(timer);
  }, [copy]);
  // The text as shown: the markdown source, or the rendered text.
  const copyShown = () => {
    const shown = raw ? text : (rendered.current?.innerText ?? text);
    navigator.clipboard.writeText(shown).then(
      () => setCopy("copied"),
      () => setCopy("failed"),
    );
  };
  return (
    <>
      {raw ? (
        <pre className="voice-code text-sm whitespace-pre-wrap [overflow-wrap:anywhere]">
          {text}
        </pre>
      ) : (
        <div ref={rendered}>
          <Markdown text={text} streaming={streaming} />
        </div>
      )}
      <div className={`${STAMP} mt-1.5 flex items-baseline justify-between gap-2`}>
        <div className="flex gap-3">
          <button
            type="button"
            aria-pressed={raw}
            onClick={() => setRaw((on) => !on)}
            className="hover:text-foreground cursor-pointer"
          >
            {raw ? "[see rendered]" : "[see raw]"}
          </button>
          <button type="button" onClick={copyShown} className="hover:text-foreground cursor-pointer">
            {copy === "copied" ? "[copied]" : copy === "failed" ? "[copy failed]" : "[copy]"}
          </button>
        </div>
        {time !== null && <span>{formatStamp(time, now)}</span>}
      </div>
    </>
  );
}

const COPY_FEEDBACK_MS = 1500;

// Items keep their identity until they change, so unchanged rows skip rendering.
const ItemView = memo(function ItemView({
  item,
  last,
  now,
  toolTime,
  awaiting,
  thinkingNow,
  onOpen,
}: {
  item: Item;
  /** Switches to an agent's discussion, or to the main one with null. */
  onOpen: (toolUseId: string | null) => void;
  last: boolean;
  /** A thinking item that is the latest of a running turn: animated; an empty one is hidden otherwise. */
  thinkingNow: boolean;
  /** Reference for the time labels, refreshed on wake. */
  now: number;
  /** A tool row shows its time (the minute differs from the item above). */
  toolTime: boolean;
  /** A tool row whose ask is pending. */
  awaiting: boolean;
}) {
  switch (item.kind) {
    case "user": {
      const stops = wakeStops(item.text);
      if (stops !== null) return <WakeRow stops={stops} pending={false} />;
      const label = last ? DELIVERY_LABEL[item.delivery] : null;
      // The room above opens each exchange; the first one sits at the top.
      return (
        <div className="space-y-1 pt-8 first:pt-0">
          <UserBubble>
            <BubbleBody text={item.text} time={item.time} now={now} />
          </UserBubble>
          {item.relayed === true && (
            <div className="text-muted-foreground voice-ui px-1 text-right text-xs">
              via main agent
            </div>
          )}
          {label !== null && (
            <div
              className={`text-muted-foreground voice-ui px-1 text-xs ${item.delivery === "handled" ? "text-left" : "text-right"}`}
            >
              {label}
            </div>
          )}
        </div>
      );
    }
    case "claude":
      return (
        <div
          className={`bg-bubble-claude border-ring voice-prose mr-16 border-l-2 ${BUBBLE} rounded-l-none`}
        >
          <BubbleBody text={item.text} streaming={item.streaming} time={item.time} now={now} />
        </div>
      );
    case "thinking":
      if (item.text === "") {
        return thinkingNow ? (
          <div className="text-muted-foreground voice-ui px-1 text-xs">
            thinking
            <Dots />
          </div>
        ) : null;
      }
      return (
        <details className="text-muted-foreground px-1 text-xs">
          <summary className="voice-ui cursor-pointer">
            {thinkingNow ? (
              <>
                thinking
                <Dots />
              </>
            ) : (
              "thought"
            )}
          </summary>
          <div className="mt-1 whitespace-pre-wrap [overflow-wrap:anywhere]">{item.text}</div>
        </details>
      );
    case "tool":
      return (
        <ToolRow
          item={item}
          stamp={toolTime && item.time !== null ? formatStamp(item.time, now) : null}
          awaiting={awaiting}
        />
      );
    case "notice": {
      const tone = item.tone === "error" ? "text-destructive" : "text-muted-foreground";
      return item.detail === null ? (
        <div className={`voice-ui px-1 text-center text-xs ${tone}`}>{item.text}</div>
      ) : (
        <details className={`px-1 text-xs ${tone}`}>
          <summary className="voice-ui cursor-pointer text-center">{item.text}</summary>
          <pre className="bg-muted mt-1 max-h-60 overflow-auto rounded-[5px] p-2 whitespace-pre-wrap [overflow-wrap:anywhere]">
            {item.detail}
          </pre>
        </details>
      );
    }
    case "agent":
      return (
        <div className={`${TOOL_SUMMARY} border-ring cursor-default border-l-2 text-sm`}>
          <span className="voice-code text-primary w-7 shrink-0 text-xs">&#x2192;</span>
          <span className="voice-ui min-w-0 truncate">Agent created: {item.name}</span>
          <button
            type="button"
            onClick={() => onOpen(item.toolUseId)}
            className="text-primary voice-ui ml-auto shrink-0 rounded-[5px] border px-2 text-xs"
          >
            Open
          </button>
        </div>
      );
    case "task_end":
      return <TaskEndRow item={item} />;
    case "brief":
      return (
        <details className="group open:bg-muted border-ring rounded-r-[5px] border-l-2 text-sm open:pb-2">
          <summary className={TOOL_SUMMARY}>
            <span className="voice-code text-muted-foreground w-7 shrink-0 text-xs">&#x2190;</span>
            <span className="voice-ui shrink-0">Brief</span>
            <span className="voice-code text-muted-foreground min-w-0 truncate">{item.text}</span>
          </summary>
          <pre className={`${PANEL} mt-1 mr-2 ml-9`}>{item.text}</pre>
        </details>
      );
    case "relay_failed":
      return (
        <div className="text-destructive voice-ui flex flex-wrap items-baseline justify-center gap-2 px-1 text-xs">
          <span>{relayFailureText(item.reason)}</span>
          <button
            type="button"
            onClick={() => onOpen(null)}
            className="border-destructive rounded-[5px] border px-2"
          >
            Go to main discussion
          </button>
        </div>
      );
    case "restart": {
      const bar = "bg-muted text-foreground voice-ui block px-3 py-1.5 text-center text-xs";
      return (
        <div className="-mx-3 pt-6">
          {item.seed === null ? (
            <div className={bar}>{restartLabel(item)}</div>
          ) : (
            <details>
              <summary className={`${bar} cursor-pointer list-none`}>{restartLabel(item)}</summary>
              <pre className={`${PANEL} mx-3 mt-1`}>{item.seed}</pre>
            </details>
          )}
        </div>
      );
    }
  }
});

function PendingView({
  pending,
  onRetry,
  onDismiss,
}: {
  pending: PendingSend;
  onRetry: (pending: PendingSend) => void;
  onDismiss: (pending: PendingSend) => void;
}) {
  const stops = wakeStops(pending.text);
  if (stops !== null && pending.status !== "failed") return <WakeRow stops={stops} pending />;
  return (
    <div className="space-y-1 pt-8 first:pt-0">
      <UserBubble dim>
        <Markdown text={pending.text} />
      </UserBubble>
      <div className="voice-ui flex justify-end gap-3 text-xs">
        {pending.status === "failed" ? (
          <>
            <span className="text-destructive">{pending.error ?? "Not sent"}</span>
            <button type="button" className="text-primary underline" onClick={() => onRetry(pending)}>
              retry
            </button>
            <button
              type="button"
              className="text-muted-foreground underline"
              onClick={() => onDismiss(pending)}
            >
              dismiss
            </button>
          </>
        ) : (
          <span className="text-muted-foreground">
            {pending.status === "sending" ? "sending..." : "on host"}
          </span>
        )}
      </div>
    </div>
  );
}

/**
 * The scroller follows new content only while it sits at the bottom; once
 * scrolled up, nothing moves it until "latest" is tapped. Earlier history
 * prepended above keeps the view's distance from the bottom.
 */
export function TranscriptView({
  loading,
  firstSeq,
  beginning,
  request,
  onNearTop,
  items: allItems,
  pending,
  onRetry,
  onDismiss,
  awaiting,
  turnOpen,
  onOpen,
  background,
  onStopTask,
  bottomRequest,
  end,
  overlay,
  panel,
}: {
  /** The history is still arriving; it shows at once when complete. */
  loading: boolean;
  /** Seq of the first event shown; a change means earlier history was prepended. */
  firstSeq: number;
  beginning: Beginning;
  request: Request;
  /** The view nears the top of what is loaded. */
  onNearTop: () => void;
  items: readonly Item[];
  pending: readonly PendingSend[];
  onRetry: (pending: PendingSend) => void;
  onDismiss: (pending: PendingSend) => void;
  /** Tool use ids with an ask pending: their diff shows in the ask card, not here. */
  awaiting: ReadonlySet<string>;
  /** A turn is running. */
  turnOpen: boolean;
  /** Switches to an agent's discussion, or to the main one with null; kept stable. */
  onOpen: (toolUseId: string | null) => void;
  /** Background tasks, listed at the end with a Stop button each. */
  background: readonly BackgroundView[];
  onStopTask: (taskId: string) => Promise<void>;
  /** Each change scrolls the view to its bottom. */
  bottomRequest: number;
  /** Closes the discussion, after the background tasks. */
  end?: ReactNode;
  /** Floats at the bottom right, under the "latest" button, aligned with the composer. */
  overlay?: ReactNode;
  /** In front of the discussion, positioned by itself. */
  panel?: ReactNode;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const atBottom = useRef(true);
  /** `scrollHeight - scrollTop` as last laid out or scrolled. */
  const fromBottom = useRef(0);
  const shownFirstSeq = useRef(firstSeq);
  const nearTop = useRef(onNearTop);
  const [showJump, setShowJump] = useState(false);
  const now = useNow();
  const items = useMemo(() => allItems.filter((item) => item.hidden !== true), [allItems]);

  useLayoutEffect(() => {
    nearTop.current = onNearTop;
  }, [onNearTop]);

  const stick = useCallback(() => {
    const element = scroller.current;
    if (element === null) return;
    if (atBottom.current) element.scrollTop = element.scrollHeight;
    fromBottom.current = element.scrollHeight - element.scrollTop;
  }, []);

  const checkTop = useCallback(() => {
    const element = scroller.current;
    if (element !== null && element.scrollTop < element.clientHeight * NEAR_TOP_SCREENS) {
      nearTop.current();
    }
  }, []);

  const onScroll = useCallback(() => {
    const element = scroller.current;
    if (element === null) return;
    const bottom =
      element.scrollHeight - element.scrollTop - element.clientHeight <= BOTTOM_SLACK;
    atBottom.current = bottom;
    fromBottom.current = element.scrollHeight - element.scrollTop;
    setShowJump(!bottom);
    checkTop();
  }, [checkTop]);

  // Opening or closing a panel never moves the view, even at the bottom: the
  // following content is pushed down instead. Position is re-read once it settles.
  const onSummaryClick = (event: ReactMouseEvent) => {
    if (event.target instanceof Element && event.target.closest("summary") !== null) {
      atBottom.current = false;
    }
  };

  useEffect(() => {
    const element = content.current;
    if (element === null) return;
    element.addEventListener("toggle", onScroll, true);
    return () => element.removeEventListener("toggle", onScroll, true);
  }, [onScroll]);

  // A prepend grows the content above the view: keep its distance from the bottom.
  useLayoutEffect(() => {
    const element = scroller.current;
    if (element !== null && shownFirstSeq.current !== firstSeq && !atBottom.current) {
      element.scrollTop = element.scrollHeight - fromBottom.current;
    }
    shownFirstSeq.current = firstSeq;
    stick();
  }, [items, pending, firstSeq, stick]);

  // A short window, or a page leaving the view near the top, asks for more.
  useEffect(checkTop, [items, request, checkTop]);

  let lastUser = items.length - 1;
  while (lastUser >= 0 && items[lastUser]?.kind !== "user") lastUser -= 1;

  useEffect(() => {
    const observer = new ResizeObserver(stick);
    if (scroller.current !== null) observer.observe(scroller.current);
    if (content.current !== null) observer.observe(content.current);
    return () => observer.disconnect();
  }, [stick]);

  // A selection stays inside the item it started in: the others stop being
  // selectable until it is cleared, so a handle cannot extend into them.
  useEffect(() => {
    const element = content.current;
    if (element === null) return;
    let scope: Element | null = null;
    const clear = () => {
      scope?.removeAttribute("data-selection-scope");
      element.removeAttribute("data-selection-scoped");
      scope = null;
    };
    const onChange = () => {
      const selection = document.getSelection();
      if (selection === null || selection.isCollapsed || selection.anchorNode === null) {
        clear();
        return;
      }
      if (scope !== null) return;
      const anchor = selection.anchorNode;
      let item = anchor instanceof Element ? anchor : anchor.parentElement;
      while (item !== null && item.parentElement !== element) item = item.parentElement;
      if (item === null) return;
      scope = item;
      item.setAttribute("data-selection-scope", "");
      element.setAttribute("data-selection-scoped", "");
    };
    document.addEventListener("selectionchange", onChange);
    return () => {
      document.removeEventListener("selectionchange", onChange);
      clear();
    };
  }, []);

  const jump = useCallback(() => {
    atBottom.current = true;
    setShowJump(false);
    stick();
  }, [stick]);

  const lastBottomRequest = useRef(bottomRequest);
  useEffect(() => {
    if (lastBottomRequest.current === bottomRequest) return;
    lastBottomRequest.current = bottomRequest;
    jump();
  }, [bottomRequest, jump]);

  return (
    <div className="relative min-h-0 flex-1">
      <div
        ref={scroller}
        onScroll={onScroll}
        className="h-full overflow-y-auto overscroll-contain"
      >
        <div
          ref={content}
          onClickCapture={onSummaryClick}
          className="mx-auto max-w-5xl space-y-2 px-3 pt-4 pb-4 select-text"
        >
          {items.length === 0 && pending.length === 0 && (
            <p className="text-muted-foreground voice-ui text-center text-sm">
              {loading ? "Loading..." : "No messages yet."}
            </p>
          )}
          {items.length > 0 && (
            // One fixed height whatever it says, so it never shifts the view.
            <div className="text-muted-foreground voice-ui flex h-6 items-center justify-center text-xs">
              {beginning === "loaded"
                ? "Beginning of discussion"
                : request === "in_flight"
                  ? "Loading earlier..."
                  : null}
            </div>
          )}
          {items.map((item, index) => (
            <ItemView
              key={item.key}
              item={item}
              onOpen={onOpen}
              last={index === lastUser && pending.length === 0}
              now={now}
              toolTime={item.kind === "tool" && toolTimeVisible(items, index)}
              awaiting={item.kind === "tool" && awaiting.has(item.toolUseId)}
              thinkingNow={item.kind === "thinking" && turnOpen && index === items.length - 1}
            />
          ))}
          {pending.map((send) => (
            <PendingView key={send.id} pending={send} onRetry={onRetry} onDismiss={onDismiss} />
          ))}
          {pending.length === 0 && quietWork(items, turnOpen) && (
            <div
              aria-label="Claude is working"
              className="text-muted-foreground voice-ui px-1 text-sm leading-none"
            >
              <Dots />
            </div>
          )}
          {background.length > 0 && (
            <BackgroundRows tasks={background} waiting={!turnOpen} onStop={onStopTask} />
          )}
          {end}
        </div>
      </div>
      <div className="pointer-events-none absolute inset-x-0 bottom-3">
        <div className="mx-auto flex max-w-5xl flex-col items-end gap-2 px-3">
          {showJump && (
            <button
              type="button"
              onClick={jump}
              className="bg-primary text-primary-foreground voice-ui pointer-events-auto rounded-[5px] px-3 py-1.5 text-xs shadow-md"
            >
              latest &darr;
            </button>
          )}
          {overlay}
        </div>
      </div>
      {panel}
    </div>
  );
}
