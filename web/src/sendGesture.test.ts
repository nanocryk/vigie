import { describe, expect, it } from "vitest";

import {
  SEND_SWIPE_PX,
  type SendAction,
  type SendGestureContext,
  type SendGestureInput,
  idleGesture,
  sendGestureView,
  sendStep,
} from "@/sendGesture";

const RUNNING: SendGestureContext = { stoppable: true };
const IDLE_TURN: SendGestureContext = { stoppable: false };

/** Actions fired by a pointer sequence, in order. */
function run(inputs: readonly SendGestureInput[], context: SendGestureContext): SendAction[] {
  let state = idleGesture;
  const actions: SendAction[] = [];
  for (const input of inputs) {
    const next = sendStep(state, input, context);
    state = next.state;
    if (next.action !== null) actions.push(next.action);
  }
  return actions;
}

/** Down in the button, two moves, up. */
function swipe(dx: number, dy: number): SendGestureInput[] {
  const x = 300;
  const y = 700;
  return [
    { type: "down", x, y },
    { type: "move", x: x + dx / 2, y: y + dy / 2 },
    { type: "move", x: x + dx, y: y + dy },
    { type: "up", x: x + dx, y: y + dy },
  ];
}

describe("sendStep", () => {
  it("sends on a tap", () => {
    expect(run(swipe(0, 0), IDLE_TURN)).toEqual(["send"]);
    expect(run(swipe(4, -3), RUNNING)).toEqual(["send"]);
  });

  it("sends on a swipe right past the fixed threshold", () => {
    expect(run(swipe(SEND_SWIPE_PX, 0), IDLE_TURN)).toEqual(["send"]);
    expect(run(swipe(SEND_SWIPE_PX - 1, 0), IDLE_TURN)).toEqual([]);
  });

  it("stops on a swipe left only while a turn runs", () => {
    expect(run(swipe(-SEND_SWIPE_PX, 0), RUNNING)).toEqual(["stop"]);
    expect(run(swipe(-SEND_SWIPE_PX, 0), IDLE_TURN)).toEqual([]);
  });

  it("opens the menu on a swipe up", () => {
    expect(run(swipe(0, -SEND_SWIPE_PX), IDLE_TURN)).toEqual(["menu"]);
    expect(run(swipe(5, -80), RUNNING)).toEqual(["menu"]);
    expect(run(swipe(0, SEND_SWIPE_PX * 2), RUNNING)).toEqual([]);
  });

  it("fires nothing on a diagonal", () => {
    expect(run(swipe(60, -50), RUNNING)).toEqual([]);
    expect(run(swipe(-60, -50), RUNNING)).toEqual([]);
  });

  it("fires nothing when the browser cancels", () => {
    const [down, move] = swipe(80, 0);
    if (down === undefined || move === undefined) throw new Error("swipe: short sequence");
    expect(run([down, move, { type: "cancel" }], RUNNING)).toEqual([]);
  });

  it("counts the release point, not the farthest point", () => {
    const inputs: SendGestureInput[] = [
      { type: "down", x: 0, y: 0 },
      { type: "move", x: 80, y: 0 },
      { type: "up", x: 20, y: 0 },
    ];
    expect(run(inputs, RUNNING)).toEqual([]);
  });

  it("ignores a second pointer going down mid-drag", () => {
    const inputs: SendGestureInput[] = [
      { type: "down", x: 0, y: 0 },
      { type: "move", x: -30, y: 0 },
      { type: "down", x: 200, y: 0 },
      { type: "up", x: -50, y: 0 },
    ];
    expect(run(inputs, RUNNING)).toEqual(["stop"]);
  });

  it("fires once per gesture", () => {
    expect(run([...swipe(80, 0), { type: "up", x: 0, y: 0 }], RUNNING)).toEqual(["send"]);
  });
});

describe("sendGestureView", () => {
  const drag = (dx: number, dy: number) => ({ kind: "dragging" as const, x0: 0, y0: 0, dx, dy });

  it("rests at the origin", () => {
    expect(sendGestureView(idleGesture, RUNNING)).toEqual({ x: 0, y: 0, latched: null });
  });

  it("follows the dominant axis up to the threshold", () => {
    expect(sendGestureView(drag(20, 5), RUNNING)).toEqual({ x: 20, y: 0, latched: null });
    expect(sendGestureView(drag(200, 5), RUNNING)).toEqual({
      x: SEND_SWIPE_PX,
      y: 0,
      latched: "send",
    });
    expect(sendGestureView(drag(3, -200), RUNNING)).toEqual({
      x: 0,
      y: -SEND_SWIPE_PX,
      latched: "menu",
    });
    expect(sendGestureView(drag(0, 60), RUNNING)).toEqual({ x: 0, y: 0, latched: null });
  });

  it("does not follow a swipe left when stopping is impossible", () => {
    expect(sendGestureView(drag(-200, 0), IDLE_TURN)).toEqual({ x: 0, y: 0, latched: null });
    expect(sendGestureView(drag(-200, 0), RUNNING)).toEqual({
      x: -SEND_SWIPE_PX,
      y: 0,
      latched: "stop",
    });
  });
});
