export interface Device {
  id: string;
  label: string;
  lastSeen: string;
  revoked: boolean;
  isYou: boolean;
  isConnected: boolean;
  lastIp: string | null;
}

export interface PairHost {
  host: string;
  /** What kind of network the host sits on, as shown next to it. */
  kind: string;
  name: string;
}

export interface PairLink {
  urls: string[];
  expiresAt: number;
}

function isRecord(raw: unknown): raw is Record<string, unknown> {
  return typeof raw === "object" && raw !== null;
}

function field<T>(
  raw: Record<string, unknown>,
  key: string,
  label: string,
  guard: (value: unknown) => value is T,
  expected: string,
): T {
  const value = raw[key];
  if (!guard(value)) {
    throw new Error(`${label}.${key}: expected ${expected}: ${JSON.stringify(value)}`);
  }
  return value;
}

const isString = (value: unknown): value is string => typeof value === "string";
const isBoolean = (value: unknown): value is boolean => typeof value === "boolean";
const isNumber = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value);
const isOptionalString = (value: unknown): value is string | null | undefined =>
  value === null || value === undefined || typeof value === "string";

function list(raw: unknown, key: string, label: string): unknown[] {
  if (!isRecord(raw)) throw new Error(`${label}: expected an object: ${JSON.stringify(raw)}`);
  const value = raw[key];
  if (!Array.isArray(value)) {
    throw new Error(`${label}.${key}: expected an array: ${JSON.stringify(value)}`);
  }
  return value;
}

export function parseDevices(raw: unknown, label: string): Device[] {
  return list(raw, "devices", label).map((entry, index) => {
    const at = `${label}.devices[${index}]`;
    if (!isRecord(entry)) throw new Error(`${at}: expected an object: ${JSON.stringify(entry)}`);
    return {
      id: field(entry, "id", at, isString, "a string"),
      label: field(entry, "label", at, isString, "a string"),
      lastSeen: field(entry, "last_seen", at, isString, "a string"),
      revoked: field(entry, "revoked", at, isBoolean, "a boolean"),
      isYou: field(entry, "is_you", at, isBoolean, "a boolean"),
      isConnected: field(entry, "is_connected", at, isBoolean, "a boolean"),
      lastIp: field(entry, "last_ip", at, isOptionalString, "a string or null") ?? null,
    };
  });
}

const KIND_LABELS: Record<string, string> = {
  private: "LAN",
  cgnat: "Tailnet",
  public: "Public",
};

/** Hosts a pair link can point at: the reachable interfaces, then the configured hosts. */
export function parsePairHosts(raw: unknown, label: string): PairHost[] {
  const interfaces = list(raw, "interfaces", label).flatMap((entry, index): PairHost[] => {
    const at = `${label}.interfaces[${index}]`;
    if (!isRecord(entry)) throw new Error(`${at}: expected an object: ${JSON.stringify(entry)}`);
    const kind = KIND_LABELS[field(entry, "kind", at, isString, "a string")];
    if (kind === undefined) return [];
    return [
      {
        host: field(entry, "ip", at, isString, "a string"),
        kind,
        name: field(entry, "name", at, isString, "a string"),
      },
    ];
  });
  const configured = list(raw, "public_hosts", label).map((entry, index): PairHost => {
    if (!isString(entry)) {
      throw new Error(`${label}.public_hosts[${index}]: expected a string: ${JSON.stringify(entry)}`);
    }
    return { host: entry, kind: "Configured", name: "" };
  });
  return [...interfaces, ...configured];
}

/** The host this page was loaded from when it is offered, else the first one. */
export function defaultPairHost(
  hosts: PairHost[],
  location: { host: string; hostname: string },
): PairHost | undefined {
  return (
    hosts.find((candidate) => candidate.host === location.host) ??
    hosts.find((candidate) => candidate.host === location.hostname) ??
    hosts[0]
  );
}

export function parsePairLink(raw: unknown, label: string, now: number): PairLink {
  if (!isRecord(raw)) throw new Error(`${label}: expected an object: ${JSON.stringify(raw)}`);
  const urls = list(raw, "urls", label).map((entry, index) => {
    if (!isString(entry)) {
      throw new Error(`${label}.urls[${index}]: expected a string: ${JSON.stringify(entry)}`);
    }
    return entry;
  });
  const seconds = field(raw, "expires_in_secs", label, isNumber, "a number");
  return { urls, expiresAt: now + seconds * 1000 };
}

async function check(response: Response): Promise<void> {
  if (response.ok) return;
  const text = await response.text().catch(() => "");
  throw new Error(`HTTP ${response.status}${text === "" ? "" : `: ${text}`}`);
}

export async function fetchDevices(): Promise<Device[]> {
  const response = await fetch("api/devices");
  await check(response);
  return parseDevices(await response.json(), "devices");
}

export async function fetchPairHosts(): Promise<PairHost[]> {
  const response = await fetch("api/net/interfaces");
  await check(response);
  return parsePairHosts(await response.json(), "interfaces");
}

export async function createPairLink(host: string): Promise<PairLink> {
  const response = await fetch("api/auth/pair", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ hosts: [host] }),
  });
  await check(response);
  return parsePairLink(await response.json(), "pair", Date.now());
}

export async function renameDevice(id: string, label: string): Promise<void> {
  const response = await fetch(`api/devices/${encodeURIComponent(id)}/rename`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ label }),
  });
  if (response.status === 400) throw new Error("A name is 1 to 64 characters");
  await check(response);
}

export async function revokeDevice(id: string): Promise<void> {
  const response = await fetch(`api/devices/${encodeURIComponent(id)}/revoke`, {
    method: "POST",
  });
  // 409: already revoked, which is the wanted end state.
  if (response.status === 409) return;
  await check(response);
}

export async function removeDevice(id: string): Promise<void> {
  await check(await fetch(`api/devices/${encodeURIComponent(id)}`, { method: "DELETE" }));
}
