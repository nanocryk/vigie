import { describe, expect, it } from "vitest";

import {
  BOTH_SIDES,
  DANGER_COOLDOWN_MS,
  DEFAULT_TUNING,
  type Side,
  type SwipeContext,
  type SwipeInput,
  type SwipeState,
  chooseSide,
  idle,
  step,
  swipeView,
} from "@/swipe";

const WIDTH = 400;
const SAFE: SwipeContext = {
  width: WIDTH,
  dangerous: false,
  sides: BOTH_SIDES,
  swipes: 2,
  presses: 1,
  tuning: DEFAULT_TUNING,
};
const CONFIRM_WINDOW_MS = DEFAULT_TUNING.confirmMs;
const DANGEROUS: SwipeContext = { ...SAFE, dangerous: true };
/** A live Home row: left only, two swipes or two presses. */
const KILL: SwipeContext = { ...SAFE, sides: ["left"], presses: 2 };
/** A stopped Home row: left only, one swipe. */
const ARCHIVE: SwipeContext = { ...SAFE, sides: ["left"], swipes: 1 };
/** 20% of 400 px. */
const THRESHOLD = 80;

function run(inputs: readonly SwipeInput[], context: SwipeContext, from = idle): SwipeState {
  return inputs.reduce((state, input) => step(state, input, context), from);
}

/** A straight swipe from the middle of the panel: down, one move, up. */
function swipe(dx: number, t: number, dy = 0, duration = 100): SwipeInput[] {
  const x = 200;
  const y = 50;
  return [
    { type: "down", x, y, t },
    { type: "move", x: x + dx / 2, y: y + dy / 2, t: t + duration / 2 },
    { type: "up", x: x + dx, y: y + dy, t: t + duration },
  ];
}

function tap(t: number): SwipeInput[] {
  return swipe(3, t, 2, 50);
}

const armed = (side: Side, at: number): SwipeState => ({ kind: "armed", side, at });
const fired = (side: Side): SwipeState => ({ kind: "fired", side });

describe("chooseSide", () => {
  it("needs 20% of the width, 56 px, and mostly horizontal travel", () => {
    const tuning = DEFAULT_TUNING;
    expect(chooseSide(THRESHOLD, 0, WIDTH, tuning)).toBe("right");
    expect(chooseSide(-THRESHOLD, 0, WIDTH, tuning)).toBe("left");
    expect(chooseSide(THRESHOLD - 1, 0, WIDTH, tuning)).toBeNull();
    expect(chooseSide(THRESHOLD, THRESHOLD / 2, WIDTH, tuning)).toBe("right");
    expect(chooseSide(THRESHOLD, THRESHOLD / 2 + 1, WIDTH, tuning)).toBeNull();
    // A narrow panel falls back on the 56 px floor.
    expect(chooseSide(55, 0, 150, tuning)).toBeNull();
    expect(chooseSide(56, 0, 150, tuning)).toBe("right");
  });

  it("follows the device's distance", () => {
    const short = { ...DEFAULT_TUNING, fraction: 0.12, minPx: 40 };
    expect(chooseSide(48, 0, WIDTH, short)).toBe("right");
    expect(chooseSide(47, 0, WIDTH, short)).toBeNull();
    expect(chooseSide(40, 0, 150, short)).toBe("right");
  });
});

describe("confirm window", () => {
  it("follows the device's tuning", () => {
    const LONG: SwipeContext = { ...SAFE, tuning: { ...DEFAULT_TUNING, confirmMs: 5000 } };
    expect(run([...swipe(150, 0), ...swipe(150, 4000)], LONG)).toEqual(fired("right"));
    expect(run([...swipe(150, 0), ...swipe(150, 4000)], SAFE)).toEqual(armed("right", 4100));
  });
});

describe("per-side swipe counts", () => {
  /** An agent row: one swipe right opens it, two swipes left stop it. */
  const AGENT: SwipeContext = { ...SAFE, swipes: { right: 1, left: 2 }, presses: 2 };

  it("fires one side at once and arms the other", () => {
    expect(run(swipe(150, 0), AGENT)).toEqual(fired("right"));
    expect(run(swipe(-150, 0), AGENT)).toEqual(armed("left", 100));
    expect(run([...swipe(-150, 0), ...swipe(-150, 500)], AGENT)).toEqual(fired("left"));
  });

  it("a swipe toward the single side while the other is armed only cancels", () => {
    expect(run([...swipe(-150, 0), ...swipe(150, 500)], AGENT)).toEqual(idle);
  });
});

