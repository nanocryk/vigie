import { Fragment, type ReactNode, useMemo, useState } from "react";

import { DiffBlock } from "@/components/Diff";
import { Swipe } from "@/components/Swipe";
import { DIFF_TOOLS, changeSummary, toolDiff } from "@/diff";
import { type Segment, broadReason, offerSegments } from "@/grant";
import type { Side } from "@/swipe";
import { type PendingAsk, relativePath, toolSummary } from "@/transcript";
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

function planText(input: unknown): string | null {
  if (!isRecord(input)) return null;
  const plan = input["plan"];
  return typeof plan === "string" ? plan : null;
}

const BUTTON = "voice-ui rounded-[5px] px-4 py-2 text-sm disabled:opacity-50";

function QuestionView({
  question,
  selected,
  onChange,
  disabled,
}: {
  question: Question;
  selected: readonly string[];
  onChange: (labels: string[]) => void;
  disabled: boolean;
}) {
  const toggle = (label: string) => {
    if (!question.multiSelect) onChange([label]);
    else if (selected.includes(label)) onChange(selected.filter((item) => item !== label));
    else onChange([...selected, label]);
  };
  return (
    <fieldset className="space-y-1.5">
      <legend className="voice-prose mb-1 text-sm">
        {question.header !== null && (
          <span className="text-muted-foreground voice-ui mr-2 text-xs">{question.header}</span>
        )}
        {question.question}
        {question.multiSelect && (
          <span className="text-muted-foreground voice-ui ml-2 text-xs">(several allowed)</span>
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
            className={`block w-full rounded-[5px] border px-3 py-1.5 text-left text-sm ${checked ? "border-primary bg-primary/10" : "bg-background"}`}
          >
            <span className="voice-ui">{option.label}</span>
            {option.description !== null && (
              <span className="text-muted-foreground block text-xs">{option.description}</span>
            )}
          </button>
        );
      })}
    </fieldset>
  );
}

function InputView({ ask, cwd = null }: { ask: PendingAsk; cwd?: string | null }) {
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
    return <p className="text-muted-foreground text-sm">Input not received.</p>;
  }
  return (
    <>
      {diff === null ? (
        <div className="voice-code bg-background rounded-[5px] border p-2 text-sm whitespace-pre-wrap [overflow-wrap:anywhere]">
          {toolSummary(ask.toolName, ask.input, cwd)}
        </div>
      ) : (
        <div className="space-y-1">
          <div className="voice-code min-w-0 truncate text-sm">
            <span className="voice-ui">{diff.verb}</span>({diff.path})
          </div>
          <div className="text-muted-foreground voice-ui text-xs">
            &#x2514; {changeSummary(diff.added, diff.removed)}
          </div>
          <DiffBlock diff={diff} maxHeight="max-h-[40dvh]" />
        </div>
      )}
      <details className="mt-1">
        <summary className="text-muted-foreground voice-ui cursor-pointer text-xs">
          full input
        </summary>
        <pre className="bg-muted mt-1 max-h-60 overflow-auto rounded-[5px] p-2 text-xs whitespace-pre-wrap [overflow-wrap:anywhere]">
          {JSON.stringify(ask.input, null, 2)}
        </pre>
      </details>
    </>
  );
}

function CardFrame({
  title,
  queued,
  children,
}: {
  /** Null when the children draw their own header. */
  title: string | null;
  queued: number;
  children: ReactNode;
}) {
  return (
    <section className="bg-card border-primary/40 max-h-[50%] shrink-0 overflow-y-auto border-t-2 px-3 py-3">
      <div className="mx-auto max-w-5xl space-y-3">
        {title !== null && (
          <div className="flex items-baseline justify-between gap-2">
            <h2 className="voice-heading text-sm">{title}</h2>
            {queued > 0 && (
              <span className="text-muted-foreground voice-ui text-xs">{queued} more queued</span>
            )}
          </div>
        )}
        {children}
      </div>
    </section>
  );
}

