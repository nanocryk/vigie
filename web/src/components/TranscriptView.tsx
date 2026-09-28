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
import { useNow } from "@/components/useNow";
import { DIFF_TOOLS, type FileDiff, changeSummary, toolDiff } from "@/diff";
import { formatDuration, formatExact, formatStamp } from "@/format";
import {
  type Delivery,
  type Item,
  type ToolStatus,
  restartLabel,
  toolTimeVisible,
} from "@/transcript";

/** Distance from the bottom, in px, still counted as "at the bottom". */
const BOTTOM_SLACK = 24;

export interface PendingSend {
  id: string;
  text: string;
  /** One per attempt; the server's `user_sent` carrying any of them replaces this entry. */
  clientIds: readonly string[];
  status: "sending" | "sent" | "failed";
  error: string | null;
}

const BUBBLE = "min-w-0 rounded-[5px] px-3 py-2 text-sm";

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
  running: "text-primary animate-pulse",
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
      {STATUS_TEXT[status]}
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

function BubbleTime({ time, now }: { time: number | null; now: number }) {
  if (time === null) return null;
  return (
    <div className={`${STAMP} mt-1.5 text-right`}>
      {formatStamp(time, now)}
    </div>
  );
}

// Items keep their identity until they change, so unchanged rows skip rendering.
const ItemView = memo(function ItemView({
  item,
  last,
  now,
  toolTime,
  awaiting,
  thinkingNow,
}: {
  item: Item;
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
      const label = last ? DELIVERY_LABEL[item.delivery] : null;
      // The room above opens each exchange; the first one sits at the top.
      return (
        <div className="space-y-1 pt-8 first:pt-0">
          <UserBubble>
            <Markdown text={item.text} />
            <BubbleTime time={item.time} now={now} />
          </UserBubble>
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
          <Markdown text={item.text} streaming={item.streaming} />
          <BubbleTime time={item.time} now={now} />
        </div>
      );
    case "thinking":
      if (item.text === "") {
        return thinkingNow ? (
          <div className="text-muted-foreground voice-ui px-1 text-xs">
            thinking<span className="thinking-dots">...</span>
          </div>
        ) : null;
      }
      return (
        <details className="text-muted-foreground px-1 text-xs">
          <summary className="voice-ui cursor-pointer">
            {thinkingNow ? (
              <>
                thinking<span className="thinking-dots">...</span>
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
 * scrolled up, nothing moves it until "latest" is tapped.
 */
export function TranscriptView({
  loading,
  items,
  pending,
  onRetry,
  onDismiss,
  awaiting,
  turnOpen,
  overlay,
  panel,
}: {
  /** The history is still arriving; it shows at once when complete. */
  loading: boolean;
  items: readonly Item[];
  pending: readonly PendingSend[];
  onRetry: (pending: PendingSend) => void;
  onDismiss: (pending: PendingSend) => void;
  /** Tool use ids with an ask pending: their diff shows in the ask card, not here. */
  awaiting: ReadonlySet<string>;
  /** A turn is running. */
  turnOpen: boolean;
  /** Floats at the bottom right, under the "latest" button, aligned with the composer. */
  overlay?: ReactNode;
  /** In front of the discussion, positioned by itself. */
  panel?: ReactNode;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const atBottom = useRef(true);
  const [showJump, setShowJump] = useState(false);
  const now = useNow();

  const stick = useCallback(() => {
    const element = scroller.current;
    if (element !== null && atBottom.current) element.scrollTop = element.scrollHeight;
  }, []);

  const onScroll = useCallback(() => {
    const element = scroller.current;
    if (element === null) return;
    const bottom =
      element.scrollHeight - element.scrollTop - element.clientHeight <= BOTTOM_SLACK;
    atBottom.current = bottom;
    setShowJump(!bottom);
  }, []);

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

  useLayoutEffect(stick, [items, pending, stick]);

  let lastUser = items.length - 1;
  while (lastUser >= 0 && items[lastUser]?.kind !== "user") lastUser -= 1;

  useEffect(() => {
    const observer = new ResizeObserver(stick);
    if (scroller.current !== null) observer.observe(scroller.current);
    if (content.current !== null) observer.observe(content.current);
    return () => observer.disconnect();
  }, [stick]);

  const jump = () => {
    atBottom.current = true;
    setShowJump(false);
    stick();
  };

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
          className="mx-auto max-w-5xl space-y-2 px-3 pt-4 pb-4"
        >
          {items.length === 0 && pending.length === 0 && (
            <p className="text-muted-foreground voice-ui text-center text-sm">
              {loading ? "Loading..." : "No messages yet."}
            </p>
          )}
          {items.map((item, index) => (
            <ItemView
              key={item.key}
              item={item}
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