describe("first swipe", () => {
  it("arms the chosen side when released past the threshold", () => {
    expect(run(swipe(150, 0), SAFE)).toEqual(armed("right", 100));
    expect(run(swipe(-150, 0), SAFE)).toEqual(armed("left", 100));
  });

  it("springs back below the threshold or when mostly vertical", () => {
    expect(run(swipe(60, 0), SAFE)).toEqual(idle);
    expect(run(swipe(150, 0, 100), SAFE)).toEqual(idle);
  });

  it("chooses on release, so coming back cancels", () => {
    const inputs: SwipeInput[] = [
      { type: "down", x: 200, y: 50, t: 0 },
      { type: "move", x: 380, y: 50, t: 50 },
      { type: "up", x: 210, y: 50, t: 100 },
    ];
    expect(run(inputs, SAFE)).toEqual(idle);
  });

  it("drops the gesture when the browser cancels it", () => {
    const inputs: SwipeInput[] = [
      { type: "down", x: 200, y: 50, t: 0 },
      { type: "move", x: 300, y: 50, t: 50 },
      { type: "cancel", t: 60 },
    ];
    expect(run(inputs, SAFE)).toEqual(idle);
  });
});

describe("second swipe", () => {
  it("fires on a second swipe in the same direction", () => {
    expect(run([...swipe(150, 0), ...swipe(150, 500)], SAFE)).toEqual(fired("right"));
    expect(run([...swipe(-150, 0), ...swipe(-160, 500)], SAFE)).toEqual(fired("left"));
  });

  it("cancels on an opposite swipe", () => {
    expect(run([...swipe(150, 0), ...swipe(-150, 500)], SAFE)).toEqual(idle);
  });

  it("cancels on a tap", () => {
    expect(run([...swipe(150, 0), ...tap(500)], SAFE)).toEqual(idle);
  });

  it("stays armed after a short or vertical movement", () => {
    expect(run([...swipe(150, 0), ...swipe(60, 500)], SAFE)).toEqual(armed("right", 100));
    expect(run([...swipe(150, 0), ...swipe(10, 500, 200)], SAFE)).toEqual(armed("right", 100));
  });

  it("stays armed when the browser cancels the second gesture", () => {
    const inputs: SwipeInput[] = [
      ...swipe(150, 0),
      { type: "down", x: 200, y: 50, t: 500 },
      { type: "cancel", t: 550 },
    ];
    expect(run(inputs, SAFE)).toEqual(armed("right", 100));
  });

  it("times out after the confirmation window", () => {
    const end = 100 + CONFIRM_WINDOW_MS;
    expect(run([...swipe(150, 0), { type: "tick", t: end - 1 }], SAFE)).toEqual(
      armed("right", 100),
    );
    expect(run([...swipe(150, 0), { type: "tick", t: end }], SAFE)).toEqual(idle);
    // Released after the window: the second swipe only arms again.
    expect(run([...swipe(150, 0), ...swipe(150, end - 50)], SAFE)).toEqual(
      armed("right", end + 50),
    );
  });

  it("ignores everything once fired", () => {
    const done = run([...swipe(150, 0), ...swipe(150, 500)], SAFE);
    expect(run([...swipe(-150, 700), { type: "tick", t: 9000 }], SAFE, done)).toEqual(
      fired("right"),
    );
  });
});

describe("dangerous asks", () => {
  it("ignore a second swipe during the cooldown", () => {
    const early = 100 + DANGER_COOLDOWN_MS - 150;
    expect(run([...swipe(150, 0), ...swipe(150, early)], DANGEROUS)).toEqual(armed("right", 100));
  });

  it("fire on a second swipe after the cooldown", () => {
    const late = 100 + DANGER_COOLDOWN_MS - 100;
    expect(run([...swipe(150, 0), ...swipe(150, late)], DANGEROUS)).toEqual(fired("right"));
  });
});

