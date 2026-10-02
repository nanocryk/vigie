import {
  Fragment,
  type ReactNode,
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
} from "react";

import { type CacheDeadline, CacheLowBanner } from "@/components/ColdPanel";
import { DiffBlock } from "@/components/Diff";
import { Markdown } from "@/components/Markdown";
import { PlanView } from "@/components/PlanView";
import { Swipe } from "@/components/Swipe";
import { DIFF_TOOLS, changeSummary, toolDiff } from "@/diff";
import { type Segment, broadReason, offerSegments } from "@/grant";
import type { Side } from "@/swipe";
import {
  type PendingAsk,
  denyCountdown,
  relativePath,
  toolDisplayName,
  toolSummary,
} from "@/transcript";
import { type Answer, type Danger, type GrantKind, isRecord } from "@/wire";

/** Sends the answer to one ask; rejects with the server's refusal. */
export type SubmitAnswer = (requestId: string, answer: Answer) => Promise<void>;

const DEFAULT_DENY = "The user denied this request.";

interface Question {
  question: string;
  header: string | null;
  multiSelect: boolean;
  options: { label: string; description: string | null }[];
}

/** `AskUserQuestion` input, or null when its shape is not the expected one. */
export function parseQuestions(input: unknown): Question[] | null {
  if (!isRecord(input) || !Array.isArray(input["questions"])) return null;
  const questions: Question[] = [];
  for (const raw of input["questions"]) {
    if (!isRecord(raw) || typeof raw["question"] !== "string" || !Array.isArray(raw["options"])) {
      return null;
    }
    const options: Question["options"] = [];
    for (const option of raw["options"]) {
      if (!isRecord(option) || typeof option["label"] !== "string") return null;
      const description = option["description"];
      options.push({
        label: option["label"],
        description: typeof description === "string" ? description : null,
      });
    }
    const header = raw["header"];
    questions.push({
      question: raw["question"],
      header: typeof header === "string" ? header : null,
      multiSelect: raw["multiSelect"] === true,
      options,
    });
  }
  return questions;
}

/**
 * The answer being built for one question. Single choice: the free text is picked while no label is.
 * Multi choice: `custom` holds the added free-text entries and `draft` the one being typed.
 */
export interface Choice {
  labels: readonly string[];
  custom: readonly string[];
  draft: string;
}

export const NO_CHOICE: Choice = { labels: [], custom: [], draft: "" };

/** The answer sent for one question, entries joined by commas; empty when unanswered. */
export function choiceAnswer(choice: Choice, multiSelect: boolean): string {
  const draft = choice.draft.trim();
  if (!multiSelect) return choice.labels[0] ?? draft;
  const pending = draft === "" || choice.custom.includes(draft) ? [] : [draft];
  return [...choice.labels, ...choice.custom, ...pending].join(", ");
}

/** Moves the draft into the custom entries; blank or duplicate drafts are dropped. */
export function addDraft(choice: Choice): Choice {
  const draft = choice.draft.trim();
  const duplicate = choice.custom.includes(draft) || choice.labels.includes(draft);
  return {
    ...choice,
    custom: draft === "" || duplicate ? choice.custom : [...choice.custom, draft],
    draft: "",
  };
}

const BUTTON = "voice-ui rounded-[5px] px-4 py-2 text-sm disabled:opacity-50";

const OPTION = "block w-full rounded-[5px] border px-3 py-1.5 text-left text-sm";

const FIELD =
  "focus:ring-ring voice-ui rounded-[5px] border px-3 py-1.5 text-base outline-none focus:ring-2";

function optionLook(checked: boolean): string {
  return checked ? "border-primary bg-primary/10" : "bg-background";
}

