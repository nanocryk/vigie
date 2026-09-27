import { useEffect, useState } from "react";

import { type ColdChoice, contextSummary } from "@/cold";
import { useNow } from "@/components/useNow";
import { type CacheGauge, cacheGauge, formatTokens } from "@/gauges";
import type { CacheView, ContextView } from "@/wire";

/** The cache gauge, recomputed on return to the foreground and at the moment it goes cold. */
export function useCacheGauge(cache: CacheView | null, lastActivity: string | null): CacheGauge {
  const woke = useNow();
  const [ticked, setTicked] = useState(() => Date.now());
  const gauge = cacheGauge(cache, Math.max(woke, ticked), lastActivity);
  const coldAt = gauge.kind === "warm" ? gauge.coldAt : null;
  useEffect(() => {
    if (coldAt === null) return;
    const timer = setTimeout(() => setTicked(Date.now()), Math.max(0, coldAt - Date.now()) + 50);
    return () => clearTimeout(timer);
  }, [coldAt]);
  return gauge;
}

const CHOICES: readonly { choice: ColdChoice; label: string; explain: string; tone: string }[] = [
  {
    choice: "rewarm",
    label: "Rewarm",
    explain: "Same Claude session: the next request reads the whole context again.",
    tone: "border-destructive bg-destructive/15 [&>:first-child]:text-destructive",
  },
  {
    choice: "continue",
    label: "Continue",
    explain: "Fresh Claude session, given the last 5 exchanges as context.",
    tone: "border-swipe-accept-strong bg-swipe-accept-strong/15 [&>:first-child]:text-swipe-accept-strong",
  },
  {
    choice: "blank",
    label: "Blank",
    explain: "Fresh Claude session, with no context.",
    tone: "border-gauge-cold bg-gauge-cold/15 [&>:first-child]:text-gauge-cold",
  },
];

function RewarmWarning({
  context,
  busy,
  onConfirm,
  onBack,
}: {
  context: ContextView | null;
  busy: boolean;
  onConfirm: () => void;
  onBack: () => void;
}) {
  const size = context === null ? "the whole context" : `${formatTokens(context.used_tokens)} tokens`;
  return (
    <div className="border-destructive bg-destructive/15 space-y-3 rounded-[5px] border-2 p-3">
      <p className="voice-heading text-destructive text-base">Rewarm re-sends {size} at full price</p>
      <p className="voice-ui text-sm">
        The cache expired, so the next request pays for writing the whole conversation into the
        cache again, charged against your 5h and 7d windows. Continue or Blank usually cost far
        less.
      </p>
      <div className="flex gap-2">
        <button
          type="button"
          disabled={busy}
          onClick={onConfirm}
          className="bg-destructive voice-ui rounded-[5px] px-4 py-2 text-sm text-black disabled:opacity-50"
        >
          Rewarm anyway
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={onBack}
          className="bg-background voice-ui rounded-[5px] border px-4 py-2 text-sm disabled:opacity-50"
        >
          Back
        </button>
      </div>
    </div>
  );
}

/** Replaces the composer and the ask card while the cache is cold, until a choice is made. */
export function ColdPanel({
  context,
  draft,
  askWaiting,
  busy,
  error,
  onChoose,
}: {
  context: ContextView | null;
  /** Composer text, sent with whichever choice is made. */
  draft: string;
  askWaiting: boolean;
  /** A choice was sent and awaits the server. */
  busy: boolean;
  error: string | null;
  onChoose: (choice: ColdChoice) => void;
}) {
  const typed = draft.trim() !== "";
  const [confirmingRewarm, setConfirmingRewarm] = useState(false);
  return (
    <section className="bg-card border-primary/40 shrink-0 border-t-2 px-3 pt-3 pb-[max(0.75rem,env(safe-area-inset-bottom))]">
      <div className="mx-auto max-w-5xl space-y-3">
        <div className="voice-ui space-y-0.5 text-sm">
          <div>
            The cache is cold. <span className="text-muted-foreground">{contextSummary(context)}</span>
          </div>
          {askWaiting && (
            <div className="text-muted-foreground text-xs">
              An approval waits. Rewarm shows it; Continue and Blank drop it.
            </div>
          )}
          {typed && (
            <div className="text-muted-foreground truncate text-xs">
              Your message goes with the choice: {draft}
            </div>
          )}
        </div>
        {confirmingRewarm ? (
          <RewarmWarning
            context={context}
            busy={busy}
            onConfirm={() => onChoose("rewarm")}
            onBack={() => setConfirmingRewarm(false)}
          />
        ) : (
          <div className="grid gap-2 sm:grid-cols-3">
            {CHOICES.map(({ choice, label, explain, tone }) => (
              <button
                key={choice}
                type="button"
                disabled={busy}
                onClick={() =>
                  choice === "rewarm" ? setConfirmingRewarm(true) : onChoose(choice)
                }
                className={`voice-ui flex flex-col items-start gap-0.5 rounded-[5px] border-2 px-3 py-2 text-left disabled:opacity-50 ${tone}`}
              >
                <span className="text-sm">{label}</span>
                <span className="text-muted-foreground text-xs">{explain}</span>
              </button>
            ))}
          </div>
        )}
        {error !== null && <p className="text-destructive voice-ui text-xs">{error}</p>}
      </div>
    </section>
  );
}
