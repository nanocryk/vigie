import qrcode from "qrcode-generator";
import { type FormEvent, useEffect, useMemo, useState } from "react";

import { SettingsSection } from "@/components/SettingsSection";
import { useClock, useNow } from "@/components/useNow";
import {
  type Device,
  type PairHost,
  type PairLink,
  createPairLink,
  defaultPairHost,
  fetchDevices,
  fetchPairHosts,
  removeDevice,
  renameDevice,
  revokeDevice,
} from "@/devices";
import { formatAgo } from "@/format";

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

const BUTTON = "voice-ui rounded-[5px] border px-3 py-1.5 text-sm disabled:opacity-50";
const SMALL = "voice-ui rounded-[5px] border px-2.5 py-1 text-xs disabled:opacity-50";
const COPIED_MS = 1500;

/** This device first, then connected ones, then by last activity; revoked last. */
function byRelevance(left: Device, right: Device): number {
  const rank = (device: Device) =>
    device.revoked ? 3 : device.isYou ? 0 : device.isConnected ? 1 : 2;
  return rank(left) - rank(right) || Date.parse(right.lastSeen) - Date.parse(left.lastSeen);
}

function useDevices(): { devices: Device[] | null; error: string | null } {
  const [devices, setDevices] = useState<Device[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    const load = () => {
      fetchDevices()
        .then((list) => {
          if (!alive) return;
          setDevices(list);
          setError(null);
        })
        .catch((err: unknown) => {
          if (alive) setError(errorMessage(err));
        });
    };
    // The stream opens with an event, and reopens with one after a drop.
    const source = new EventSource("api/events/devices");
    source.addEventListener("change", load);
    return () => {
      alive = false;
      source.close();
    };
  }, []);
  return { devices, error };
}

function StatusLine({ device, now }: { device: Device; now: number }) {
  if (device.revoked) return <span className="text-muted-foreground">Revoked</span>;
  if (device.isConnected) {
    return (
      <span className="flex items-center gap-1.5 text-gauge-ok">
        <span className="inline-block size-2 rounded-full bg-gauge-ok" />
        Connected
      </span>
    );
  }
  return <span className="text-muted-foreground">Seen {formatAgo(device.lastSeen, now)}</span>;
}

function RenameForm({ device, done }: { device: Device; done: () => void }) {
  const [label, setLabel] = useState(device.label);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    const trimmed = label.trim();
    if (trimmed === device.label) {
      done();
      return;
    }
    setBusy(true);
    setError(null);
    renameDevice(device.id, trimmed)
      .then(done)
      .catch((err: unknown) => {
        setError(errorMessage(err));
        setBusy(false);
      });
  };
  return (
    <form onSubmit={submit} className="space-y-2">
      <div className="flex gap-2">
        <input
          autoFocus
          value={label}
          maxLength={64}
          onChange={(event) => setLabel(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") done();
          }}
          aria-label="Device name"
          className="min-w-0 flex-1 rounded-[5px] border bg-background px-2 py-1 text-sm voice-ui"
        />
        <button
          type="submit"
          disabled={busy || label.trim() === ""}
          className={`${SMALL} border-primary bg-primary text-primary-foreground`}
        >
          Save
        </button>
        <button type="button" onClick={done} className={`${SMALL} hover:bg-muted`}>
          Cancel
        </button>
      </div>
      {error !== null && <p className="text-xs voice-ui text-destructive">{error}</p>}
    </form>
  );
}

type CardMode = "view" | "rename" | "confirm-revoke";

