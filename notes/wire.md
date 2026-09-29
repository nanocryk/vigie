# Client protocol

Messages between the server and its clients: the WebSocket protocol at `/api/ws` and the REST bodies. Types in `src/http/wire.rs`, mirrored by `web/src/wire.ts`. `GET /api/usage` answers `usage::Snapshot`.

WebSocket frames are JSON text tagged by `type`.

## Client to server

Subscriptions are not acked:

- `subscribe {session, after_seq, exchanges?}`: opens the session, sends its stored events with a seq above `after_seq`, then its live events, each once and in order. Subscribing again to the same session restarts from the new `after_seq`. With `exchanges` and an `after_seq` of 0, the stored events sent are only the last `exchanges` exchanges: first `window`, then the prelude, then the events from the window's `first_seq`, then `synced`.
- An exchange starts at a `user_sent` host event; a window or a page starts at one, or at the discussion's first event when fewer exchanges precede it. Its prelude is the latest `started`, `permission_mode`, `catalog` and `restarted` host events stored before it, those that exist, in seq order: the state folding the events from its start needs.
- `history {session, before_seq, exchanges}`: answered by `page` with the last `exchanges` exchanges stored below `before_seq` (at least one).
- `unsubscribe {session}`
- `presence {session, active}`: the discussion on screen (`null` on Home or elsewhere) and whether the user is there (page visible and recent input). Sent on every change.

Every other frame is a command on one session. It carries `session` and `client_id`, and is answered by one `ack` with that `client_id`:

- `send {session, text, rewarm?, client_id}`: spawns the child again if it ended. The session emits a `user_sent {text, client_id}` host event before writing the text.
- `interrupt {session, client_id}`
- `relay {session, task_id, text, rewarm?, client_id}`: a message to the subagent `task_id` of the running child, which `-p` cannot reach directly. The main agent gets a wrapped message asking it to forward `text` verbatim with SendMessage to `task_id`, then to reply exactly `[Relay acknowledged]`. Emits `relay_sent` before writing it; follows the cold rule of `send`. Refused when the child is not running or does not know the agent.
- `stop_task {session, task_id, client_id}`: stops one agent of the running child. `stop_agents {session, client_id}` stops every running one.
- `answer {session, request_id, answer, rewarm?, client_id}`: `answer` is `allow {updated_input?, note?, accept_edits?, folder?, always_for_file?}`, `deny {message}` or `questions {answers}`, tagged by `kind`. A non-blank allow `note` is then sent as a user message queued for the running turn, with its own `user_sent` event. `accept_edits: true` also sends the ask's `setMode acceptEdits` suggestion; `folder` also grants that folder for the session, in the kind of the ask's folder suggestion; `always_for_file: true` grants the ask's file until the child stops: later asks on it of the same kind are allowed by the server, and the allow carries a session rule for it unless the ask is a safety check. Each is refused when the ask did not offer it, and `folder` must be the offered folder or one of its parents.
- While the discussion's cache is cold (`gauges::is_cold`), or a model or effort change awaits its first main request (`settings_changed_at` set), a `send` or an `answer` without `rewarm: true` is refused with the ack error `cache_cold` and has no effect. `restart` and `interrupt` are always accepted.
- `set_permission_mode {session, mode, client_id}`: a live session asks the CLI; a stopped one only stores the mode (`default`, `acceptEdits` or `plan`) for its next launch and announces `sessions_changed`.
- `set_settings {session, model?, effort?, client_id}`: stores the model and the effort level (`low`, `medium`, `high`, `xhigh` or `max`) every launch passes as `--model` and `--effort`; an absent field keeps its value, `null` lets the CLI pick. Settings equal to the stored ones change nothing. A change sets `settings_changed_at`, clears `applied_model` and `applied_effort`, and announces `sessions_changed`; on a live child it also interrupts the running turn, if any, then applies the new model with `set_model` and a new effort level with `apply_flag_settings`, and asks `get_settings` again.
- `restart {session, mode, text?, client_id}`: moves the discussion to a fresh Claude session in the same permission mode, model and effort, and announces `sessions_changed`. The live child, if any, is stopped first, its pending asks dropped. The session then records a `restarted {mode, exchanges, total_exchanges, seed?}` host event and forgets its `cache` and `context`. `mode` `continue` launches the new session with one message: `seed`, a blank line, then `text`, or `seed::CLOSING` (asks for a summary and what to do next) when `text` is blank; `seed` renders the last `exchanges` of the discussion's `total_exchanges`. `blank` launches it with `text` when not blank, else only at the next `send`; `clear` does the same and is recorded as the Clear command. The `user_sent` event carries `text` alone, and there is none for a blank `text`.
- `kill {session, reason?, client_id}`: stops the live child the graceful way; the discussion stays listed and the next `send` resumes it. Acked without effect when not live. `reason` is `kill` (the default, from Home) or `revoke` (dropping the granted folders and files). A stop for a reason (these two, or `shutdown` when the server stops) emits `stopping {reason}` as it begins, and `child_exited` carries the reason too.
- `archive {session, client_id}`: refused with the ack error `session_live` while the child runs. `restore {session, client_id}` unarchives.
- A `send`, `answer` or `restart` on an archived discussion unarchives it first, then follows its usual rules, the cold check included.
- `kill`, `archive` and `restore` announce `sessions_changed`.

## Server to client