function QuestionView({
  question,
  choice,
  onChange,
  disabled,
}: {
  question: Question;
  choice: Choice;
  onChange: (choice: Choice) => void;
  disabled: boolean;
}) {
  const selected = choice.labels;
  const toggle = (label: string) => {
    if (!question.multiSelect) onChange({ ...choice, labels: [label] });
    else if (selected.includes(label)) {
      onChange({ ...choice, labels: selected.filter((item) => item !== label) });
    } else onChange({ ...choice, labels: [...selected, label] });
  };
  const otherPicked = !question.multiSelect && selected.length === 0 && choice.draft.trim() !== "";
  const label = `Other answer to: ${question.question}`;
  return (
    <fieldset className="space-y-1.5">
      <legend className="mb-1 text-sm voice-prose">
        {question.header !== null && (
          <span className="mr-2 text-xs voice-ui text-muted-foreground">{question.header}</span>
        )}
        {question.question}
        {question.multiSelect && (
          <span className="ml-2 text-xs voice-ui text-muted-foreground">(several allowed)</span>
        )}
      </legend>
      {question.options.map((option) => {
        const checked = selected.includes(option.label);
        return (
          <button
            key={option.label}
            type="button"
            disabled={disabled}
            aria-pressed={checked}
            onClick={() => toggle(option.label)}
            className={`${OPTION} ${optionLook(checked)}`}
          >
            <span className="voice-ui">{option.label}</span>
            {option.description !== null && (
              <span className="block text-xs text-muted-foreground">{option.description}</span>
            )}
          </button>
        );
      })}
      {question.multiSelect ? (
        <>
          {choice.custom.map((entry) => (
            <div
              key={entry}
              className={`${OPTION} ${optionLook(true)} flex items-center justify-between gap-2`}
            >
              <span className="min-w-0 voice-ui [overflow-wrap:anywhere]">{entry}</span>
              <button
                type="button"
                disabled={disabled}
                aria-label={`Remove ${entry}`}
                onClick={() =>
                  onChange({ ...choice, custom: choice.custom.filter((item) => item !== entry) })
                }
                className="shrink-0 px-1 text-muted-foreground hover:text-foreground disabled:opacity-50"
              >
                ✕
              </button>
            </div>
          ))}
          <div className="flex gap-1.5">
            <input
              value={choice.draft}
              onChange={(event) => onChange({ ...choice, draft: event.target.value })}
              onKeyDown={(event) => {
                if (event.key !== "Enter") return;
                event.preventDefault();
                onChange(addDraft(choice));
              }}
              disabled={disabled}
              placeholder="Other"
              aria-label={label}
              className={`${FIELD} min-w-0 flex-1 bg-background`}
            />
            <button
              type="button"
              disabled={disabled || choice.draft.trim() === ""}
              aria-label="Add this answer"
              onClick={() => onChange(addDraft(choice))}
              className="rounded-[5px] border bg-background px-3 text-base voice-ui disabled:opacity-50"
            >
              +
            </button>
          </div>
        </>
      ) : (
        <input
          value={choice.draft}
          onChange={(event) => onChange({ ...choice, labels: [], draft: event.target.value })}
          onFocus={() => {
            if (choice.draft.trim() !== "") onChange({ ...choice, labels: [] });
          }}
          disabled={disabled}
          placeholder="Other"
          aria-label={label}
          className={`${FIELD} ${optionLook(otherPicked)} w-full`}
        />
      )}
    </fieldset>
  );
}

/** Asks showing the text Claude wrote before the call: the command's explanation. */
const CONTEXT_TOOLS: ReadonlySet<string> = new Set(["Bash"]);

