# vigie2

A web app reachable from a phone that drives Claude Code headlessly on the host (`claude -p` with stream-json input and output) and renders a native chat UI. No TUI in the loop. Protocol research lives in `notes/protocol.md`, the client protocol in `notes/wire.md`.

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
- vigie2 serves past context from its own history, more efficiently than a script reading transcripts. `/tail-session` stays independent of vigie2, for when vigie2 is broken or not in use.
- Search across all discussions, served from vigie2's history without touching Claude's context or the SDK.
- Background tasks started by Claude are visible to vigie2, shown in the session, and count in the session's idle state.
- vigie2 remembers where sessions were started; no favorites. A session that ended through a server reboot or a failure stays known, and is continued with the same choices as a cold cache.

## Continuity

- Project status panel: Claude keeps it current through the MCP server (goal, current phase, next step, blockers). It survives cold caches and new sessions, feeds the seed of a continued session, and answers "where was I" when a session is reopened.
- Daily journal: a per-day view across projects of what got done and what is pending, built from history without a model call.
- Device continuity: a client opens where another left off (scroll position, open card, draft).
- Offline reading: clients cache history so it stays readable on a bad network; sends wait in an outbox.

## Cache and context

- The top bar has three slots: a fixed-width left slot (the vigie2 mark on Home, a Home icon on every other screen, pulsing like a Home row in the strongest state among the sessions not on screen: ask, unexpected stop, finished turn; not work), the gauges in the middle, and a fixed-width right slot with the connection status and the device settings cogwheel. The account column sits at exactly the same place on Home and in a session.
- The gauges have two columns: the account usage windows (see Account usage), then the session: a context bar (bar and numbers yellow from 50% of the window, red from 70%) and a cache bar of the same length, the cache bar draining over the TTL. Every gauge bar has a notch each 10%, except the cache bar: each 10 minutes of a 1 hour TTL, each minute of a 5 minute one. Side by side from about 600 px wide (unfolded phone, desktop), stacked below.
- On Home the session column is replaced by "needs you": pending asks across all sessions (colored when any) and the number of sessions working, on its first row; its second row stays empty so the top bar keeps its height.
- A thin bar under the top bar holds the session's name and status on the left (on Home, nothing), and in a session a "Project folder" button on the right, opening the session's explorer tab. Reloading is the browser's pull-down on the top bar.
- The system back button returns to the previous screen. The Home icon returns to Home and drops the screens in between from the history; opened directly on a screen (a notification), it replaces that screen with Home.
- In a session, the thin bar also shows two pills: the permission mode in its mode's color, and the model with its effort (`Opus 5.5 · high`). Tapping a pill opens the menu on that entry.
- Context usage comes from the CLI's `get_context_usage`, asked when the session's child starts and after each turn (total, window, autocompact threshold, categories). Between two asks, each main-conversation request updates the total from its own prompt tokens, which is the same figure.
- Cache expiry is tracked from the last main-conversation request and the TTL each response reports it wrote. A discussion with no recorded request but idle for longer than the longest TTL (1 hour) counts as cold.
- Push notification at 50 minutes of the 1 hour TTL, warning the cache will soon be cold. Not after a kill from Home: killing is how the user lets a discussion go for now. A crash, a revoke or a server stop still warns.
- Push notification when the user is not looking at a discussion and their intervention is needed: an ask is queued (main agent or subagent), the main agent finished answering with no tool use and no background task pending, or the child stopped unexpectedly.

## Devices

- The device settings screen lists paired devices as cards: label, a "this device" badge, last IP, and "Connected" while the device has an app open (at least one WebSocket), otherwise when it was last seen. Revoked devices are dimmed with a "Revoked" tag.
- Active devices can be renamed in place and revoked after a confirmation; revoked devices can be removed. The list updates live on any change, including a device connecting or disconnecting.
- Pairing a new device: pick the network address the link points at (LAN, tailnet, public, or a configured host), then generate. A card shows a QR code, the link with a copy button, and a countdown over the 5-minute single-use token. It closes when a new device pairs; once expired, the button offers a new link.

## Notifications

- Web Push (VAPID), keys generated on first start in the state directory. Subscription endpoints must be public HTTPS hostnames, checked at subscribe and at send; the sender does not follow redirects.
- Opt-in per device, in the device settings screen, opened from a large cogwheel button at the far right of the top bar, on every screen. The screen holds notifications and devices (see Devices).
- No push for a discussion an active client has on screen (see Seen, under Projects and Home).
- One notification per discussion: each push replaces the previous one of that discussion. Once the discussion is seen on any device, every device that got a push for it since it was last seen receives a silent push closing that notification, so they never pile up. Tapping one opens the discussion.
- Cold cache: sending and approving are blocked until the user picks one of:
  - rewarm: send on the cold cache, in the same Claude session;
  - continue: a fresh Claude session seeded with past context from vigie2's history;
  - blank: a fresh empty Claude session.
