import { useEffect, useState } from "react";

/**
 * The current time, refreshed when the app comes back to the foreground
 * (whichever of `visibilitychange`, `pageshow` or `focus` fires first), never on a timer.
 */
export function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const wake = () => {
      if (document.visibilityState === "visible") setNow(Date.now());
    };
    document.addEventListener("visibilitychange", wake);
    window.addEventListener("pageshow", wake);
    window.addEventListener("focus", wake);
    return () => {
      document.removeEventListener("visibilitychange", wake);
      window.removeEventListener("pageshow", wake);
      window.removeEventListener("focus", wake);
    };
  }, []);
  return now;
}

/** The current time, ticking every `periodMs` and refreshed on return to the foreground. */
export function useClock(periodMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const tick = () => setNow(Date.now());
    const wake = () => {
      if (document.visibilityState === "visible") tick();
    };
    const timer = setInterval(tick, periodMs);
    document.addEventListener("visibilitychange", wake);
    window.addEventListener("pageshow", wake);
    window.addEventListener("focus", wake);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", wake);
      window.removeEventListener("pageshow", wake);
      window.removeEventListener("focus", wake);
    };
  }, [periodMs]);
  return now;
}
