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
- `↑↓` selects, `Enter` confirms, and `Esc` returns to the form with every
  value kept. **Background** and **Terminal** launch from the form as
  before — the Mode step is Interactive-only.

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

## Monitoring

- **Agent status colors** — teal for working (recent output), blue for
  idle, red for failed, gray for exited. The sidebar shows a colored
  gutter bar per agent.
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
  elapsed time, iteration).
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
- **Crews face** — `F6` pins a panel face; **crews** is pin-only (never
  selected automatically). It lists every harness/model pair in use with its
  effective availability — `●` available, `○` unavailable, `◐` expired — its
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
  kaomoji status messages (whimsg).

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
visible in the TUI's gamification panel.

Press **F1** to open the **Canopy hub**, a full-screen tabbed overlay:
**Settings** edits every registry setting in place (with the value's apply
mode and status), **Keys** is a grouped reference of every shortcut the TUI
handles, and **Missions & stats** lists the missions above. `Tab`/`1`/`2`/`3`
switch tabs; `F1` or `Esc` closes it.

In **Settings**, `Space` flips a bool, `←`/`→` cycle an enum, `Enter` edits a
number, duration, string or format, and `r` resets the selected setting to its
default. A path list edits with `a` (add a folder), `d` (delete) and
`Shift+↑`/`Shift+↓` (reorder). `/` filters every group by key or description.
Each change goes through the daemon; a refused value keeps the old one and the
daemon's message shows in the status line.

## Projects sidebar

The sidebar shows sections for the selected project:

- **Graphs** — active graphs with spec progress (done/total) and current
  spec indicator. Selecting a graph opens the live graph view.
- **Backlog** — standalone specs tagged to the project's workdir, not
  yet assigned to any graph.
- **History** — completed and failed graphs for the project.

Sections are automatically filtered by the selected project's workdir.

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
  preset selection.
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
