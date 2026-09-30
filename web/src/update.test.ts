import { describe, expect, it } from "vitest";

import { parseUpdateStatus } from "@/update";

describe("parseUpdateStatus", () => {
  it("reads the check and the run", () => {
    expect(
      parseUpdateStatus(
        { check: { state: "done", behind: 3 }, run: { state: "running", step: "web" } },
        "u",
      ),
    ).toEqual({ check: { state: "done", behind: 3 }, run: { state: "running", step: "web" } });
    expect(
      parseUpdateStatus(
        {
          check: { state: "failed", message: "offline" },
          run: { state: "failed", step: "pull", output: "diverged" },
        },
        "u",
      ),
    ).toEqual({
      check: { state: "failed", message: "offline" },
      run: { state: "failed", step: "pull", output: "diverged" },
    });
  });

  it("names the bad field", () => {
    expect(() =>
      parseUpdateStatus({ check: { state: "done", behind: -1 }, run: { state: "idle" } }, "u"),
    ).toThrow("u.check.behind: expected a count: -1");
    expect(() =>
      parseUpdateStatus(
        { check: { state: "unchecked" }, run: { state: "running", step: "deploy" } },
        "u",
      ),
    ).toThrow('u.run.step: unknown step: "deploy"');
    expect(() => parseUpdateStatus({ check: { state: "unchecked" } }, "u")).toThrow(
      "u.run: expected an object: undefined",
    );
  });
});
