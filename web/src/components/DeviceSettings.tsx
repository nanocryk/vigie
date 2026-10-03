import { useEffect, useState } from "react";

import type { Client } from "@/client";
import { Devices } from "@/components/Devices";
import { SettingsSection } from "@/components/SettingsSection";
import { Header, HomeButton, NeedsYouColumn } from "@/components/TopBar";
import { useSnapshot } from "@/components/useSnapshot";
import { SCALES, THEMES, type Scale, type ThemeChoice, setDisplay, useDisplay } from "@/display";
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
  CONFIRM_WINDOWS,
  DISTANCES,
  type Distance,
  SAFE_ASK_SWIPES,
  type SafeAskSwipes,
  setSwipePrefs,
  useSwipePrefs,
} from "@/swipePrefs";
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

const SCALE_LABELS: Record<Scale, string> = {
  87.5: "Small",
  100: "Default",
  112.5: "Large",
  125: "Larger",
};

const THEME_LABELS: Record<ThemeChoice, string> = {
  system: "System",
  light: "Light",
  dark: "Dark",
};

function Choices<T extends string | number>({
  label,
  options,
  value,
  name,
  onPick,
}: {
  label: string;
  options: readonly T[];
  value: T;
  name: (option: T) => string;
  onPick: (option: T) => void;
}) {
  return (
    <div className="space-y-1.5">
      <p className="text-xs voice-ui text-muted-foreground">{label}</p>
      <div className="flex flex-wrap gap-2" role="radiogroup" aria-label={label}>
        {options.map((option) => (
          <button
            key={option}
            type="button"
            role="radio"
            aria-checked={option === value}
            onClick={() => onPick(option)}
            className={`rounded-[5px] border px-3 py-1.5 text-sm voice-ui ${option === value ? "border-primary bg-primary/10" : "hover:bg-muted"}`}
          >
            {name(option)}
          </button>
        ))}
      </div>
    </div>
  );
}

const DISTANCE_LABELS: Record<Distance, string> = {
  short: "Short",
  default: "Default",
  long: "Long",
};

const SAFE_ASK_LABELS: Record<SafeAskSwipes, string> = {
  two: "Two swipes",
  one: "One swipe",
};

function Swipes() {
  const prefs = useSwipePrefs();
  return (
    <SettingsSection
      id="swipes"
      title="Swipes"
      summary={`${DISTANCE_LABELS[prefs.distance]} distance, ${prefs.confirmWindow / 1000} s to confirm, ${SAFE_ASK_LABELS[prefs.safeAsk].toLowerCase()} for safe asks.`}
    >
      <Choices
        label="Distance to choose a side"
        options={DISTANCES}
        value={prefs.distance}
        name={(distance) => DISTANCE_LABELS[distance]}
        onPick={(distance) => setSwipePrefs({ ...prefs, distance })}
      />
      <Choices
        label="Time for the second swipe"
        options={CONFIRM_WINDOWS}
        value={prefs.confirmWindow}
        name={(window) => `${window / 1000} s`}
        onPick={(confirmWindow) => setSwipePrefs({ ...prefs, confirmWindow })}
      />
      <Choices
        label="Asks that are not dangerous take"
        options={SAFE_ASK_SWIPES}
        value={prefs.safeAsk}
        name={(count) => SAFE_ASK_LABELS[count]}
        onPick={(safeAsk) => setSwipePrefs({ ...prefs, safeAsk })}
      />
    </SettingsSection>
  );
}

