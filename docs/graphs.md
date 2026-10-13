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
    With `"decision": true` in its config, a router instead asks the
    platform's decision model (a fast typed-choice classifier such as
    `typesafe/jev`, invoked in print mode with the request JSON on stdin)
    which declared route to take — no agent is spawned. Routes map onto the
    model's `criteria` as label/description pairs; the rendered
    `prompt_template` (with `{{previous_output}}` also available) becomes
    the request state, and an optional `"min_confidence"` (0–1, default 0)
    sends low-confidence answers to the declared fallback. The platform must
    declare a decision-model invocation (`decision_args`) in the registry or
    the node is refused.
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

### Check node commands

Check node `command` strings have their own marker vocabulary, separate
from the agent-prompt table above — a check command cannot use
`{{spec_content}}`, `{{node_id}}`, `{{previous_feedback}}` or
`{{output:…}}`. Every marker a check command supports:

| Marker | Expands to | Quoting |
|---|---|---|
| `{{graph_name}}` | The graph's name | shell-quoted — safe for values with spaces or quotes |
| `{{workdir}}` | The directory the command runs in (the graph's workdir, or the run's sandbox worktree / `workdir_override` when one applies) | shell-quoted |
| `{{spec_id}}` | The spec's unique ID | shell-quoted |
| `{{spec_name}}` | The spec's name (empty on an idea run) | shell-quoted |
| `{{spec_start_head}}` | The git HEAD commit at the start of the spec run, empty when the workdir is not a Git repository or on a legacy run | raw — a hex SHA |
| `{{spec_start_dirty}}` | The number of non-ignored entries reported by `git status --porcelain` at the spec boundary, empty when unknown | raw — a number |
| `{{spec_committed_head}}` | The git HEAD left behind by the last `commit_rights: true` node's own execution during *this* spec run, empty until one actually commits (see [Commit rights](#commit-rights)) | raw — a hex SHA |

The shell-quoted markers expand to a single POSIX-quoted shell word, so a
name containing spaces or quotes can never break or inject into the
command. Write them bare (`{{spec_name}}`) or already single-quoted
(`'{{spec_name}}'` — the engine strips that exact wrapper so the quoting
is not doubled), never inside other quotes; a `\{{marker}}` escape stays
literal text. The raw markers expand unquoted exactly as before.

Any other `{{…}}` marker fails the check node before the command runs: the
run records `unknown marker {{x}} in check command` and nothing reaches
the shell. `graph_preflight` reports the same defect as an error before
spending any probe quota.

Every check command also runs with these environment variables (there is
no `CANOPY_WORKDIR` — the command's working directory already is the
workdir):

| Variable | Value |
|---|---|
| `CANOPY_SPEC_ID` | The spec's unique ID |
| `CANOPY_SPEC_NAME` | The spec's name; the empty string on an idea run |
| `CANOPY_GRAPH_ID` | The graph's unique ID |
| `CANOPY_GRAPH_NAME` | The graph's name |
| `CANOPY_SPEC_START_HEAD` | The same value as `{{spec_start_head}}`; the empty string when unknown |
| `CANOPY_SPEC_FILE` | Absolute path of a file holding the spec's full description — the idea text on an idea run, an empty file for a spec with no description. The engine writes it just before the command runs and removes it once the node finishes, so read it inside the command (quote the expansion: `"$CANOPY_SPEC_FILE"`). There is deliberately no `{{spec_content}}` marker in check commands: a file has no quoting problem and no command-length limit |

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
workdir is currently dirty and a spec on that same workdir ended `Failed`,
`Interrupted`, or `Skipped` (never `Completed`). The warning names the spec
whose last recorded run in **this graph** left the workdir dirty
(`graph_runs` joined to `spec_end_dirty > 0`); when no run of this graph
recorded dirt the warning says `dirty before this graph ran` and names no
spec. A resuming spec is still refused while another spec on the same workdir
ended dirty: the refusal names the blocking predecessor — its name (id when
the name is empty), the graph it ran in, its end status and end time — and,
when the next spec is resuming its own dirt, says so in the same line. Finish,
reset or remove that blocking spec (or set `allow_dirty_start`) to proceed.
When the launch proceeds, the warning says why — `allow_dirty_start` is
set on the graph, or the next spec is resuming the dirty work it left itself —
instead of telling you to set a flag. When it blocks, the message says
`not launched` before the `Set allow_dirty_start` hint. Set
`allow_dirty_start: true` via `graph_update` to downgrade a refusal to a
warning. `graph_preflight` separately reports a dirty workdir as a warning
unconditionally, with no "previous spec" logic — call it before `graph_run` to
see this regardless of history.

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