- Continue and blank keep the same discussion: same Home entry, same history, same permission mode. Only the Claude session behind it changes; later messages go to the new one. The discussion shows a divider row across its full width, on a solid background: "Restarted: N exchanges given as context" or "Restarted: blank"; tapping it shows the seed that was sent.
- Seed of a continue, built from vigie2's history as `/tail-session` renders a transcript: a header "This is a new session. The previous vigie2 session in <folder> (started <date>, M exchanges) was restarted into this one. Its last N exchanges, oldest first:", then the last 5 exchanges (an exchange is a user message and everything until the next one). Each keeps the user text cut at 1200 characters, each Claude text block cut at 800, and one line per tool call `Name(key=arg) → result` with argument and result cut at 100; thinking is dropped. The message the user had typed follows the seed in the same first message, so no turn is spent on the seed alone; with nothing typed, the seed ends with "The user has not typed a request yet: summarize where things stand and ask what to do next."
- The choice shows the context usage % and, when available, the estimated cost of rewarming in the 5h and 7d usage windows. At low %, rewarming costs about what a new session plus recovering the context costs; at high %, it costs too much.
- vigie2 never picks for the user; it only prevents interacting with a cold cache by accident.
- The server enforces it too: while the cache is cold (by the same rule as the client), a message or an ask answer is refused unless it carries the rewarm choice; restarting (continue, blank) and interrupting are always accepted. A client bug cannot bypass the choice.
- The choice is one panel replacing the composer, shared with archived sessions. Rewarm is red, continue green, blank blue. Rewarm asks for confirmation behind a large warning that it re-sends the whole context at full price.

## Discussion view

- Classic chat layout: Claude's text in bubbles on the left, the user's on the right. A bubble spans the full width minus a fixed margin on the side opposite its sender. Bubbles are rectangular with small rounded corners (about 5px) and no speech tail.
- Bubbles hold text only; tool uses appear outside bubbles.
- Thinking: while it is the latest item of a running turn, a "thinking" line with three dots brightening and rising in turn. An empty thinking (no readable text) disappears once anything follows; one with text stays as a collapsed "thought".
- While a turn runs with nothing on screen showing it (no tool running, no live thinking, no text streaming), the discussion ends with the same dots, alone; they go as soon as something shows. A running tool row's status shows them too.
- Background tasks (commands Claude runs in the background; subagents show as agents) end the discussion with one muted row each: "In background: <description> · <running time>" while a turn runs, "Waiting for: <description> · <running time>" with the animated dots once the turn ended and Claude waits for them to continue. Each row has a Stop button. A stopped task does not wake Claude, so stops made while it waits for them gather for 5 seconds (each new stop restarting the wait), then one message sent in the user's name wakes it: "Stopped background task: <description>", or the list for several. The discussion shows that message as a "Wake" tool row, recognized by its wording, not as the user's bubble. None is sent during a turn, which sees the notice at its end, nor on a cold cache, where the notice waits for the next message. The thin bar shows a "N background" pill beside the agents pill while any runs; tapping it scrolls the discussion to its bottom.
- A background task's end, which the CLI writes as a user message marked as a task notification, shows as a tool row, never as the user's bubble: `ok`, `--` (stopped) or `err`, "Background", and the notification's summary. Tapping it shows the raw notification and, when the task wrote to a file, an "Open output" button opening it in the file explorer.
- Both sides' bubbles render markdown (GFM tables, lists, code blocks with syntax highlighting), at 14 px like the composer. A toggle at each bubble's bottom switches it between rendered and raw, the raw view showing the markdown source to copy with its formatting. Links open in a new tab on desktop and in the external browser from the installed app.
- Timestamps, in the device's local time and 24-hour clock: `HH:MM` the same day, `Tue HH:MM` within the last week, `May 12 HH:MM` beyond, with the year added when not the current one. Recomputed when the app comes back to the foreground.
  - Bubbles always show theirs, small and muted in the bottom-right corner, for both sides. The user's bubble shows when the server sent it to Claude; Claude's shows when its text started.
  - Tool rows show theirs right-aligned at the row's end, only when the minute differs from the item above. Expanding a row shows the exact time with seconds and the call's duration.
