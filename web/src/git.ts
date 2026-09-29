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

function parseChangedFile(item: unknown, at: string): ChangedFile {
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
}

export function parseRepoDiff(raw: unknown, label: string): RepoDiff {
  const value = record(raw, label);
  return {
    root: str(value, "root", label),
    files: list(value, "files", label).map((item, index) =>
      parseChangedFile(item, `${label}.files[${index}]`),
    ),
  };
}

export type StatusKind = "modified" | "added" | "deleted";

export interface RepoStatus {
  root: string | null;
  changes: { path: string; kind: StatusKind }[];
}

export function parseRepoStatus(raw: unknown, label: string): RepoStatus {
  const value = record(raw, label);
  return {
    root: optStr(value, "root", label),
    changes: list(value, "changes", label).map((item, index) => {
      const at = `${label}.changes[${index}]`;
      const change = record(item, at);
      const kind = str(change, "kind", at);
      if (kind !== "modified" && kind !== "added" && kind !== "deleted") {
        throw new Error(`${at}.kind: unknown: ${JSON.stringify(kind)}`);
      }
      return { path: str(change, "path", at), kind };
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

/** A file's pending changes and its repository, null when it has none or is outside one. */
export async function fetchFileDiff(
  path: string,
): Promise<{ root: string; file: ChangedFile } | null> {
  const response = await fetch(`api/git/file?path=${encodeURIComponent(path)}`);
  await check(response);
  const body = record(await response.json(), "file diff");
  const root = optStr(body, "root", "file diff");
  const diff = body["diff"];
  if (root === null || diff === null || diff === undefined) return null;
  return { root, file: parseChangedFile(diff, "file diff.diff") };
}

/** Changed paths of the repository containing `folder`. */
export async function fetchStatus(folder: string): Promise<RepoStatus> {
  const response = await fetch(`api/git/status?path=${encodeURIComponent(folder)}`);
  await check(response);
  return parseRepoStatus(await response.json(), "status");
}

export interface GutterMarks {
  added: ReadonlySet<number>;
  modified: ReadonlySet<number>;
  /** Lines with removed lines just above them; past the last line for a removal at the end. */
  removedAbove: ReadonlySet<number>;
}

/**
 * New-side line marks from a file's hunks. A change block (lines between two
 * context lines) with removals and additions marks its additions modified;
 * with additions only, added; with removals only, a rule above where they were.
 */
export function gutterMarks(file: ChangedFile): GutterMarks {
  const added = new Set<number>();
  const modified = new Set<number>();
  const removedAbove = new Set<number>();
  for (const hunk of file.hunks) {
    let removed = 0;
    let additions: number[] = [];
    let previousNew = 0;
    const flush = (nextNew: number) => {
      if (additions.length > 0) {
        for (const line of additions) (removed > 0 ? modified : added).add(line);
      } else if (removed > 0) {
        removedAbove.add(nextNew);
      }
      removed = 0;
      additions = [];
    };
    for (const line of hunk) {
      if (line.kind === "removed") {
        removed += 1;
      } else if (line.kind === "added") {
        if (line.new !== null) additions.push(line.new);
      } else {
        flush(line.new ?? previousNew + 1);
      }
      if (line.new !== null) previousNew = line.new;
    }
    flush(previousNew + 1);
  }
  return { added, modified, removedAbove };
}

export type RowMark = "added" | "modified";

export interface FolderMarks {
  /** Entry name to its mark: a file's own change, or a folder's summary. */
  rows: ReadonlyMap<string, RowMark>;
  /** Repository paths of deleted files directly in the folder. */
  deleted: string[];
}

/** Marks for the entries of `folder` from its repository's changes. */
export function folderMarks(status: RepoStatus, folder: string): FolderMarks {
  const rows = new Map<string, RowMark>();
  const deleted: string[] = [];
  if (status.root === null) return { rows, deleted };
  const windows = /^[A-Za-z]:/.test(status.root) || status.root.startsWith("\\\\");
  const fold = (text: string) => (windows ? text.toLowerCase() : text);
  const root = status.root.replace(/[\\/]+$/, "");
  if (!fold(folder).startsWith(fold(root))) return { rows, deleted };
  const rel = folder.slice(root.length).replace(/^[\\/]+|[\\/]+$/g, "").replaceAll("\\", "/");
  const prefix = rel === "" ? "" : `${rel}/`;
  const folders = new Map<string, boolean>();
  for (const change of status.changes) {
    if (!fold(change.path).startsWith(fold(prefix))) continue;
    const below = change.path.slice(prefix.length);
    const slash = below.indexOf("/");
    if (slash < 0) {
      if (change.kind === "deleted") deleted.push(change.path);
      else rows.set(below, change.kind);
      continue;
    }
    const name = below.slice(0, slash);
    // A folder stays green only while everything changed inside it is new.
    folders.set(name, (folders.get(name) ?? true) && change.kind === "added");
  }
  for (const [name, allAdded] of folders) {
    if (!rows.has(name)) rows.set(name, allAdded ? "added" : "modified");
  }
  return { rows, deleted };
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
