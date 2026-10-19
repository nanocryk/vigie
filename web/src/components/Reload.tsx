import { useEffect, useState } from "react";

import type { Client } from "@/client";
import { useSnapshot } from "@/components/useSnapshot";
import { type ServerStatus, certWarning, fetchServerStatus } from "@/server";
import { reload, useNewVersion } from "@/version";

const UNREACHABLE_AFTER_MS = 30_000;

/** A server problem, else a new web build to reload. */
export function Banner({ client }: { client: Client }) {
  const problem = useServerProblem(client);
  const stale = useNewVersion();
  if (problem !== null) {
    return (
      <div className="w-full shrink-0 bg-destructive px-3 py-1.5 text-center text-sm voice-ui text-card">
        {problem}
      </div>
    );
  }
  if (!stale) return null;
  return (
    <button
      type="button"
      onClick={reload}
      className="w-full shrink-0 bg-primary px-3 py-1.5 text-sm voice-ui text-primary-foreground"
    >
      New version available: tap to reload
    </button>
  );
}

/**
 * Under the shim: a server gone for a while, or one running a previous build.
 * Anywhere: a TLS certificate about to expire.
 */
function useServerProblem(client: Client): string | null {
  const open = useSnapshot(client).status === "open";
  const [server, setServer] = useState<ServerStatus | null>(null);
  const [unreachable, setUnreachable] = useState(false);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    fetchServerStatus()
      .then((fresh) => {
        if (!cancelled) setServer(fresh);
      })
      .catch((err: unknown) => console.warn("server status failed", err));
    return () => {
      cancelled = true;
    };
  }, [open]);

  useEffect(() => {
    if (open) return;
    // Hidden time does not count: a resumed page reconnects before the delay ends.
    let timer: number | null = null;
    const arm = () => {
      if (timer !== null) window.clearTimeout(timer);
      timer =
        document.visibilityState === "visible"
          ? window.setTimeout(() => setUnreachable(true), UNREACHABLE_AFTER_MS)
          : null;
    };
    arm();
    document.addEventListener("visibilitychange", arm);
    return () => {
      document.removeEventListener("visibilitychange", arm);
      if (timer !== null) window.clearTimeout(timer);
      setUnreachable(false);
    };
  }, [open]);

  if (server === null) return null;
  if (server.supervised && unreachable) {
    return "Server unreachable for 30 s: if it was restarting, it likely needs manual intervention";
  }
  if (server.supervised && server.staleCopy) {
    return "Restart ran the previous build: copying the new one failed";
  }
  return server.certExpiresSoon === null ? null : certWarning(server.certExpiresSoon, Date.now());
}
