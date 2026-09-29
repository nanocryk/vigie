# Restart advice

A bar closing the discussion, shaped like the restart divider and absent while a turn runs, says when most of the context is old work and restarting the discussion costs less than continuing it: `Task done? Restarting should make each message ~1.6× cheaper.`, the stale share in its tooltip. Whether the task is done is left to the user: no signal tells it across workflows. Main conversation only, hidden while the cache is cold (the cold panel covers that case). It never restarts anything.

## What a restart drops

A restart with `continue` seeds the fresh session with the last `SEED_EXCHANGES` exchanges, truncated, tool results left out. The fresh session then reads again the files those exchanges touched.

- `fixed`: the context categories of kind `used` except `Messages` (system prompt, tools, memory). A restart keeps them.
- `window_start`: the prompt tokens of the first main request of the oldest seeded exchange, from the stored usage. Everything under it but `fixed` is conversation older than the seed. The window never reaches before the latest exchange whose first prompt is smaller than the previous one's: a restart, a compaction or a clear rebuilt the context there, and older prompts no longer describe it.
- Stale share = (`window_start` - `fixed`) / `C`, with `C` the context used.
- `S`, the restarted context = `fixed` + the seed text + the files touched in the seeded exchanges (Read, Edit, Write, MultiEdit, NotebookEdit), each sized by its largest Read result in the discussion, 0 when never read. Text is counted at 4 characters per token.

## Cost

Prices in base input units: cache read 0.1, 1 hour cache write 2, 5 minute write 1.25, output 5.

- Continuing, each call reads `C` from the cache; after a restart it reads `S`. The first call after the restart writes `S`. A restart pays back after `K* = 2 S / (0.1 (C - S))` calls.
- `other`: what a call costs besides reading the cache (input, cache writes, output), averaged over the main calls of the seeded exchanges. A restart does not change it.
- Ratio = (`0.1 C + other`) / (`0.1 S + other`): how much less each call costs after a restart.

`K*` counts API calls; a message makes several (one per tool round trip). Messages = `ceil(K* / calls_per_message)`.

The banner shows when the stale share is at least `STALE_MIN_SHARE`, messages at most `ADVICE_MAX_MESSAGES` and `C` at least `ADVICE_MIN_CONTEXT`. These constants live in `web/src/gauges.ts`; the backend measures, sent as the session's `restart`, are taken after each turn.

## Calls per message

`calls_per_message` is `main_requests / turns`, two counters on the discussion's row: a main-conversation response with a new message id, and a `result` line. It covers the discussion's whole life, restarts included, and is null before the first turn.

A plain average weighs every turn the same. An outlier turn (say 50 calls among turns of 2) inflates it, so the banner shows too early, and the outlier fades only as `1 / turns`. If that misleads in practice:

- Median of the last N turns: ignores outliers; needs per-turn counts stored instead of two totals.
- Exponential average, each new turn weighing e.g. 20%: an outlier fades in about ten turns; still two numbers per discussion.
