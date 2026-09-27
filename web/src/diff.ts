import { highlightLines, languageFromPath } from "@/highlight";
import type { ToolStatus } from "@/transcript";
import { isRecord } from "@/wire";

export type LineKind = "context" | "added" | "removed";

export interface DiffLine {
  kind: LineKind;
  text: string;
  /** In the old file for a removed line, the new file otherwise; null when unknown. */
  number: number | null;
}

export type Hunk = readonly DiffLine[];

export interface FileDiff {
  /** `Update` changes a file, `Write` creates one (or writes it whole, not knowing which). */
  verb: "Update" | "Write";
  path: string;
  language: string | null;
  hunks: readonly Hunk[];
  added: number;
  removed: number;
}

/** Tools whose rows show a diff. */
export const DIFF_TOOLS: ReadonlySet<string> = new Set(["Edit", "MultiEdit", "Write"]);

/** Beyond this many line pairs, an input diff shows every old line removed and every new one added. */
const DIFF_LIMIT = 250_000;

/** Lines of a text; a final newline ends the last line instead of starting an empty one. */
export function splitLines(text: string): string[] {
  if (text === "") return [];
  const lines = text.split("\n").map((line) => line.replace(/\r$/, ""));
  if (lines[lines.length - 1] === "") lines.pop();
  return lines;
}

function hunkFromPatch(raw: unknown): Hunk | null {
  if (!isRecord(raw)) return null;
  const { oldStart, newStart, lines } = raw;
  if (typeof oldStart !== "number" || typeof newStart !== "number" || !Array.isArray(lines)) {
    return null;
  }
  let oldLine = oldStart;
  let newLine = newStart;
  const hunk: DiffLine[] = [];
  for (const line of lines) {
    if (typeof line !== "string") return null;
    const sign = line.charAt(0);
    if (sign === "\\") continue;
    if (sign === "-") {
      hunk.push({ kind: "removed", text: line.slice(1), number: oldLine });
      oldLine += 1;
    } else if (sign === "+") {
      hunk.push({ kind: "added", text: line.slice(1), number: newLine });
      newLine += 1;
    } else {
      hunk.push({ kind: "context", text: sign === " " ? line.slice(1) : line, number: newLine });
      oldLine += 1;
      newLine += 1;
    }
  }
  return hunk;
}

/** Hunks of a `structuredPatch`; null when it is missing or not of the expected shape. */
export function parsePatch(raw: unknown): Hunk[] | null {
  if (!Array.isArray(raw)) return null;
  const hunks: Hunk[] = [];
  for (const entry of raw) {
    const hunk = hunkFromPatch(entry);
    if (hunk === null) return null;
    hunks.push(hunk);
  }
  return hunks;
}

/**
 * Line diff of two texts (longest common subsequence), removed lines before
 * added ones within a change. Line numbers are unknown.
 */
export function lineDiff(oldText: string, newText: string): Hunk {
  const a = splitLines(oldText);
  const b = splitLines(newText);
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head += 1;
  let tail = 0;
  while (
    tail < a.length - head &&
    tail < b.length - head &&
    a[a.length - 1 - tail] === b[b.length - 1 - tail]
  ) {
    tail += 1;
  }
  const line = (kind: LineKind, text: string): DiffLine => ({ kind, text, number: null });
  const midA = a.slice(head, a.length - tail);
  const midB = b.slice(head, b.length - tail);
  const middle: DiffLine[] = [];
  if (midA.length * midB.length > DIFF_LIMIT) {
    middle.push(...midA.map((text) => line("removed", text)));
    middle.push(...midB.map((text) => line("added", text)));
  } else {
    // common[i][j]: length of the longest common subsequence of midA[i..] and midB[j..].
    const width = midB.length + 1;
    const common = new Uint32Array((midA.length + 1) * width);
    const at = (i: number, j: number) => common[i * width + j] ?? 0;
    for (let i = midA.length - 1; i >= 0; i -= 1) {
      for (let j = midB.length - 1; j >= 0; j -= 1) {
        common[i * width + j] =
          midA[i] === midB[j] ? at(i + 1, j + 1) + 1 : Math.max(at(i + 1, j), at(i, j + 1));
      }
    }
    let i = 0;
    let j = 0;
    while (i < midA.length || j < midB.length) {
      const left = midA[i];
      const right = midB[j];
      if (left !== undefined && right !== undefined && left === right) {
        middle.push(line("context", left));
        i += 1;
        j += 1;
      } else if (left !== undefined && (right === undefined || at(i + 1, j) >= at(i, j + 1))) {
        middle.push(line("removed", left));
        i += 1;
      } else if (right !== undefined) {
        middle.push(line("added", right));
        j += 1;
      }
    }
  }
  return [
    ...a.slice(0, head).map((text) => line("context", text)),
    ...middle,
    ...a.slice(a.length - tail).map((text) => line("context", text)),
  ];
}

/** Every line of a new file, added, numbered from 1. */
export function wholeFile(content: string): Hunk {
  return splitLines(content).map((text, index) => ({ kind: "added", text, number: index + 1 }));
}

