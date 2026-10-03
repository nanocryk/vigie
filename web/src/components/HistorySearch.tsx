import { type FormEvent, useEffect, useRef, useState } from "react";

import type { Client } from "@/client";
import { Markdown } from "@/components/Markdown";
import { Header, HomeButton, NeedsYouColumn } from "@/components/TopBar";
import { useSnapshot } from "@/components/useSnapshot";
import { useNow } from "@/components/useNow";
import { formatStamp, sessionTitle } from "@/format";
import { navigate, replaceRoute, routeHash } from "@/route";
import { markWords, searchWords } from "@/search";
import {
  type Around,
  type SearchHit,
  type SearchPage,
  type SessionView,
  parseAround,
  parseSearchPage,
} from "@/wire";

/** Exchanges the preview adds per tap on Earlier or Later. */
const AROUND_STEP = 1;
/** Exchanges `api/history/around` gives at most on either side. */
const AROUND_MAX = 50;

async function getJson(url: string): Promise<unknown> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.json();
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function discussionTitle(
  sessions: readonly SessionView[] | null,
  id: string,
  fallback: Pick<SessionView, "name" | "cwd"> | null = null,
): string {
  const session = sessions?.find((candidate) => candidate.id === id) ?? fallback;
  return session === null ? "..." : sessionTitle(session);
}

function BackButton({ label }: { label: string }) {
  return (
    <button
      type="button"
      onClick={() => history.back()}
      className="mr-1 rounded-[5px] px-1 text-primary hover:bg-muted"
    >
      &lsaquo; {label}
    </button>
  );
}

function Marked({ text, words }: { text: string; words: readonly string[] }) {
  return (
    <>
      {markWords(text, words).map((run, index) =>
        run.marked ? (
          <mark key={index} className="rounded-[3px] bg-primary/25 text-foreground">
            {run.text}
          </mark>
        ) : (
          <span key={index}>{run.text}</span>
        ),
      )}
    </>
  );
}

/** The field searching the history: Enter runs `onSearch` with the trimmed query. */
export function SearchField({
  initial,
  placeholder,
  autoFocus = false,
  onSearch,
}: {
  initial: string;
  placeholder: string;
  autoFocus?: boolean;
  onSearch: (query: string) => void;
}) {
  const [draft, setDraft] = useState(initial);
  useEffect(() => setDraft(initial), [initial]);
  const field = useRef<HTMLInputElement>(null);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    const words = searchWords(draft);
    if (words.length === 0) return;
    field.current?.blur();
    onSearch(words.join(" "));
  };
  return (
    <form role="search" onSubmit={submit}>
      <input
        ref={field}
        type="search"
        enterKeyHint="search"
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
        placeholder={placeholder}
        autoFocus={autoFocus}
        className="w-full rounded-[5px] border bg-card px-3 py-2 text-sm voice-ui placeholder:text-muted-foreground focus:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      />
    </form>
  );
}

/** Opens the search of one discussion's history. */
export function SearchButton({ discussion }: { discussion: string }) {
  return (
    <button
      type="button"
      aria-label="Search this discussion"
      title="Search this discussion"
      onClick={() => navigate({ kind: "search", discussion, query: "" })}
      className="flex size-10 shrink-0 items-center justify-center rounded-[5px] text-muted-foreground hover:bg-muted hover:text-foreground"
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth={1.6}
        strokeLinecap="round"
        aria-hidden
        className="size-6"
      >
        <circle cx="11" cy="11" r="6" />
        <path d="m20 20-4.5-4.5" />
      </svg>
    </button>
  );
}

type Results =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "failed"; message: string }
  | { kind: "loaded"; page: SearchPage; more: "idle" | "loading" | "failed" };

/** The results last shown, so coming back from a preview keeps the pages loaded. */
let kept: { hash: string; page: SearchPage } | null = null;

function searchUrl(discussion: string | null, query: string, offset: number): string {
  const scope = discussion === null ? "" : `&discussion=${encodeURIComponent(discussion)}`;
  return `api/history/search?q=${encodeURIComponent(query)}${scope}&offset=${offset}`;
}