- `event {session, seq, time, kind, payload}`: `kind` `line` carries a CLI stdout line as JSON (a string when the line is not JSON), `host` a host event tagged by `event`.
  - `ask_queued` carries `danger {dangerous, reasons}`, `accept_edits` (true on an edit ask offering Accept edits, absent otherwise) and `folder {kind, folder, broad}` when the CLI suggests a folder to grant: `kind` `read` or `directory`, `folder` as `C:\a\b` or `/a/b`, `broad` the folder and parents too broad to grant (drive roots, the home folder, ...). It also carries `file {kind, path}` on a Read or edit-tool ask: `kind` `read` or `edit`, `path` the absolute file path, as `C:\a\b.md` or `/a/b.md`.
  - An ask on a file granted with `always_for_file` is answered by the server and recorded as `auto_allowed {request_id, tool_name, tool_use_id, file}` instead of `ask_queued`. Both carry `agent_id`, the task id of the subagent asking, absent for the main agent.
  - `relay_sent {text, client_id?, task_id, agent_tool_use_id, name, wrapped, mid_turn?}` precedes the write of a relay: `wrapped` is the message the main agent gets, `agent_tool_use_id` the Agent call that started the agent.
  - `relay_checked {relay_seq, task_id, agent_tool_use_id, outcome}` follows the turn that took it (`relay_seq` is the seq of its `relay_sent`): `outcome` is `faithful {send_message}` when that turn called SendMessage to `task_id` with `text` unchanged (`send_message` is that call's tool use id) and replied `[Relay acknowledged]`, else `failed {reason}` with `reason` `altered` (SendMessage to the agent with other text), `other_recipient`, `not_forwarded`, `no_acknowledgement` or `ended` (the child stopped first).
  - `catalog {models, commands}` follows each accepted `initialize`: `models [{value, resolved_model?, display_name, description?, effort_levels}]` (`resolved_model` the model id `value` stands for, `effort_levels` empty for a model without effort) and `commands [{name, description, argument_hint}]`, the CLI's slash commands and skills.
- `synced {session, seq}`: the stored events of a subscribed session were all sent, up to `seq`; follows the history sent on `subscribe`, and each later catch-up from the store.
- `window {session, first_seq, start_reached}`: a windowed `subscribe` sends the events from `first_seq`, after the prelude; `start_reached` when no event precedes `first_seq`.
- `page {session, first_seq, start_reached, events}`: answers `history`; `events` holds the prelude then the events from `first_seq` to below `before_seq`, in seq order, shaped as `event` without its `type`.
- `ack {client_id, ok, error?}`: one per command.
- `sessions_changed`: `GET /api/sessions` has a new list. Sent when a session:
  - is created, ended, killed, archived or restored, or restarted on a fresh Claude session;
  - changes activity or its count of pending asks;
  - has the permission mode of its stopped state, its model or its effort changed;
  - has its main conversation make a request or finish a turn (new `cache` and `context`);
  - gets a new context usage or new applied settings from the CLI;
  - has one of its agents start, progress, make a request or end.
- `usage_changed`: a usage reading was taken or a turn finished; `GET /api/usage` has the new snapshot.
- `catalog_changed`: the shared catalog or default settings changed; `GET /api/catalog` has the new ones.
- `error {message, session?}`: a malformed frame, or a `subscribe` or a `history` that failed.

## REST

`GET /api/catalog` is what all discussions share, persisted: `models` and `commands` (shaped as in `catalog`) from the latest `initialize` of any Claude start, `terminal_commands` (names of the commands that only work in the terminal) from the latest `system/init`, and `defaults {model, effort}`, what `get_settings` reports applying for a child launched without a model or effort whose discussion still has none. Each is null until first reported. With `models` or `defaults` unknown at launch, the server starts one Claude in its state folder to read them (`initialize` and `get_settings`, no user message), then stops it; a failure waits for the next launch.

Each session of `GET /api/sessions` carries:

- `archived_at`: null while not archived.
- `model` and `effort`: null when the CLI picks.
- `settings_changed_at`: null unless a model or effort change awaits its first main request.
- `applied_model` and `applied_effort`: what the CLI reports applying through `get_settings`, asked when the child starts, after each turn and after a change, e.g. `claude-opus-5-5[1m]` and `medium`; kept once the child stops, null while unknown.
- `live`, `busy`, `asks`: the permission, question and plan approval asks its running child awaits an answer to, 0 when not live.
- `cache {last_request_at, ttl_secs}` and `context {used_tokens, max_tokens, percentage, source, autocompact_threshold, categories}`; either is null while unknown. `source` is `cli` when the total comes from the CLI's `get_context_usage` (asked when the child starts and after each turn), `estimate` when a later main request replaced it with its prompt tokens. `max_tokens` is the CLI's when known, else the model's context window. `autocompact_threshold` (nullable) and `categories [{name, tokens, kind}]` (empty while unknown) come from the latest CLI reply.
- `calls_per_message`: main-conversation requests per finished turn over the discussion's life, null before its first turn.
- `restart {window_start_tokens, seed_tokens, reread_tokens, other_cost_per_call}`: what a restart would keep, measured after each turn, null before (see `notes/restart-advice.md`). `window_start_tokens` is the prompt of the first main request of the oldest exchange a seed carries; `seed_tokens` the seed text; `reread_tokens` the files those exchanges touched; `other_cost_per_call` the mean cost of their main calls besides cache reads, in base input tokens.
- `agents`: the subagents the running child started, oldest first, empty when not live: `{task_id, tool_use_id, name, subagent_type, parent_tool_use_id, state, activity, cache, prompt_tokens, started_at}`. `tool_use_id` is the Agent call, which the agent's lines carry as `parent_tool_use_id`; `name` its description; `parent_tool_use_id` the Agent call of the agent that started it, null for the main agent; `state` `running`, `completed`, `stopped` or `failed`; `activity` its latest progress, then its final summary (nullable); `cache` as the session's, from its own last request (nullable); `prompt_tokens` that request's prompt, the context a relay to a cold agent caches again (0 before any).

REST errors answer `{"error": code}` with their status.
