import { lineDiff } from "@/diff";
import { isRecord } from "@/wire";

/** Text taken out of a plan. */
export interface Removal {
  /** Offset in the new text where it was. */
  at: number;
  text: string;
  /** Whole lines went, shown as a line between blocks; else words, shown as a dot. */
  lines: boolean;
}

export interface PlanChanges {
  /** Offset ranges `[start, end)` of the new text that are new, sorted. */
  added: readonly (readonly [number, number])[];
  removals: readonly Removal[];
}

export const ADDED_CLASS = "plan-added";
export const DOT_CLASS = "plan-dot";
export const CUT_CLASS = "plan-cut";

/** Beyond this many token pairs, a changed run counts as all new, its old text as removed lines. */
const WORD_LIMIT = 250_000;

const TOKEN = /\s+|[\p{L}\p{N}_]+|[^\s\p{L}\p{N}_]/gu;

/** Start offset of each line of `text`. */
function lineStarts(text: string): number[] {
  const starts = [0];
  for (let index = 0; index < text.length; index += 1) {
    if (text[index] === "\n") starts.push(index + 1);
  }
  return starts;
}

/** `[start, end)` of 0-based line `line`, without its line break. */
function lineSpan(text: string, starts: readonly number[], line: number): [number, number] {
  const start = starts[line] ?? text.length;
  let end = (starts[line + 1] ?? text.length + 1) - 1;
  if (end > start && text[end - 1] === "\r") end -= 1;
  return [start, Math.max(start, end)];
}

/** Longest common subsequence walk of two token lists: `same`, `removed` or `added` per step. */
function tokenDiff(
  left: readonly string[],
  right: readonly string[],
): ("same" | "removed" | "added")[] | null {
  if (left.length * right.length > WORD_LIMIT) return null;
  const width = right.length + 1;
  const common = new Uint32Array((left.length + 1) * width);
  const at = (i: number, j: number) => common[i * width + j] ?? 0;
  for (let i = left.length - 1; i >= 0; i -= 1) {
    for (let j = right.length - 1; j >= 0; j -= 1) {
      common[i * width + j] =
        left[i] === right[j] ? at(i + 1, j + 1) + 1 : Math.max(at(i + 1, j), at(i, j + 1));
    }
  }
  const steps: ("same" | "removed" | "added")[] = [];
  let i = 0;
  let j = 0;
  while (i < left.length || j < right.length) {
    if (i < left.length && j < right.length && left[i] === right[j]) {
      steps.push("same");
      i += 1;
      j += 1;
    } else if (i < left.length && (j >= right.length || at(i + 1, j) >= at(i, j + 1))) {
      steps.push("removed");
      i += 1;
    } else {
      steps.push("added");
      j += 1;
    }
  }
  return steps;
}

/** Beyond this many old and new line pairs in one run, no line is paired. */
const PAIR_LIMIT = 2_500;
/** Share of words two lines have in common, at least, to read as one line edited. */
const SIMILAR = 0.5;

/** Dice similarity of the words of two lines, 0 to 1. */
function similarity(left: string, right: string): number {
  const leftWords = left.match(TOKEN)?.filter((token) => token.trim() !== "") ?? [];
  const rightWords = right.match(TOKEN)?.filter((token) => token.trim() !== "") ?? [];
  const total = leftWords.length + rightWords.length;
  if (total === 0) return 0;
  const steps = tokenDiff(leftWords, rightWords);
  if (steps === null) return 0;
  return (2 * steps.filter((step) => step === "same").length) / total;
}

/**
 * Pairs of `[old index, new index]`, in order, matching old lines to the new
 * lines they resemble: the order-keeping pairing with the most similarity.
 */
