// State machine of the swipe confirmation, double or single. Pure: positions and
// times come in as inputs, so it runs the same under a finger, a mouse button or a test.

export type Side = "left" | "right";

/** A side needs this share of the panel width... */
export const SIDE_FRACTION = 0.2;
/** ...and at least this many pixels of horizontal travel. */
export const SIDE_MIN_PX = 56;
/** Horizontal travel must be at least this multiple of the vertical travel. */
export const HORIZONTAL_RATIO = 2;
/** Movement under this, in pixels, counts as a tap. */
export const TAP_SLOP_PX = 10;
/** Time after the first swipe during which the second one confirms. */
export const CONFIRM_WINDOW_MS = 3000;
/** Time after the first swipe before a dangerous choice can be confirmed. */
export const DANGER_COOLDOWN_MS = 1000;

export interface Armed {
  side: Side;
  /** Time of the first swipe. */
  at: number;
}

export type SwipeState =
  | { kind: "idle" }
  /** A finger is down; `armed` is set when this is the second swipe. */
  | { kind: "dragging"; x0: number; y0: number; dx: number; dy: number; armed: Armed | null }
  | { kind: "armed"; side: Side; at: number }
  | { kind: "fired"; side: Side };

export type SwipeInput =
  | { type: "down"; x: number; y: number; t: number }
  | { type: "move"; x: number; y: number; t: number }
  | { type: "up"; x: number; y: number; t: number }
  /** The browser took the pointer over, e.g. for a scroll. */
  | { type: "cancel"; t: number }
  | { type: "tick"; t: number }
  /** A button press on a fine pointer. */
  | { type: "press"; side: Side; t: number };

export interface SwipeContext {
  /** Panel width in pixels. */
  width: number;
  dangerous: boolean;
  /** Sides a swipe can choose; a swipe toward another only cancels an arming. */
  sides: readonly Side[];
  /** 2: the first swipe arms, the second fires. 1: a swipe fires at once. Can differ per side. */
  swipes: SwipeCount | Readonly<Record<Side, SwipeCount>>;
  /** 2: the first press arms, the second fires. A dangerous choice always takes 2. */
  presses: 1 | 2;
}

export type SwipeCount = 1 | 2;

export type SwipeRules = Omit<SwipeContext, "width">;

function swipesFor(context: SwipeContext, side: Side): SwipeCount {
  return typeof context.swipes === "number" ? context.swipes : context.swipes[side];
}

export const BOTH_SIDES: readonly Side[] = ["left", "right"];

export const idle: SwipeState = { kind: "idle" };

export function sideThreshold(width: number): number {
  return Math.max(SIDE_FRACTION * width, SIDE_MIN_PX);
}

/** The side a movement chooses, or null below the threshold or when not mostly horizontal. */
export function chooseSide(dx: number, dy: number, width: number): Side | null {
  const travel = Math.abs(dx);
  if (travel < sideThreshold(width) || travel < HORIZONTAL_RATIO * Math.abs(dy)) return null;
  return dx > 0 ? "right" : "left";
}

function isTap(dx: number, dy: number): boolean {
  return Math.abs(dx) < TAP_SLOP_PX && Math.abs(dy) < TAP_SLOP_PX;
}

function cooled(armed: Armed, t: number, dangerous: boolean): boolean {
  return !dangerous || t >= armed.at + DANGER_COOLDOWN_MS;
}

/** Drops an arming whose confirmation window has passed. */
function expire(state: SwipeState, t: number): SwipeState {
  if (state.kind === "armed" && t >= state.at + CONFIRM_WINDOW_MS) return idle;
  if (state.kind === "dragging" && state.armed !== null) {
    if (t >= state.armed.at + CONFIRM_WINDOW_MS) return { ...state, armed: null };
  }
  return state;
}

/** Second choice of `side` after the first swipe: fires once the cooldown is over. */
function confirm(armed: Armed, side: Side, t: number, context: SwipeContext): SwipeState {
  if (side !== armed.side) return idle;
  if (!cooled(armed, t, context.dangerous)) return { kind: "armed", ...armed };
  return { kind: "fired", side };
}

