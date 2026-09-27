import { describe, expect, it } from "vitest";

import { type BootEnv, bootstrap, extractAuthToken, parseWhoami } from "@/pairing";

interface Call {
  url: string;
  init: RequestInit | undefined;
}

function json(body: unknown, status = 200, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json", ...headers },
  });
}

function fakeEnv(
  hash: string,
  routes: Record<string, () => Response>,
): { env: BootEnv; calls: Call[]; stripped: () => number } {
  const calls: Call[] = [];
  let strips = 0;
  const env: BootEnv = {
    hash,
    fetch: async (url, init) => {
      calls.push({ url, init });
      const route = routes[url];
      if (route === undefined) throw new Error(`unexpected fetch ${url}`);
      return route();
    },
    stripHash: () => {
      strips += 1;
    },
  };
  return { env, calls, stripped: () => strips };
}

describe("extractAuthToken", () => {
  it("reads and decodes the token from the hash", () => {
    expect(extractAuthToken("#auth=abc%2Bdef")).toBe("abc+def");
  });

  it("ignores other or empty hashes", () => {
    expect(extractAuthToken("")).toBeNull();
    expect(extractAuthToken("#session=x")).toBeNull();
    expect(extractAuthToken("#auth=")).toBeNull();
  });
});

describe("parseWhoami", () => {
  it("accepts a missing label", () => {
    expect(parseWhoami({ authed: false }, "whoami")).toEqual({ authed: false, label: null });
  });

  it("names the field and value on a bad shape", () => {
    expect(() => parseWhoami({ authed: "yes" }, "whoami")).toThrow(/whoami: .*"authed":"yes"/);
  });
});

describe("bootstrap", () => {
  it("is authed without an exchange when the cookie is valid", async () => {
    const { env, calls, stripped } = fakeEnv("#auth=tok", {
      "api/whoami": () => json({ authed: true, label: "device 1" }),
    });
    expect(await bootstrap(env)).toEqual({ kind: "authed", label: "device 1" });
    expect(calls.map((c) => c.url)).toEqual(["api/whoami"]);
    expect(stripped()).toBe(1);
  });

  it("is unpaired without a cookie or a token", async () => {
    const { env, calls } = fakeEnv("", {
      "api/whoami": () => json({ authed: false }),
    });
    expect(await bootstrap(env)).toEqual({ kind: "unpaired" });
    expect(calls).toHaveLength(1);
  });

  it("exchanges the hash token and strips it", async () => {
    const { env, calls, stripped } = fakeEnv("#auth=tok", {
      "api/whoami": () => json({ authed: false }),
      "api/auth/exchange": () => json({ label: "device 2" }),
    });
    expect(await bootstrap(env)).toEqual({ kind: "authed", label: "device 2" });
    const exchange = calls[1];
    expect(exchange?.url).toBe("api/auth/exchange");
    expect(exchange?.init?.method).toBe("POST");
    expect(exchange?.init?.body).toBe(JSON.stringify({ token: "tok" }));
    expect(stripped()).toBe(1);
  });

  it("reports a rejected token", async () => {
    const { env, stripped } = fakeEnv("#auth=tok", {
      "api/whoami": () => json({ authed: false }),
      "api/auth/exchange": () => json({ error: "bad_token" }, 401),
    });
    expect(await bootstrap(env)).toEqual({ kind: "bad-token" });
    expect(stripped()).toBe(1);
  });

  it("reports the rate limit with its retry delay", async () => {
    const { env } = fakeEnv("#auth=tok", {
      "api/whoami": () => json({ authed: false }),
      "api/auth/exchange": () => json({ error: "rate_limited" }, 429, { "Retry-After": "120" }),
    });
    expect(await bootstrap(env)).toEqual({ kind: "rate-limited", retryAfterSecs: 120 });
  });

  it("reports an unreachable server", async () => {
    const env: BootEnv = {
      hash: "",
      fetch: async () => {
        throw new Error("offline");
      },
      stripHash: () => {},
    };
    expect(await bootstrap(env)).toEqual({
      kind: "error",
      message: "Cannot reach server: offline",
    });
  });
});
