# vigie2

A web app reachable from a phone that drives Claude Code headlessly on the host (`claude -p` with stream-json input and output) and renders a native chat UI. No TUI in the loop. Protocol research lives in `notes/protocol.md`.

Vigie (`../vigie`) stays unchanged and usable alongside it: "still allow to use vigie in the meantime without breaking anything or if the billing changes". Vigie's hook already does nothing without the `VIGIE_HOOK_URL` and `VIGIE_HOOK_SECRET` env vars, so vigie2 removes every `VIGIE_*` var from its children's env.

## Usage profile

- Supervised use, not unattended agents. The user reads and steers; on a big task whose plan was approved, glances quickly at the commands.
- 1 to 3 sessions at once, usually one per project, several projects in parallel.
- Continuing work on an existing project: usually a single Claude session, reused. When its cache is cold, the user starts a fresh session and pulls back the recent context (today `/clear` then `/tail-session`).
- Random experiments or discussions not tied to a project, with the user's own Claude Code instructions applied (the official Claude app lacks them).
- Used on the phone and on the computer.
- Designed for an "autistic adhd brain".

## Principles

- Reliability first. Pain points of the Claude mobile app: "messages are lost in transit, can't diff, stop orders are "sent" but not really".
- No "cold corporate feel". Code stays readable as a developer reads it: no hiding diffs.

## Access and clients

- Remote access and authentication work as in Vigie.
- Several clients (phone, computer) can be on the same session at once. A card answered on one client disappears on all of them.
- Compose drafts can be stored on the server.

## Sessions and history

- The host owns every session; clients connect to the server.
- A session is never continued in a terminal.
- vigie2 keeps its own history of messages per session and never asks the SDK or the transcript files for past messages: "history is not edited".
- vigie2 serves past context from its own history, more efficiently than a script reading transcripts. It can also serve as the backend of `/tail-session`.
- Search across all discussions, served from vigie2's history without touching Claude's context or the SDK.
- Background tasks started by Claude are visible to vigie2, shown in the session, and count in the session's idle state.
- vigie2 remembers where sessions were started; no favorites. A session that ended through a server reboot or a failure stays known, and is continued with the same choices as a cold cache.

## Continuity

- Project status panel: Claude keeps it current through the MCP server (goal, current phase, next step, blockers). It survives cold caches and new sessions, feeds the seed of a continued session, and answers "where was I" when a session is reopened.
- Daily journal: a per-day view across projects of what got done and what is pending, built from history without a model call.
- Device continuity: a client opens where another left off (scroll position, open card, draft).
- Offline reading: clients cache history so it stays readable on a bad network; sends wait in an outbox.

## Cache and context

- The top bar has two columns: the account usage windows (see Account usage), then the session: a context bar and a cache bar of the same length, the cache bar draining over the TTL. Side by side from about 600 px wide (unfolded phone, desktop), stacked below. Tapping the bar expands it to every detail (rate, work left, deadline, TTL). Home shows the account column only.
- Cache expiry is tracked from the last main-conversation request and the TTL each response reports it wrote.
- Push notification at 50 minutes of the 1 hour TTL, warning the cache will soon be cold.
- Push notification when the user is not looking at a discussion and their intervention is needed: an ask is queued, or the main agent finished answering with no tool use and no background task pending.
- Cold cache: sending and approving are blocked until the user picks one of:
  - rewarm: send on the cold cache;
  - continue: a new session seeded with past context from vigie2's history;
  - blank: a new empty session.
- The choice shows the context usage % and, when available, the estimated cost of rewarming in the 5h and 7d usage windows. At low %, rewarming costs about what a new session plus recovering the context costs; at high %, it costs too much.
- vigie2 never picks for the user; it only prevents interacting with a cold cache by accident.
- The choice is one panel replacing the composer, shared with archived sessions.

## Discussion view

- Classic chat layout: Claude's text in bubbles on the left, the user's on the right. A bubble spans the full width minus a fixed margin on the side opposite its sender. Bubbles are rectangular with small rounded corners (about 5px) and no speech tail.
- Bubbles hold text only; tool uses appear outside bubbles.
- Timestamps, in the device's local time and 24-hour clock: `HH:MM` the same day, `Tue HH:MM` within the last week, `May 12 HH:MM` beyond, with the year added when not the current one. Recomputed when the app comes back to the foreground.
  - Bubbles always show theirs, small and muted in the bottom-right corner, for both sides. The user's bubble shows when the server sent it to Claude; Claude's shows when its text started.
  - Tool rows show theirs right-aligned at the row's end, only when the minute differs from the item above. Expanding a row shows the exact time with seconds and the call's duration.
