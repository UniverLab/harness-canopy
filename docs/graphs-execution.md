---
title: Execution
description: Commit rights, self-reports, retries, the execution limit, lifecycle, deterministic commits and spec notes.
order: 10
---

# Execution

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
reason (`Node 'X': HEAD moved from a1b2c3 to d4e5f6 while it ran`,
plus the new commits in that range, capped at 5) lands in the run
output, in `canopy graph info`, and in the next node's
`{{previous_feedback}}`. The engine cannot know *who* moved HEAD, so
the message never claims the node committed: the commit may come from
this node, another session, a hook, or a human — the listing shows
each new commit as `a1b2c3d subject line (2026-10-16)` so you can tell
at a glance when it belongs to something else entirely.

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
[Template variables](graphs-templates.md#template-variables).

**Editing the recommended check command text here does not retroactively
touch any graph.** A node's `command` is whatever text was written into
its config when the graph was authored (`graph_node` `add`, or copied from
a node document) — the engine reads it fresh at execution time but never
rewrites it. A spec already using the old `{{spec_start_head}}`-only
comparison keeps using it until someone explicitly runs
`graph_node` `update` on it; only graphs authored or edited after adopting
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
(`failure_kind: "no_report"`) and routes down the node's `fail` edge.
It is re-dispatched exactly once with an explicit reporting instruction
(the `# [REPORT REQUIRED]` nudge) rather than retried on the
infra-crash budget, since nothing actually crashed; a second unreported
exit fails the node as usual. An explicit self-report always wins
regardless of this flag, pass or fail; `require_report` only judges the
case where none was ever made.

Whether or not the flag is set, every agent run that finishes without
calling `graph_complete_node` carries `"unreported": true` in its output
— this is unconditional, so a resilience node downstream can always
tell "the harness ran and chose not to report" apart from "the harness
never ran," without needing `require_report` itself. Ensemble members
are judged individually, exactly like a lone node.

An agent never retypes its run's ids to file a verdict: every agent
process starts with `CANOPY_GRAPH_ID`, `CANOPY_NODE_ID` and
`CANOPY_NODE_RUN_ID` in its environment, and its prompt's `[REPORTING]`
section tells it to call `graph_complete_node`/`graph_report_blocker`
without ids — the daemon resolves them from that identity. A supplied
id with the right 8-character prefix but a mistyped tail is corrected
to the environment's run (the reply says `Corrected ...`); any other
unknown id fails naming the run id the environment carries, and an
explicit id naming a different, still-running run is honored as before.

## Infra-retry budget

An agent attempt that never files a verdict (no `graph_complete_node`,
no `graph_report_blocker`) is an infrastructure crash, retried with a
doubling backoff. One exception: an attempt whose process exited 0 with
real output is re-dispatched exactly once with a `# [REPORT REQUIRED]`
instruction first — only if that retry also exits unreported does the
node settle as failed. Three keys control the budget, with engine defaults
of **2 retries / 60 s crash-shape window / 30 s backoff**:

```
infra_retry_limit: 0
infra_crash_max_seconds: 60
infra_backoff_seconds: 30
```

`infra_retry_limit: 0` means one attempt and no retry on the crash
budget: a member that exits with the infra-crash signature falls through
to the next cascade member immediately. (The one `# [REPORT REQUIRED]`
re-dispatch above is outside this budget, so it still applies once.)

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
readable via `graph` `node_run` (which already surfaces the
`infra_attempt`/`infra_crash` markers when present).

`graph` `get` reports the effective value and its source
(`"node"`/`"platform"`/`"default"`) per node under `infra_config`, and
the ensemble's own JSON carries the raw ensemble-level fields plus each
member's `*_override` fields so member vs ensemble vs platform can be
told apart. `graph` `audit` lists every node/member whose
effective budget differs from its platform's own under
`infra_config_overrides`.

## Execution limit

A spec that fails with a genuine verdict in separate graph executions is
counted per spec, across relaunches and daemon restarts. Once it reaches
the limit (`spec_attempt_limit`, default 3) the graph pauses with a blocker
instead of failing again. Infrastructure failures do not count.

To clear the limit, do one of these:

- **Reset the spec by id.** `graph_control` `reset` with `specs: [<spec id>]` zeroes
  that spec's execution count and leaves its worktree untouched. Then
  `graph_control` `run` or a directed `graph_control` `continue` (`mode: run_from_node`) relaunches it. A `graph_control` `reset`
  without `specs` keeps every spec's count.
- **Give an explicit directive.** `graph_control` `run` with `start_node_id`,
  `graph_control` `continue` with `mode: run_from_node`, or `graph_control` `continue` with
  `mode: retry_current_node` (on a failed graph, or on a pending spec with a
  recorded node) resets the targeted spec's count before its run starts. The
  response says `Execution limit reset for spec '<name>'`.

Automatic re-runs (cron auto-continue, autorun) never reset the count.

## Lifecycle

Create → run → (pause / continue) → complete. `graph_control` `continue`
supports **retry** and **skip** strategies for stuck nodes, and
`graph_report_blocker` escalates to a human when intervention is
needed. Iteration limits prevent infinite retry graphs. `graph_control` `reset`
returns a completed/failed graph to pending so `graph_control` `run` can restart
it, and `graph_control` `schedule_autorun` sets a future time at which the graph
auto-resumes (useful for quota-limited graphs that fail and need to
wait before retrying). Call it again with `at` omitted to cancel a
pending schedule.

A run remembers the workdir it was launched with — the `workdir` override
when `run` was given one, else the graph's stored workdir — and every
continuation of that run (`continue` in any mode, scheduled resumes, quota
auto-resume) re-dispatches in it, as do the run's hooks (`{{workdir}}`,
`CANOPY_HOOK_WORKDIR`). `continue` without `workdir` keeps it; an explicit
`workdir` replaces it. A new `run` without an override uses the stored
workdir again; `graph` `get`/`list` show the run's as `run_workdir` when it differs.

