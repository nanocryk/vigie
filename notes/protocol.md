# Headless Claude Code protocol

Facts from the Claude Code docs and the `@anthropic-ai/claude-agent-sdk` 0.3.283 type definitions (`sdk.d.ts`), matching CLI 2.1.283. `## Observed` lists what a live spike on Windows confirmed (driver and recordings in `spike/`).

## Launch

- `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio`
- `--session-id <uuid>` to pick the id, `--resume <id>` to continue, `--forward-subagent-text` for subagent text.
- `--replay-user-messages` echoes submitted user messages back (with their uuid).
- Never `--bare`: it ignores OAuth and needs `ANTHROPIC_API_KEY`. Docs: `--bare` "will become the default for `-p` in a future release".
- Any `ANTHROPIC_API_KEY` in the child env switches billing to the API.
- Without `--bare`, `-p` runs the hooks from `~/.claude/settings.json` and the project settings.

## Control protocol

Line-delimited JSON on stdin/stdout, the same wire protocol the Agent SDKs use. Not documented as a CLI contract; the types are exported by the SDK.

- Envelope: `{type: "control_request", request_id, request: {subtype, ...}}`, answered by one `{type: "control_response", response}` echoing `request_id`. Either side can send requests. `control_cancel_request` withdraws one.
- `can_use_tool` (CLI to host): `tool_name`, `input`, `tool_use_id`, `title`, `description`, `display_name`, `decision_reason`, `decision_reason_type`, `permission_suggestions`, `blocked_path`, `default_to_no`, `suppress_always_allow_rule`, `requires_user_interaction`, `agent_id`.
  - Answer `{behavior: "allow", updatedInput, updatedPermissions?}` or `{behavior: "deny", message}`. The deny message is shown to Claude.
  - `AskUserQuestion` arrives here; answer allow with `updatedInput: {questions, answers}` (answers keyed by question text) or `response` for a free reply.
  - `ExitPlanMode` (plan approval) arrives here too.
  - Can stay pending indefinitely. A `PreToolUse` hook returning `defer` lets the process exit and resume later.
  - In `auto` and `bypassPermissions` modes it is never called, so questions and plans skip the host unless a `PreToolUse` hook routes them.
- Host to CLI: `initialize`, `interrupt` (optional `cancel_queued`), `set_permission_mode`, `set_model`, `get_usage` (experimental), `get_context_usage`, `get_settings`, `side_question`, `rewind_files`, `request_user_dialog` answers, and more.
- `reinitialize` after a transport gap: the response carries the `can_use_tool` requests still pending.
- `system/init.capabilities` for feature detection: `interrupt_receipt_v1` (interrupt response lists `still_queued`), `interrupt_cancel_queued_v1`.
- SIGTERM leaves the turn unfinished (exit 143); use `interrupt` to end a turn.

## Events

- `system/init` per turn: `session_id`, `model`, `cwd`, `tools`, `permissionMode`, `slash_commands`, `terminal_slash_commands` (hide on phone), `apiKeySource`, `capabilities`.
- `stream_event` token deltas, `assistant` / `user` messages with `tool_use` / `tool_result` blocks, `parent_tool_use_id` for subagents.
- `system/api_retry`: `attempt`, `max_retries`, `retry_delay_ms`, `error_status`, `error` in `authentication_failed`, `oauth_org_not_allowed`, `account_on_hold`, `billing_error`, `rate_limit`, `overloaded`, `invalid_request`, `model_not_found`, `server_error`, `max_output_tokens`, `cloud_credential_error`, `unknown`.
- `permission_denied` system messages; `result` lists `permission_denials`.
- `result`: final text, `session_id`, `is_error`, `total_cost_usd` (client-side estimate).

## Auth and billing signals

