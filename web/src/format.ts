import type { SessionState } from "@/wire";

/** Last segment of a Windows or POSIX path. */
export function folderName(path: string): string {
  return path.split(/[\\/]/).filter((part) => part !== "").at(-1) ?? path;
}

export function formatAgo(iso: string, now: number): string {
  const time = Date.parse(iso);
  if (Number.isNaN(time)) return iso;
  const seconds = Math.max(0, Math.round((now - time) / 1000));
  if (seconds < 60) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  return new Date(time).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

export function stateLabel(state: SessionState): string {
  switch (state.kind) {
    case "running":
      return "running";
    case "stopped":
      return "stopped";
    case "exited":
      return state.code === null ? "exited" : `exited (${state.code})`;
    case "gate_failed":
      return "stopped by the auth gate";
  }
}
