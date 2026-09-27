import { useState } from "react";

import type { Client } from "@/client";
import { type PendingAsk, toolSummary } from "@/transcript";
import { type Answer, isRecord } from "@/wire";

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

export function AskCard({
  client,
  session,
  ask,
  queued,
}: {
  client: Client;
  session: string;
  ask: PendingAsk;
  /** Asks waiting behind this one. */
  queued: number;
}) {
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [denyMessage, setDenyMessage] = useState("");
  const [selections, setSelections] = useState<Record<string, string[]>>({});

  const questions = ask.toolName === "AskUserQuestion" ? parseQuestions(ask.input) : null;
  const plan = ask.toolName === "ExitPlanMode" ? planText(ask.input) : null;
  const known = ask.input !== undefined;

  const answer = async (value: Answer) => {
    setSending(true);
    setError(null);
    try {
      await client.command({
        type: "answer",
        session,
        request_id: ask.requestId,
        answer: value,
        client_id: client.newClientId(),
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setSending(false);
    }
  };

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

  const title =
    questions !== null ? "Question" : plan !== null ? "Plan approval" : `Allow ${ask.toolName}?`;

  return (
    <section className="bg-card border-primary/40 max-h-[50%] shrink-0 overflow-y-auto border-t-2 px-3 py-3">
      <div className="mx-auto max-w-5xl space-y-3">
        <div className="flex items-baseline justify-between gap-2">
          <h2 className="voice-heading text-sm">{title}</h2>
          {queued > 0 && (
            <span className="text-muted-foreground voice-ui text-xs">{queued} more queued</span>
          )}
        </div>

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
        ) : plan !== null ? (
          <div className="bg-background voice-prose max-h-[40dvh] overflow-y-auto rounded-[5px] border p-2 text-sm whitespace-pre-wrap [overflow-wrap:anywhere]">
            {plan}
          </div>
        ) : known ? (
          <>
            <div className="voice-code bg-background rounded-[5px] border p-2 text-sm whitespace-pre-wrap [overflow-wrap:anywhere]">
              {toolSummary(ask.toolName, ask.input)}
            </div>
            <details>
              <summary className="text-muted-foreground voice-ui cursor-pointer text-xs">
                full input
              </summary>
              <pre className="bg-muted mt-1 max-h-60 overflow-auto rounded-[5px] p-2 text-xs whitespace-pre-wrap [overflow-wrap:anywhere]">
                {JSON.stringify(ask.input, null, 2)}
              </pre>
            </details>
          </>
        ) : (
          <p className="text-muted-foreground text-sm">Input not received.</p>
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
      </div>
    </section>
  );
}
