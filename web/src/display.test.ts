import { describe, expect, it } from "vitest";

import { DEFAULT_DISPLAY, parseDisplay, resolveTheme } from "@/display";

describe("display preferences", () => {
  it("fall back to the defaults field by field", () => {
    expect(parseDisplay(null)).toEqual(DEFAULT_DISPLAY);
    expect(parseDisplay("not json")).toEqual(DEFAULT_DISPLAY);
    expect(parseDisplay("[1]")).toEqual(DEFAULT_DISPLAY);
    expect(parseDisplay('{"scale": 112.5, "theme": "dark"}')).toEqual({
      scale: 112.5,
      theme: "dark",
    });
    expect(parseDisplay('{"scale": 300, "theme": "dark"}')).toEqual({
      scale: 100,
      theme: "dark",
    });
    expect(parseDisplay('{"scale": 125, "theme": "sepia"}')).toEqual({
      scale: 125,
      theme: "system",
    });
  });

  it("resolve the system theme from the OS, keep an explicit one", () => {
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
    expect(resolveTheme("light", true)).toBe("light");
    expect(resolveTheme("dark", false)).toBe("dark");
  });
});
