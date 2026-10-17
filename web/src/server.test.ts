import { describe, expect, it } from "vitest";

import { parseServerStatus } from "@/server";

const IDLE = { supervised: true, stale_copy: false, new_build: false, pending: false, turns: 0 };

describe("parseServerStatus", () => {
  it("reads the supervision, the builds and the running turns", () => {
    expect(
      parseServerStatus(
        { supervised: true, stale_copy: true, new_build: true, pending: true, turns: 2 },
        "s",
      ),
    ).toEqual({
      supervised: true,
      staleCopy: true,
      newBuild: true,
      pending: true,
      turns: 2,
    });
  });

  it("names the bad field", () => {
    expect(() => parseServerStatus({ ...IDLE, supervised: "yes" }, "s")).toThrow(
      's.supervised: expected a boolean: "yes"',
    );
    expect(() => parseServerStatus({ ...IDLE, stale_copy: undefined }, "s")).toThrow(
      "s.stale_copy: expected a boolean: undefined",
    );
    expect(() => parseServerStatus({ ...IDLE, new_build: undefined }, "s")).toThrow(
      "s.new_build: expected a boolean: undefined",
    );
    expect(() => parseServerStatus({ ...IDLE, pending: undefined }, "s")).toThrow(
      "s.pending: expected a boolean: undefined",
    );
    expect(() => parseServerStatus({ ...IDLE, turns: -1 }, "s")).toThrow(
      "s.turns: expected a count: -1",
    );
  });
});