- A thin border in the focus-ring color marks each side: on the left of Claude's bubbles and tool rows (aligned), on the right of the user's bubbles.
- Scroll never yanked: new content arriving while the view is scrolled up does not move the view.
- Permission and question cards. Several asks queue; a new one never replaces the card being looked at, and never captures input meant for something else (the TUI failure: a prompt appearing while typing takes an Enter not meant for it).
- Approvals by swipe: right accepts, left rejects, with a way to add notes.
  - Tool-use asks and plan approval are swiped, notes included.
  - Every ask takes two swipes in a row in the same direction: one gesture for all asks, so no conflicting muscle memory. The first swipe shows "Confirm?" in that side's color; the second sends the answer at once. A tap, a swipe in the other direction, or waiting 3 s cancels.
  - A swipe can start anywhere on the panel. Its side is chosen by a minimum travel distance, large enough that a movement in that direction is never a fluke: at least 20% of the panel width and 56 px, mostly horizontal (horizontal travel at least twice the vertical) so scrolling never registers. Below the threshold the panel springs back.
  - Dangerous asks force a 1 s cooldown between the two swipes and show "Are you sure?", greyed out until the cooldown ends.
  - Desktop uses buttons instead: a single press answers. A dangerous ask shows an "Are you sure?" button that can only be pressed after the same cooldown.
  - The swipe is done on the panel representing the choice, so it can never confirm the wrong thing. The whole panel swipes, header included.
  - Panels that accept notes have a button opening a text field; the note is sent with the swipe. A deny note is the deny message; an allow note follows as a queued user message framed as attached to the approval.
  - Dangerous means what the CLI flags, plus vigie2's own checks on top: `rm -r` / `rm -rf`, `git push --force`, `git reset --hard`, writes outside the session folder.
  - The swipe is a shared component, used elsewhere in the app too (e.g. swipe left on a discussion to archive it).
  - Animation: at rest the panel is undecided yellow, the "waiting for a decision" color. While the finger moves, it goes toward the decision's green or red, and snaps to a more intense shade on reaching the threshold, like a latch. After the first swipe, the yellow fills back in over the 3 s, showing the choice fading away. The fill sweeps toward the chosen side: left to right after a right swipe, right to left after a left swipe.
- Diffs viewable like a developer reads code.
- Subagents integrated directly, their discussion visible.
- Notification and visibility settings per session or project. Example: an RPG with a game-master agent keeping a secret adventure state must not leak it.
- Session controls: model, effort, permission mode.
- Stop: a floating button above Send, shown only while a turn runs (not for background tasks alone). Send stays available to queue messages.
- Session control tabs (permission mode first; model, effort, agent later) float over the bottom of the discussion, resting on the composer's top edge on the left, without a solid background line: while scrolling they peek over the bubbles, and the discussion ends with enough space that, scrolled to the bottom, the tabs cover nothing. Tapping a tab opens a list upward to pick directly (mode: Manual, Accept edits, Plan): only the other modes are listed, as square chips of the tab's width in their mode's color. Each mode has its own tab color: yellow for Manual, teal for Accept edits, purple for Plan.
- The permission mode is stored per session and kept current (picked in the tab, or reported by the CLI). Every start of the child (resume after a cold cache, a crash or a server restart) passes it with `--permission-mode`, so a session comes back in its mode. On a stopped session, picking a mode only stores it; the next message starts the child in it. New sessions start in Manual; a cold-cache "continue" inherits the mode of the session it continues.
- Picker for skills and slash commands.
- File references (`path:line`) and URLs in messages are links, opened inside the app.
- Attachments in prompts, including screenshots taken on the phone, better integrated than in Vigie.
- Commit card: a commit is presented as a card, not a bare command. Generalizes to custom presentations: specific commands get a customized interface.
- Verification chips: checks (command, pass or fail, output tail) reported through the MCP server, expandable. Checks are registered per project, so the full list shows, including the ones skipped; the agent answers why a check was not run (not needed, not relevant, ...).
- Plan file progress shown in the session. Mentions of a plan part (e.g. "Phase 3 of the plan") are clickable and show that part.
- Glance mode: the usage module, a quick look at and approval of asks, and the last messages of the discussion.

