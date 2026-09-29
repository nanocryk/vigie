import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { DiffBlock } from "@/components/Diff";
import { folderName } from "@/format";
import {
  type ChangedFile,
  type FileStatus,
  type RepoDiff,
  absolutePath,
  baseName,
  blockDiff,
  dirName,
  fetchRepoDiff,
  groupByFolder,
  startsCollapsed,
} from "@/git";

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** The explorer's content width: every bar and body of its views lines up on it. */
export const PANEL = "mx-auto w-full max-w-5xl";

const TOOL =
  "voice-ui hover:bg-muted flex items-center gap-1.5 rounded-[5px] border px-2.5 py-1 text-xs disabled:opacity-50";

const STATUS: Record<FileStatus, { letter: string; className: string; label: string }> = {
  modified: { letter: "M", className: "text-git-modified", label: "Modified" },
  added: { letter: "A", className: "text-git-added", label: "Added" },
  deleted: { letter: "D", className: "text-git-deleted", label: "Deleted" },
  renamed: { letter: "R", className: "text-git-modified", label: "Renamed" },
};

function StatusMark({ status }: { status: FileStatus }) {
  const mark = STATUS[status];
  return (
    <span
      title={mark.label}
      className={`voice-code w-3 shrink-0 text-center text-xs font-bold ${mark.className}`}
    >
      {mark.letter}
    </span>
  );
}

function Counts({ file }: { file: Pick<ChangedFile, "added" | "removed"> }) {
  return (
    <span className="voice-code shrink-0 text-xs whitespace-nowrap">
      {file.added > 0 && <span className="text-git-added">+{file.added}</span>}
      {file.added > 0 && file.removed > 0 && " "}
      {file.removed > 0 && <span className="text-git-deleted">−{file.removed}</span>}
    </span>
  );
}

function fileId(path: string): string {
  return `diff-${encodeURIComponent(path)}`;
}

export interface DiffFocus {
  /** Relative to the repository root. */
  path: string;
  /** A new-side line, or null for the file's top. */
  line: number | null;
}

/** Outlines the row a jump lands on, for a moment. */
const FLASH = ["outline-2", "-outline-offset-2", "outline-git-modified"];
const FLASH_MS = 1600;

interface Kept {
  diff: RepoDiff;
  flipped: ReadonlyMap<string, boolean>;
  scroll: number;
}

/** Per tab and repository, so coming back from a file lands where the diff was left. */
const kept = new Map<string, Kept>();

/** Collapsed files: the automatic choice unless the user flipped it. */
function useCollapsed(files: readonly ChangedFile[], initial: ReadonlyMap<string, boolean>) {
  const [flipped, setFlipped] = useState<ReadonlyMap<string, boolean>>(initial);
  const collapsed = useCallback(
    (file: ChangedFile) => flipped.get(file.path) ?? startsCollapsed(file),
    [flipped],
  );
  const set = useCallback(
    (path: string, value: boolean) => setFlipped((current) => new Map(current).set(path, value)),
    [],
  );
  const setAll = (value: boolean) =>
    setFlipped(new Map(files.map((file) => [file.path, value])));
  return { collapsed, flipped, set, setAll };
}

