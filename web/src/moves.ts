/** Folders picked in the Explorer, by session, until the session screen opens its restart choices. */
const pending = new Map<string, string>();

export function requestMove(session: string, folder: string): void {
  pending.set(session, folder);
}

/** The folder picked for `session`, once. */
export function takeMove(session: string): string | null {
  const folder = pending.get(session) ?? null;
  pending.delete(session);
  return folder;
}
