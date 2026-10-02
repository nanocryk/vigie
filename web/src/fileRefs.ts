import { useEffect, useState } from "react";

import { fetchPathKinds, isWindowsPath, joinPath, KINDS_MAX_PATHS, type PathKind } from "@/files";

/** A path named in a message, with the line it points at. */
export interface FileRef {
  path: string;
  line: number | null;
}

/** `:12`, `:12:5` or `:12-20`; the first number is the line. */
const LINE_SUFFIX = /:(\d+)(?:[:-]\d+)?$/;
const NOT_A_PATH = /[`"'<>|*?\n\t]|:\/\//;
const ABSOLUTE = /^(?:[A-Za-z]:[\\/]|\\\\|\/)/;
const EXTENSION = /\.[A-Za-z][A-Za-z0-9_-]*$/;
const MAX_LENGTH = 500;

/**
 * The path an inline code span holds as its whole text, or null. A relative
 * path needs a separator or an extension, and no spaces, to tell it from code.
 */
export function parseFileRef(code: string): FileRef | null {
  const text = code.trim();
  if (
    text.length < 2 ||
    text.length > MAX_LENGTH ||
    text.startsWith("-") ||
    NOT_A_PATH.test(text)
  ) {
    return null;
  }
  const suffix = LINE_SUFFIX.exec(text);
  const path = suffix === null ? text : text.slice(0, suffix.index);
  const line = suffix === null ? 0 : Number(suffix[1]);
  if (path === "") return null;
  if (!ABSOLUTE.test(path)) {
    if (/\s/.test(path)) return null;
    if (!/[\\/]/.test(path) && !EXTENSION.test(path)) return null;
  }
  return { path, line: line > 0 ? line : null };
}

/** `path` made absolute against `cwd`; null for a relative path without one. */
export function resolveRefPath(path: string, cwd: string | null): string | null {
  if (ABSOLUTE.test(path)) return path;
  if (cwd === null) return null;
  const relative = path.replace(/^\.[\\/]/, "");
  return joinPath(cwd, isWindowsPath(cwd) ? relative.replaceAll("/", "\\") : relative);
}

/** How long a lookup answer is trusted: a missing path may appear soon. */
const FOUND_TTL_MS = 5 * 60_000;
const MISSING_TTL_MS = 30_000;

type FetchKinds = (paths: readonly string[]) => Promise<(PathKind | null)[]>;

/**
 * Path lookups gathered over one task tick into requests of at most
 * `KINDS_MAX_PATHS`, answers cached for a while.
 */
export function createKindLookup(fetchKinds: FetchKinds, now: () => number = Date.now) {
  const known = new Map<string, { kind: PathKind | null; at: number }>();
  // Paths not yet sent, then sent and unanswered; each with its callers.
  const waiting = new Map<string, ((kind: PathKind | null) => void)[]>();
  const sent = new Map<string, ((kind: PathKind | null) => void)[]>();
  let scheduled = false;

  const settle = (path: string, kind: PathKind | null) => {
    const resolvers = sent.get(path) ?? [];
    sent.delete(path);
    for (const resolve of resolvers) resolve(kind);
  };

  const flush = async () => {
    scheduled = false;
    const paths = [...waiting.keys()];
    for (const [path, resolvers] of waiting) sent.set(path, resolvers);
    waiting.clear();
    for (let start = 0; start < paths.length; start += KINDS_MAX_PATHS) {
      const chunk = paths.slice(start, start + KINDS_MAX_PATHS);
      try {
        const kinds = await fetchKinds(chunk);
        chunk.forEach((path, index) => {
          const kind = kinds[index] ?? null;
          known.set(path, { kind, at: now() });
          settle(path, kind);
        });
      } catch {
        for (const path of chunk) settle(path, null);
      }
    }
  };

  return (path: string): Promise<PathKind | null> => {
    const cached = known.get(path);
    if (cached !== undefined) {
      const ttl = cached.kind === null ? MISSING_TTL_MS : FOUND_TTL_MS;
      if (now() - cached.at < ttl) return Promise.resolve(cached.kind);
      known.delete(path);
    }
    return new Promise((resolve) => {
      const inFlight = sent.get(path);
      if (inFlight !== undefined) {
        inFlight.push(resolve);
        return;
      }
      const queued = waiting.get(path);
      if (queued === undefined) waiting.set(path, [resolve]);
      else queued.push(resolve);
      if (!scheduled) {
        scheduled = true;
        setTimeout(() => void flush(), 0);
      }
    });
  };
}

const lookupPathKind = createKindLookup(fetchPathKinds);

/** What `path` is on disk, null while unknown or when it does not exist. */
export function usePathKind(path: string | null): PathKind | null {
  const [found, setFound] = useState<{ path: string; kind: PathKind | null } | null>(null);
  useEffect(() => {
    if (path === null) return;
    let alive = true;
    void lookupPathKind(path).then((kind) => {
      if (alive) setFound({ path, kind });
    });
    return () => {
      alive = false;
    };
  }, [path]);
  return found !== null && found.path === path ? found.kind : null;
}