function DeviceCard({ device, now }: { device: Device; now: number }) {
  const [mode, setMode] = useState<CardMode>("view");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const act = (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    action()
      .then(() => setMode("view"))
      .catch((err: unknown) => setError(errorMessage(err)))
      .finally(() => setBusy(false));
  };

  return (
    <li
      className={`rounded-[5px] border bg-card p-3 ${device.revoked ? "border-long-dash" : ""} ${device.isYou ? "border-primary/60" : ""}`}
    >
      <div className={`space-y-1 ${device.revoked ? "opacity-45" : ""}`}>
        <div className="flex items-center gap-2">
          <span className="min-w-0 truncate voice-heading">{device.label}</span>
          {device.isYou && (
            <span className="shrink-0 rounded-full bg-primary/15 px-2 py-0.5 text-[0.6875rem] voice-ui text-primary">
              this device
            </span>
          )}
          <span className="ml-auto shrink-0 text-xs voice-ui">
            <StatusLine device={device} now={now} />
          </span>
        </div>
        <div className="truncate text-xs voice-code text-muted-foreground">
          {device.lastIp ?? "no address yet"}
        </div>
      </div>

      <div className="mt-3">
        {mode === "rename" && <RenameForm device={device} done={() => setMode("view")} />}
        {mode === "confirm-revoke" && (
          <div className="space-y-2 rounded-[5px] border border-destructive/40 bg-destructive/5 p-2">
            <p className="text-sm voice-ui">
              {device.isYou
                ? "Revoke this device? It loses access right away and needs a new pairing link."
                : `Revoke ${device.label}? It loses access on its next request.`}
            </p>
            <div className="flex justify-end gap-2">
              <button
                type="button"
                disabled={busy}
                onClick={() => setMode("view")}
                className={`${SMALL} hover:bg-muted`}
              >
                Cancel
              </button>
              <button
                type="button"
                disabled={busy}
                onClick={() => act(() => revokeDevice(device.id))}
                className={`${SMALL} border-destructive bg-destructive text-white`}
              >
                {busy ? "Revoking..." : "Revoke"}
              </button>
            </div>
          </div>
        )}
        {mode === "view" && (
          <div className="flex justify-end gap-2">
            {device.revoked ? (
              <button
                type="button"
                disabled={busy}
                onClick={() => act(() => removeDevice(device.id))}
                className={`${SMALL} hover:bg-muted`}
              >
                {busy ? "Removing..." : "Remove"}
              </button>
            ) : (
              <>
                <button
                  type="button"
                  onClick={() => setMode("rename")}
                  className={`${SMALL} hover:bg-muted`}
                >
                  Rename
                </button>
                <button
                  type="button"
                  onClick={() => setMode("confirm-revoke")}
                  className={`${SMALL} border-destructive/50 text-destructive hover:bg-destructive/10`}
                >
                  Revoke
                </button>
              </>
            )}
          </div>
        )}
        {error !== null && <p className="mt-2 text-xs voice-ui text-destructive">{error}</p>}
      </div>
    </li>
  );
}

function QrCode({ text, faded }: { text: string; faded: boolean }) {
  const cells = useMemo(() => {
    const code = qrcode(0, "M");
    code.addData(text);
    code.make();
    const size = code.getModuleCount();
    const dark: string[] = [];
    for (let row = 0; row < size; row++) {
      for (let col = 0; col < size; col++) {
        if (code.isDark(row, col)) dark.push(`M${col} ${row}h1v1h-1z`);
      }
    }
    return { size, path: dark.join("") };
  }, [text]);
  // The quiet zone and white ground keep the code scannable in dark theme.
  const margin = 2;
  const extent = cells.size + margin * 2;
  return (
    <svg
      viewBox={`${-margin} ${-margin} ${extent} ${extent}`}
      role="img"
      aria-label="Pairing QR code"
      shapeRendering="crispEdges"
      className={`aspect-square w-full max-w-60 rounded-[5px] bg-white transition-opacity ${faded ? "opacity-20" : ""}`}
    >
      <path d={cells.path} fill="black" />
    </svg>
  );
}

function CopyButton({ text }: { text: string }) {
  const [label, setLabel] = useState("Copy link");
  const show = (shown: string) => {
    setLabel(shown);
    setTimeout(() => setLabel("Copy link"), COPIED_MS);
  };
  const copy = () => {
    // Undefined outside secure contexts, whatever the DOM types say.
    const clipboard: Clipboard | undefined = navigator.clipboard;
    if (clipboard === undefined) {
      show("Copy failed");
      return;
    }
    clipboard.writeText(text).then(
      () => show("Copied"),
      () => show("Copy failed"),
    );
  };
  return (
    <button type="button" onClick={copy} className={`${SMALL} shrink-0 hover:bg-muted`}>
      {label}
    </button>
  );
}

