export type BootResult =
  | { kind: "authed"; label: string | null }
  | { kind: "unpaired" }
  | { kind: "bad-token" }
  | { kind: "rate-limited"; retryAfterSecs: number }
  | { kind: "error"; message: string };

export interface BootEnv {
  hash: string;
  fetch: (input: string, init?: RequestInit) => Promise<Response>;
  stripHash: () => void;
}

const AUTH_PREFIX = "#auth=";
const DEFAULT_RETRY_AFTER_SECS = 3600;

export function extractAuthToken(hash: string): string | null {
  if (!hash.startsWith(AUTH_PREFIX)) return null;
  const token = decodeURIComponent(hash.slice(AUTH_PREFIX.length));
  return token === "" ? null : token;
}

function isRecord(raw: unknown): raw is Record<string, unknown> {
  return typeof raw === "object" && raw !== null;
}

function optionalLabel(raw: Record<string, unknown>, label: string): string | null {
  const value = raw["label"];
  if (value === undefined || value === null) return null;
  if (typeof value !== "string") {
    throw new Error(`${label}: label is not a string: ${JSON.stringify(raw)}`);
  }
  return value;
}

export function parseWhoami(
  raw: unknown,
  label: string,
): { authed: boolean; label: string | null } {
  if (!isRecord(raw) || typeof raw["authed"] !== "boolean") {
    throw new Error(`${label}: expected {authed: boolean}: ${JSON.stringify(raw)}`);
  }
  return { authed: raw["authed"], label: optionalLabel(raw, label) };
}

export function parseExchangeOk(raw: unknown, label: string): string | null {
  if (!isRecord(raw)) {
    throw new Error(`${label}: expected an object: ${JSON.stringify(raw)}`);
  }
  return optionalLabel(raw, label);
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Resolves the device's auth state, exchanging a `#auth=` token when present. */
export async function bootstrap(env: BootEnv): Promise<BootResult> {
  const token = extractAuthToken(env.hash);

  let whoami: { authed: boolean; label: string | null };
  try {
    const res = await env.fetch("api/whoami");
    whoami = parseWhoami(await res.json(), "whoami");
  } catch (err) {
    return { kind: "error", message: `Cannot reach server: ${errorMessage(err)}` };
  }

  if (whoami.authed) {
    if (token !== null) env.stripHash();
    return { kind: "authed", label: whoami.label };
  }
  if (token === null) return { kind: "unpaired" };

  let res: Response;
  try {
    res = await env.fetch("api/auth/exchange", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ token }),
    });
  } catch (err) {
    return { kind: "error", message: `Pairing request failed: ${errorMessage(err)}` };
  }

  if (res.ok) {
    env.stripHash();
    try {
      return { kind: "authed", label: parseExchangeOk(await res.json(), "exchange") };
    } catch (err) {
      return { kind: "error", message: errorMessage(err) };
    }
  }
  if (res.status === 401) {
    env.stripHash();
    return { kind: "bad-token" };
  }
  if (res.status === 429) {
    env.stripHash();
    const header = Number.parseInt(res.headers.get("Retry-After") ?? "", 10);
    const retryAfterSecs = Number.isFinite(header) ? header : DEFAULT_RETRY_AFTER_SECS;
    return { kind: "rate-limited", retryAfterSecs };
  }
  return { kind: "error", message: `Pairing failed: HTTP ${res.status}` };
}
