# Hook ideas

Ideas (2026-10-04). Nothing here is decided. Mechanism: `notes/protocol.md`, Hooks.

## Facts

- vigie registers SDK hooks in `initialize`, per CLI process it launches. No settings file is written; terminal sessions never see them; they end with the process.
- The handler runs in the server, so it can read anything the server knows: agent caches, relays, session state, the danger classifier.
- PreToolUse can answer `{}` (no effect), `deny` with a reason the model reads, or `ask`, which turns any tool call into a normal ask card, even one the CLI or the mode would allow. In use for SendMessage to cold subagents.
- The CLI's hook schema also lists PostToolUse, UserPromptSubmit and PostModelSwitch. Untested.
- Each firing waits for the server's answer: a handler must answer fast.
- Not checked: which decision wins when a settings hook and a vigie hook answer differently.

## Ideas

### Enforcing rules mechanically

- Subagent and fan-out gate: PreToolUse on `Agent`, `Workflow`, `CronCreate`, `ScheduleWakeup` turns each into an ask, the card naming the model and an estimated cost. Today the rule against unrequested subagents is only an instruction.
- Dangers past the rules: when the danger classifier flags a Bash command (force push, `reset --hard`, `rm -rf`), force an ask even when a broad allow rule or the mode would let it run.

### Cache-aware gating

- Waits that outlive the cache: a `sleep`, Monitor or ScheduleWakeup longer than the TTL makes the next request read a cold cache. Ask, or deny with the reason.
- Subagent tool calls near expiry: warn before a subagent starts a call likely to outlast its cache, on top of the existing auto-deny of its asks.

### Rewriting or enriching calls

Rely on PreToolUse `updatedInput` and PostToolUse `additionalContext`, both untested.

- Steering to canif: rewrite a raw shell command matching a canif script to `canif auto <script>`, or deny with "use `canif auto X`".
- Token discipline: deny a Read of a large file without `offset` / `limit`, with "read the section you need".
- Exact touched files: PostToolUse on Edit / Write tells the server which files changed as they change, for the Diff panel and the restart advice.

## Open

- Which idea to build first. Suggested: the subagent and fan-out gate, the same shape as the SendMessage gate.