function pairLines(oldLines: readonly string[], newLines: readonly string[]): [number, number][] {
  if (oldLines.length * newLines.length > PAIR_LIMIT) return [];
  const width = newLines.length + 1;
  const score = new Float64Array((oldLines.length + 1) * width);
  const at = (i: number, j: number) => score[i * width + j] ?? 0;
  const like = oldLines.map((oldLine) =>
    newLines.map((newLine) => {
      const value = similarity(oldLine, newLine);
      return value >= SIMILAR ? value : 0;
    }),
  );
  const likeness = (i: number, j: number) => like[i]?.[j] ?? 0;
  for (let i = oldLines.length - 1; i >= 0; i -= 1) {
    for (let j = newLines.length - 1; j >= 0; j -= 1) {
      const paired = likeness(i, j) > 0 ? likeness(i, j) + at(i + 1, j + 1) : 0;
      score[i * width + j] = Math.max(paired, at(i + 1, j), at(i, j + 1));
    }
  }
  const pairs: [number, number][] = [];
  let i = 0;
  let j = 0;
  while (i < oldLines.length && j < newLines.length) {
    if (likeness(i, j) > 0 && at(i, j) === likeness(i, j) + at(i + 1, j + 1)) {
      pairs.push([i, j]);
      i += 1;
      j += 1;
    } else if (at(i, j) === at(i + 1, j)) {
      i += 1;
    } else {
      j += 1;
    }
  }
  return pairs;
}

/**
 * What changed from `previous` to `text`: lines first, then, inside each changed
 * run, words of the lines edited in place. Whitespace-only changes are ignored.
 */
export function planChanges(previous: string, text: string): PlanChanges {
  const starts = lineStarts(text);
  const added: [number, number][] = [];
  const removals: Removal[] = [];
  const remove = (at: number, removed: string, lines: boolean) => {
    const trimmed = lines ? removed.replace(/^\n+|\n+$/g, "") : removed.trim();
    if (trimmed.trim() !== "") removals.push({ at, text: trimmed, lines });
  };

  const wholeLine = (line: number) => {
    const [start, end] = lineSpan(text, starts, line);
    if (text.slice(start, end).trim() !== "") added.push([start, end]);
  };

  /** Word diff of an old line against new line `line`, paired as its new version. */
  const words = (oldLine: string, line: number) => {
    const [start, end] = lineSpan(text, starts, line);
    const oldTokens = oldLine.match(TOKEN) ?? [];
    const newTokens = text.slice(start, end).match(TOKEN) ?? [];
    const steps = tokenDiff(oldTokens, newTokens);
    if (steps === null) {
      remove(start, oldLine, true);
      wholeLine(line);
      return;
    }
    let oldIndex = 0;
    let newIndex = 0;
    let offset = start;
    let gone: string[] = [];
    for (const step of steps) {
      if (step === "removed") {
        gone.push(oldTokens[oldIndex] ?? "");
        oldIndex += 1;
        continue;
      }
      remove(offset, gone.join(""), false);
      gone = [];
      const token = newTokens[newIndex] ?? "";
      if (step === "added" && token.trim() !== "") added.push([offset, offset + token.length]);
      if (step === "same") oldIndex += 1;
      newIndex += 1;
      offset += token.length;
    }
    remove(offset, gone.join(""), false);
  };

  /**
   * A changed run: old lines paired with the new lines they resemble get a word
   * diff; the other old lines are removed lines, the other new lines are new.
   */
  const run = (removed: readonly string[], first: number, count: number) => {
    const newLines = Array.from({ length: count }, (_, index) =>
      text.slice(...lineSpan(text, starts, first + index)),
    );
    let oldNext = 0;
    let newNext = 0;
    const end: [number, number] = [removed.length, count];
    for (const [oldIndex, newIndex] of [...pairLines(removed, newLines), end]) {
      const at = lineSpan(text, starts, first + newNext)[0];
      remove(at, removed.slice(oldNext, oldIndex).join("\n"), true);
      for (let index = newNext; index < newIndex; index += 1) wholeLine(first + index);
      const oldLine = removed[oldIndex];
      if (oldLine !== undefined) words(oldLine, first + newIndex);
      oldNext = oldIndex + 1;
      newNext = newIndex + 1;
    }
  };

  let line = 0;
  let removed: string[] = [];
  let first = 0;
  let count = 0;
  const flush = () => {
    if (removed.length > 0 || count > 0) run(removed, first, count);
    removed = [];
    count = 0;
  };
  for (const entry of lineDiff(previous, text)) {
    if (entry.kind === "context") {
      flush();
      line += 1;
      continue;
    }
    if (removed.length === 0 && count === 0) first = line;
    if (entry.kind === "removed") {
      removed.push(entry.text);
    } else {
      count += 1;
      line += 1;
    }
  }
  flush();

  // Words a space apart read as one change.
  const merged: [number, number][] = [];
  for (const range of added.sort((left, right) => left[0] - right[0])) {
    const last = merged[merged.length - 1];
    if (last !== undefined && text.slice(last[1], range[0]).trim() === "") {
      last[1] = Math.max(last[1], range[1]);
    } else {
      merged.push([range[0], range[1]]);
    }
  }
  return { added: merged, removals };
}

