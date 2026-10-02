import {
  type FormEvent,
  type ReactNode,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import type { Client } from "@/client";
import { type DiffFocus, DiffView, PANEL } from "@/components/DiffView";
import { Markdown } from "@/components/Markdown";
import { ConnectionDot } from "@/components/SessionList";
import { Header, HomeButton, NeedsYouColumn } from "@/components/TopBar";
import { useNow } from "@/components/useNow";
import { useSnapshot } from "@/components/useSnapshot";
import {
  type Entry,
  type ExplorerTab,
  type FileView,
  type Listing,
  type Place,
  type Places,
  activeTab,
  canGoUp,
  changeExplorer,
  closeTab,
  crumbs,
  explorerState,
  fetchFile,
  fetchListing,
  fetchPlaces,
  goUp,
  joinPath,
  makeFolder,
  newFreeTab,
  pruneSessionTabs,
  rawUrl,
  selectTab,
  sessionPlaces,
  updateTab,
  upSteps,
  useExplorer,
} from "@/files";
import { folderName, formatAgo, sessionTitle } from "@/format";
import {
  type FolderMarks,
  type GutterMarks,
  type RowMark,
  baseName,
  fetchFileDiff,
  fetchStatus,
  folderMarks,
  gutterMarks,
  repoRoot,
} from "@/git";
import { highlightLines, languageFromPath } from "@/highlight";
import { goHome, navigate, pushEntry, routeHash } from "@/route";
import type { SessionView } from "@/wire";

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Past this, text shows plain: highlighting a huge file stalls a phone. */
const HIGHLIGHT_MAX_CHARS = 256 * 1024;

const TOOL = "voice-ui hover:bg-muted flex items-center gap-1.5 rounded-[5px] border px-2.5 py-1.5 text-sm disabled:opacity-50";
const ROW = "hover:bg-muted flex w-full items-center gap-3 px-3 py-2.5 text-left";

// ---- Icons ----

function Icon({ children, className = "" }: { children: ReactNode; className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      className={`shrink-0 ${/\bsize-/.test(className) ? "" : "size-5"} ${className}`}
    >
      {children}
    </svg>
  );
}

export const FolderIcon = ({ className }: { className?: string }) => (
  <Icon className={className}>
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
  </Icon>
);
export const FileIcon = ({ className }: { className?: string }) => (
  <Icon className={className}>
    <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
    <path d="M14 3v5h5" />
  </Icon>
);
const ImageIcon = ({ className }: { className?: string }) => (
  <Icon className={className}>
    <rect x="3" y="4" width="18" height="16" rx="2" />
    <circle cx="9" cy="10" r="1.6" />
    <path d="m21 16-5-5-8 9" />
  </Icon>
);
const DriveIcon = ({ className }: { className?: string }) => (
  <Icon className={className}>
    <rect x="3" y="13" width="18" height="7" rx="2" />
    <path d="M5 13 7.5 5h9L19 13" />
    <path d="M7 16.5h.01" />
  </Icon>
);
const ChatIcon = ({ className }: { className?: string }) => (
  <Icon className={className}>
    <path d="M4 5h16v11H9l-5 4z" />
  </Icon>
);
const PlacesIcon = ({ className }: { className?: string }) => (
  <Icon className={className}>
    <path d="M12 21s-7-6.2-7-11.5a7 7 0 0 1 14 0C19 14.8 12 21 12 21z" />
    <circle cx="12" cy="9.5" r="2.5" />
  </Icon>
);
const PlusIcon = ({ className }: { className?: string }) => (
  <Icon className={className}>
    <path d="M12 5v14M5 12h14" />
  </Icon>
);
export const CloseIcon = ({ className }: { className?: string }) => (
  <Icon className={className}>
    <path d="M6 6l12 12M18 6 6 18" />
  </Icon>
);
export const PaperclipIcon = ({ className }: { className?: string }) => (
  <Icon className={className}>
    <path d="m20 11.5-8.2 8.2a5 5 0 0 1-7.1-7.1l8.5-8.5a3.3 3.3 0 0 1 4.7 4.7l-8.5 8.5a1.7 1.7 0 0 1-2.4-2.4l7.9-7.9" />
  </Icon>
);
export const ChevronIcon = ({ className }: { className?: string }) => (
  <Icon className={className}>
    <path d="m6 9 6 6 6-6" />
  </Icon>
);
export const DiffIcon = ({ className }: { className?: string }) => (
  <Icon className={className}>
    <path d="M12 4v7M8.5 7.5h7" />
    <path d="M8.5 17h7" />
    <path d="M5 21h14" />
  </Icon>
);
const EyeIcon = ({ className }: { className?: string }) => (
  <Icon className={className}>
    <path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12z" />
    <circle cx="12" cy="12" r="3" />
  </Icon>
);

const IMAGE_EXTENSIONS = new Set(["png", "jpg", "jpeg", "gif", "webp", "avif", "bmp", "ico"]);

function isImageName(name: string): boolean {
  const dot = name.lastIndexOf(".");
  return dot > 0 && IMAGE_EXTENSIONS.has(name.slice(dot + 1).toLowerCase());
}

function isMarkdown(path: string): boolean {
  return /\.(md|markdown)$/i.test(path);
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

// ---- Tabs ----

function tabTitle(tab: ExplorerTab, sessions: readonly SessionView[] | null): string {
  const owner = tab.owner;
  if (owner.kind === "session") {
    const session = sessions?.find((candidate) => candidate.id === owner.session);
    return session === undefined ? folderName(owner.root) : sessionTitle(session);
  }
  return tab.folder === null ? "Places" : folderName(tab.folder);
}

function tabDetail(tab: ExplorerTab): string {
  return tab.folder ?? "Drives and folders";
}

function TabIcon({ tab, className }: { tab: ExplorerTab; className?: string }) {
  if (tab.owner.kind === "session") return <ChatIcon className={className} />;
  return tab.folder === null ? (
    <PlacesIcon className={className} />
  ) : (
    <FolderIcon className={className} />
  );
}

function TabSwitcher({
  tabs,
  active,
  sessions,
}: {
  tabs: ExplorerTab[];
  active: ExplorerTab;
  sessions: readonly SessionView[] | null;
}) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onPointer = (event: PointerEvent) => {
      if (event.target instanceof Node && root.current?.contains(event.target) === true) return;
      setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", onPointer, { passive: true });
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const free = tabs.filter((tab) => tab.owner.kind === "free");
  const sessionTabs = tabs.filter((tab) => tab.owner.kind === "session");
  const entry = (tab: ExplorerTab, closable: boolean) => {
    const current = tab.id === active.id;
    return (
      <li key={tab.id} className="flex items-stretch">
        <button
          type="button"
          onClick={() => {
            changeExplorer((state) => selectTab(state, tab.id));
            setOpen(false);
          }}
          className={`hover:bg-muted flex min-w-0 flex-1 items-center gap-3 px-3 py-2.5 text-left ${current ? "bg-primary/10" : ""}`}
        >
          <TabIcon
            tab={tab}
            className={
              tab.owner.kind === "session"
                ? "text-mode-plan"
                : current
                  ? "text-primary"
                  : "text-muted-foreground"
            }
          />
          <span className="min-w-0 flex-1">
            <span className="voice-ui block truncate text-sm">{tabTitle(tab, sessions)}</span>
            <span className="text-muted-foreground voice-code block truncate text-xs">
              {tabDetail(tab)}
            </span>
          </span>
        </button>
        <button
          type="button"
          disabled={!closable}
          aria-label={`Close ${tabTitle(tab, sessions)}`}
          onClick={() => changeExplorer((state) => closeTab(state, tab.id))}
          className="text-muted-foreground hover:bg-muted hover:text-foreground px-3 disabled:invisible"
        >
          <CloseIcon className="size-4" />
        </button>
      </li>
    );
  };

  return (
    <div ref={root} className="relative min-w-0">
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
        className={`${TOOL} bg-card max-w-full`}
      >
        <TabIcon
          tab={active}
          className={active.owner.kind === "session" ? "text-mode-plan" : "text-primary"}
        />
        <span className="truncate">{tabTitle(active, sessions)}</span>
        {tabs.length > 1 && (
          <span className="bg-muted text-muted-foreground rounded-full px-1.5 text-[11px]">
            {tabs.length}
          </span>
        )}
        <ChevronIcon className="text-muted-foreground size-4" />
      </button>
      {open && (
        <div
          role="menu"
          className="bg-background absolute top-full left-0 z-30 mt-1 max-h-[70dvh] w-[min(22rem,calc(100vw-1.5rem))] overflow-y-auto overscroll-contain rounded-[5px] border shadow-lg"
        >
          <p className="voice-ui text-muted-foreground px-3 pt-2 pb-1 text-xs">Tabs</p>
          <ul>{free.map((tab) => entry(tab, free.length > 1))}</ul>
          {sessionTabs.length > 0 && (
            <>
              <p className="voice-ui text-muted-foreground border-t px-3 pt-2 pb-1 text-xs">
                Session tabs
              </p>
              <ul>{sessionTabs.map((tab) => entry(tab, true))}</ul>
            </>
          )}
          <button
            type="button"
            onClick={() => {
              changeExplorer(newFreeTab);
              setOpen(false);
            }}
            className="voice-ui text-primary hover:bg-muted flex w-full items-center gap-3 border-t px-3 py-2.5 text-sm"
          >
            <PlusIcon />
            New tab
          </button>
        </div>
      )}
    </div>
  );
}

// ---- Places ----

function PlaceRow({
  place,
  icon,
  detail,
  go,
}: {
  place: Place;
  icon: ReactNode;
  detail: string | null;
  go: (path: string) => void;
}) {
  return (
    <li>
      <button type="button" onClick={() => go(place.path)} className={ROW}>
        {icon}
        <span className="min-w-0 flex-1">
          <span className="voice-ui block truncate">{place.label}</span>
          {detail !== null && (
            <span className="text-muted-foreground voice-code block truncate text-xs">
              {detail}
            </span>
          )}
        </span>
      </button>
    </li>
  );
}

function PlacesSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="space-y-1">
      <h2 className="voice-ui text-muted-foreground px-1 text-xs tracking-wide uppercase">
        {title}
      </h2>
      <ul className="bg-card divide-y overflow-hidden rounded-[5px] border">{children}</ul>
    </section>
  );
}

