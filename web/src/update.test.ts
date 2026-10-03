import { describe, expect, it } from "vitest";

import { parseUpdateStatus } from "@/update";

describe("parseUpdateStatus", () => {
  it("reads the check, the run and the changelog", () => {
    expect(
      parseUpdateStatus(
        {
          check: { state: "done", behind: 3, local: null },
          run: { state: "running", step: "web" },
          changelog: [{ sha: "592a350", title: "Keep the transcript at the bottom" }],
        },
        "u",
      ),
    ).toEqual({
      check: { state: "done", behind: 3, local: null },
      run: { state: "running", step: "web" },
      changelog: [{ sha: "592a350", title: "Keep the transcript at the bottom" }],
    });
    expect(
      parseUpdateStatus(
        {
          check: { state: "failed", message: "offline" },
          run: { state: "failed", step: "pull", output: "diverged" },
          changelog: [],
        },
        "u",
      ),
    ).toEqual({
      check: { state: "failed", message: "offline" },
      run: { state: "failed", step: "pull", output: "diverged" },
      changelog: [],
    });
  });

  it("names the bad field", () => {
    expect(() =>
      parseUpdateStatus(
        {
          check: { state: "done", behind: -1, local: null },
          run: { state: "idle" },
          changelog: [],
        },
        "u",
      ),
    ).toThrow("u.check.behind: expected a count: -1");
    expect(() =>
      parseUpdateStatus(
        { check: { state: "done", behind: 0 }, run: { state: "idle" }, changelog: [] },
        "u",
      ),
    ).toThrow("u.check.local: expected a string or null: undefined");
    expect(() =>
      parseUpdateStatus(
        { check: { state: "unchecked" }, run: { state: "running", step: "deploy" }, changelog: [] },
        "u",
      ),
    ).toThrow('u.run.step: unknown step: "deploy"');
    expect(() => parseUpdateStatus({ check: { state: "unchecked" } }, "u")).toThrow(
      "u.run: expected an object: undefined",
    );
    expect(() =>
      parseUpdateStatus(
        { check: { state: "unchecked" }, run: { state: "idle" }, changelog: [{ sha: "abc" }] },
        "u",
      ),
    ).toThrow("u.changelog[0].title: expected a string: undefined");
  });
});
