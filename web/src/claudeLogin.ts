import { parseErrorBody } from "@/wire";

/** Id of the Device settings section holding the login. */
export const CLAUDE_SECTION = "claude";

/** The account the Claude CLI is logged in to. */
export interface ClaudeAccount {
  loggedIn: boolean;
  email: string | null;
  subscription: string | null;
}

function stringOrNull(raw: unknown, label: string): string | null {
  if (raw === null || raw === undefined) return null;
  if (typeof raw !== "string") {
    throw new Error(`${label}: expected a string or null: ${JSON.stringify(raw)}`);
  }
  return raw;
}

export function parseClaudeAccount(raw: unknown, label: string): ClaudeAccount {
  if (typeof raw !== "object" || raw === null) {
    throw new Error(`${label}: expected an object: ${JSON.stringify(raw)}`);
  }
  const loggedIn: unknown = Reflect.get(raw, "logged_in");
  if (typeof loggedIn !== "boolean") {
    throw new Error(`${label}.logged_in: expected a boolean: ${JSON.stringify(loggedIn)}`);
  }
  return {
    loggedIn,
    email: stringOrNull(Reflect.get(raw, "email"), `${label}.email`),
    subscription: stringOrNull(Reflect.get(raw, "subscription"), `${label}.subscription`),
  };
}

export function parseLoginUrl(raw: unknown, label: string): string {
  const url: unknown = typeof raw === "object" && raw !== null ? Reflect.get(raw, "url") : null;
  if (typeof url !== "string" || !url.startsWith("https://")) {
    throw new Error(`${label}.url: expected an https URL: ${JSON.stringify(url)}`);
  }
  return url;
}

const ERROR_TEXT: Record<string, string> = {
  no_login: "This login expired: start again",
  bad_code: "Paste the code the page shows, without spaces",
};

/** Throws the server's error text, or the status when the body has none. */
async function check(response: Response): Promise<void> {
  if (response.ok) return;
  const body: unknown = await response.json().catch(() => null);
  const code = parseErrorBody(body);
  if (code === null) throw new Error(`HTTP ${response.status}`);
  throw new Error(ERROR_TEXT[code] ?? code);
}

export async function fetchClaudeAccount(): Promise<ClaudeAccount> {
  const response = await fetch("api/claude/account", { cache: "no-store" });
  await check(response);
  return parseClaudeAccount(await response.json(), "claude account");
}

/** Starts a login on the server; resolves to the URL whose page gives the code. */
export async function startLogin(): Promise<string> {
  const response = await fetch("api/claude/login", { method: "POST" });
  await check(response);
  return parseLoginUrl(await response.json(), "claude login");
}

/** Ends the login with `code`; rejects with the CLI's message when refused. */
export async function submitLoginCode(code: string): Promise<void> {
  await check(
    await fetch("api/claude/login/code", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ code }),
    }),
  );
}