function PlacesView({
  sessions,
  go,
}: {
  sessions: readonly SessionView[] | null;
  go: (path: string) => void;
}) {
  const [places, setPlaces] = useState<Places | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    fetchPlaces()
      .then(setPlaces)
      .catch((err: unknown) => setError(errorMessage(err)));
  }, []);
  const fromSessions = useMemo(() => sessionPlaces(sessions ?? []), [sessions]);

  return (
    <div className={`${PANEL} space-y-5 p-3`}>
      {error !== null && <p className="text-destructive voice-ui text-sm">{error}</p>}
      {places !== null && places.drives.length > 0 && (
        <section className="space-y-1">
          <h2 className="voice-ui text-muted-foreground px-1 text-xs tracking-wide uppercase">
            Drives
          </h2>
          <div className="flex flex-wrap gap-2">
            {places.drives.map((drive) => (
              <button
                key={drive.path}
                type="button"
                onClick={() => go(drive.path)}
                className="bg-card hover:bg-muted voice-ui flex items-center gap-2 rounded-[5px] border px-3 py-2.5"
              >
                <DriveIcon className="text-muted-foreground" />
                {drive.label}
              </button>
            ))}
          </div>
        </section>
      )}
      {fromSessions.length > 0 && (
        <PlacesSection title="Sessions">
          {fromSessions.map((place) => (
            <PlaceRow
              key={place.path}
              place={place}
              icon={<ChatIcon className="text-mode-plan" />}
              detail={place.path}
              go={go}
            />
          ))}
        </PlacesSection>
      )}
      {places !== null && places.folders.length > 0 && (
        <PlacesSection title="Folders">
          {places.folders.map((place) => (
            <PlaceRow
              key={place.path}
              place={place}
              icon={<FolderIcon className="text-primary" />}
              detail={place.path}
              go={go}
            />
          ))}
        </PlacesSection>
      )}
      {places === null && error === null && (
        <p className="text-muted-foreground voice-ui text-sm">Loading...</p>
      )}
    </div>
  );
}

