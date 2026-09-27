/**
 * Publishes the visible height as `--app-height`, so a layout sized with it keeps
 * its bottom edge above the OS keyboard where `100dvh` does not shrink (iOS).
 */
let tracking = false;

export function trackVisualViewport(): void {
  const viewport = window.visualViewport;
  if (viewport === null || tracking) return;
  tracking = true;
  const apply = (): void => {
    // A pinch zoom also shrinks the visual viewport; the layout ignores it.
    if (Math.abs(viewport.scale - 1) > 0.01) return;
    document.documentElement.style.setProperty("--app-height", `${viewport.height}px`);
    if (window.scrollY !== 0) window.scrollTo(0, 0);
  };
  viewport.addEventListener("resize", apply, { passive: true });
  apply();
}
