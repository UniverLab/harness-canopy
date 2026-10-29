---
title: CLI Reference
description: Every canopy command and flag.
order: 18
---

# CLI Reference

```
canopy [command] [options]
```

Running `canopy` with no command opens the [TUI](tui.md) (running setup
first if needed, and checking for updates (notice only — run `canopy update`
to install)).

## Daemon

| Command | Description |
|---|---|
| `canopy daemon start` | Start the daemon (MCP server, scheduler, watchers) |
| `canopy daemon stop` | Stop the daemon |
| `canopy daemon status` | Show daemon status |
| `canopy daemon restart` | Restart the daemon |
| `canopy daemon logs` | Show the daemon log |
| `canopy daemon install-service` | Register as a system service |
| `canopy daemon uninstall-service` | Remove the system service |
| `canopy daemon health-check` | Run the daily database integrity check and backup now, instead of waiting for the daemon to find an idle moment (see [Installation](installation.md#health-check)) |

## Updates

| Command | Description |
|---|---|
| `canopy update` | Check the latest stable release, ask `Update to <tag>? [y/N]` (default no), and install it atomically; prints `canopy <current> → <latest>` or `canopy <current> is up to date` |
| `canopy update --check` | Print the update status without changing anything; exits 0 when up to date, 1 when an update exists, and 2 when the check could not complete (network, DNS, TLS, HTTP error, or unparsable response — one stderr line names the cause) |
| `canopy update --yes` | Install without the confirmation prompt |

A cargo-installed binary is never overwritten: `canopy update` prints
`installed with cargo — run: cargo install harness-canopy --force`.
After a successful replacement, a running systemd-managed daemon is restarted
with `systemctl --user restart canopy.service`; an unmanaged daemon uses the
normal `canopy daemon stop/start` path. If a graph is running, update asks
before restarting and otherwise leaves the daemon untouched and prints the
command to run later.

Plain `canopy update` exits 2 on the same lookup errors.

## Setup & diagnostics

| Command | Description |
|---|---|
| `canopy setup` | Interactive setup wizard — detects AI CLIs, generates `~/.canopy/config.toml` |
| `canopy setup --local-registry <path>` | Use a local registry directory (for registry development) |
| `canopy setup --force-skills` | Overwrite local skill files that diverge from the sync source, even when they were edited locally (default: skip them with a warning) |
| `canopy mcp` | MCP wizard — sync/add/remove canopy's MCP entry across platforms |
| `canopy mcp --local-registry <path>` | Run the MCP wizard against a local registry directory instead of GitHub (same flag as `canopy setup --local-registry`) |
| `canopy mcp` → Add | Add a server via pasted README JSON, local stdio command (with args + env), or remote URL (with headers); previews entries, marks `(replaces existing)` overwrites, then asks `Install on N platform(s)?` |
| `canopy uninstall --dry-run` | Preview removal: stop daemon, remove service unit + `canopy.service.d/` drop-ins, revert platform configs, remove skill symlinks (changes nothing) |
| `canopy uninstall --keep-shared-servers` | Only remove canopy's own `canopy` server entry; leave shared `fetch`/`filesystem` entries in place |
| `canopy doctor` | Full health diagnostics |

## Platform registry

| Command | Description |
|---|---|
| `canopy registry refresh` | Fetch the platform registry and apply it now, ignoring the 24-hour interval the TUI's background refresh uses; prints one line per affected platform (`<platform>: changed <fields>`, `<platform>: kept local override <fields>`, `<platform>: added`, `<platform>: updated`, `<platform>: removed`), or `registry already up to date` when nothing changed. Rewrites only `config.toml` and the baseline file — never starts the TUI, wizard or daemon, safe while graphs run |
| `canopy registry refresh --json` | Print the same report as JSON (`changed`, `kept`, `added`, `updated`, `deleted`, `up_to_date`) |
| `canopy registry refresh --local-registry <path>` | Apply a local registry directory instead of fetching from GitHub (same flag as `canopy setup --local-registry`) |

Registry-owned CLI fields — binary path, model flag, headless mode,
environment variables — are rewritten from the registry. A field you edited
yourself is kept as it is; when the registry changed that same field, the
refresh reports it as a local override, so a refresh never overwrites your
choice. The command needs an existing `~/.canopy/config.toml` — on a machine
that has not run setup it fails with ``run `canopy setup` first``. The refresh
time is recorded where the TUI's background refresh records it, so a manual
refresh also restarts that 24-hour window.

## Graph inspection

| Command | Description |
|---|---|
| `canopy graph list` | List all graphs with status and spec progress |
| `canopy graph list --workdir <path>` | Filter graphs by workdir |
| `canopy graph info <id-or-name>` | Detailed status for a single graph (specs, runs, hooks) |
| `canopy graph crew [id-or-name]` | List harness/model assignments — agent nodes outside ensembles, ensemble members and agent hooks, each with platform, model, timeout, crew and availability. Omit the graph for every graph |
| `canopy graph crew --platform <p> --model <m>` | Filter by exact platform and case-insensitive model substring |
| `canopy graph crew --all` | Include archived graphs |
| `canopy graph crew --group-by-pair` | Summarise per platform/model pair: graph, ensemble, node and hook counts plus the graph names |
| `canopy graph crew --json` | Print the MCP `crew` `assignments` JSON shape instead of tables |
| `canopy graph validate <path>` | Check a shared graph or node JSON document without importing it: every structural check `import` runs plus the sharing rules. Prints each problem with its JSON path; exits 0 when the document is valid, 1 when it is not |
| `canopy graph validate <path> --json` | Print the machine-readable report instead: `ok` with the kind, name and counts, or `problems` |
| `canopy graph install <ref>` | Install a graph from the catalogue in one command: `<slug>` or `<owner>/<repo>/<slug>`, each with an optional `@<ref>` suffix, or `draft:<id>` for an unreviewed editor draft ([graphs.univerlab.org/editor](https://graphs.univerlab.org/editor), 30 minutes, installs once). Prints a review of every shell command and agent prompt the document runs (with attention marks), asks `Install <name> from <source>? [y/N]`, then — after confirmation — validates the fetched document, installs its bundled presets, and imports it as a new graph |
| `canopy graph install <ref> [--workdir <dir>] [--name <name>] [--overwrite-presets] [--yes] [--show-prompts] [--json]` | Install into another workdir (default: the current directory), under another name, overwriting differing local preset files instead of refusing, skipping the confirmation question (`--yes`, after printing the review), printing every prompt in full (`--show-prompts`), or printing the review and report as machine-readable JSON |

`canopy graph validate` runs entirely locally — no daemon, no database,
no workdir — so it works before anything is installed. The rules it
enforces are in [Graphs](graphs-sharing.md#validate-before-you-share).

Graph ids can be specified by exact id, exact name, or unambiguous id
prefix. Ambiguous references list the candidates.

## Nodes

| Command | Description |
|---|---|
| `canopy node list [--json]` | List installed node documents and the builtins embedded in the binary, marking each `installed` or `builtin` |
| `canopy node install <ref>` | Install a node document from the catalogue: `<slug>` or `<owner>/<repo>/<slug>`, each with an optional `@<ref>` suffix, or `draft:<id>` for an unreviewed editor draft. Prints the same review as `graph install` and asks for confirmation first; after confirmation it validates the document, installs its bundled preset, and writes it to `~/.canopy/nodes` |
| `canopy node install <ref> [--overwrite-presets] [--yes] [--show-prompts] [--json]` | Overwrite differing local preset files instead of refusing, skip the confirmation question (`--yes`, after printing the review), print every prompt in full (`--show-prompts`), or print the review and report as machine-readable JSON |
| `canopy node remove <name> [--json]` | Remove an installed node document. A builtin with no installed copy cannot be removed; an installed override can |

`canopy node` runs entirely locally — no daemon needed. See
[Graphs](graphs-nodes.md#nodes).

## Graph control

Mirrors the `graph_control` actions `run`/`pause`/`continue`/`reset`/
`schedule_autorun` — a second way to reach the same daemon
operations when an MCP client can't. Every command resolves `<id-or-name>`
locally (same rule as graph inspection above) and then delegates the actual
state change to the daemon over its existing MCP endpoint; it never writes
to the database directly. If the daemon isn't reachable on the resolved
port, the error says so explicitly instead of looking like the graph is
missing.

| Command | Description |
|---|---|
| `canopy graph run <id-or-name>` | Run a graph in the background, spec by spec |
| `canopy graph run <id-or-name> --queue <queue-id>` | Run the queue's pending specs through the graph instead |
| `canopy graph run <id-or-name> --workdir <path>` | Override the graph's workdir for this run only |
| `canopy graph run <id-or-name> --from <node>` | Start the first spec this run executes at a chosen node (id, id prefix, node name, or ensemble name), reusing the recorded outputs of upstream nodes; refused, naming the node, if an upstream node's latest run failed, errored or was interrupted |
| `canopy graph run <id-or-name> --idea <text>` | Run a spec-less idea graph: the text is fed to nodes as `{{spec_content}}` (mutually exclusive with `--queue`; the graph must have no bound specs and no queue) |
| `canopy graph pause <id-or-name>` | Pause a running graph after the current node finishes |
| `canopy graph continue <id-or-name> --retry-current-node` | Resume a paused or failed graph by retrying the current node (also resumes a paused graph whose current spec is pending with a recorded node; clears that spec's execution count and the reply says so). On an idea-driven graph (one run with `--idea`), it resumes that idea run; an idea graph that has never run is refused, naming `graph run --idea` as the way to start one |
| `canopy graph continue <id-or-name> --skip-next-spec` | Resume a paused or failed graph by skipping to the next spec |
| `canopy graph continue <id-or-name> --from <node> [--spec <spec>]` | Start a spec at a chosen node, reusing the recorded outputs of upstream nodes (refused, naming the node, if one failed, errored or was interrupted; defaults to the graph's current spec: a running/interrupted member, else a failed one, else the next pending one). On an idea-driven graph it resumes that idea run with the stored idea as `{{spec_content}}` and no `--spec` to give |
| `canopy graph reset <id-or-name>` | Reset a completed/failed graph back to pending (prompts for confirmation) |
| `canopy graph reset <id-or-name> --specs <id>...` | Reset specific spec ids, even if already completed |
| `canopy graph reset <id-or-name> --yes` | Skip the confirmation prompt |
| `canopy graph reset <id-or-name> --clear-notes` | Also delete the reset specs' spec notes (default keeps them) |
| `canopy graph remove-spec <id-or-name> <spec-id>` | Unbind a spec from the graph, making it a standalone backlog spec; its execution history is kept |
| `canopy graph autorun <id-or-name> --at <iso8601>` | Schedule a one-shot resume at a future time |
| `canopy graph autorun <id-or-name> --quota-reset-message <text>` | Schedule a resume from a raw CLI quota-limit message |
| `canopy graph autorun <id-or-name> --cancel` | Cancel a pending autorun schedule |
| `canopy graph log <id-or-name>` | Print the graph's durable event log, one line per event |
| `canopy graph log <id-or-name> --since <id\|RFC3339>` | Start from an event id or a timestamp |
| `canopy graph log <id-or-name> --kind <k>` | Only events of this kind (repeatable) |
| `canopy graph log <id-or-name> --spec <id>` | Only this spec's events |
| `canopy graph log <id-or-name> --follow` | Poll every 2 s and print new events until Ctrl+C |

`canopy graph log` prints `<created_at> <KIND> spec=<name> node=<name> <message>`
and goes through the daemon like the other `graph` subcommands. See the
[Event log](graphs-operations.md#event-log) section.

A `continue` on a sandboxed graph resumes inside that run's sandbox worktree;
when the worktree is gone the continue is refused, naming the sandbox id,
rather than running the graph against the original workdir. A failed
sandboxed graph always keeps its worktree — see
[Sandboxes](graphs-isolation.md#sandboxes).

## Agents

Read-only inspection of registered agents, served from the local database
(like `canopy graph info` — no daemon required).

| Command | Description |
|---|---|
| `canopy agent show <id>` | Show one agent's full stored definition, prompt last and untruncated |
| `canopy agent show <id> --json` | Print the same definition as JSON (identical to the `agent` `get` action's output) |

## Missions

Read-only listing of the 28 gamification missions (see [TUI](tui.md#gamification)),
served from the local database — no daemon required.

| Command | Description |
|---|---|
| `canopy missions` | List every mission: icon, title, category, `unlocked <date>` or `locked: <challenge>`, plus `N / 28 unlocked` |
| `canopy missions --json` | Print the same report as one JSON object (`unlocked`, `total`, `missions` with `id`, `title`, `icon`, `category`, `challenge`, `unlocked_at` as RFC 3339 UTC or null) |

## Spec backlog

| Command | Description |
|---|---|
| `canopy spec create --name <name> --description <text>` | Create a standalone spec (a backlog item) |
| `canopy spec create ... --workdir <path>` | Tag the new spec with a workdir, for backlog filtering |
| `canopy spec create ... --spec-type <type>` | Set the Conventional Commits type (`feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`). Optional — when omitted and the name is a conventional title, the type and scope are parsed from the name |
| `canopy spec create ... --spec-scope <scope>` | Set the optional scope (e.g. `tui`). Part of the deterministic commit message |
| `canopy spec create ... --queue <queue-id>` | Also append the new spec to an existing queue |
| `canopy spec create ... --queue <queue-id> --group <group>` | Add it to the queue within a context group |
| `canopy spec complete <spec-id> --reason <text>` | Mark a standalone spec as completed |
| `canopy spec skip <spec-id> --reason <text>` | Mark a standalone spec as skipped |
| `canopy spec reopen <spec-id> --reason <text>` | Reopen a completed/skipped spec back to pending |
| `canopy spec notes <spec-id>` | Print every spec note of a spec, newest first, with its owner, last update and body (see [Spec notes](graphs-execution.md#spec-notes)) |

## Availability roster

| Command | Description |
|---|---|
| `canopy roster list [--platform <platform>]` | List roster entries with their effective state now |
| `canopy roster set <platform> [<model_pattern>] --unavailable [--reason <text>] [--until <rfc3339>]` | Declare a platform/model pair unavailable; `until` must be a future RFC 3339 UTC instant |
| `canopy roster set <platform> [<model_pattern>] --available` | Declare it available |
| `canopy roster clear <platform> [<model_pattern>]` | Delete one entry (idempotent) |
| `canopy roster check <platform> [<model>]` | Show the effective availability of one pair |

Omitting `<model_pattern>` addresses every model of the platform; a pattern
ending in `*` (e.g. `opencode-go/*`) addresses a prefix. The engine skips
effectively-unavailable members at dispatch and blocks a single unavailable
agent node or hook — it never edits a graph.

## Named mutexes

| Command | Description |
|---|---|
| `canopy mutex list [--json]` | List granted mutex leases and queued waiters (FIFO); `--json` prints the raw MCP shape |

## Crews

| Command | Description |
|---|---|
| `canopy crew list` | List every crew with its version, members and binding counts |
| `canopy crew show <name>` | Show one crew and every binding of it |
| `canopy crew create <name> --from-ensemble <id> [--description <text>]` | Create a crew seeded from an ensemble's current members |
| `canopy crew delete <name>` | Delete a crew (refused while any ensemble, node or hook binds it) |

A crew is a named, ordered member list ensembles and agent nodes/hooks bind
to; member editing is MCP/TUI only (see
[MCP tools](mcp-tools.md)).

## Shelf

| Command | Description |
|---|---|
| `canopy shelf list [--json]` | List every shelf file (the host shelf plus every session's), newest first, through the daemon; `--json` prints the items with their file ids |
| `canopy shelf open <file_id>` | Open one shelf file with the system viewer, via the daemon |

Both subcommands go through the running daemon over the session-protocol
WebSocket — the CLI never writes the shelf itself. `file_id` is the
16-lowercase-hex id printed by `canopy shelf list --json`.

## Remote

| Command | Description |
|---|---|
| `canopy remote login` | Sign in to GitHub with the OAuth device flow (identity only, no scopes) |
| `canopy remote logout` | Delete the stored GitHub identity |
| `canopy remote status [--json]` | Show the sign-in state, whether remote is enabled, and the relay URL; `--json` prints `{signed_in, login, enabled, relay_url}` |
| `canopy remote on` / `off` | Turn the relay connection on or off (`[remote] enabled`); the daemon picks the change up without a restart |
| `canopy remote pair` | Create a one-time code, print it with its `<relay_url>/pair#<code>` link and expiry, and draw the link as a terminal QR code |
| `canopy remote devices [--json]` | List paired browsers (`id`, `label`, `paired_at`, `last_seen`) |
| `canopy remote revoke <device_id>` | Delete a paired browser and close its live connection at once |
| `canopy remote disconnect <device_id>` | Close a paired browser's live connection at once, keeping the pairing so it may reconnect |

The same actions live in the TUI: `F1`, then `5` opens the hub's **Remote**
tab — GitHub sign-in with the device flow, the relay switch and phase,
pairing with a QR overlay, and disconnecting and revoking devices ([TUI](tui.md)).

### Pairing a browser

Pairing turns a browser into a revocable device of this host. The code is
ten Crockford base32 characters shown as `XXXXX-XXXXX`, works once for ten
minutes, and is burned after one success or five failed code checks. One
code is live per host: `canopy remote pair` replaces the previous one, and
the old code stops working at once. Case, spaces and dashes do not matter
when the code is typed, `I`/`L` are read as `1` and `O` as `0`.

`canopy remote pair` prints the code, the link and the expiry time, then
the link as a terminal QR code:

```text
Pairing code: 7K3QH-M9P2X
Link: https://remote.univerlab.org/pair#7K3QH-M9P2X
Expires: 2026-10-28 15:04:05 UTC
<the link as a QR code, drawn with half blocks>
```

The browser opens that link and exchanges long-term P-256 public keys with
this host, authenticated by the code, so later connections need no code
and the relay never learns anything that lets it impersonate either side.
The exchange rides the daemon's relay connection, so pairing completes
only while the daemon is running, `remote status` shows a login, and
remote is on. A failed exchange tells the browser only `no_code`,
`expired` or `bad_code`: run `canopy remote pair` again for a fresh code.

`canopy remote devices [--json]` prints `id`, `label`, `paired_at` and
`last_seen` (`never` when the registry has no time), one line per device
or as a JSON array; `no paired devices` when there are none. `canopy
remote revoke <device_id>` deletes the device and closes its live
connection within a second, printing `revoked <device_id>`, and refuses
an unknown id with `no paired device with id <device_id>`. `canopy
remote disconnect <device_id>` closes the live connection the same way
but keeps the pairing, printing `disconnected <device_id>` and refusing
an unknown id the same way. Re-pairing the
same browser updates its label and `paired_at` instead of adding a row.

The host key lives in `~/.canopy/remote/host_key` (PKCS#8 DER, mode 0600)
and `host_id` is the first 16 hex chars of SHA-256 over its public key;
the live code and its derived key sit in `~/.canopy/remote/pairing.json`
(mode 0600, removed once the code is used, refused as expired, or burned —
deleting it cancels the code). Paired browsers are recorded in the
`remote_devices` table and identified by the first 16 hex chars of SHA-256
over their own key.

### The relay connection

With `remote on` and a stored identity, the daemon keeps one WebSocket to
`<relay_url>/ws/host`, carrying the GitHub token and this host's id and
label, and answers the pairing frames the relay forwards to it. A dropped
socket is retried after one second, doubling to one minute, with jitter.
`canopy remote off` closes the socket and `on` reconnects within a second
— neither needs a daemon restart. If the relay answers HTTP 401, the
daemon stops retrying that token and logs `github token rejected: run
canopy remote login` (see `canopy daemon logs`); a fresh `canopy remote
login` makes it connect again.

Remote control is owner-only: the relay admits only hosts signed in with
the same GitHub account. `login` prints the device URL and the code to
enter (`Open https://github.com/login/device and enter <user_code>`, plus
the full verification URL when GitHub returns one), then polls GitHub at
the interval it asks for — `slow_down` adds five seconds, an expired or
denied code stops with a one-line message — and ends with `Signed in to
GitHub as <login>.` It stores `{token, login, id}` in
`~/.canopy/remote/github.json` (mode 0600, directory 0700); the token never
appears in logs, errors, or status output. `logout` deletes that file and
succeeds when it is already gone. `status` prints `login: <login>` (or
`not signed in`), `enabled: …` and `relay_url: …`.

### A paired browser's connection

The relay only carries signaling. A paired browser sends a signed WebRTC
offer through it; the host verifies the signature against
`remote_devices` and refuses `unknown_device`, `stale` (over 120 seconds
off) or `bad_signature`, then answers with a signature of its own. Both
signatures cover the SDP, so the DTLS fingerprints are pinned end to end
and neither the relay nor a TURN server can read or impersonate the
session. Nothing but those signaling bodies reaches the relay.

Once the data channel is open, the browser speaks the same protocol v1 as
`/sessions/ws`, with four restrictions: terminal sessions are hidden from
the list and cannot be attached, written to, prompted or killed, `resize`
is always refused (the browser renders the host's size scaled), and `spawn`
creates `agent` sessions only. A `spawn` names a directory (`workdir`)
rather than a project. `workdir` may be absolute or start with `~/` (`~` and
`~/` alone mean the host user's home), and the expanded path must be an
existing directory; the browser's New dialog types that directory. A
relative path is refused with `spawn: field 'workdir' must be an
absolute path`, and a path that does not exist or is not a directory with
`spawn: workdir '<expanded>' does not exist or is not a directory`; both are
`error{code:"bad_request"}`. The `yolo` field (default `false`) launches the
harness with its approval prompts off — the switch the TUI's New dialog
offers — by appending that harness's registry `yolo_flag` after whatever
interactive args the frame sent; a harness with no `yolo_flag` refuses it
with `spawn: harness '<cli>' has no yolo mode`, and a `terminal` spawn with
`spawn: field 'yolo' is not allowed for kind 'terminal'`. Each `catalog` CLI
row carries `yolo: true|false`, so the browser only offers the switch where
the host can honor it. Output and snapshot frames are chunked to stay under
16 KiB; a snapshot's first part carries `cols`/`rows`, the
rest travels as `snapshot_more`, and the final `snapshot_more` carries
`last: true` (an empty one when the snapshot fit the first part). The shelf rides the same channel:
`shelf_list` and `shelf_get` answer `shelf` and `file` frames, and
`shelf_added` / `shelf_removed` push arrivals and removals. Files from
terminal sessions stay hidden from lists and pushes and answer
`error{code:"forbidden"}` on fetch; host-shelf files are visible. `file`
frames stay under 16 KiB, at most two downloads run at once — a third
answers `error{code:"busy"}` — and their parts interleave with terminal
output so a large image never stalls it. A host serves at most four device
connections at once — a fifth is told `busy` — each success refreshes the
device's `last_seen`, and `canopy remote revoke` (or `disconnect`, keeping
the pairing) closes a live connection within a second. A revoked device hears `error{code:"revoked"}` before its
channel closes, so its app can say the device was removed; answers larger
than the peer's negotiated frame cap are refused with
`error{code:"frame_too_large"}` instead of being sent halfway.

Graphs ride the same channel, read-only. `graphs{req}` answers
`graphs{req,graphs}` with the non-archived running, paused and failed
graphs plus completed graphs active within the last 24 hours — running
first, then paused, failed, completed, most recent activity first, at
most 50 — each row carrying `id`, `name`, `status`, `workdir` as a
basename (never the host's absolute path), `current_spec` (the current
spec's name), `current_node` (the node's *name*, never its id),
`done`/`total` over the graph's queue or specs, `blocked` and
`updated_at`:

```json
{"t": "graphs", "req": "g1"}
```

`graph_watch{req,id}` first answers `graph{req,id,document,state}` — the
graph's export document, exactly what `canopy graph export` writes, plus
its live `state` — and then pushes `graph_state{id,state}` only when
that state differs from the last one sent on the connection, re-read
every 2 seconds. `state` carries `status`, `blocked`/`blocker`,
`current_spec_id`, the `current_node` name, the `nodes` map of the
current spec's latest iteration (`"running"`, `"pass"` or `"fail"`, a
node with no run absent), the queue-ordered `specs` (each `id`, `code`
such as `FEAT-12` or null, `name`, `status`, `attempts`) and
`updated_at`:

```json
{"t": "graph_watch", "req": "w1", "id": "7c1e4b2a-9d3f-4a6e-8b5c-2f0d1e3a4b6c"}
```

`graph_unwatch{id}` stops that watch and answers nothing:

```json
{"t": "graph_unwatch", "id": "7c1e4b2a-9d3f-4a6e-8b5c-2f0d1e3a4b6c"}
```

A connection holds at most four watches — a fifth answers
`error{code:"busy"}` and starts nothing — an unknown or archived graph
id answers `error{code:"not_found"}` and starts no watch either, and
every watch ends with the connection — or as soon as its graph is
deleted, which stops the pushes and frees the slot for another watch.
None of these frames ever changes a graph, a spec or a queue: pausing
and continuing stay local.

The `[remote]` table in `~/.canopy/config.toml` carries `enabled` (default
`false`), `relay_url`, `github_client_id` (the public Client ID of
UniverLab's "Canopy Remote" OAuth App — no configuration needed), and
`host_label` (empty means the hostname). Overriding `github_client_id`
with an empty string makes `login` stop before any request:
`remote.github_client_id is not set in ~/.canopy/config.toml`.

## Settings

Every setting in `~/.canopy/config.toml` is declared once, in a typed
registry, and read and written through one surface the CLI, MCP and the TUI
all use.

| Command | Description |
|---|---|
| `canopy config list [--group <group>]` | List settings with their group, kind, default, effective value, where it came from (`default`, `file`, `env`), when it applies, and status (`ok`, `invalid: <reason>`, `pending_restart`, `unknown`) |
| `canopy config get <key>` | Show one setting by its dotted key (e.g. `clean.retention_days`) |
| `canopy config set <key> <value>` | Change one setting; validated first, and nothing is written if it is refused |
| `canopy config reset <key>` | Write the setting's default back |
| `canopy config path` | Print the path of `config.toml` |

Only the daemon writes `config.toml`, and it edits the file with `toml_edit`,
so hand-written comments, key order and unknown keys survive every change.
`canopy config set` reports what the user must do for the value to take
effect: `applied`, `applies on next run`, `restart the daemon`, or `restart
the TUI`. A hand edit is picked up by the daemon's file watcher without a
restart, and an invalid value keeps the previous effective value — it is
reported by `canopy config list` and as a `canopy doctor` warning rather than
being applied. `pending_restart` marks a restart-only setting whose value on
disk differs from the one the running daemon loaded, including a key the file
no longer holds, and a `config.toml` that will not parse at all is reported
as a single `config.toml` row (and doctor warning) instead of being silently
ignored. `skills.sources` (a list of tables) must still be edited by
hand.

## Spec queues

| Command | Description |
|---|---|
| `canopy queue group <queue> <spec> <group>` | Set the context group of an existing queue member; a spec joining a group with pending members moves next to them |
| `canopy queue group <queue> <spec> --clear` | Clear the member's group (it stays in place unless that splits its old group) |

## RAG

| Command | Description |
|---|---|
| `canopy rag auto-index start` | Resume automatic indexing (default state) |
| `canopy rag auto-index stop` | Pause indexing without losing the queue |
| `canopy rag report` | Detailed per-file indexing report, store footprint and health |
| `canopy rag compact` | Merge store fragments and prune old versions (daemon does it, or this process when the daemon is down) |
| `canopy rag backfill` | Index every configured file that is not in the vector store yet |
| `canopy rag purge [--yes]` | Delete the entire vector store and reset all RAG state; asks for confirmation unless `--yes` (`-y`) is given |
| `canopy rag model retry` | Clear a failed local embedding-model download so the daemon retries it in the background, without re-running setup |

## Sandboxes

Sandboxed graph runs (`graph_control` `run` with `sandbox: true`) leave their worktree
behind whenever it holds work; see [Sandboxes](graphs-isolation.md#sandboxes). These
commands — or the [`sandbox` MCP tool](mcp-tools.md), which shares their
implementation and refusals — are how you deal with them, and none of them
guesses.

| Command | Description |
|---|---|
| `canopy sandbox list` | Read-only: every sandbox worktree canopy created — id, path, graph/run that made it, branch, whether the branch holds commits that exist nowhere else, how far behind its base it is, and its status (`active`, `kept`, `cleaned`, `cleanup_failed`, `merged`, `discarded`). Directories under canopy's worktrees root with no sandbox row are listed as `untracked` and never touched |
| `canopy sandbox list --json` | The same list as JSON |
| `canopy sandbox land <id-or-prefix>` | Merge one sandbox's branch onto its base branch; refuses rather than forcing when the workdir is on another branch, has uncommitted changes, or the merge conflicts (naming the conflicting paths), and leaves the sandbox in place. On success it removes the worktree, deletes the branch and marks the sandbox `merged` |
| `canopy sandbox discard <id-or-prefix>` | Remove one sandbox's worktree and delete its branch; refuses when the branch holds commits that exist nowhere else (or when that is uncertain) |
| `canopy sandbox discard <id-or-prefix> --discard-unique-commits` | Explicit consent to delete commits that exist only in this sandbox, in that same call |

`canopy clean` also reclaims sandboxes it considers finished once their branch
holds no commits that exist nowhere else — it does not look at uncommitted
files. Land or discard a `kept` sandbox you care about before running it.

## Clean

`canopy clean` removes stale data. The default run is a soft cleanup:

| Command | Description |
|---|---|
| `canopy clean --dry-run` | Preview what would be removed; deletes and modifies nothing |
| `canopy clean` | Soft cleanup: sessions and files older than `clean.retention_days`, and graph event-log rows older than `graph_event_retention_days` for graphs that are not running |
| `canopy clean --older-than <days>` | Override the retention window for this run only |
| `canopy clean --hard` | Also delete orphaned projects (workdir missing) and their dependent rows; asks for confirmation unless `--yes` is given. Soft cleanup runs first |
| `canopy clean --yes` | Skip the confirmation prompt that `--hard` requires. A typo here can delete a real cascade |
| `canopy clean --no-reclaim` | Skip reclaiming freed database space (VACUUM and WAL checkpoint) even when this run deleted enough rows to warrant it |
| `canopy clean --stop-daemon` | Consent to stop a running daemon for a warranted reclaim, then restart it exactly as it was. Without it, an interactive terminal is prompted; a non-interactive run skips reclamation while the daemon is up |

Reclaim runs only when a run deletes at least `clean.reclaim_min_deleted_rows`
rows (default 50). It takes an exclusive lock and rewrites the whole database
file, which is why it can be skipped or deferred.

## Settings reference

Every key the registry declares, with its default and range. `canopy config
list` prints the same keys with the value this machine actually uses and where
it came from. **Apply** says when a change takes effect: `applied` (at once),
`applies on next run`, `restart the daemon` or `restart the TUI`.

| Key | Default | Range | Apply | Meaning |
|---|---|---|---|---|
| `mcp_filesystem_root` | home directory | — | next run | Root directory the MCP filesystem server exposes |
| `temperature_unit` | `celsius` | `celsius`, `fahrenheit` | restart the TUI | Temperature unit for the TUI's system widgets |
| `theme` | `classic` | `classic`, `modern` | applied | TUI colour theme: bordered or borderless |
| `pinned_panel_face` | `auto` | `auto`, `activity`, `knowledge`, `graph`, `crews` | applied | Pinned right-panel face; `auto` lets the panel switch on its own |
| `tui.datetime_format` | `%Y-%m-%d %H:%M` | chrono format | applied | Dates and times in the TUI (validated by formatting a fixed instant) |
| `tui.time_format` | `%H:%M` | chrono format | applied | Time-only labels in the TUI |
| `tui.panel_dwell_seconds` | 10 | 0–300 | applied | Seconds a panel event holds the right panel; 0 disables the dwell |
| `tui.brians_brain` | `true` | bool | applied | Home-banner animation |
| `tui.atmosphere` | `true` | bool | applied | Fireflies and dandelions |
| `tui.scroll_step` | 3 | 1–20 | applied | Lines per scroll event |
| `tui.page_step` | 15 | 5–100 | applied | Lines per page-up/down |
| `notifications.enabled` | `true` | bool | applied | Desktop notifications |
| `update.check_enabled` | `true` | bool | applied | Automatic update checks |
| `update.check_interval_hours` | 24 | 1–720 | applied | Hours between automatic update checks |
| `announcements_enabled` | `true` | bool | next run | Upgrade and tip announcements |
| `embeddings_model` | empty | — | restart the daemon | Embedding model used by the knowledge layer |
| `embeddings_idle_unload_secs` | 600 | 1–86400 s | restart the daemon | Idle time before the embedding model is unloaded |
| `similarity_threshold` | 0.25 | 0.0–1.0 | next run | Lexical-cohesion threshold for semantic chunk merging |
| `rag_personal_dirs` | `[]` | list of paths | next run | Personal directories indexed recursively |
| `rag_max_file_mb` | 10 | 1–ceiling | next run | Per-file indexing size cap, in MB |
| `rag_vector_cache_entries` | 64 | 1–4096 | restart the daemon | LanceDB index cache ceiling, in entries |
| `skills.sources` | the UniverLab skills repo | list of tables | restart the daemon | Git sources for the skill store, checked in order (edit by hand) |
| `skills.ttl_minutes` | 15 | 0–10080 | next run | Minutes a fetched skill is served before its source is re-checked |
| `models.catalog_ttl_minutes` | 1440 | 1–43200 | next run | Minutes the models.dev catalog cache is served |
| `models.native_ttl_minutes` | 60 | 1–43200 | next run | Minutes a platform's native model list is served |
| `agents.probe_timeout_seconds` | 30 | 5–300 s | applied | Timeout for a probe before the harness gives up |
| `scheduler.retry_enabled` | `true` | bool | next run | Retry a failed scheduled run after a delay |
| `scheduler.retry_delay_minutes` | 60 | 1–1440 | next run | Wait before a scheduled run is retried |
| `scheduler.retry_max` | 3 | 0–20 | next run | Retries of a failed scheduled run |
| `spec_attempt_limit` | 3 | 1–20 | restart the daemon | Separate graph executions a spec may fail with a verdict before it is blocked |
| `ensemble_concurrency_cap` | 4 | 1–64 | restart the daemon | Ensemble members that run at once across every graph |
| `graphs.*` | see [Tuning the engine](graphs-operations.md#tuning-the-engine) | see there | see there | Graph engine retry budgets, ensemble member caps, check output, and the graph catalogue URL |
| `clean.retention_days` | 7 | 1–3650 days | next run | Days kept before `canopy clean` removes sessions and files |
| `graph_event_retention_days` | 90 | 1–3650 days | next run | Days a graph's event log is kept before `canopy clean` prunes it |
| `clean.reclaim_min_deleted_rows` | 50 | 0–100000 | applied | Deleted rows that trigger VACUUM during `canopy clean` |
| `activity.retention_per_workdir` | 500 | 50–100000 | applied | Activity-log entries kept per workdir |
| `remote.enabled` | `false` | bool | applied | Whether this host accepts owner-only remote control via the relay |
| `remote.relay_url` | `https://remote.univerlab.org` | — | next run | Relay URL for owner-only remote control |
| `remote.github_client_id` | `Ov23liVCfE58kUb3ssDy` | — | next run | Public Client ID of UniverLab's "Canopy Remote" OAuth App (identity only; empty disables `remote login`) |
| `remote.host_label` | empty | — | next run | Label shown for this host; empty means the hostname |

`scheduler.retry_*` and `spec_attempt_limit` are not in `graphs.*`, which is
why they are listed here.

## Spec queues

## Plumbing

| Command | Description |
|---|---|
| `canopy stdio` | Run the MCP server over stdio |
| `canopy bridge --id <session>` | Stdio sidecar proxy that injects canopy identity headers |

## Global flags

| Flag | Description |
|---|---|
| `-p, --port <port>` | Override the daemon port (default 7755) |
| `--help` | Show help for any command |
| `--version` | Show canopy version |
