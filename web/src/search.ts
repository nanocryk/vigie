/** A run of text, and whether it is one of the searched words. */
export interface Run {
  text: string;
  marked: boolean;
}

export function searchWords(query: string): string[] {
  return query.split(/\s+/).filter((word) => word !== "");
}

/** `text` cut into runs, the searched words (ignoring case) marked. */
export function markWords(text: string, words: readonly string[]): Run[] {
  if (words.length === 0) return [{ text, marked: false }];
  const escaped = words.map((word) => word.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  // A capturing split alternates the text between matches and the matches.
  return text
    .split(new RegExp(`(${escaped.join("|")})`, "iu"))
    .map((part, index) => ({ text: part, marked: index % 2 === 1 }))
    .filter((run) => run.text !== "");
}
