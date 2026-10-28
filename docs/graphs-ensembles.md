---
title: Ensembles
description: Parallel, cascade and round-robin member groups, crews, and availability.
order: 9
---

# Ensembles

An **ensemble** is a group of `agent` nodes that receive the same prompt,
plus a quorum that waits for every member before routing onward. A
`parallel` ensemble runs 2–8 members at once by default (up to 16 if you
raise `graphs.ensemble_max_members_parallel`). A `cascade` or `round_robin`
ensemble runs its members one at a time, so it takes 2–16 by default (up to
32 with `graphs.ensemble_max_members_sequential`). Both caps are listed in
[Tuning the engine](graphs-operations.md#tuning-the-engine). It replaces what would otherwise be N member
nodes, N prompts, and 2N+2 edges wired by hand — `graph_ensemble` `add`
creates the whole unit in one call:

```json
graph_ensemble {
  action: "add",
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

`graph_ensemble` `update` changes the shared prompt
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
remove-then-add cycle. `graph_control` `preflight` reports incomplete source reachability
with the source and member counts before `graph_control` `run` spends probe quota.
`graph_ensemble` `delete` removes the whole unit. `graph` `get` returns the
ensemble as one unit (`ensemble_id`, members, quorum config, every entry
source) alongside its expanded nodes.

`on_fail_to = null` (the default when an ensemble is created without one)
ends the spec when the quorum fails: a member that reports FAIL — even with
partial work — leaves the quorum with nowhere to route, so the run stops
there and the graph ends `failed` instead of routing the failure onward. This
is sometimes deliberate, so it is never an error; `graph_control` `preflight` warns
about it in `graph_warnings` for every ensemble whose quorum has no
`fail`/`always` exit. Set `on_fail_to` with `graph_ensemble` `update`, or add
the edge with `graph_edge` `add`, when a failed quorum should route somewhere.

The quorum's consolidated output is one document with a
`## <platform/model> [pass|fail]` section per member, in member order. The
quorum reports pass when at least `min_pass` members passed. Member
execution is capped by a global concurrency limit
(`ensemble_concurrency_cap`, default 4, counted across every graph) so an
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
**parallel-only** — `graph_ensemble` `add`/`graph_ensemble` `update` refuse
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

## Availability at dispatch

The roster (`harness` `roster_set`, or `canopy roster set` — see the
[CLI reference](cli-reference.md) and [MCP tools](mcp-tools.md)) declares a
platform/model pair — or a whole platform — out for a while. At dispatch an
ensemble simply skips members whose pair is effectively unavailable: a
`parallel` quorum spawns (and counts) only the available members, a
`cascade` falls through them in order, and a `round_robin` rotation steps
over them without leaving the full member list's index space, so a member
that comes back is picked up where the rotation already is. Nothing is
edited — the member list stays as written, the filter is re-evaluated at
every dispatch (a pair declared out mid-run is skipped from its next
dispatch on), and an entry past its `until` simply reads as available
again.

An ensemble blocks instead of dispatching when the roster leaves it
unrunnable: every member unavailable (any kind), or a `parallel` ensemble
with fewer available members than `min_pass`. A single `agent` node or
agent hook whose pair is unavailable blocks the spec the same way. These
blocks go through the ordinary blocked path — the graph pauses with a
blocker naming the pair and its reason, `on_blocked` fires, and no attempt
is spent, so an availability block never burns the attempt ceiling; clear
the entry (or wait out its `until`) and relaunch. `graph_control` `preflight` never
probes an unavailable pair: it lists them under `skipped_unavailable` and
counts an ensemble the roster would block as `would_fail`.

## Crews

A **crew** is a named, ordered list of members — `(platform, model, timeout,
infra-retry budget)` — kept in one place, so the same "designer crew" or
"implementer crew" is written once instead of copied into every graph. The
tool is `crew` with actions `list`, `get`, `create` (`members` or
`from_ensemble`), `update` and `delete`; the CLI mirrors them as
`canopy crew list | show | create --from-ensemble | delete` (member editing
is MCP/TUI only). `from_ensemble` refuses an ensemble whose members carry a
per-member `prompt_override` — a review panel with its own angles stays
literal.

An ensemble binds to a crew with `graph_ensemble` `add` (`crew`) or
`graph_ensemble` `update` (`crew: "name"`); `crew: null` unbinds and keeps the
current member rows as literal members. A crew change is its own call — it
is never combined with a literal `members` list (or any other ensemble
field) in one update. A bound ensemble renders the shared
`prompt_template` for every member. Binding replaces the member rows through
the one sync path and clamps `min_pass` to the crew size. Member order is
meaningful: `cascade` tries it in order, `round_robin` rotates through it,
and a crew-bound `agent` node takes the first member the roster reports
available.

Editing a crew reaches every bound ensemble at its next dispatch: the engine
re-syncs a bound ensemble's members just before it dispatches (never while a
member is in flight), and `crew` `update` also syncs every bound ensemble whose
graph is not `running`/`pausing` immediately. A node or hook names a crew in
its config (`{"crew": "name"}`) instead of `platform`; `crew` `delete` is
refused while anything still binds the crew and the error lists every
binding.

## No-verdict blocks

A parallel or round-robin ensemble that exits with no verdict and no
outgoing `Error` edge cannot route — every member infra-failed or walked
off without filing a verdict. Rather than failing the spec, the graph
pauses with a blocker that names the ensemble and the member reasons,
and the spec is marked `interrupted` (not `failed`). That makes the
block resumable in place: `graph_control` `continue` with `mode: retry_current_node` re-enters
the ensemble at its join cursor (already-passed nodes such as the entry
node are not re-run), and `graph_ensemble` `update` lets you rewire the
members before retrying. A `graph_control` `run` on a blocked (paused + blocker)
graph refuses and reports the blocker, so a blind relaunch never masks a
crew that is silently filing nothing; clear the blocker first, or
`graph_control` `reset` the spec. A **directed run** —
`graph_control` `continue` with `mode: run_from_node` (or `graph_control` `run` with
`start_node_id`) — is the general
escape hatch: it starts a spec at a node you choose, reusing the recorded
outputs of every earlier node, and accepts a paused, completed or failed
graph as long as it has no active run.

