import { useSyncExternalStore } from "react";

import { folderName } from "@/format";
import type { SessionView } from "@/wire";

// ---- Server responses ----

export interface Entry {
  name: string;
  kind: "dir" | "file";
  size: number | null;
  modified: string | null;
  hidden: boolean;
}

export interface Listing {
  path: string;
  parent: string | null;
  entries: Entry[];
}

export interface Place {
  label: string;
  path: string;
}

export interface Places {
  drives: Place[];
  folders: Place[];
}

export type FileContent =
  | { kind: "text"; text: string; truncated: boolean }
  | { kind: "image" }
  | { kind: "binary" };

export type FileView = { path: string; size: number; modified: string | null } & FileContent;

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

function num(raw: Record<string, unknown>, key: string, label: string): number {
  const value = raw[key];
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new Error(`${label}.${key}: expected a number: ${JSON.stringify(value)}`);
  }
  return value;
}

function bool(raw: Record<string, unknown>, key: string, label: string): boolean {
  const value = raw[key];
  if (typeof value !== "boolean") {
    throw new Error(`${label}.${key}: expected a boolean: ${JSON.stringify(value)}`);
  }
  return value;
}

function array(raw: Record<string, unknown>, key: string, label: string): unknown[] {
  const value = raw[key];
  if (!Array.isArray(value)) {
    throw new Error(`${label}.${key}: expected an array: ${JSON.stringify(value)}`);
  }
  return value;
}

export function parseListing(raw: unknown, label: string): Listing {
  const value = record(raw, label);
  return {
    path: str(value, "path", label),
    parent: optStr(value, "parent", label),
    entries: array(value, "entries", label).map((item, index) => {
      const at = `${label}.entries[${index}]`;
      const entry = record(item, at);
      const kind = str(entry, "kind", at);
      if (kind !== "dir" && kind !== "file") {
        throw new Error(`${at}.kind: expected dir or file: ${JSON.stringify(kind)}`);
      }
      const size = entry["size"];
      return {
        name: str(entry, "name", at),
        kind,
        size: size === null || size === undefined ? null : num(entry, "size", at),
        modified: optStr(entry, "modified", at),
        hidden: bool(entry, "hidden", at),
      };
    }),
  };
}

function parsePlaceList(raw: Record<string, unknown>, key: string, label: string): Place[] {
  return array(raw, key, label).map((item, index) => {
    const at = `${label}.${key}[${index}]`;
    const place = record(item, at);
    return { label: str(place, "label", at), path: str(place, "path", at) };
  });
}

export function parsePlaces(raw: unknown, label: string): Places {
  const value = record(raw, label);
  return {
    drives: parsePlaceList(value, "drives", label),
    folders: parsePlaceList(value, "folders", label),
  };
}

export function parseFileView(raw: unknown, label: string): FileView {
  const value = record(raw, label);
  const base = {
    path: str(value, "path", label),
    size: num(value, "size", label),
    modified: optStr(value, "modified", label),
  };
  const kind = str(value, "kind", label);
  switch (kind) {
    case "text":
      return {
        ...base,
        kind,
        text: str(value, "text", label),
        truncated: bool(value, "truncated", label),
      };
    case "image":
    case "binary":
      return { ...base, kind };
    default:
      throw new Error(`${label}.kind: expected text, image or binary: ${JSON.stringify(kind)}`);
  }
}

const ERROR_TEXT: Record<string, string> = {
  not_found: "Not found",
  not_a_directory: "Not a folder",
  not_a_file: "Not a file",
  permission_denied: "Access denied",
  already_exists: "Something with that name already exists",
  empty_name: "Give the folder a name",
  bad_name: "A name cannot contain / or \\, nor be . or ..",
};

async function check(response: Response): Promise<void> {
  if (response.ok) return;
  let code: string | null = null;
  try {
    const body: unknown = await response.json();
    if (isRecord(body) && typeof body["error"] === "string") code = body["error"];
  } catch {
    code = null;
  }
  throw new Error(
    code === null ? `HTTP ${response.status}` : (ERROR_TEXT[code] ?? `${code} (HTTP ${response.status})`),
  );
}

const query = (path: string) => `?path=${encodeURIComponent(path)}`;

export async function fetchListing(path: string): Promise<Listing> {
  const response = await fetch(`api/fs/list${query(path)}`);
  await check(response);
  return parseListing(await response.json(), "listing");
}

