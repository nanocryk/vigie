import type { FileDiff as BlockDiff, DiffLine } from "@/diff";
import { languageFromPath } from "@/highlight";

export type FileStatus = "modified" | "added" | "deleted" | "renamed";
export type ChangeKind = "context" | "added" | "removed";

export interface ChangeLine {
  kind: ChangeKind;
  text: string;
  old: number | null;
  new: number | null;
}

export interface ChangedFile {
  /** Relative to the repository root, `/`-separated; the old path for a deletion. */
  path: string;
  oldPath: string | null;
  status: FileStatus;
  added: number;
  removed: number;
  /** Why lines are not shown, when they are not. */
  note: string | null;
  hunks: ChangeLine[][];
}

export interface RepoDiff {
  root: string;
  files: ChangedFile[];
}

function isRecord(raw: unknown): raw is Record<string, unknown> {
  return typeof raw === "object" && raw !== null;
}

function record(raw: unknown, label: string): Record<string, unknown> {
  if (!isRecord(raw)) throw new Error(`${label}: expected an object: ${JSON.stringify(raw)}`);
  return raw;
}

function str(raw: Record<string, unknown>, key: string, label: string): string {
  const value = raw[key];
  if (typeof value !== "string") {
    throw new Error(`${label}.${key}: expected a string: ${JSON.stringify(value)}`);
  }
  return value;
}

function optStr(raw: Record<string, unknown>, key: string, label: string): string | null {
  const value = raw[key];
  if (value === null || value === undefined) return null;
  if (typeof value !== "string") {
    throw new Error(`${label}.${key}: expected a string or null: ${JSON.stringify(value)}`);
  }
  return value;
}

function count(raw: Record<string, unknown>, key: string, label: string): number {
  const value = raw[key];
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0) {
    throw new Error(`${label}.${key}: expected a count: ${JSON.stringify(value)}`);
  }
  return value;
}

function optLine(raw: Record<string, unknown>, key: string, label: string): number | null {
  const value = raw[key];
  if (value === null || value === undefined) return null;
  return count(raw, key, label);
}

function list(raw: Record<string, unknown>, key: string, label: string): unknown[] {
  const value = raw[key];
  if (!Array.isArray(value)) {
    throw new Error(`${label}.${key}: expected an array: ${JSON.stringify(value)}`);
  }
  return value;
}

const STATUSES: readonly string[] = ["modified", "added", "deleted", "renamed"];
const KINDS: readonly string[] = ["context", "added", "removed"];

function isStatus(value: string): value is FileStatus {
  return STATUSES.includes(value);
}

function isKind(value: string): value is ChangeKind {
  return KINDS.includes(value);
}

function parseLine(raw: unknown, label: string): ChangeLine {
  const line = record(raw, label);
  const kind = str(line, "kind", label);
  if (!isKind(kind)) throw new Error(`${label}.kind: unknown: ${JSON.stringify(kind)}`);
  return {
    kind,
    text: str(line, "text", label),
    old: optLine(line, "old", label),
    new: optLine(line, "new", label),
  };
}

export function parseRepoDiff(raw: unknown, label: string): RepoDiff {
  const value = record(raw, label);
  return {
    root: str(value, "root", label),
    files: list(value, "files", label).map((item, index) => {
      const at = `${label}.files[${index}]`;
      const file = record(item, at);
      const status = str(file, "status", at);
      if (!isStatus(status)) throw new Error(`${at}.status: unknown: ${JSON.stringify(status)}`);
      return {
        path: str(file, "path", at),
        oldPath: optStr(file, "old_path", at),
        status,
        added: count(file, "added", at),
        removed: count(file, "removed", at),
        note: optStr(file, "note", at),
        hunks: list(file, "hunks", at).map((hunk, h) =>
          list(record(hunk, `${at}.hunks[${h}]`), "lines", `${at}.hunks[${h}]`).map((line, l) =>
            parseLine(line, `${at}.hunks[${h}].lines[${l}]`),
          ),
        ),
      };
    }),
  };
}

async function check(response: Response): Promise<void> {
  if (response.ok) return;
  let code: string | null = null;
  try {
    const body: unknown = await response.json();
    if (isRecord(body) && typeof body["error"] === "string") code = body["error"];
  } catch {
    code = null;
  }
  if (code === "not_a_repo") throw new Error("Not inside a git repository");
  throw new Error(code ?? `HTTP ${response.status}`);
}

const roots = new Map<string, Promise<string | null>>();

/** The repository containing `path`, or null; remembered for the page's life. */
export function repoRoot(path: string): Promise<string | null> {
  const known = roots.get(path);
  if (known !== undefined) return known;
  const lookup = fetch(`api/git/root?path=${encodeURIComponent(path)}`)
    .then(async (response) => {
      await check(response);
      const root = record(await response.json(), "git root")["root"];
      return typeof root === "string" ? root : null;
    })
    .catch((err: unknown) => {
      roots.delete(path);
      throw err;
    });
  roots.set(path, lookup);
  return lookup;
}

export async function fetchRepoDiff(root: string): Promise<RepoDiff> {
  const response = await fetch(`api/git/diff?root=${encodeURIComponent(root)}`);
  await check(response);
  return parseRepoDiff(await response.json(), "diff");
}

const LOCKFILES = new Set([
  "package-lock.json",
  "yarn.lock",
  "pnpm-lock.yaml",
  "bun.lockb",
  "cargo.lock",
  "poetry.lock",
  "composer.lock",
  "gemfile.lock",
  "go.sum",
  "uv.lock",
]);

/** Lines past which a file's diff starts collapsed. */
export const COLLAPSE_LINES = 200;

export function baseName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

export function dirName(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash < 0 ? "" : path.slice(0, slash);
}

export function startsCollapsed(file: ChangedFile): boolean {
  return (
    file.added + file.removed > COLLAPSE_LINES || LOCKFILES.has(baseName(file.path).toLowerCase())
  );
}

/** Files grouped by folder, folders in path order, the root first. */
export function groupByFolder(files: readonly ChangedFile[]): { folder: string; files: ChangedFile[] }[] {
  const groups = new Map<string, ChangedFile[]>();
  for (const file of files) {
    const folder = dirName(file.path);
    const group = groups.get(folder);
    if (group === undefined) groups.set(folder, [file]);
    else group.push(file);
  }
  return [...groups.entries()]
    .sort(([left], [right]) => (left === "" ? -1 : right === "" ? 1 : left.localeCompare(right)))
    .map(([folder, grouped]) => ({ folder, files: grouped }));
}

/** A changed file in the shape `DiffBlock` renders: removed lines by old number, others by new. */
export function blockDiff(file: ChangedFile): BlockDiff {
  const hunks = file.hunks.map((hunk) =>
    hunk.map(
      (line): DiffLine => ({
        kind: line.kind,
        text: line.text,
        number: line.kind === "removed" ? line.old : line.new,
      }),
    ),
  );
  return {
    verb: file.status === "added" ? "Write" : "Update",
    path: file.path,
    language: languageFromPath(file.path),
    hunks,
    added: file.added,
    removed: file.removed,
  };
}

/** The path under `root` in the OS's form: backslashes for a Windows root. */
export function absolutePath(root: string, relative: string): string {
  const windows = /^[A-Za-z]:/.test(root) || root.startsWith("\\\\");
  const separator = windows ? "\\" : "/";
  const tail = windows ? relative.replaceAll("/", "\\") : relative;
  return root.endsWith(separator) ? `${root}${tail}` : `${root}${separator}${tail}`;
}