## MCP server

- vigie2 exposes an MCP server to its sessions. Claude drives the UI through tool calls instead of text syntax parsed out of messages.
- Presentation: show images (e.g. a screenshot a command took), file excerpts, diffs, and other content in the discussion.
- Interaction: custom blocking widgets whose answer returns as the tool result, a richer `AskUserQuestion`.
- Panels with state: e.g. an RPG character sheet the game master updates, or a status report while working on a project.
- Ambience: music, sounds, theme or background changes.

## Projects and Home

- Projects Home designed anew. Vigie's Home (file explorer, favorites, discussions per project) is "a bit messy".
- No workspace concept: commands reach the whole machine anyway. Projects are the folders sessions were started in.
- Home lists projects and scratch discussions as two kinds of entries.
- Full directory explorer, built on a tree library unless a fitting file manager component exists. Folders can be opened from discussions.
- Tabs or similar for multitasking over files and folders. Discussions stay separate from them.
- Buttons switch between a session and its related folder or file, "so it becomes more fluid to switch between them".
- Opening a file or folder lands where expected. Vigie often lands in the wrong directory; the navigation logic is designed anew.
- Handoffs are detected and give their project a special status. Opening the session suggests starting from the handoff. The handoff, clear, read-handoff sequence is integrated as a workflow.
- canif scripts are first-class: shown with their description and what each argument is.
- Session rows on Home: a double swipe left kills a live session (stops its Claude process; it stays listed), a single swipe left archives a stopped one. No swipe right. Desktop has a Kill button on live rows and an Archive button on stopped rows. The session screen has no kill.
- Archived sessions sit in their own collapsed section of Home. Opening one shows it read-only, with the cold-cache panel (rewarm, continue, blank) in place of the composer; any choice unarchives it.
- No terminal support.

## Scratch discussions

- A discussion can start without a project, in a new folder under a configurable scratch root.
- A scratch folder can be promoted when the idea turns out interesting: vigie2 moves it to a chosen destination. vigie2 implements promotion itself; `/cd` is unavailable under `-p`.
- Scratch folders are never deleted automatically; deletion is a manual action.
- Scratch sessions start in a looser default permission mode than project sessions.

## Account usage

- Usage module inside the app, doing what the custom Claude status bar does today, and more: 5h and 7d windows with projections, stats. Remade in this service to avoid useless dependencies, porting the existing Rust code from `../claude-projection-status`.
- Estimates are calibrated from the tokens each turn reports, window usage measured before and after turns, and history. The same data feeds stats and projections.
- History stores the raw CLI lines and periodic `get_usage` samples from the first slice on, so the usage module starts with data.
- The usage storage is vigie2's own schema, starting from fresh data; nothing is imported from `claude-projection-status`'s `state.db`.
- The backend ports every feature of the status bar (windows, projections, rate, work left, deadline, fun line facts, model mix, subagent share, bypass). The frontend shows them through the top bar; the others get their own presentation.
- Window bars: the spent part solid, the projection radiating horizontally from its edge toward the right, brighter when well supported and fainter when barely. Values animate when they change.
- Readings are `get_usage` samples, taken only while a session is live, and `rate_limit_event` lines. When the newest is older than 10 minutes, the account column dims and shows its age.
- Fit check: before starting a phase or task, warn when it likely does not fit in the remaining 5h window, with the reset time.
- Cost per phase or task, as a share of the usage windows.

## Usage credits

- Usage credits are disabled on the user's account. Their use is detected on a best-effort basis; on detection vigie2 stops the discussion automatically and warns the user so they can investigate.
- Signals: `rate_limit_event` reporting overage use or an `overage` limit type, main-conversation cache writes moving from the 1 hour to the 5 minute TTL, `get_usage` extra-usage credits increasing.

## Host

- One long-lived `claude` child per session: background tasks run inside it and start turns on their own. Idle children are reaped and restarted with `--resume` on demand, also after a server restart.
- Own serde types for the stream-json protocol, validated against the recordings. Unknown fields pass through as raw JSON. `claude-wrapper` serves as reference only.
- History in SQLite, for queries and search.
- Runs next to Vigie on its own port and state directory. vigie2 sets its own env var on its children, so hooks can detect which host runs them.
- Children run without claude.ai connectors (`ENABLE_CLAUDEAI_MCP_SERVERS=false`): an unauthorized connector otherwise gets reported by the model in its replies.

