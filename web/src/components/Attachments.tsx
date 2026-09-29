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

const CHIP = "bg-card relative flex h-12 shrink-0 items-center overflow-hidden rounded-[5px] border";

/** An image's thumbnail, else a file icon and the start of its name. */
function ChipFace({ name, path }: { name: string; path: string | null }) {
  const [broken, setBroken] = useState(false);
  if (path !== null && isImagePath(path) && !broken) {
    return (
      <img
        src={rawUrl(path)}
        alt={name}
        onError={() => setBroken(true)}
        className="size-12 object-cover"
      />
    );
  }
  return (
    <span className="voice-ui flex items-center gap-1 px-2 text-xs">
      <FileIcon className="text-muted-foreground size-4" />
      <span className="max-w-24 truncate">{name}</span>
    </span>
  );
}

/** A sent file; a tap opens it. */
export function AttachmentChip({ path, onOpen }: { path: string; onOpen: (path: string) => void }) {
  const name = attachmentName(path);
  return (
    <button
      type="button"
      title={name}
      onClick={() => onOpen(path)}
      className={`${CHIP} hover:border-ring cursor-pointer`}
    >
      <ChipFace name={name} path={path} />
    </button>
  );
}

/** The composer's files, each removable; a failed one says why on hover. */
export function ComposerFiles({
  files,
  onRemove,
}: {
  files: readonly ComposerFile[];
  onRemove: (key: string) => void;
}) {
  if (files.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-2 pt-2 pr-2">
      {files.map((file) => (
        <div
          key={file.key}
          title={file.error ?? file.name}
          className={`${CHIP} overflow-visible ${file.status === "failed" ? "border-destructive" : ""}`}
        >
          <div className={`overflow-hidden rounded-[5px] ${file.status === "uploading" ? "opacity-50" : ""}`}>
            <ChipFace name={file.name} path={file.status === "ready" ? file.path : null} />
          </div>
          {file.status === "uploading" && (
            <span className="voice-ui text-muted-foreground absolute inset-x-0 bottom-0 text-center text-[10px]">
              uploading
            </span>
          )}
          <button
            type="button"
            aria-label={`Remove ${file.name}`}
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => onRemove(file.key)}
            className="bg-background hover:bg-muted absolute -top-2 -right-2 flex size-5 cursor-pointer items-center justify-center rounded-full border"
          >
            <CloseIcon className="size-3" />
          </button>
        </div>
      ))}
    </div>
  );
}
