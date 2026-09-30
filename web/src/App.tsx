import { type ReactNode, use, useEffect, useState } from "react";

import { getClient } from "@/client";
import { DeviceSettings } from "@/components/DeviceSettings";
import { Explorer } from "@/components/Explorer";
import { Banner } from "@/components/Reload";
import { SessionList } from "@/components/SessionList";
import { SessionScreen } from "@/components/SessionScreen";
import type { BootResult } from "@/pairing";
import { watchActivity } from "@/presence";
import { clearNotification } from "@/push";
import { useRoute } from "@/route";
import { trackVisualViewport } from "@/viewport";

function Gate({ children }: { children: ReactNode }) {
  return (
    <div className="flex min-h-dvh items-center justify-center p-6">
      <div className="bg-card text-card-foreground w-full max-w-sm rounded-xl border p-6 shadow-sm">
        <h1 className="text-primary voice-heading mb-3 text-xl">vigie</h1>
        <div className="text-muted-foreground space-y-2 text-sm">{children}</div>
      </div>
    </div>
  );
}

function Shell() {
  const client = getClient();
  useEffect(() => {
    trackVisualViewport();
    client.start();
  }, [client]);
  const route = useRoute();
  const [active, setActive] = useState(false);
  useEffect(() => watchActivity(setActive), []);
  const viewing = route.kind === "session" ? route.id : null;
  useEffect(() => {
    client.setPresence(viewing, active);
    if (viewing !== null && active) void clearNotification(viewing);
  }, [client, viewing, active]);
  // In a discussion only its text selects: Android re-places a selection handle
  // scrolled out of view from its screen position, which must not land on the UI.
  return (
    <div
      className={`flex h-[var(--app-height,100dvh)] flex-col overflow-hidden ${viewing !== null ? "select-none" : ""}`}
    >
      <Banner client={client} />
      {route.kind === "session" ? (
        <SessionScreen key={route.id} client={client} id={route.id} />
      ) : route.kind === "settings" ? (
        <DeviceSettings client={client} />
      ) : route.kind === "files" ? (
        <Explorer client={client} />
      ) : (
        <SessionList client={client} />
      )}
    </div>
  );
}

export function App({ boot }: { boot: Promise<BootResult> }) {
  const result = use(boot);
  switch (result.kind) {
    case "authed":
      return <Shell />;
    case "unpaired":
      return (
        <Gate>
          <p>Not paired.</p>
          <p>Open the pair URL printed by vigie on the server.</p>
        </Gate>
      );
    case "bad-token":
      return (
        <Gate>
          <p>Invalid pair token.</p>
          <p>Get a fresh URL from the server, or from a paired device.</p>
        </Gate>
      );
    case "rate-limited":
      return (
        <Gate>
          <p>Too many failed pair attempts from this address.</p>
          <p>Try again in {Math.ceil(result.retryAfterSecs / 60)} minutes.</p>
        </Gate>
      );
    case "error":
      return (
        <Gate>
          <p>{result.message}</p>
        </Gate>
      );
  }
}