- A thin border in the focus-ring color marks each side: on the left of Claude's bubbles and tool rows (aligned), on the right of the user's bubbles.
- Scroll never yanked: new content arriving while the view is scrolled up does not move the view.
- A discussion opens at its bottom, on its last 20 exchanges, shown at once. Scrolling near the top of what is loaded fetches the 20 exchanges before, without moving what is on screen. The start shows "Beginning of discussion". Discussions can span weeks, so the whole history is never sent at once.
- One button slot at the bottom right of the discussion. Scrolled up: "Latest", back to the bottom. At the bottom with the user's last message off screen: "↑ Last message", scrolling that bubble to the top of the view, so what Claude did since reads downward; the bubble glows once, fading out, to draw the eye. At the bottom with it on screen: no button. In an agent's view, the last message is the last one relayed to it.
- Permission and question cards. Several asks queue; a new one never replaces the card being looked at, and never captures input meant for something else (the TUI failure: a prompt appearing while typing takes an Enter not meant for it).
- Approvals by swipe: right accepts, left rejects, with a way to add notes.
  - Tool-use asks and plan approval are swiped, notes included.
  - Every ask takes two swipes in a row in the same direction: one gesture for all asks, so no conflicting muscle memory. The first swipe shows "Confirm?" in that side's color; the second sends the answer at once. A tap, a swipe in the other direction, or waiting 3 s cancels.
  - A swipe can start anywhere on the panel. Its side is chosen by a minimum travel distance, large enough that a movement in that direction is never a fluke: at least 20% of the panel width and 56 px, mostly horizontal (horizontal travel at least twice the vertical) so scrolling never registers. Below the threshold the panel springs back.
  - Dangerous asks force a 1 s cooldown between the two swipes and show "Are you sure?", greyed out until the cooldown ends.
  - The panel swipes with a mouse too. Under it, on every device, a Deny button on the left and an Allow button on the right frame the "swipe twice" hint: a single press answers; a dangerous ask turns the pressed button into "Are you sure?", which can only be pressed after the same cooldown.
  - The swipe is done on the panel representing the choice, so it can never confirm the wrong thing. The whole panel swipes, header included.
  - Panels that accept notes have a button opening a text field; the note is sent with the swipe. A deny note is the deny message; an allow note follows as a queued user message framed as attached to the approval.
  - Edit-tool asks (Edit, Write, MultiEdit, NotebookEdit) have an "Accept edits" toggle beside the notes button: allowing with it on also switches the discussion to Accept edits. Hidden on dangerous asks.
  - Asks the CLI pairs with a folder suggestion (access outside the allowed folders) show the path as segments. None is highlighted by default; tapping a segment highlights it, meaning "allowing also allows this folder for the session", and tapping it again clears it. Any segment can be picked, so a parent covers its subfolders at once. The grant keeps the kind the CLI suggested, moved to the picked folder: read access for Read, Glob, Grep and Bash, a working folder for Write. Picking a folder matching the list of bad picks (drive roots, the home folder, `~/.claude`, `Documents`, ...) makes the ask dangerous. The grant lives in the Claude process: after a restart it is asked again.
  - Read and edit-tool asks have an "Always for this file" toggle, shown on dangerous asks too. Allowing with it on makes vigie2 itself allow the later asks on that exact file of the same kind (reading, or any edit tool), from the main agent or a subagent; each shows a one-line "Auto-allowed" row in the discussion. When the ask is not a CLI safety check, the allow also sends the CLI a session rule for the file, so the CLI stops asking. vigie2 answers safety-check asks itself because the CLI ignores rules for them (edits under `~/.claude`). The file grants last as long as the Claude process: a restart of it or of vigie2 asks again.
  - The danger reasons have a copy button: the swipe surface leaves no text to select.
  - Bash asks show, above the command, the text Claude wrote before the call in the same message (one round of text and tool calls, before their results come back; none when that message has no text), muted and height-capped: the command's explanation.
  - Dangerous means what the CLI flags, plus vigie2's own checks on top: `rm -r` / `rm -rf`, `git push --force`, `git reset --hard`, writes outside the session folder.
  - The swipe is a shared component, used elsewhere in the app too (e.g. swipe left on a discussion to archive it).
  - Animation: at rest the panel is undecided yellow, the "waiting for a decision" color. While the finger moves, it goes toward the decision's green or red, and snaps to a more intense shade on reaching the threshold, like a latch. After the first swipe, the yellow fills back in over the 3 s, showing the choice fading away. The fill sweeps toward the chosen side: left to right after a right swipe, right to left after a left swipe.
