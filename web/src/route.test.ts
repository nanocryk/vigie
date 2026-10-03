import { describe, expect, it } from "vitest";

import { type Route, parseRoute, routeHash } from "@/route";

describe("routes", () => {
  it("round-trip through their hash", () => {
    const routes: Route[] = [
      { kind: "list" },
      { kind: "session", id: "abc" },
      { kind: "search", discussion: null, query: "" },
      { kind: "search", discussion: null, query: "prettier config?" },
      { kind: "search", discussion: "abc", query: "a&b #c" },
      { kind: "around", discussion: "abc", seq: 42 },
    ];
    for (const route of routes) expect(parseRoute(routeHash(route))).toEqual(route);
  });

  it("send an unknown hash home", () => {
    expect(parseRoute("#/around/abc/x")).toEqual({ kind: "list" });
  });
});