/** The mdast shape this module reads; anything else is left alone. */
interface MdNode {
  type: string;
  value?: unknown;
  children?: MdNode[];
  position?: { start: { line: number; offset?: number }; end: { line: number; offset?: number } };
  data?: Record<string, unknown>;
}

function isNode(raw: unknown): raw is MdNode {
  return isRecord(raw) && typeof raw["type"] === "string";
}

function offsets(node: MdNode): [number, number] | null {
  const start = node.position?.start.offset;
  const end = node.position?.end.offset;
  return start === undefined || end === undefined ? null : [start, end];
}

function mark(type: string, tag: string, className: string, index?: number): MdNode {
  const properties: Record<string, unknown> = { className: [className] };
  if (index !== undefined) properties["dataRemoved"] = index;
  return { type, data: { hName: tag, hProperties: properties }, children: [] };
}

const dot = (index: number) => mark("planDot", "span", DOT_CLASS, index);
const cut = (index: number) => mark("planCut", "div", CUT_CLASS, index);

function addedNode(child: MdNode): MdNode {
  return { ...mark("planAdded", "span", ADDED_CLASS), children: [child] };
}

/** Blocks holding inline content, where a removal inside them shows as a dot. */
const INLINE_HOLDERS = new Set(["paragraph", "heading", "tableCell"]);
/** Blocks whose children are blocks, searched for where a removed line goes. */
const CONTAINERS = new Set(["list", "listItem", "blockquote"]);

/**
 * A remark plugin placing plan changes: new words wrapped in `plan-added` spans,
 * removed words as `plan-dot` spans and removed lines as `plan-cut` divs, both
 * with `data-removed` the removal's index. A code block gets `data-added` (its
 * new line indexes) and `data-cuts` (JSON `[line, index, lines]` triples, a
 * removal before `line`, or at its end for words) for its renderer.
 */
