import { describe, expect, it } from "vitest";

import { DEFAULT_TUNING } from "@/swipe";
import { DEFAULT_SWIPE_PREFS, parseSwipePrefs, tuningOf } from "@/swipePrefs";

describe("swipe preferences", () => {
  it("fall back to the defaults field by field", () => {
    expect(parseSwipePrefs(null)).toEqual(DEFAULT_SWIPE_PREFS);
    expect(parseSwipePrefs("not json")).toEqual(DEFAULT_SWIPE_PREFS);
    expect(parseSwipePrefs("[1]")).toEqual(DEFAULT_SWIPE_PREFS);
    expect(
      parseSwipePrefs('{"distance": "long", "confirmWindow": 5000, "safeAsk": "one"}'),
    ).toEqual({ distance: "long", confirmWindow: 5000, safeAsk: "one" });
    expect(parseSwipePrefs('{"distance": "far", "confirmWindow": 4000, "safeAsk": "one"}')).toEqual(
      { ...DEFAULT_SWIPE_PREFS, safeAsk: "one" },
    );
  });

  it("tune the gesture as the built-in default when left alone", () => {
    expect(tuningOf(DEFAULT_SWIPE_PREFS)).toEqual(DEFAULT_TUNING);
  });

  it("tune distance and window from the choices", () => {
    expect(tuningOf({ ...DEFAULT_SWIPE_PREFS, distance: "short", confirmWindow: 2000 })).toEqual({
      fraction: 0.12,
      minPx: 40,
      confirmMs: 2000,
    });
  });
});
