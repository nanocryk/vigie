import { describe, expect, it } from "vitest";

import { adviceHidden, parseDismissals } from "@/restartDismissals";

describe("restart advice dismissals", () => {
  it("keep the numeric entries of the stored map", () => {
    expect(parseDismissals(null)).toEqual({});
    expect(parseDismissals("not json")).toEqual({});
    expect(parseDismissals("[1]")).toEqual({});
    expect(parseDismissals('{"a": 120000, "b": "x"}')).toEqual({ a: 120000 });
  });

  it("hide the advice until the context shrinks below its size at the tap", () => {
    expect(adviceHidden(undefined, 150000)).toBe(false);
    expect(adviceHidden(120000, 120000)).toBe(true);
    expect(adviceHidden(120000, 150000)).toBe(true);
    expect(adviceHidden(120000, 40000)).toBe(false);
  });
});
