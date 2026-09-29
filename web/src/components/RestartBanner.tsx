import { useClock } from "@/components/useNow";
import { cacheGauge, formatRatio, restartAdvice } from "@/gauges";
import type { SessionView } from "@/wire";

/**
 * Closes the discussion when most of the context is old work and restarting costs
 * less than continuing; nothing while the cache is cold, the cold panel covering it.
 */
export function RestartBanner({ session }: { session: SessionView | null }) {
  const now = useClock(1000);
  if (session === null || session.context === null) return null;
  if (cacheGauge(session.cache, now, session.updated_at).kind === "cold") return null;
  const advice = restartAdvice(session.context, session.calls_per_message, session.restart);
  if (advice === null) return null;
  return (
    // A full-width bar like the restart divider.
    <div className="-mx-3 pt-6">
      <div
        title={`${Math.round(advice.staleShare * 100)}% of the context is older than the last exchanges a restart keeps`}
        className="bg-primary text-primary-foreground voice-ui block px-3 py-1.5 text-center text-xs"
      >
        Task done? Restarting should make each message {formatRatio(advice.ratio)} cheaper.
      </div>
    </div>
  );
}