// ---- Folder ----

function Breadcrumb({ folder, go }: { folder: string | null; go: (path: string | null) => void }) {
  const strip = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const element = strip.current;
    if (element !== null) element.scrollLeft = element.scrollWidth;
  }, [folder]);
  const segments = folder === null ? [] : crumbs(folder);
  return (
    <div
      ref={strip}
      className="voice-ui flex min-w-0 items-center gap-0.5 overflow-x-auto text-sm whitespace-nowrap [scrollbar-width:none]"
    >
      <button
        type="button"
        aria-label="Places"
        onClick={() => go(null)}
        className={`hover:bg-muted flex shrink-0 items-center rounded-[5px] p-1.5 ${folder === null ? "text-primary" : "text-muted-foreground"}`}
      >
        <PlacesIcon className="size-4" />
      </button>
      {segments.map((crumb, index) => {
        const last = index === segments.length - 1;
        return (
          <span key={crumb.path} className="flex shrink-0 items-center">
            <span className="text-muted-foreground/60 px-0.5">›</span>
            <button
              type="button"
              disabled={last}
              onClick={() => go(crumb.path)}
              className={`rounded-[5px] px-1.5 py-1 ${last ? "text-foreground font-medium" : "text-muted-foreground hover:bg-muted"}`}
            >
              {crumb.label}
            </button>
          </span>
        );
      })}
    </div>
  );
}

const ROW_MARK: Record<RowMark, string> = {
  added: "inset 3px 0 0 var(--color-git-added)",
  modified: "inset 3px 0 0 var(--color-git-modified)",
};

function EntryRow({
  entry,
  mark,
  now,
  open,
}: {
  entry: Entry;
  /** Pending git changes in this file, or inside this folder. */
  mark: RowMark | undefined;
  now: number;
  open: (entry: Entry) => void;
}) {
  const icon =
    entry.kind === "dir" ? (
      <FolderIcon className="text-primary" />
    ) : isImageName(entry.name) ? (
      <ImageIcon className="text-mode-accept" />
    ) : (
      <FileIcon className="text-muted-foreground" />
    );
  const meta = [
    entry.size === null ? null : formatSize(entry.size),
    entry.modified === null ? null : formatAgo(entry.modified, now),
  ].filter((part) => part !== null);
  return (
    <li>
      <button
        type="button"
        onClick={() => open(entry)}
        style={{ boxShadow: mark === undefined ? undefined : ROW_MARK[mark] }}
        className={`${ROW} ${entry.hidden ? "opacity-55" : ""}`}
      >
        {icon}
        <span className="voice-ui min-w-0 flex-1 truncate">{entry.name}</span>
        <span className="text-muted-foreground voice-ui shrink-0 text-xs">{meta.join(" · ")}</span>
      </button>
    </li>
  );
}