function Display() {
  const display = useDisplay();
  return (
    <SettingsSection
      id="display"
      title="Display"
      summary={`${SCALE_LABELS[display.scale]} scale, ${THEME_LABELS[display.theme].toLowerCase()} theme.`}
    >
      <Choices
        label="Scale"
        options={SCALES}
        value={display.scale}
        name={(scale) => SCALE_LABELS[scale]}
        onPick={(scale) => setDisplay({ ...display, scale })}
      />
      <Choices
        label="Theme"
        options={THEMES}
        value={display.theme}
        name={(theme) => THEME_LABELS[theme]}
        onPick={(theme) => setDisplay({ ...display, theme })}
      />
    </SettingsSection>
  );
}

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
    <SettingsSection
      id="notifications"
      title="Notifications"
      summary={
        !pushSupported()
          ? "Not available in this browser."
          : on
            ? "On for this device."
            : "Off for this device."
      }
      forceOpen={error !== null}
      action={
        pushSupported() && (
          <button
            type="button"
            role="switch"
            aria-checked={on}
            aria-label="Notifications"
            disabled={busy || config === null}
            onClick={() => {
              if (config === null) return;
              run(() => (on ? disablePush() : enablePush(config)));
            }}
            className={`${BUTTON} shrink-0 ${on ? "border-primary bg-primary text-primary-foreground" : "hover:bg-muted"}`}
          >
            {busy ? "..." : on ? "On" : "Off"}
          </button>
        )
      }
    >
      <p className="text-sm voice-ui text-muted-foreground">
        Asks, finished turns, unexpected stops and caches about to go cold, while no device is
        looking at the discussion.
      </p>
      {!pushSupported() && (
        <p className="text-sm voice-ui text-muted-foreground">
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
      {error !== null && <p className="text-xs voice-ui text-destructive">{error}</p>}
    </SettingsSection>
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
    return error === null ? null : <p className="text-xs voice-ui text-destructive">{error}</p>;
  }
  const { check, run, changelog } = status;
  const behind = check.state === "done" ? check.behind : 0;
  const checked =
    check.state === "unchecked"
      ? "Not checked yet."
      : check.state === "failed"
        ? `Cannot check for updates: ${check.message}`
        : behind === 0
          ? "Up to date."
          : `${behind} new ${behind === 1 ? "commit" : "commits"} upstream.`;
  const summary = changelog.length > 0 ? `${checked} Open to see the changelog.` : checked;
  const offered = supervised === true && !running && (behind > 0 || run.state === "failed");
  const manual = supervised === false && behind > 0;
  const body = manual || run.state !== "idle" || changelog.length > 0 || error !== null;
  return (
    <SettingsSection
      id="updates"
      title="Updates"
      summary={summary}
      forceOpen={run.state !== "idle" || error !== null}
      action={
        offered && (
          <button
            type="button"
            disabled={busy}
            onClick={start}
            className={`${BUTTON} shrink-0 hover:bg-muted`}
          >
            {run.state === "failed" ? "Retry" : "Update"}
          </button>
        )
      }
    >
      {body && (
        <>
          {manual && (
            <p className="text-sm voice-ui">
              To update, stop the server, run <code className="voice-code">git pull</code> in the
              repository, then <code className="voice-code">npm ci</code> and{" "}
              <code className="voice-code">npm run build</code> in{" "}
              <code className="voice-code">web</code>, and start it again with{" "}
              <code className="voice-code">serve.sh</code> or{" "}
              <code className="voice-code">serve.bat</code>, which also let it update itself.
            </p>
          )}
          {run.state === "running" && (
            <p className="text-sm voice-ui">{STEP_LABELS[run.step]}...</p>
          )}
          {run.state === "built" && (
            <p className="text-sm voice-ui">Update built: restart the server below to run it.</p>
          )}
          {run.state === "failed" && (
            <div className="space-y-2">
              <p className="text-sm voice-ui text-destructive">{STEP_LABELS[run.step]} failed:</p>
              <pre className="max-h-60 overflow-auto rounded-[5px] border bg-background p-2 text-xs voice-code [overflow-wrap:anywhere] whitespace-pre-wrap">
                {run.output}
              </pre>
            </div>
          )}
          {changelog.length > 0 && (
            <div className="space-y-1">
              <p className="text-sm voice-ui">
                {run.state === "running" ? "Pulled" : "Latest update"}: {changelog.length}{" "}
                {changelog.length === 1 ? "commit" : "commits"}
              </p>
              <ul className="max-h-60 space-y-0.5 overflow-auto text-sm voice-ui">
                {changelog.map((commit) => (
                  <li key={commit.sha} className="flex gap-2">
                    <code className="shrink-0 text-xs voice-code text-muted-foreground">
                      {commit.sha}
                    </code>
                    <span className="min-w-0 [overflow-wrap:anywhere]">{commit.title}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {error !== null && <p className="text-xs voice-ui text-destructive">{error}</p>}
        </>
      )}
    </SettingsSection>
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
    return error === null ? null : <p className="text-xs voice-ui text-destructive">{error}</p>;
  }
  const turns = status.turns;
  const waiting = pending && !restarting && !confirming;
  const body = waiting || confirming || error !== null;
  return (
    <SettingsSection
      id="server"
      title="Server"
      summary={
        updated
          ? "An update is built: restart to run it. The app reconnects on its own."
          : "Runs the last built server binary again. The app reconnects on its own."
      }
      forceOpen={body}
      action={
        !confirming && (
          <button
            type="button"
            disabled={busy || restarting}
            onClick={ask}
            className={`${BUTTON} shrink-0 ${updated ? "border-primary bg-primary text-primary-foreground" : "hover:bg-muted"}`}
          >
            {restarting ? "Restarting..." : pending ? "Restart now" : "Restart"}
          </button>
        )
      }
    >
      {body && (
        <>
          {waiting && (
            <div className="flex items-center justify-between gap-3">
              <p className="text-sm voice-ui">
                Restart pending: waits for {turns} running {turns === 1 ? "turn" : "turns"} to end.
                Messages sent meanwhile are held.
              </p>
              <button
                type="button"
                disabled={busy}
                onClick={cancel}
                className={`${BUTTON} shrink-0 hover:bg-muted`}
              >
                Cancel
              </button>
            </div>
          )}
          {confirming && (
            <div className="space-y-3 rounded-[5px] border-2 border-destructive bg-destructive/15 p-3">
              <p className="text-base voice-heading text-destructive">Restart the server now?</p>
              <p className="text-sm voice-ui">
                {turns === 0
                  ? "No turn is running."
                  : `${turns} running ${turns === 1 ? "turn" : "turns"} will be stopped.`}
              </p>
              <div className="flex gap-2">
                <button
                  type="button"
                  disabled={busy}
                  onClick={restart}
                  className="rounded-[5px] bg-destructive px-4 py-2 text-sm voice-ui text-black disabled:opacity-50"
                >
                  Restart
                </button>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => setConfirming(false)}
                  className="rounded-[5px] border bg-background px-4 py-2 text-sm voice-ui disabled:opacity-50"
                >
                  Back
                </button>
              </div>
            </div>
          )}
          {error !== null && <p className="text-xs voice-ui text-destructive">{error}</p>}
        </>
      )}
    </SettingsSection>
  );
}

export function DeviceSettings({ client }: { client: Client }) {
  const snapshot = useSnapshot(client);
  const [updated, setUpdated] = useState(false);
  return (
    <>
      <Header
        left={<HomeButton sessions={snapshot.sessions} />}
        status={snapshot.status}
        usage={snapshot.usage}
        column={<NeedsYouColumn sessions={snapshot.sessions} />}
        info={<span className="voice-heading">Device settings</span>}
      />
      <main className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
        <div className="mx-auto max-w-3xl space-y-2 p-4">
          <Display />
          <Swipes />
          <Notifications />
          <Devices />
          <Update onBuilt={setUpdated} />
          <ServerRestart updated={updated} />
        </div>
      </main>
    </>
  );
}
