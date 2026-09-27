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
