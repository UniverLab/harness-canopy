---
title: Graphs
description: The DAG graph engine — specs, nodes, edges, gates and lifecycle.
order: 7
---

# Graphs

The graph engine runs multi-step processes as a DAG, combining agent
invocations, shell checks and validation gates.

## Structure

```
Graph
└── Spec (ordered)
    └── Node ──Edge(pass/fail/always)──▶ Node
```

- **Specs** — ordered units of work inside a graph.
- **Nodes** — five kinds:
  - `agent` — invokes a CLI tool with a prompt template.
  - `check` — executes a shell command.
  - `gate` — validates previous output (e.g. `output_contains`).
  - `router` — classifies and routes by token matching with fallback.
  - `quorum` — engine-managed node that closes an
    [ensemble](#ensembles), never created directly.
- **Edges** — connect nodes with routing conditions: `pass`, `fail`,
  `always`.

## Template variables

Agent node prompts support these placeholders:

| Variable | Expands to |
|---|---|
| `{{graph_name}}` | The graph's name |
| `{{workdir}}` | The graph's working directory |
| `{{spec_id}}` | The spec's unique ID |
| `{{spec_name}}` | The spec's name |
| `{{spec_content}}` | The spec's description (falls back to name) |
| `{{node_id}}` | The node's unique ID |
| `{{previous_feedback}}` | JSON output of the previous node (truncated if oversized) |

Check node commands additionally support `{{spec_start_head}}` — the
git HEAD commit at the start of the spec run — and
`{{spec_committed_head}}` — the git HEAD left behind by the last
`commit_rights: true` node's own execution during *this* spec run,
empty until one actually commits (see [Commit rights](#commit-rights)).

`{{spec_start_head}}` only proves *some* commit landed since the spec
began; it's satisfied just as well by a commit from outside this run
sharing the same worktree (another agent, a human, a cherry-pick) as
by this spec's own work. A check node that exists to gate "did this
spec's work actually get committed" should use
`{{spec_committed_head}}` instead:

`{{spec_start_dirty}}` is the number of non-ignored entries reported by
`git status --porcelain` at the spec boundary. It is empty when the
workdir is not a Git repository or when reading a legacy run, rather
than being reported as clean.

`spec_end_dirty`/`spec_end_dirty_paths` (readable via `graph_get`/
`graph_node_runs_list`, not a template placeholder) record the same count and
the first 20 paths at the moment the spec's attempt actually ends —
`Completed`, `Failed`, or `Interrupted`. A spec that ends `Failed` or
`Interrupted` with a dirty tree folds a one-line note onto its own failure
summary/blocker text and (for a plain `Failed` ending) onto the `on_failed`
hook's `{{node}}` value.

`graph_run` and the autorun scheduler refuse to launch the next spec when the
workdir is currently dirty and the most recently touched spec on that same
workdir ended `Failed`, `Interrupted`, or `Skipped` (never `Completed`) —
naming that spec and the current dirty count. Set `allow_dirty_start: true` via
`graph_update` to downgrade this from a refusal to a warning. `graph_preflight`
separately reports a dirty workdir as a warning unconditionally, with no
"previous spec" logic — call it before `graph_run` to see this regardless of
history.

```bash
test -z "$(git status --porcelain -- src/)" \
  && test -n "{{spec_committed_head}}" \
  && test "$(git rev-parse HEAD)" = "{{spec_committed_head}}"
```

This requires a node in the graph to declare `commit_rights: true` —
without one, nothing ever populates `{{spec_committed_head}}` and the
check above always fails. `{{spec_start_head}}` remains useful on its
own for anything that only needs "did the tree change at all", where
a concurrent commit from outside this run isn't a concern.

## Ensembles

An **ensemble** is a group of 2-8 `agent` nodes that receive the same
prompt in parallel, plus a quorum that waits for every member
before routing onward. It replaces what would otherwise be N member
nodes, N prompts, and 2N+2 edges wired by hand — `graph_add_ensemble`
creates the whole unit in one call:

```json
graph_add_ensemble {
  graph_id, name: "proposers",
  prompt_template: "...",              # one prompt, shared by every member
  members: [
    { platform: "opencode", model: "mimo-v2.5-free" },
    { platform: "opencode", model: "glm-4.6-free" },
    { platform: "opencode", model: "qwen3-coder-free" }
  ],
  from_node, condition: "always",       # entry wiring
  min_pass: 2,                          # default: all members
  on_pass_to, on_fail_to                # exit wiring
}
```

Canonical uses: (a) N cheap/free models draft a solution in parallel,
the quorum consolidates their proposals, and an arbiter (or the
implementer itself) receives all of them and implements the consensus
— expensive quota is spent once, on a pre-digested task; (b) 2-3 free
reviewer models review the same diff in parallel and the quorum
consolidates their findings into one verdict.

Members differ by `platform`/`model`, and each may set its own
`prompt_override` to review the same input from a different angle instead
of sharing the template. Each member may also set its own `timeout_minutes`,
overriding the ensemble's shared value for that member only — the leash a
flaky free-tier member needs (say, 3 minutes) and the room a careful review
needs (say, 50 minutes) no longer have to be the same number. A member
without one uses the ensemble's `timeout_minutes`, exactly as before this
field existed. The difference matters at scale: a 3-minute leash instead of
a 60-minute one on a stalling member saves 57 minutes of wall clock per
rotation, on a graph that rotates many times a night.

`graph_update_ensemble` changes the shared prompt
(propagated to every member without its own override), the member list,
quorum config (`min_pass`, `straggler_timeout_minutes`,
`quorum_grace_minutes`), exit wiring
(`on_pass_to`/`on_fail_to` take a node id or another ensemble's id, chaining
quorums with no intermediate node), and entry wiring (`from_node` replaces
every entry; `add_entry_from`/`remove_entry_from` add or detach one source so
several nodes can enter with no relay), all without touching member nodes
directly. `graph_delete_ensemble` removes the whole unit. `graph_get` returns the
ensemble as one unit (`ensemble_id`, members, quorum config, every entry
source) alongside its expanded nodes.

The quorum's consolidated output is one document with a
`## <platform/model> [pass|fail]` section per member, in member order. The
quorum reports pass when at least `min_pass` members passed. Member
execution is capped by a global concurrency limit (default 4) so an
8-member ensemble queues rather than forking every member at once.

By default the quorum waits for every member branch to terminate
(pass, fail, or straggler timeout past
`straggler_timeout_minutes`, which kills the still-running process
and counts it as a fail) — it never fires early. A parallel ensemble
may instead set `quorum_grace_minutes`: the moment `passed >=
min_pass`, a timer of that many minutes starts; members finishing
inside it are consolidated as usual, and when it expires the members
still in flight are terminated with reason `quorum met` and counted
as fail (distinct from `ensemble straggler timeout` in run history).
`0` terminates stragglers immediately on quorum. The join output then
carries `quorum_met_at` and `grace_minutes`. `None` (the default)
keeps the wait-for-all behaviour; cascade and round-robin ensembles
ignore the field. `straggler_timeout_minutes` still caps each member
from its own start, independently of the grace window.

Members are agent nodes only, and nested ensembles (an ensemble wired
into another ensemble's members or quorum) are rejected. A bounce back
into an ensemble costs one iteration against the ensemble's shared budget,
same as any node, but no longer cold-starts every member: a parallel
member whose last run in this spec passed resumes that session (one that
failed, including a terminated straggler, cold-starts); a round-robin
bounce after a pass re-invokes the same member resumed, and after a
failure moves on to the next member cold and stays there for the rest of
the spec; a cascade bounce resumes the member that last passed, falling
through to the next member cold only if the resumed one fails again. A
member's own `resume: false` still forces a cold start. The TUI graph view renders an ensemble
collapsed as one box (`name [N models]` + quorum) with live per-member
state while running, expandable on inspect.

## Commit rights

Most graphs want exactly one node to land work in git — a committer at
the end of the chain — while every earlier node leaves its changes
uncommitted so the reviewers downstream have a real diff to review.
Asking for that in the prompt does not hold: three different models
have committed anyway against an explicit, capitalised "you have no
commit rights" rule, and each time the reviewers that followed were
handed an empty diff and the quality gate quietly became a no-op.

So the engine enforces it. Mark the committer in its **node config**:

```
commit_rights: true
```

The engine records `git rev-parse HEAD` before and after every node it
runs and compares the two. A node without `commit_rights` that moved
HEAD is a deterministic **fail**, whatever the node itself reported —
it routes through the `fail` edge like any other failure, and the
reason (`Node 'X' committed but has no commit rights: HEAD moved
a1b2c3 -> d4e5f6`) lands in the run output, in `canopy graph info`, and
in the next node's `{{previous_feedback}}`.

Three things are deliberate:

- **Enforcement is opt-in per graph.** It activates only once some node
  in the graph declares `commit_rights: true`. A graph that designates
  nobody can't be told apart from one written before this key existed,
  so enforcing there would fail the very node it relies on to land
  work. Existing graphs are unchanged until you name a committer.
- **Rights are explicit configuration**, never inferred from a node's
  name, kind, or prompt. Nodes without the key have no commit rights.
- **The engine never undoes the commit.** It reports and routes; it
  will not `reset`, `revert`, or rewrite your history, because the
  unauthorized commit usually contains the *correct* work made by the
  wrong node, and an unattended daemon rewriting history is a far worse
  failure than the one it is fixing. Both hashes are in the output —
  undo it yourself if it doesn't belong.

Ensemble members are checked as a group rather than individually: they
run concurrently against one workdir, so a moved HEAD can't be
attributed to a single member, and the quorum fails as a whole.
Non-git workdirs are unaffected, as is any node that edits files
without committing — the normal case.

The same `commit_rights: true` node is also what populates
`{{spec_committed_head}}`: whenever its own execution moves HEAD, that
new HEAD is recorded against this spec run, overwritten each time it
commits again (e.g. a review/retry cycle). A downstream check node
uses it to verify *this run's own committer* landed a commit, not
merely that HEAD differs from wherever the spec started — see
[Template variables](#template-variables).

**Editing the recommended check command text here does not retroactively
touch any graph.** A node's `command` is whatever text was written into
its config when the graph was authored (`graph_add_node`, or copied from
a blueprint) — the engine reads it fresh at execution time but never
rewrites it. A spec already using the old `{{spec_start_head}}`-only
comparison keeps using it until someone explicitly runs
`graph_update_node` on it; only graphs authored or edited after adopting
`{{spec_committed_head}}` get the stronger check.

## Self-report requirement

A harness can exit 0 having done nothing: every tool call refused,
a quota exhausted, a provider outage — the process still ends cleanly
and the graph engine sees a clean exit code. By default that is
recorded as `Pass` if the process also produced output, exactly as it
always has. Set `require_report: true` in an agent node's config to
close that gap for a node whose graph depends on being able to tell:

```
require_report: true
```

With the flag set, an agent run that exits 0 but never calls
`graph_complete_node` itself is recorded as a deterministic **fail**
(`failure_kind: "no_report"`) and routes down the node's `fail` edge —
it is never retried as an infra crash, since nothing actually crashed.
An explicit self-report always wins regardless of this flag, pass or
fail; `require_report` only judges the case where none was ever made.

Whether or not the flag is set, every agent run that finishes without
calling `graph_complete_node` carries `"unreported": true` in its output
— this is unconditional, so a resilience node downstream can always
tell "the harness ran and chose not to report" apart from "the harness
never ran," without needing `require_report` itself. Ensemble members
are judged individually, exactly like a lone node.

## Lifecycle

Create → run → (pause / continue) → complete. `graph_continue`
supports **retry** and **skip** strategies for stuck nodes, and
`graph_report_blocker` escalates to a human when intervention is
needed. Iteration limits prevent infinite retry graphs. `graph_reset`
returns a completed/failed graph to pending so `graph_run` can restart
it, and `graph_schedule_autorun` sets a future time at which the graph
auto-resumes (useful for quota-limited graphs that fail and need to
wait before retrying). Call it again with `at` omitted to cancel a
pending schedule.

## Concurrency

Running many graphs at once, against different working directories, is
a supported capability — not an accident of the implementation. Start,
run, pause, resume, or finish one graph, and no other graph's status,
specs, node runs, or worktree are affected. There is no cap on how
many graphs can run concurrently, and nothing needs to be configured to
enable it: it is the default behavior of the daemon.

The one boundary: **two graphs must not share a workdir.** Two graphs
racing to commit, check out, or edit files in the same working tree
will fight over it — the engine does nothing to make that safe, and
doing so is deliberately out of scope. Point concurrent graphs at
different working directories (or at worktrees of the same repo) and
they run independently with no coordination required from you.

## Event-keyed hooks

A graph carries hooks keyed by event, over four events: `on_completed`,
`on_failed`, `on_blocked`, `on_spec_completed`. More than one hook may be
registered for a single event, via `graph_update`'s `hooks` map (each key is
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
  `graph_reset` → completed cycle fires it again, once per completing run.
- `on_failed` fires when the graph reaches `failed`.
- `on_blocked` fires when it stops with a blocker set (transition to
  `paused` with blocker data).
- `on_spec_completed` fires once per spec reaching `completed`, whether the
  specs come from the graph's own bound specs or from a queue.

**Hooks are not retroactive** — a hook registered after its event has
already happened does not fire. A hook that fails is recorded and never
changes the graph's own status, and never stops the remaining hooks of that
event from running.

Every hook run records the event it served and which hook of that event it
was (`event`, `hook_index`), and is readable from `graph_get`'s
`completion_hook_runs` and `canopy graph info`'s hook-runs listing alongside
the graph's node runs.

Each event exposes what its consumer needs, in addition to `{{graph_name}}`
and `{{workdir}}`: `on_completed` keeps `{{completed_specs}}` (name +
one-line summary of each spec this run completed, one per line, `(none)` if
the run completed zero specs); `on_spec_completed` gets `{{spec_name}}` and
`{{spec_id}}`; `on_failed` and `on_blocked` get `{{blocker}}` and `{{node}}`
(the name of the node that ended the run). A prompt carrying a marker its
event cannot bind is refused and recorded as a failed hook run.

The existing configuration keeps working, unchanged and unattended:
whatever a graph has in its `on_completed` column becomes a hook on the
`on_completed` event with no user action and no loss, and a `graph_update`
call passing today's `on_completed` shape still registers that hook.

The first intended use is a documentation-maintenance agent: on
completion, review the specs this run closed, the resulting code, and
`docs/`/`README`, then update the docs to match what actually shipped.

An interactive hook targets a session either by its exact id
(`target_session_id`) or by its name (`target_session_name`) — exactly one
of the two must be set. A session id is not stable over time: a hook
configured with one will eventually point at a session that no longer
exists (a daemon reinstall or TUI restart changes ids), and firing then
fails loudly naming the id rather than redirecting the message anywhere
else. `target_session_name` exists for exactly this case: names survive a
restart that ids don't, so a name-targeted hook is resolved against the
live session set fresh on every fire — never cached — the same set
`session_list` reads. Exactly one live session with that name resolves and
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

A message enqueued while no TUI is running is queued, not lost: it stays
pending and is delivered when a TUI later starts.

## Standalone spec backlog

Specs don't have to belong to a graph. The spec backlog lets you create,
list, update and delete specs independently, optionally tagging each to
a workdir for filtering:

| Tool | Description |
|---|---|
| `spec_create` | Create a standalone spec |
| `spec_list` | List specs (filterable by workdir, status) |
| `spec_update` | Update a spec's name, description, or workdir tag |
| `spec_delete` | Delete an unbound spec |
| `spec_set_status` | Admin transition: complete, skip, or reopen a standalone spec |
| `spec_section_get` | Extract one canonical section from a spec body |
| `spec_convert` | Migrate legacy heading-format spec bodies to the tagged `<spec>` format |

Spec bodies are written in the tagged `<spec>` format — all seven canonical
section tags (`<objective>`, `<functional_requirements>`,
`<non_functional_requirements>`, `<constraints>`, `<guidelines>`, `<in_scope>`,
`<out_of_scope>`) with markdown inside each. A body that does not parse is
rejected on write with the offending tag named. Existing heading-format specs
stay readable during the transition and are reported as needing conversion;
`spec_convert` migrates them one row at a time and never rewrites a running
spec.

The TUI sidebar shows backlog specs under the **Backlog** section,
filtered to the selected project's workdir.

## Spec queues

A **queue** is an ordered list of existing specs decoupled from any one
graph. When a graph runs against a queue, it drains the queue's pending
specs (in queue order) through the graph instead of its own
bound specs:

| Tool | Description |
|---|---|
| `queue_create` | Create an empty queue |
| `queue_add_spec` | Append a spec to the end of a queue |
| `queue_list` | List a queue's members (or all queues) |
| `queue_remove_spec` | Remove a spec from a queue |
| `queue_reorder` | Full replacement of a queue's order |

Pass a queue to `graph_run` via `queue_id`.

Queue membership is unaffected by `graph_run` — specs stay standalone.
The `graph info` CLI and `graph_get` MCP tool show queue-driven progress
by reconstructing what ran from the run history.

## The 32 MCP tools

| Stage | Tools |
|---|---|
| Authoring | `graph_create`, `graph_update`, `graph_add_spec`, `graph_update_spec`, `graph_add_node`, `graph_update_node`, `graph_add_edge`, `graph_update_edge`, `graph_delete_edge`, `graph_delete_node`, `graph_add_ensemble`, `graph_update_ensemble`, `graph_delete_ensemble`, `graph_copy_node`, `graph_copy_ensemble`, `graph_audit_node_configs` |
| Sharing | `graph_export`, `graph_import`, `graph_archive`, `graph_restore` |
| Inspection | `graph_get`, `graph_list`, `graph_node_runs_list`, `graph_node_run_get` |
| Runtime | `graph_run`, `graph_reset`, `graph_schedule_autorun`, `graph_schedule_continue`, `graph_pause`, `graph_continue`, `graph_complete_node`, `graph_report_blocker`, `graph_preflight` |

Graphs can be authored programmatically by agents through these tools,
or edited in the [TUI graph editor](tui.md) with inline JSON config
validation. The `canopy graph` CLI subcommands mirror the runtime tools
from the terminal: `list`/`info`/`export` are read-only inspection, and
`import`/`run`/`pause`/`continue`/`reset`/`autorun` delegate the
matching MCP tool to the daemon — a second way to drive a graph when an
MCP client can't reach it. See the
[CLI reference](cli-reference.md#graph-control).

## Export and import

A graph's design — its name, description, nodes, edges, and ensembles —
can leave one machine as a single JSON file and be recreated on
another. Sharing a graph becomes sending a file, not narrating the
`graph_add_node`/`graph_add_edge`/`graph_add_ensemble` calls that built
it, and the file is also a diff: review a change to a graph, or keep
one in a repo next to the code it operates on.

```
canopy graph export <graph_id> [--output <path>]
canopy graph import <path> [--workdir <dir>] [--name <name>]
```

`export` writes to `--output`, or to stdout (so it can be piped) when
omitted. `import` always creates a **new** graph — it never updates,
merges, or overwrites an existing one; `--workdir` defaults to the
current directory, and `--name` overrides the file's own name. If the
resolved name is already taken in the target workdir, import still
succeeds under a numeric suffix (`"My Graph (2)"`) and reports which
name it used. The same two operations exist as MCP tools,
`graph_export { graph_id }` and
`graph_import { document, workdir?, name? }`, so a graph is drivable end
to end through MCP as well as the CLI.

What the file **excludes** is deliberate: no ids (edges reference
nodes by `name`, which is what makes the file reviewable and
hand-editable — node names must therefore be unique within an exported
graph, or export refuses with the names it found), no `workdir`, no
specs, and no run/status state. A graph file is a shape and a set of
instructions, not somebody else's backlog or history.

`platform`/`model` are always included for every agent node and
ensemble member (`format_version: 2`), so an exported file round-trips
its harness bindings through `import` unchanged. A node that uses its
platform's default model exports `"model": null` explicitly, and an
ensemble member without a binding or override exports explicit `null`s
for the unset fields. Import accepts both `format_version: 1`
(members with no binding, reported as missing a platform) and `2`
(bindings restored verbatim); it never rejects a platform the
importing machine has not configured — validating that a pair is
usable belongs to `graph_preflight`. `import`'s response always lists
every agent node left without a platform, so there's exactly one
thing to check before running an imported graph:
`nodes_missing_platform` in the MCP response, or the same list
printed by the CLI.

An [ensemble](#ensembles) round-trips as one ensemble — not as its
expanded member/quorum nodes — via its own `ensembles` array entry.
The `entry_from_node`, `on_pass_to`, and `on_fail_to` fields use a bare
string for a plain node. When wiring to another ensemble's quorum, export
uses `{ "ensemble": "<name>" }` so an ensemble target is distinct from a
plain node with the same name; import resolves that table to the target
ensemble's join node and restores its member fan-out edges.

Here is a complete, hand-writable example: an implementer, a 2-model
ensemble of reviewers, and a committer the quorum routes to on pass.

```json
{
  "format_version": 2,
  "name": "implement-and-review",
  "description": "Implement a spec, get two model opinions, then commit.",
  "nodes": [
    {
      "name": "implementer",
      "kind": "agent",
      "position": 1,
      "config": {
        "platform": "opencode",
        "model": null,
        "prompt_template": "Implement: {{spec_content}}",
        "timeout_minutes": 30
      }
    },
    {
      "name": "committer",
      "kind": "agent",
      "position": 4,
      "config": {
        "platform": "opencode",
        "model": "opencode/muse-spark-1.3-contributor-free",
        "prompt_template": "Review the feedback and commit if satisfied.",
        "commit_rights": true,
        "timeout_minutes": 15
      }
    }
  ],
  "edges": [],
  "ensembles": [
    {
      "name": "reviewers",
      "prompt_template": "Review this diff for correctness: {{previous_feedback}}",
      "entry_from_node": "implementer",
      "entry_condition": "always",
      "on_pass_to": "committer",
      "min_pass": 2,
      "timeout_minutes": 20,
      "members": [
        {"platform": "copilot", "model": null, "prompt_override": null},
        {"platform": "opencode", "model": "opencode-go/qwen3.7-plus", "prompt_override": null}
      ]
    }
  ]
}
```

Note what is absent: no `id` anywhere, and the ensemble's own
member/quorum nodes never appear in `nodes` — only its
`entry_from_node`/`on_pass_to` (both plain node names) and its
`members` array do. `implementer` uses its platform's default model,
hence the explicit `"model": null`; a `format_version: 1` document
with `{}` members still imports, with every unbound node reported as
`nodes_missing_platform`.

## Archive and restore

Graphs can be archived instead of deleted — they leave the main
`graph_list`/sidebar view but their row, specs, and full run history
are untouched and can be restored at any time:

```
canopy graph archive <graph_id>
canopy graph restore <graph_id>
```

Over MCP: `graph_archive { graph_id }` and `graph_restore { graph_id }`.
Archiving refuses a `running` graph — pause it first. The archived graph
is still reachable directly by id via `graph_get` regardless of
archived state. Restoring is a plain flag flip — no data is lost or
moved.

## Node run history

When a graph fails, the node run history lets you diagnose exactly what
happened:

```
canopy graph runs <graph_id> [--node <node_id>] [--limit <n>]
```

Over MCP: `graph_node_runs_list { graph_id, node_id?, spec_id?, limit? }`
returns the most recent runs first (default 20, capped at 200).
`graph_node_run_get { run_id }` fetches one run's full stored input and
output, including `infra_attempt`/`infra_crash` markers when present.
Secret-shaped substrings are redacted before the output crosses the
boundary.

This is the second step of failure diagnosis: list to find the
offending run, then fetch its output here — the exact stderr/stdout/
reported_output the engine recorded.

## Node blueprints

Rather than pasting a full `config` into every `graph_add_node` call, a
node can reference a **blueprint** — a reusable `{name, kind, config}`
template — by name:

```
graph_add_node { spec_id, name, blueprint: "cargo-gates" }
graph_add_node { spec_id, name, blueprint: "implementer-claude", config_overrides: { "model": "opus" } }
```

`config_overrides` is a shallow merge on top of the blueprint's config
template — override keys win, every other templated key is preserved.
An unknown blueprint name returns an actionable error listing every
available blueprint.

Five builtins are seeded automatically at daemon startup if missing
(re-seeded if deleted from the DB directly): `implementer-claude`,
`cargo-gates`, `reviewer-committer-mimo`, `commit-check`,
`resilience-mimo`. Builtins can't be deleted. Manage blueprints with
`blueprint_list`, `blueprint_create`, and `blueprint_delete` (custom
only). The TUI sidebar lists available blueprints, and the graph editor
validates blueprint references inline.