An **ensemble** is a group of 2–8 `agent` nodes in parallel (2–16 for the
sequential `cascade`/`round_robin` kinds, whose members run one at a time —
parallel stays at 8 because it runs every member concurrently) that receive
the same
prompt, plus a quorum that waits for every member
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
directly. Every entry source fans out to every member after a member-list
growth or shrink, and that replacement is atomic. `add_entry_from` completes
missing edges when the source already exists, so it does not require a
remove-then-add cycle. `graph_preflight` reports incomplete source reachability
with the source and member counts before `graph_run` spends probe quota.
`graph_delete_ensemble` removes the whole unit. `graph_get` returns the
ensemble as one unit (`ensemble_id`, members, quorum config, every entry
source) alongside its expanded nodes.

The quorum's consolidated output is one document with a
`## <platform/model> [pass|fail]` section per member, in member order. The
quorum reports pass when at least `min_pass` members passed. Member
execution is capped by a global concurrency limit (default 4) so an
8-member ensemble queues rather than forking every member at once.

By default a **parallel** quorum waits for every member branch to
terminate (pass, fail, or straggler timeout past
`straggler_timeout_minutes`, which kills the still-running process
and counts it as a fail) — it never fires early. A parallel ensemble
may instead set `quorum_grace_minutes`: the moment `passed >=
min_pass`, a timer of that many minutes starts; members finishing
inside it are consolidated as usual, and when it expires the members
still in flight are terminated with reason `quorum met` and counted
as fail (distinct from `ensemble straggler timeout` in run history).
`0` terminates stragglers immediately on quorum. The join output then
carries `quorum_met_at` and `grace_minutes`. `None` (the default)
keeps the wait-for-all behaviour.

`straggler_timeout_minutes` and `quorum_grace_minutes` are both
**parallel-only** — `graph_add_ensemble`/`graph_update_ensemble` refuse
to set either on a `cascade` or `round_robin` ensemble, and switching
an ensemble's `kind` to one of those clears both fields automatically.
The reason: a **cascade** or **round_robin** ensemble runs its members
one at a time, never concurrently, so there is no "quorum" to wait for
stragglers around and no ensemble-wide deadline is ever armed. Each
member attempt there is bounded only by its own effective timeout —
its own `timeout_minutes` override, or else the ensemble's shared
`timeout_minutes` — timed from that attempt's own start, and every
infra retry of that attempt gets that same timeout fresh. A member that
hangs can never eat into a later member's (or its own retry's) budget.

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

### No-verdict blocks

A parallel or round-robin ensemble that exits with no verdict and no
outgoing `Error` edge cannot route — every member infra-failed or walked
off without filing a verdict. Rather than failing the spec, the graph
pauses with a blocker that names the ensemble and the member reasons,
and the spec is marked `interrupted` (not `failed`). That makes the
block resumable in place: `graph_continue retry_current_node` re-enters
the ensemble at its join cursor (already-passed nodes such as the entry
node are not re-run), and `graph_update_ensemble` lets you rewire the
members before retrying. A `graph_run` on a blocked (paused + blocker)
graph refuses and reports the blocker, so a blind relaunch never masks a
crew that is silently filing nothing; clear the blocker first, or
`graph_reset` the spec. A **directed run** —
`graph_continue run_from_node` (or `graph_run --from`) — is the general
escape hatch: it starts a spec at a node you choose, reusing the recorded
outputs of every earlier node, and accepts a paused, completed or failed
graph as long as it has no active run.

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

## Infra-retry budget

An agent attempt that never files a verdict (no `graph_complete_node`,
no `graph_report_blocker`) is an infrastructure crash, retried with a
doubling backoff. Three keys control the budget, with engine defaults
of **2 retries / 60 s crash-shape window / 30 s backoff**:

```
infra_retry_limit: 0
infra_crash_max_seconds: 60
infra_backoff_seconds: 30
```

`infra_retry_limit: 0` means one attempt and no retry: a member that
exits with the infra-crash signature falls through to the next cascade
member immediately.