- `system/init.apiKeySource`: `"none"` for claude.ai OAuth; `ANTHROPIC_API_KEY`, `apiKeyHelper`, `/login managed key` mean an API key.
- `initialize` response `account`: `email`, `organization`, `subscriptionType`, `tokenSource`, `apiKeySource`, `apiProvider` (`firstParty` for Anthropic).
- `rate_limit_event.rate_limit_info`: `status` (`allowed`, `allowed_warning`, `rejected`), `resetsAt`, `rateLimitType` (`five_hour`, `seven_day`, `seven_day_opus`, `seven_day_sonnet`, `seven_day_overage_included`, `overage`), `utilization`, `surpassedThreshold`, `overageStatus`, `isUsingOverage`, `overageInUse`, `errorCode: "credits_required"`.
- `get_usage` response `rate_limits`: `limits[]` rows (`kind`, `group`, `percent`, `resets_at`, `scope.model` / `scope.surface`, `severity`, `is_active`) and `extra_usage` (`is_enabled`, `monthly_limit`, `used_credits`, `utilization`). `rate_limits_available` is false for API key sessions.
- Billing context (2026-09-27): the move of `-p` / Agent SDK usage to a separate credit, announced 2026-05-13, was paused on 2026-06-15; usage still draws from the subscription limits.

## Observed

CLI 2.1.283, Windows, Max subscription, launched with the flags under Launch. Recordings: `spike/runs/<scenario>.ndjson`; `canif run spike/summary <scenario>` prints them.

- Startup: `SessionStart` hooks run before the `initialize` response and show as `system/hook_started` and `system/hook_response`.
- `initialize` response: `account.subscriptionType` is `"Claude Max"`, `account.apiProvider` is `"firstParty"`; also `email`, `organization`. Other keys: `commands`, `agents`, `models`, `current_permission_mode`, `fast_mode_state`, `session_state`, `pid`, `remote_control_*`.
- `system/init` arrives on every turn: `apiKeySource: "none"`, `capabilities: ["interrupt_receipt_v1", "interrupt_cancel_queued_v1", "msg_lifecycle_v1", "mcp_read_resource_v1", "mcp_tool_ui_meta_v1"]`. Then `system/status` `requesting`; `system/thinking_tokens` while thinking.
- User message echo (`--replay-user-messages`): arrives when the model request starts, about 1 to 2 s after sending, just before the first `assistant` message.
- Every `control_response` the host sends is echoed back on stdout with the same `request_id`.
- claude.ai MCP connectors load under `-p` (an unauthorized one was reported by the model in its replies).

### Permissions

- Tools allowed by the settings rules never reach the host (`echo` through Bash ran without `can_use_tool`).
- `can_use_tool` carries a CLI-chosen UUID `request_id`, `tool_name`, `display_name`, `input`, `tool_use_id`, `description`. Write adds `permission_suggestions: [{type: "setMode", mode: "acceptEdits", destination: "session"}]`. `AskUserQuestion` and `ExitPlanMode` add `requires_user_interaction: true`.
- Deny with `message`: the model sees the message and follows it.
- `AskUserQuestion`: allow with `updatedInput: {questions, answers: {"<question text>": "<option label>"}}`; the model receives the answer.
- `set_permission_mode` answers `{mode}`. In plan mode the model writes the plan file without asking, into the user's `~/.claude/plans/`. `ExitPlanMode` input: `plan`, `planFilePath`. After allow, the same turn goes on to implement, and edits ask again.
- Suggestions for access outside the working folder, `<folder>` being the file's folder or the searched path:
  - Read, Glob, Grep: `{type: "addRules", rules: [{toolName: "Read", ruleContent: "//c/<folder>/**"}], behavior: "allow", destination: "session"}`. Glob and Grep also get a Read rule.
  - Write: `{type: "addDirectories", directories: ["<folder>"], destination: "session"}`, with a Windows path.
  - Bash (`cat` of an outside file): the same Read rule as above, preceded by `{type: "addRules", rules: [{toolName: "Bash", ruleContent: "<exact command>"}], behavior: "allow", destination: "localSettings"}`, a persisted rule for the exact command.
- The allow result can grant an ancestor of the suggested folder, as a Read rule (`//c/<parent>/**`) or as `addDirectories: ["<parent>"]`: a later Read in a sibling folder under `<parent>` does not ask, and a Read outside `<parent>` still asks.
- Allowing a Write with its `setMode acceptEdits` suggestion switches the mode: the next Write does not ask.

### Interrupt

- Works on Windows, during text streaming and during a foreground Bash call. Answer is immediate: `{still_queued: []}`.
- Followed by a `user` message `[Request interrupted by user]` (streaming) or a rejected `tool_result` plus `[Request interrupted by user for tool use]` (tool), then `result` with `subtype: "error_during_execution"`, `is_error: true`, `result: null`.
- Interrupted while streaming: the `result` usage is all zeros and `modelUsage` is empty, although tokens were generated.
- The next user message runs normally.

