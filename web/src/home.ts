import type { Side, SwipeRules } from "@/swipe";
import type { SessionView } from "@/wire";

export interface HomeSections {
  /** In the server's order, most recently updated first. */
  active: SessionView[];
  /** Most recently archived first. */
  archived: SessionView[];
}

export function homeSections(sessions: readonly SessionView[]): HomeSections {
  const active: SessionView[] = [];
  const archived: { session: SessionView; at: number }[] = [];
  for (const session of sessions) {
    if (session.archived_at === null) active.push(session);
    else archived.push({ session, at: Date.parse(session.archived_at) });
  }
  archived.sort((a, b) => b.at - a.at);
  return { active, archived: archived.map((entry) => entry.session) };
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
