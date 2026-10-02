// State machine of the Send button's gestures on touch screens: a tap or a swipe
// right sends, a swipe left stops, a swipe up opens the menu. Single swipes, no
// confirmation. Pure: positions come in as inputs, so tests drive it directly.

import { HORIZONTAL_RATIO, TAP_SLOP_PX } from "@/swipe";

export type SendAction = "send" | "stop" | "menu";

/** Travel choosing a direction, in pixels: fixed, whatever the button's width. */
export const SEND_SWIPE_PX = 40;

export type SendGestureState =
  { kind: "idle" } | { kind: "dragging"; x0: number; y0: number; dx: number; dy: number };

export type SendGestureInput =
  | { type: "down"; x: number; y: number }
  | { type: "move"; x: number; y: number }
  | { type: "up"; x: number; y: number }
  /** The browser took the pointer over. */
  | { type: "cancel" };

export interface SendGestureContext {
  /** A turn runs and no stop is on its way: a swipe left stops it. */
  stoppable: boolean;
}

export const idleGesture: SendGestureState = { kind: "idle" };

/** The swipe a movement makes, or null below the threshold, off-axis, or toward a disabled stop. */
export function swipeAction(
  dx: number,
  dy: number,
  context: SendGestureContext,
): SendAction | null {
  const across = Math.abs(dx);
  const up = -dy;
  if (across >= SEND_SWIPE_PX && across >= HORIZONTAL_RATIO * Math.abs(dy)) {
    if (dx > 0) return "send";
    return context.stoppable ? "stop" : null;
  }
  if (up >= SEND_SWIPE_PX && up >= HORIZONTAL_RATIO * across) return "menu";
  return null;
}

function isTap(dx: number, dy: number): boolean {
  return Math.abs(dx) < TAP_SLOP_PX && Math.abs(dy) < TAP_SLOP_PX;
}

/** The next state, and the action a release fires, if any. */
export function sendStep(
  state: SendGestureState,
  input: SendGestureInput,
  context: SendGestureContext,
): { state: SendGestureState; action: SendAction | null } {
  switch (input.type) {
    case "down":
      if (state.kind === "dragging") return { state, action: null };
      return {
        state: { kind: "dragging", x0: input.x, y0: input.y, dx: 0, dy: 0 },
        action: null,
      };
    case "move":
      if (state.kind !== "dragging") return { state, action: null };
      return {
        state: { ...state, dx: input.x - state.x0, dy: input.y - state.y0 },
        action: null,
      };
    case "up": {
      if (state.kind !== "dragging") return { state, action: null };
      const dx = input.x - state.x0;
      const dy = input.y - state.y0;
      return { state: idleGesture, action: isTap(dx, dy) ? "send" : swipeAction(dx, dy, context) };
    }
    case "cancel":
      return { state: idleGesture, action: null };
  }
}

export interface SendGestureView {
  /** Offset of the button's face, following the finger up to the threshold along one axis. */
  x: number;
  y: number;
  /** The action a release here would fire, a tap excluded. */
  latched: SendAction | null;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

export function sendGestureView(
  state: SendGestureState,
  context: SendGestureContext,
): SendGestureView {
  if (state.kind === "idle") return { x: 0, y: 0, latched: null };
  const { dx, dy } = state;
  const latched = swipeAction(dx, dy, context);
  if (Math.abs(dx) >= Math.abs(dy)) {
    const min = context.stoppable ? -SEND_SWIPE_PX : 0;
    return { x: clamp(dx, min, SEND_SWIPE_PX), y: 0, latched };
  }
  return { x: 0, y: clamp(dy, -SEND_SWIPE_PX, 0), latched };
}
