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
import {
  STEP_LABELS,
  type UpdateStatus,
  checkForUpdate,
  fetchUpdateStatus,
  startUpdate,
} from "@/update";
import { fetchBuild, reload } from "@/version";

const PENDING_POLL_MS = 2_000;
const UPDATE_POLL_MS = 2_000;

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
          This browser cannot receive push notifications. On iOS, add vigie to the home screen
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

function Update({ onBuilt }: { onBuilt: (built: boolean) => void }) {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [supervised, setSupervised] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    checkForUpdate()
      .then(setStatus)
      .catch((err: unknown) => setError(errorMessage(err)));
    fetchServerStatus()
      .then((server) => setSupervised(server.supervised))
      .catch((err: unknown) => setError(errorMessage(err)));
  }, []);

  const running = status?.run.state === "running";
  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(() => {
      fetchUpdateStatus()
        .then(setStatus)
        .catch((err: unknown) => setError(errorMessage(err)));
    }, UPDATE_POLL_MS);
    return () => window.clearInterval(timer);
  }, [running]);

  const built = status?.run.state === "built";
  useEffect(() => onBuilt(built), [built, onBuilt]);

  const start = () => {
    setBusy(true);
    setError(null);
    startUpdate()
      .then(setStatus)
      .catch((err: unknown) => setError(errorMessage(err)))
      .finally(() => setBusy(false));
  };

  if (status === null) {
    return error === null ? null : <p className="text-destructive voice-ui text-xs">{error}</p>;
  }
  const { check, run } = status;
  const behind = check.state === "done" ? check.behind : 0;
  const summary =
    check.state === "unchecked"
      ? "Not checked yet."
      : check.state === "failed"
        ? `Cannot check for updates: ${check.message}`
        : behind === 0
          ? "Up to date."
          : `${behind} new ${behind === 1 ? "commit" : "commits"} upstream.`;
  const offered = supervised === true && !running && (behind > 0 || run.state === "failed");
  return (
    <section className="bg-card space-y-3 rounded-[5px] border p-4">
      <div className="flex items-center justify-between gap-3">
        <div>
          <h2 className="voice-heading text-base">Updates</h2>
          <p className="text-muted-foreground voice-ui text-xs">{summary}</p>
        </div>
        {offered && (
          <button
            type="button"
            disabled={busy}
            onClick={start}
            className={`${BUTTON} hover:bg-muted shrink-0`}
          >
            {run.state === "failed" ? "Retry" : "Update"}
          </button>
        )}
      </div>
      {supervised === false && behind > 0 && (
        <p className="voice-ui text-sm">
          To update, stop the server, run <code className="voice-code">git pull</code> in the
          repository, then <code className="voice-code">npm ci</code> and{" "}
          <code className="voice-code">npm run build</code> in <code className="voice-code">web</code>,
          and start it again with <code className="voice-code">serve.sh</code> or{" "}
          <code className="voice-code">serve.bat</code>, which also let it update itself.
        </p>
      )}
      {run.state === "running" && <p className="voice-ui text-sm">{STEP_LABELS[run.step]}...</p>}
      {run.state === "built" && (
        <p className="voice-ui text-sm">Update built: restart the server below to run it.</p>
      )}
      {run.state === "failed" && (
        <div className="space-y-2">
          <p className="text-destructive voice-ui text-sm">{STEP_LABELS[run.step]} failed:</p>
          <pre className="voice-code bg-background max-h-60 overflow-auto rounded-[5px] border p-2 text-xs whitespace-pre-wrap [overflow-wrap:anywhere]">
            {run.output}
          </pre>
        </div>
      )}
      {error !== null && <p className="text-destructive voice-ui text-xs">{error}</p>}
    </section>
  );
}

function ServerRestart({ updated }: { updated: boolean }) {
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
            {updated
              ? "An update is built: restart to run it. The app reconnects on its own."
              : "Runs the last built server binary again. The app reconnects on its own."}
          </p>
        </div>
        {!confirming && (
          <button
            type="button"
            disabled={busy || restarting}
            onClick={ask}
            className={`${BUTTON} shrink-0 ${updated ? "bg-primary text-primary-foreground border-primary" : "hover:bg-muted"}`}
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
  const [updated, setUpdated] = useState(false);
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
          <Update onBuilt={setUpdated} />
          <ServerRestart updated={updated} />
        </div>
      </main>
    </>
  );
}
