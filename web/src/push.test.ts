import { describe, expect, it } from "vitest";

import { base64UrlBytes, parsePushConfig } from "@/push";
import { parseRoute, routeHash } from "@/route";

describe("push config", () => {
  it("reads the key and the subscription state, naming a bad field", () => {
    expect(parsePushConfig({ vapid_public_key: "BAbc", subscribed: true }, "c")).toEqual({
      vapidPublicKey: "BAbc",
      subscribed: true,
    });
    expect(() => parsePushConfig({ vapid_public_key: "BAbc", subscribed: "yes" }, "c")).toThrow(
      'c.subscribed: expected a boolean: "yes"',
    );
  });

  it("decodes base64url without padding", () => {
    expect([...base64UrlBytes("-_8")]).toEqual([0xfb, 0xff]);
    expect([...base64UrlBytes("AQID")]).toEqual([1, 2, 3]);
  });
});

describe("settings route", () => {
  it("round-trips through the hash", () => {
    expect(parseRoute(routeHash({ kind: "settings" }))).toEqual({ kind: "settings" });
    expect(parseRoute("#/s/abc")).toEqual({ kind: "session", id: "abc" });
    expect(parseRoute("#/")).toEqual({ kind: "list" });
  });
});
