---
title: The TUI — Canopy Hub
description: The full-screen terminal interface — agents, split views, dashboard, transfers.
order: 4
---

# The TUI — Canopy Hub

Running `canopy` with no arguments opens the Canopy Hub, a full-screen
[ratatui](https://ratatui.rs/) interface for managing everything in real
time.

## Creating agents

`Ctrl+N` / `n` opens the **NewAgentDialog** with three modes:

1. **Interactive** — PTY session with full vt100 emulation, 24-bit color
   and support for interactive applications.
2. **Background** — cron-scheduled or file-watcher-triggered agent.
3. **Terminal** — raw shell session with per-session command history
   (TOML-backed), cross-session autocomplete search, and a Warp-like
   input mode.

The dialog includes a CLI picker, model picker, seed identity selector
(`◀ None / SeedName ▶`), directory browser, and a yolo-mode toggle.

For **Interactive**, `Enter` on the finished form opens a second **Mode**
step instead of launching straight away:

- **New session** — exactly the launch the form always did: the launchpad
  opens (or the session starts directly when a new seed is being planted).
- **Resume a session** — lists the finished sessions that belong to the
  selected harness *and* directory, or the harness's own `session_list_cmd`
  output when it defines one. `Enter` on a session launches it resumed,
  skipping the mission step; a harness with nothing to resume shows
  `No sessions to resume for <harness> in <dir>` and `New session` stays
  usable.
  - Resuming the same harness conversation (a harness conversation id canopy
    has already recorded) reuses that session: it keeps its id and its name,
    so hooks and scheduled sends addressed to it still reach it afterwards,
    and `session_list` shows one row — never the old one plus a newly-named
    one. A conversation canopy has never seen starts a fresh session with a
    new id and a new generated name, as before.
  - `↑↓` selects, `Enter` confirms, and `Esc` returns to the form with every
  value kept. **Background** and **Terminal** launch from the form as
  before — the Mode step is Interactive-only.

> **Note:** The Session/Mode choice is no longer a row on the form itself.
> Resume is handled entirely on the separate Mode step, so the form never
> shows a "Resume <dir>" entry that could be mistaken for the launch button.

## Interactive sessions

- **Copying text** — drag inside a focused session pane to select and copy;
  the key-hint row shows `drag to copy`, so no keyboard modifier is needed.
  A `Ctrl+C` within 3 s of that copy is taken as the copy reflex and
  swallowed, and the pane shows `already copied`.
- **Interrupting a session** — a platform whose registry entry sets
  `ctrl_c_exits = true` (opencode) exits on a *single* `Ctrl+C`. To protect
  the copy reflex, the first `Ctrl+C` of such a session is held back and the
  pane shows `Ctrl+C again within 1 s to send it to <platform>`; press it
  twice within that second to forward the interrupt. Every other platform
  (claude, …) keeps forwarding `Ctrl+C` unchanged.
- **Scrolling up** — the scrollback keeps everything the session wrote,
  including output an inline TUI scrolled off the top of the screen, so
  scrolling up shows the transcript instead of stale rows.
- **Overlays keep the panel live behind them** — opening the prompt
  builder, the knowledge list or viewer, context transfer, RAG transfer, the
  launchpad or the new-agent form never replaces the panel behind it: a
  focused session keeps its live PTY with its own colours (no accent tint,
  no plain-text log), home and the graph view keep what they were showing,
  and the focus rail with the `Focus`/`Preview` pill keep naming the focus
  underneath — including when one overlay opens on top of another.
- **Surviving TUI restarts** — sessions live in the daemon, not the TUI:
  quitting (`F10`, closing the window) leaves every running session running,
  and reopening the TUI reattaches to them live, with screen and scrollback
  intact; a session that ended while the TUI was closed is not reopened.
  Only closing a session explicitly ends it. Restarting the daemon itself
  (`canopy daemon restart`) does end its sessions — they are its children —
  but the new daemon respawns every session the database still lists as
  active as soon as it starts, under the same id and name, resuming the
  harness conversation where the platform supports it. A terminal comes back
  in its working directory with the scrollback it had when the daemon shut
  down, above a `--- restored session history above ---` divider.
  While the TUI runs, a dropped daemon connection is retried on its own
  (0.5 s doubling up to 5 s) and the sidebar re-lists what the daemon still
  has; sessions started or closed from another TUI appear in, or leave, the
  sidebar live. When the daemon speaks an incompatible session protocol the
  footer shows `canopy daemon speaks session protocol <n>; restart the
  daemon (canopy daemon restart)` and nothing attaches until the restarted
  daemon matches.

## Monitoring

- **Agent status colors** — teal for working (recent output), blue for
  idle, red for failed, gray for exited. A running session whose daemon
  connection dropped shows dim gray until it reconnects. The sidebar shows
  a colored gutter bar per agent.
- **Split groups** — side-by-side horizontal/vertical views to watch
  multiple agents simultaneously.
- **System dashboard** — CPU, memory, disk, GPU (NVIDIA/Linux/macOS) and
  temperatures with amber/red alert thresholds. Under WSL, metrics come
  from the Windows host via PowerShell.
- **Live graph view** — real-time graph rendering of the running graph's
  current spec, auto-following the active node. When the engine moves to a
  new node, the view centres that node's whole box (a tall collapsed
  ensemble included) in the panel; your own scroll is left alone until the
  next node change. Manual node inspection shows per-node run info (status,
  elapsed time, iteration; a node parked on a named mutex shows `waiting`
  with a `waiting for:` line naming each resource and who holds it).
  `Esc` toggles between auto-follow and manual mode.
