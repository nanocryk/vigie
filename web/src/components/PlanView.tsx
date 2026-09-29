import { Fragment, memo, useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import type { Components, ExtraProps } from "react-markdown";

import {
  CodeBlock,
  type HastElement,
  Markdown,
  fenceLanguage,
  textOf,
} from "@/components/Markdown";
import { highlightLines } from "@/highlight";
import {
  ADDED_CLASS,
  CUT_CLASS,
  DOT_CLASS,
  type Removal,
  planChanges,
  remarkPlanChanges,
} from "@/planDiff";
import type { PlanReview } from "@/wire";

const BUTTON = "voice-ui rounded-[5px] border px-2 py-0.5 text-xs";

/** A removal: a red dot for words, a red line for lines; a tap shows the removed text. */
function RemovalMark({ removal }: { removal: Removal }) {
  const [open, setOpen] = useState(false);
  const kind = removal.lines ? "line" : "dot";
  return (
    <span className={`plan-mark plan-mark-${kind}`}>
      <button
        type="button"
        aria-label="Show removed text"
        aria-expanded={open}
        onClick={() => setOpen((shown) => !shown)}
        className={removal.lines ? CUT_CLASS : DOT_CLASS}
      />
      {open && <span className="plan-pop voice-code">{removal.text}</span>}
    </span>
  );
}

/** `[line, removal index]` pairs of a marked code block's `data-cuts`; empty when malformed. */
function parseCuts(raw: unknown): [number, number][] {
  if (typeof raw !== "string") return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.flatMap((entry: unknown): [number, number][] => {
      if (!Array.isArray(entry)) return [];
      const [line, index]: unknown[] = entry;
      return typeof line === "number" && typeof index === "number" ? [[line, index]] : [];
    });
  } catch {
    return [];
  }
}

/** A code block with changed lines tinted and removal marks before or at the end of lines. */
function PlanCode({ node, removals }: ExtraProps & { removals: readonly Removal[] }) {
  const code = node?.children.find(
    (child): child is HastElement => child.type === "element" && child.tagName === "code",
  );
  const addedRaw = code?.properties["dataAdded"];
  const cutsRaw = code?.properties["dataCuts"];
  if (code === undefined || (addedRaw === undefined && cutsRaw === undefined)) {
    return <CodeBlock node={node} />;
  }
  const added = new Set(
    typeof addedRaw === "string" && addedRaw !== "" ? addedRaw.split(",").map(Number) : [],
  );
  const cuts = parseCuts(cutsRaw);
  const text = textOf(code).replace(/\n$/, "");
  const lines = text.split("\n");
  const html = highlightLines(text, fenceLanguage(code));
  const marks = (line: number, lines: boolean) =>
    cuts.flatMap(([at, index]) => {
      const removal = removals[index];
      return at === line && removal !== undefined && removal.lines === lines
        ? [<RemovalMark key={index} removal={removal} />]
        : [];
    });
  return (
    <pre>
      <code>
        {lines.map((line, index) => (
          <Fragment key={index}>
            {marks(index, true)}
            <span className={`plan-code-line ${added.has(index) ? ADDED_CLASS : ""}`}>
              {html === null ? (
                line
              ) : (
                <span dangerouslySetInnerHTML={{ __html: html[index] ?? "" }} />
              )}
              {marks(index, false)}
            </span>
          </Fragment>
        ))}
      </code>
    </pre>
  );
}

/** The plan as markdown, words changed since `previous` tinted, removals marked. */
export const PlanBody = memo(function PlanBody({
  text,
  previous,
}: {
  text: string;
  previous: string | null;
}) {
  const extras = useMemo(() => {
    if (previous === null) return null;
    const changes = planChanges(previous, text);
    const removal = (node: HastElement | undefined) => {
      const index = node?.properties["dataRemoved"];
      return index === undefined || index === null ? null : changes.removals[Number(index)];
    };
    const components: Components = {
      div: ({ node, children, ...props }) => {
        const found = removal(node);
        if (found === null) return <div {...props}>{children}</div>;
        return found === undefined ? null : <RemovalMark removal={found} />;
      },
      span: ({ node, children, ...props }) => {
        const found = removal(node);
        if (found === null) return <span {...props}>{children}</span>;
        return found === undefined ? null : <RemovalMark removal={found} />;
      },
      pre: ({ node }) => <PlanCode node={node} removals={changes.removals} />,
    };
    return { plugins: [remarkPlanChanges(text, changes)], components };
  }, [text, previous]);
  return (
    <Markdown text={text} plugins={extras?.plugins} components={extras?.components} />
  );
});

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/** The plan of a plan approval, with a full-screen reader. */
export function PlanView({ plan }: { plan: PlanReview | null }) {
  const [full, setFull] = useState(false);

  useEffect(() => {
    if (!full) return;
    const close = (event: KeyboardEvent) => {
      if (event.key === "Escape") setFull(false);
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [full]);

  if (plan === null) {
    return <p className="text-muted-foreground voice-ui text-sm">No plan file found.</p>;
  }
  const name = plan.path === null ? "Plan" : fileName(plan.path);
  const note = plan.previous === null ? null : "changes since the last review are marked";
  return (
    <div className="flex min-h-0 flex-col gap-1">
      <div className="flex items-baseline gap-2">
        <span className="voice-code text-muted-foreground min-w-0 truncate text-xs">{name}</span>
        {note !== null && (
          <span className="text-muted-foreground voice-ui shrink-0 text-xs">{note}</span>
        )}
        <button type="button" onClick={() => setFull(true)} className={`${BUTTON} ml-auto shrink-0`}>
          Full screen
        </button>
      </div>
      {/* Shrinks to what the card leaves, the one scroll area of a plan approval. */}
      <div className="bg-background min-h-0 overflow-y-auto overscroll-contain rounded-[5px] border p-2 text-sm">
        <PlanBody text={plan.text} previous={plan.previous} />
      </div>
      {full &&
        // Outside the card's DOM, so its swipe never sees the reader's gestures.
        createPortal(
          <div className="bg-background fixed inset-0 z-40 flex flex-col">
            <div className="flex items-baseline gap-2 border-b px-3 py-2">
              <span className="voice-code min-w-0 truncate text-sm">{name}</span>
              {note !== null && (
                <span className="text-muted-foreground voice-ui shrink-0 text-xs">{note}</span>
              )}
              <button
                type="button"
                onClick={() => setFull(false)}
                className={`${BUTTON} ml-auto shrink-0`}
              >
                Close
              </button>
            </div>
            <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
              <div className="mx-auto max-w-3xl px-4 py-3 text-sm">
                <PlanBody text={plan.text} previous={plan.previous} />
              </div>
            </div>
          </div>,
          document.body,
        )}
    </div>
  );
}
