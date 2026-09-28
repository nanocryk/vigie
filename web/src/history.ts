import { type TranscriptState, applyEvents, emptyTranscript } from "@/transcript";
import type { WireEvent } from "@/wire";

/** Exchanges asked for on opening a discussion and per page of earlier history. */
export const HISTORY_EXCHANGES = 20;

/** Whether the discussion's first event is loaded. */
export type Beginning = "loaded" | "not_loaded";

/** Whether a `history` request awaits its `page`. */
export type Request = "idle" | "in_flight";

/** The part of a discussion's history a client holds. */
export interface Loaded {
  /** Every event received, the preludes included, in seq order. */
  events: readonly WireEvent[];
  /** Seq of the first event shown; 0 when the whole history was sent. */
  firstSeq: number;
  beginning: Beginning;
  request: Request;
}

/** A subscribe that sent the whole history. */
export const wholeHistory: Loaded = {
  events: [],
  firstSeq: 0,
  beginning: "loaded",
  request: "idle",
};

export function beginning(startReached: boolean): Beginning {
  return startReached ? "loaded" : "not_loaded";
}

/** Opened by a `window` message: its events are still to come. */
export function openWindow(firstSeq: number, startReached: boolean): Loaded {
  return { events: [], firstSeq, beginning: beginning(startReached), request: "idle" };
}

/** Events received after the ones held, in seq order. */
export function appendEvents(loaded: Loaded, events: readonly WireEvent[]): Loaded {
  const last = loaded.events.at(-1)?.seq ?? 0;
  const fresh = events.filter((event) => event.seq > last);
  return fresh.length === 0 ? loaded : { ...loaded, events: [...loaded.events, ...fresh] };
}

/** The `before_seq` of the next `history` request; null while one is in flight or nothing precedes. */
export function nextRequest(loaded: Loaded): number | null {
  if (loaded.request === "in_flight" || loaded.beginning === "loaded") return null;
  return loaded.firstSeq;
}

export function requested(loaded: Loaded): Loaded {
  return { ...loaded, request: "in_flight" };
}

/** A lost connection drops the answer to any request in flight. */
export function dropRequest(loaded: Loaded): Loaded {
  return loaded.request === "idle" ? loaded : { ...loaded, request: "idle" };
}

export interface Page {
  firstSeq: number;
  startReached: boolean;
  /** Its prelude, then its events from `firstSeq` on. */
  events: readonly WireEvent[];
}

/** Puts a page before the events held; a page not reaching further back only ends the request. */
export function prependPage(loaded: Loaded, page: Page): Loaded {
  if (page.firstSeq >= loaded.firstSeq) return dropRequest(loaded);
  const bySeq = new Map<number, WireEvent>();
  for (const event of [...page.events, ...loaded.events]) bySeq.set(event.seq, event);
  return {
    events: [...bySeq.values()].sort((left, right) => left.seq - right.seq),
    firstSeq: page.firstSeq,
    beginning: beginning(page.startReached),
    request: "idle",
  };
}

/** The transcript of the events held. */
export function fold(loaded: Loaded): TranscriptState {
  return applyEvents({ ...emptyTranscript, firstSeq: loaded.firstSeq }, loaded.events);
}
