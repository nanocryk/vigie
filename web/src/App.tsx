import { type ReactNode, use, useEffect } from "react";

import { getClient } from "@/client";
import { NewVersionBanner } from "@/components/Reload";
import { SessionList } from "@/components/SessionList";
import { SessionScreen } from "@/components/SessionScreen";
import type { BootResult } from "@/pairing";
import { useRoute } from "@/route";
import { trackVisualViewport } from "@/viewport";

function Gate({ children }: { children: ReactNode }) {
  return (
    <div className="flex min-h-dvh items-center justify-center p-6">
      <div className="bg-card text-card-foreground w-full max-w-sm rounded-xl border p-6 shadow-sm">
        <h1 className="text-primary voice-heading mb-3 text-xl">vigie2</h1>
        <div className="text-muted-foreground space-y-2 text-sm">{children}</div>
      </div>
    </div>
  );
}

function Shell({ label }: { label: string | null }) {
  const client = getClient();
  useEffect(() => {
    trackVisualViewport();
    client.start();
  }, [client]);
  const route = useRoute();
  return (
    <div className="flex h-[var(--app-height,100dvh)] flex-col overflow-hidden">
      <NewVersionBanner />
      {route.kind === "session" ? (
        <SessionScreen key={route.id} client={client} id={route.id} />
      ) : (
        <SessionList client={client} label={label} />
      )}
    </div>
  );
}

export function App({ boot }: { boot: Promise<BootResult> }) {
  const result = use(boot);
  switch (result.kind) {
    case "authed":
      return <Shell label={result.label} />;
    case "unpaired":
      return (
        <Gate>
          <p>Not paired.</p>
          <p>Open the pair URL printed by vigie2 on the server.</p>
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