If the daemon stops while a node is running — a graceful shutdown or a
crash — the killed run is finalized as interrupted and routes no edge
(in particular, never its fail edge). On the next start the graph is
reconciled to `Paused` with the spec `Interrupted`, and
`graph_control` `continue` with `mode: retry_current_node` re-runs the interrupted node itself with
the same input it had (the same command for a check node, the same
`previous_feedback` for an ensemble member) — not whichever node an
edge would have led to.

When a node is terminated, canopy also ends the harness session that node
started, for platforms that declare how to end a session (opencode does).
Without that, a terminated opencode session could be resumed by the
platform's background service after a restart and edit the repo beside the
next attempt. Every harness node runs with the node's workdir as its
working directory, so the session list the end step searches is the same
one the node created. The outcome is recorded on the run's output as
`session_end` — `"ended <id>"`, `"not configured"` (the platform declares
no way to end a session), `"not found"` (no id on the run and no title
match), or `"failed: <reason>"` — readable via `graph` `node_run`.

An agent node that has filed its verdict must not keep running. Once the
verdict is recorded, its process gets a grace period to exit on its own
(`graphs.verdict_grace_seconds`, default 15; 0 disables the shutdown). If it
is still alive when the grace period ends, canopy terminates its process
group and ends its harness session as described above. The next node,
ensemble member or join starts only after that process is gone; in a parallel
ensemble each member gets the same grace period and is ended without waiting
for the others. Termination never changes the verdict: a pass stays a pass.
The node's output records `ended_after_verdict` — `"exited"` when the process
left on its own, `"terminated"` when canopy ended it — readable via
`graph` `node_run`.