function InputView({
  ask,
  cwd = null,
  context = null,
}: {
  ask: PendingAsk;
  cwd?: string | null;
  context?: string | null;
}) {
  const diff = useMemo(
    () =>
      ask.input === undefined || !DIFF_TOOLS.has(ask.toolName)
        ? null
        : toolDiff({
            name: ask.toolName,
            input: ask.input,
            result: undefined,
            status: "running",
            path: toolSummary(ask.toolName, ask.input, cwd),
          }),
    [ask, cwd],
  );
  if (ask.input === undefined) {
    return <p className="text-sm text-muted-foreground">Input not received.</p>;
  }
  return (
    <>
      {context !== null && CONTEXT_TOOLS.has(ask.toolName) && (
        <div className="max-h-[25dvh] min-w-0 overflow-y-auto rounded-[5px] rounded-l-none border-l-2 border-ring bg-bubble-claude px-3 py-2 text-sm voice-prose">
          <Markdown text={context} />
        </div>
      )}
      {diff === null ? (
        <div className="rounded-[5px] border bg-background p-2 text-sm voice-code [overflow-wrap:anywhere] whitespace-pre-wrap">
          {toolSummary(ask.toolName, ask.input, cwd)}
        </div>
      ) : (
        <div className="space-y-1">
          <div className="min-w-0 truncate text-sm voice-code">
            <span className="voice-ui">{diff.verb}</span>({diff.path})
          </div>
          <div className="text-xs voice-ui text-muted-foreground">
            &#x2514; {changeSummary(diff.added, diff.removed)}
          </div>
          <DiffBlock diff={diff} maxHeight="max-h-[40dvh]" wrap />
        </div>
      )}
      <details className="mt-1">
        <summary className="cursor-pointer text-xs voice-ui text-muted-foreground">
          full input
        </summary>
        <pre className="mt-1 max-h-60 overflow-auto rounded-[5px] bg-muted p-2 text-xs [overflow-wrap:anywhere] whitespace-pre-wrap">
          {JSON.stringify(ask.input, null, 2)}
        </pre>
      </details>
    </>
  );
}

function CardFrame({
  title,
  queued,
  scroll = "frame",
  holdsInput,
  children,
}: {
  /** Null when the children draw their own header. */
  title: string | null;
  queued: number;
  /** `content`: the frame never scrolls; a `min-h-0` child down its flex columns shrinks and scrolls instead. */
  scroll?: "frame" | "content";
  /** Something was picked or typed: a cold cache would put it behind the cold-cache choice. */
  holdsInput: boolean;
  children: ReactNode;
}) {
  const goTo = useContext(GoToAgent);
  const denyAt = useContext(DenyAt);
  const deadline = useContext(MainDeadline);
  return (
    <section
      className={`flex max-h-[50%] shrink-0 flex-col border-t-2 border-primary/40 bg-card px-3 py-3 ${scroll === "frame" ? "overflow-y-auto" : "overflow-hidden"}`}
    >
      {/* Shrinking only for `content`: in `frame` it would clip the bottom instead of scrolling. */}
      <div
        className={`mx-auto flex w-full max-w-5xl flex-col gap-3 ${scroll === "content" ? "min-h-0" : ""}`}
      >
        {deadline !== null && holdsInput && <CacheLowBanner deadline={deadline} subject="ask" />}
        {goTo !== null && (
          <button
            type="button"
            onClick={goTo.onGo}
            className="self-start rounded-[5px] border border-primary px-2 py-1 text-xs voice-ui text-primary hover:bg-muted"
          >
            Go to agent: {goTo.name}
          </button>
        )}
        {denyAt !== null && <DenyCountdown denyAt={denyAt} />}
        {title !== null && (
          <div className="flex items-baseline justify-between gap-2">
            <h2 className="text-sm voice-heading">{title}</h2>
            {queued > 0 && (
              <span className="text-xs voice-ui text-muted-foreground">{queued} more queued</span>
            )}
          </div>
        )}
        {children}
      </div>
    </section>
  );
}

/** Input given to an ask still unanswered, per request id: the cold-cache panel unmounts its card. */
const keptApprovals = new Map<string, ApprovalInput>();
const keptDecisions = new Map<string, DecisionInput>();

/** State that outlives the card's unmounting, until the ask is answered. */
function useKept<T>(
  store: Map<string, T>,
  requestId: string,
  initial: T,
): [T, (change: (current: T) => T) => void] {
  const [value, setValue] = useState(() => store.get(requestId) ?? initial);
  const update = (change: (current: T) => T) => {
    const next = change(value);
    store.set(requestId, next);
    setValue(next);
  };
  return [value, update];
}

