import { describe, expect, it } from "vitest";

import { nextMode } from "@/components/PermissionMode";

describe("nextMode", () => {
  it("cycles in the TUI order", () => {
    expect(nextMode("default")).toBe("acceptEdits");
    expect(nextMode("acceptEdits")).toBe("plan");
    expect(nextMode("plan")).toBe("default");
  });

  it("cycles an unknown or unlisted mode as manual", () => {
    expect(nextMode(null)).toBe("acceptEdits");
    expect(nextMode("bypassPermissions")).toBe("acceptEdits");
  });
});
