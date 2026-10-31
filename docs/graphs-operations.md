---
title: Graph Operations
description: Archive and restore, node run history, the event log, and engine tuning.
order: 15
---

# Graph Operations

## Archive and restore

Graphs can be archived instead of deleted — they leave the main
`graph` `list`/sidebar view but their row, specs, and full run history
are untouched and can be restored at any time. The surface is MCP:
`graph_edit` `archive` / `restore` (`graph_id`) — there is no `canopy graph
archive` or `canopy graph restore` subcommand.
Archiving refuses a `running` graph — pause it first. The archived graph
is still reachable directly by id via `graph` `get` regardless of
archived state. Restoring is a plain flag flip — no data is lost or
moved.

## Node run history

When a graph fails, the node run history lets you diagnose exactly what
happened:

The CLI prints the most recent node runs in `canopy graph info <graph>`.
Over MCP: `graph` `node_runs` (`graph_id`, `spec_id?`, `node_id?`, `limit?`,
`offset?`, `compact?`) returns the most recent runs first (default 20,
capped at 200); `offset` pages back through older runs and `compact` trims
each run to its summary fields.
`graph` `node_run` (`run_id`) fetches one run's full stored input and
output, including `infra_attempt`/`infra_crash` markers when present, and
`ended_after_verdict`/`session_end` when an agent filed a verdict.
Secret-shaped substrings are redacted before the output crosses the
boundary.

This is the second step of failure diagnosis: list to find the
offending run, then fetch its output here — the exact stderr/stdout/
reported_output the engine recorded.

## Event log

Every graph keeps its own durable, structured history in the `graph_events`
table — the engine writes one row per event as the run happens:

| Group | Kinds |
|---|---|
| Graph | `graph_started`, `graph_paused`, `graph_resumed`, `graph_completed`, `graph_failed`, `graph_blocked`, `pause_requested`, `pause_cancelled` |
| Spec | `spec_started` (payload: `spec_start_head`, `spec_start_dirty`), `spec_completed`, `spec_failed`, `spec_skipped` |
| Node | `node_started`, `node_finished` (payload: `verdict`, `platform`, `model`, `duration_s`) |
| Mutex | `mutex_waiting` (payload: `resources`, `waiting_for`), `mutex_acquired` (payload: `resources`) |
| Hooks | `hook_fired` (payload: `event`, `mode`, `ok`, `error`) |
| Config | `config_changed` (a node, ensemble or hook edit made while the graph runs) |
| Notes | `note` (reserved for the CM47 spec-note mechanism; payload: `owner_name`, `chars`) |

Read it from the CLI:

```
canopy graph log <graph-id-or-name> [--since <id|RFC3339>] [--kind <k>] [--spec <id>] [--follow]
```

Each line is `<created_at> <KIND> spec=<name> node=<name> <message>`.
`--follow` polls the daemon every 2 s and prints new events until Ctrl+C.
Over MCP, `graph` `events` (`graph_id`, `since_id?`, `since_ts?`, `spec_id?`, `kinds?`, `limit?`,
`include_payload?`) returns events oldest-first with `next_since_id` for
paging.

**This replaces the `printf … >> file` logging-hook pattern.** You no longer
need a command hook that appends to a per-graph log file on `on_completed` /
`on_failed` — the engine records that history itself, intact and queryable.
Ordinary logging hooks keep working; they simply stop being necessary.

Unlike the shared activity stream (bounded per workdir, free text), this log
is never pruned by count. `canopy clean` removes events older than
`graph_event_retention_days` (default 90) for graphs that are not running.

## Tuning the engine

These `[graphs]` settings (see `canopy config list`) adjust the engine's
retry budgets and caps. Each keeps its default until you set it.

| Setting | Default | What it does |
|---|---|---|
| `graphs.max_iterations_per_node` | 5 | Node visits a spec may make before it is blocked (the iteration ceiling). |
| `graphs.infra_retry_limit` | 2 | Infra-crash retries before the infra failure falls through the error edge. |
| `graphs.infra_crash_max_seconds` | 60 | Longest an infra crash may last before the retry budget is exhausted. |
| `graphs.infra_backoff_seconds` | 30 | Backoff between infra retries. |
| `graphs.straggler_timeout_minutes` | 30 | Default timeout (1–480) for a **new** ensemble; existing ensembles keep theirs. |
| `graphs.ensemble_max_members_parallel` | 8 (range 2–16) | Largest member count a `parallel` ensemble may have. Lowering it never edits an existing ensemble. |
| `graphs.ensemble_max_members_sequential` | 16 (range 2–32) | Largest member count for a `cascade`/`round_robin` ensemble, and the largest crew. |
| `graphs.check_output_max_kib` | 64 (range 16–4096) | Check-output tail kept, in KiB. |
| `graphs.verdict_grace_seconds` | 15 (range 0–300) | Seconds an agent has to exit after filing its verdict before canopy terminates it. 0 disables the shutdown. |
| `graphs.catalogue_url` | `https://graphs.univerlab.org` | Base URL of the catalogue `canopy graph install` and `canopy node install` fetch from. |

Two more engine settings sit outside the `graphs.` prefix (see
[CLI reference](cli-reference.md#settings-reference)):
`spec_attempt_limit` (default 3) caps how many separate executions of a spec
may fail with a verdict before it is blocked, and `ensemble_concurrency_cap`
(default 4) caps how many ensemble members run at once across every graph.

The three `graphs.infra_*` settings resolve with this precedence, strongest
first: **member > ensemble > the platform's `clis[]` entry > the global
setting**. Nothing else supplies a value.

The member caps (`graphs.ensemble_max_members_parallel` /
`graphs.ensemble_max_members_sequential`) bound **new** writes only:
lowering a cap below an existing ensemble's member count never edits that
ensemble, and an over-cap create is refused with the setting key named.
