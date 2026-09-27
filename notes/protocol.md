# Headless Claude Code protocol

Facts from the Claude Code docs and the `@anthropic-ai/claude-agent-sdk` 0.3.283 type definitions (`sdk.d.ts`), matching CLI 2.1.283. Not yet exercised against a live session.

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
- Host to CLI: `initialize`, `interrupt` (optional `cancel_queued`), `set_permission_mode`, `set_model`, `get_usage` (experimental), `side_question`, `rewind_files`, `request_user_dialog` answers, and more.
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