export async function fetchPlaces(): Promise<Places> {
  const response = await fetch("api/fs/places");
  await check(response);
  return parsePlaces(await response.json(), "places");
}

export async function fetchFile(path: string): Promise<FileView> {
  const response = await fetch(`api/fs/file${query(path)}`);
  await check(response);
  return parseFileView(await response.json(), "file");
}

export function rawUrl(path: string): string {
  return `api/fs/raw${query(path)}`;
}

/** Creates folder `name` in `parent` and returns its path. */
export async function makeFolder(parent: string, name: string): Promise<string> {
  const response = await fetch("api/fs/mkdir", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ parent, name }),
  });
  await check(response);
  return str(record(await response.json(), "mkdir"), "path", "mkdir");
}

// ---- Paths ----

function isWindowsPath(path: string): boolean {
  return /^[A-Za-z]:/.test(path) || path.startsWith("\\\\");
}

export function joinPath(dir: string, name: string): string {
  const separator = isWindowsPath(dir) ? "\\" : "/";
  return dir.endsWith(separator) ? `${dir}${name}` : `${dir}${separator}${name}`;
}

export interface Crumb {
  label: string;
  path: string;
}

/** One segment per folder from the root down, each with the path it opens. */
export function crumbs(path: string): Crumb[] {
  if (isWindowsPath(path)) {
    const unc = /^\\\\([^\\]+)\\([^\\]+)/.exec(path);
    const root = unc === null ? path.slice(0, 2) : unc[0];
    const rootPath = `${root}\\`;
    const parts = path.slice(root.length).split("\\").filter((part) => part !== "");
    const result: Crumb[] = [{ label: root, path: rootPath }];
    let current = rootPath;
    for (const part of parts) {
      current = joinPath(current, part);
      result.push({ label: part, path: current });
    }
    return result;
  }
  const result: Crumb[] = [{ label: "/", path: "/" }];
  let current = "/";
  for (const part of path.split("/").filter((segment) => segment !== "")) {
    current = joinPath(current, part);
    result.push({ label: part, path: current });
  }
  return result;
}

/** `path` is `root` or below it; case-insensitive for Windows paths. */
export function isInside(path: string, root: string): boolean {
  const windows = isWindowsPath(root);
  const fold = (text: string) => (windows ? text.toLowerCase() : text);
  const separator = windows ? "\\" : "/";
  const base = fold(root.endsWith(separator) ? root.slice(0, -1) : root);
  const target = fold(path);
  return target === base || target.startsWith(base + separator);
}

/** Folders of sessions not archived, most recently active first, each once. */
export function sessionPlaces(sessions: readonly SessionView[]): Place[] {
  const seen = new Set<string>();
  const places: Place[] = [];
  const live = sessions
    .filter((session) => session.archived_at === null)
    .sort((left, right) => Date.parse(right.updated_at) - Date.parse(left.updated_at));
  for (const session of live) {
    const key = isWindowsPath(session.cwd) ? session.cwd.toLowerCase() : session.cwd;
    if (seen.has(key)) continue;
    seen.add(key);
    places.push({ label: folderName(session.cwd), path: session.cwd });
  }
  return places;
}

// ---- Tabs ----

export type TabOwner = { kind: "free" } | { kind: "session"; session: string; root: string };

export interface ExplorerTab {
  id: string;
  owner: TabOwner;
  /** Null at Places. */
  folder: string | null;
  /** The repository root whose pending changes show over the folder. */
  diff: string | null;
  /** The open file, shown over the diff and the folder. */
  file: string | null;
  /** A line of the open file to bring into view and flash, once. */
  line: number | null;
  scroll: number;
}

export interface ExplorerState {
  tabs: ExplorerTab[];
  active: string;
  /** The free tab used last, which Home opens. */
  lastFree: string;
  showHidden: boolean;
}

let idCounter = 0;
function newId(): string {
  idCounter += 1;
  return `${Date.now().toString(36)}-${idCounter}`;
}

function freeTab(): ExplorerTab {
  return {
    id: newId(),
    owner: { kind: "free" },
    folder: null,
    diff: null,
    file: null,
    line: null,
    scroll: 0,
  };
}

export function initialState(): ExplorerState {
  const tab = freeTab();
  return { tabs: [tab], active: tab.id, lastFree: tab.id, showHidden: false };
}

function isFree(tab: ExplorerTab): boolean {
  return tab.owner.kind === "free";
}

