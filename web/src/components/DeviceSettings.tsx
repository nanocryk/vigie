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

export function DeviceSettings({ client, label }: { client: Client; label: string | null }) {
  const snapshot = useSnapshot(client);
  return (
    <>
      <Header
        left={<HomeButton />}
        right={<ConnectionDot status={snapshot.status} />}
        usage={snapshot.usage}
        column={<NeedsYouColumn sessions={snapshot.sessions} />}
        info={<span className="voice-heading">Device settings</span>}
        label={label}
      />
      <main className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
        <div className="mx-auto max-w-3xl space-y-4 p-4">
          <Notifications />
          <Devices />
        </div>
      </main>
    </>
  );
}
