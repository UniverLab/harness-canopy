---
title: Hooks and Queues
description: Event-keyed hooks, the standalone spec backlog, and spec queues.
order: 12
---

# Hooks and Queues

## Event-keyed hooks

A graph carries hooks keyed by event, over four events: `on_completed`,
`on_failed`, `on_blocked`, `on_spec_completed`. More than one hook may be
registered for a single event, via `graph_edit` `update`'s `hooks` map (each key is
an event name, each value is an ordered array of hook configs). Hooks
registered for one event run in declaration order. There are three hook
modes — exactly one per hook:

- **Agent** (`platform` + `prompt`, optional `model`/`effort`/
  `timeout_minutes`): launches a CLI process with the rendered prompt.
- **Command** (`command`): runs a shell command directly with the same
  `{{...}}` placeholders substituted before execution.
- **Interactive** (`prompt` + `target_session_id` or `target_session_name`):
  enqueues a message into a live interactive session, exactly as if sent
  from the promptbuilder, marked as hook-originated. Fire-and-forget: the
  hook never reads or waits for a reply.

- `on_completed` fires exactly once when a run transitions to `completed`
  (only when the dispatch completed at least one spec). A completed →
  `graph_control` `reset` → completed cycle fires it again, once per completing run.
- `on_failed` fires when the graph reaches `failed`.
- `on_blocked` fires when it stops with a blocker set (transition to
  `paused` with blocker data).
- `on_spec_completed` fires once per spec reaching `completed`, whether the
  specs come from the graph's own bound specs or from a queue.

**Hooks are not retroactive** — a hook registered after its event has
already happened does not fire. A hook that fails is recorded and never
changes the graph's own status, and never stops the remaining hooks of that
event from running.

Editing hooks on a running graph takes effect for that run: the engine reads
the graph's stored hooks at the moment each event fires, so a `graph_edit` `update`
landing before an event fires is the edit that fires on the current run's
remaining events — the same way ensembles, nodes and prompts already take
effect at the next dispatch. When `graph_edit` `update` changes hooks while a run is
active, its response names those remaining events. An edit made after an
event already fired does not re-fire it.

Every hook run records the event it served and which hook of that event it
was (`event`, `hook_index`), plus a copy of the exact hook config that
actually fired (`hook_config`) — so a later reader can see what ran even
after the hooks are edited again. Hook runs are readable from `graph` `get`'s
`completion_hook_runs` and `canopy graph info`'s hook-runs listing alongside
the graph's node runs.

Hooks accept the complete marker vocabulary `{{graph_name}}`, `{{workdir}}`,
`{{completed_specs}}`, `{{spec_name}}`, `{{spec_id}}`, `{{blocker}}`, and
`{{node}}`. Event context determines which values are populated:
`on_completed` provides `{{completed_specs}}` (name + one-line summary of
each spec this run completed, one per line); `on_spec_completed` provides
`{{spec_name}}` and `{{spec_id}}`; and `on_failed`/`on_blocked` provide
`{{blocker}}` and `{{node}}` (the name of the node that ended the run).
Every known marker with no value for the event renders exactly `(none)` in
command, agent, interactive, and graph-idea hooks. Unknown marker names are
rejected by `graph_edit` `update` with the marker name and supported list; no hook
firing leaves a marker literal.

The existing configuration keeps working, unchanged and unattended:
whatever a graph has in its `on_completed` column becomes a hook on the
`on_completed` event with no user action and no loss, and a `graph_edit` `update`
call passing today's `on_completed` shape still registers that hook.

The first intended use is a documentation-maintenance agent: on
completion, review the specs this run closed, the resulting code, and
`docs/`/`README`, then update the docs to match what actually shipped.

An interactive hook targets a session either by its exact id
(`target_session_id`) or by its name (`target_session_name`) — exactly one
of the two must be set. Picking a finished session in the New Session Resume
step relaunches that same session — it keeps its id and its name even when
canopy never recorded the harness's own conversation id — so a hook
addressed to either keeps reaching it afterwards. Only when another live
session already holds the name does the resumed session take a fresh name
(keeping its id). A conversation picked out of the harness's own list that
canopy has never seen comes back as a new session with a fresh id and a
fresh generated name, and a hook configured with the old id then points at a
session that no longer exists: firing fails loudly naming the id rather than
redirecting the message anywhere else.
`target_session_name` exists for exactly this case: a name-targeted hook is
resolved against the live session set fresh on every fire — never cached —
the same set `session_list` reads. Exactly one live session with that name resolves and
delivers; zero matches fails naming the name, and more than one match fails
listing every matching id, rather than guessing which one was meant. Session
names are not required to be unique, so a hook aimed at a name that two
people are using at once will fail until only one of them is live.

