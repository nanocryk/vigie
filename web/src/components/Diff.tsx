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

/** Hunks with a line-number gutter, full-width colored rows, scrolling within the block. */
export function DiffBlock({
  diff,
  maxHeight = "max-h-[70dvh]",
}: {
  diff: FileDiff;
  /** A literal `max-h-*` class. */
  maxHeight?: string;
}) {
  const width = gutterWidth(diff.hunks);
  const html = useMemo(
    () => diff.hunks.map((hunk) => highlightHunk(hunk, diff.language)),
    [diff],
  );
  return (
    <div
      className={`bg-code-block voice-code overflow-auto rounded-[5px] py-1 text-xs leading-[1.45] ${maxHeight}`}
    >
      <div className="w-max min-w-full">
        {diff.hunks.map((hunk, h) => (
          <div key={h} className={h > 0 ? "mt-3" : undefined}>
            {hunk.map((line, index) => {
              const sign = SIGN[line.kind];
              const lineHtml = html[h]?.[index] ?? null;
              return (
                <div key={index} className={`flex min-h-[1.45em] ${ROW[line.kind]}`}>
                  {width > 0 && (
                    <span
                      className="text-muted-foreground shrink-0 pl-2 text-right select-none"
                      style={{ width: `calc(${width}ch + 0.5rem)` }}
                    >
                      {line.number ?? ""}
                    </span>
                  )}
                  <span className={`w-[3ch] shrink-0 text-center select-none ${sign.className}`}>
                    {sign.text}
                  </span>
                  {lineHtml === null ? (
                    <span className="pr-3 whitespace-pre">{line.text}</span>
                  ) : (
                    <span
                      className="pr-3 whitespace-pre"
                      dangerouslySetInnerHTML={{ __html: lineHtml }}
                    />
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