Precedence, most specific first: **member → ensemble → node config →
platform config → engine default.** A member's own value wins over the
ensemble's default; a plain node's own config key wins over its
platform; the platform value lives in `config.toml`'s `[[clis]]`
entries (`infra_retry_limit`, `infra_crash_max_seconds`,
`infra_backoff_seconds`) and is read fresh on every dispatch, so editing
`config.toml` needs no daemon restart. More specific ensemble/member/node
overrides win; absent everything, the engine default (2/60/30) applies.

A retry-skipped (`infra_retry_limit: 0`) crash is recorded with
`infra_retry_skipped: "limit 0"` alongside `infra_crash`/`infra_attempt`,
readable via `graph_node_run_get` (which already surfaces the
`infra_attempt`/`infra_crash` markers when present).

`graph_get` reports the effective value and its source
(`"node"`/`"platform"`/`"default"`) per node under `infra_config`, and
the ensemble's own JSON carries the raw ensemble-level fields plus each
member's `*_override` fields so member vs ensemble vs platform can be
told apart. `graph_audit_node_configs` lists every node/member whose
effective budget differs from its platform's own under
`infra_config_overrides`.

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

If the daemon stops while a node is running — a graceful shutdown or a
crash — the killed run is finalized as interrupted and routes no edge
(in particular, never its fail edge). On the next start the graph is
reconciled to `Paused` with the spec `Interrupted`, and
`graph_continue` with `retry` re-runs the interrupted node itself with
the same input it had (the same command for a check node, the same
`previous_feedback` for an ensemble member) — not whichever node an
edge would have led to.