Both targeting modes support the same event placeholders as the other hook
modes, rendered before enqueueing. The message carries its provenance —
graph id, event, and (for a name-targeted hook) the name it was aimed at —
as structure alongside the prompt, so the recipient, and the delivery
history, can tell both that it came from a hook and which session actually
received it, without that origin being buried in the prompt text.

A message is delivered by the daemon: the scheduler fires it on its next tick
whether or not a TUI is attached. A message whose target session is gone by
then is not lost either — the prompt is preserved for recall and a
notification reports that the session was not found.

## Standalone spec backlog

Specs don't have to belong to a graph. The spec backlog lets you create,
list, update and delete specs independently, optionally tagging each to
a workdir for filtering:

| Tool | Description |
|---|---|
| `spec` `list` | List specs (filterable by workdir, status) |
| `spec` `get` | One spec with its full description |
| `spec_edit` `create` | Create a standalone spec |
| `spec_edit` `update` | Update a spec's name, description, or workdir tag |
| `spec_edit` `delete` | Delete an unbound spec |
| `spec_edit` `set_status` | Admin transition: complete, skip, or reopen a standalone spec |
| `spec` `section` | Extract one canonical section from a spec body |

Spec bodies are written in the tagged `<spec>` format — all seven canonical
section tags (`<objective>`, `<functional_requirements>`,
`<non_functional_requirements>`, `<constraints>`, `<guidelines>`, `<in_scope>`,
`<out_of_scope>`) with markdown inside each. A body that does not parse is
rejected on write with the offending tag named. Existing heading-format specs
stay readable and are reported as needing conversion; rewrite them into the
tagged format by hand.

The TUI sidebar shows backlog specs under the **Backlog** section,
filtered to the selected project's workdir.

## Spec queues

A **queue** is an ordered list of existing specs decoupled from any one
graph. When a graph runs against a queue, it drains the queue's pending
specs (in queue order) through the graph instead of its own
bound specs:

| Tool | Description |
|---|---|
| `queue` `create` | Create an empty queue |
| `queue` `add_spec` | Append a spec to a queue — a grouped add lands after the group's last pending member |
| `queue` `list` | List a queue's members (or all queues) |
| `queue` `remove_spec` | Remove a spec from a queue |
| `queue` `reorder` | Full replacement of a queue's order |
| `queue` `set_group` | Set or clear a member's context group |

Pass a queue to `graph_control` `run` via `queue_id` (or `canopy graph run <graph>
--queue <queue-id>`).

**Context groups.** A queue member may carry a `group` label. Grouped
specs stay together: `queue` `add_spec` with a `group` lands the spec right
after the group's last pending member, and `queue` `set_group` (or
`canopy queue group`) moves a member next to its group. `queue` `reorder`
refuses an order that would split a group's pending members, and reports
the positions that would have split it; nothing changes on refusal. Members
of one group share a warm harness session: a grouped spec resumes the
session captured by its previous successfully-completed sibling instead of
analysing the repo from cold. Ungrouped members never share a session.

Queue membership is unaffected by `graph_control` `run` — specs stay standalone.
The `graph info` CLI and `graph` `get` MCP tool show queue-driven progress
by reconstructing what ran from the run history.

**Dirty-tree guard between specs.** The launch guard that refuses a dirty
worktree at `graph_control` `run` also runs every time a queue run moves from one spec
to the next: after a completed, failed, skipped, interrupted or limit-blocked
spec alike. A refusal pauses the graph (it does not fail it) and stores the
blocker on the graph, so `graph` `get` and `graph` `list` report it and the
TUI flags the graph blocked, until a resume clears it. The blocker
lists the first 20 dirty paths and the ways out: `graph_control` `continue`
with `mode: run_from_node` on the next spec, cleaning the tree by hand and then
`graph_control` `continue` with `mode: skip_next_spec`, or `allow_dirty_start` on the graph. A directed start on that
spec is not refused again by the guard. `allow_dirty_start` lets the run
advance.

When a queue run drains all runnable members, the graph settles on its own:
`completed` if every member completed or was skipped, or `failed` — naming
the failed members in its blocker/summary — if any member failed. A graph
reported as `running` that has no node run in progress and no scheduled
resume surfaces a `consistency_warning` in `graph` `get`.

