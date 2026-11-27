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
| Config | `config_changed` (a node, ensemble or hook edit made while the graph runs), `ensemble_changed` (payload: `old_kind`, `new_kind`, `old_crew`, `new_crew`) |
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

## Pending pauses

Pausing is graceful and visible: a pause requested while a node runs is
recorded at once as a `pause_requested` event naming the node(s) it waits
for, and the graph reaches `paused` (with `graph_paused`) at the next node
boundary. Meanwhile `graph` `get`/`list` still report status `running`,
with `pause_pending: true` and the waited-for names in `pause_waiting_for`;
the TUI shows `pausing — waiting for <node>` instead of the running
indicator. Requesting pause again while one is pending is a no-op answering
`pause already pending`, and `continue`/`run` while one is pending cancels
it — one `pause_cancelled` event, then the graph keeps running.

## Cron triggers and trigger prompts

A graph with a cron trigger can run on its own on every tick with no
specs bound: `graph_edit` `create`/`update` accept `trigger_prompt`
alongside a cron `trigger`, and each tick launches the graph with the
prompt as its idea (`{{spec_content}}` renders it). The prompt stands in
for the idea everywhere: `graph_control` `run` (`canopy graph run`) on
that graph needs no `idea` and no `queue_id` either. On a graph that
also has bound, non-blank specs the prompt is ignored — those specs run
as they always have.

`trigger_prompt` is part of the trigger, not of the graph document:
setting it on a manual or watch graph is refused (`set a cron trigger,
or clear trigger_prompt`), and so is removing or changing the cron
trigger of a graph that keeps one. `graph` `export` never carries it and
`graph_edit` `import` never sets it. `graph` `get`/`list` report it as
`trigger_prompt`. A tick never starts a second run while the previous
one is still running — it logs `tick skipped: previous run still
running` and moves on.

The intended shape is check-then-agent: a `check` node whose exit code 0
means "news found" routes `pass` to the `agent` node and has no `fail`
edge, while any other outcome means "nothing to do". On a cron tick of a
spec-less graph only, a `check` node that ends the run with no outgoing
`fail` edge, no `fail` edge having routed earlier in that run and no
agent having run, completes it as `completed` — with `graph_completed`
payload `{"outcome": "idle"}` — instead of failing it: no failure hook
fires and the attempt budget is untouched. A non-zero exit, a timeout
and an unfillable `{{…}}` marker in the command all count as that quiet
check failure. A tick run that reaches and runs its agent node records
`{"outcome": "worked"}` when it then completes. Every other failure — a
failing agent, a `fail` edge leading to a failing node, an infrastructure
error, a check dead end after an agent already ran — fails exactly as
before, as does the same graph run manually via `graph_control` (only a
tick run can be idle).

Copy-paste example over MCP (a per-minute inbox sweep; the check exits
non-zero when there is nothing to do):

```json
{ "tool": "graph_edit", "action": "create",
  "name": "inbox-sweep", "workdir": "/srv/inbox",
  "trigger": { "kind": "cron", "schedule": "* * * * *" },
  "trigger_prompt": "Fetch new messages and hand the new ones to the agent." }
{ "tool": "graph_node", "action": "add",
  "graph_id": "<graph_id>", "name": "sweep", "kind": "check",
  "config": { "command": "fetch-new-messages", "success_condition": "exit_code_0" } }
{ "tool": "graph_node", "action": "add",
  "graph_id": "<graph_id>", "name": "worker", "kind": "agent",
  "config": { "platform": "opencode", "prompt_template": "{{spec_content}}" } }
{ "tool": "graph_edge", "action": "add",
  "graph_id": "<graph_id>",
  "from_node": "<sweep node id>", "to_node": "<worker node id>",
  "condition": "pass" }
```

Each `graph_node` `add` answers with the new `node_id`; edge endpoints
take a node id (or an unambiguous prefix), not a name. Note there is
deliberately no `fail` edge out of `sweep`: that is what makes a quiet
tick complete idle instead of failing the graph.

## One-shot scheduled runs

To run a graph once at a specific future time rather than on a repeating
cron schedule, use `graph_control` `schedule_run` with `graph_id` and an
RFC 3339 timestamp `at` (must be in the future). Optional parameters:
`idea` or `queue_id` (mutually exclusive), `workdir`, and `on_miss`
(`"skip"` by default, or `"run"`).

`CronScheduler` wakes at the earliest `pending` `fire_at`. When `fire_at`
is reached:

- Within 60 seconds of `fire_at` (or with `on_miss: "run"`), the engine
  launches the graph with the stored `idea`, `queue_id`, and `workdir`,
  and marks the scheduled run `fired` (or `error` if the graph is
  missing, blocked by status, or already has an in-flight run).
- More than 60 seconds past `fire_at` (for example, while the daemon was
  offline) with `on_miss: "skip"`, the entry is marked `missed` without
  launching.

Inspect, cancel, or manually launch scheduled runs with `graph_control`:

- `list_scheduled_runs` (optional `graph_id`, `status`) — lists one-shot
  scheduled runs ordered by `fire_at` ascending.
- `cancel_scheduled_run` (`id`) — cancels a `pending` or `missed` entry
  (refuses already `fired` entries).
- `run_scheduled_run` (`id`) — launches a `missed` or `pending` entry
  immediately and marks it `fired`.

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
| `graphs.ensemble_max_members_sequential` | 16 (range 2–32) | Largest member count for a `cascade`/`round_robin`/`random` ensemble, and the largest crew. |
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