function useAnswer(submit: SubmitAnswer, requestId: string) {
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const answer = async (value: Answer): Promise<boolean> => {
    setSending(true);
    setError(null);
    try {
      await submit(requestId, value);
      keptApprovals.delete(requestId);
      keptDecisions.delete(requestId);
      return true;
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setSending(false);
      return false;
    }
  };
  return { sending, error, answer };
}

const GRANT_HINT: Record<GrantKind, string> = {
  read: "Tap a folder to also allow reading it for the session",
  directory: "Tap a folder to also allow working in it for the session",
};

const GRANT_PICKED: Record<GrantKind, string> = {
  read: "Allowing also allows reading",
  directory: "Allowing also allows working in",
};

/** The suggested folder as segments; tapping one picks it, tapping it again clears the pick. */
function FolderPicker({
  kind,
  segments,
  picked,
  onPick,
  disabled,
}: {
  kind: GrantKind;
  segments: readonly Segment[];
  picked: Segment | null;
  onPick: (path: string | null) => void;
  disabled: boolean;
}) {
  const pickedIndex = picked === null ? -1 : segments.indexOf(picked);
  return (
    <div className="space-y-1">
      <p className="text-xs voice-ui [overflow-wrap:anywhere] text-muted-foreground">
        {picked === null ? GRANT_HINT[kind] : `${GRANT_PICKED[kind]} ${picked.path}`}
      </p>
      <div className="flex flex-wrap items-center gap-y-1 text-sm voice-code">
        {segments.map((segment, index) => {
          const on = index === pickedIndex;
          // Folders under the pick are covered by it.
          const covered = pickedIndex >= 0 && index > pickedIndex;
          const look = on
            ? segment.broad
              ? "bg-destructive text-card border-destructive"
              : "bg-primary text-primary-foreground border-primary"
            : covered
              ? "bg-primary/15 border-transparent"
              : "bg-background border-transparent";
          return (
            <Fragment key={segment.path}>
              {segment.separator !== "" && (
                <span className="px-0.5 text-muted-foreground">{segment.separator}</span>
              )}
              <button
                type="button"
                disabled={disabled}
                aria-pressed={on}
                onClick={() => onPick(on ? null : segment.path)}
                className={`rounded-[5px] border px-1.5 py-1 disabled:opacity-50 ${look}`}
              >
                {segment.label}
              </button>
            </Fragment>
          );
        })}
      </div>
    </div>
  );
}

const COPIED_MS = 1500;

/** Copies `text`; text inside the swipe surface cannot be selected. */
function CopyButton({ text }: { text: string }) {
  const [label, setLabel] = useState("Copy");
  const show = (shown: string) => {
    setLabel(shown);
    setTimeout(() => setLabel("Copy"), COPIED_MS);
  };
  const copy = () => {
    // Undefined outside secure contexts, whatever the DOM types say.
    const clipboard: Clipboard | undefined = navigator.clipboard;
    if (clipboard === undefined) {
      show("Copy failed");
      return;
    }
    clipboard.writeText(text).then(
      () => show("Copied"),
      () => show("Copy failed"),
    );
  };
  return (
    <button
      type="button"
      onClick={copy}
      className="rounded-[5px] border border-destructive px-2 py-0.5 text-xs voice-ui"
    >
      {label}
    </button>
  );
}

interface ApprovalInput {
  noteOpen: boolean;
  note: string;
  acceptEdits: boolean;
  alwaysForFile: boolean;
  pickedPath: string | null;
}

const NO_APPROVAL_INPUT: ApprovalInput = {
  noteOpen: false,
  note: "",
  acceptEdits: false,
  alwaysForFile: false,
  pickedPath: null,
};

function approvalHoldsInput(input: ApprovalInput): boolean {
  return (
    input.note.trim() !== "" ||
    input.acceptEdits ||
    input.alwaysForFile ||
    input.pickedPath !== null
  );
}