A spec interrupted by a no-verdict ensemble block (see
[No-verdict blocks](graphs-ensembles.md#no-verdict-blocks)) is resumed the same way:
`graph_control` `continue` with `mode: retry_current_node` re-enters the blocked ensemble with its
current member list, without re-running nodes or specs that already
passed, and `graph_control` `continue` with `mode: skip_next_spec` marks that blocked spec
`skipped` and moves on to the next one. `graph_control` `reset` still restarts
the blocked spec from its entry node.

When the work up to some node is already done and good — after a crash,
a rewired ensemble, or a hand fix — a **directed run** starts a spec at
a node you choose instead of at Start: `graph_control` `continue` with
`mode: run_from_node` and optional `spec_id` (CLI: `canopy graph continue --from
<node> [--spec <spec>]`), or `graph_control` `run` with `start_node_id` for
the first spec the run executes. The spec reference accepts a full id, an
unambiguous id prefix, or the spec's code (e.g. `FIX-7`), resolved within the
graph's workdir; on a queue-driven graph it must name a member of the queue
being run. `--spec` is optional: on a queue-driven graph it defaults to the
graph's current spec — an in-flight member (`running` or `interrupted`)
first, else a `failed` one, else the next `pending` one — so a bare directed
run restarts the failed spec rather than the next fresh one. It reuses the
most recent recorded output of each node upstream of the start node for that
spec; the start node and everything downstream of it always run again, and
its own old output is never reused. If an upstream node's latest run is
`failed`, `error` or `interrupted`, the directed run is refused and that node
is named: a failed prerequisite is never treated as passed. An upstream node
that is a member of an ensemble is judged by the ensemble's quorum result —
its join node's run for this spec — not by that member's own latest run: a
member that failed while the quorum passed is a satisfied prerequisite, while
a failed quorum refuses the run and names the join. A quorum only governs the
pass it settled: a member that ran again after the last join (a crash between
the two) has no verdict yet and is judged on its own run, still naming the
member. The spec cursor
is set to the start node, and the graph then runs with its normal edges,
routers, ensembles and gates. On a queue-driven graph the chosen spec is
what runs first: queue members before it are marked `skipped`, members
after it run in their usual order, and any other member left `running` by a
stale or mistaken state is reset to `pending` and left for a later run. On a
graph running its own bound specs the same rule holds: bound specs before
the chosen one are marked `skipped` — one that already `completed` or
`failed` keeps its status, and one left `running` is named in the
`graph_failed` event instead — the chosen spec runs first whatever its own
status, and the bound specs after it run in their usual order. The
per-node iteration counters restart at 1 (earlier attempts stay in the run
history). A directed run accepts a `pending`, `running`, `interrupted`,
`failed` or `completed` spec on a graph with no active run; a running
graph refuses. An ensemble is targeted by its name; targeting the node the
ensemble fans out from starts at that node itself, runs it, and fans out
to the ensemble from there — the named node always runs first and is never
reported as skipped. A join node or a single ensemble member is refused,
naming the ensemble.
On a graph that ran with an `idea` (a spec-less idea run), the directed run
resumes *that* run: the graph remembers the idea it last ran with,
`{{spec_content}}` renders the stored idea, and the failed run's
blank-name bookkeeping spec plus its node history (so `{{previous_feedback}}`
still carries the failed run's feedback) are kept rather than purged — no
`spec_id` is needed. An idea run that predates the graph remembering its
idea (older databases) resumes the same way: the idea is read from that
bookkeeping spec's text and remembered from then on. An idea graph that has
never run has nothing to resume: `graph_control` `continue` with `mode: run_from_node`
(and `mode: retry_current_node`) refuses it, naming `graph_control` `run` with an `idea`
as the way to start one.
If a reachable prompt or check references a node with no recorded output
for the spec, the run is refused and the missing node is named.

**Bound specs are read live.** Every advance re-reads the graph's bound
specs and picks the first one still `pending`, instead of working through a
list captured when the run started. A spec added with `graph_edit` `attach_spec`
while the graph runs is therefore never left behind: one bound at a position
after the current spec runs later, in position order, and one bound at an
earlier position runs right after the current spec. `graph_completed` is
emitted only when no bound spec is left runnable; if a run would settle
with one still `pending` — or one left `running` or `interrupted` by a
stale state — the graph fails instead, naming those spec ids in its
`graph_failed` event, and `on_completed` hooks do not fire.
`graph_control` `continue` then picks the pending spec up.

## Deterministic commits

Canopy can build a deterministic commit message from a spec's type and
scope, and number specs automatically per type — a short code that serves
as a quick reference and counts features and bugs.

1. **Number specs by change type.** Give each spec a `type` (one of the
   Conventional Commits types: `feat`, `fix`, `docs`, `style`, `refactor`,
   `perf`, `test`, `build`, `ci`, `chore`, `revert`). Canopy assigns
   `code = <TYPE>-<n>` with TYPE upper-cased (`FEAT-12`, `FIX-7`, `DOCS-3`)
   and `n` the next number for that type among the specs tagged with the
   same workdir (counter persisted per workdir and type; never reused,
   even after a spec is deleted). The code is immutable and never set by
   callers. Setting `type` later on a legacy spec through `spec_edit` `update`
   assigns its code at that moment; changing the type afterwards does not
   renumber it.

2. **Conventional titles.** When `spec_edit` `create`/`graph_edit` `attach_spec` receive a
   name of the form `<type>(<scope>)?!?: <subject>` and no explicit `type`,
   canopy parses it and stores the subject as the name, with type and scope
   extracted. An explicit `type` wins over the parsed one.

3. **Commit message.** Every spec exposes `CANOPY_SPEC_COMMIT_MESSAGE`
   to check nodes and `{{spec_commit_message}}` to agent prompts. The
   message is `<type>(<scope>): <name>` (or `<type>: <name>` without scope).
   A deterministic commit node uses `$CANOPY_SPEC_COMMIT_MESSAGE` as the
   commit message and never guesses.

4. **Opt-in enforcement.** A graph created or updated with
   `require_spec_type: true` (default `false`) refuses a spec without a type
   in `graph_edit` `attach_spec`, and `graph_control` `run` refuses a queue holding one, naming
   the spec. `queue` `add_spec` refuses an untyped spec while any graph has the
   setting on, because queues are not tied to a single graph. Legacy specs
   keep working everywhere else.

**Recipe: deterministic commit node**

Add a check node with `commit_rights: true` and use the prebuilt message:

```json
{
  "name": "commit",
  "kind": "check",
  "command": "git add -A && git commit -m \"$CANOPY_SPEC_COMMIT_MESSAGE\"",
  "success_condition": "exit_code_0",
  "commit_rights": true
}
```

When the spec has `type: fix` and `scope: tui`, and its name is
`"open F1 from inside a session"`, the node commits with message
`fix(tui): open F1 from inside a session`.

## Spec notes

Every spec carries one working-memory note per owner: a short document a
node writes while it works, and that every later node, model and iteration
of the same spec receives in its prompt. Notes are working memory for the
next agent, not a log — the history of changes lives in the event log's
`note` rows above.

**Ownership.** The owner of a note is the node that wrote it — except when
the writer is an ensemble member, in which case the owner is the ensemble,
so every member shares the one note and a replacing member continues where
the cut one left off. Each spec has at most one note per owner, plus one
`operator` note written by people and orchestrators without a run. A node
can only write its own note: `spec_edit` `note_write` takes no owner argument.

**Writing and reading.** An agent writes with `spec_edit` `note_write`
(`run_id`, `body`) — the engine resolves the owner from the active run and refuses a
`run_id` that is no longer running. The body is at most 2 500 characters;
longer writes are refused naming the limit. `spec_edit` `note_write` with
(`spec_id`, `body`) and no `run_id` writes the `operator` note. Read with
`spec` `notes` (`spec_id`) (newest first), or `canopy spec notes <spec-id>`.

**Prompt injection.** Every agent-node prompt gets a `[SPEC NOTES]` section
before `[REPORTING]` with a 5 000-character budget: the caller's own note
first, then the Designer's note (the owner of the graph's first agent node
or ensemble after Start), then the rest newest-first. A note is included
whole or as a stub naming its owner, size and time — never truncated — so
the same notes always give the same section bytes.

**Lifetime.** Notes survive bounces, resumes, `graph_control` `continue` and node
retries of the same spec. `graph_control` `reset` keeps them unless called with
`clear_notes: true`. Deleting the spec deletes its notes.

**Check nodes.** Before a check node's command runs, the engine writes the
spec's notes to a temp file — one `## <owner_name>` section per note — and
exports its path as `CANOPY_SPEC_NOTES_FILE`. A presence check like
`grep -q '^## Designer' "$CANOPY_SPEC_NOTES_FILE"` needs no `canopy`
binary inside a check command.

