import { describe, expect, it } from "vitest";

import { certWarning, parseServerStatus } from "@/server";

const IDLE = {
  supervised: true,
  stale_copy: false,
  new_build: false,
  pending: false,
  turns: 0,
  cert_expires_soon: null,
};

describe("parseServerStatus", () => {
  it("reads the supervision, the builds, the running turns and the certificate expiry", () => {
    expect(
      parseServerStatus(
        {
          supervised: true,
          stale_copy: true,
          new_build: true,
          pending: true,
          turns: 2,
          cert_expires_soon: "2030-01-02T00:00:00Z",
        },
        "s",
      ),
    ).toEqual({
      supervised: true,
      staleCopy: true,
      newBuild: true,
      pending: true,
      turns: 2,
      certExpiresSoon: new Date("2030-01-02T00:00:00Z"),
    });
    expect(parseServerStatus(IDLE, "s").certExpiresSoon).toBeNull();
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
    expect(() => parseServerStatus({ ...IDLE, cert_expires_soon: "soon" }, "s")).toThrow(
      's.cert_expires_soon: expected a date or null: "soon"',
    );
    expect(() => parseServerStatus({ ...IDLE, cert_expires_soon: undefined }, "s")).toThrow(
      "s.cert_expires_soon: expected a date or null: undefined",
    );
  });
});

describe("certWarning", () => {
  const expiresAt = new Date("2030-01-02T00:00:00Z");

  it("names the expiry date ahead of it, and says expired after", () => {
    expect(certWarning(expiresAt, expiresAt.getTime() - 1)).toContain(
      expiresAt.toLocaleDateString(),
    );
    expect(certWarning(expiresAt, expiresAt.getTime())).toContain("has expired");
  });
});
