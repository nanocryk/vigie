import { describe, expect, it } from "vitest";

import { parseVersion } from "@/version";

describe("parseVersion", () => {
  it("reads the build id", () => {
    expect(parseVersion({ build: "00ff" }, "version")).toBe("00ff");
  });

  it("names the label and the value on a bad shape", () => {
    expect(() => parseVersion({ build: 1 }, "version")).toThrow(
      'version: expected {build: string}: {"build":1}',
    );
    expect(() => parseVersion(null, "version")).toThrow("version:");
  });
});
