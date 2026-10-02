import { type ReactNode, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

import {
  BOTH_SIDES,
  type Side,
  type SwipeContext,
  type SwipeInput,
  type SwipeRules,
  type SwipeState,
  type SwipeView,
  TAP_SLOP_PX,
  idle,
  step,
  swipeView,
} from "@/swipe";
import { tuningOf, useSwipePrefs } from "@/swipePrefs";

/** Controls inside the panel keep their own pointer handling. */
const OWN_POINTER = "button, a, input, textarea, select, [data-swipe-ignore]";
const SPRING_BACK = "transform 200ms ease-out";

const FILL: Record<Side, string> = {
  left: "var(--swipe-deny)",
  right: "var(--swipe-accept)",
};
/** Snapped to once the travel reaches the threshold, so the choice feels latched. */
const LATCHED: Record<Side, string> = {
  left: "var(--swipe-deny-latched)",
  right: "var(--swipe-accept-latched)",
};
const TEXT: Record<Side, string> = {
  left: "text-swipe-deny-strong",
  right: "text-swipe-accept-strong",
};
const BUTTON = "voice-ui rounded-[5px] px-4 py-2 text-sm disabled:opacity-50";
const BUTTON_STYLE: Record<Side, string> = {
  left: "border-swipe-deny-strong text-swipe-deny-strong border",
  right: "bg-swipe-accept-strong text-card",
};

const UNDECIDED = "var(--swipe-undecided)";

/** Panel fill: undecided yellow at rest, toward the side color while dragging, then the sweep back to undecided once armed. */
function background(view: SwipeView): string {
  if (view.fired !== null) return LATCHED[view.fired];
  let base: string;
  if (view.dragging) {
    const leanSide: Side = view.lean < 0 ? "left" : "right";
    if (Math.abs(view.lean) >= 1) {
      base = LATCHED[leanSide];
    } else {
      const from = view.armed === null ? UNDECIDED : LATCHED[view.armed.side];
      const share = Math.round(Math.abs(view.lean) * 100);
      base = `color-mix(in oklch, ${FILL[leanSide]} ${share}%, ${from})`;
    }
  } else if (view.armed !== null) {
    base = LATCHED[view.armed.side];
  } else {
    return UNDECIDED;
  }
  if (view.armed === null) return base;
  const neutral = view.armed.elapsed * 100;
  const direction = view.armed.side === "right" ? "to right" : "to left";
  return `linear-gradient(${direction}, ${UNDECIDED} ${neutral}%, ${base} ${neutral}%)`;
}

/**
 * Drives `step` from the pointer events of the element `surface` is attached to.
 * `onConfirm` is called once, when the machine fires. Remount the owner (new
 * `key`) to start over.
 */
export function useSwipe(rules: SwipeRules, onConfirm: (side: Side) => void) {
  const tuning = tuningOf(useSwipePrefs());
  const [state, setState] = useState<SwipeState>(idle);
  const [now, setNow] = useState(() => performance.now());
  const [width, setWidth] = useState(0);
  const surface = useRef<HTMLDivElement>(null);
  const stateRef = useRef<SwipeState>(idle);
  const contextRef = useRef<SwipeContext>({ width: 0, tuning, ...rules });
  const confirmRef = useRef(onConfirm);

  useLayoutEffect(() => {
    confirmRef.current = onConfirm;
    contextRef.current = { ...rules, tuning, width: contextRef.current.width };
  });

  const dispatch = useCallback((input: SwipeInput) => {
    const previous = stateRef.current;
    const next = step(previous, input, contextRef.current);
    if (next === previous) return;
    stateRef.current = next;
    setState(next);
    setNow(input.t);
    if (next.kind === "fired" && previous.kind !== "fired") confirmRef.current(next.side);
  }, []);

  const ticking = state.kind === "armed" || (state.kind === "dragging" && state.armed !== null);
  useEffect(() => {
    if (!ticking) return;
    let frame = 0;
    const loop = () => {
      const t = performance.now();
      setNow(t);
      dispatch({ type: "tick", t });
      frame = requestAnimationFrame(loop);
    };
    frame = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(frame);
  }, [ticking, dispatch]);

  useEffect(() => {
    const element = surface.current;
    if (element === null) return;
    let active: number | null = null;
    let startX = 0;
    let startY = 0;
    const down = (event: PointerEvent) => {
      if (active !== null || !event.isPrimary) return;
      if (event.target instanceof Element && event.target.closest(OWN_POINTER) !== null) return;
      active = event.pointerId;
      startX = event.clientX;
      startY = event.clientY;
      const panelWidth = element.getBoundingClientRect().width;
      contextRef.current = { ...contextRef.current, width: panelWidth };
      setWidth(panelWidth);
      dispatch({ type: "down", x: event.clientX, y: event.clientY, t: performance.now() });
    };
    const move = (event: PointerEvent) => {
      if (event.pointerId !== active) return;
      // Captured only once it is a drag, so a tap still clicks what it landed on.
      const travel = Math.hypot(event.clientX - startX, event.clientY - startY);
      if (travel >= TAP_SLOP_PX && !element.hasPointerCapture(event.pointerId)) {
        element.setPointerCapture(event.pointerId);
      }
      dispatch({ type: "move", x: event.clientX, y: event.clientY, t: performance.now() });
    };
    const finish = (event: PointerEvent, input: SwipeInput) => {
      if (event.pointerId !== active) return;
      active = null;
      if (element.hasPointerCapture(event.pointerId)) element.releasePointerCapture(event.pointerId);
      dispatch(input);
    };
    const up = (event: PointerEvent) =>
      finish(event, { type: "up", x: event.clientX, y: event.clientY, t: performance.now() });
    const cancel = (event: PointerEvent) => finish(event, { type: "cancel", t: performance.now() });
    element.addEventListener("pointerdown", down, { passive: true });
    element.addEventListener("pointermove", move, { passive: true });
    element.addEventListener("pointerup", up, { passive: true });
    element.addEventListener("pointercancel", cancel, { passive: true });
    return () => {
      element.removeEventListener("pointerdown", down);
      element.removeEventListener("pointermove", move);
      element.removeEventListener("pointerup", up);
      element.removeEventListener("pointercancel", cancel);
    };
  }, [dispatch]);

  const view = swipeView(state, now, { ...rules, tuning, width });
  const press = (side: Side) => dispatch({ type: "press", side, t: performance.now() });
  return { surface, view, press };
}

export interface SwipeProps {
  labels: Record<Side, string>;
  dangerous: boolean;
  /** Called once, when the choice is confirmed. */
  onConfirm: (side: Side) => void;
  /** Shown immediately left of the right button. */
  beforeRight?: ReactNode;
  children: ReactNode;
}

/**
 * A panel confirmed by two swipes in the same direction (one when the device
 * allows it for a choice that is not dangerous), or by the buttons under it.
 * Remount it (new `key`) to start over.
 */
export function Swipe({ labels, dangerous, onConfirm, beforeRight, children }: SwipeProps) {
  const { safeAsk } = useSwipePrefs();
  const swipes = !dangerous && safeAsk === "one" ? 1 : 2;
  const { surface, view, press } = useSwipe(
    { dangerous, sides: BOTH_SIDES, swipes, presses: 1 },
    onConfirm,
  );
  const sideButton = (side: Side) => {
    const armed = view.armed?.side === side ? view.armed : null;
    return (
      <button
        type="button"
        disabled={armed === null ? view.fired !== null : !armed.ready}
        onClick={() => press(side)}
        className={`${BUTTON_STYLE[side]} ${BUTTON}`}
      >
        {armed === null ? labels[side] : armed.label}
      </button>
    );
  };

  return (
    // Flex columns down to the children, so a `min-h-0` child can shrink in a bounded frame.
    <div className="flex min-h-0 flex-col gap-2">
      <div
        ref={surface}
        // touch-action only reaches down to the nearest scroll container, so inner
        // scroll boxes (a long plan) need pan-y too or the browser takes the swipe.
        className="relative flex min-h-0 touch-pan-y flex-col overflow-hidden rounded-[5px] border select-none [&_*]:touch-pan-y"
        style={{ background: background(view) }}
      >
        <div
          className="flex min-h-0 flex-col p-2"
          style={{
            transform: `translateX(${view.offset}px)`,
            transition: view.dragging ? "none" : SPRING_BACK,
          }}
        >
          {children}
        </div>
        {view.armed !== null && (
          <span
            className={`bg-card voice-heading pointer-events-none absolute top-1.5 rounded-[5px] px-2 py-0.5 text-sm shadow-sm ${view.armed.side === "right" ? "right-2" : "left-2"} ${view.armed.ready ? TEXT[view.armed.side] : "text-muted-foreground opacity-60"}`}
          >
            {view.armed.label}
          </span>
        )}
      </div>

      <div className="flex items-center justify-between gap-2">
        {sideButton("left")}
        <span className="text-muted-foreground voice-ui text-xs">
          {swipes === 1 ? "swipe once" : "swipe twice"}
        </span>
        <div className="flex items-center gap-2">
          {beforeRight}
          {sideButton("right")}
        </div>
      </div>
    </div>
  );
}