## Frontend stack

- React, TypeScript and Vite: a single-page app served by the host server.
- Tailwind and shadcn/ui, restyled with vigie2's own theme rather than the shadcn defaults.
- Motion for gestures and animation (swipe cards).
- Recursive Mono for all text, prose included, so code-related text never misaligns. Its settings vary by role to keep the UI "varied while consistent".

## Development

- Lots of unit and integration tests. A fake `claude` replays recorded stream-json sessions, so tests cost no usage and stay deterministic. Recordings also catch regressions across Claude Code updates and show what changed.
- vigie2 is developed with vigie2 (dogfooding), so it must stay usable with degraded features. When it is too broken and a plain Claude Code TUI is used instead, the user's hooks, skills and tools must handle not running under vigie2.
- The first slice holds the core features the others build on. Authentication is copied from Vigie. Content:
  - Server with Vigie's authentication.
  - Sessions: start one in a chosen folder, list, reopen.
  - Child process with the auth gate: no API key in the child env; `apiKeySource` other than `"none"` stops the session.
  - SQLite history; clients connect over a websocket and get a replay on reconnect.
  - Chat view: streamed text, compact tool calls, composer, stop (interrupt).
  - Permission cards with allow and deny buttons, `AskUserQuestion` answers; one card shown, the rest queued.
  - Several clients on one session; a card answered on one disappears on the others.
  - Fake `claude` replaying the recordings in tests.
- Outside the first slice: directory explorer, swipe, cache and usage header, cold-cache blocking, MCP server, scratch folders.
- Second slice, making vigie2 the daily tool:
  - Markdown rendering in bubbles; diffs for Edit and Write.
  - Usage module (see Account usage).
  - Cache and context header with cold-cache blocking.
  - Session controls (model, effort, permission mode) and the slash command picker.
  - In-app pairing with a QR code.
  - Push notifications.
  - Session management: archive a session (hidden from Home, restorable) and kill it (stop its Claude process; the session stays listed).
  - Swipe approvals.

## Open

- Swipe details: whether questions use a card other than swipe.
- Delivery states for outbound actions (queued, on host, received by Claude, done) and how they show.
- When an idle child is reaped.
- Server to client events: raw CLI lines plus thin host events; their exact shape and the SQLite schema.
- Seed format for continuing in a new session, and the interface `/tail-session` would use.
- Scratch permission mode: which one.
- Auth gate (refuse API-key billing) and the rest of the billing kill switch.
- Projects Home design, including how projects and scratch discussions are laid out.
- Multitasking tabs: how they work on the phone.
- Which folder or file counts as related to a session (its cwd, the last file referenced, files it edited).
- Terminals for non-Claude tasks: approach undecided (ideas: fixed size, piping output and showing text instead of a terminal emulator, named run configurations such as canif scripts with live text output).
- Per-session and per-project notification and visibility settings: which settings.
- MCP server: tool list and schemas, routing calls to their session, pre-allowing vigie2's own tools, how skills or CLAUDE.md tell Claude when to use them.
- Sensory features (sound, music, animation): opt-in per device, global mute, reduced motion.
- Which commands get a custom presentation beyond commits, and how one is defined.
- Welcomed from other tools, not designed: permission cards for edits showing the diff, reconnect with sequence numbers and a replay buffer, word-level and side-by-side diffs, checkpoints and rewind, usage analytics by project, model and time, session fork, rename and auto-title, Mermaid rendering, QR code pairing.
- Welcomed, not designed: pinning messages to a session or project, one-tap quick replies customizable per project, two kinds of stop ("stop now" interrupts, "stop after this step" queues a steer).
- How checks are registered per project.
- Claude Code version check at startup (`claude --version`, `system/init.capabilities`), and how features degrade on an unsupported version.
- Raw protocol log per session: possibly unnecessary, can grow big.
- Lag when the phone's VPN sleeps. Ideas: self-signed certificate and port forwarding.
- Mascot and session comic: undecided.
- Brainstormed and not yet confirmed: step pace, steer box, tangent thread, grouped tool activity, dimmed intermediate chatter, phase-boundary card (testable changes, diff, commit and next phase as separate actions), an MCP tool opening a URL on the phone client for on-device testing.
