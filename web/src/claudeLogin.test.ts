import { describe, expect, it } from "vitest";

import { parseClaudeAccount, parseLoginUrl } from "@/claudeLogin";

describe("parseClaudeAccount", () => {
  it("reads the account and a logged-out one", () => {
    expect(
      parseClaudeAccount(
        { logged_in: true, email: "someone@example.com", subscription: "max" },
        "a",
      ),
    ).toEqual({ loggedIn: true, email: "someone@example.com", subscription: "max" });
    expect(parseClaudeAccount({ logged_in: false, email: null, subscription: null }, "a")).toEqual({
      loggedIn: false,
      email: null,
      subscription: null,
    });
  });

  it("names the bad field", () => {
    expect(() => parseClaudeAccount({ logged_in: "yes" }, "a")).toThrow(
      'a.logged_in: expected a boolean: "yes"',
    );
    expect(() => parseClaudeAccount({ logged_in: true, email: 3 }, "a")).toThrow(
      "a.email: expected a string or null: 3",
    );
  });
});

describe("parseLoginUrl", () => {
  it("takes an https URL only", () => {
    expect(parseLoginUrl({ url: "https://claude.com/cai/oauth/authorize?x=1" }, "l")).toBe(
      "https://claude.com/cai/oauth/authorize?x=1",
    );
    expect(() => parseLoginUrl({ url: "javascript:alert(1)" }, "l")).toThrow(
      'l.url: expected an https URL: "javascript:alert(1)"',
    );
  });
});
