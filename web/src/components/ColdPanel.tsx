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

const RESUME = {
  label: "Resume, same model",
  explain: "Nothing changed: back to the discussion, the cache is untouched.",
  tone: "border-swipe-accept-strong bg-swipe-accept-strong/15 [&>:first-child]:text-swipe-accept-strong",
};
const RESUME_AS_IS = {
  ...RESUME,
  label: "Resume",
  explain: "Nothing changes: back to the discussion as it is, the cache is untouched.",
};
const REWARM_SETTINGS = {
  label: "Rewarm with new settings",
  explain: "Same Claude session on the new settings: the next request reads the whole context again.",
};

function RewarmWarning({
  context,
  settings,
  busy,
  onConfirm,
  onBack,
}: {
  context: ContextView | null;
  /** The reload comes from a model or effort change, not an expired cache. */
  settings: boolean;
  busy: boolean;
  onConfirm: () => void;
  onBack: () => void;
}) {
  const size = context === null ? "the whole context" : `${formatTokens(context.used_tokens)} tokens`;
  return (
    <div className="border-destructive bg-destructive/15 space-y-3 rounded-[5px] border-2 p-3">
      <p className="voice-heading text-destructive text-base">Rewarm re-sends {size} at full price</p>
      <p className="voice-ui text-sm">
        {settings
          ? "A new model or effort level cannot use the current cache"
          : "The cache expired"}
        , so the next request pays for writing the whole conversation into the cache again,
        charged against your 5h and 7d windows. Continue or Blank usually cost far less.
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

/**
 * Replaces the composer when a message for an agent whose own cache went cold
 * waits: relaying it makes the agent write its whole context to the cache again.
 */
export function AgentColdPanel({
  name,
  promptTokens,
  text,
  busy,
  onRelay,
  onCancel,
}: {
  name: string;
  /** The agent's last prompt: what it caches again; 0 when unknown. */
  promptTokens: number;
  /** The message waiting. */
  text: string;
  busy: boolean;
  onRelay: () => void;
  onCancel: () => void;
}) {
  const size = promptTokens > 0 ? `about ${formatTokens(promptTokens)} tokens` : "its whole context";
  return (
    <section className="bg-card border-primary/40 shrink-0 border-t-2 px-3 pt-3 pb-[max(0.75rem,env(safe-area-inset-bottom))]">
      <div className="mx-auto max-w-5xl space-y-3">
        <div className="border-destructive bg-destructive/15 space-y-3 rounded-[5px] border-2 p-3">
          <p className="voice-heading text-destructive text-base">
            The cache of agent {name} is cold
          </p>
          <p className="voice-ui text-sm">
            An agent's cache lasts 5 minutes after its last request. Relaying this message makes
            it read {size} again at full price, charged against your 5h and 7d windows.
          </p>
          <p className="text-muted-foreground voice-ui truncate text-xs">Your message: {text}</p>
          <div className="flex gap-2">
            <button
              type="button"
              disabled={busy}
              onClick={onRelay}
              className="bg-destructive voice-ui rounded-[5px] px-4 py-2 text-sm text-black disabled:opacity-50"
            >
              Relay anyway
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={onCancel}
              className="bg-background voice-ui rounded-[5px] border px-4 py-2 text-sm disabled:opacity-50"
            >
              Cancel
            </button>
          </div>
        </div>
      </div>
    </section>
  );
}

/**
 * Replaces the composer and the ask card while the cache is cold, until a choice
 * is made, and the composer of an archived discussion, which any choice restores.
 * During a model or effort change (`settings`), its first choice resumes for free
 * when the picks equal the current settings, or rewarms on the new ones.
 */
export function ColdPanel({
  archived,
  context,
  draft,
  askWaiting,
  busy,
  error,
  settings,
  onChoose,
}: {
  archived: boolean;
  context: ContextView | null;
  /** Composer text, sent with whichever choice is made. */
  draft: string;
  askWaiting: boolean;
  /** A choice was sent and awaits the server. */
  busy: boolean;
  error: string | null;
  /** `restart`: opened from the menu's Restart entry, not a model or effort change. */
  settings: { changed: boolean; restart: boolean; onCancel: () => void } | null;
  onChoose: (choice: ColdChoice) => void;
}) {
  const typed = draft.trim() !== "" && settings?.changed !== false;
  const [confirmingRewarm, setConfirmingRewarm] = useState(false);
  const choices = CHOICES.map((entry) => {
    if (settings === null || entry.choice !== "rewarm") return entry;
    if (settings.changed) return { ...entry, ...REWARM_SETTINGS };
    return settings.restart ? { ...entry, ...RESUME_AS_IS } : { ...entry, ...RESUME };
  });
  const confirms = (choice: ColdChoice) =>
    choice === "rewarm" && (settings === null || settings.changed);
  const heading = archived
    ? "This discussion is archived; any choice restores it."
    : settings?.restart === true
      ? "Restart this discussion?"
      : settings !== null
        ? "Pick a model and an effort level in the menu, opened from the model pill at the top."
        : "The cache is cold.";
  return (
    <section className="bg-card border-primary/40 shrink-0 border-t-2 px-3 pt-3 pb-[max(0.75rem,env(safe-area-inset-bottom))]">
      <div className="mx-auto max-w-5xl space-y-3">
        <div className="voice-ui space-y-0.5 text-sm">
          <div>
            {heading} <span className="text-muted-foreground">{contextSummary(context)}</span>
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
            settings={settings !== null}
            busy={busy}
            onConfirm={() => onChoose("rewarm")}
            onBack={() => setConfirmingRewarm(false)}
          />
        ) : (
          <div className="grid gap-2 sm:grid-cols-3">
            {choices.map(({ choice, label, explain, tone }) => (
              <button
                key={choice}
                type="button"
                disabled={busy}
                onClick={() => (confirms(choice) ? setConfirmingRewarm(true) : onChoose(choice))}
                className={`voice-ui flex flex-col items-start gap-0.5 rounded-[5px] border-2 px-3 py-2 text-left disabled:opacity-50 ${tone}`}
              >
                <span className="text-sm">{label}</span>
                <span className="text-muted-foreground text-xs">{explain}</span>
              </button>
            ))}
          </div>
        )}
        {settings !== null && (
          <button
            type="button"
            disabled={busy}
            onClick={settings.onCancel}
            className="bg-background voice-ui rounded-[5px] border px-4 py-2 text-sm disabled:opacity-50"
          >
            Cancel
          </button>
        )}
        {error !== null && <p className="text-destructive voice-ui text-xs">{error}</p>}
      </div>
    </section>
  );
}