/** An allow or deny ask (tool use or plan approval), answered by swipe. */
function ApprovalCard({
  submit,
  ask,
  queued,
  cwd,
  context,
}: {
  submit: SubmitAnswer;
  ask: PendingAsk;
  queued: number;
  cwd: string | null;
  context: string | null;
}) {
  const { sending, error, answer } = useAnswer(submit, ask.requestId);
  const [input, update] = useKept(keptApprovals, ask.requestId, NO_APPROVAL_INPUT);
  const { noteOpen, note, acceptEdits, alwaysForFile, pickedPath } = input;
  const set = (patch: Partial<ApprovalInput>) => update((current) => ({ ...current, ...patch }));
  // A failed answer remounts the swipe so it can be done again.
  const [attempt, setAttempt] = useState(0);
  const isPlan = ask.toolName === "ExitPlanMode";
  const offersAcceptEdits = ask.acceptEdits && !ask.danger.dangerous;
  const segments = useMemo(
    () => (ask.folder === null ? [] : offerSegments(ask.folder)),
    [ask.folder],
  );
  const picked = segments.find((segment) => segment.path === pickedPath) ?? null;
  const danger: Danger =
    picked?.broad === true
      ? { dangerous: true, reasons: [...ask.danger.reasons, broadReason(picked.path)] }
      : ask.danger;

  const confirm = (side: Side) => {
    const text = note.trim();
    const value: Answer =
      side === "right"
        ? {
            kind: "allow",
            ...(text === "" ? {} : { note: text }),
            ...(acceptEdits && offersAcceptEdits ? { accept_edits: true } : {}),
            ...(picked === null ? {} : { folder: picked.path }),
            ...(alwaysForFile && ask.file !== null ? { always_for_file: true } : {}),
          }
        : { kind: "deny", message: text === "" ? DEFAULT_DENY : text };
    void answer(value).then((ok) => {
      if (!ok) setAttempt((count) => count + 1);
    });
  };

  // The whole panel is the swipe surface; its own controls keep their taps.
  return (
    <CardFrame
      title={null}
      queued={0}
      scroll={isPlan ? "content" : "frame"}
      holdsInput={approvalHoldsInput(input)}
    >
      {/* Keyed by the ask and the choices: another ask or another grant means a fresh gesture. */}
      <Swipe
        key={`${ask.requestId}:${attempt}:${pickedPath ?? ""}:${acceptEdits}:${alwaysForFile}`}
        labels={isPlan ? { left: "Reject", right: "Approve" } : { left: "Deny", right: "Allow" }}
        dangerous={danger.dangerous}
        onConfirm={confirm}
        beforeRight={
          offersAcceptEdits && (
            <button
              type="button"
              disabled={sending}
              aria-pressed={acceptEdits}
              onClick={() => set({ acceptEdits: !acceptEdits })}
              className={`rounded-[5px] border px-2 py-1 text-sm voice-ui disabled:opacity-50 ${acceptEdits ? "border-mode-accept bg-mode-accept text-mode-accept-foreground" : "bg-card/60 text-muted-foreground"}`}
            >
              {acceptEdits ? "✓ " : ""}Accept edits
            </button>
          )
        }
      >
        <div className="flex min-h-0 flex-col gap-3">
          <div className="flex items-baseline justify-between gap-2">
            <h2 className="text-sm voice-heading">
              {isPlan
                ? "Plan approval"
                : DIFF_TOOLS.has(ask.toolName)
                  ? "Allow this change?"
                  : `Allow ${toolDisplayName(ask.toolName)}?`}
            </h2>
            {queued > 0 && (
              <span className="text-xs voice-ui text-muted-foreground">{queued} more queued</span>
            )}
          </div>
          {danger.dangerous && (
            <div className="rounded-[5px] border border-destructive bg-destructive/15 px-3 py-2 text-sm text-destructive">
              <div className="flex items-baseline justify-between gap-2">
                <p className="voice-heading">Dangerous</p>
                <CopyButton text={danger.reasons.join("\n")} />
              </div>
              <ul className="list-disc pl-5">
                {danger.reasons.map((reason) => (
                  <li key={reason} className="[overflow-wrap:anywhere]">
                    {reason}
                  </li>
                ))}
              </ul>
            </div>
          )}
          {isPlan ? (
            <PlanView plan={ask.plan} />
          ) : (
            <InputView ask={ask} cwd={cwd} context={context} />
          )}
          {ask.folder !== null && segments.length > 0 && (
            <FolderPicker
              kind={ask.folder.kind}
              segments={segments}
              picked={picked}
              onPick={(path) => set({ pickedPath: path })}
              disabled={sending}
            />
          )}
          {ask.file !== null && (
            <div className="space-y-1">
              <button
                type="button"
                disabled={sending}
                aria-pressed={alwaysForFile}
                onClick={() => set({ alwaysForFile: !alwaysForFile })}
                className={`rounded-[5px] border px-2 py-1 text-sm voice-ui disabled:opacity-50 ${alwaysForFile ? "border-primary bg-primary text-primary-foreground" : "bg-card/60 text-muted-foreground"}`}
              >
                {alwaysForFile ? "✓ " : ""}Always for this file
              </button>
              {alwaysForFile && (
                <p className="text-xs voice-ui [overflow-wrap:anywhere] text-muted-foreground">
                  {`Allowing also allows later ${ask.file.kind === "read" ? "reads" : "edits"} of ${relativePath(ask.file.path, cwd)} until the Claude process stops`}
                </p>
              )}
            </div>
          )}
          {noteOpen ? (
            <textarea
              value={note}
              onChange={(event) => set({ note: event.target.value })}
              disabled={sending}
              rows={2}
              placeholder="Note sent with the answer: the deny reason, or a follow-up after allowing"
              className="w-full shrink-0 resize-y rounded-[5px] border bg-background px-3 py-1.5 text-base voice-prose outline-none focus:ring-2 focus:ring-ring"
            />
          ) : (
            <button
              type="button"
              disabled={sending}
              onClick={() => set({ noteOpen: true })}
              className="self-start rounded-[5px] bg-card/60 px-2 py-1 text-sm voice-ui text-primary disabled:opacity-50"
            >
              + Note
            </button>
          )}
        </div>
      </Swipe>
      {sending && <p className="text-sm voice-ui text-muted-foreground">Sending...</p>}
      {error !== null && <p className="text-sm text-destructive">{error}</p>}
    </CardFrame>
  );
}