function NewFolderRow({
  parent,
  done,
}: {
  parent: string;
  done: (created: string | null) => void;
}) {
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    makeFolder(parent, name)
      .then(done)
      .catch((err: unknown) => {
        setError(errorMessage(err));
        setBusy(false);
      });
  };
  return (
    <li className="bg-primary/5 px-3 py-2">
      <form onSubmit={submit} className="flex items-center gap-2">
        <FolderIcon className="text-primary" />
        <input
          autoFocus
          value={name}
          disabled={busy}
          onChange={(event) => setName(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") done(null);
          }}
          placeholder="Folder name"
          aria-label="New folder name"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          className="voice-ui bg-background focus:ring-ring min-w-0 flex-1 rounded-[5px] border px-2 py-1.5 text-base outline-none focus:ring-2"
        />
        <button
          type="submit"
          disabled={busy || name.trim() === ""}
          className="voice-ui bg-primary text-primary-foreground rounded-[5px] px-3 py-1.5 text-sm disabled:opacity-50"
        >
          Create
        </button>
        <button
          type="button"
          aria-label="Cancel"
          onClick={() => done(null)}
          className="text-muted-foreground hover:bg-muted rounded-[5px] p-1.5"
        >
          <CloseIcon className="size-4" />
        </button>
      </form>
      {error !== null && <p className="text-destructive voice-ui mt-1 pl-8 text-xs">{error}</p>}
    </li>
  );
}

function FolderView({
  tab,
  showHidden,
  creating,
  onCreated,
  go,
  openFile,
  openDiff,
}: {
  tab: ExplorerTab & { folder: string };
  showHidden: boolean;
  creating: boolean;
  onCreated: () => void;
  go: (path: string | null) => void;
  openFile: (path: string) => void;
  /** Opens the diff of `root`, scrolled to `focus`. */
  openDiff: (root: string, focus: DiffFocus) => void;
}) {
  const [listing, setListing] = useState<Listing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const [marks, setMarks] = useState<(FolderMarks & { root: string }) | null>(null);

  useEffect(() => {
    let alive = true;
    const load = () => {
      fetchStatus(tab.folder)
        .then((status) => {
          if (!alive) return;
          setMarks(
            status.root === null
              ? null
              : { ...folderMarks(status, tab.folder), root: status.root },
          );
        })
        .catch(() => {});
    };
    load();
    const wake = () => {
      if (document.visibilityState === "visible") load();
    };
    document.addEventListener("visibilitychange", wake);
    return () => {
      alive = false;
      document.removeEventListener("visibilitychange", wake);
    };
  }, [tab.folder, reload]);
  const now = useNow();
  const scroller = useRef<HTMLDivElement>(null);
  const scroll = useRef(tab.scroll);
  const restored = useRef(false);

  useEffect(() => {
    let alive = true;
    setError(null);
    fetchListing(tab.folder)
      .then((result) => {
        if (alive) setListing(result);
      })
      .catch((err: unknown) => {
        if (alive) setError(errorMessage(err));
      });
    return () => {
      alive = false;
    };
  }, [tab.folder, reload]);

  // Back to where the list was, once, when it first shows.
  useLayoutEffect(() => {
    if (listing === null || restored.current) return;
    restored.current = true;
    if (scroller.current !== null) scroller.current.scrollTop = scroll.current;
  }, [listing]);

  // The scroll position is kept in the tab when the view goes away or the page hides.
  const { id, folder } = tab;
  useEffect(() => {
    const keep = () =>
      changeExplorer((state) =>
        state.tabs.some((candidate) => candidate.id === id && candidate.folder === folder)
          ? updateTab(state, id, { scroll: scroll.current })
          : state,
      );
    const onHide = () => {
      if (document.visibilityState === "hidden") keep();
    };
    document.addEventListener("visibilitychange", onHide);
    return () => {
      document.removeEventListener("visibilitychange", onHide);
      keep();
    };
  }, [id, folder]);

  const shown = (listing?.entries ?? []).filter((entry) => showHidden || !entry.hidden);
  const hiddenCount = (listing?.entries.length ?? 0) - shown.length;
  const open = (entry: Entry) => {
    if (listing === null) return;
    const path = joinPath(listing.path, entry.name);
    if (entry.kind === "dir") go(path);
    else openFile(path);
  };

  return (
    <div
      ref={scroller}
      onScroll={(event) => {
        scroll.current = event.currentTarget.scrollTop;
      }}
      className="min-h-0 flex-1 overflow-y-auto overscroll-contain"
    >
      <div className={`${PANEL} p-3`}>
        {error !== null && (
          <div className="space-y-2 rounded-[5px] border p-3">
            <p className="text-destructive voice-ui text-sm">{error}</p>
            <div className="flex gap-2">
              <button type="button" onClick={() => setReload((count) => count + 1)} className={TOOL}>
                Retry
              </button>
              <button type="button" onClick={() => go(null)} className={TOOL}>
                <PlacesIcon className="size-4" />
                Places
              </button>
            </div>
          </div>
        )}
        {listing === null && error === null && (
          <p className="text-muted-foreground voice-ui text-sm">Loading...</p>
        )}
        {listing !== null && error === null && (
          <ul className="bg-card divide-y overflow-hidden rounded-[5px] border">
            {creating && (
              <NewFolderRow
                parent={listing.path}
                done={(created) => {
                  onCreated();
                  if (created !== null) setReload((count) => count + 1);
                }}
              />
            )}
            {listing.parent !== null && (
              <li>
                <button
                  type="button"
                  onClick={() => go(listing.parent)}
                  className={`${ROW} text-muted-foreground`}
                >
                  <FolderIcon />
                  <span className="voice-ui">..</span>
                </button>
              </li>
            )}
            {shown.map((entry) => (
              <EntryRow
                key={entry.name}
                entry={entry}
                mark={marks?.rows.get(entry.name)}
                now={now}
                open={open}
              />
            ))}
            {marks?.deleted.map((path) => (
              <li key={`deleted:${path}`}>
                <button
                  type="button"
                  onClick={() => openDiff(marks.root, { path, line: null })}
                  title="Deleted: show in the diff"
                  style={{ boxShadow: "inset 3px 0 0 var(--color-git-deleted)" }}
                  className={`${ROW} bg-git-deleted/15 hover:bg-git-deleted/25`}
                >
                  <FileIcon className="text-git-deleted" />
                  <span className="voice-ui text-muted-foreground min-w-0 flex-1 truncate">
                    {baseName(path)}
                  </span>
                  <span className="text-git-deleted voice-ui shrink-0 text-xs">deleted</span>
                </button>
              </li>
            ))}
            {shown.length === 0 && (marks?.deleted.length ?? 0) === 0 && !creating && (
              <li className="text-muted-foreground voice-ui px-3 py-6 text-center text-sm">
                {hiddenCount > 0 ? `Only hidden items (${hiddenCount})` : "Empty folder"}
              </li>
            )}
          </ul>
        )}
        {listing !== null && hiddenCount > 0 && shown.length > 0 && (
          <p className="text-muted-foreground voice-ui px-1 pt-2 text-xs">
            {hiddenCount} hidden {hiddenCount === 1 ? "item" : "items"}
          </p>
        )}
      </div>
    </div>
  );
}

