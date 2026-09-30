import { useEffect, useState } from "react";

import type { Client } from "@/client";
import { Devices } from "@/components/Devices";
import { ConnectionDot } from "@/components/SessionList";
import { Header, HomeButton, NeedsYouColumn } from "@/components/TopBar";
import { useSnapshot } from "@/components/useSnapshot";
import {
  type PushConfig,
  disablePush,
  enablePush,
  fetchPushConfig,
  pushSupported,
  sendTestPush,
} from "@/push";
import {
  type ServerStatus,
  cancelRestart,
  fetchServerStatus,
  restartServer,
  waitForNewBuild,
} from "@/server";
import { fetchBuild, reload } from "@/version";

const PENDING_POLL_MS = 2_000;

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

const BUTTON = "voice-ui rounded-[5px] border px-3 py-1.5 text-sm disabled:opacity-50";

function Notifications() {
  const [config, setConfig] = useState<PushConfig | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [tested, setTested] = useState(false);

  useEffect(() => {
    if (!pushSupported()) return;
    fetchPushConfig()
      .then(setConfig)
      .catch((err: unknown) => setError(errorMessage(err)));
  }, []);

  const run = (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    setTested(false);
    action()
      .then(fetchPushConfig)
      .then(setConfig)
      .catch((err: unknown) => setError(errorMessage(err)))
      .finally(() => setBusy(false));
  };

  const on = config?.subscribed === true;
  return (
    <section className="bg-card space-y-3 rounded-[5px] border p-4">
      <div className="flex items-center justify-between gap-3">
        <div>
          <h2 className="voice-heading text-base">Notifications</h2>
          <p className="text-muted-foreground voice-ui text-xs">
            Asks, finished turns, unexpected stops and caches about to go cold, while no device is
            looking at the discussion.
          </p>
        </div>
        {pushSupported() && (
          <button
            type="button"
            role="switch"
            aria-checked={on}
            disabled={busy || config === null}
            onClick={() => {
              if (config === null) return;
              run(() => (on ? disablePush() : enablePush(config)));
            }}
            className={`${BUTTON} shrink-0 ${on ? "bg-primary text-primary-foreground border-primary" : "hover:bg-muted"}`}
          >
            {busy ? "..." : on ? "On" : "Off"}
          </button>
        )}
      </div>
      {!pushSupported() && (
        <p className="text-muted-foreground voice-ui text-sm">
          This browser cannot receive push notifications. On iOS, add vigie2 to the home screen
          first.
        </p>
      )}
      {on && (
        <button
          type="button"
          disabled={busy}
          onClick={() => {
            setError(null);
            sendTestPush()
              .then(() => setTested(true))
              .catch((err: unknown) => setError(errorMessage(err)));
          }}
          className={`${BUTTON} hover:bg-muted`}
        >
          {tested ? "Test sent" : "Send a test"}
        </button>
      )}
      {error !== null && <p className="text-destructive voice-ui text-xs">{error}</p>}
    </section>
  );
}

function ServerRestart() {
  const [status, setStatus] = useState<ServerStatus | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    fetchServerStatus()
      .then(setStatus)
      .catch((err: unknown) => setError(errorMessage(err)));
  }, []);

  // A pending restart fires on its own once every turn ends: follow it, then reload.
  const pending = status?.pending === true;
  useEffect(() => {
    if (!pending || restarting) return;
    let cancelled = false;
    let before: string | null = null;
    void fetchBuild().then((build) => (before = build));
    const timer = window.setInterval(() => {
      fetchServerStatus()
        .then((fresh) => {
          if (!cancelled) setStatus(fresh);
        })
        .catch(() => {
          if (cancelled) return;
          cancelled = true;
          window.clearInterval(timer);
          setRestarting(true);
          waitForNewBuild(before)
            .then(reload)
            .catch((err: unknown) => {
              setRestarting(false);
              setError(errorMessage(err));
            });
        });
    }, PENDING_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [pending, restarting]);

  const cancel = () => {
    setBusy(true);
    setError(null);
    cancelRestart()
      .then(fetchServerStatus)
      .then(setStatus)
      .catch((err: unknown) => setError(errorMessage(err)))
      .finally(() => setBusy(false));
  };

  const ask = () => {
    setBusy(true);
    setError(null);
    fetchServerStatus()
      .then((fresh) => {
        setStatus(fresh);
        setConfirming(true);
      })
      .catch((err: unknown) => setError(errorMessage(err)))
      .finally(() => setBusy(false));
  };

  const restart = () => {
    setConfirming(false);
    setRestarting(true);
    setError(null);
    restartServer()
      .then(reload)
      .catch((err: unknown) => {
        setRestarting(false);
        setError(errorMessage(err));
      });
  };

  if (status?.supervised !== true) {
    return error === null ? null : <p className="text-destructive voice-ui text-xs">{error}</p>;
  }
  const turns = status.turns;
  return (
    <section className="bg-card space-y-3 rounded-[5px] border p-4">
      <div className="flex items-center justify-between gap-3">
        <div>
          <h2 className="voice-heading text-base">Server</h2>
          <p className="text-muted-foreground voice-ui text-xs">
            Runs the last built server binary again. The app reconnects on its own.
          </p>
        </div>
        {!confirming && (
          <button
            type="button"
            disabled={busy || restarting}
            onClick={ask}
            className={`${BUTTON} hover:bg-muted shrink-0`}
          >
            {restarting ? "Restarting..." : pending ? "Restart now" : "Restart"}
          </button>
        )}
      </div>
      {pending && !restarting && !confirming && (
        <div className="flex items-center justify-between gap-3">
          <p className="voice-ui text-sm">
            Restart pending: waits for {turns} running {turns === 1 ? "turn" : "turns"} to end.
            Messages sent meanwhile are held.
          </p>
          <button
            type="button"
            disabled={busy}
            onClick={cancel}
            className={`${BUTTON} hover:bg-muted shrink-0`}
          >
            Cancel
          </button>
        </div>
      )}
      {confirming && (
        <div className="border-destructive bg-destructive/15 space-y-3 rounded-[5px] border-2 p-3">
          <p className="voice-heading text-destructive text-base">Restart the server now?</p>
          <p className="voice-ui text-sm">
            {turns === 0
              ? "No turn is running."
              : `${turns} running ${turns === 1 ? "turn" : "turns"} will be stopped.`}
          </p>
          <div className="flex gap-2">
            <button
              type="button"
              disabled={busy}
              onClick={restart}
              className="bg-destructive voice-ui rounded-[5px] px-4 py-2 text-sm text-black disabled:opacity-50"
            >
              Restart
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => setConfirming(false)}
              className="bg-background voice-ui rounded-[5px] border px-4 py-2 text-sm disabled:opacity-50"
            >
              Back
            </button>
          </div>
        </div>
      )}
      {error !== null && <p className="text-destructive voice-ui text-xs">{error}</p>}
    </section>
  );
}

export function DeviceSettings({ client }: { client: Client }) {
  const snapshot = useSnapshot(client);
  return (
    <>
      <Header
        left={<HomeButton sessions={snapshot.sessions} />}
        right={<ConnectionDot status={snapshot.status} />}
        usage={snapshot.usage}
        column={<NeedsYouColumn sessions={snapshot.sessions} />}
        info={<span className="voice-heading">Device settings</span>}
      />
      <main className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
        <div className="mx-auto max-w-3xl space-y-4 p-4">
          <Notifications />
          <Devices />
          <ServerRestart />
        </div>
      </main>
    </>
  );
}