- **Output pane** — `t` opens a read-only viewer beside the live graph that
  streams the highlighted node's stdout/stderr as the engine writes it (agent
  nodes and check nodes alike). In auto-follow the pane retargets node by
  node as the graph advances; in manual mode it shows whichever node you
  select. `Enter` in manual mode toggles the pane on the selected node (in
  auto-follow `Enter` still enters manual mode). The layout tiles at 140
  columns or wider (graph left, pane right) and stacks below that.
  `PageUp`/`PageDown` scroll by half a screen, `End` returns to the live
  edge, and `,`/`.` step through an ensemble's members. Opening, closing or
  switching the pane never writes to a run.
- **Crews face** — pin a panel face with the `pinned_panel_face` setting
  (F1 → Settings → TUI Appearance); **crews** is shown by pinning
  `crews` there (never selected automatically). It lists every
  harness/model pair in use with its effective availability — `●`
  available, `○` unavailable, `◐` expired — its
  reason and expiry, and how many graphs and crews use it, plus every crew
  with its member and binding counts. Click the panel to focus it, then `Tab`
  switches between the Pairs and Crews sections. In Pairs, `Space` toggles the
  selected pair, `e` marks it unavailable with a reason and an optional
  `YYYY-MM-DD HH:MM` expiry, and `p` marks its whole provider prefix
  unavailable (showing how many pairs that covers first). In Crews, `Enter`
  expands a crew to its ordered members and bindings, `a` adds a member from
  the model picker, `d` removes one (refused when it would empty the crew),
  and `Shift+↑`/`Shift+↓` reorder members. Every roster and crew write goes
  through the daemon — the face never touches SQLite — and the view refreshes
  without blocking while a call is in flight.
- **Idle visuals** — Brian's Brain cellular automaton, drawn as Braille
  glyphs and seeded by the cursor at a fixed step rate, plus animated
  kaomoji status messages (whimsg). With no live event, messages come from
  curated forest-voice phrases — idle, busy or scrolling to match what the
  TUI is doing — and about a quarter of them are deliberate jokes. When
  something just happened, a real-event message takes over instead, naming
  the agent, graph, or spec involved (a failure never gets a joke). The header
  title (`harness-canopy`) and those kaomoji share one slow sweep: a dark
  highlight travels across the letters, then the text rests for 2 seconds
  before the next sweep.

## Gamification

Canopy tracks 28 achievement-style missions across six categories:

