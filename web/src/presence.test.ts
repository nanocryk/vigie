import { describe, expect, it } from "vitest";

import { IDLE_AFTER_MS, isActive } from "@/presence";

describe("isActive", () => {
  it("holds while the page is visible and input is recent", () => {
    expect(isActive(0, true, IDLE_AFTER_MS - 1)).toBe(true);
    expect(isActive(0, true, IDLE_AFTER_MS)).toBe(false);
    expect(isActive(0, false, 1)).toBe(false);
  });
});
