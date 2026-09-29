import { type PointerEvent as ReactPointerEvent, useRef, useState } from "react";

import { MENU_TOGGLE } from "@/components/SessionMenu";
import {
  type SendAction,
  type SendGestureInput,
  type SendGestureState,
  idleGesture,
  sendGestureView,
  sendStep,
} from "@/sendGesture";

const LATCHED_LABEL: Record<SendAction, string> = { send: "Send", stop: "Stop", menu: "Menu" };
const LATCHED_STYLE: Record<SendAction, string> = {
  send: "bg-primary text-primary-foreground",
  stop: "bg-destructive text-black",
  menu: "bg-card text-foreground",
};

/** Keeps the focus, and with it the OS keyboard, in the composer. */
const keepFocus = (event: { preventDefault: () => void }) => event.preventDefault();

/**
 * Send on touch screens: a tap or a swipe right sends, a swipe left stops
 * (while `stoppable`), a swipe up opens the menu. A pulsing red left border
 * shows while stopping is possible; the top border stands for the menu.
 */
export function TouchSend({
  ready,
  stoppable,
  stopping,
  onAction,
}: {
  /** The composer holds text to send. */
  ready: boolean;
  stoppable: boolean;
  stopping: boolean;
  onAction: (action: SendAction) => void;
}) {
  const [state, setState] = useState<SendGestureState>(idleGesture);
  const stateRef = useRef<SendGestureState>(idleGesture);
  const active = useRef<number | null>(null);
  const context = { stoppable };

  const dispatch = (input: SendGestureInput) => {
    const next = sendStep(stateRef.current, input, context);
    stateRef.current = next.state;
    setState(next.state);
    if (next.action !== null) onAction(next.action);
  };

  const down = (event: ReactPointerEvent<HTMLButtonElement>) => {
    if (active.current !== null || !event.isPrimary) return;
    active.current = event.pointerId;
    event.currentTarget.setPointerCapture(event.pointerId);
    dispatch({ type: "down", x: event.clientX, y: event.clientY });
  };
  const move = (event: ReactPointerEvent<HTMLButtonElement>) => {
    if (event.pointerId !== active.current) return;
    dispatch({ type: "move", x: event.clientX, y: event.clientY });
  };
  const finish = (event: ReactPointerEvent<HTMLButtonElement>, input: SendGestureInput) => {
    if (event.pointerId !== active.current) return;
    active.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    dispatch(input);
  };

  const view = sendGestureView(state, context);
  const label = stopping ? "Stopping..." : LATCHED_LABEL[view.latched ?? "send"];
  const dim = view.latched === null && !ready && !stopping;
  return (
    <button
      type="button"
      aria-label="Send. Swipe left to stop, up for the menu."
      onPointerDown={down}
      onPointerMove={move}
      onPointerUp={(event) => finish(event, { type: "up", x: event.clientX, y: event.clientY })}
      onPointerCancel={(event) => finish(event, { type: "cancel" })}
      onMouseDown={keepFocus}
      onContextMenu={keepFocus}
      className={`voice-ui border-t-foreground h-14 w-28 shrink-0 touch-none rounded-[5px] border-t-4 text-sm select-none ${LATCHED_STYLE[view.latched ?? "send"]} ${stoppable ? "send-stop-edge border-l-4" : ""}`}
      style={{
        transform: `translate(${view.x / 2}px, ${view.y / 2}px)`,
        transition: state.kind === "dragging" ? "none" : "transform 200ms ease-out",
      }}
    >
      <span className={dim ? "opacity-50" : ""}>{label}</span>
    </button>
  );
}

const DESKTOP_BUTTON = "voice-ui h-14 shrink-0 rounded-[5px] border text-sm";

/** Send's gestures as buttons on a fine pointer: Stop, Menu, Send. Stop always shows. */
export function DesktopSend({
  ready,
  turnOpen,
  stopping,
  menuOpen,
  onSend,
  onStop,
  onMenu,
}: {
  ready: boolean;
  turnOpen: boolean;
  stopping: boolean;
  menuOpen: boolean;
  onSend: () => void;
  onStop: () => void;
  onMenu: () => void;
}) {
  return (
    <>
      <button
        type="button"
        onMouseDown={keepFocus}
        onClick={onStop}
        disabled={!turnOpen || stopping}
        className={`${DESKTOP_BUTTON} border-destructive w-20 ${turnOpen ? "bg-destructive text-black disabled:opacity-60" : "text-destructive/60 bg-black"}`}
      >
        {stopping ? "Stopping..." : "Stop"}
      </button>
      <button
        type="button"
        {...{ [MENU_TOGGLE]: "" }}
        aria-expanded={menuOpen}
        onMouseDown={keepFocus}
        onClick={onMenu}
        className={`${DESKTOP_BUTTON} border-t-foreground w-20 border-t-4 ${menuOpen ? "bg-muted" : "bg-card hover:bg-muted"}`}
      >
        Menu
      </button>
      <button
        type="button"
        onMouseDown={keepFocus}
        onClick={onSend}
        disabled={!ready}
        className={`${DESKTOP_BUTTON} bg-primary text-primary-foreground border-primary w-20 disabled:opacity-50`}
      >
        Send
      </button>
    </>
  );
}