/** Switches to the discussion of the agent an ask belongs to, when another one is on screen. */
export interface GoTo {
  name: string;
  onGo: () => void;
}

const GoToAgent = createContext<GoTo | null>(null);
/** When the server denies the ask on screen; null for the main agent's asks. */
const DenyAt = createContext<number | null>(null);
/** When the main discussion's cache goes cold; null outside it or without a warm cache. */
const MainDeadline = createContext<CacheDeadline | null>(null);

function DenyCountdown({ denyAt }: { denyAt: number }) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);
  return (
    <p
      className="text-xs voice-ui text-destructive"
      title="The agent's cache expires soon: unanswered, the ask is denied and the agent told to wrap up"
    >
      {denyCountdown(denyAt, now)}
    </p>
  );
}

export function AskCard({
  submit,
  ask,
  queued,
  cwd,
  context,
  goTo,
  deadline,
}: {
  submit: SubmitAnswer;
  ask: PendingAsk;
  /** Asks waiting behind this one. */
  queued: number;
  /** The session folder; paths inside it show relative. */
  cwd: string | null;
  /** The text Claude wrote before the call, from `askContext`. */
  context: string | null;
  /** Null when the ask belongs to the discussion on screen. */
  goTo: GoTo | null;
  deadline: CacheDeadline | null;
}) {
  return (
    <GoToAgent.Provider value={goTo}>
      <DenyAt.Provider value={ask.denyAt}>
        <MainDeadline.Provider value={deadline}>
          {ask.toolName !== "AskUserQuestion" ? (
            <ApprovalCard submit={submit} ask={ask} queued={queued} cwd={cwd} context={context} />
          ) : (
            <DecisionCard submit={submit} ask={ask} queued={queued} />
          )}
        </MainDeadline.Provider>
      </DenyAt.Provider>
    </GoToAgent.Provider>
  );
}