describe("buttons", () => {
  const press = (side: Side, t: number): SwipeInput => ({ type: "press", side, t });

  it("answer at once on a safe ask", () => {
    expect(run([press("left", 0)], SAFE)).toEqual(fired("left"));
  });

  it("ask again after the cooldown on a dangerous ask", () => {
    expect(run([press("right", 0)], DANGEROUS)).toEqual(armed("right", 0));
    expect(run([press("right", 0), press("right", 500)], DANGEROUS)).toEqual(armed("right", 0));
    expect(run([press("right", 0), press("right", DANGER_COOLDOWN_MS)], DANGEROUS)).toEqual(
      fired("right"),
    );
    expect(run([press("right", 0), press("left", 1500)], DANGEROUS)).toEqual(idle);
    expect(run([press("right", 0), { type: "tick", t: CONFIRM_WINDOW_MS }], DANGEROUS)).toEqual(
      idle,
    );
  });
});

describe("left-only double swipe", () => {
  it("arms and fires on the left only", () => {
    expect(run(swipe(-150, 0), KILL)).toEqual(armed("left", 100));
    expect(run([...swipe(-150, 0), ...swipe(-150, 500)], KILL)).toEqual(fired("left"));
    expect(run(swipe(150, 0), KILL)).toEqual(idle);
  });

  it("cancels on a right swipe, a tap or the timeout", () => {
    expect(run([...swipe(-150, 0), ...swipe(150, 500)], KILL)).toEqual(idle);
    expect(run([...swipe(-150, 0), ...tap(500)], KILL)).toEqual(idle);
    expect(run([...swipe(-150, 0), { type: "tick", t: 100 + CONFIRM_WINDOW_MS }], KILL)).toEqual(
      idle,
    );
  });

  it("takes two presses", () => {
    const press = (t: number): SwipeInput => ({ type: "press", side: "left", t });
    expect(run([press(0)], KILL)).toEqual(armed("left", 0));
    expect(run([press(0), press(200)], KILL)).toEqual(fired("left"));
  });
});

describe("left-only single swipe", () => {
  it("fires on the first left swipe", () => {
    expect(run(swipe(-150, 0), ARCHIVE)).toEqual(fired("left"));
  });

  it("ignores right swipes, short swipes and vertical movement", () => {
    expect(run(swipe(150, 0), ARCHIVE)).toEqual(idle);
    expect(run(swipe(-60, 0), ARCHIVE)).toEqual(idle);
    expect(run(swipe(-150, 0, 100), ARCHIVE)).toEqual(idle);
  });

  it("never follows the finger to the right", () => {
    const right = run(swipe(150, 0).slice(0, 2), ARCHIVE);
    expect(swipeView(right, 50, ARCHIVE)).toMatchObject({ offset: 0, lean: 0 });
    const left = run(swipe(-150, 0).slice(0, 2), ARCHIVE);
    expect(swipeView(left, 50, ARCHIVE).offset).toBe(-75);
  });
});

describe("swipeView", () => {
  it("leans toward the side as the finger travels", () => {
    const halfway = run(swipe(150, 0).slice(0, 2), SAFE);
    const view = swipeView(halfway, 50, SAFE);
    expect(view.dragging).toBe(true);
    expect(view.offset).toBe(75);
    expect(view.lean).toBeCloseTo(75 / THRESHOLD);
    const far = step(halfway, { type: "move", x: 0, y: 50, t: 80 }, SAFE);
    expect(swipeView(far, 80, SAFE).lean).toBe(-1);
    const vertical = step(halfway, { type: "move", x: 210, y: 150, t: 80 }, SAFE);
    expect(swipeView(vertical, 80, SAFE).lean).toBe(0);
  });

  it("shows the armed label, cooldown and elapsed window", () => {
    const state = armed("left", 0);
    expect(swipeView(state, 1500, SAFE).armed).toEqual({
      side: "left",
      label: "Confirm?",
      ready: true,
      elapsed: 0.5,
    });
    expect(swipeView(state, 500, DANGEROUS).armed).toMatchObject({
      label: "Are you sure?",
      ready: false,
    });
    expect(swipeView(state, DANGER_COOLDOWN_MS, DANGEROUS).armed?.ready).toBe(true);
    expect(swipeView(state, CONFIRM_WINDOW_MS, SAFE).armed).toBeNull();
  });
});