A spec interrupted by a no-verdict ensemble block (see
[No-verdict blocks](#no-verdict-blocks)) is resumed the same way:
`graph_continue` with `retry` re-enters the blocked ensemble with its
current member list, without re-running nodes or specs that already
passed, and `graph_continue` with `skip` marks that blocked spec
`skipped` and moves on to the next one. `graph_reset` still restarts
the blocked spec from its entry node.

When the work up to some node is already done and good — after a crash,
a rewired ensemble, or a hand fix — a **directed run** starts a spec at
a node you choose instead of at Start: `graph_continue run_from_node
--spec <spec>` (or `graph_run --from` for the first spec the run
executes). It reuses every earlier node's most recent recorded output
for that spec, sets the spec cursor to the start node, and then runs the
graph with its normal edges, routers, ensembles and gates. The per-node
iteration counters restart at 1 (earlier attempts stay in the run
history). A directed run accepts a `pending`, `paused`, `interrupted`,
`failed` or `completed` spec on a graph with no active run; a running
graph refuses. An ensemble is targeted by its name (or its fan-out
node); a join node or a single member is refused, naming the ensemble.
If a reachable prompt or check references a node with no recorded output
for the spec, the run is refused and the missing node is named.

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

Editing hooks on a running graph takes effect for that run: the engine reads
the graph's stored hooks at the moment each event fires, so a `graph_update`
landing before an event fires is the edit that fires on the current run's
remaining events — the same way ensembles, nodes and prompts already take
effect at the next dispatch. When `graph_update` changes hooks while a run is
active, its response names those remaining events. An edit made after an
event already fired does not re-fire it.

Every hook run records the event it served and which hook of that event it
was (`event`, `hook_index`), plus a copy of the exact hook config that
actually fired (`hook_config`) — so a later reader can see what ran even
after the hooks are edited again. Hook runs are readable from `graph_get`'s
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
rejected by `graph_update` with the marker name and supported list; no hook
firing leaves a marker literal.

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
| `queue_add_spec` | Append a spec to a queue — a grouped add lands after the group's last pending member |
| `queue_list` | List a queue's members (or all queues) |
| `queue_remove_spec` | Remove a spec from a queue |
| `queue_reorder` | Full replacement of a queue's order |
| `queue_set_group` | Set or clear a member's context group |

Pass a queue to `graph_run` via `queue_id`.

Queue membership is unaffected by `graph_run` — specs stay standalone.
The `graph info` CLI and `graph_get` MCP tool show queue-driven progress
by reconstructing what ran from the run history.

## The 33 MCP tools

| Stage | Tools |
|---|---|
| Authoring | `graph_create`, `graph_update`, `graph_add_spec`, `graph_update_spec`, `graph_add_node`, `graph_update_node`, `graph_add_edge`, `graph_update_edge`, `graph_delete_edge`, `graph_delete_node`, `graph_add_ensemble`, `graph_update_ensemble`, `graph_delete_ensemble`, `graph_copy_node`, `graph_copy_ensemble`, `graph_audit_node_configs` |
| Sharing | `graph_export`, `graph_import`, `graph_archive`, `graph_restore` |
| Inspection | `graph_get`, `graph_list`, `graph_node_runs_list`, `graph_node_run_get`, `graph_events` |
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

Every daemon database connection waits up to 5 s for a concurrent writer
before giving up, so imports normally queue behind a busy database rather
than failing. If the wait still expires, `graph_import` returns
`graph_import: database busy after 5 s — nothing was written, retry`:
the import is all-or-nothing, nothing partial is left behind, and the
call is safe to retry as-is.

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

An ensemble's `kind` is carried in the document for `cascade` and
`round_robin`; export omits it for `parallel`, which is what an
absent `kind` means on import. Import refuses a `kind` value it does
not know — naming the ensemble and the value — and refuses a document
that carries any field it does not know how to restore, as part of
its all-or-nothing contract. `format_version` 4 remains the newest
accepted version; 1, 2 and 3 documents import unchanged (they carry fewer fields,
never more).

An [ensemble](#ensembles) round-trips as one ensemble — not as its
expanded member/quorum nodes — via its own `ensembles` array entry.
The `entry_from_node`, `on_pass_to`, and `on_fail_to` fields use a bare
string for a plain node. When wiring to another ensemble's quorum, export
uses `{ "ensemble": "<name>" }` so an ensemble target is distinct from a
plain node with the same name; import resolves that table to the target
ensemble's join node and restores its member fan-out edges.

An ensemble may be entered from more than one node
(`graph_update_ensemble`'s `add_entry_from`). Export writes every such
source in `extra_entry_sources`, an array of
`{"from_node": "<node name>", "condition": "<pass|fail|always|error>"}` entries
beyond the primary (`entry_from_node` + `entry_condition`). Import restores each as
an edge to every member, so multi-source entry — e.g. a committer entered on
`pass` by an unlock check and on `fail` by a commit-verification check — survives
the round trip. The field is omitted when an ensemble has only its primary source.

Here is a complete, hand-writable example: an implementer, a 2-model
ensemble of reviewers, and a committer the quorum routes to on pass.

```json
{
  "format_version": 4,
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
      "kind": "cascade",
      "prompt_template": "Review this diff for correctness: {{previous_feedback}}",
      "entry_from_node": "implementer",
      "entry_condition": "always",
      "on_pass_to": "committer",
      "min_pass": 1,
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
`nodes_missing_platform`. `reviewers` runs as a cascade: one member at
a time, first usable verdict wins. `reviewers` has a single entry source, so
`extra_entry_sources` is omitted; an ensemble entered from several nodes carries
each additional one there.

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

## Event log

Every graph keeps its own durable, structured history in the `graph_events`
table — the engine writes one row per event as the run happens:

| Group | Kinds |
|---|---|
| Graph | `graph_started`, `graph_paused`, `graph_resumed`, `graph_completed`, `graph_failed`, `graph_blocked` |
| Spec | `spec_started`, `spec_completed`, `spec_failed`, `spec_skipped` |
| Node | `node_started`, `node_finished` (payload: `verdict`, `platform`, `model`, `duration_s`) |
| Hooks | `hook_fired` (payload: `event`, `mode`, `ok`, `error`) |
| Config | `config_changed` (a node, ensemble or hook edit made while the graph runs) |
| Notes | `note` (reserved for the CM47 spec-note mechanism; payload: `owner_name`, `chars`) |

Read it from the CLI:

```
canopy graph log <graph-id-or-name> [--since <id|RFC3339>] [--kind <k>] [--spec <id>] [--follow]
```

Each line is `<created_at> <KIND> spec=<name> node=<name> <message>`.
`--follow` polls the daemon every 2 s and prints new events until Ctrl+C.
Over MCP, `graph_events { graph_id, since_id?, spec_id?, kinds?, limit?,
include_payload? }` returns events oldest-first with `next_since_id` for
paging.

**This replaces the `printf … >> file` logging-hook pattern.** You no longer
need a command hook that appends to a per-graph log file on `on_completed` /
`on_failed` — the engine records that history itself, intact and queryable.
Ordinary logging hooks keep working; they simply stop being necessary.

Unlike the shared activity stream (bounded per workdir, free text), this log
is never pruned by count. `canopy clean` removes events older than
`graph_event_retention_days` (default 90) for graphs that are not running.

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
