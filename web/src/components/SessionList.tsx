import { type FormEvent, useEffect, useState } from "react";

import type { Client, ConnectionStatus } from "@/client";
import { Header, NeedsYouColumn } from "@/components/TopBar";
import { useSnapshot } from "@/components/useSnapshot";
import { folderName, formatAgo, stateLabel } from "@/format";
import { navigate } from "@/route";

const CLOCK_TICK_MS = 30_000;

/** Nothing while connected; a dot named after the status otherwise. */
export function ConnectionDot({ status }: { status: ConnectionStatus }) {
  if (status === "open") return null;
  const text = status === "connecting" ? "connecting..." : "offline, retrying";
  return (
    <span
      role="status"
      aria-label={text}
      title={text}
      className={
        status === "connecting"
          ? "bg-gauge-glow inline-block size-2 shrink-0 animate-pulse rounded-full"
          : "bg-destructive inline-block size-2 shrink-0 rounded-full"
      }
    />
  );
}

export function BusyDot({ busy }: { busy: boolean }) {
  return (
    <span
      aria-label={busy ? "busy" : "idle"}
      className={
        busy
          ? "bg-primary inline-block size-2 shrink-0 animate-pulse rounded-full"
          : "bg-muted-foreground/30 inline-block size-2 shrink-0 rounded-full"
      }
    />
  );
}

function NewSession({ client }: { client: Client }) {
  const [cwd, setCwd] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const path = cwd.trim();
    if (path === "" || pending) return;
    setPending(true);
    setError(null);
    const result = await client.createSession(path);
    setPending(false);
    if (result.ok) {
      setCwd("");
      navigate({ kind: "session", id: result.session.id });
    } else {
      setError(result.error);
    }
  };

  return (
    <form onSubmit={(event) => void submit(event)} className="space-y-2">
      <label className="voice-ui text-muted-foreground block text-xs" htmlFor="new-cwd">
        New session in folder (absolute path)
      </label>
      <div className="flex gap-2">
        <input
          id="new-cwd"
          value={cwd}
          onChange={(event) => setCwd(event.target.value)}
          placeholder="C:\path\to\project"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          className="bg-card focus:ring-ring voice-code min-w-0 flex-1 rounded-[5px] border px-3 py-2 text-base outline-none focus:ring-2"
        />
        <button
          type="submit"
          disabled={pending || cwd.trim() === ""}
          className="bg-primary text-primary-foreground voice-ui shrink-0 rounded-[5px] px-4 py-2 text-sm disabled:opacity-50"
        >
          {pending ? "Starting..." : "Start"}
        </button>
      </div>
      {error !== null && <p className="text-destructive text-sm">{error}</p>}
    </form>
  );
}

export function SessionList({ client, label }: { client: Client; label: string | null }) {
  const snapshot = useSnapshot(client);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), CLOCK_TICK_MS);
    return () => clearInterval(timer);
  }, []);

  return (
    <>
      <Header
        left={<span className="text-primary voice-heading px-2 text-lg">vigie2</span>}
        right={<ConnectionDot status={snapshot.status} />}
        usage={snapshot.usage}
        column={<NeedsYouColumn sessions={snapshot.sessions} />}
        info={null}
        label={label}
      />
      <main className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
        <div className="mx-auto max-w-3xl space-y-6 p-4">
          <NewSession client={client} />
          {snapshot.sessionsError !== null && (
            <p className="text-destructive text-sm">{snapshot.sessionsError}</p>
          )}
          {snapshot.sessions === null ? (
            <p className="text-muted-foreground text-sm">Loading sessions...</p>
          ) : snapshot.sessions.length === 0 ? (
            <p className="text-muted-foreground text-sm">No sessions yet.</p>
          ) : (
            <ul className="space-y-2">
              {snapshot.sessions.map((session) => (
                <li key={session.id}>
                  <button
                    type="button"
                    onClick={() => navigate({ kind: "session", id: session.id })}
                    className="bg-card hover:bg-muted block w-full rounded-[5px] border px-3 py-2 text-left"
                  >
                    <div className="flex items-center gap-2">
                      <BusyDot busy={session.busy} />
                      <span className="voice-heading min-w-0 flex-1 truncate">
                        {folderName(session.cwd)}
                      </span>
                      <span className="text-muted-foreground voice-ui shrink-0 text-xs">
                        {formatAgo(session.updated_at, now)}
                      </span>
                    </div>
                    <div className="text-muted-foreground voice-code truncate text-xs">
                      {session.cwd}
                    </div>
                    <div className="text-muted-foreground voice-ui text-xs">
                      {stateLabel(session.state)}
                    </div>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </main>
    </>
  );
}
