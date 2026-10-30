---
title: Template Variables
description: Placeholders in agent prompts and check commands.
order: 8
---

# Template Variables

Agent node prompts support these placeholders:

| Variable | Expands to |
|---|---|
| `{{graph_name}}` | The graph's name |
| `{{workdir}}` | The graph's working directory |
| `{{spec_id}}` | The spec's unique ID |
| `{{spec_name}}` | The spec's name |
| `{{spec_content}}` | The spec's description (falls back to name) |
| `{{spec_code}}` | Engine-generated quick reference (e.g. `FIX-7`) — empty on legacy specs without a type |
| `{{spec_type}}` | Conventional Commits type (`feat`, `fix`, etc.) — empty on legacy specs without a type |
| `{{spec_scope}}` | Optional scope (e.g. `tui`) — empty when absent |
| `{{spec_commit_message}}` | Prebuilt commit message (`fix(tui): open F1`) — empty on legacy specs without a type |
| `{{node_id}}` | The node's unique ID |
| `{{previous_feedback}}` | JSON output of the previous node (truncated if oversized) |
| `{{output:<node>}}` | The recorded output of another node in the same graph, by node name |

Outputs are read from nodes, never from ensembles: `{{output:<name>}}`
naming an ensemble is refused, and the message names the ensemble's quorum
node when one exists — that node is the source meant instead. Any other
placeholder nothing can bind is refused when the template is **written** —
`graph_node` `add`, `graph_node` `update`, `graph_ensemble` `add` and
`graph_ensemble` `update` (the shared prompt and every member
`prompt_override`), and `graph_edit` `import` (all-or-nothing) — naming the node
or ensemble and the placeholder, plus the forms it accepts when the fix is
a different marker, instead of failing member by member at run time. A
marker meant as literal text stays
accepted when escaped as `\{{...}}`. A `prompt_preset` is a file name, not
text, so it keeps its spawn-time check. A `router`'s `prompt_template` binds
only `{{previous_feedback}}`, `{{previous_output}}`, `{{spec_content}}`
and `{{spec_name}}`: it reads no node output, so an `{{output:…}}` marker
there is refused as well.

A template stored before this validation is reported by `graph_control` `preflight`
in `graph_warnings`, one entry per offending placeholder naming the node or
ensemble — advisory only, since the spawn-time refusal still applies. The
exception is an unknown `{{output:…}}` target in a graph node's
`prompt_template`, which preflight has always refused as an error.

## Check node commands

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
| `{{spec_committed_head}}` | The git HEAD left behind by the last `commit_rights: true` node's own execution during *this* spec run, empty until one actually commits (see [Commit rights](graphs-execution.md#commit-rights)) | raw — a hex SHA |

The shell-quoted markers expand to a single POSIX-quoted shell word, so a
name containing spaces or quotes can never break or inject into the
command. Write them bare (`{{spec_name}}`) or already single-quoted
(`'{{spec_name}}'` — the engine strips that exact wrapper so the quoting
is not doubled), never inside other quotes; a `\{{marker}}` escape stays
literal text. The raw markers expand unquoted exactly as before.

Any other `{{…}}` marker fails the check node before the command runs: the
run records `unknown marker {{x}} in check command` and nothing reaches
the shell. `graph_control` `preflight` reports the same defect as an error before
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
| `CANOPY_SPEC_CODE` | Engine-generated quick reference (e.g. `FIX-7`) — empty on legacy specs without a type |
| `CANOPY_SPEC_TYPE` | Conventional Commits type (`feat`, `fix`, etc.) — empty on legacy specs without a type |
| `CANOPY_SPEC_SCOPE` | Optional scope (e.g. `tui`) — empty when absent |
| `CANOPY_SPEC_COMMIT_MESSAGE` | Prebuilt commit message (`fix(tui): open F1`) — empty on legacy specs without a type |

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

`spec_end_dirty`/`spec_end_dirty_paths` (readable via `graph` `get`/
`graph` `node_runs`, not a template placeholder) record the same count and
the first 20 paths at the moment the spec's attempt actually ends —
`Completed`, `Failed`, or `Interrupted`. A spec that ends `Failed` or
`Interrupted` with a dirty tree folds a one-line note onto its own failure
summary/blocker text and (for a plain `Failed` ending) onto the `on_failed`
hook's `{{node}}` value.

`graph_control` `run` and the autorun scheduler refuse to launch the next spec when the
workdir is currently dirty and a *different* spec of the same graph — or a
member of its active queue run — ended `Failed` or `Interrupted` with
recorded dirty paths that overlap the ones dirty now. Records from other
graphs and from standalone backlog specs are ignored, so a stale row
elsewhere on the workdir never blocks a spec resuming its own work.
`Completed` specs never block, and neither do `Skipped` ones: a
skipped spec never ran, so its stale record must not make a later spec's own
dirt look ambiguous. The comparison uses the (first 20) paths each spec
recorded at its end, so a record naming files that are no longer dirty never
blocks. A spec resuming interrupted work of its own proceeds when the dirty
paths are exactly the ones its own latest run recorded. The warning names the
spec whose last recorded run in **this graph** left the workdir dirty
(`graph_runs` joined to `spec_end_dirty > 0`); when no run of this graph
recorded dirt the warning says `dirty before this graph ran` and names no
spec. A resuming spec is still refused while another spec of the same graph
(or active queue run) left overlapping dirt on the workdir: the refusal
names the blocking predecessor — its name (id when
the name is empty), the graph it ran in, its end status and end time — and,
when the next spec is resuming its own dirt, says so in the same line. Finish,
reset or remove that blocking spec (or set `allow_dirty_start`) to proceed.
When the launch proceeds, the warning says why — `allow_dirty_start` is
set on the graph, or the next spec is resuming the dirty work it left itself —
instead of telling you to set a flag. When it blocks, the message says
`not launched` before the `Set allow_dirty_start` hint. Set
`allow_dirty_start: true` via `graph_edit` `update` to downgrade a refusal to a
warning. `graph_control` `preflight` separately reports a dirty workdir as a warning
unconditionally, with no "previous spec" logic — call it before `graph_control` `run` to
see this regardless of history. A queue run applies the same guard between
its specs, not only at launch — see [Spec queues](graphs-hooks-and-queues.md#spec-queues).

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