export function SearchScreen({
  client,
  discussion,
  query,
}: {
  client: Client;
  discussion: string | null;
  query: string;
}) {
  const snapshot = useSnapshot(client);
  const now = useNow();
  const hash = routeHash({ kind: "search", discussion, query });
  const [results, setResults] = useState<Results>(() =>
    kept?.hash === hash ? { kind: "loaded", page: kept.page, more: "idle" } : { kind: "idle" },
  );
  useEffect(() => {
    if (query === "") {
      setResults({ kind: "idle" });
      return;
    }
    if (kept?.hash === hash) {
      setResults({ kind: "loaded", page: kept.page, more: "idle" });
      return;
    }
    let live = true;
    setResults({ kind: "loading" });
    getJson(searchUrl(discussion, query, 0))
      .then((raw) => parseSearchPage(raw, "history search"))
      .then(
        (page) => {
          kept = { hash, page };
          if (live) setResults({ kind: "loaded", page, more: "idle" });
        },
        (err: unknown) => {
          if (live) setResults({ kind: "failed", message: errorText(err) });
        },
      );
    return () => {
      live = false;
    };
  }, [discussion, query, hash]);

  const loadMore = (page: SearchPage) => {
    setResults({ kind: "loaded", page, more: "loading" });
    getJson(searchUrl(discussion, query, page.hits.length))
      .then((raw) => parseSearchPage(raw, "history search"))
      .then(
        (next) => {
          const merged = { total: next.total, hits: [...page.hits, ...next.hits] };
          kept = { hash, page: merged };
          setResults({ kind: "loaded", page: merged, more: "idle" });
        },
        () => setResults({ kind: "loaded", page, more: "failed" }),
      );
  };

  const words = searchWords(query);
  const scope =
    discussion === null ? "every discussion" : discussionTitle(snapshot.sessions, discussion);
  return (
    <>
      <Header
        left={<HomeButton sessions={snapshot.sessions} />}
        status={snapshot.status}
        usage={snapshot.usage}
        column={<NeedsYouColumn sessions={snapshot.sessions} />}
        info={
          <>
            <BackButton label="back" />
            <span>Search in {scope}</span>
          </>
        }
      />
      <main className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
        <div className="mx-auto max-w-3xl space-y-4 p-4">
          <SearchField
            initial={query}
            placeholder={`Search ${scope}`}
            autoFocus={query === ""}
            onSearch={(next) => replaceRoute({ kind: "search", discussion, query: next })}
          />
          {results.kind === "loading" && (
            <p className="text-sm text-muted-foreground">Searching...</p>
          )}
          {results.kind === "failed" && (
            <p className="text-sm text-destructive">Search failed: {results.message}</p>
          )}
          {results.kind === "loaded" && (
            <>
              <p className="px-1 text-xs voice-ui text-muted-foreground">
                {results.page.total === 0
                  ? "No message holds every word."
                  : `${results.page.total} ${results.page.total === 1 ? "message" : "messages"}, newest first`}
              </p>
              <ul className="space-y-2">
                {results.page.hits.map((hit) => (
                  <HitRow
                    key={`${hit.discussion}/${hit.seq}`}
                    hit={hit}
                    words={words}
                    now={now}
                    showDiscussion={discussion === null}
                  />
                ))}
              </ul>
              {results.page.hits.length < results.page.total && (
                <button
                  type="button"
                  disabled={results.more === "loading"}
                  onClick={() => loadMore(results.page)}
                  className="w-full rounded-[5px] border px-3 py-2 text-sm voice-ui text-primary hover:bg-muted disabled:opacity-60"
                >
                  {results.more === "loading"
                    ? "Loading..."
                    : results.more === "failed"
                      ? "Loading failed, retry"
                      : `More (${results.page.total - results.page.hits.length} left)`}
                </button>
              )}
            </>
          )}
        </div>
      </main>
    </>
  );
}

function HitRow({
  hit,
  words,
  now,
  showDiscussion,
}: {
  hit: SearchHit;
  words: readonly string[];
  now: number;
  showDiscussion: boolean;
}) {
  return (
    <li>
      <button
        type="button"
        onClick={() => navigate({ kind: "around", discussion: hit.discussion, seq: hit.seq })}
        className="block w-full rounded-[5px] border bg-card px-3 py-2 text-left hover:bg-muted"
      >
        <span className="flex items-baseline justify-between gap-2 text-xs voice-ui text-muted-foreground">
          <span className="min-w-0 truncate">
            {showDiscussion && <span className="text-foreground">{sessionTitle(hit)} · </span>}
            {hit.role === "user" ? "you" : "Claude"}
          </span>
          <span className="shrink-0">{formatStamp(Date.parse(hit.time), now)}</span>
        </span>
        <span className="mt-1 block text-sm [overflow-wrap:anywhere]">
          <Marked text={hit.snippet} words={words} />
        </span>
      </button>
    </li>
  );
}