- Diffs viewable like a developer reads code. Edit and Write rows show their diff as the TUI does: `Update(path)` with the path relative to the session folder, an "Added N lines, removed M lines" summary, a line-number gutter, added lines on teal with `+`, removed lines on red with `-`, syntax highlighting. Expanded by default and collapsible; a reload expands them again. The raw tool result stays one toggle away. While an edit waits for approval, its diff is in the approval panel (the thing swiped), and the discussion shows a one-line "awaiting approval" row in its place.
- A turn whose Edit, MultiEdit or Write calls succeeded, the main agent's and its subagents', ends with one row: "Changed 4 files, +120 -30", lines summed over the calls. Tapping it opens the session tab's diff view; in a folder outside a repository the row is not tappable. Changes made through Bash are not counted.
- Subagents integrated directly, their discussion visible.
  - An agent is named by its task description (the Agent call's `description`), as the TUI does.
  - Discussions never mix: a subagent's discussion shows only in its own view, reached only by switching to it manually. The main discussion shows one line when an agent is created, nothing of its discussion. vigie2 still receives the subagents' text (`--forward-subagent-text`), each line routed to its agent's view only; workflows keeping a subagent secret rely on not switching to its view. An agent's view opens with its brief, the prompt its parent gave it, as a folded row like a tool call.
  - A subagent's permission asks reach the parent's ask panel (the CLI sends them on the parent's control channel, with `agent_id`). An ask from any agent can be answered from any agent's view.
  - When the ask belongs to another agent than the one on screen, its card offers a "Go to agent: <name>" button switching to that agent's discussion for context. In that agent's own view, the button is absent.
  - Glance: a pill in the thin bar, next to the mode and model pills, gives the number of agents working ("2 agents working"); hidden when no agent exists. Tapping it opens the agents panel.
  - The agents panel (also a menu entry) shows the tree of agents, each with its current or last activity. It hides agents last active more than 1 hour ago, except the one on screen. Tap or swipe right switches to that agent's discussion; double swipe left stops it (`stop_task`). On desktop, buttons. The menu's Agents entry is also a panic control stopping every running agent: double swipe left on it; on desktop, a "Stop all" button beside it, pressed twice. Both stay available with no agent running, for discoverability, and then only say so.
  - Messages to a subagent: `-p` cannot reach a subagent directly, so a message typed in an agent's view is relayed by the main agent. vigie2 sends the main agent a wrapped message: a short note that the user is writing to agent `<name>` (task `<task_id>`), that vigie2 cannot reach it directly so the text passes through it, that it must not act on or comment the text, only forward it verbatim with SendMessage to `<task_id>`, then reply exactly `[Relay acknowledged]`; then the text. In the agent's view the message shows as a user bubble marked "via main agent". In the main discussion the wrapped message, the SendMessage call and the acknowledgement are hidden when the relay was faithful (SendMessage `to` is that task id and `message` the exact text); anything else shows as usual. A failed relay (other recipient, altered text, no acknowledgement) shows a warning line in the agent's view pointing to the main discussion. A relay sent while the main agent works is queued like any message.
  - A relay to a finished agent resumes it. Subagents cache with a 5-minute TTL (the main agent 1 hour): an agent's view has its own cache bar over 5 minutes from that agent's last request, and a relay to an agent whose cache is cold goes through the cold-cache confirmation, showing the context to re-cache. The main discussion's own cold check applies to the relay first.
  - An agent's view shows that agent's context, never the main one's: the prompt of its last request as a token count ("84k") over a dashed empty bar, "--" before its first request. The agents panel shows the same count after each agent's state ("done · 84k").
- Notification and visibility settings per session or project. Example: an RPG with a game-master agent keeping a secret adventure state must not leak it.
- Session controls: model, effort, permission mode.
- Send takes gestures: tap or swipe right sends, swipe left stops, swipe up opens the menu. Single swipes, no confirmation. Stop works only while a turn runs (not for background tasks alone); Send stays available to queue messages. Nothing floats over the discussion.
- Send's borders show the directions: a red left border pulsing while stopping is possible, absent otherwise; a top border in the theme's text color for the menu.
- The composer's buttons keep the height of a one-line text area and stay at the bottom as the text grows. The bar has a taller minimum height and Send is wide, an easy target to press or swipe. Swipe thresholds on Send are fixed distances, not a share of its width.
- On desktop, Send's gestures are three buttons: Send, Stop and Menu. Stop is always shown; while no turn runs it is inactive, with a red border on a black background. Enter sends. Esc stops the turn; with the menu open, Esc closes the menu instead; with no turn running, it does nothing.
- The menu opens upward over the discussion, anchored on the right. It holds the controls that need no single-tap access, as large readable entries: permission mode, model, effort, commands, agents, restart, revoke all grants. Restart opens the panel of a model or effort change on unchanged settings: Resume (nothing changes), Continue (fresh session with the last 5 exchanges) or Blank, plus Cancel. Revoke all grants stops the Claude process, dropping the folders and files granted from asks; the next send resumes the same session. The mode entry shows all modes on one line, colored per mode: yellow for Manual, teal for Accept edits, purple for Plan. The current mode is filled; the others have a border in their color on a black background.
- With a keyboard, Shift+Tab cycles the permission mode in the TUI order: Manual, Accept edits, Plan, Manual. A mode outside that list cycles as Manual.
- The permission mode is stored per session and kept current (picked in the menu, or reported by the CLI). Every start of the child (resume after a cold cache, a crash or a server restart) passes it with `--permission-mode`, so a session comes back in its mode. On a stopped session, picking a mode only stores it; the next message starts the child in it. New sessions start in Manual; a cold-cache "continue" inherits the mode of the session it continues.
- Model and effort: a pick is stored per discussion and passed on every start of the child; without a pick the CLI chooses. The menu always shows what the CLI actually applies, read with `get_settings` (`applied.model`, `applied.effort`) when the child starts, after each turn and after a change, and kept for stopped discussions: e.g. "Opus 5.5" and "medium effort", never "Default" or "auto". Only those two values are kept from that reply, which also carries the user's settings. The catalog (models, commands) and the applied values with no pick are shared by all discussions: the server keeps the latest seen from any Claude start, persisted, so a stopped or cold discussion still shows its values and opens its lists; a discussion without its own applied values shows its pick, else those defaults. With nothing stored yet, the server starts a short-lived Claude at launch to read them, without any model request. Since a change reloads the cache (model always; effort treated the same), the model and effort menu entries each open a single choice, "Stop here and change model" (or effort). It interrupts a running turn and puts the discussion in the cold-cache state: the choice panel shows and the entry used opens its list (models from the CLI's `initialize`; effort levels those the chosen model supports, none on a model without effort). While the panel shows, the menu's model and effort entries open their lists; a pick, a tap elsewhere or Escape closes it. The panel's first button is a green "Resume, same model" when nothing changed (no cache cost), or a red "Rewarm with new settings" with the reload warning and a confirmation when something did; continue and blank start on the picked settings.
- Commands: the menu's commands entry opens a panel in front of the discussion, one card per command with its name, a short description and its argument hint. vigie2's own commands come first: Clear (a fresh Claude session and a "Cleared" divider, like blank, without the cold check), Change model, Change effort. Then Claude's commands and skills from `initialize`, minus the terminal-only ones. A card is used by a double swipe right, or on desktop a Use button on its right pressed twice; for Clear that is its confirmation. The filter field sits at the bottom, in the composer's place. Typing `/` as the composer's first character opens the same panel, the composer text acting as the filter. A command without an argument is sent at once; one with an argument turns the bottom field into its argument entry, prefilled with `/name `. A composer draft present when the panel is used is never touched: the command goes alone, and the draft is back afterwards. A `/...` typed in the composer is the command itself and is consumed.
- File references (`path:line`) and URLs in messages are links, opened inside the app.
- Attachments in prompts, including screenshots taken on the phone, better integrated than in Vigie.
- Commit card: a commit is presented as a card, not a bare command. Generalizes to custom presentations: specific commands get a customized interface.
- Verification chips: checks (command, pass or fail, output tail) reported through the MCP server, expandable. Checks are registered per project, so the full list shows, including the ones skipped; the agent answers why a check was not run (not needed, not relevant, ...).
- Plan file progress shown in the session. Mentions of a plan part (e.g. "Phase 3 of the plan") are clickable and show that part.
- Glance mode: the usage module, a quick look at and approval of asks, and the last messages of the discussion.

## MCP server

- vigie2 exposes an MCP server to its sessions. Claude drives the UI through tool calls instead of text syntax parsed out of messages.
- Hosted in each session's actor as an SDK MCP server: calls arrive on the child's control channel, so they reach their session with no port or auth. vigie2's tools are allowed without asking; their descriptions alone tell Claude when to use them.
- Attachments: `set_attachments` sets the files shown on Claude's reply at the end of the turn; each call replaces the list, so a wrong file is dropped by calling again (see `notes/wire.md`).
- Presentation: show images (e.g. a screenshot a command took), file excerpts, diffs, and other content in the discussion.
- Interaction: custom blocking widgets whose answer returns as the tool result, a richer `AskUserQuestion`.
- Panels with state: e.g. an RPG character sheet the game master updates, or a status report while working on a project.
- Ambience: music, sounds, theme or background changes.

## Projects and Home

- Projects Home designed anew. Vigie's Home (file explorer, favorites, discussions per project) is "a bit messy".
- No workspace concept: commands reach the whole machine anyway. Projects are the folders sessions were started in.
- Home lists projects and scratch discussions as two kinds of entries.
- A full-screen file explorer (see File explorer), opened from a "File explorer" button on Home. Sessions start from it; there is no path field.
- Buttons switch between a session and its related folder or file, "so it becomes more fluid to switch between them".
- Opening a file or folder lands where expected. Vigie often lands in the wrong directory; the navigation logic is designed anew.
- Handoffs are detected and give their project a special status. Opening the session suggests starting from the handoff. The handoff, clear, read-handoff sequence is integrated as a workflow.
- canif scripts are first-class: shown with their description and what each argument is.
- Items with positive and negative actions take them by swipe: negative ones to the left, positive ones to the right. On desktop, each action's button sits on the side the item slides toward. On touch screens, the action is shown either by a button or by a hint above the list, on that same side.
- Session rows on Home: a double swipe left kills a live session (stops its Claude process; it stays listed), a single swipe left archives a stopped one, a single swipe right restores an archived one. The background a swipe reveals is red for kill, blue for archive and green for restore, snapping brighter once the swipe reaches its threshold, like the ask panel. On touch screens, a small "‹ Kill / Archive" hint sits once at the left above the first active row, and "Restore ›" at the right above the first archived row. Desktop has Kill and Archive buttons on the row's left and Restore on its right, narrow with the label written vertically. The session screen has no kill; its menu's "Revoke all folders" stops the process the same way, only while no turn runs. The discussion shows why the process stopped ("Stopped from Home", "Folders revoked", "Server stopped"), not the process exit.
- A session can be given a name, stored by vigie2; its folder never changes. The name shows wherever the folder name would (Home row, session title, push notifications). It is set from the Name entry of the session menu, archived sessions included; clearing it brings the folder name back.
- A session row shows its name (or folder name) and last activity time, then its path; no status line and no status dot. A row whose Claude process is not running has a dashed border and faded contents. While a kill or a restore runs, its label replaces the time.
- Session rows keep last-activity order, frozen while Home is visible so a row never moves under a tap: a new or restored session goes on top of its list, and the order refreshes on the next visit or when the app is hidden. A row's border pulses slowly in the color of its strongest state: yellow while an ask waits (until answered), red for an unexpected stop not yet seen, green for a finished turn not yet seen, a very dim blue while working. Idle and seen: no highlight. Unexpected stops are the child exiting unasked and a failed auth gate; Kill from Home, revoking folders and a server stop are not.
- Seen is shared across devices: a session is seen once any device has it on screen with an active user, the page visible and some input (pointer, touch, key, wheel) within the last 2 minutes. Scrolling is not input: the discussion scrolls itself as replies stream in. A window left open while away does not count.
- Archived sessions sit in their own collapsed section of Home. Opening one shows it read-only, with the cold-cache panel (rewarm, continue, blank) in place of the composer; any choice unarchives it.
- No terminal support.

## File explorer

- A full-screen page browsing the whole machine: into and out of folders, opening files a phone can display (text, images), creating folders, and "Start session here", which starts a new session in the current folder.
- Tabs, each remembering its folder, its open file and its scroll position, stored per device. Discussions stay separate from them.
  - Free tabs: one or more; the last one cannot be closed. Home's "File explorer" button opens the free tab used last.
  - Session tabs: one per session, opened by the session's "Project folder" button on the folder view (any diff or file closed), in the folder where it was left, or at the session's folder the first time. An attachment chip of the discussion opens its file in the session's tab. A session tab can browse anywhere; its header has a Project button returning to the session's folder and a Discussion button opening the session. Closing it only forgets its location; archiving the session removes it.
- A tab switcher menu, opened from the explorer header where the current tab's name shows, lists free tabs (folder icon, folder name, path) then session tabs (discussion icon, session name, current folder), each with a close button, and a "New tab" entry.
- One toolbar stays above every view of a tab (folder, diff, file): the tab switcher, then on the right Hidden in the folder view, Discussion and Project on a session tab, and Diff when inside a repository (pressed while the diff shows, closing it). Hidden comes first so the others keep their places when it is absent. Under it, each view's own bar: the breadcrumb, or a back button and a title. Every bar and body shares one content width.
- Places, where a new tab starts and the breadcrumb's first segment leads: the drives, the folders of current sessions, and common folders (home, Documents, Desktop, Downloads).
- The system back button goes up a level: it closes the open file, then goes to the parent folder, then from a drive root to Places; from Places it leaves the explorer. A file opened from an attachment chip is the exception: back returns to the discussion, until the user moves inside the explorer. The top bar's Home icon leaves it at once. A tab reopens as it was left.
- A folder lists subfolders then files, case-insensitively, each with its modification time, and files with their size. Dotfiles are hidden behind a toggle persisted per device. A breadcrumb leads back up; every segment is tappable.
- New folder: a name typed in place in the list; separators, `..` and existing names are refused.
- File viewer: text highlighted by language, up to 2 MiB (marked truncated beyond), with a rendered and raw toggle for Markdown; images (png, jpg, gif, webp, avif, bmp, ico, up to 16 MiB) fitted to the screen with their dimensions and size. Other files show that they cannot be displayed.

## Diff view

- Pending changes of a git repository: HEAD against the working tree, staged, unstaged and untracked merged into one view. Read through the `git` command line.
- A view of an explorer tab, like a folder or a file; the system back button leaves it the same way. Opened by a Diff button in the explorer header when the current folder is inside a repository, and by a Diff button in a session's thin bar when its folder is, which opens the session tab on it. Both buttons show their label on wide screens and only their icon on narrow ones, like Project folder.
- Top: a summary (files, lines added and removed), Refresh, and Collapse all or Expand all. Then the changed files grouped by folder, each with its status color and counts, tapping one scrolling to its diff.
- Then each file's diff, rendered by vigie2 itself, long lines wrapped: a header with status, path (old and new for a rename) and counts, tapping it collapsing or expanding the diff. Files over 200 changed lines and lockfiles start collapsed. A deleted file shows its removed lines. Binary files show a one-line note instead of lines.
- Tapping a line number on the new side opens the file view at that line; the back button returns to the diff. In a deleted file, tapping a line number opens the folder that held it, where it shows as a deleted row.
- Refreshed when opened, when the app returns to the foreground, and by Refresh; no file watcher.
- File view of a file with pending changes: its line-number column marks added lines green and modified lines orange (a block that both removes and adds), and draws a red rule between two line numbers where lines were removed. Opened from the diff, it scrolls to the line and flashes it. Tapping a marked line number opens the diff at that file and line, the row outlined for a moment.
- Folder view: a changed row has a colored left border, orange for a modified file, green for a new or untracked one. A folder is green when everything changed inside is new, orange otherwise, including when files inside are deleted. Files deleted directly inside a folder close its list as rows of their own on a dark red background with a red left border, marked "deleted"; tapping one opens the diff at it. Red never marks a folder, which would read as the folder being deleted.

## Scratch discussions

- A discussion can start without a project, in a new folder under the scratch root `~/.vigie2/scratch`, named by the local date and two random words (`2026-09-29-amber-fox`).
- A session is scratch while its folder sits directly in the scratch root; nothing else marks it.
- Home has a Scratch section under the session list, with a New scratch button that creates the folder and opens the empty session, its composer focused. Scratch sessions are listed there, not in the main list, with the same swipes. Archived ones join the shared Archived section.
- A scratch folder can be promoted when the idea turns out interesting: vigie2 moves it to a chosen destination. vigie2 implements promotion itself; `/cd` is unavailable under `-p`.
- Scratch folders are never deleted automatically; deletion is a manual action.
- Scratch sessions start in `acceptEdits`; project sessions start in `default`.

## Account usage

- Usage module inside the app, doing what the custom Claude status bar does today, and more: 5h and 7d windows with projections, stats. Remade in this service to avoid useless dependencies, porting the existing Rust code from `../claude-projection-status`.
- Estimates are calibrated from the tokens each turn reports, window usage measured before and after turns, and history. The same data feeds stats and projections.
- History stores the raw CLI lines and periodic `get_usage` samples.
- The usage storage is vigie2's own schema, starting empty and filled from live readings only; nothing is backfilled from history or imported from `claude-projection-status`'s `state.db`.
- The backend ports every feature of the status bar (windows, projections, rate, work left, deadline, fun line facts, model mix, subagent share, bypass). The frontend shows them through the top bar; the others get their own presentation.
- Each window row starts with the time left before its reset over the window length, as in the status bar (`3h55/5h`).
- Bars use the text color by default (white on the dark theme). Window colors follow the status bar:
  - the spent part is always the text color, red once 100% is used;
  - the projection, from the spent edge to the projected value, is green, yellow or red by the projected share: yellow from 75% and red from 90% on the 5h window, from 85% and 95% on the 7d window; more opaque when well supported and less when barely;
  - the used number is green, yellow from 40% and red from 70%; the projected number takes the projection's color, bold when well supported and faint when barely.
- Values animate when they change.
- Readings are `get_usage` samples, taken only while a session is live, and `rate_limit_event` lines. When the newest is older than 10 minutes, the account column dims.
- Fit check: before starting a phase or task, warn when it likely does not fit in the remaining 5h window, with the reset time.
- Cost per phase or task, as a share of the usage windows.

## Usage credits

- Usage credits are disabled on the user's account. Their use is detected on a best-effort basis; on detection vigie2 stops the discussion automatically and warns the user so they can investigate.
- Signals: `rate_limit_event` reporting overage use or an `overage` limit type, main-conversation cache writes moving from the 1 hour to the 5 minute TTL, `get_usage` extra-usage credits increasing.

## Host

- One long-lived `claude` child per session: background tasks run inside it and start turns on their own. Idle children are reaped and restarted with `--resume` on demand, also after a server restart. A Claude session no message reached yet has no conversation to resume, so it launches again with `--session-id` until one does.
- Own serde types for the stream-json protocol, validated against the recordings. Unknown fields pass through as raw JSON. `claude-wrapper` serves as reference only.
- History in SQLite, for queries and search.
- Runs next to Vigie on its own port and state directory. vigie2 sets its own env var on its children, so hooks can detect which host runs them.
- Children run without claude.ai connectors (`ENABLE_CLAUDEAI_MCP_SERVERS=false`): an unauthorized connector otherwise gets reported by the model in its replies.

## Frontend stack

- React, TypeScript and Vite: a single-page app served by the host server.
- Tailwind and shadcn/ui, restyled with vigie2's own theme rather than the shadcn defaults.
- Motion for gestures and animation (swipe cards).
- Recursive for all text, its settings varying by role to keep the UI "varied while consistent". Code (spans and blocks) is monospaced and fully linear; everything else (message prose, UI, headings) is proportional, prose and headings half casual. Text needing alignment comes in code blocks, so it keeps it.

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
- Auth gate (refuse API-key billing) and the rest of the billing kill switch.
- Projects Home design, including how projects and scratch discussions are laid out.
- Which folder or file counts as related to a session (its cwd, the last file referenced, files it edited).
- Terminals for non-Claude tasks: approach undecided (ideas: fixed size, piping output and showing text instead of a terminal emulator, named run configurations such as canif scripts with live text output).
- Per-session and per-project notification and visibility settings: which settings.
- MCP server: the tools beyond `set_attachments`, and their schemas.
- Sensory features (sound, music, animation): opt-in per device, global mute, reduced motion.
- Which commands get a custom presentation beyond commits, and how one is defined.
- Welcomed from other tools, not designed: permission cards for edits showing the diff, reconnect with sequence numbers and a replay buffer, word-level and side-by-side diffs, checkpoints and rewind, usage analytics by project, model and time, session fork, auto-title, Mermaid rendering.
- Welcomed, not designed ("all your new ideas are very good"):
  - rounds (one Claude message: text and tool calls, before their results come back) as the grouping unit, not rendered as frames: a round's parallel calls as one expandable row, earlier rounds' text dimmed or folded so the last round's answer stands out, a little more space between rounds than within one;
  - editing or cancelling a queued message until Claude has received it;
  - a suggestion to turn a Bash command allowed N times across sessions into a canif script;
  - the cost of each turn as a share of the 5h window, shown on the turn.
- Welcomed, not designed: pinning messages to a session or project, one-tap quick replies customizable per project, two kinds of stop ("stop now" interrupts, "stop after this step" queues a steer).
- How checks are registered per project.
- Claude Code version check at startup (`claude --version`, `system/init.capabilities`), and how features degrade on an unsupported version.
- Raw protocol log per session: possibly unnecessary, can grow big.
- Lag when the phone's VPN sleeps. Ideas: self-signed certificate and port forwarding.
- Mascot and session comic: undecided.
- Folder grants: whether a same-session restart within the TTL keeps the cache warm.
- Bash asks also carry a suggestion to always allow the exact command, saved in the project's local settings. The CLI's suggested command is "often either too broad or overly specific"; the ask panel would offer an improved version of it. How is not designed.
- Brainstormed and not yet confirmed: step pace, steer box, tangent thread, grouped tool activity, dimmed intermediate chatter, phase-boundary card (testable changes, diff, commit and next phase as separate actions), an MCP tool opening a URL on the phone client for on-device testing.