export function activeTab(state: ExplorerState): ExplorerTab {
  const tab = state.tabs.find((candidate) => candidate.id === state.active) ?? state.tabs[0];
  if (tab === undefined) throw new Error("explorer state has no tab");
  return tab;
}

export function selectTab(state: ExplorerState, id: string): ExplorerState {
  const tab = state.tabs.find((candidate) => candidate.id === id);
  if (tab === undefined) return state;
  return { ...state, active: id, lastFree: isFree(tab) ? id : state.lastFree };
}

/** Activates the free tab used last, the one Home opens. */
export function selectLastFree(state: ExplorerState): ExplorerState {
  const last = state.tabs.find((tab) => tab.id === state.lastFree && isFree(tab));
  const fallback = last ?? state.tabs.find(isFree);
  return fallback === undefined ? state : selectTab(state, fallback.id);
}

/** A new free tab at the active tab's folder, activated. */
export function newFreeTab(state: ExplorerState): ExplorerState {
  const tab = { ...freeTab(), folder: activeTab(state).folder };
  return selectTab({ ...state, tabs: [...state.tabs, tab] }, tab.id);
}

/** Closes a tab; the last free tab stays. The neighbour of an active closed tab takes over. */
export function closeTab(state: ExplorerState, id: string): ExplorerState {
  const index = state.tabs.findIndex((tab) => tab.id === id);
  const tab = state.tabs[index];
  if (tab === undefined) return state;
  if (isFree(tab) && state.tabs.filter(isFree).length === 1) return state;
  const tabs = state.tabs.filter((candidate) => candidate.id !== id);
  let next = { ...state, tabs };
  if (state.active === id) {
    const neighbour = tabs[Math.min(index, tabs.length - 1)];
    if (neighbour !== undefined) next = { ...next, active: neighbour.id };
  }
  if (state.lastFree === id) {
    const free = tabs.find(isFree);
    if (free !== undefined) next = { ...next, lastFree: free.id };
  }
  return next;
}

export function updateTab(
  state: ExplorerState,
  id: string,
  patch: Partial<Pick<ExplorerTab, "folder" | "diff" | "file" | "line" | "scroll">>,
): ExplorerState {
  return {
    ...state,
    tabs: state.tabs.map((tab) => (tab.id === id ? { ...tab, ...patch } : tab)),
  };
}

/** Activates the session's tab, created at its root on first use. */
export function openSessionTab(state: ExplorerState, session: string, root: string): ExplorerState {
  const existing = state.tabs.find(
    (tab) => tab.owner.kind === "session" && tab.owner.session === session,
  );
  if (existing !== undefined) return selectTab(state, existing.id);
  const tab: ExplorerTab = {
    id: newId(),
    owner: { kind: "session", session, root },
    folder: root,
    diff: null,
    file: null,
    line: null,
    scroll: 0,
  };
  return selectTab({ ...state, tabs: [...state.tabs, tab] }, tab.id);
}

/** Activates the free tab used last on `file`, with its folder behind it for the back button. */
export function openFileInLastFree(state: ExplorerState, file: string): ExplorerState {
  const next = selectLastFree(state);
  return updateTab(next, next.active, { folder: parentFolder(file), diff: null, file, line: null, scroll: 0 });
}

function parentFolder(file: string): string | null {
  const path = crumbs(file);
  return path.length >= 2 ? (path[path.length - 2]?.path ?? null) : null;
}

/** Activates the session's tab on `file`, with its folder behind it for the back button. */
export function openSessionFile(
  state: ExplorerState,
  session: string,
  root: string,
  file: string,
): ExplorerState {
  const next = openSessionTab(state, session, root);
  return updateTab(next, next.active, { folder: parentFolder(file), diff: null, file, line: null, scroll: 0 });
}

/** Activates the session's tab on its folder view, the diff and any file closed. */
export function openSessionFolder(state: ExplorerState, session: string, root: string): ExplorerState {
  const next = openSessionTab(state, session, root);
  return updateTab(next, next.active, { diff: null, file: null, line: null });
}

/** Activates the session's tab on the pending changes of `repo`. */
export function openSessionDiff(
  state: ExplorerState,
  session: string,
  root: string,
  repo: string,
): ExplorerState {
  const next = openSessionTab(state, session, root);
  return updateTab(next, next.active, { diff: repo, file: null });
}

