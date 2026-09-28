import type { FolderOffer } from "@/wire";

/** One folder of a path, as the ask panel shows it. */
export interface Segment {
  /** Shown between the previous segment and this one: `\`, `/` or nothing. */
  separator: string;
  label: string;
  /** The folder up to this segment, in the offer's form (`C:\a`, `/a`). */
  path: string;
  /** Too broad to grant: picking it makes the ask dangerous. */
  broad: boolean;
}

/**
 * Splits `C:\a\b` (either slash) or `/a/b` into its folders, root first.
 * Anything else, a relative path included, has no segments.
 */
export function folderSegments(folder: string): Omit<Segment, "broad">[] {
  const drive = /^([A-Za-z]):(?:[\\/](.*))?$/.exec(folder);
  let root: string;
  let rootLabel: string;
  let separator: string;
  let rest: string;
  if (drive !== null) {
    const letter = (drive[1] ?? "").toUpperCase();
    root = `${letter}:\\`;
    rootLabel = `${letter}:`;
    separator = "\\";
    rest = drive[2] ?? "";
  } else if (folder.startsWith("/")) {
    root = "/";
    rootLabel = "/";
    separator = "/";
    rest = folder;
  } else {
    return [];
  }
  const segments = [{ separator: "", label: rootLabel, path: root }];
  let path = root;
  for (const part of rest.split(/[\\/]/)) {
    if (part === "") continue;
    const first = segments.length === 1;
    path = first ? `${root}${part}` : `${path}${separator}${part}`;
    // The POSIX root already reads as its separator.
    segments.push({ separator: first && drive === null ? "" : separator, label: part, path });
  }
  return segments;
}

function samePath(left: string, right: string): boolean {
  return left.toLowerCase() === right.toLowerCase();
}

/** The offer's folder as segments, each flagged when the server lists it as too broad. */
export function offerSegments(offer: FolderOffer): Segment[] {
  return folderSegments(offer.folder).map((segment) => ({
    ...segment,
    broad: offer.broad.some((path) => samePath(path, segment.path)),
  }));
}

/** Danger reason shown once a broad folder is picked. */
export function broadReason(path: string): string {
  return `Grants a broad folder for the session: ${path}`;
}
