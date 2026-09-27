import { type KeyboardEvent, useEffect, useLayoutEffect, useRef, useState } from "react";

import type { Client } from "@/client";
import { AskCard } from "@/components/AskCard";
import { BusyDot, ConnectionBadge } from "@/components/SessionList";
import { type PendingSend, TranscriptView } from "@/components/TranscriptView";
import { useSnapshot } from "@/components/useSnapshot";
import { folderName, stateLabel } from "@/format";
import { backToList } from "@/route";
import { type Item, emptyTranscript } from "@/transcript";

/** Unsent composer text per session, kept while the app stays open. */
const drafts = new Map<string, string>();

function userBubbles(items: readonly Item[], text: string): number {
  return items.filter((item) => item.kind === "user" && item.text === text).length;
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function Composer({
  session,
  onSend,
}: {
  session: string;
  onSend: (text: string) => void;
}) {
  const [text, setText] = useState(() => drafts.get(session) ?? "");
  const area = useRef<HTMLTextAreaElement>(null);

  useLayoutEffect(() => {
    const element = area.current;
    if (element === null) return;
    element.style.height = "auto";
    element.style.height = `${element.scrollHeight}px`;
  }, [text]);

  const change = (value: string) => {
    setText(value);
    if (value === "") drafts.delete(session);
    else drafts.set(session, value);
  };

  const submit = () => {
    if (text.trim() === "") return;
    onSend(text);
    change("");
  };

  // Enter always inserts a newline; Ctrl or Cmd with Enter sends.
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      submit();
    }
  };

  return (
    <div className="mx-auto flex max-w-5xl items-end gap-2">
      <textarea
        ref={area}
        rows={1}
        value={text}
        onChange={(event) => change(event.target.value)}
        onKeyDown={onKeyDown}
        placeholder="Message Claude"
        className="bg-card focus:ring-ring voice-prose max-h-[35dvh] min-h-10 flex-1 resize-none rounded-[5px] border px-3 py-2 text-base outline-none focus:ring-2"
      />
      <button
        type="button"
        // Keeps the focus, and with it the OS keyboard, in the textarea.
        onMouseDown={(event) => event.preventDefault()}
        onClick={submit}
        disabled={text.trim() === ""}
        className="bg-primary text-primary-foreground voice-ui h-10 shrink-0 rounded-[5px] px-4 text-sm disabled:opacity-50"
      >
        Send
      </button>
    </div>
  );
}

export function SessionScreen({ client, id }: { client: Client; id: string }) {
  const snapshot = useSnapshot(client);
  const transcript = snapshot.transcripts.get(id) ?? emptyTranscript;
  const view = snapshot.sessions?.find((session) => session.id === id) ?? null;
  const serverError = snapshot.sessionErrors.get(id) ?? null;
  const [pending, setPending] = useState<PendingSend[]>([]);
  const [stopping, setStopping] = useState(false);
  const [stopError, setStopError] = useState<string | null>(null);
  const nextPending = useRef(0);

  useEffect(() => {
    client.openSession(id);
    return () => client.closeSession(id);
  }, [client, id]);

  // A pending message leaves once its echo is in the transcript.
  const visiblePending = pending.filter(
    (send) => userBubbles(transcript.items, send.text) <= send.baseline,
  );
  useEffect(() => {
    if (visiblePending.length !== pending.length) setPending(visiblePending);
  }, [visiblePending, pending.length]);

  const patch = (sendId: string, change: Partial<PendingSend>) =>
    setPending((list) => list.map((send) => (send.id === sendId ? { ...send, ...change } : send)));

  const deliver = (send: PendingSend) => {
    client
      .command((clientId) => ({ type: "send", session: id, text: send.text, client_id: clientId }))
      .then(
        () => patch(send.id, { status: "sent" }),
        (err: unknown) => patch(send.id, { status: "failed", error: errorMessage(err) }),
      );
  };

  const send = (text: string) => {
    nextPending.current += 1;
    const entry: PendingSend = {
      id: String(nextPending.current),
      text,
      baseline: userBubbles(transcript.items, text),
      status: "sending",
      error: null,
    };
    setPending((list) => [...list, entry]);
    deliver(entry);
  };

  const retry = (entry: PendingSend) => {
    patch(entry.id, { status: "sending", error: null });
    deliver(entry);
  };

  const dismiss = (entry: PendingSend) =>
    setPending((list) => list.filter((item) => item.id !== entry.id));

  const stop = () => {
    setStopping(true);
    setStopError(null);
    client
      .command((clientId) => ({ type: "interrupt", session: id, client_id: clientId }))
      .catch((err: unknown) => setStopError(errorMessage(err)))
      .finally(() => setStopping(false));
  };

  const busy = view?.busy ?? false;
  // Asks die with their process; a session known to have none shows no card.
  const asks = view?.live === false ? [] : transcript.asks;
  const ask = asks[0];

  return (
    <>
      <header className="flex items-center gap-3 border-b px-3 py-2">
        <button
          type="button"
          onClick={backToList}
          className="text-primary voice-ui hover:bg-muted shrink-0 rounded-[5px] px-2 py-1 text-sm"
        >
          &lsaquo; Sessions
        </button>
        <div className="min-w-0 flex-1">
          <div className="voice-heading truncate">{view === null ? "..." : folderName(view.cwd)}</div>
          {view !== null && (
            <div className="text-muted-foreground voice-ui truncate text-xs">
              {busy ? "working" : stateLabel(view.state)}
            </div>
          )}
        </div>
        <ConnectionBadge status={snapshot.status} />
        <BusyDot busy={busy} />
      </header>

      {serverError !== null && (
        <p className="text-destructive border-b px-3 py-2 text-sm">{serverError}</p>
      )}

      <TranscriptView
        items={transcript.items}
        pending={visiblePending}
        onRetry={retry}
        onDismiss={dismiss}
      />

      {ask !== undefined && (
        <AskCard
          key={ask.requestId}
          client={client}
          session={id}
          ask={ask}
          queued={asks.length - 1}
        />
      )}

      <footer className="border-t px-3 pt-2 pb-[max(0.5rem,env(safe-area-inset-bottom))]">
        {(busy || stopError !== null) && (
          <div className="mx-auto mb-2 flex max-w-5xl items-center gap-3">
            {busy && (
              <button
                type="button"
                onClick={stop}
                disabled={stopping}
                className="border-destructive text-destructive voice-ui rounded-[5px] border px-3 py-1 text-sm disabled:opacity-50"
              >
                {stopping ? "Stopping..." : "Stop"}
              </button>
            )}
            {stopError !== null && <span className="text-destructive text-xs">{stopError}</span>}
          </div>
        )}
        <Composer session={id} onSend={send} />
      </footer>
    </>
  );
}