function formatRemaining(ms: number): string {
  const seconds = Math.max(0, Math.ceil(ms / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

function PairCard({
  link,
  renew,
  close,
}: {
  link: PairLink & { lifetime: number };
  renew: () => void;
  close: () => void;
}) {
  const now = useClock(1000);
  const remaining = link.expiresAt - now;
  const expired = remaining <= 0;
  const url = link.urls[0] ?? "";
  return (
    <div className="space-y-3 rounded-[5px] border bg-background p-3">
      <div className="flex justify-center">
        <QrCode text={url} faded={expired} />
      </div>
      <div className="flex items-center gap-2">
        <span className="min-w-0 flex-1 text-xs voice-code break-all text-muted-foreground">
          {url}
        </span>
        <CopyButton text={url} />
      </div>
      <div className="space-y-1">
        <div className="h-1 overflow-hidden rounded-full bg-muted">
          <div
            className="h-full bg-gauge-ok transition-[width] duration-1000 ease-linear"
            style={{ width: `${Math.max(0, Math.min(1, remaining / link.lifetime)) * 100}%` }}
          />
        </div>
        <div className="flex items-center justify-between text-xs voice-ui">
          <span className={expired ? "text-destructive" : "text-muted-foreground"}>
            {expired ? "Link expired" : `Waiting for the new device, ${formatRemaining(remaining)}`}
          </span>
          <div className="flex gap-2">
            {expired && (
              <button
                type="button"
                onClick={renew}
                className={`${SMALL} border-primary bg-primary text-primary-foreground`}
              >
                New link
              </button>
            )}
            <button type="button" onClick={close} className={`${SMALL} hover:bg-muted`}>
              Close
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

type Pairing =
  | { kind: "idle" }
  | { kind: "creating" }
  | { kind: "open"; link: PairLink & { lifetime: number }; knownIds: Set<string> }
  | { kind: "paired"; label: string };

function PairSection({ devices }: { devices: Device[] | null }) {
  const [hosts, setHosts] = useState<PairHost[] | null>(null);
  const [host, setHost] = useState<string | null>(null);
  const [pairing, setPairing] = useState<Pairing>({ kind: "idle" });
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    fetchPairHosts()
      .then((list) => {
        setHosts(list);
        setHost(defaultPairHost(list, window.location)?.host ?? null);
      })
      .catch((err: unknown) => setError(errorMessage(err)));
  }, []);

  // A device that was not active when the link was made is the one that used it.
  useEffect(() => {
    if (pairing.kind !== "open" || devices === null) return;
    const fresh = devices.find((device) => !device.revoked && !pairing.knownIds.has(device.id));
    if (fresh !== undefined) setPairing({ kind: "paired", label: fresh.label });
  }, [devices, pairing]);

  const create = () => {
    if (host === null) return;
    setError(null);
    setPairing({ kind: "creating" });
    const knownIds = new Set(
      (devices ?? []).filter((device) => !device.revoked).map((device) => device.id),
    );
    createPairLink(host)
      .then((link) =>
        setPairing({
          kind: "open",
          link: { ...link, lifetime: link.expiresAt - Date.now() },
          knownIds,
        }),
      )
      .catch((err: unknown) => {
        setError(errorMessage(err));
        setPairing({ kind: "idle" });
      });
  };

  return (
    <div className="space-y-3 border-t pt-3">
      <div>
        <h3 className="text-sm voice-heading">Pair a new device</h3>
        <p className="text-xs voice-ui text-muted-foreground">
          Scan the code or open the link on the new device. Single use, valid 5 minutes.
        </p>
      </div>
      {pairing.kind === "open" ? (
        <PairCard link={pairing.link} renew={create} close={() => setPairing({ kind: "idle" })} />
      ) : (
        <>
          {hosts !== null && hosts.length === 0 && (
            <p className="text-sm voice-ui text-muted-foreground">
              No network address to offer: the server only listens locally.
            </p>
          )}
          {hosts !== null && hosts.length > 0 && (
            <div className="space-y-1.5">
              <p className="text-xs voice-ui text-muted-foreground">The link points at</p>
              <div className="flex flex-wrap gap-2" role="radiogroup">
                {hosts.map((candidate) => {
                  const chosen = candidate.host === host;
                  return (
                    <button
                      key={candidate.host}
                      type="button"
                      role="radio"
                      aria-checked={chosen}
                      onClick={() => setHost(candidate.host)}
                      className={`flex flex-col items-start rounded-[5px] border px-3 py-1.5 text-left voice-ui ${chosen ? "border-primary bg-primary/10" : "hover:bg-muted"}`}
                    >
                      <span className="text-xs font-medium">
                        {candidate.kind}
                        {candidate.name !== "" && (
                          <span className="font-normal text-muted-foreground">
                            {" "}
                            {candidate.name}
                          </span>
                        )}
                      </span>
                      <span className="text-xs voice-code">{candidate.host}</span>
                    </button>
                  );
                })}
              </div>
            </div>
          )}
          {pairing.kind === "paired" && (
            <p className="text-sm voice-ui text-gauge-ok">Paired: {pairing.label}</p>
          )}
          <button
            type="button"
            disabled={host === null || pairing.kind === "creating"}
            onClick={create}
            className={`${BUTTON} border-primary bg-primary text-primary-foreground`}
          >
            {pairing.kind === "creating" ? "Creating..." : "Create pairing link"}
          </button>
        </>
      )}
      {error !== null && <p className="text-xs voice-ui text-destructive">{error}</p>}
    </div>
  );
}

export function Devices() {
  const { devices, error } = useDevices();
  const now = useNow();
  const sorted = useMemo(
    () => (devices === null ? null : [...devices].sort(byRelevance)),
    [devices],
  );
  return (
    <SettingsSection
      id="devices"
      title="Devices"
      summary={
        sorted === null
          ? "Every browser or installed app that can reach this server."
          : `${sorted.length} ${sorted.length === 1 ? "browser or app" : "browsers or apps"} can reach this server.`
      }
      forceOpen={error !== null}
    >
      {sorted === null && error === null && (
        <p className="text-sm voice-ui text-muted-foreground">Loading...</p>
      )}
      {sorted !== null && (
        <ul className="space-y-2">
          {sorted.map((device) => (
            <DeviceCard key={device.id} device={device} now={now} />
          ))}
        </ul>
      )}
      {error !== null && <p className="text-xs voice-ui text-destructive">{error}</p>}
      <PairSection devices={devices} />
    </SettingsSection>
  );
}