### Usage and rate limits

- `result.usage.cache_creation`: `ephemeral_1h_input_tokens` holds every cache write, `ephemeral_5m_input_tokens` is 0. Also `iterations[]`, `modelUsage.<model>` with `costUSD`, `contextWindow` (1000000), `costBasis: "list"`.
- `rate_limit_event` comes after the first `assistant` message of some turns, not every turn. Shape: `status`, `resetsAt` (epoch s), `rateLimitType`, `overageStatus: "rejected"`, `overageDisabledReason: "org_level_disabled"`, `isUsingOverage: false`, `unifiedWindows: {five_hour, seven_day}` each `{utilization, resetsAt}` with utilization a fraction (0.07).
- `get_usage` (`skip_behaviors: true`, about 230 ms): `subscription_type: "max"`, `rate_limits_available: true`, `rate_limits.five_hour` / `seven_day` `{utilization, resets_at}` with utilization in percent (7) and ISO dates, `extra_usage.is_enabled: false`, `limits[]` (`kind`: `session`, `weekly_all`, `weekly_scoped` with `scope.model.display_name`), `spend`, `seven_day_breakdown`, plus many codename keys set to null.
- `get_context_usage` (no parameters, 10 to 500 ms): answered before the first turn and while a turn runs. Reply keys: `totalTokens`, `maxTokens`, `rawMaxTokens`, `percentage` (rounded integer), `model` (`claude-opus-5-5[1m]`), `autoCompactThreshold`, `isAutoCompactEnabled`, `autocompactSource`, `apiUsage` (null before the first request, else the last request's `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`), `categories[]` (`name`, `tokens`, `color`, `kind`: `used`, `deferred`, `buffer` or `free`, optional `isDeferred`), plus `memoryFiles`, `skills`, `slashCommands`, `agents`, `mcpTools`, `messageBreakdown`, `gridRows`, which hold the user's configuration.
- `get_settings` (no parameters): answered before the first turn and after. Reply keys: `applied` (`model`, e.g. `claude-opus-5-5[1m]`, and `effort`, e.g. `medium`: what the CLI really uses), `effective` and `sources[]`, which hold the user's settings. Only `applied.model` and `applied.effort` are parsed, stored, logged or sent to clients; the reply line is kept out of the recorded events.
- `initialize` `models[]` entries carry `resolvedModel`, the id `value` stands for (`default` and `opus` both `claude-opus-5-5`).
- After a request, `totalTokens` equals the last main request's input, cache read and cache creation tokens exactly. Before any request it still counts the system prompt, tools, memory and skills.

### Background tasks

- Start: `system/background_tasks_changed` (`tasks[]` with `task_id`, `task_type: "local_bash"`, `description`) and `system/task_started` (`task_id`, `tool_use_id`, `is_backgrounded`).
- End: `background_tasks_changed` with the new list, `task_updated` (`patch: {status: "completed", end_time}`), `task_notification` (`status`, `output_file`, `summary`).
- On `task_notification` the CLI starts a new turn by itself, with no user message: `system/init`, the model's reaction, `result`.
- A foreground `sleep 60` is blocked; the model reruns it in the background.

### Slash commands

- `/cd` under `-p`: "/cd isn't available in this environment.", answered locally as an `assistant` message and a `result`, no model request.

## Sessions

- Transcripts: `~/.claude/projects/<project>/<session-id>.jsonl`, internal format that changes between versions.
- Resuming the same session from two live processes interleaves both into one transcript.
- A crashed tool call is marked as cut off on resume (2.1.281+).

## Prior art

- sugyan/claude-code-webui (MIT, archived): SDK `query()` per message with `resume`; no approval flow.
- siteboon/claudecodeui (AGPL-3, do not copy code): SDK with `canUseTool` routed to a prompt UI. Issues worth reading: #873 (auto mode skips the UI), #1350 (denial reasons), #1376 (obsolete 55 s prompt timeout).

## Sources

- https://code.claude.com/docs/en/headless
- https://code.claude.com/docs/en/agent-sdk/user-input
- https://code.claude.com/docs/en/sessions
- https://github.com/yanqirenshi/yaoyorozu/pull/383
- https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan
