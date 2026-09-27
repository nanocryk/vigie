import { type KeyboardEvent, useEffect, useRef } from "react";

import { type CommandCard, type PanelState, commandQuery } from "@/commands";
import { useSwipe } from "@/components/Swipe";
import type { Side } from "@/swipe";

const SPRING_BACK = "transform 200ms ease-out";
const USE_SIDES: readonly Side[] = ["right"];
/** Same size as Send, which the field's action button replaces. */
const ACTION = "voice-ui h-12 w-20 shrink-0 rounded-[5px] text-sm";

function coarsePointer(): boolean {
  return window.matchMedia("(pointer: coarse)").matches;
}

/** Revealed behind a card as it slides right: toward green, brighter once latched. */
function reveal(lean: number, armed: boolean): string {
  if (armed || lean >= 1) return "var(--swipe-accept-latched)";
  const share = Math.round(Math.max(0, lean) * 100);
  return `color-mix(in oklch, var(--swipe-accept) ${share}%, var(--card))`;
}

function CardBody({ card }: { card: CommandCard }) {
  return (
    <>
      <div className="flex items-baseline gap-2">
        <span className="voice-heading text-sm">{card.title ?? `/${card.name}`}</span>
        {card.argumentHint !== "" && (
          <span className="text-muted-foreground voice-code truncate text-xs">
            {card.argumentHint}
          </span>
        )}
      </div>
      {card.description !== "" && (
        <p className="text-muted-foreground voice-ui line-clamp-2 text-xs">{card.description}</p>
      )}
    </>
  );
}

/** Used by two swipes right on touch screens, or by its Use button pressed twice. */
function CommandCardView({ card, onUse }: { card: CommandCard; onUse: (card: CommandCard) => void }) {
  const { surface, view, press } = useSwipe(
    { dangerous: false, sides: USE_SIDES, swipes: 2, presses: 2 },
    () => onUse(card),
  );
  const coarse = coarsePointer();
  const armed = view.armed !== null;
  return (
    <li className="flex items-stretch gap-2">
      <div className="relative min-w-0 flex-1 overflow-hidden rounded-[5px]">
        {coarse && (
          <div
            aria-hidden
            className="voice-ui absolute inset-0 flex items-center justify-start rounded-[5px] px-4 text-sm text-black"
            style={{ background: reveal(view.lean, armed) }}
          >
            Use
          </div>
        )}
        <div
          ref={coarse ? surface : undefined}
          className={`bg-card relative w-full touch-pan-y rounded-[5px] border px-3 py-2 select-none ${armed ? "border-swipe-accept-strong" : ""} ${card.own !== null ? "border-l-primary border-l-4" : ""}`}
          style={{
            transform: `translateX(${view.offset}px)`,
            transition: view.dragging ? "none" : SPRING_BACK,
          }}
        >
          <CardBody card={card} />
        </div>
        {coarse && armed && (
          <span className="bg-card voice-heading text-swipe-accept-strong pointer-events-none absolute top-1.5 right-2 rounded-[5px] px-2 py-0.5 text-sm shadow-sm">
            Use?
          </span>
        )}
      </div>
      {!coarse && (
        <button
          type="button"
          disabled={view.fired !== null}
          onClick={() => press("right")}
          className={`voice-ui shrink-0 rounded-[5px] border px-3 text-xs disabled:opacity-50 ${armed ? "bg-swipe-accept-strong text-card border-swipe-accept-strong" : "border-swipe-accept-strong text-swipe-accept-strong hover:bg-muted"}`}
        >
          {armed ? "Use?" : "Use"}
        </button>
      )}
    </li>
  );
}

/** In front of the discussion, above the tabs: one card per command matching the filter. */
export function CommandsPanel({
  cards,
  argument,
  onUse,
  onClose,
}: {
  cards: readonly CommandCard[];
  /** The command whose arguments the bottom field takes, if any. */
  argument: CommandCard | null;
  onUse: (card: CommandCard) => void;
  onClose: () => void;
}) {
  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-7 z-30 px-3">
      <div className="bg-background pointer-events-auto mx-auto flex max-h-[60dvh] max-w-5xl flex-col rounded-[5px] border shadow-lg">
        <div className="flex items-center justify-between gap-2 border-b px-3 py-1.5">
          <span className="voice-heading text-sm">
            {argument === null ? "Commands" : "Arguments"}
          </span>
          <span className="text-muted-foreground voice-ui truncate text-xs">
            {argument === null && coarsePointer() ? "swipe right twice to use ›" : ""}
          </span>
          <button
            type="button"
            onClick={onClose}
            className="voice-ui hover:bg-muted shrink-0 rounded-[5px] px-2 py-0.5 text-xs"
          >
            Close
          </button>
        </div>
        <div className="min-h-0 overflow-y-auto overscroll-contain p-2">
          {argument !== null ? (
            <div className="bg-card rounded-[5px] border px-3 py-2">
              <CardBody card={argument} />
            </div>
          ) : cards.length === 0 ? (
            <p className="text-muted-foreground voice-ui px-1 py-2 text-sm">No matching command.</p>
          ) : (
            <ul className="space-y-1.5">
              {cards.map((card) => (
                <CommandCardView key={`${card.own ?? "claude"}-${card.name}`} card={card} onUse={onUse} />
              ))}
            </ul>
          )}
        </div>
      </div>
    </div>
  );
}

/** The bottom field while the panel is open: the filter, or the argument entry. */
export function CommandField({
  state,
  draftKept,
  onFilter,
  onArgument,
  onSend,
  onClose,
}: {
  state: Exclude<PanelState, { kind: "closed" }>;
  /** A composer draft waits for the panel to close. */
  draftKept: boolean;
  onFilter: (text: string) => void;
  onArgument: (text: string) => void;
  onSend: () => void;
  onClose: () => void;
}) {
  const input = useRef<HTMLInputElement>(null);
  const browsing = state.kind === "browsing";
  const value = browsing ? state.filter : state.text;

  useEffect(() => {
    const element = input.current;
    if (element === null) return;
    element.focus();
    element.setSelectionRange(element.value.length, element.value.length);
  }, [state.kind]);

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
    } else if (event.key === "Enter" && !browsing && !event.nativeEvent.isComposing) {
      event.preventDefault();
      onSend();
    }
  };

  return (
    <div className="mx-auto max-w-5xl space-y-1">
      {draftKept && (
        <div className="text-muted-foreground voice-ui text-xs">
          Your draft stays in the composer; the command goes alone.
        </div>
      )}
      <div className="flex items-end gap-2">
        <input
          ref={input}
          value={value}
          onChange={(event) => (browsing ? onFilter : onArgument)(event.target.value)}
          onKeyDown={onKeyDown}
          placeholder={browsing ? "Filter commands" : `/${commandQuery(value)} arguments`}
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          enterKeyHint={browsing ? "done" : "send"}
          className="bg-card focus:ring-ring voice-code min-h-12 min-w-0 flex-1 rounded-[5px] border px-3 text-sm outline-none focus:ring-2"
        />
        {browsing ? (
          <button
            type="button"
            onClick={onClose}
            className={`bg-card border ${ACTION}`}
          >
            Close
          </button>
        ) : (
          <button
            type="button"
            onMouseDown={(event) => event.preventDefault()}
            onClick={onSend}
            disabled={value.trim() === ""}
            className={`bg-primary text-primary-foreground ${ACTION} disabled:opacity-50`}
          >
            Send
          </button>
        )}
      </div>
    </div>
  );
}