// ---- File viewer ----

const FLASH_MS = 1600;

/** The number cell's marks as inset shadows, so marked and plain cells keep one size. */
function markShadow(line: number, count: number, marks: GutterMarks | null): string | undefined {
  if (marks === null) return undefined;
  const shadows: string[] = [];
  if (marks.added.has(line)) shadows.push("inset 3px 0 0 var(--color-git-added)");
  if (marks.modified.has(line)) shadows.push("inset 3px 0 0 var(--color-git-modified)");
  if (marks.removedAbove.has(line)) shadows.push("inset 0 2px 0 var(--color-git-deleted)");
  if (line === count && marks.removedAbove.has(count + 1)) {
    shadows.push("inset 0 -2px 0 var(--color-git-deleted)");
  }
  return shadows.length === 0 ? undefined : shadows.join(", ");
}

function isMarked(line: number, count: number, marks: GutterMarks): boolean {
  return (
    marks.added.has(line) ||
    marks.modified.has(line) ||
    marks.removedAbove.has(line) ||
    (line === count && marks.removedAbove.has(count + 1))
  );
}

function TextBody({
  path,
  text,
  marks,
  target,
  onMark,
}: {
  path: string;
  text: string;
  /** Pending git changes to mark in the line-number column. */
  marks: GutterMarks | null;
  /** A line to bring into view and flash. */
  target: number | null;
  /** A marked line number was tapped. */
  onMark: (line: number) => void;
}) {
  const lines = useMemo(() => {
    const language = text.length <= HIGHLIGHT_MAX_CHARS ? languageFromPath(path) : null;
    return highlightLines(text, language) ?? null;
  }, [path, text]);
  const plain = useMemo(() => (lines === null ? text.split("\n") : null), [lines, text]);
  const count = lines?.length ?? plain?.length ?? 0;
  const body = useRef<HTMLDivElement>(null);
  const [flash, setFlash] = useState<number | null>(null);

  useLayoutEffect(() => {
    if (target === null) return;
    body.current?.querySelector(`[data-line="${target}"]`)?.scrollIntoView({ block: "center" });
    setFlash(target);
    const timer = setTimeout(() => setFlash(null), FLASH_MS);
    return () => clearTimeout(timer);
  }, [target]);

  return (
    <div ref={body} className="voice-code grid grid-cols-[auto_1fr] text-[13px] leading-5">
      {Array.from({ length: count }, (_, index) => {
        const number = index + 1;
        const lit = flash === number ? "bg-git-modified/25" : "";
        return (
          <div key={index} className="contents">
            {marks !== null && isMarked(number, count, marks) ? (
              <button
                type="button"
                data-line={number}
                onClick={() => onMark(number)}
                title="Show in the diff"
                style={{ boxShadow: markShadow(number, count, marks) }}
                className={`text-muted-foreground hover:text-primary cursor-pointer border-r pr-2 pl-3 text-right transition-colors duration-700 select-none hover:underline ${lit}`}
              >
                {number}
              </button>
            ) : (
              <span
                data-line={number}
                className={`text-muted-foreground/60 border-r pr-2 pl-3 text-right transition-colors duration-700 select-none ${lit}`}
              >
                {number}
              </span>
            )}
            {lines !== null ? (
              <span
                className={`pr-3 pl-3 break-all whitespace-pre-wrap transition-colors duration-700 ${lit}`}
                dangerouslySetInnerHTML={{ __html: lines[index] ?? "" }}
              />
            ) : (
              <span
                className={`pr-3 pl-3 break-all whitespace-pre-wrap transition-colors duration-700 ${lit}`}
              >
                {plain?.[index] ?? ""}
              </span>
            )}
          </div>
        );
      })}
    </div>
  );
}

