import { type KeyboardEvent, useEffect, useLayoutEffect, useRef, useState } from "react";

import type { Client } from "@/client";
import { AskCard } from "@/components/AskCard";
import { PermissionMode } from "@/components/PermissionMode";
import { BusyDot, ConnectionBadge } from "@/components/SessionList";
import { ReloadButton } from "@/components/Reload";
import { TOP_BAR_IN_ROW, TopBar } from "@/components/TopBar";
import { type PendingSend, TranscriptView } from "@/components/TranscriptView";
import { useSnapshot } from "@/components/useSnapshot";
import { folderName, stateLabel } from "@/format";
import { backToList } from "@/route";
import { type Item, emptyTranscript } from "@/transcript";

/** Unsent composer text per session, kept while the app stays open. */
const drafts = new Map<string, string>();

function sentClientIds(items: readonly Item[]): Set<string> {
  const ids = new Set<string>();
  for (const item of items) {
    if (item.kind === "user" && item.clientId !== null) ids.add(item.clientId);
  }
  return ids;
}

function coarsePointer(): boolean {
  return window.matchMedia("(pointer: coarse)").matches;
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
    // On touch, dropping focus closes the virtual keyboard so the reply is visible.
    if (coarsePointer()) area.current?.blur();
  };

  // Desktop: Enter sends, Shift+Enter inserts a newline. Touch: Enter inserts a
  // newline and only the send button sends. An IME composition keeps its own Enter.
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (
      event.key === "Enter" &&
      !event.shiftKey &&
      !event.nativeEvent.isComposing &&
      !coarsePointer()
    ) {
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
        enterKeyHint={coarsePointer() ? "enter" : "send"}
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

  // A pending message leaves once the server's `user_sent` for one of its attempts is in.
  const sent = sentClientIds(transcript.items);
  const visiblePending = pending.filter((send) => !send.clientIds.some((cid) => sent.has(cid)));
  useEffect(() => {
    if (visiblePending.length !== pending.length) setPending(visiblePending);
  }, [visiblePending, pending.length]);

  const patch = (sendId: string, change: Partial<PendingSend>) =>
    setPending((list) => list.map((send) => (send.id === sendId ? { ...send, ...change } : send)));

  const deliver = (sendId: string, text: string, clientId: string) => {
    client.command({ type: "send", session: id, text, client_id: clientId }).then(
      () => patch(sendId, { status: "sent" }),
      (err: unknown) => patch(sendId, { status: "failed", error: errorMessage(err) }),
    );
  };

  const send = (text: string) => {
    nextPending.current += 1;
    const clientId = client.newClientId();
    const entry: PendingSend = {
      id: String(nextPending.current),
      text,
      clientIds: [clientId],
      status: "sending",
      error: null,
    };
    setPending((list) => [...list, entry]);
    deliver(entry.id, text, clientId);
  };

  const retry = (entry: PendingSend) => {
    const clientId = client.newClientId();
    patch(entry.id, { status: "sending", error: null, clientIds: [...entry.clientIds, clientId] });
    deliver(entry.id, entry.text, clientId);
  };

  const dismiss = (entry: PendingSend) =>
    setPending((list) => list.filter((item) => item.id !== entry.id));

  const stop = () => {
    setStopping(true);
    setStopError(null);
    client
      .command({ type: "interrupt", session: id, client_id: client.newClientId() })
      .catch((err: unknown) => setStopError(errorMessage(err)))
      .finally(() => setStopping(false));
  };

  // A failed stop is reported until its turn ends.
  const turnOpen = transcript.turnOpen;
  useEffect(() => {
    if (!turnOpen) setStopError(null);
  }, [turnOpen]);

  const busy = view?.busy ?? false;
  // Asks die with their process; a session known to have none shows no card.
  const asks = view?.live === false ? [] : transcript.asks;
  const ask = asks[0];

  return (
    <>
      <header className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b px-3 py-2">
        <button
          type="button"
          onClick={backToList}
          className="text-primary voice-ui hover:bg-muted shrink-0 rounded-[5px] px-2 py-1 text-sm"
        >
          &lsaquo; Sessions
        </button>
        <div className="min-w-0 flex-1 min-[600px]:max-w-40 min-[600px]:flex-none">
          <div className="voice-heading truncate">
            {view === null ? "..." : folderName(view.cwd)}
          </div>
          {view !== null && (
            <div className="text-muted-foreground voice-ui truncate text-xs">
              {busy ? "working" : stateLabel(view.state)}
            </div>
          )}
        </div>
        <TopBar
          usage={snapshot.usage}
          session={view}
          className={TOP_BAR_IN_ROW}
        />
        <ConnectionBadge status={snapshot.status} />
        <BusyDot busy={busy} />
        <ReloadButton />
      </header>

      {serverError !== null && (
        <p className="text-destructive border-b px-3 py-2 text-sm">{serverError}</p>
      )}

      <TranscriptView
        items={transcript.items}
        pending={visiblePending}
        onRetry={retry}
        onDismiss={dismiss}
        overlay={
          transcript.turnOpen && (
            <div className="pointer-events-auto flex items-center gap-2">
              {stopError !== null && (
                <span className="bg-background text-destructive rounded-[5px] px-2 py-1 text-xs shadow-sm">
                  {stopError}
                </span>
              )}
              <button
                type="button"
                onClick={stop}
                disabled={stopping}
                className="border-destructive text-destructive bg-background voice-ui h-10 shrink-0 rounded-[5px] border px-4 text-sm shadow-md disabled:opacity-50"
              >
                {stopping ? "Stopping..." : "Stop"}
              </button>
            </div>
          )
        }
        tabs={
          <PermissionMode
            client={client}
            session={id}
            mode={
              view?.live === true
                ? transcript.permissionMode
                : (view?.permission_mode ?? transcript.permissionMode)
            }
            enabled={view !== null}
          />
        }
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
        <Composer session={id} onSend={send} />
      </footer>
    </>
  );
}