function FileSection({
  file,
  collapsed,
  toggle,
  openFile,
  openFolder,
}: {
  file: ChangedFile;
  collapsed: boolean;
  toggle: () => void;
  /** Opens the file, at a line when given; null for a deleted file. */
  openFile: ((line: number | null) => void) | null;
  /** Opens the folder that held a deleted file. */
  openFolder: () => void;
}) {
  const block = useMemo(() => (collapsed ? null : blockDiff(file)), [collapsed, file]);
  const folder = dirName(file.path);
  return (
    <section id={fileId(file.path)} className="scroll-mt-2 rounded-[5px] border">
      <div className="bg-card sticky top-0 z-10 flex items-center gap-2 rounded-t-[5px] border-b px-2 py-1.5">
        <button
          type="button"
          aria-expanded={!collapsed}
          onClick={toggle}
          className="flex min-w-0 flex-1 items-center gap-2 text-left"
        >
          <span
            aria-hidden
            className={`text-muted-foreground w-3 shrink-0 text-xs transition-transform ${collapsed ? "" : "rotate-90"}`}
          >
            ▶
          </span>
          <StatusMark status={file.status} />
          <span className="voice-code min-w-0 flex-1 truncate text-xs">
            {folder !== "" && <span className="text-muted-foreground">{folder}/</span>}
            {baseName(file.path)}
            {file.oldPath !== null && (
              <span className="text-muted-foreground"> ← {file.oldPath}</span>
            )}
          </span>
          <Counts file={file} />
        </button>
        {openFile !== null && (
          <button
            type="button"
            onClick={() => openFile(null)}
            title="Open the file"
            className="text-muted-foreground hover:bg-muted hover:text-foreground voice-ui shrink-0 rounded-[5px] px-1.5 text-xs"
          >
            Open
          </button>
        )}
      </div>
      {!collapsed &&
        (file.note !== null ? (
          <p className="text-muted-foreground voice-ui px-3 py-2 text-xs">{file.note}</p>
        ) : block !== null && block.hunks.length > 0 ? (
          <DiffBlock
            diff={block}
            maxHeight=""
            {...(openFile === null ? {} : { onNumber: (line: number) => openFile(line) })}
            {...(file.status === "deleted" ? { onRemovedNumber: openFolder } : {})}
          />
        ) : (
          <p className="text-muted-foreground voice-ui px-3 py-2 text-xs">
            {file.status === "renamed"
              ? "Renamed, content unchanged."
              : file.status === "deleted"
                ? "Empty file deleted."
                : "No line changes."}
          </p>
        ))}
    </section>
  );
}