function stringField(raw: unknown, key: string): string | null {
  if (!isRecord(raw)) return null;
  const value = raw[key];
  return typeof value === "string" ? value : null;
}

/** Non-empty hunks of the result's `structuredPatch`, else null. */
function resultHunks(result: unknown): Hunk[] | null {
  if (!isRecord(result)) return null;
  const hunks = parsePatch(result["structuredPatch"]);
  return hunks === null || hunks.length === 0 ? null : hunks;
}

function editHunks(input: unknown): Hunk[] | null {
  const oldText = stringField(input, "old_string");
  const newText = stringField(input, "new_string");
  return oldText === null || newText === null ? null : [lineDiff(oldText, newText)];
}

function multiEditHunks(input: unknown): Hunk[] | null {
  if (!isRecord(input) || !Array.isArray(input["edits"])) return null;
  const hunks: Hunk[] = [];
  for (const edit of input["edits"]) {
    const [hunk] = editHunks(edit) ?? [];
    if (hunk === undefined) return null;
    hunks.push(hunk);
  }
  return hunks;
}

function writeDiff(input: unknown, result: unknown): Pick<FileDiff, "verb" | "hunks"> | null {
  if (isRecord(result) && result["type"] === "update") {
    const hunks = parsePatch(result["structuredPatch"]);
    if (hunks !== null) return { verb: "Update", hunks };
  }
  const content = stringField(result, "content") ?? stringField(input, "content");
  return content === null ? null : { verb: "Write", hunks: [wholeFile(content)] };
}

function counts(hunks: readonly Hunk[]): { added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const hunk of hunks) {
    for (const line of hunk) {
      if (line.kind === "added") added += 1;
      else if (line.kind === "removed") removed += 1;
    }
  }
  return { added, removed };
}

/**
 * The diff an Edit, MultiEdit or Write call shows: from the result's
 * `structuredPatch` when present, else built from the input. Null for other
 * tools, a failed or unfinished call, or an input of unexpected shape.
 * `path` is the file as displayed.
 */
export function toolDiff(tool: {
  name: string;
  input: unknown;
  result: unknown;
  status: ToolStatus;
  path: string;
}): FileDiff | null {
  if (tool.status === "error" || tool.status === "unfinished") return null;
  let body: Pick<FileDiff, "verb" | "hunks"> | null = null;
  switch (tool.name) {
    case "Edit": {
      const hunks = resultHunks(tool.result) ?? editHunks(tool.input);
      body = hunks === null ? null : { verb: "Update", hunks };
      break;
    }
    case "MultiEdit": {
      const hunks = resultHunks(tool.result) ?? multiEditHunks(tool.input);
      body = hunks === null ? null : { verb: "Update", hunks };
      break;
    }
    case "Write":
      body = writeDiff(tool.input, tool.result);
      break;
  }
  if (body === null) return null;
  const filePath = stringField(tool.input, "file_path") ?? tool.path;
  return { ...body, path: tool.path, language: languageFromPath(filePath), ...counts(body.hunks) };
}

/** `Added 2 lines, removed 1 line`, as the TUI words it. */
export function changeSummary(added: number, removed: number): string {
  const lines = (count: number) => (count === 1 ? "1 line" : `${count} lines`);
  if (added === 0 && removed === 0) return "No changes";
  if (removed === 0) return `Added ${lines(added)}`;
  if (added === 0) return `Removed ${lines(removed)}`;
  return `Added ${lines(added)}, removed ${lines(removed)}`;
}

/** Characters needed by the widest line number; 0 when no line is numbered. */
export function gutterWidth(hunks: readonly Hunk[]): number {
  let widest = 0;
  for (const hunk of hunks) {
    for (const line of hunk) {
      if (line.number !== null) widest = Math.max(widest, String(line.number).length);
    }
  }
  return widest;
}

/**
 * Highlighted HTML per line of a hunk, null entries when the language is unknown.
 * Each side (old: context and removed, new: context and added) is highlighted
 * as a whole so tokens spanning lines keep their colors.
 */
export function highlightHunk(hunk: Hunk, language: string | null): (string | null)[] {
  const side = (kind: LineKind) =>
    hunk.filter((line) => line.kind === "context" || line.kind === kind);
  const oldSide = side("removed");
  const newSide = side("added");
  const oldHtml = highlightLines(oldSide.map((line) => line.text).join("\n"), language);
  const newHtml = highlightLines(newSide.map((line) => line.text).join("\n"), language);
  if (oldHtml === null || newHtml === null) return hunk.map(() => null);
  let oldIndex = 0;
  let newIndex = 0;
  return hunk.map((line) => {
    if (line.kind === "removed") {
      const html = oldHtml[oldIndex] ?? null;
      oldIndex += 1;
      return html;
    }
    if (line.kind === "context") oldIndex += 1;
    const html = newHtml[newIndex] ?? null;
    newIndex += 1;
    return html;
  });
}
