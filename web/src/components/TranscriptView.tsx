import {
  type ReactNode,
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";

import type { Item, ToolStatus } from "@/transcript";

/** Distance from the bottom, in px, still counted as "at the bottom". */
const BOTTOM_SLACK = 24;

export interface PendingSend {
  id: string;
  text: string;
  /** User bubbles with this text already in the transcript when it was sent. */
  baseline: number;
  status: "sending" | "sent" | "failed";
  error: string | null;
}

const BUBBLE = "rounded-[5px] px-3 py-2 whitespace-pre-wrap [overflow-wrap:anywhere]";

function UserBubble({ children, dim }: { children: ReactNode; dim?: boolean }) {
  return (
    <div className={`bg-bubble-user ml-10 ${BUBBLE} ${dim === true ? "opacity-60" : ""}`}>
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

function ToolRow({ item }: { item: Extract<Item, { kind: "tool" }> }) {
  return (
    <details className="group text-sm">
      <summary className="hover:bg-muted flex cursor-pointer list-none items-baseline gap-2 rounded-[5px] px-1 py-0.5">
        <span className={`voice-code w-7 shrink-0 text-xs ${STATUS_CLASS[item.status]}`}>
          {STATUS_TEXT[item.status]}
        </span>
        <span className="voice-ui shrink-0">{item.name}</span>
        <span className="voice-code text-muted-foreground min-w-0 truncate">{item.summary}</span>
      </summary>
      <div className="mt-1 ml-9 space-y-2">
        {item.output !== null && (
          <pre
            className={`bg-muted max-h-80 overflow-auto rounded-[5px] p-2 text-xs whitespace-pre-wrap [overflow-wrap:anywhere] ${item.status === "error" ? "text-destructive" : ""}`}
          >
            {item.output === "" ? "(no output)" : item.output}
          </pre>
        )}
        <details>
          <summary className="text-muted-foreground voice-ui cursor-pointer text-xs">input</summary>
          <pre className="bg-muted mt-1 max-h-80 overflow-auto rounded-[5px] p-2 text-xs whitespace-pre-wrap [overflow-wrap:anywhere]">
            {JSON.stringify(item.input, null, 2)}
          </pre>
        </details>
      </div>
    </details>
  );
}

// Items keep their identity until they change, so unchanged rows skip rendering.
const ItemView = memo(function ItemView({ item }: { item: Item }) {
  switch (item.kind) {
    case "user":
      return <UserBubble>{item.text}</UserBubble>;
    case "claude":
      return (
        <div className={`bg-bubble-claude voice-prose mr-10 ${BUBBLE}`}>
          {item.text}
          {item.streaming && <span className="text-primary animate-pulse"> _</span>}
        </div>
      );
    case "thinking":
      return item.text === "" ? (
        <div className="text-muted-foreground voice-ui px-1 text-xs">thinking</div>
      ) : (
        <details className="text-muted-foreground px-1 text-xs">
          <summary className="voice-ui cursor-pointer">thinking</summary>
          <div className="mt-1 whitespace-pre-wrap [overflow-wrap:anywhere]">{item.text}</div>
        </details>
      );
    case "tool":
      return <ToolRow item={item} />;
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
    <div className="space-y-1">
      <UserBubble dim>{pending.text}</UserBubble>
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
            {pending.status === "sending" ? "sending..." : "sent, waiting for Claude"}
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
  items,
  pending,
  onRetry,
  onDismiss,
}: {
  items: readonly Item[];
  pending: readonly PendingSend[];
  onRetry: (pending: PendingSend) => void;
  onDismiss: (pending: PendingSend) => void;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const atBottom = useRef(true);
  const [showJump, setShowJump] = useState(false);

  const stick = useCallback(() => {
    const element = scroller.current;
    if (element !== null && atBottom.current) element.scrollTop = element.scrollHeight;
  }, []);

  const onScroll = () => {
    const element = scroller.current;
    if (element === null) return;
    const bottom =
      element.scrollHeight - element.scrollTop - element.clientHeight <= BOTTOM_SLACK;
    atBottom.current = bottom;
    setShowJump(!bottom);
  };

  useLayoutEffect(stick, [items, pending, stick]);

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
        <div ref={content} className="mx-auto max-w-5xl space-y-2 px-3 py-4">
          {items.length === 0 && pending.length === 0 && (
            <p className="text-muted-foreground voice-ui text-center text-sm">No messages yet.</p>
          )}
          {items.map((item) => (
            <ItemView key={item.key} item={item} />
          ))}
          {pending.map((send) => (
            <PendingView key={send.id} pending={send} onRetry={onRetry} onDismiss={onDismiss} />
          ))}
        </div>
      </div>
      {showJump && (
        <button
          type="button"
          onClick={jump}
          className="bg-primary text-primary-foreground voice-ui absolute right-4 bottom-3 rounded-[5px] px-3 py-1.5 text-xs shadow-md"
        >
          latest &darr;
        </button>
      )}
    </div>
  );
}