export function DiffView({
  keepKey,
  root,
  focus,
  close,
  openFile,
  openFolder,
}: {
  /** Identifies what to keep across remounts: the tab and the repository. */
  keepKey: string;
  root: string;
  /** A file, and a new-side line in it, to scroll to once loaded. */
  focus: DiffFocus | null;
  close: () => void;
  openFile: (path: string, line: number | null) => void;
  /** Leaves the diff for a folder. */
  openFolder: (path: string) => void;
}) {
  const [restore] = useState(() => kept.get(keepKey) ?? null);
  const [diff, setDiff] = useState<RepoDiff | null>(restore?.diff ?? null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const scroller = useRef<HTMLDivElement>(null);
  const scroll = useRef(restore?.scroll ?? 0);
  const files = useMemo(() => diff?.files ?? [], [diff]);
  const { collapsed, flipped, set, setAll } = useCollapsed(files, restore?.flipped ?? new Map());

  useLayoutEffect(() => {
    if (restore !== null && scroller.current !== null) scroller.current.scrollTop = restore.scroll;
  }, [restore]);

  const latest = useRef({ diff, flipped });
  useEffect(() => {
    latest.current = { diff, flipped };
  });
  useEffect(
    () => () => {
      const { diff: last, flipped: lastFlipped } = latest.current;
      if (last !== null) kept.set(keepKey, { diff: last, flipped: lastFlipped, scroll: scroll.current });
    },
    [keepKey],
  );

  const load = useCallback(() => {
    setLoading(true);
    fetchRepoDiff(root)
      .then((result) => {
        setDiff(result);
        setError(null);
      })
      .catch((err: unknown) => setError(errorMessage(err)))
      .finally(() => setLoading(false));
  }, [root]);

  useEffect(() => {
    load();
    const wake = () => {
      if (document.visibilityState === "visible") load();
    };
    document.addEventListener("visibilitychange", wake);
    window.addEventListener("pageshow", wake);
    return () => {
      document.removeEventListener("visibilitychange", wake);
      window.removeEventListener("pageshow", wake);
    };
  }, [load]);

  const totals = files.reduce(
    (sum, file) => ({ added: sum.added + file.added, removed: sum.removed + file.removed }),
    { added: 0, removed: 0 },
  );
  const allCollapsed = files.length > 0 && files.every(collapsed);
  const jump = (file: ChangedFile) => {
    set(file.path, false);
    requestAnimationFrame(() => {
      document.getElementById(fileId(file.path))?.scrollIntoView({ block: "start" });
    });
  };

  const focused = useRef(false);
  useEffect(() => {
    if (focused.current || focus === null || diff === null) return;
    focused.current = true;
    const target = diff.files.find((file) => file.path === focus.path);
    if (target === undefined) return;
    set(target.path, false);
    // Two frames: the expanded file renders before its rows are looked up.
    requestAnimationFrame(() =>
      requestAnimationFrame(() => {
        const section = document.getElementById(fileId(target.path));
        const row =
          focus.line === null ? null : section?.querySelector(`[data-new="${focus.line}"]`);
        if (row === null || row === undefined) {
          section?.scrollIntoView({ block: "start" });
          return;
        }
        row.scrollIntoView({ block: "center" });
        row.classList.add(...FLASH);
        setTimeout(() => row.classList.remove(...FLASH), FLASH_MS);
      }),
    );
  }, [diff, focus, set]);

  return (
    <>
      <div className="shrink-0 border-b px-3 py-1.5">
        <div className={`${PANEL} flex items-center gap-2`}>
          <button
            type="button"
            onClick={close}
            aria-label="Back to the folder"
            className="text-primary hover:bg-muted voice-ui shrink-0 rounded-[5px] px-2 py-1 text-sm"
          >
            &lsaquo; Folder
          </button>
          <span className="voice-ui min-w-0 flex-1 truncate text-sm font-medium" title={root}>
            Changes in {folderName(root)}
          </span>
          <button
            type="button"
            disabled={files.length === 0}
            onClick={() => setAll(!allCollapsed)}
            className={TOOL}
          >
            {allCollapsed ? "Expand all" : "Collapse all"}
          </button>
          <button type="button" disabled={loading} onClick={load} className={TOOL}>
            {loading ? "..." : "Refresh"}
          </button>
        </div>
      </div>
      <div
        ref={scroller}
        onScroll={(event) => {
          scroll.current = event.currentTarget.scrollTop;
        }}
        className="min-h-0 flex-1 overflow-y-auto overscroll-contain"
      >
        <div className={`${PANEL} space-y-3 p-3`}>
          {error !== null && <p className="text-destructive voice-ui text-sm">{error}</p>}
          {diff === null && error === null && (
            <p className="text-muted-foreground voice-ui text-sm">Loading...</p>
          )}
          {diff !== null && files.length === 0 && (
            <p className="text-muted-foreground voice-ui py-8 text-center text-sm">
              No pending changes.
            </p>
          )}
          {files.length > 0 && (
            <>
              <div className="voice-ui flex items-center gap-2 px-1 text-sm">
                <span>
                  {files.length} {files.length === 1 ? "file" : "files"}
                </span>
                <Counts file={totals} />
              </div>
              <nav className="bg-card overflow-hidden rounded-[5px] border">
                {groupByFolder(files).map((group) => (
                  <div key={group.folder} className="border-t first:border-t-0">
                    <p className="text-muted-foreground voice-code bg-muted/40 px-3 py-1 text-[11px]">
                      {group.folder === "" ? "./" : `${group.folder}/`}
                    </p>
                    <ul>
                      {group.files.map((file) => (
                        <li key={file.path}>
                          <button
                            type="button"
                            onClick={() => jump(file)}
                            className="hover:bg-muted flex w-full items-center gap-2 px-3 py-1.5 text-left"
                          >
                            <StatusMark status={file.status} />
                            <span
                              className={`voice-code min-w-0 flex-1 truncate text-xs ${file.status === "deleted" ? "text-muted-foreground line-through" : ""}`}
                            >
                              {baseName(file.path)}
                            </span>
                            <Counts file={file} />
                          </button>
                        </li>
                      ))}
                    </ul>
                  </div>
                ))}
              </nav>
              {files.map((file) => (
                <FileSection
                  key={file.path}
                  file={file}
                  collapsed={collapsed(file)}
                  toggle={() => set(file.path, !collapsed(file))}
                  openFile={
                    file.status === "deleted"
                      ? null
                      : (line) => openFile(absolutePath(diff?.root ?? root, file.path), line)
                  }
                  openFolder={() => {
                    const base = diff?.root ?? root;
                    const folder = dirName(file.path);
                    openFolder(folder === "" ? base : absolutePath(base, folder));
                  }}
                />
              ))}
            </>
          )}
        </div>
      </div>
    </>
  );
}
