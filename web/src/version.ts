import { useEffect, useState } from "react";

const POLL_MS = 60_000;

export function parseVersion(raw: unknown, label: string): string {
  if (typeof raw !== "object" || raw === null || !("build" in raw) || typeof raw.build !== "string") {
    throw new Error(`${label}: expected {build: string}: ${JSON.stringify(raw)}`);
  }
  return raw.build;
}

async function fetchBuild(): Promise<string | null> {
  try {
    const response = await fetch("api/version", { cache: "no-store" });
    if (!response.ok) return null;
    return parseVersion(await response.json(), "version");
  } catch (err) {
    console.warn("version check failed", err);
    return null;
  }
}

/** True once the server reports a build other than the one this page loaded with. */
export function useNewVersion(): boolean {
  const [stale, setStale] = useState(false);
  useEffect(() => {
    let loaded: string | null = null;
    let cancelled = false;
    const check = async () => {
      if (document.visibilityState !== "visible") return;
      const build = await fetchBuild();
      if (cancelled || build === null) return;
      if (loaded === null) loaded = build;
      else if (build !== loaded) setStale(true);
    };
    void check();
    // A resumed PWA does not reliably fire visibilitychange, so any wake signal re-checks.
    const wake = () => void check();
    document.addEventListener("visibilitychange", wake);
    window.addEventListener("pageshow", wake);
    window.addEventListener("focus", wake);
    window.addEventListener("online", wake);
    const timer = window.setInterval(wake, POLL_MS);
    return () => {
      cancelled = true;
      document.removeEventListener("visibilitychange", wake);
      window.removeEventListener("pageshow", wake);
      window.removeEventListener("focus", wake);
      window.removeEventListener("online", wake);
      window.clearInterval(timer);
    };
  }, []);
  return stale;
}

export function reload(): void {
  location.reload();
}
