import { createContext, useContext, useEffect, useState } from "react";

import { attachmentName, isImagePath } from "@/attachments";
import { CloseIcon, FileIcon } from "@/components/Explorer";
import { fetchHash, rawUrl } from "@/files";

/** A file of the composer: uploading, stored at `path`, or failed. */
export interface ComposerFile {
  key: string;
  name: string;
  status: "uploading" | "ready" | "failed";
  path: string | null;
  error: string | null;
}

/** Square for an image, as wide as the name for another file. */
function ChipFace({
  name,
  path,
  hash,
  size,
}: {
  name: string;
  path: string | null;
  /** The content to show, by hash; the current one when absent. */
  hash?: string | undefined;
  size: string;
}) {
  const [broken, setBroken] = useState(false);
  if (path !== null && isImagePath(path) && !broken) {
    return (
      <img
        src={rawUrl(path, hash)}
        alt={name}
        draggable={false}
        onError={() => setBroken(true)}
        className={`${size} object-cover`}
      />
    );
  }
  return (
    <span className="flex h-full max-w-36 items-center gap-1.5 px-2.5 text-xs voice-ui">
      <FileIcon className="size-4 text-muted-foreground" />
      <span className="truncate">{name}</span>
    </span>
  );
}

/** The hash of each path's latest attachment in the discussion. */
export const LatestHashes = createContext<ReadonlyMap<string, string>>(new Map());

type Drift = "same" | "modified" | "deleted";

/**
 * Whether the file still has content `hash`: modified at once when a later
 * attachment of the path has another hash, else as the server reports on mount.
 */
function useDrift(path: string, hash: string | undefined): Drift {
  const latest = useContext(LatestHashes).get(path);
  const [drift, setDrift] = useState<Drift>("same");
  useEffect(() => {
    if (hash === undefined) return;
    let live = true;
    fetchHash(path).then(
      (current) => {
        if (live) setDrift(current === null ? "deleted" : current === hash ? "same" : "modified");
      },
      () => {},
    );
    return () => {
      live = false;
    };
  }, [path, hash]);
  if (drift === "same" && hash !== undefined && latest !== undefined && latest !== hash) {
    return "modified";
  }
  return drift;
}

/**
 * A sent file, hanging over its bubble's edge; a tap opens it. With `hash`, it
 * shows that content and flags a file modified or deleted since.
 */
export function AttachmentChip({
  path,
  hash,
  onOpen,
}: {
  path: string;
  hash?: string | undefined;
  onOpen: () => void;
}) {
  const name = attachmentName(path);
  const drift = useDrift(path, hash);
  const note =
    drift === "modified" ? "modified since" : drift === "deleted" ? "deleted since" : null;
  return (
    <button
      type="button"
      title={note === null ? name : `${name} (${note})`}
      onClick={onOpen}
      className="relative flex h-11 min-w-11 cursor-pointer items-center justify-center overflow-hidden rounded-md bg-card shadow-md ring-2 ring-background transition-shadow hover:ring-ring"
    >
      <ChipFace name={name} path={path} hash={hash} size="size-11" />
      {note !== null && (
        <span className="absolute inset-x-0 bottom-0 bg-black/70 text-center text-[0.625rem] leading-3 voice-ui text-white">
          {drift === "modified" ? "modified" : "deleted"}
        </span>
      )}
    </button>
  );
}

/** The composer's files, in a row above the text, inside the input's frame. */
export function ComposerFiles({
  files,
  onRemove,
}: {
  files: readonly ComposerFile[];
  onRemove: (key: string) => void;
}) {
  if (files.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-2 px-3 pt-2 pb-2">
      {files.map((file) => (
        <div
          key={file.key}
          title={file.error ?? file.name}
          className={`relative flex h-14 min-w-14 items-center justify-center overflow-hidden rounded-md border bg-background ${
            file.status === "failed" ? "border-destructive" : ""
          } ${file.status === "uploading" ? "animate-pulse" : ""}`}
        >
          <ChipFace
            name={file.name}
            path={file.status === "ready" ? file.path : null}
            size="size-14"
          />
          {file.status === "failed" && (
            <span className="absolute bottom-0.5 left-0.5 flex size-4 items-center justify-center rounded-full bg-destructive text-[0.625rem] voice-ui font-bold text-black">
              !
            </span>
          )}
          <button
            type="button"
            aria-label={`Remove ${file.name}`}
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => onRemove(file.key)}
            className="absolute top-0.5 right-0.5 flex size-5 cursor-pointer items-center justify-center rounded-full bg-black/65 text-white hover:bg-black/85"
          >
            <CloseIcon className="size-3" />
          </button>
        </div>
      ))}
    </div>
  );
}
