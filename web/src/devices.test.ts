import { describe, expect, it } from "vitest";

import { defaultPairHost, parseDevices, parsePairHosts, parsePairLink } from "@/devices";

describe("parseDevices", () => {
  it("maps the server fields", () => {
    const devices = parseDevices(
      {
        devices: [
          {
            id: "a",
            label: "phone",
            issued_at: "2026-09-01T00:00:00Z",
            last_seen: "2026-09-29T10:00:00Z",
            revoked: false,
            is_you: true,
            is_connected: true,
            last_ip: null,
          },
        ],
      },
      "devices",
    );
    expect(devices).toEqual([
      {
        id: "a",
        label: "phone",
        lastSeen: "2026-09-29T10:00:00Z",
        revoked: false,
        isYou: true,
        isConnected: true,
        lastIp: null,
      },
    ]);
  });

  it("names the bad field", () => {
    expect(() =>
      parseDevices({ devices: [{ id: "a", label: "x", is_connected: "yes" }] }, "devices"),
    ).toThrow(/devices\.devices\[0\]\.last_seen/);
  });
});

describe("parsePairHosts", () => {
  const raw = {
    port: 8443,
    scheme: "https",
    interfaces: [
      { name: "Ethernet", ip: "192.168.1.10", kind: "private" },
      { name: "Tailscale", ip: "100.64.0.2", kind: "cgnat" },
      { name: "lo", ip: "127.0.0.1", kind: "loopback" },
      { name: "apipa", ip: "169.254.1.1", kind: "linklocal" },
    ],
    public_hosts: ["box.example.ts.net"],
  };

  it("keeps the hosts the server accepts, configured ones last", () => {
    expect(parsePairHosts(raw, "interfaces")).toEqual([
      { host: "192.168.1.10", kind: "LAN", name: "Ethernet" },
      { host: "100.64.0.2", kind: "Tailnet", name: "Tailscale" },
      { host: "box.example.ts.net", kind: "Configured", name: "" },
    ]);
  });

  it("defaults to the host the page was loaded from", () => {
    const hosts = parsePairHosts(raw, "interfaces");
    expect(defaultPairHost(hosts, { host: "100.64.0.2:8443", hostname: "100.64.0.2" })?.host).toBe(
      "100.64.0.2",
    );
    expect(defaultPairHost(hosts, { host: "localhost:8443", hostname: "localhost" })?.host).toBe(
      "192.168.1.10",
    );
    expect(defaultPairHost([], { host: "x", hostname: "x" })).toBeUndefined();
  });
});

describe("parsePairLink", () => {
  it("turns the lifetime into a deadline", () => {
    expect(parsePairLink({ token: "t", expires_in_secs: 300, urls: ["u"] }, "pair", 1000)).toEqual({
      urls: ["u"],
      expiresAt: 301_000,
    });
  });
});