type Preview =
  { kind: "loading" } | { kind: "failed"; message: string } | { kind: "loaded"; around: Around };

export function AroundScreen({
  client,
  discussion,
  seq,
}: {
  client: Client;
  discussion: string;
  seq: number;
}) {
  const snapshot = useSnapshot(client);
  const now = useNow();
  const [before, setBefore] = useState(AROUND_STEP);
  const [after, setAfter] = useState(AROUND_STEP);
  const [preview, setPreview] = useState<Preview>({ kind: "loading" });
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    let live = true;
    setBusy(true);
    const url = `api/history/around?discussion=${encodeURIComponent(discussion)}&seq=${seq}&before=${before}&after=${after}`;
    getJson(url)
      .then((raw) => parseAround(raw, "history around"))
      .then(
        (around) => {
          if (live) setPreview({ kind: "loaded", around });
        },
        (err: unknown) => {
          if (live) setPreview({ kind: "failed", message: errorText(err) });
        },
      )
      .finally(() => {
        if (live) setBusy(false);
      });
    return () => {
      live = false;
    };
  }, [discussion, seq, before, after]);

  // Centers the hit once, when it first shows.
  const hitRef = useRef<HTMLDivElement>(null);
  const centered = useRef(false);
  useEffect(() => {
    if (centered.current || preview.kind !== "loaded") return;
    centered.current = true;
    hitRef.current?.scrollIntoView({ block: "center" });
  }, [preview]);

  const around = preview.kind === "loaded" ? preview.around : null;
  const title = discussionTitle(snapshot.sessions, discussion, around);
  const step = (label: string, onClick: () => void) => (
    <button
      type="button"
      disabled={busy}
      onClick={onClick}
      className="w-full rounded-[5px] border px-3 py-1.5 text-sm voice-ui text-primary hover:bg-muted disabled:opacity-60"
    >
      {label}
    </button>
  );
  return (
    <>
      <Header
        left={<HomeButton sessions={snapshot.sessions} />}
        status={snapshot.status}
        usage={snapshot.usage}
        column={<NeedsYouColumn sessions={snapshot.sessions} />}
        info={
          <>
            <BackButton label="results" />
            <span>{title}</span>
          </>
        }
      />
      <main className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
        <div className="mx-auto max-w-3xl space-y-3 p-4">
          {preview.kind === "loading" && (
            <p className="text-sm text-muted-foreground">Loading...</p>
          )}
          {preview.kind === "failed" && (
            <p className="text-sm text-destructive">Cannot load the messages: {preview.message}</p>
          )}
          {around !== null && (
            <>
              {around.earlier &&
                before < AROUND_MAX &&
                step("Earlier", () => setBefore((count) => count + AROUND_STEP))}
              {around.messages.map((message) => {
                const hit = message.seq === seq;
                const frame =
                  message.role === "user"
                    ? "ml-16 border-r-2 rounded-r-none bg-bubble-user"
                    : "mr-16 border-l-2 rounded-l-none bg-bubble-claude voice-prose";
                return (
                  <div
                    key={message.seq}
                    ref={hit ? hitRef : undefined}
                    className={`min-w-0 rounded-[5px] border-ring px-3 py-2 text-sm ${frame} ${hit ? "ring-2 ring-primary" : ""}`}
                  >
                    <Markdown text={message.text} />
                    <div className="mt-1.5 text-[0.6875rem] leading-none voice-ui text-muted-foreground">
                      {formatStamp(Date.parse(message.time), now)}
                    </div>
                  </div>
                );
              })}
              {around.later &&
                after < AROUND_MAX &&
                step("Later", () => setAfter((count) => count + AROUND_STEP))}
              <button
                type="button"
                onClick={() => navigate({ kind: "session", id: discussion })}
                className="w-full rounded-[5px] border bg-card px-3 py-2 text-sm voice-ui text-primary hover:bg-muted"
              >
                Open discussion
              </button>
            </>
          )}
        </div>
      </main>
    </>
  );
}
