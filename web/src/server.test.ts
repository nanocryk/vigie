import { describe, expect, it } from "vitest";

import { parseServerStatus } from "@/server";

describe("parseServerStatus", () => {
  it("reads the supervision, the running build and the running turns", () => {
    expect(
      parseServerStatus({ supervised: true, stale_copy: true, pending: true, turns: 2 }, "s"),
    ).toEqual({
      supervised: true,
      staleCopy: true,
      pending: true,
      turns: 2,
    });
  });

  it("names the bad field", () => {
    expect(() =>
      parseServerStatus({ supervised: "yes", stale_copy: false, pending: false, turns: 0 }, "s"),
    ).toThrow('s.supervised: expected a boolean: "yes"');
    expect(() => parseServerStatus({ supervised: true, pending: false, turns: 0 }, "s")).toThrow(
      "s.stale_copy: expected a boolean: undefined",
    );
    expect(() => parseServerStatus({ supervised: true, stale_copy: false, turns: 0 }, "s")).toThrow(
      "s.pending: expected a boolean: undefined",
    );
    expect(() =>
      parseServerStatus({ supervised: false, stale_copy: false, pending: false, turns: -1 }, "s"),
    ).toThrow("s.turns: expected a count: -1");
  });
});
