/** Without input for this long, a visible page counts as left alone. */
export const IDLE_AFTER_MS = 2 * 60_000;

// No `scroll`: the discussion scrolls itself as a reply streams in.
const INPUT_EVENTS = ["pointerdown", "pointermove", "keydown", "wheel", "touchstart"];

/** The user is at this page: it is visible and got input recently. */
export function isActive(lastInput: number, visible: boolean, now: number): boolean {
  return visible && now - lastInput < IDLE_AFTER_MS;
}

/**
 * Calls `onChange` with the page's activity now and on every flip. A wake from
 * the lock screen or a notification counts as input. Returns the teardown.
 */
export function watchActivity(onChange: (active: boolean) => void): () => void {
  let lastInput = Date.now();
  let active: boolean | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const check = (): void => {
    const now = Date.now();
    const next = isActive(lastInput, document.visibilityState === "visible", now);
    if (timer !== null) clearTimeout(timer);
    timer = next ? setTimeout(check, lastInput + IDLE_AFTER_MS - now) : null;
    if (next === active) return;
    active = next;
    onChange(next);
  };
  const input = (): void => {
    lastInput = Date.now();
    if (active !== true) check();
  };
  const wake = (): void => {
    if (document.visibilityState === "visible") lastInput = Date.now();
    check();
  };

  for (const name of INPUT_EVENTS) {
    window.addEventListener(name, input, { passive: true, capture: true });
  }
  document.addEventListener("visibilitychange", wake);
  window.addEventListener("pageshow", wake);
  window.addEventListener("focus", wake);
  check();
  return () => {
    for (const name of INPUT_EVENTS) {
      window.removeEventListener(name, input, { capture: true });
    }
    document.removeEventListener("visibilitychange", wake);
    window.removeEventListener("pageshow", wake);
    window.removeEventListener("focus", wake);
    if (timer !== null) clearTimeout(timer);
  };
}
