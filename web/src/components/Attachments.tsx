import { useState } from "react";

import { attachmentName, isImagePath } from "@/attachments";
import { CloseIcon, FileIcon } from "@/components/Explorer";
import { rawUrl } from "@/files";

/** A file of the composer: uploading, stored at `path`, or failed. */
export interface ComposerFile {
  key: string;
  name: string;
  status: "uploading" | "ready" | "failed";
  path: string | null;
  error: string | null;
}

/** Square for an image, as wide as the name for another file. */
function ChipFace({ name, path, size }: { name: string; path: string | null; size: string }) {
  const [broken, setBroken] = useState(false);
  if (path !== null && isImagePath(path) && !broken) {
    return (
      <img
        src={rawUrl(path)}
        alt={name}
        draggable={false}
        onError={() => setBroken(true)}
        className={`${size} object-cover`}
      />
    );
  }
  return (
    <span className="voice-ui flex h-full max-w-36 items-center gap-1.5 px-2.5 text-xs">
      <FileIcon className="text-muted-foreground size-4" />
      <span className="truncate">{name}</span>
    </span>
  );
}

/** A sent file, hanging over its bubble's edge; a tap opens it. */
export function AttachmentChip({ path, onOpen }: { path: string; onOpen: (path: string) => void }) {
  const name = attachmentName(path);
  return (
    <button
      type="button"
      title={name}
      onClick={() => onOpen(path)}
      className="bg-card ring-background hover:ring-ring flex h-11 min-w-11 cursor-pointer items-center justify-center overflow-hidden rounded-md shadow-md ring-2 transition-shadow"
    >
      <ChipFace name={name} path={path} size="size-11" />
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
          className={`bg-background relative flex h-14 min-w-14 items-center justify-center overflow-hidden rounded-md border ${
            file.status === "failed" ? "border-destructive" : ""
          } ${file.status === "uploading" ? "animate-pulse" : ""}`}
        >
          <ChipFace
            name={file.name}
            path={file.status === "ready" ? file.path : null}
            size="size-14"
          />
          {file.status === "failed" && (
            <span className="bg-destructive voice-ui absolute bottom-0.5 left-0.5 flex size-4 items-center justify-center rounded-full text-[10px] font-bold text-black">
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
