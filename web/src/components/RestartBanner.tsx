import { useEffect } from "react";

import { CloseIcon } from "@/components/Explorer";
import { useClock } from "@/components/useNow";
import { cacheGauge, formatRatio, restartAdvice } from "@/gauges";
import { adviceHidden, dismissAdvice, forgetDismissal, useDismissals } from "@/restartDismissals";
import type { SessionView } from "@/wire";

/**
 * Closes the discussion when most of the context is old work and restarting costs
 * less than continuing; nothing while the cache is cold, the cold panel covering it.
 * A tap hides it until the context shrinks.
 */
export function RestartBanner({ session }: { session: SessionView | null }) {
  const now = useClock(1000);
  const dismissals = useDismissals();
  const id = session?.id ?? null;
  const used = session?.context?.used_tokens ?? null;
  const hiddenAt = id === null ? undefined : dismissals[id];
  const hidden = used !== null && adviceHidden(hiddenAt, used);
  useEffect(() => {
    if (id !== null && used !== null && hiddenAt !== undefined && !hidden) forgetDismissal(id);
  }, [id, used, hiddenAt, hidden]);

  if (session === null || session.context === null || hidden) return null;
  if (cacheGauge(session.cache, now, session.updated_at).kind === "cold") return null;
  const advice = restartAdvice(session.context, session.calls_per_message, session.restart);
  if (advice === null) return null;
  const tokens = session.context.used_tokens;
  return (
    // A full-width bar like the restart divider.
    <div className="-mx-3 pt-6">
      <button
        type="button"
        onClick={() => dismissAdvice(session.id, tokens)}
        title={`${Math.round(advice.staleShare * 100)}% of the context is older than the last exchanges a restart keeps. Tap to hide until the context is rebuilt.`}
        className="flex w-full cursor-pointer items-center gap-2 bg-primary px-3 py-1.5 text-xs voice-ui text-primary-foreground"
      >
        <span className="flex-1 text-center">
          Task done? Restarting should make each message {formatRatio(advice.ratio)} cheaper.
        </span>
        <CloseIcon className="size-3.5 shrink-0" />
      </button>
    </div>
  );
}
