import type { SessionState, SessionView } from "@/wire";

/** Last segment of a Windows or POSIX path. */
export function folderName(path: string): string {
  return (
    path
      .split(/[\\/]/)
      .filter((part) => part !== "")
      .at(-1) ?? path
  );
}

/** The session's name, else its folder name. */
export function sessionTitle(session: Pick<SessionView, "name" | "cwd">): string {
  return session.name ?? folderName(session.cwd);
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

const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const DAY_MS = 86_400_000;

function pad(value: number): string {
  return String(value).padStart(2, "0");
}

function clock(date: Date): string {
  return `${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** Whole local calendar days from `time` to `now`, DST shifts included. */
function daysBetween(time: Date, now: Date): number {
  const start = new Date(time.getFullYear(), time.getMonth(), time.getDate());
  const end = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  return Math.round((end.getTime() - start.getTime()) / DAY_MS);
}

/**
 * Local 24-hour time of an event: `HH:MM` the same day, `Tue HH:MM` within the
 * last week, `May 12 HH:MM` beyond, with the year when not the current one.
 */
export function formatStamp(time: number, now: number): string {
  const date = new Date(time);
  const today = new Date(now);
  const days = daysBetween(date, today);
  if (days <= 0) return clock(date);
  if (days < 7) return `${WEEKDAYS[date.getDay()] ?? ""} ${clock(date)}`;
  const day = `${MONTHS[date.getMonth()] ?? ""} ${date.getDate()}`;
  const year = date.getFullYear() === today.getFullYear() ? "" : ` ${date.getFullYear()}`;
  return `${day}${year} ${clock(date)}`;
}

/** `HH:MM:SS`, local. */
export function formatExact(time: number): string {
  const date = new Date(time);
  return `${clock(date)}:${pad(date.getSeconds())}`;
}

export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.max(0, Math.round(ms))} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
  const seconds = Math.round(ms / 1000);
  return `${Math.floor(seconds / 60)} min ${seconds % 60} s`;
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
