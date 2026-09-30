import { describe, expect, it } from "vitest";

import { parseServerStatus } from "@/server";

describe("parseServerStatus", () => {
  it("reads the supervision and the running turns", () => {
    expect(parseServerStatus({ supervised: true, turns: 2 }, "s")).toEqual({
      supervised: true,
      turns: 2,
    });
  });

  it("names the bad field", () => {
    expect(() => parseServerStatus({ supervised: "yes", turns: 0 }, "s")).toThrow(
      's.supervised: expected a boolean: "yes"',
    );
    expect(() => parseServerStatus({ supervised: false, turns: -1 }, "s")).toThrow(
      "s.turns: expected a count: -1",
    );
  });
});