function useAnswer(submit: SubmitAnswer, requestId: string) {
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const answer = async (value: Answer): Promise<boolean> => {
    setSending(true);
    setError(null);
    try {
      await submit(requestId, value);
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
      <p className="text-muted-foreground voice-ui text-xs [overflow-wrap:anywhere]">
        {picked === null ? GRANT_HINT[kind] : `${GRANT_PICKED[kind]} ${picked.path}`}
      </p>
      <div className="voice-code flex flex-wrap items-center gap-y-1 text-sm">
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
                <span className="text-muted-foreground px-0.5">{segment.separator}</span>
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
      className="voice-ui border-destructive rounded-[5px] border px-2 py-0.5 text-xs"
    >
      {label}
    </button>
  );
}

/** An allow or deny ask (tool use or plan approval), answered by swipe. */
function ApprovalCard({
  submit,
  ask,
  queued,
  cwd,
}: {
  submit: SubmitAnswer;
  ask: PendingAsk;
  queued: number;
  cwd: string | null;
}) {
  const { sending, error, answer } = useAnswer(submit, ask.requestId);
  const [noteOpen, setNoteOpen] = useState(false);
  const [note, setNote] = useState("");
  const [acceptEdits, setAcceptEdits] = useState(false);
  const [alwaysForFile, setAlwaysForFile] = useState(false);
  const [pickedPath, setPickedPath] = useState<string | null>(null);
  // A failed answer remounts the swipe so it can be done again.
  const [attempt, setAttempt] = useState(0);
  const plan = ask.toolName === "ExitPlanMode" ? planText(ask.input) : null;
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
    <CardFrame title={null} queued={0}>
      {/* Keyed by the ask and the choices: another ask or another grant means a fresh gesture. */}
      <Swipe
        key={`${ask.requestId}:${attempt}:${pickedPath ?? ""}:${acceptEdits}:${alwaysForFile}`}
        labels={plan !== null ? { left: "Reject", right: "Approve" } : { left: "Deny", right: "Allow" }}
        dangerous={danger.dangerous}
        onConfirm={confirm}
        beforeRight={
          offersAcceptEdits && (
            <button
              type="button"
              disabled={sending}
              aria-pressed={acceptEdits}
              onClick={() => setAcceptEdits((on) => !on)}
              className={`voice-ui rounded-[5px] border px-2 py-1 text-sm disabled:opacity-50 ${acceptEdits ? "bg-mode-accept text-mode-accept-foreground border-mode-accept" : "text-muted-foreground bg-card/60"}`}
            >
              {acceptEdits ? "✓ " : ""}Accept edits
            </button>
          )
        }
      >
        <div className="space-y-3">
          <div className="flex items-baseline justify-between gap-2">
            <h2 className="voice-heading text-sm">
              {plan !== null
                ? "Plan approval"
                : DIFF_TOOLS.has(ask.toolName)
                  ? "Allow this change?"
                  : `Allow ${ask.toolName}?`}
            </h2>
            {queued > 0 && (
              <span className="text-muted-foreground voice-ui text-xs">{queued} more queued</span>
            )}
          </div>
          {danger.dangerous && (
            <div className="border-destructive bg-destructive/15 text-destructive rounded-[5px] border px-3 py-2 text-sm">
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
          {plan !== null ? (
            <div className="bg-background voice-prose max-h-[40dvh] overflow-y-auto rounded-[5px] border p-2 text-sm whitespace-pre-wrap [overflow-wrap:anywhere]">
              {plan}
            </div>
          ) : (
            <InputView ask={ask} cwd={cwd} />
          )}
          {ask.folder !== null && segments.length > 0 && (
            <FolderPicker
              kind={ask.folder.kind}
              segments={segments}
              picked={picked}
              onPick={setPickedPath}
              disabled={sending}
            />
          )}
          {ask.file !== null && (
            <div className="space-y-1">
              <button
                type="button"
                disabled={sending}
                aria-pressed={alwaysForFile}
                onClick={() => setAlwaysForFile((on) => !on)}
                className={`voice-ui rounded-[5px] border px-2 py-1 text-sm disabled:opacity-50 ${alwaysForFile ? "bg-primary text-primary-foreground border-primary" : "text-muted-foreground bg-card/60"}`}
              >
                {alwaysForFile ? "✓ " : ""}Always for this file
              </button>
              {alwaysForFile && (
                <p className="text-muted-foreground voice-ui text-xs [overflow-wrap:anywhere]">
                  {`Allowing also allows later ${ask.file.kind === "read" ? "reads" : "edits"} of ${relativePath(ask.file.path, cwd)} until the Claude process stops`}
                </p>
              )}
            </div>
          )}
          {noteOpen ? (
            <textarea
              value={note}
              onChange={(event) => setNote(event.target.value)}
              disabled={sending}
              rows={2}
              placeholder="Note sent with the answer: the deny reason, or a follow-up after allowing"
              className="bg-background focus:ring-ring voice-prose w-full resize-y rounded-[5px] border px-3 py-1.5 text-base outline-none focus:ring-2"
            />
          ) : (
            <button
              type="button"
              disabled={sending}
              onClick={() => setNoteOpen(true)}
              className="text-primary voice-ui bg-card/60 rounded-[5px] px-2 py-1 text-sm disabled:opacity-50"
            >
              + Note
            </button>
          )}
        </div>
      </Swipe>
      {sending && <p className="text-muted-foreground voice-ui text-sm">Sending...</p>}
      {error !== null && <p className="text-destructive text-sm">{error}</p>}
    </CardFrame>
  );
}

export function AskCard({
  submit,
  ask,
  queued,
  cwd,
}: {
  submit: SubmitAnswer;
  ask: PendingAsk;
  /** Asks waiting behind this one. */
  queued: number;
  /** The session folder; paths inside it show relative. */
  cwd: string | null;
}) {
  if (ask.toolName !== "AskUserQuestion") {
    return <ApprovalCard submit={submit} ask={ask} queued={queued} cwd={cwd} />;
  }
  return <DecisionCard submit={submit} ask={ask} queued={queued} />;
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
  const [denyMessage, setDenyMessage] = useState("");
  const [selections, setSelections] = useState<Record<string, string[]>>({});

  const questions = ask.toolName === "AskUserQuestion" ? parseQuestions(ask.input) : null;

  const deny = () => {
    const message = denyMessage.trim();
    void answer({ kind: "deny", message: message === "" ? DEFAULT_DENY : message });
  };

  const complete =
    questions !== null &&
    questions.every((question) => (selections[question.question] ?? []).length > 0);

  const submitQuestions = () => {
    if (questions === null) return;
    const answers: Record<string, string> = {};
    for (const question of questions) {
      answers[question.question] = (selections[question.question] ?? []).join(", ");
    }
    void answer({ kind: "questions", answers });
  };

  const title = questions !== null ? "Question" : `Allow ${ask.toolName}?`;

  return (
    <CardFrame title={title} queued={queued}>
      {questions !== null ? (
        questions.map((question) => (
          <QuestionView
            key={question.question}
            question={question}
            disabled={sending}
            selected={selections[question.question] ?? []}
            onChange={(labels) =>
              setSelections((current) => ({ ...current, [question.question]: labels }))
            }
          />
        ))
      ) : (
        <InputView ask={ask} />
      )}

      <input
        value={denyMessage}
        onChange={(event) => setDenyMessage(event.target.value)}
        disabled={sending}
        placeholder="Deny message for Claude (optional)"
        className="bg-background focus:ring-ring w-full rounded-[5px] border px-3 py-1.5 text-base outline-none focus:ring-2"
      />

      <div className="flex flex-wrap gap-2">
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
        <button
          type="button"
          disabled={sending}
          onClick={deny}
          className={`border-destructive text-destructive border ${BUTTON}`}
        >
          Deny
        </button>
      </div>
      {error !== null && <p className="text-destructive text-sm">{error}</p>}
    </CardFrame>
  );
}