interface DecisionInput {
  denyMessage: string;
  /** Per question text. */
  choices: Record<string, Choice>;
}

const NO_DECISION_INPUT: DecisionInput = { denyMessage: "", choices: {} };

function decisionHoldsInput(input: DecisionInput): boolean {
  return (
    input.denyMessage.trim() !== "" ||
    Object.values(input.choices).some(
      (choice) =>
        choice.labels.length > 0 || choice.custom.length > 0 || choice.draft.trim() !== "",
    )
  );
}

/** Questions. */
function DecisionCard({
  submit,
  ask,
  queued,
}: {
  submit: SubmitAnswer;
  ask: PendingAsk;
  queued: number;
}) {
  const { sending, error, answer } = useAnswer(submit, ask.requestId);
  const [input, update] = useKept(keptDecisions, ask.requestId, NO_DECISION_INPUT);
  const { denyMessage, choices } = input;

  const questions = ask.toolName === "AskUserQuestion" ? parseQuestions(ask.input) : null;

  const deny = () => {
    const message = denyMessage.trim();
    void answer({ kind: "deny", message: message === "" ? DEFAULT_DENY : message });
  };

  const complete =
    questions !== null &&
    questions.every(
      (question) =>
        choiceAnswer(choices[question.question] ?? NO_CHOICE, question.multiSelect) !== "",
    );

  const submitQuestions = () => {
    if (questions === null) return;
    const answers: Record<string, string> = {};
    for (const question of questions) {
      answers[question.question] = choiceAnswer(
        choices[question.question] ?? NO_CHOICE,
        question.multiSelect,
      );
    }
    void answer({ kind: "questions", answers });
  };

  const title = questions !== null ? "Question" : `Allow ${toolDisplayName(ask.toolName)}?`;

  return (
    <CardFrame title={title} queued={queued} holdsInput={decisionHoldsInput(input)}>
      {questions !== null ? (
        questions.map((question) => (
          <QuestionView
            key={question.question}
            question={question}
            disabled={sending}
            choice={choices[question.question] ?? NO_CHOICE}
            onChange={(choice) =>
              update((current) => ({
                ...current,
                choices: { ...current.choices, [question.question]: choice },
              }))
            }
          />
        ))
      ) : (
        <InputView ask={ask} />
      )}

      <input
        value={denyMessage}
        onChange={(event) => {
          const message = event.target.value;
          update((current) => ({ ...current, denyMessage: message }));
        }}
        disabled={sending}
        placeholder="Deny message for Claude (optional)"
        className="w-full rounded-[5px] border bg-background px-3 py-1.5 text-base outline-none focus:ring-2 focus:ring-ring"
      />

      <div className="flex flex-wrap justify-end gap-2">
        <button
          type="button"
          disabled={sending}
          onClick={deny}
          className={`border border-destructive text-destructive ${BUTTON}`}
        >
          Deny
        </button>
        {questions !== null ? (
          <button
            type="button"
            disabled={sending || !complete}
            onClick={submitQuestions}
            className={`bg-primary text-primary-foreground ${BUTTON}`}
          >
            Answer
          </button>
        ) : (
          <button
            type="button"
            disabled={sending}
            onClick={() => void answer({ kind: "allow" })}
            className={`bg-primary text-primary-foreground ${BUTTON}`}
          >
            Allow
          </button>
        )}
      </div>
      {error !== null && <p className="text-sm text-destructive">{error}</p>}
    </CardFrame>
  );
}