| Category | Missions | Examples |
|---|---|---|
| **Environment** | 4 | Firefly Catcher, Harness Master, Canopy Explorer, Multitasker |
| **Intelligence** | 6 | World Connector, Data Architect, Deep Searcher, Digital Archeologist |
| **Projects** | 3 | Project Polyglot, The Gardener, Data Hoarder |
| **Graph** | 4 | Automation Engineer, Pipeline Pilot, Parallel Vision, Graph Survivor |
| **Seeds** | 4 | First Bloom, Identity Evolved, The Orchard, Deep Roots |
| **SysInfo** | 7 | Full Throttle, Nuclear Winter, VRAM Squeezer, YOLO Pilot |

Missions are checked automatically during normal TUI operation — no
manual action required. Progress is persisted in the database and
visible in the TUI's gamification panel. `canopy missions [--json]`
lists the same missions read-only from the command line (see
[CLI Reference](cli-reference.md#missions)).

Two missions to know by their exact challenge: **The Gardener** —
update descriptions or tags for 5+ different projects (via the
`project` tool's `update` action); **Parallel Vision** — run a graph with a
parallel ensemble. On systems with no CPU temperature sensor (such as
WSL), **Nuclear Winter** shows as `not measurable on this system`.

Press **F1** to open the **Canopy hub**, a centered tabbed dialog:
**Settings** edits every registry setting in place (with the value's apply
mode and status), **Keys** is a grouped reference of every shortcut the TUI
handles, **Missions & stats** lists all the missions above — unlocked
ones in full colour, locked ones dimmed — **Shelf** lists the files
agents have sent (see below), and **Remote** is everything `canopy remote`
does (see below). `Shift+←`/`Shift+→`
switch tabs, as everywhere else in Canopy; `Tab`/`Shift+Tab` and `1`/`2`/`3`/`4`/`5`
also switch. `F1` closes the hub from any state; `Esc` closes it from the top
level of every tab. `F1` works everywhere — including inside a focused
interactive session whose harness has claimed the keyboard (alternate screen
or the Kitty protocol) — the key opens the hub instead of being forwarded to
the session.

In **Settings**, the `↑`/`↓` keys move within the focused column and `→` or
`Enter` step from the category list into its properties (`←` goes back, keeping
the same category selected). At the properties level `Enter` edits a number,
duration, string, format or path list, `Space` toggles a bool or cycles an enum
forward, and `r` resets the selected setting to its default. A path list edits
with `a` (add a folder), `d` (delete) and `Shift+↑`/`Shift+↓` (reorder). `/`
filters every group by key or description. Each change goes through the daemon;
a refused value keeps the old one and the daemon's message shows in the status
line. `←`/`→` never change a value.

In **Remote** (`F1`, then `5`), `↑`/`↓` (or `j`/`k`) move over the GitHub,
Remote control, Relay and Pair rows plus one row per paired device. `Enter`
on GitHub starts the device flow while signed out (the row shows the user
code and `https://github.com/login/device` until it completes or expires);
`Enter` on Remote control flips the relay switch (disabled while signed
out); Relay only reports the phase (`connected`, `connecting`,
`token rejected: sign in again`, `off`); `Enter` on Pair a device shows the
fresh code with its link, QR and countdown in an overlay (`Esc` hides it,
the code stays valid). Each device row shows its label, id, last seen time
and `live` while a peer is open; `d` disconnects its live peer at once
(the pairing stays), `r` revokes it behind a `y`/`n`
confirmation (`Enter` also confirms, `Esc` cancels); both report success
or the exact error inline. While any device is
live the sidebar header shows `⇄ n`.

### Session shelf

- **Sending** — an agent delivers files to its session with the MCP tool
  `session_send_file`: 1..10 absolute paths per call, up to 20 MiB each.
  PNG, JPEG, WebP, GIF and SVG are recognised from the file's content, not
  its extension.
- **Listing** — the hub's **Shelf** tab (`F1`, then `4`) lists the files
  agents have sent — the host shelf plus one per session, merged newest
  first — with arrival time, sender, name, size and caption per row.
- **Opening** — `Enter` (or `Space`) opens the selected file with the
  system viewer — `open` on macOS, `xdg-open` on Linux, `wslview` (or
  `explorer.exe` with the path translated by `wslpath`) under WSL. `c`
  copies its absolute path, and `d` deletes it after a `y`/`n`
  confirmation. A viewer that fails or is missing raises a toast naming the
  file and the error — which carries the path — instead of crashing the TUI.
- **Confirming** — while the `y`/`n` prompt is armed every key answers it:
  `y` or `Enter` deletes, `n` or `Esc` cancels, and nothing else moves — not
  even a tab switch.
- **Arrivals** — a file sent while the TUI runs joins the list live and
  shows a `📎 <name> — <from>` toast for five seconds, wherever you are in
  the TUI.
- **Lifetime** — a session's files are kept until 7 days after the session
  is removed, and the whole shelf is capped at 1 GiB: when a new file
  would exceed it, the oldest files are pruned first.

## Projects sidebar

The sidebar shows sections for the selected project:

- **Graphs** — active graphs with spec progress (done/total) and current
  spec indicator. Selecting a graph opens the live graph view.
- **Backlog** — standalone specs tagged to the project's workdir, not
  yet assigned to any graph. A spec with a type shows its code as a badge
  before the name (`[FIX-7] open F1 from inside a session`), followed by its
  type and scope dimmed (`(fix:tui)`). Legacy specs without a type show the
  name alone.
- **History** — completed and failed graphs for the project.

Sections are automatically filtered by the selected project's workdir.

## Knowledge tab

The sidebar's **Knowledge** tab lists the registered projects, most recently
active first — a project's last activity is the newest of its registration,
its indexing, its knowledge entries and its project history, so the projects
you are actually working on stay at the top instead of being buried under old
ones. Each row's first meta line shows how long ago that was, before the
project hash.

Typing a plain letter filters the list the way a file browser does: the filter
matches project names case-insensitively, the list keeps its recency order, and
the selection jumps to the first match. The panel title shows the live
`/ filter`, and `No project matches "<filter>"` appears when nothing does.
`Backspace` removes the last character, and `Esc` clears a non-empty filter
instead of leaving for the Home screen. Because every letter has to be
typable on this one list, the single-letter shortcuts it used to carry don't
apply there: arrows, `Enter` and `Esc` still do, the relation dialog moves to
`Ctrl+R`, and leaving the Knowledge tab clears the filter.

Once a project is entered, its **Knowledge** tab lists every fact and pattern
of that project as a row: a `[fact]`/`[pattern]` badge in its kind colour, the
title in bold, and how long ago the entry was last updated. `↑↓` moves the
selection and the list scrolls to keep it visible (showing `n / total` on its
first row), and clicking a row selects it. `Enter` opens a read-only viewer for
the selected entry — `↑↓` and `PageUp`/`PageDown` scroll the body, `e` closes
the viewer and opens the entry for editing, and `Esc` returns to the list with
the same row still selected. The list's own keys keep working on the selected
row: `/` filters, `n` creates, `e` edits and `F4` deletes.

## Moving context around

- **Scheduled delivery** — `Shift+Enter` (requires Kitty keyboard
  enhancement protocol support) opens a time picker to send the prompt
  at a chosen hour instead of immediately. Falls back to `Ctrl+S` on
  terminals without Kitty protocol.
- **Context transfer** — a two-step modal (preview → agent picker)
  transfers conversation context, prompts and output between agents while
  preserving session state and scrollback.
- **RAG transfer** — send semantic search results to another agent as
  injected context.
- **Prompt builder** — structured prompt templates with configurable
  sections (instruction, context, resources, examples) and @-mention
  agent references. Press `Ctrl+L` to recall the last prompt you sent
  in the current project, restoring the full prompt text, tools, and
  preset selection. The Tools picker (opened from the `Tools` section in
  the section-add menu) lists on-disk `[skill]:`/`[global]:` skills first,
  then canopy's dynamic skills as `[canopy]:<name>` (name-sorted, local
  cache only — no network). A name present both on disk and in canopy's
  store appears under both prefixes, so you pick which one to reference.
  Selecting a `[canopy]:` entry renders
  `canopy skill <name> — load it with the skill tool's get action (name="<name>")` in the
  built prompt's tools block, so the agent fetches it over MCP; on-disk
  skills keep their plain `skill:<name>` entry.
- **Canopy protocol — once per session** — the builder prepends the
  `[CANOPY]` session-start protocol to the *first* prompt of an
  interactive session, never to the ones that follow. That fact is stored
  with the session id in the database, so it survives restarting canopy
  and resuming the session: a conversation that already carries the
  protocol does not receive it again, and a resumed conversation keeps its
  session id, so the flag travels with it. When the protocol is
  *not* due, the builder shows nothing extra about it — to resend the
  block on purpose, add the **Canopy protocol** section from the section
  picker (`Ctrl+A`): it inserts a locked, read-only section holding the
  protocol text, the prompt then carries the block exactly once at the
  very top (before the per-turn workspace context), and that send counts
  as the session's one delivery. The section can be taken out again with
  the remove-section picker (`Ctrl+X`) like any other section.
- **Launchpad** — start new sessions with previous-mission recovery and
  auto-injected context.

## Graphs in the TUI

The graph editor allows inline editing of node config JSON with
validation. See [Graphs](graphs.md) for the engine itself.

## Notifications

Native desktop notifications for task completions, failures, watcher
triggers, and graph lifecycle events (graph started, spec completed with
progress, graph finished with outcome, blocker reported, completion hook
failure). Platform auto-detected: WSL (PowerShell toasts), macOS
(`osascript`), Linux (`notify-send`). Set `notifications.enabled = false`
to silence them.

A message delivered into this session by a graph hook or by an agent's
`send_to_session` trigger raises one too, naming the sender instead of
showing a raw id. `Hook message delivered` reads `<graph name> · <event>:
message delivered to this session` (for example `canopy-tui-polish ·
on_completed: message delivered to this session`); `Agent message
delivered` reads `<agent id> (<trigger>): message delivered to this
session`. When the graph or agent no longer exists, the first 8
characters of its id stand in for the name.

## Colour and tinting

The TUI speaks the same palette as the web surfaces. Both themes — `classic`
(bordered, Canopy green) and `modern` (borderless) — paint the big views and
the alert dialogs on meaning-tinted grounds instead of one flat green slab.
In `classic` those grounds are the operator's dark palette: tints of the same
lightness, differing only in hue, plus a neutral stage that big views stand
on.

- **Prompt Builder** — the window sits on the info tint, its section fields
  on the near-black stage ground, and the focused section on the selection
  colour. Borders and the title keep the theme's own colours, and the text
  on the new grounds stays at 4.5:1 contrast or better.
- **Graph view** — the canvas sits on the same info tint. Nodes and edges
  keep their status colours, which stay at 3:1 or better against it.
- **Alert dialogs** — confirmations of destructive actions are tinted for
  warning: deleting a project, permanently deleting a graph, and resetting a
  graph. A failed graph action is reported on the error tint. Every dialog
  still spells out what it does in words, so the tint is never the only
  signal; the quit and archive confirmations keep the neutral dialog ground.

The sidebar, header, footer and session panes are unchanged.

## Appearance and behaviour settings

The `[tui]` settings (see `canopy config list`) tune the interface without a
restart. Each keeps its default until you set it. All of them — and every
other registry setting — can also be edited in place from the hub's
**Settings** tab (`F1`), which writes through the daemon and shows each
value's apply mode.

| Setting | Default | What it does |
|---|---|---|
| `tui.datetime_format` | `%Y-%m-%d %H:%M` | chrono format for dates and times (validated by formatting a fixed instant). |
| `tui.time_format` | `%H:%M` | chrono format for time-only labels (validated the same way). |
| `tui.panel_dwell_seconds` | 10 | Seconds a panel event holds the right panel (0 disables the dwell). |
| `tui.brians_brain` | true | false keeps the home banner static. |
| `tui.atmosphere` | true | false disables fireflies and dandelions. |
| `tui.scroll_step` | 3 | Terminal scroll step, in lines. |
| `tui.page_step` | 15 | Terminal page step, in lines. |
