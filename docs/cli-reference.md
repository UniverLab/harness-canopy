---
title: CLI Reference
description: Every canopy command and flag.
order: 12
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

## Updates

| Command | Description |
|---|---|
| `canopy update` | Check the latest stable release, ask `Update to <tag>? [y/N]` (default no), and install it atomically; prints `canopy <current> → <latest>` or `canopy <current> is up to date` |
| `canopy update --check` | Print the update status without changing anything; exits 1 when an update exists and 0 when up to date |
| `canopy update --yes` | Install without the confirmation prompt |

A cargo-installed binary is never overwritten: `canopy update` prints
`installed with cargo — run: cargo install harness-canopy --force`.
After a successful replacement, a running systemd-managed daemon is restarted
with `systemctl --user restart canopy.service`; an unmanaged daemon uses the
normal `canopy daemon stop/start` path. If a graph is running, update asks
before restarting and otherwise leaves the daemon untouched and prints the
command to run later.

## Setup & diagnostics

| Command | Description |
|---|---|
| `canopy setup` | Interactive setup wizard — detects AI CLIs, generates `~/.canopy/config.toml` |
| `canopy setup --local-registry <path>` | Use a local registry directory (for registry development) |
| `canopy mcp` | MCP wizard — sync/add/remove canopy's MCP entry across platforms |
| `canopy mcp --local-registry <path>` | Run the MCP wizard against a local registry directory instead of GitHub (same flag as `canopy setup --local-registry`) |
| `canopy mcp` → Add | Add a server via pasted README JSON, local stdio command (with args + env), or remote URL (with headers); previews entries, marks `(replaces existing)` overwrites, then asks `Install on N platform(s)?` |
| `canopy uninstall --dry-run` | Preview removal: stop daemon, remove service unit + `canopy.service.d/` drop-ins, revert platform configs, remove skill symlinks (changes nothing) |
| `canopy uninstall --keep-shared-servers` | Only remove canopy's own `canopy` server entry; leave shared `fetch`/`filesystem` entries in place |
| `canopy doctor` | Full health diagnostics |

## Graph inspection

| Command | Description |
|---|---|
| `canopy graph list` | List all graphs with status and spec progress |
| `canopy graph list --workdir <path>` | Filter graphs by workdir |
| `canopy graph info <id-or-name>` | Detailed status for a single graph (specs, runs, hooks) |

Graph ids can be specified by exact id, exact name, or unambiguous id
prefix. Ambiguous references list the candidates.

## Graph control

Mirrors the `graph_run`/`graph_pause`/`graph_continue`/`graph_reset`/
`graph_schedule_autorun` MCP tools — a second way to reach the same daemon
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
| `canopy graph pause <id-or-name>` | Pause a running graph after the current node finishes |
| `canopy graph continue <id-or-name> --retry-current-node` | Resume a paused graph by retrying the current node |
| `canopy graph continue <id-or-name> --skip-next-spec` | Resume a paused graph by skipping to the next spec |
| `canopy graph reset <id-or-name>` | Reset a completed/failed graph back to pending (prompts for confirmation) |
| `canopy graph reset <id-or-name> --specs <id>...` | Reset specific spec ids, even if already completed |
| `canopy graph reset <id-or-name> --yes` | Skip the confirmation prompt |
| `canopy graph autorun <id-or-name> --at <iso8601>` | Schedule a one-shot resume at a future time |
| `canopy graph autorun <id-or-name> --quota-reset-message <text>` | Schedule a resume from a raw CLI quota-limit message |
| `canopy graph autorun <id-or-name> --cancel` | Cancel a pending autorun schedule |

## Agents

Read-only inspection of registered agents, served from the local database
(like `canopy graph info` — no daemon required).

| Command | Description |
|---|---|
| `canopy agent show <id>` | Show one agent's full stored definition, prompt last and untruncated |
| `canopy agent show <id> --json` | Print the same definition as JSON (identical to the `agent_get` MCP tool's output) |

## Spec backlog

| Command | Description |
|---|---|
| `canopy spec create --name <name> --description <text>` | Create a standalone spec (a backlog item) |
| `canopy spec create ... --workdir <path>` | Tag the new spec with a workdir, for backlog filtering |
| `canopy spec create ... --queue <queue-id>` | Also append the new spec to an existing queue |
| `canopy spec create ... --queue <queue-id> --group <group>` | Add it to the queue within a context group |
| `canopy spec complete <spec-id> --reason <text>` | Mark a standalone spec as completed |
| `canopy spec skip <spec-id> --reason <text>` | Mark a standalone spec as skipped |
| `canopy spec reopen <spec-id> --reason <text>` | Reopen a completed/skipped spec back to pending |
| `canopy spec convert` | Migrate legacy heading-format spec bodies to the tagged `<spec>` format (skips tagged and running specs; prints a report) |

## RAG

| Command | Description |
|---|---|
| `canopy rag auto-index start` | Resume automatic indexing (default state) |
| `canopy rag auto-index stop` | Pause indexing without losing the queue |
| `canopy rag report` | Detailed per-file indexing report |

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