/** Drops the tabs of sessions not in `live` (archived or gone). */
export function pruneSessionTabs(state: ExplorerState, live: ReadonlySet<string>): ExplorerState {
  let next = state;
  for (const tab of state.tabs) {
    if (tab.owner.kind === "session" && !live.has(tab.owner.session)) next = closeTab(next, tab.id);
  }
  return next;
}

function parseOwner(raw: unknown): TabOwner | null {
  if (!isRecord(raw)) return null;
  if (raw["kind"] === "free") return { kind: "free" };
  const session = raw["session"];
  const root = raw["root"];
  if (raw["kind"] === "session" && typeof session === "string" && typeof root === "string") {
    return { kind: "session", session, root };
  }
  return null;
}

function parseTab(raw: unknown): ExplorerTab | null {
  if (!isRecord(raw)) return null;
  const { id, folder, diff, file, line, scroll } = raw;
  const owner = parseOwner(raw["owner"]);
  if (typeof id !== "string" || owner === null) return null;
  return {
    id,
    owner,
    folder: typeof folder === "string" ? folder : null,
    diff: typeof diff === "string" ? diff : null,
    file: typeof file === "string" ? file : null,
    line: typeof line === "number" && Number.isInteger(line) && line > 0 ? line : null,
    scroll: typeof scroll === "number" && Number.isFinite(scroll) ? scroll : 0,
  };
}

/** A stored state, or a fresh one when missing or malformed. */
export function parseStoredState(text: string | null): ExplorerState {
  if (text === null) return initialState();
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return initialState();
  }
  if (!isRecord(raw) || !Array.isArray(raw["tabs"])) return initialState();
  const tabs = raw["tabs"].map(parseTab).filter((tab): tab is ExplorerTab => tab !== null);
  const firstFree = tabs.find(isFree);
  if (firstFree === undefined) return initialState();
  const known = (id: unknown) => typeof id === "string" && tabs.some((tab) => tab.id === id);
  const active = known(raw["active"]) ? String(raw["active"]) : firstFree.id;
  const lastFree =
    known(raw["lastFree"]) && tabs.some((tab) => tab.id === raw["lastFree"] && isFree(tab))
      ? String(raw["lastFree"])
      : firstFree.id;
  return { tabs, active, lastFree, showHidden: raw["showHidden"] === true };
}

const STORAGE_KEY = "vigie2.explorer";
const SAVE_DELAY_MS = 300;

function load(): ExplorerState {
  try {
    return parseStoredState(localStorage.getItem(STORAGE_KEY));
  } catch {
    return initialState();
  }
}

let current: ExplorerState | null = null;
const listeners = new Set<() => void>();
let saveTimer: ReturnType<typeof setTimeout> | null = null;

function snapshot(): ExplorerState {
  current ??= load();
  return current;
}

function save(): void {
  saveTimer = null;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(snapshot()));
  } catch {
    // Storage full or disabled: the tabs last for this page only.
  }
}

/** Applies `change` to the explorer state, persisted per device. */
export function changeExplorer(change: (state: ExplorerState) => ExplorerState): void {
  const next = change(snapshot());
  if (next === current) return;
  current = next;
  if (saveTimer === null) saveTimer = setTimeout(save, SAVE_DELAY_MS);
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function explorerState(): ExplorerState {
  return snapshot();
}

/** One level up: the open file closes, else the diff, else the folder's parent, else Places. */
export function goUp(state: ExplorerState): ExplorerState {
  const tab = activeTab(state);
  if (tab.file !== null) return updateTab(state, tab.id, { file: null, line: null });
  if (tab.diff !== null) return updateTab(state, tab.id, { diff: null });
  if (tab.folder === null) return state;
  const path = crumbs(tab.folder);
  const parent = path.length >= 2 ? (path[path.length - 2]?.path ?? null) : null;
  return updateTab(state, tab.id, { folder: parent, scroll: 0 });
}

/** The active tab shows a folder, a diff or a file, so going up has somewhere to go. */
export function canGoUp(state: ExplorerState): boolean {
  return upSteps(state) > 0;
}

/** How many times going up leads somewhere: the file, the diff, then each folder to Places. */
export function upSteps(state: ExplorerState): number {
  const tab = activeTab(state);
  return (
    (tab.file === null ? 0 : 1) +
    (tab.diff === null ? 0 : 1) +
    (tab.folder === null ? 0 : crumbs(tab.folder).length)
  );
}

export function useExplorer(): ExplorerState {
  return useSyncExternalStore(subscribe, snapshot);
}
