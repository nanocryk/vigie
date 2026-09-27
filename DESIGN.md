# vigie2

A web app reachable from a phone that drives Claude Code headlessly on the host (`claude -p` with stream-json input and output) and renders a native chat UI. No TUI in the loop. Protocol research lives in `notes/protocol.md`.

Vigie (`../vigie`) stays unchanged and usable alongside it: "still allow to use vigie in the meantime without breaking anything or if the billing changes".

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

- The session header shows, easily visible, the cache TTL and the context usage: the two session-level metrics.
- Cache expiry is tracked from the last main-conversation request and the TTL each response reports it wrote.
- Push notification at 50 minutes of the 1 hour TTL, warning the cache will soon be cold.
- Cold cache: sending and approving are blocked until the user picks one of:
  - rewarm: send on the cold cache;
  - continue: a new session seeded with past context from vigie2's history;
  - blank: a new empty session.
- The choice shows the context usage % and, when available, the estimated cost of rewarming in the 5h and 7d usage windows. At low %, rewarming costs about what a new session plus recovering the context costs; at high %, it costs too much.
- vigie2 never picks for the user; it only prevents interacting with a cold cache by accident.

## Discussion view

- Scroll never yanked: new content arriving while the view is scrolled up does not move the view.
- Permission and question cards. Several asks queue; a new one never replaces the card being looked at, and never captures input meant for something else (the TUI failure: a prompt appearing while typing takes an Enter not meant for it).
- Approvals by swipe: one side accepts, the other rejects, with a way to add notes.
- Diffs viewable like a developer reads code.
- Subagents integrated directly, their discussion visible.
- Notification and visibility settings per session or project. Example: an RPG with a game-master agent keeping a secret adventure state must not leak it.
- Session controls: model, effort, permission mode.
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
- Home lists projects and scratch discussions as two kinds of entries.
- Handoffs are detected and give their project a special status. Opening the session suggests starting from the handoff. The handoff, clear, read-handoff sequence is integrated as a workflow.
- canif scripts are first-class: shown with their description and what each argument is.
- No terminal support.

## Scratch discussions

- A discussion can start without a project, in a new folder under `<workspace>/.scratch`.
- A scratch folder can be promoted into the workspace when the idea turns out interesting. vigie2 implements promotion itself; `/cd` is unavailable under `-p`.
- Scratch folders are never deleted automatically; deletion is a manual action.
- Scratch sessions start in a looser default permission mode than project sessions.

## Account usage

- Usage module inside the app, doing what the custom Claude status bar does today, and more: 5h and 7d windows with projections, stats. Remade in this service to avoid useless dependencies, porting the existing Rust code from `../claude-projection-status`.
- Estimates are calibrated from the tokens each turn reports, window usage measured before and after turns, and history. The same data feeds stats and projections.
- Fit check: before starting a phase or task, warn when it likely does not fit in the remaining 5h window, with the reset time.
- Cost per phase or task, as a share of the usage windows.

## Usage credits

- Usage credits are disabled on the user's account. Their use is detected on a best-effort basis; on detection vigie2 stops the discussion automatically and warns the user so they can investigate.
- Signals: `rate_limit_event` reporting overage use or an `overage` limit type, main-conversation cache writes moving from the 1 hour to the 5 minute TTL, `get_usage` extra-usage credits increasing.

## Development

- Lots of unit and integration tests. A fake `claude` replays recorded stream-json sessions, so tests cost no usage and stay deterministic. Recordings also catch regressions across Claude Code updates and show what changed.
- vigie2 is developed with vigie2 (dogfooding), so it must stay usable with degraded features. When it is too broken and a plain Claude Code TUI is used instead, the user's hooks, skills and tools must handle not running under vigie2.
- The first slice holds the core features the others build on. Authentication is copied from Vigie.

## Open

- Swipe details: which side accepts, the notes gesture, an undo window before the answer is sent, handling of asks the CLI flags as dangerous (`default_to_no`), whether questions and plan approval use cards other than swipe.
- Delivery states for outbound actions (queued, on host, received by Claude, done) and how they show.
- Process model: one long-lived child per session or one per turn; reaping idle children.
- Event schema between server and clients; history storage format.
- Seed format for continuing in a new session, and the interface `/tail-session` would use.
- Scratch permission mode: which one.
- Auth gate (refuse API-key billing) and the rest of the billing kill switch.
- Keeping headless children from firing Vigie's Claude Code hooks; ports and state directory next to Vigie.
- Projects Home design, including how projects and scratch discussions are laid out.
- Terminals for non-Claude tasks: approach undecided (ideas: fixed size, piping output and showing text instead of a terminal emulator, named run configurations such as canif scripts with live text output).
- Per-session and per-project notification and visibility settings: which settings.
- MCP server: tool list and schemas, routing calls to their session, pre-allowing vigie2's own tools, how skills or CLAUDE.md tell Claude when to use them.
- Sensory features (sound, music, animation): opt-in per device, global mute, reduced motion.
- Which commands get a custom presentation beyond commits, and how one is defined.
- Rust wire codec: own implementation, or `claude-wrapper` (MIT/Apache, duplex stream-json session) as dependency or reference.
- Welcomed from other tools, not designed: permission cards for edits showing the diff, reconnect with sequence numbers and a replay buffer, word-level and side-by-side diffs, checkpoints and rewind, usage analytics by project, model and time, session fork, rename and auto-title, Mermaid rendering, QR code pairing.
- Welcomed, not designed: pinning messages to a session or project, one-tap quick replies customizable per project, two kinds of stop ("stop now" interrupts, "stop after this step" queues a steer).
- How checks are registered per project.
- Content of the first slice.
- Claude Code version check at startup (`claude --version`, `system/init.capabilities`), and how features degrade on an unsupported version.
- Raw protocol log per session: possibly unnecessary, can grow big.
- Lag when the phone's VPN sleeps. Ideas: self-signed certificate and port forwarding.
- Mascot and session comic: undecided.
- Brainstormed and not yet confirmed: step pace, steer box, tangent thread, grouped tool activity, dimmed intermediate chatter, phase-boundary card (testable changes, diff, commit and next phase as separate actions), an MCP tool opening a URL on the phone client for on-device testing.
