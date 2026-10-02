import { describe, expect, it } from "vitest";

import { parseOpenSections, toggledSections } from "@/sectionsOpen";

describe("open settings sections", () => {
  it("keep the string ids of the stored list", () => {
    expect(parseOpenSections(null)).toEqual(new Set());
    expect(parseOpenSections("not json")).toEqual(new Set());
    expect(parseOpenSections('{"a": 1}')).toEqual(new Set());
    expect(parseOpenSections('["display", 3, "server"]')).toEqual(new Set(["display", "server"]));
  });

  it("toggle a section without touching the others", () => {
    const open = new Set(["display"]);
    expect(toggledSections(open, "server")).toEqual(new Set(["display", "server"]));
    expect(toggledSections(open, "display")).toEqual(new Set());
    expect(open).toEqual(new Set(["display"]));
  });
});
