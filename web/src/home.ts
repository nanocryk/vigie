import { cacheGauge } from "@/gauges";
import type { Side, SwipeRules } from "@/swipe";
import type { SessionView } from "@/wire";

export interface HomeSections {
  /** In the server's order, most recently updated first; scratch sessions left out. */
  active: SessionView[];
  /** Active scratch sessions, in the server's order. */
  scratch: SessionView[];
  /** Most recently archived first, scratch sessions included. */
  archived: SessionView[];
}

export function homeSections(sessions: readonly SessionView[]): HomeSections {
  const active: SessionView[] = [];
  const scratch: SessionView[] = [];
  const archived: { session: SessionView; at: number }[] = [];
  for (const session of sessions) {
    if (session.archived_at !== null) {
      archived.push({ session, at: Date.parse(session.archived_at) });
    } else if (session.scratch) scratch.push(session);
    else active.push(session);
  }
  archived.sort((a, b) => b.at - a.at);
  return { active, scratch, archived: archived.map((entry) => entry.session) };
}

/** `sessions` in the order of the ids in `previous`, with ids not in it first, in their given order. */
export function keepOrder(
  previous: readonly string[],
  sessions: readonly SessionView[],
): SessionView[] {
  const rank = new Map(previous.map((id, index) => [id, index]));
  const fresh = sessions.filter((session) => !rank.has(session.id));
  const known = sessions
    .filter((session) => rank.has(session.id))
    .sort((a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0));
  return [...fresh, ...known];
}

/**
 * What a row's border shows, strongest first: an ask, an unseen stop, a cache
 * about to go cold, an unseen finished turn, work.
 */
export type RowHighlight = "ask" | "stopped" | "cooling" | "finished" | "working";

const ONE_HOUR_TTL_MS = 60 * 60_000;

/**
 * The spell of an unarchived discussion whose 1 hour cache runs low, as the
 * push warning has it: its last request time. Null when not cooling.
 */
export function coolingSpell(session: SessionView, now: number): string | null {
  if (session.archived_at !== null || session.cache === null) return null;
  const gauge = cacheGauge(session.cache, now);
  const low = gauge.kind === "warm" && gauge.ttlMs === ONE_HOUR_TTL_MS && gauge.low;
  return low ? session.cache.last_request_at : null;
}

function highlightOf(session: SessionView, cooling: boolean): RowHighlight | null {
  if (session.asks > 0) return "ask";
  if (session.unread === "stopped") return "stopped";
  if (cooling) return "cooling";
  if (session.unread === "finished") return "finished";
  return session.busy ? "working" : null;
}

export function rowHighlight(session: SessionView, now: number): RowHighlight | null {
  return highlightOf(session, coolingSpell(session, now) !== null);
}

const HIGHLIGHT_RANK: readonly RowHighlight[] = [
  "ask",
  "stopped",
  "cooling",
  "finished",
  "working",
];

/**
 * The strongest highlight among `sessions`, leaving out the one on screen and
 * the cooling spells in `seen` (per session, its seen spell).
 */
export function strongestHighlight(
  sessions: readonly SessionView[],
  now: number,
  seen: Readonly<Record<string, string>>,
  onScreen: string | null = null,
): RowHighlight | null {
  let best: number | null = null;
  for (const session of sessions) {
    if (session.id === onScreen) continue;
    const spell = coolingSpell(session, now);
    const highlight = highlightOf(session, spell !== null && seen[session.id] !== spell);
    if (highlight === null) continue;
    const rank = HIGHLIGHT_RANK.indexOf(highlight);
    if (best === null || rank < best) best = rank;
  }
  return best === null ? null : (HIGHLIGHT_RANK[best] ?? null);
}

/** A live discussion can be killed, a stopped one archived, an archived one restored. */
export type RowAction = "kill" | "archive" | "restore";

export function rowAction(session: SessionView): RowAction {
  if (session.archived_at !== null) return "restore";
  return session.live ? "kill" : "archive";
}

/** The side a row slides toward: negative actions left, positive ones right. */
export function actionSide(action: RowAction): Side {
  return action === "restore" ? "right" : "left";
}

/** Each row swipes toward its action's side only: twice to kill, once otherwise. Kill takes two presses too. */
export function rowSwipeRules(action: RowAction): SwipeRules {
  const sides = [actionSide(action)];
  return action === "kill"
    ? { dangerous: false, sides, swipes: 2, presses: 2 }
    : { dangerous: false, sides, swipes: 1, presses: 1 };
}