function release(
  drag: Extract<SwipeState, { kind: "dragging" }>,
  t: number,
  context: SwipeContext,
): SwipeState {
  const side = chooseSide(drag.dx, drag.dy, context.width);
  const armed = drag.armed;
  if (armed === null) {
    if (side === null || !context.sides.includes(side)) return idle;
    return swipesFor(context, side) === 1
      ? { kind: "fired", side }
      : { kind: "armed", side, at: t };
  }
  if (side !== null) return confirm(armed, side, t, context);
  if (isTap(drag.dx, drag.dy)) return idle;
  return { kind: "armed", ...armed };
}

export function step(state: SwipeState, input: SwipeInput, context: SwipeContext): SwipeState {
  if (state.kind === "fired") return state;
  const current = expire(state, input.t);
  switch (input.type) {
    case "down":
      if (current.kind === "dragging") return current;
      return {
        kind: "dragging",
        x0: input.x,
        y0: input.y,
        dx: 0,
        dy: 0,
        armed: current.kind === "armed" ? { side: current.side, at: current.at } : null,
      };
    case "move":
      if (current.kind !== "dragging") return current;
      return { ...current, dx: input.x - current.x0, dy: input.y - current.y0 };
    case "up":
      if (current.kind !== "dragging") return current;
      return release(
        { ...current, dx: input.x - current.x0, dy: input.y - current.y0 },
        input.t,
        context,
      );
    case "cancel":
      if (current.kind !== "dragging") return current;
      return current.armed === null ? idle : { kind: "armed", ...current.armed };
    case "tick":
      return current;
    case "press":
      if (current.kind === "dragging") return current;
      if (current.kind === "armed") return confirm(current, input.side, input.t, context);
      return context.dangerous || context.presses === 2
        ? { kind: "armed", side: input.side, at: input.t }
        : { kind: "fired", side: input.side };
  }
}

export interface ArmedView {
  side: Side;
  label: string;
  /** The second swipe or press can confirm now. */
  ready: boolean;
  /** Share of the confirmation window elapsed, 0 to 1. */
  elapsed: number;
}

export interface SwipeView {
  dragging: boolean;
  /** Horizontal offset of the panel in pixels. */
  offset: number;
  /** -1 (fully left) to 1 (fully right); 0 when undecided or not mostly horizontal. */
  lean: number;
  armed: ArmedView | null;
  fired: Side | null;
}

export function armedLabel(dangerous: boolean): string {
  return dangerous ? "Are you sure?" : "Confirm?";
}

/** What to draw for `state` at time `now`. */
export function swipeView(state: SwipeState, now: number, context: SwipeContext): SwipeView {
  const current = expire(state, now);
  const armedOf = (armed: Armed): ArmedView => ({
    side: armed.side,
    label: armedLabel(context.dangerous),
    ready: cooled(armed, now, context.dangerous),
    elapsed: Math.min(1, Math.max(0, (now - armed.at) / CONFIRM_WINDOW_MS)),
  });
  switch (current.kind) {
    case "idle":
      return { dragging: false, offset: 0, lean: 0, armed: null, fired: null };
    case "dragging": {
      // A panel never follows the finger toward a side it cannot choose.
      const dx = context.sides.includes(current.dx < 0 ? "left" : "right") ? current.dx : 0;
      const horizontal = Math.abs(dx) >= HORIZONTAL_RATIO * Math.abs(current.dy);
      const reach = Math.min(1, Math.abs(dx) / sideThreshold(context.width));
      return {
        dragging: true,
        offset: dx,
        lean: horizontal ? Math.sign(dx) * reach : 0,
        armed: current.armed === null ? null : armedOf(current.armed),
        fired: null,
      };
    }
    case "armed":
      return { dragging: false, offset: 0, lean: 0, armed: armedOf(current), fired: null };
    case "fired":
      return { dragging: false, offset: 0, lean: 0, armed: null, fired: current.side };
  }
}
