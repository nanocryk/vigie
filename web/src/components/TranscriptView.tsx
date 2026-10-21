import {
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
  createContext,
  memo,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { DiffBlock } from "@/components/Diff";
import { Counts } from "@/components/DiffView";
import { Markdown } from "@/components/Markdown";
import { useClock, useNow } from "@/components/useNow";
import { DIFF_TOOLS, type FileDiff, changeSummary, toolDiff } from "@/diff";
import { formatDuration, formatExact, formatStamp } from "@/format";
import { AttachmentChip, LatestHashes } from "@/components/Attachments";
import { changeExplorer, openFileInLastFree } from "@/files";
import { formatSpan } from "@/gauges";
import { navigate } from "@/route";
import type { BackgroundView, HeldMessage, HeldMode } from "@/wire";
import type { Beginning, Request } from "@/history";
import {
  type Delivery,
  type Item,
  type TaskEndStatus,
  type ToolStatus,
  latestHashes,
  planFileName,
  quietWork,
  relayFailureText,
  restartLabel,
  toolDisplayName,
  toolTimeVisible,
  turnEndBubble,
  wakeStops,
} from "@/transcript";
import { onNextInput } from "@/presence";

/** A bubble drawing the eye: breathing until the next input, then fading out. */
type Glow = "on" | "fading";

const GLOW_CLASS: Record<Glow, string> = {
  on: "message-glow",
  fading: "message-glow-out",
};
/** Input right after the glow starts (the tap that asked for it) does not end it. */
const GLOW_GRACE_MS = 500;
/** Matches the `message-glow-out` animation in index.css. */
const GLOW_FADE_MS = 1000;

/** Distance from the bottom, in px, still counted as "at the bottom". */
const BOTTOM_SLACK = 24;
/** Earlier history is asked for once the view is within this many screen heights of the top. */
const NEAR_TOP_SCREENS = 1;

export interface PendingSend {
  id: string;
  text: string;
  /** Paths of the attached files. */
  attachments: readonly string[];
  /** One per attempt; the server's `user_sent` carrying any of them replaces this entry. */
  clientIds: readonly string[];
  status: "sending" | "sent" | "failed";
  error: string | null;
}

const BUBBLE = "min-w-0 rounded-[5px] px-3 py-2 text-sm";

/** Stops a task: the ids being stopped and the latest failure. */
function useStopTask(onStop: (taskId: string) => Promise<void>) {
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
  return { stopping, error, stop };
}

const STOP_BUTTON =
  "border-destructive/50 text-destructive hover:bg-destructive/10 voice-ui shrink-0 rounded-[5px] border text-xs disabled:opacity-50";

/** Stops one tool call run as a task; no button without a provider. */
const StopToolTask = createContext<((taskId: string) => Promise<void>) | null>(null);

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
  const { stopping, error, stop } = useStopTask(onStop);
  // Full-width bars, like the restart divider.
  return (
    <div className="-mx-3 space-y-px pt-2">
      {tasks.map((task) => {
        const started = Date.parse(task.started_at);
        return (
          <div
            key={task.task_id}
            className="flex items-center gap-2 bg-muted px-3 py-1.5 text-xs voice-ui text-foreground"
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
              className={`${STOP_BUTTON} px-2 py-0.5`}
            >
              {stopping.has(task.task_id) ? "Stopping..." : "Stop"}
            </button>
          </div>
        );
      })}
      {error !== null && <p className="px-3 text-xs voice-ui text-destructive">{error}</p>}
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

function UserBubble({
  children,
  dim,
  held = false,
  attachments,
  glow = null,
}: {
  children: ReactNode;
  dim?: boolean;
  /** Kept by vigie until sent: a dashed frame instead of the side bar. */
  held?: boolean;
  attachments?: readonly string[];
  glow?: Glow | null;
}) {
  const frame = held ? "border-2 border-dashed" : "border-r-2 rounded-r-none";
  return (
    <div
      className={`ml-16 border-ring bg-bubble-user ${BUBBLE} ${frame} ${dim === true ? "opacity-60" : ""} ${filesRoom(attachments)} ${glow === null ? "" : GLOW_CLASS[glow]}`}
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
    <span className={`w-7 shrink-0 text-xs voice-code ${STATUS_CLASS[status]}`}>
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
      <summary className="cursor-pointer text-xs voice-ui text-muted-foreground">input</summary>
      <pre className={`${PANEL} mt-1`}>{JSON.stringify(item.input, null, 2)}</pre>
    </details>
  );
}

/** Stop button of a running tool call; the turn goes on with the call failed. */
function ToolStop({ taskId, alone }: { taskId: string; alone: boolean }) {
  const onStop = useContext(StopToolTask);
  const { stopping, error, stop } = useStopTask(onStop ?? (() => Promise.resolve()));
  if (onStop === null) return null;
  return (
    <span
      className={`flex shrink-0 items-center gap-1.5 self-center ${alone ? "ml-auto pl-2" : ""}`}
    >
      {error !== null && <span className="text-xs voice-ui text-destructive">{error}</span>}
      <button
        type="button"
        disabled={stopping.has(taskId)}
        onClick={(event) => {
          // Inside the summary: a click would toggle the details too.
          event.preventDefault();
          event.stopPropagation();
          stop(taskId);
        }}
        className={`${STOP_BUTTON} px-1.5 leading-4`}
      >
        {stopping.has(taskId) ? "Stopping..." : "Stop"}
      </button>
    </span>
  );
}

function ToolRow({
  item,
  stamp,
  awaiting,
  taskId,
}: {
  item: ToolItem;
  /** Shown at the row's end; null hides it. */
  stamp: string | null;
  awaiting: boolean;
  /** The task running the call, while it runs; null when it is not one. */
  taskId: string | null;
}) {
  const plan = DIFF_TOOLS.has(item.name) ? planFileName(item.input) : null;
  const diff = useMemo(
    () =>
      DIFF_TOOLS.has(item.name) && plan === null ? toolDiff({ ...item, path: item.summary }) : null,
    [item, plan],
  );
  if (diff !== null && awaiting) {
    return (
      <div className="border-l-2 border-ring text-sm">
        <div className={TOOL_SUMMARY}>
          <StatusMark status={item.status} />
          <span className="min-w-0 truncate voice-code">
            <span className="voice-ui">{diff.verb}</span>({diff.path})
          </span>
          <span className="ml-auto shrink-0 pl-2 text-xs voice-ui text-muted-foreground">
            awaiting approval
          </span>
        </div>
      </div>
    );
  }
  if (diff !== null) return <DiffRow item={item} diff={diff} stamp={stamp} />;
  const timing = toolTiming(item);
  const planApproval = item.name === "ExitPlanMode";
  return (
    <div
      className={`rounded-r-[5px] border-l-2 text-sm ${planApproval ? "border-mode-plan bg-mode-plan/15" : "border-ring"}`}
    >
      <details className="group rounded-r-[5px] open:bg-muted open:pb-2">
        <summary className={TOOL_SUMMARY}>
          <StatusMark status={item.status} />
          <span className="shrink-0 voice-ui">
            {plan === null
              ? toolDisplayName(item.name)
              : item.name === "Write"
                ? "Plan written"
                : "Plan edited"}
          </span>
          <span className="min-w-0 truncate voice-code text-muted-foreground">
            {plan ?? item.summary}
          </span>
          {stamp !== null && <span className={`${STAMP} ml-auto shrink-0 pl-2`}>{stamp}</span>}
          {taskId !== null && <ToolStop taskId={taskId} alone={stamp === null} />}
        </summary>
        <div className="mt-1 mr-2 ml-9 space-y-2">
          {timing !== null && <div className={STAMP}>{timing}</div>}
          <ToolOutput item={item} />
          {item.attached !== null && (
            <details>
              <summary className="cursor-pointer text-xs voice-ui text-muted-foreground">
                skill content
              </summary>
              <pre className={`${PANEL} mt-1`}>{item.attached}</pre>
            </details>
          )}
          <ToolInput item={item} />
        </div>
      </details>
      <ApprovalNote item={item} />
    </div>
  );
}

/** The note sent with a tool call's approval, under its row. */
function ApprovalNote({ item }: { item: ToolItem }) {
  if (item.note === undefined) return null;
  return (
    <div className="mr-2 ml-9 pb-1 text-sm voice-prose">
      <span className="text-xs voice-ui text-muted-foreground">Note</span>
      <Markdown text={item.note} />
    </div>
  );
}

/** The message that woke Claude after background tasks were stopped, shown as a tool row. */
function WakeRow({ stops, pending }: { stops: readonly string[]; pending: boolean }) {
  return (
    <div className={`${TOOL_SUMMARY} cursor-default border-l-2 border-ring text-sm`}>
      <span className="w-7 shrink-0 text-xs voice-code text-primary">
        {pending ? <Dots /> : "↻"}
      </span>
      <span className="shrink-0 voice-ui">Wake</span>
      <span className="min-w-0 truncate voice-code text-muted-foreground">
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
    <details className="group rounded-r-[5px] border-l-2 border-ring text-sm open:bg-muted open:pb-2">
      <summary className={TOOL_SUMMARY}>
        <span className={`w-7 shrink-0 text-xs voice-code ${mark.className}`}>{mark.text}</span>
        <span className="shrink-0 voice-ui">Background</span>
        <span className="min-w-0 truncate voice-code text-muted-foreground">{item.summary}</span>
      </summary>
      <div className="mt-1 mr-2 ml-9 space-y-2">
        {outputFile !== null && (
          <button
            type="button"
            onClick={() => {
              changeExplorer((state) => openFileInLastFree(state, outputFile));
              navigate({ kind: "files" });
            }}
            className="rounded-[5px] border px-2 py-0.5 text-xs voice-ui text-primary"
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
    <div className="rounded-r-[5px] border-l-2 border-ring text-sm">
      <details open={open} onToggle={(event) => setOpen(event.currentTarget.open)}>
        <summary className={TOOL_SUMMARY}>
          <StatusMark status={item.status} />
          <span className="min-w-0 truncate voice-code">
            <span className="voice-ui">{diff.verb}</span>({diff.path})
          </span>
          {stamp !== null && <span className={`${STAMP} ml-auto shrink-0 pl-2`}>{stamp}</span>}
        </summary>
        <div className="mt-0.5 mr-2 ml-2 space-y-1 pb-1">
          <div className="flex items-baseline gap-2 pl-7">
            <span className="text-xs voice-ui text-muted-foreground">
              &#x2514; {changeSummary(diff.added, diff.removed)}
            </span>
            {timing !== null && <span className={`${STAMP} ml-auto`}>{timing}</span>}
          </div>
          <DiffBlock diff={diff} />
          <details className="space-y-2">
            <summary className="cursor-pointer text-xs voice-ui text-muted-foreground">raw</summary>
            <div className="mt-1 space-y-2">
              <ToolOutput item={item} />
              <ToolInput item={item} />
            </div>
          </details>
        </div>
      </details>
      <ApprovalNote item={item} />
    </div>
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
  tools = "shown",
  attachments = [],
  hashes,
  side,
}: {
  text: string;
  streaming?: boolean;
  time: number | null;
  now: number;
  /** The raw toggle and copy button. */
  tools?: "shown" | "hidden";
  attachments?: readonly string[];
  hashes?: Readonly<Record<string, string>>;
  /** The edge the attachments line up on. */
  side: "user" | "claude";
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
        <pre className="text-sm voice-code [overflow-wrap:anywhere] whitespace-pre-wrap">
          {text}
        </pre>
      ) : (
        <div ref={rendered}>
          <Markdown text={text} streaming={streaming} />
        </div>
      )}
      <div className={`${STAMP} mt-1.5 flex items-baseline justify-between gap-2`}>
        {tools === "shown" && (
          <div className="flex gap-3">
            <button
              type="button"
              aria-pressed={raw}
              onClick={() => setRaw((on) => !on)}
              className="cursor-pointer hover:text-foreground"
            >
              {raw ? "[see rendered]" : "[see raw]"}
            </button>
            <button
              type="button"
              onClick={copyShown}
              className="cursor-pointer hover:text-foreground"
            >
              {copy === "copied" ? "[copied]" : copy === "failed" ? "[copy failed]" : "[copy]"}
            </button>
          </div>
        )}
        {time !== null && <span className="ml-auto">{formatStamp(time, now)}</span>}
      </div>
      <BubbleFiles paths={attachments} hashes={hashes} side={side} />
    </>
  );
}

const OpenAttachment = createContext<(paths: readonly string[], index: number) => void>(() => {});

/** Opens the session's diff view; null outside a repository. */
const OpenDiff = createContext<(() => void) | null>(null);

/** The files a turn changed, opening the diff view when the folder is a repository. */
function ChangesRow({ item }: { item: Extract<Item, { kind: "changes" }> }) {
  const openDiff = useContext(OpenDiff);
  const files = item.files === 1 ? "1 file" : `${item.files} files`;
  const body = (
    <>
      <span className="w-7 shrink-0 text-xs voice-code text-primary">±</span>
      <span className="shrink-0 voice-ui">Changed {files}</span>
      <Counts file={item} />
    </>
  );
  return openDiff === null ? (
    <div className={`${TOOL_SUMMARY} cursor-default border-l-2 border-ring text-sm`}>{body}</div>
  ) : (
    <button
      type="button"
      onClick={openDiff}
      className={`${TOOL_SUMMARY} w-full border-l-2 border-ring text-left text-sm`}
    >
      {body}
    </button>
  );
}

/**
 * A bubble's files, as a row of chips hanging over its bottom edge. The bubble
 * leaves `BUBBLE_FILES_ROOM` below it for the overhang.
 */
function BubbleFiles({
  paths,
  hashes,
  side,
}: {
  paths: readonly string[];
  /** Content hash of each path when attached; absent for uploads. */
  hashes?: Readonly<Record<string, string>>;
  side: "user" | "claude";
}) {
  const openAttachment = useContext(OpenAttachment);
  if (paths.length === 0) return null;
  return (
    <div
      className={`mt-1.5 -mb-[1.875rem] flex flex-wrap gap-2 ${side === "user" ? "justify-end" : "justify-start"}`}
    >
      {paths.map((path, index) => (
        <AttachmentChip
          key={`${index}:${path}`}
          path={path}
          hash={hashes?.[path]}
          onOpen={() => openAttachment(paths, index)}
        />
      ))}
    </div>
  );
}

/** Room below a bubble for the half of its file chips hanging out, and a gap. */
const BUBBLE_FILES_ROOM = "mb-8";

function filesRoom(paths: readonly string[] | undefined): string {
  return paths !== undefined && paths.length > 0 ? BUBBLE_FILES_ROOM : "";
}

const COPY_FEEDBACK_MS = 1500;

// Items keep their identity until they change, so unchanged rows skip rendering.
const ItemView = memo(function ItemView({
  item,
  last,
  now,
  toolTime,
  awaiting,
  taskId,
  thinkingNow,
  glow,
  onOpen,
}: {
  item: Item;
  /** A running tool row's task, stoppable from the row. */
  taskId: string | null;
  /** A user or Claude bubble drawing the eye. */
  glow: Glow | null;
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
      // A message still on the host says so wherever it sits: Claude takes it after its running tool call.
      const label = last || item.delivery === "on_host" ? DELIVERY_LABEL[item.delivery] : null;
      // The room above opens each exchange; the first one sits at the top.
      return (
        <div className="space-y-1 pt-8 first:pt-0" data-item={item.key}>
          <UserBubble attachments={item.attachments} glow={glow}>
            <BubbleBody
              text={item.text}
              time={item.time}
              now={now}
              tools="hidden"
              attachments={item.attachments}
              side="user"
            />
          </UserBubble>
          {item.relayed === true && (
            <div className="px-1 text-right text-xs voice-ui text-muted-foreground">
              via main agent
            </div>
          )}
          {label !== null && (
            <div
              className={`px-1 text-xs voice-ui text-muted-foreground ${item.delivery === "handled" ? "text-left" : "text-right"}`}
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
          className={`mr-16 border-l-2 border-ring bg-bubble-claude voice-prose ${BUBBLE} rounded-l-none ${filesRoom(item.attachments)} ${glow === null ? "" : GLOW_CLASS[glow]}`}
        >
          <BubbleBody
            text={item.text}
            streaming={item.streaming}
            time={item.time}
            now={now}
            attachments={item.attachments}
            hashes={item.attachmentHashes}
            side="claude"
          />
        </div>
      );
    case "thinking":
      if (item.text === "") {
        return thinkingNow ? (
          <div className="px-1 text-xs voice-ui text-muted-foreground">
            thinking
            <Dots />
          </div>
        ) : null;
      }
      return (
        <details className="px-1 text-xs text-muted-foreground">
          <summary className="cursor-pointer voice-ui">
            {thinkingNow ? (
              <>
                thinking
                <Dots />
              </>
            ) : (
              "thought"
            )}
          </summary>
          <div className="mt-1 [overflow-wrap:anywhere] whitespace-pre-wrap">{item.text}</div>
        </details>
      );
    case "tool":
      return (
        <ToolRow
          item={item}
          stamp={toolTime && item.time !== null ? formatStamp(item.time, now) : null}
          awaiting={awaiting}
          taskId={taskId}
        />
      );
    case "notice": {
      const tone = item.tone === "error" ? "text-destructive" : "text-muted-foreground";
      return item.detail === null ? (
        <div className={`px-1 text-center text-xs voice-ui ${tone}`}>{item.text}</div>
      ) : (
        <details className={`px-1 text-xs ${tone}`}>
          <summary className="cursor-pointer text-center voice-ui">{item.text}</summary>
          <pre className="mt-1 max-h-60 overflow-auto rounded-[5px] bg-muted p-2 [overflow-wrap:anywhere] whitespace-pre-wrap">
            {item.detail}
          </pre>
        </details>
      );
    }
    case "agent":
      return (
        <div className={`${TOOL_SUMMARY} cursor-default border-l-2 border-ring text-sm`}>
          <span className="w-7 shrink-0 text-xs voice-code text-primary">&#x2192;</span>
          <span className="min-w-0 truncate voice-ui">Agent created: {item.name}</span>
          <button
            type="button"
            onClick={() => onOpen(item.toolUseId)}
            className="ml-auto shrink-0 rounded-[5px] border px-2 text-xs voice-ui text-primary"
          >
            Open
          </button>
        </div>
      );
    case "task_end":
      return <TaskEndRow item={item} />;
    case "changes":
      return <ChangesRow item={item} />;
    case "brief":
      return (
        <details className="group rounded-r-[5px] border-l-2 border-ring text-sm open:bg-muted open:pb-2">
          <summary className={TOOL_SUMMARY}>
            <span className="w-7 shrink-0 text-xs voice-code text-muted-foreground">&#x2190;</span>
            <span className="shrink-0 voice-ui">Brief</span>
            <span className="min-w-0 truncate voice-code text-muted-foreground">{item.text}</span>
          </summary>
          <pre className={`${PANEL} mt-1 mr-2 ml-9`}>{item.text}</pre>
        </details>
      );
    case "relay_failed":
      return (
        <div className="flex flex-wrap items-baseline justify-center gap-2 px-1 text-xs voice-ui text-destructive">
          <span>{relayFailureText(item.reason)}</span>
          <button
            type="button"
            onClick={() => onOpen(null)}
            className="rounded-[5px] border border-destructive px-2"
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
      <UserBubble dim attachments={pending.attachments}>
        <Markdown text={pending.text} />
        <BubbleFiles paths={pending.attachments} side="user" />
      </UserBubble>
      <div className="flex justify-end gap-3 text-xs voice-ui">
        {pending.status === "failed" ? (
          <>
            <span className="text-destructive">{pending.error ?? "Not sent"}</span>
            <button
              type="button"
              className="text-primary underline"
              onClick={() => onRetry(pending)}
            >
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

/** What can be done to a held message from its bubble. */
export interface HeldActions {
  onSendNow: (held: HeldMessage) => void;
  onEdit: (held: HeldMessage) => void;
  onMode: (held: HeldMessage, mode: HeldMode) => void;
  onDrop: (held: HeldMessage) => void;
  /** Id of the message open in the composer, null when none. */
  editing: string | null;
}

const HELD_TAG: Record<HeldMode, string> = { held: "held", turn_end: "at turn end" };

const HELD_ACTION = "voice-ui rounded-[5px] border px-2.5 py-1.5 text-xs";

/** A message vigie keeps until sent; tapping it opens its actions. */
function HeldView({
  held,
  open,
  onToggle,
  actions,
}: {
  held: HeldMessage;
  open: boolean;
  onToggle: () => void;
  actions: HeldActions;
}) {
  const editing = actions.editing === held.id;
  const other: HeldMode = held.mode === "held" ? "turn_end" : "held";
  return (
    <div className="space-y-1 pt-8 first:pt-0">
      <div
        role="button"
        tabIndex={0}
        aria-expanded={open}
        onClick={(event) => {
          // Attachment chips keep their own tap.
          if (event.target instanceof Element && event.target.closest("button, a") !== null) return;
          onToggle();
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            onToggle();
          }
        }}
        className="cursor-pointer"
      >
        <UserBubble dim held attachments={held.attachments}>
          <Markdown text={held.text} />
          <BubbleFiles paths={held.attachments} side="user" />
        </UserBubble>
      </div>
      <div className="px-1 text-right text-xs voice-ui text-muted-foreground">
        {editing ? "editing in the composer" : HELD_TAG[held.mode]}
      </div>
      {open && !editing && (
        <div className="flex flex-wrap justify-end gap-1.5">
          <button
            type="button"
            onClick={() => actions.onSendNow(held)}
            className={`${HELD_ACTION} border-primary bg-primary text-primary-foreground`}
          >
            Send now
          </button>
          <button
            type="button"
            onClick={() => actions.onEdit(held)}
            className={`${HELD_ACTION} bg-card hover:bg-muted`}
          >
            Edit
          </button>
          <button
            type="button"
            onClick={() => actions.onMode(held, other)}
            className={`${HELD_ACTION} bg-card hover:bg-muted`}
          >
            {other === "held" ? "Hold" : "At turn end"}
          </button>
          <button
            type="button"
            onClick={() => actions.onDrop(held)}
            className={`${HELD_ACTION} border-destructive bg-card text-destructive hover:bg-muted`}
          >
            Delete
          </button>
        </div>
      )}
    </div>
  );
}

function HeldList({ held, actions }: { held: readonly HeldMessage[]; actions: HeldActions }) {
  const [open, setOpen] = useState<string | null>(null);
  return (
    <>
      {held.map((message) => (
        <HeldView
          key={message.id}
          held={message}
          open={open === message.id}
          onToggle={() => setOpen((current) => (current === message.id ? null : message.id))}
          actions={actions}
        />
      ))}
    </>
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
  onAtBottom,
  items: allItems,
  pending,
  onRetry,
  onDismiss,
  awaiting,
  turnOpen,
  onOpen,
  onOpenAttachment,
  onOpenDiff,
  glowTurnEnd,
  background,
  onStopTask,
  toolTasks,
  onStopTool,
  bottomRequest,
  held,
  heldActions,
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
  /** The content changed, or the view returned to the bottom, while at the bottom. */
  onAtBottom?: () => void;
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
  /** Opens the file at `index` of a bubble's attachment chips. */
  onOpenAttachment: (paths: readonly string[], index: number) => void;
  /** Opens the session's diff view; null outside a repository. */
  onOpenDiff: (() => void) | null;
  /** A turn ending while shown glows its closing Claude bubble until the next input. */
  glowTurnEnd: boolean;
  /** Background tasks, listed at the end with a Stop button each. */
  background: readonly BackgroundView[];
  onStopTask: (taskId: string) => Promise<void>;
  /** Task id of each tool call run as a task, by tool use id: running ones get a Stop button. */
  toolTasks: ReadonlyMap<string, string>;
  /** Stops a running tool call's task; kept stable. */
  onStopTool: (taskId: string) => Promise<void>;
  /** Each change scrolls the view to its bottom. */
  bottomRequest: number;
  /** Messages kept until sent, pinned under everything else but `end`. */
  held: readonly HeldMessage[];
  heldActions: HeldActions;
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
  const [jumpButton, setJumpButton] = useState<"latest" | "last_message" | null>(null);
  const now = useNow();
  const items = useMemo(() => allItems.filter((item) => item.hidden !== true), [allItems]);
  const attachedHashes = useMemo(() => latestHashes(allItems), [allItems]);
  const lastMessageKey = useMemo(() => {
    for (let index = items.length - 1; index >= 0; index -= 1) {
      const item = items[index];
      if (item?.kind === "user" && wakeStops(item.text) === null) return item.key;
    }
    return null;
  }, [items]);
  const lastMessage = useRef(lastMessageKey);
  const [glow, setGlow] = useState<{ key: string; phase: Glow } | null>(null);

  // Breathing until the next input past a grace, then fading out.
  useEffect(() => {
    if (glow === null) return;
    if (glow.phase === "fading") {
      const timer = setTimeout(() => setGlow(null), GLOW_FADE_MS);
      return () => clearTimeout(timer);
    }
    let stopWaiting = () => {};
    const grace = setTimeout(() => {
      stopWaiting = onNextInput(() => setGlow({ key: glow.key, phase: "fading" }));
    }, GLOW_GRACE_MS);
    return () => {
      clearTimeout(grace);
      stopWaiting();
    };
  }, [glow]);

  // A turn ending live glows its closing Claude bubble.
  const wasTurnOpen = useRef(turnOpen);
  useEffect(() => {
    const ended = wasTurnOpen.current && !turnOpen;
    wasTurnOpen.current = turnOpen;
    if (!ended || !glowTurnEnd) return;
    const key = turnEndBubble(items);
    if (key !== null) setGlow({ key, phase: "on" });
  }, [turnOpen, glowTurnEnd, items]);

  useLayoutEffect(() => {
    nearTop.current = onNearTop;
  }, [onNearTop]);

  /** The bubble of the user's last message, when rendered. */
  const lastMessageElement = useCallback((): HTMLElement | null => {
    const key = lastMessage.current;
    if (key === null || content.current === null) return null;
    return content.current.querySelector<HTMLElement>(`[data-item="${CSS.escape(key)}"]`);
  }, []);

  /** Scrolled up: back to the bottom. At the bottom: to the last message when it is off screen. */
  const updateJump = useCallback(() => {
    const element = scroller.current;
    if (element === null) return;
    if (!atBottom.current) {
      setJumpButton("latest");
      return;
    }
    const message = lastMessageElement();
    const offScreen =
      message !== null &&
      message.getBoundingClientRect().bottom < element.getBoundingClientRect().top;
    setJumpButton(offScreen ? "last_message" : null);
  }, [lastMessageElement]);

  useLayoutEffect(() => {
    lastMessage.current = lastMessageKey;
    updateJump();
  }, [lastMessageKey, updateJump]);

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
    const bottom = element.scrollHeight - element.scrollTop - element.clientHeight <= BOTTOM_SLACK;
    atBottom.current = bottom;
    fromBottom.current = element.scrollHeight - element.scrollTop;
    updateJump();
    checkTop();
  }, [checkTop, updateJump]);

  // The jump button's room is bottom padding: re-stick once it changes.
  useLayoutEffect(stick, [jumpButton, stick]);

  // Opening or closing a panel never moves the view, even at the bottom: the
  // following content is pushed down instead. Position is re-read once it settles.
  const onSummaryClick = (event: ReactMouseEvent) => {
    // A button inside a summary does not toggle it.
    if (
      event.target instanceof Element &&
      event.target.closest("summary") !== null &&
      event.target.closest("button") === null
    ) {
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
  }, [items, pending, held, firstSeq, stick]);

  // A short window, or a page leaving the view near the top, asks for more.
  useEffect(checkTop, [items, request, checkTop]);

  useEffect(() => {
    if (atBottom.current) onAtBottom?.();
  }, [items, jumpButton, onAtBottom]);

  let lastUser = items.length - 1;
  while (lastUser >= 0 && items[lastUser]?.kind !== "user") lastUser -= 1;

  useEffect(() => {
    const observer = new ResizeObserver(stick);
    if (scroller.current !== null) observer.observe(scroller.current);
    // Border box: the room for the jump button is padding, and must re-stick too.
    if (content.current !== null) observer.observe(content.current, { box: "border-box" });
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
    stick();
    updateJump();
  }, [stick, updateJump]);

  const jumpToLastMessage = useCallback(() => {
    const element = scroller.current;
    const message = lastMessageElement();
    if (element === null || message === null) return;
    atBottom.current = false;
    element.scrollTop += message.getBoundingClientRect().top - element.getBoundingClientRect().top;
    const key = lastMessage.current;
    if (key !== null) setGlow({ key, phase: "on" });
  }, [lastMessageElement]);

  const lastBottomRequest = useRef(bottomRequest);
  useEffect(() => {
    if (lastBottomRequest.current === bottomRequest) return;
    lastBottomRequest.current = bottomRequest;
    jump();
  }, [bottomRequest, jump]);

  return (
    <div className="relative min-h-0 flex-1">
      <div ref={scroller} onScroll={onScroll} className="h-full overflow-y-auto overscroll-contain">
        <div
          ref={content}
          onClickCapture={onSummaryClick}
          // Room under the last row for the jump button floating over it.
          className={`mx-auto max-w-5xl space-y-2 px-3 pt-4 select-text ${jumpButton === null ? "pb-4" : "pb-12"}`}
        >
          {items.length === 0 && pending.length === 0 && held.length === 0 && (
            <p className="text-center text-sm voice-ui text-muted-foreground">
              {loading ? "Loading..." : "No messages yet."}
            </p>
          )}
          {items.length > 0 && (
            // One fixed height whatever it says, so it never shifts the view.
            <div className="flex h-6 items-center justify-center text-xs voice-ui text-muted-foreground">
              {beginning === "loaded"
                ? "Beginning of discussion"
                : request === "in_flight"
                  ? "Loading earlier..."
                  : null}
            </div>
          )}
          <OpenAttachment.Provider value={onOpenAttachment}>
            <LatestHashes.Provider value={attachedHashes}>
              <OpenDiff.Provider value={onOpenDiff}>
                <StopToolTask.Provider value={onStopTool}>
                  {items.map((item, index) => (
                    <ItemView
                      key={item.key}
                      item={item}
                      onOpen={onOpen}
                      last={index === lastUser && pending.length === 0}
                      now={now}
                      toolTime={item.kind === "tool" && toolTimeVisible(items, index)}
                      awaiting={item.kind === "tool" && awaiting.has(item.toolUseId)}
                      taskId={
                        item.kind === "tool" && item.status === "running"
                          ? (toolTasks.get(item.toolUseId) ?? null)
                          : null
                      }
                      thinkingNow={
                        item.kind === "thinking" && turnOpen && index === items.length - 1
                      }
                      glow={glow?.key === item.key ? glow.phase : null}
                    />
                  ))}
                  {pending.map((send) => (
                    <PendingView
                      key={send.id}
                      pending={send}
                      onRetry={onRetry}
                      onDismiss={onDismiss}
                    />
                  ))}
                </StopToolTask.Provider>
              </OpenDiff.Provider>
            </LatestHashes.Provider>
          </OpenAttachment.Provider>
          {pending.length === 0 && quietWork(items, turnOpen) && (
            <div
              aria-label="Claude is working"
              className="px-1 text-sm leading-none voice-ui text-muted-foreground"
            >
              <Dots />
            </div>
          )}
          {background.length > 0 && (
            <BackgroundRows tasks={background} waiting={!turnOpen} onStop={onStopTask} />
          )}
          <OpenAttachment.Provider value={onOpenAttachment}>
            <HeldList held={held} actions={heldActions} />
          </OpenAttachment.Provider>
          {end}
        </div>
      </div>
      <div className="pointer-events-none absolute inset-x-0 bottom-3">
        <div className="mx-auto flex max-w-5xl flex-col items-end gap-2 px-3">
          {jumpButton !== null && (
            <button
              type="button"
              onClick={jumpButton === "latest" ? jump : jumpToLastMessage}
              className="pointer-events-auto rounded-[5px] bg-primary px-3 py-1.5 text-xs voice-ui text-primary-foreground shadow-md"
            >
              {jumpButton === "latest" ? <>latest &darr;</> : <>&uarr; Last message</>}
            </button>
          )}
          {overlay}
        </div>
      </div>
      {panel}
    </div>
  );
}