export function remarkPlanChanges(text: string, changes: PlanChanges) {
  const starts = lineStarts(text);
  const overlaps = (start: number, end: number) =>
    changes.added.some(([from, to]) => from < end && to > start);
  const inside = (at: number, start: number, end: number) => at >= start && at <= end;

  return () => (tree: unknown) => {
    if (!isNode(tree)) return;
    const placed = new Set<number>();

    const words = (start: number, end: number) =>
      changes.removals
        .map((removal, index) => ({ removal, index }))
        .filter(
          ({ removal, index }) =>
            !removal.lines && !placed.has(index) && inside(removal.at, start, end),
        );

    const splitText = (node: MdNode): MdNode[] => {
      const span = offsets(node);
      if (span === null || typeof node.value !== "string") return [node];
      const [start, end] = span;
      const pending = words(start, end);
      if (text.slice(start, end) !== node.value) {
        for (const { index } of pending) placed.add(index);
        return [
          overlaps(start, end) ? addedNode(node) : node,
          ...pending.map(({ index }) => dot(index)),
        ];
      }
      const points = new Set([start, end]);
      for (const [from, to] of changes.added) {
        if (from > start && from < end) points.add(from);
        if (to > start && to < end) points.add(to);
      }
      for (const { removal } of pending) points.add(removal.at);
      const sorted = [...points].sort((left, right) => left - right);
      const pieces: MdNode[] = [];
      const dotsAt = (at: number) => {
        for (const { removal, index } of pending) {
          if (removal.at === at && !placed.has(index)) {
            placed.add(index);
            pieces.push(dot(index));
          }
        }
      };
      for (let index = 0; index + 1 < sorted.length; index += 1) {
        const from = sorted[index] ?? start;
        const to = sorted[index + 1] ?? end;
        dotsAt(from);
        const piece: MdNode = { type: "text", value: text.slice(from, to) };
        pieces.push(overlaps(from, to) ? addedNode(piece) : piece);
      }
      dotsAt(end);
      return pieces;
    };

    const markCode = (node: MdNode) => {
      const span = offsets(node);
      if (span === null || node.position === undefined || typeof node.value !== "string") return;
      const startLine = node.position.start.line - 1;
      const opening = text.slice(...lineSpan(text, starts, startLine)).trimStart();
      const firstLine = /^(```|~~~)/.test(opening) ? startLine + 1 : startLine;
      const count = node.value === "" ? 0 : node.value.split("\n").length;
      const addedLines: number[] = [];
      const cuts: [number, number, boolean][] = [];
      for (let line = 0; line < count; line += 1) {
        const [from, to] = lineSpan(text, starts, firstLine + line);
        if (to > from && overlaps(from, to)) addedLines.push(line);
        changes.removals.forEach((removal, index) => {
          if (placed.has(index)) return;
          const before = removal.lines && removal.at === from;
          const within = !removal.lines && inside(removal.at, from, to);
          if (before || within) {
            placed.add(index);
            cuts.push([line, index, removal.lines]);
          }
        });
      }
      if (addedLines.length === 0 && cuts.length === 0) return;
      node.data = {
        ...node.data,
        hProperties: { dataAdded: addedLines.join(","), dataCuts: JSON.stringify(cuts) },
      };
    };

    const walk = (node: MdNode) => {
      if (node.type === "code") {
        markCode(node);
        return;
      }
      if (node.children === undefined) return;
      node.children = node.children.flatMap((child) => {
        if (child.type === "text") return splitText(child);
        if (child.type === "inlineCode") {
          const span = offsets(child);
          if (span === null) return [child];
          const pending = words(...span);
          for (const { index } of pending) placed.add(index);
          return [
            overlaps(...span) ? addedNode(child) : child,
            ...pending.map(({ index }) => dot(index)),
          ];
        }
        walk(child);
        return [child];
      });
    };
    walk(tree);

    /** Puts removal `index` in the flow content `children` of a `parent`. */
    const place = (parent: MdNode, at: number, index: number, lines: boolean) => {
      const children = parent.children ?? [];
      parent.children = children;
      for (const child of children) {
        const span = offsets(child);
        if (span === null) continue;
        const [start, end] = span;
        if (at <= start) {
          if (parent.type === "list") {
            child.children = [cut(index), ...(child.children ?? [])];
          } else {
            children.splice(children.indexOf(child), 0, lines ? cut(index) : dot(index));
          }
          return;
        }
        if (at >= end) continue;
        if (CONTAINERS.has(child.type)) {
          place(child, at, index, lines);
        } else if (INLINE_HOLDERS.has(child.type)) {
          child.children = [...(child.children ?? []), dot(index)];
        } else {
          children.splice(children.indexOf(child) + 1, 0, cut(index));
        }
        return;
      }
      const last = children[children.length - 1];
      if (parent.type === "list" && last !== undefined) {
        last.children = [...(last.children ?? []), cut(index)];
      } else {
        children.push(cut(index));
      }
    };
    changes.removals.forEach((removal, index) => {
      if (!placed.has(index)) place(tree, removal.at, index, removal.lines);
    });
  };
}
