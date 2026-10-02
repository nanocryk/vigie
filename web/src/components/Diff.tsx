import { useMemo } from "react";

import { type FileDiff, type LineKind, gutterWidth, highlightHunk } from "@/diff";

const ROW: Record<LineKind, string> = {
  context: "",
  added: "bg-diff-added",
  removed: "bg-diff-removed",
};

const SIGN: Record<LineKind, { text: string; className: string }> = {
  context: { text: " ", className: "" },
  added: { text: "+", className: "text-diff-added-sign" },
  removed: { text: "-", className: "text-diff-removed-sign" },
};

/** Hunks with a line-number gutter, full-width colored rows, scrolling or wrapping within the block. */
export function DiffBlock({
  diff,
  maxHeight = "max-h-[70dvh]",
  onNumber,
  onRemovedNumber,
  wrap = false,
}: {
  diff: FileDiff;
  /** A literal `max-h-*` class. */
  maxHeight?: string;
  /** Long lines wrap instead of scrolling sideways. */
  wrap?: boolean;
  /** Makes new-side line numbers tappable. */
  onNumber?: (line: number) => void;
  /** Makes removed lines' numbers tappable. */
  onRemovedNumber?: (line: number) => void;
}) {
  const width = gutterWidth(diff.hunks);
  const text = wrap ? "pr-3 min-w-0 whitespace-pre-wrap break-all" : "pr-3 whitespace-pre";
  const html = useMemo(() => diff.hunks.map((hunk) => highlightHunk(hunk, diff.language)), [diff]);
  return (
    <div
      className={`overflow-auto rounded-[5px] bg-code-block py-1 text-xs leading-[1.45] voice-code ${maxHeight}`}
    >
      <div className={wrap ? "w-full" : "w-max min-w-full"}>
        {diff.hunks.map((hunk, h) => (
          <div key={h} className={h > 0 ? "mt-3" : undefined}>
            {hunk.map((line, index) => {
              const sign = SIGN[line.kind];
              const lineHtml = html[h]?.[index] ?? null;
              const tap = line.kind === "removed" ? onRemovedNumber : onNumber;
              return (
                <div
                  key={index}
                  data-new={line.kind === "removed" ? undefined : (line.number ?? undefined)}
                  className={`flex min-h-[1.45em] ${wrap ? "items-start" : ""} ${ROW[line.kind]}`}
                >
                  {width > 0 &&
                    (tap !== undefined && line.number !== null ? (
                      <button
                        type="button"
                        onClick={() => {
                          if (line.number !== null) tap(line.number);
                        }}
                        title={
                          line.kind === "removed"
                            ? "Open its folder"
                            : `Open at line ${line.number}`
                        }
                        className="shrink-0 cursor-pointer pl-2 text-right text-muted-foreground select-none hover:text-primary hover:underline"
                        style={{ width: `calc(${width}ch + 0.5rem)` }}
                      >
                        {line.number}
                      </button>
                    ) : (
                      <span
                        className="shrink-0 pl-2 text-right text-muted-foreground select-none"
                        style={{ width: `calc(${width}ch + 0.5rem)` }}
                      >
                        {line.number ?? ""}
                      </span>
                    ))}
                  <span className={`w-[3ch] shrink-0 text-center select-none ${sign.className}`}>
                    {sign.text}
                  </span>
                  {lineHtml === null ? (
                    <span className={text}>{line.text}</span>
                  ) : (
                    <span className={text} dangerouslySetInnerHTML={{ __html: lineHtml }} />
                  )}
                </div>
              );
            })}
          </div>
        ))}
      </div>
    </div>
  );
}