function ImageBody({ path, size }: { path: string; size: number }) {
  const [dimensions, setDimensions] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  return (
    <div className="flex min-h-full flex-col items-center justify-center gap-2 p-3">
      {failed ? (
        <p className="text-muted-foreground voice-ui text-sm">Cannot display this image.</p>
      ) : (
        <img
          src={rawUrl(path)}
          alt={folderName(path)}
          onLoad={(event) =>
            setDimensions(`${event.currentTarget.naturalWidth} × ${event.currentTarget.naturalHeight}`)
          }
          onError={() => setFailed(true)}
          className="max-h-[70dvh] max-w-full rounded-[5px] border bg-[repeating-conic-gradient(#8881_0_25%,transparent_0_50%)] bg-[length:16px_16px] object-contain"
        />
      )}
      <p className="text-muted-foreground voice-ui text-xs">
        {[dimensions, formatSize(size)].filter((part) => part !== null).join(", ")}
      </p>
    </div>
  );
}

function FileViewer({
  path,
  line,
  backTo,
  close,
  onJumped,
  showInDiff,
}: {
  path: string;
  /** A line to bring into view and flash once shown. */
  line: number | null;
  /** Where closing leads: `Folder` or `Diff`. */
  backTo: string;
  close: () => void;
  /** The jump to `line` is taken; the tab stops asking for it. */
  onJumped: () => void;
  /** Opens the diff of `root` at the repository path `path`, line `line`. */
  showInDiff: (root: string, focus: DiffFocus) => void;
}) {
  const [view, setView] = useState<FileView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [target] = useState(line);
  // A line to reach shows the source, which has line numbers.
  const [raw, setRaw] = useState(line !== null);
  const [changes, setChanges] = useState<{ root: string; path: string; marks: GutterMarks } | null>(
    null,
  );

  const jumpedRef = useRef(onJumped);
  useEffect(() => {
    jumpedRef.current = onJumped;
  });
  useEffect(() => {
    if (target !== null) jumpedRef.current();
  }, [target]);

  useEffect(() => {
    let alive = true;
    setChanges(null);
    fetchFileDiff(path)
      .then((found) => {
        if (!alive) return;
        setChanges(
          found === null
            ? null
            : { root: found.root, path: found.file.path, marks: gutterMarks(found.file) },
        );
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [path]);

  useEffect(() => {
    let alive = true;
    setView(null);
    setError(null);
    fetchFile(path)
      .then((result) => {
        if (alive) setView(result);
      })
      .catch((err: unknown) => {
        if (alive) setError(errorMessage(err));
      });
    return () => {
      alive = false;
    };
  }, [path]);
  const markdown = isMarkdown(path) && view?.kind === "text";

  return (
    <>
      <div className="shrink-0 border-b px-3 py-1.5">
        <div className={`${PANEL} flex items-center gap-2`}>
          <button
            type="button"
            onClick={close}
            aria-label={`Back to the ${backTo.toLowerCase()}`}
            className="text-primary hover:bg-muted voice-ui shrink-0 rounded-[5px] px-2 py-1 text-sm"
          >
            &lsaquo; {backTo}
          </button>
          <span className="voice-ui min-w-0 flex-1 truncate text-sm font-medium" title={path}>
            {folderName(path)}
          </span>
          {markdown && (
            <div className="flex shrink-0 overflow-hidden rounded-[5px] border text-xs">
              {(["Rendered", "Raw"] as const).map((choice) => {
                const on = (choice === "Raw") === raw;
                return (
                  <button
                    key={choice}
                    type="button"
                    aria-pressed={on}
                    onClick={() => setRaw(choice === "Raw")}
                    className={`voice-ui px-2.5 py-1 ${on ? "bg-primary text-primary-foreground" : "hover:bg-muted"}`}
                  >
                    {choice}
                  </button>
                );
              })}
            </div>
          )}
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-auto overscroll-contain">
        <div className={PANEL}>
          {error !== null && <p className="text-destructive voice-ui p-3 text-sm">{error}</p>}
          {view === null && error === null && (
            <p className="text-muted-foreground voice-ui p-3 text-sm">Loading...</p>
          )}
          {view?.kind === "text" && view.truncated && (
            <p className="bg-gauge-glow/15 voice-ui px-3 py-1.5 text-xs">
              Showing the first 2 MB of {formatSize(view.size)}.
            </p>
          )}
          {view?.kind === "text" &&
            (markdown && !raw ? (
              <div className="p-4">
                <Markdown text={view.text} />
              </div>
            ) : (
              <div className="py-2">
                <TextBody
                  path={view.path}
                  text={view.text}
                  marks={changes?.marks ?? null}
                  target={target}
                  onMark={(line) => {
                    if (changes !== null) showInDiff(changes.root, { path: changes.path, line });
                  }}
                />
              </div>
            ))}
          {view?.kind === "image" && <ImageBody path={view.path} size={view.size} />}
          {view?.kind === "binary" && (
            <div className="text-muted-foreground voice-ui flex flex-col items-center gap-2 p-8 text-center text-sm">
              <FileIcon className="size-8" />
              <p>This file cannot be displayed.</p>
              <p className="text-xs">{formatSize(view.size)}</p>
            </div>
          )}
        </div>
      </div>
    </>
  );
}

// ---- Page ----

const UP_ENTRY = "vigie-explorer-up";
/** Marks an explorer entry whose back leaves the explorer until the user moves inside it. */
const BACK_LEAVES = "vigie-explorer-back-leaves";

/** Opens the explorer so that back returns to the screen before, as for an attachment. */
export function navigateBackLeaves(): void {
  pushEntry({ [BACK_LEAVES]: true }, routeHash({ kind: "files" }));
}

function entryBackLeaves(): boolean {
  const state: unknown = history.state;
  return typeof state === "object" && state !== null && Reflect.get(state, BACK_LEAVES) === true;
}

/** How many up entries stand on top of the explorer's own history entry. */
function upEntries(): number {
  const state: unknown = history.state;
  if (typeof state !== "object" || state === null) return 0;
  const count: unknown = Reflect.get(state, UP_ENTRY);
  return typeof count === "number" && Number.isInteger(count) && count > 0 ? count : 0;
}

/**
 * The system back button goes up a level: one history entry per level stands
 * on top of the explorer's own, each pop going up once. Entries are only added
 * right after a tap: Chrome skips on back an entry added without one, which
 * rules out putting an entry back from the pop itself. Returns the Home action,
 * whose own pops are not taken for going up.
 */
function useBackGoesUp(steps: number): () => void {
  const leaving = useRef(false);
  const backLeaves = useRef(entryBackLeaves());
  const arrival = useRef(steps);
  useEffect(() => {
    if (steps !== arrival.current) backLeaves.current = false;
    if (backLeaves.current) return;
    for (let count = upEntries(); count < steps; count++) {
      pushEntry({ [UP_ENTRY]: count + 1 });
    }
  }, [steps]);
  useEffect(() => {
    const onPop = () => {
      // A pop landing outside the explorer is the plain leave from Places.
      if (leaving.current || location.hash !== routeHash({ kind: "files" })) return;
      if (canGoUp(explorerState())) {
        changeExplorer(goUp);
        return;
      }
      // At Places with entries left over from jumping up by the breadcrumb.
      leaving.current = true;
      history.go(-1 - upEntries());
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);
  return () => {
    leaving.current = true;
    goHome();
  };
}

export function Explorer({ client }: { client: Client }) {
  const snapshot = useSnapshot(client);
  const state = useExplorer();
  const tab = activeTab(state);
  const [creating, setCreating] = useState(false);
  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);
  const leave = useBackGoesUp(upSteps(state));

  const sessions = snapshot.sessions;
  useEffect(() => {
    if (sessions === null) return;
    const live = new Set(sessions.filter((s) => s.archived_at === null).map((s) => s.id));
    changeExplorer((current) => pruneSessionTabs(current, live));
  }, [sessions]);

  useEffect(() => {
    setCreating(false);
    setStartError(null);
  }, [tab.id, tab.folder]);

  const [repo, setRepo] = useState<string | null>(null);
  useEffect(() => {
    setRepo(null);
    if (tab.folder === null) return;
    let alive = true;
    repoRoot(tab.folder)
      .then((root) => {
        if (alive) setRepo(root);
      })
      .catch(() => {
        if (alive) setRepo(null);
      });
    return () => {
      alive = false;
    };
  }, [tab.folder]);

  // Where the diff scrolls once open: a folder's pending deletion, or a file view's marked line.
  const [diffFocus, setDiffFocus] = useState<DiffFocus | null>(null);

  const go = (folder: string | null) =>
    changeExplorer((current) =>
      updateTab(current, tab.id, { folder, diff: null, file: null, line: null, scroll: 0 }),
    );
  const openFile = (file: string, line: number | null = null) => {
    setDiffFocus(null);
    changeExplorer((current) => updateTab(current, tab.id, { file, line }));
  };
  const closeFile = () =>
    changeExplorer((current) => updateTab(current, tab.id, { file: null, line: null }));
  const openDiff = (root: string, focus: DiffFocus | null = null) => {
    setDiffFocus(focus);
    changeExplorer((current) =>
      updateTab(current, tab.id, { diff: root, file: null, line: null }),
    );
  };
  const closeDiff = () =>
    changeExplorer((current) => updateTab(current, tab.id, { diff: null, file: null, line: null }));
  const view = tab.file !== null ? "file" : tab.diff !== null ? "diff" : "folder";

  const start = async (folder: string) => {
    setStarting(true);
    setStartError(null);
    const result = await client.createSession(folder);
    setStarting(false);
    if (result.ok) navigate({ kind: "session", id: result.session.id });
    else setStartError(result.error);
  };

  const folder = tab.folder;
  const owner = tab.owner.kind === "session" ? tab.owner : null;

  return (
    <>
      <Header
        left={<HomeButton sessions={snapshot.sessions} onClick={leave} />}
        right={<ConnectionDot status={snapshot.status} />}
        usage={snapshot.usage}
        column={<NeedsYouColumn sessions={snapshot.sessions} />}
        info={<span className="voice-heading">File explorer</span>}
      />
      <div className="shrink-0 space-y-2 border-b px-3 py-2">
        <div className={`${PANEL} flex items-center gap-2`}>
          <TabSwitcher tabs={state.tabs} active={tab} sessions={snapshot.sessions} />
          <div className="ml-auto flex shrink-0 gap-2">
            {/* Leftmost, so the buttons after it keep their places when it hides. */}
            {view === "folder" && (
              <button
                type="button"
                aria-pressed={state.showHidden}
                onClick={() =>
                  changeExplorer((current) => ({ ...current, showHidden: !current.showHidden }))
                }
                title={state.showHidden ? "Hide hidden files" : "Show hidden files"}
                className={`${TOOL} ${state.showHidden ? "border-primary text-primary bg-primary/10" : "text-muted-foreground"}`}
              >
                <EyeIcon className="size-4" />
                <span className="text-xs max-sm:hidden">Hidden</span>
              </button>
            )}
            {owner !== null && (
              <>
                <button
                  type="button"
                  onClick={() => navigate({ kind: "session", id: owner.session })}
                  title="Open the session's discussion"
                  className={TOOL}
                >
                  <ChatIcon className="text-mode-plan size-4" />
                  <span className="text-xs max-sm:hidden">Discussion</span>
                </button>
                <button
                  type="button"
                  onClick={() => go(owner.root)}
                  title="Back to the session's folder"
                  className={TOOL}
                >
                  <FolderIcon className="text-mode-plan size-4" />
                  <span className="text-xs max-sm:hidden">Project</span>
                </button>
              </>
            )}
            {(repo !== null || tab.diff !== null) && (
              <button
                type="button"
                aria-pressed={tab.diff !== null}
                onClick={() => (tab.diff !== null ? closeDiff() : repo !== null && openDiff(repo))}
                title={tab.diff !== null ? "Close the diff" : "Pending changes"}
                className={`${TOOL} ${tab.diff !== null ? "border-git-modified bg-git-modified/10" : ""}`}
              >
                <DiffIcon className="text-git-modified size-4" />
                <span className="text-xs max-sm:hidden">Diff</span>
              </button>
            )}
          </div>
        </div>
        {view === "folder" && (
          <div className={PANEL}>
            <Breadcrumb folder={folder} go={go} />
          </div>
        )}
      </div>
      {tab.file !== null ? (
        <FileViewer
          key={`${tab.id}:${tab.file}`}
          path={tab.file}
          line={tab.line}
          backTo={tab.diff !== null ? "Diff" : "Folder"}
          close={closeFile}
          onJumped={() => changeExplorer((current) => updateTab(current, tab.id, { line: null }))}
          showInDiff={openDiff}
        />
      ) : tab.diff !== null ? (
        <DiffView
          key={`${tab.id}:${tab.diff}`}
          keepKey={`${tab.id}:${tab.diff}`}
          root={tab.diff}
          focus={diffFocus}
          close={closeDiff}
          openFile={openFile}
          openFolder={go}
        />
      ) : (
        <>
          {folder === null ? (
            <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
              <PlacesView sessions={sessions} go={go} />
            </div>
          ) : (
            <FolderView
              key={`${tab.id}:${folder}`}
              tab={{ ...tab, folder }}
              showHidden={state.showHidden}
              creating={creating}
              onCreated={() => setCreating(false)}
              go={go}
              openFile={openFile}
              openDiff={openDiff}
            />
          )}
          {folder !== null && (
            <footer className="shrink-0 border-t px-3 pt-2 pb-[max(0.5rem,env(safe-area-inset-bottom))]">
              <div className={`${PANEL} space-y-1.5`}>
                {startError !== null && (
                  <p className="text-destructive voice-ui text-xs">{startError}</p>
                )}
                <div className="flex gap-2">
                  <button
                    type="button"
                    disabled={creating}
                    onClick={() => setCreating(true)}
                    className={`${TOOL} shrink-0 py-2`}
                  >
                    <PlusIcon className="size-4" />
                    New folder
                  </button>
                  <button
                    type="button"
                    disabled={starting}
                    onClick={() => void start(folder)}
                    className="bg-primary text-primary-foreground voice-ui flex min-w-0 flex-1 items-center justify-center gap-2 rounded-[5px] px-3 py-2 text-sm disabled:opacity-50"
                  >
                    <ChatIcon className="size-4" />
                    <span className="truncate">
                      {starting ? "Starting..." : `Start session in ${folderName(folder)}`}
                    </span>
                  </button>
                </div>
              </div>
            </footer>
          )}
        </>
      )}
    </>
  );
}
