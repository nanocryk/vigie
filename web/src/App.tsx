import { type ReactNode, use } from "react";

import type { BootResult } from "@/pairing";

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
  return (
    <div className="flex min-h-dvh flex-col">
      <header className="flex items-center justify-between border-b px-4 py-3">
        <span className="text-primary voice-heading text-lg">vigie2</span>
        {label !== null && <span className="text-muted-foreground voice-ui text-sm">{label}</span>}
      </header>
      <main className="flex flex-1 items-center justify-center p-6">
        <p className="text-muted-foreground">No sessions yet</p>
      </main>
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
