---
title: MCP Tools
description: All 116 MCP tools exposed by the canopy daemon, by category.
order: 11
---

# MCP Tools

The daemon exposes **116 MCP tools** over Streamable HTTP (port 7755) and
stdio. Connect any MCP-capable AI CLI with `canopy mcp`.

Writes wait up to 5 seconds for a concurrent writer. If the database is still
busy after that, the call fails with `<tool>: database busy after 5 s — nothing
was written, retry`. Nothing changed, so retry the same call.

Graph-node sessions (agent and ensemble nodes launched by the graph engine) get
a fixed node profile of 18 tools in `tools/list` (`CANOPY_MCP_PROFILE=node`), so
every node stays under the 128-tool cap of OpenAI-compatible providers. Calls to
any other tool still work. All other sessions keep the full list.

## Agent management (18)

| Tool | Description |
|---|---|
| `agent_add` | Create a cron-scheduled background agent |
| `agent_watch` | Create a file-watcher-triggered agent |
| `agent_list` | List registered agents |
| `agent_get` | Get one agent's full stored definition as JSON, including the untruncated prompt |
| `agent_remove` | Remove an agent |
| `agent_enable` | Enable an agent |
| `agent_schedule_enable` | Schedule a one-shot enable at a future time |
| `agent_disable` | Disable an agent |
| `agent_run` | Run an agent immediately |
| `agent_status` | Daemon health and agent status |
| `agent_models` | List available models per CLI |
| `agent_logs` | Read execution logs |
| `agent_update` | Update an agent's definition |
| `agent_report` | Report execution status for scheduled tasks |
| `agent_probe` | Test platform+model liveness before graph_run |
| `agent_probe_recent` | Sweep recently-used platform+model pairs before a run |
| `subagent_spawn` | Launch an ephemeral subagent, asynchronously or blocking; the subagent runs with the given `workdir` as its working directory (default: the caller's project), and a missing or non-directory `workdir` is refused before anything spawns, naming the path |
| `subagent_collect` | Collect an ephemeral subagent result and mark it for cleanup |

## Multi-agent sync (4)

| Tool | Description |
|---|---|
| `sync_declare_intent` | Declare a mission with impact level |
| `sync_report_status` | Report workspace stability |
| `sync_broadcast` | Send info/query/answer messages |
| `sync_get_context` | Active missions, chatter, workspace vibe |

## Intelligence (8)

| Tool | Description |
|---|---|
| `intelligence_get_context` | Curated project context for session start |
| `intelligence_upsert` | Persist a fact, pattern or session summary |
| `intelligence_search` | Full-text search across nodes |
| `intelligence_graph_walk` | Traverse the knowledge graph |
| `intelligence_list_projects` | List known projects |
| `intelligence_link_projects` | Link projects with typed relations |
| `intelligence_delete_node` | Delete an intelligence node and its relations |
| `intelligence_delete_relation` | Delete a single relation by ID |

## Seed identity (5)

| Tool | Description |
|---|---|
| `get_identity` | Read the bound seed identity |
| `evolve_identity` | Refine the identity over time |
| `create_seed` | Create a seed identity |
| `list_seeds` | List seed identities |
| `remove_seed` | Remove a seed identity |

## Skills (2)

| Tool | Description |
|---|---|
| `skill_list` | List skills available from the dynamic skill store and configured catalogs; an installed skill that its git source no longer lists is marked `orphaned`, and its description starts with `[removed upstream]` |
| `skill_get` | Fetch a skill's full instructions by name, cloning or refreshing it on demand |

## Graph engine (45)

| Tool | Description |
|---|---|
| `graph_create` | Create a graph container. `require_spec_type: true` (default `false`) makes it refuse specs without a type |
| `graph_update` | Update graph metadata and hooks, including `require_spec_type` |
| `graph_add_spec` | Add an ordered spec to a graph. Supports `spec_type` and `spec_scope` to enable automatic code generation. A graph with `require_spec_type: true` refuses specs without a type |
| `graph_update_spec` | Update a graph spec. Accepts `spec_type` and `spec_scope` |
| `graph_remove_spec` | Unbind a spec from a graph, making it a standalone backlog spec; execution history is preserved. |
| `graph_add_node` | Add a node to a graph |
| `graph_update_node` | Update a graph node |
| `graph_add_edge` | Connect graph nodes |
| `graph_update_edge` | Update a graph edge |
| `graph_delete_edge` | Delete a graph edge |
| `graph_delete_node` | Delete a graph node |
| `graph_add_ensemble` | Add an ensemble to a graph |
| `graph_update_ensemble` | Update an ensemble |
| `graph_delete_ensemble` | Delete an ensemble |
| `crew_list` | List every crew: name, description, version, ordered members, and how many ensembles/nodes/hooks bind it |
| `crew_get` | Show one crew and every binding of it |
| `crew_create` | Create a crew from an ordered member list, or seed it from an ensemble's current members |
| `crew_update` | Update a crew's ordered members, description and/or name; bound ensembles pick it up at their next dispatch |
| `crew_delete` | Delete a crew (refused while any ensemble, node or hook still binds it) |
| `config_list` | List every setting with its group, kind, default, effective value, source (`default`/`file`/`env`), apply mode and status (`ok`, `invalid: <reason>`, `pending_restart`, `unknown`); a `config.toml` that will not parse appears as one `config.toml` row with `invalid: <reason>`; optional `group` filter |
| `config_get` | Read one setting by its dotted key (e.g. `clean.retention_days`) |
| `config_set` | Change one setting, validated against its type and range first (nothing is written on refusal); returns the new effective value and what the user must do — `applied`, `applies on next run`, `restart the daemon`, or `restart the TUI` |
| `config_reset` | Reset one setting to its registry default, through the same validating path as `config_set` |
| `graph_get` | Get a graph with its specs, nodes, and edges |
| `graph_list` | List graphs |
| `graph_crew` | List every graph's harness/model assignments — agent nodes outside ensembles, ensemble members and agent hooks with platform, model, timeout, crew and availability; `group_by: "pair"` summarises per platform/model pair. No prompt text is returned |
| `graph_run` | Run a graph (optionally `start_node_id` to start the first spec at a chosen node — a directed start clears that spec's execution count before it runs and the reply says `Execution limit reset for spec '<name>'`) |
| `graph_reset` | Reset a graph's pending specs; with `specs: [<id>, …]` exactly those specs are reset and each named spec's execution count (the per-spec failure limit) is zeroed, while a reset without `specs` keeps every count — the worktree is never touched. `clear_notes: true` also deletes the reset specs' spec notes (kept by default) |
| `graph_schedule_autorun` | Schedule a graph to resume or run |
| `graph_pause` | Pause a running graph |
| `graph_continue` | Continue a paused **or failed** graph (retry/skip), or start a spec at a chosen node with `run_from_node` (`node_id` names the node the spec starts at; `spec_id` names the spec and defaults to the graph's current spec — in a queue-driven graph: a running/interrupted member, else a failed one, else the next pending one). `retry_current_node` also resumes a paused graph whose current spec is `pending` with a recorded node (the state daemon recovery leaves after a reboot mid-node). `retry_current_node` and `run_from_node` (like `graph_run`'s `start_node_id`) clear the targeted spec's execution count before it runs and the reply says `Execution limit reset for spec '<name>'`; automatic re-runs keep the count. Any other graph status is refused naming the status and the ways out (`run_from_node` with `spec_id`, or `graph_reset` with `specs: [<spec id>]` then `graph_run`). The launch is refused, naming the node, if an upstream node's latest run failed, errored or was interrupted. On an idea-driven graph (one run with an `idea`), `run_from_node` and `retry_current_node` resume that idea run — `{{spec_content}}` renders the stored idea and `spec_id` defaults to the run's blank-name bookkeeping spec; an idea graph that has never run is refused, naming `graph_run` with an `idea` as the way to start one |
| `graph_complete_node` | Complete an active graph node |
| `graph_report_blocker` | Report a blocker for an active graph node |
| `graph_export` | Export a graph design |
| `graph_import` | Import a graph design |
| `graph_archive` | Archive a graph |
| `graph_restore` | Restore an archived graph |
| `graph_node_runs_list` | List a graph's node run history |
| `graph_node_run_get` | Fetch a graph node run's full input and output |
| `graph_events` | Read a graph's durable event log (oldest-first, with `since_id` paging) |
| `graph_copy_node` | Copy a graph node configuration |
| `graph_copy_ensemble` | Copy an ensemble configuration |
| `graph_audit_node_configs` | Audit graph node configurations |
| `graph_schedule_continue` | Schedule a paused graph to continue |
| `graph_preflight` | Probe graph agent platforms and models before running |

See [Graphs](graphs.md).

<!-- RETIRED-VOCAB-BEGIN: 2.x alias history — this subsection names the retired term on purpose. -->

### Deprecated names (2.x)

Kept callable for the whole 3.x line, but excluded from tool listings
(`tools/list`) so nothing new adopts them — call the 3.x name instead. Not
counted in the 116 tools above.

| Deprecated name | Renamed to (3.0.0) |
|---|---|
| `loop_complete_node` | `graph_complete_node` |
| `loop_report_blocker` | `graph_report_blocker` |
| `loop_schedule_autorun` | `graph_schedule_autorun` (also still accepts a `loop_id` argument as `graph_id`) |

A successful call through any of these three carries an extra `deprecated`
field naming the rename. No other `loop_*` tool has a 3.x alias — every
other 2.x `loop_*` MCP tool name and the `canopy loop` CLI subcommand
(now `canopy graph`) are gone for good; `graph_preflight` and `canopy
doctor` both warn if a stored prompt or hook still mentions one.

<!-- RETIRED-VOCAB-END -->

### Hook placeholders and shell safety

Command hooks substitute event-bound `{{...}}` markers as POSIX shell-quoted
single-quoted literals: each value is wrapped in `'...'`, with internal `'`
escaped as `'\''`. This keeps values containing shell syntax in one safe word;
`'{{spec_name}}'` and `{{spec_name}}` both remain valid. The same values are
available as `CANOPY_HOOK_LOOP_NAME` (alias `CANOPY_HOOK_GRAPH_NAME`),
`CANOPY_HOOK_WORKDIR`, `CANOPY_HOOK_SPEC_NAME`, `CANOPY_HOOK_SPEC_ID`,
`CANOPY_HOOK_COMPLETED_SPECS`, `CANOPY_HOOK_NODE`, `CANOPY_HOOK_BLOCKER`, and
`CANOPY_HOOK_EVENT` environment variables, so hooks can use
`"$CANOPY_HOOK_SPEC_NAME"` without interpolation.
Agent, interactive, and graph-idea hooks substitute the same markers without
shell quoting. A known marker with no value for the event renders `(none)` in
all hook modes. Unknown marker names are rejected during `graph_update`, so a
hook never fires with a marker left literal.

An interactive hook targets a session by `target_session_id` or by
`target_session_name` (exactly one) — see [Graphs](graphs.md#event-keyed-hooks)
for how name resolution and its failure modes work. `session_list` remains
the way to see both the names and ids of every live session a hook can
target.

## Availability roster (4)

The roster declares which platform/model pairs may run. The engine consults
it at dispatch — an unavailable member is skipped, an unavailable single node
or agent hook blocks the spec — and never infers availability from failures,
exit codes or quota messages. Only these tools (and the CLI, which calls them)
write the table.

| Tool | Description |
|---|---|
| `roster_set` | Declare one `platform`/`model_pattern` pair `available` or `unavailable`. `model_pattern` is omitted for every model of the platform, or an exact model id, or a prefix ending in `*` (e.g. `opencode-go/*`). `until` (RFC 3339 UTC) is accepted only with `unavailable` and must be in the future |
| `roster_clear` | Delete one entry (idempotent); omit `model_pattern` for the platform-wide entry |
| `roster_list` | Every entry, filterable by `platform`, each with its effective state now |
| `roster_check` | The effective availability of one `platform`/`model` pair, naming the entry that decided it |

The most specific entry wins: exact model, then the longest `*` prefix, then the
platform-wide entry. An `unavailable` entry past its `until` reads as available
(expired) — expiry is evaluated at read time, never by a timer.

## Named mutexes (1)

A node (agent, check, gate) or ensemble declares the shared resources it
needs with `mutex: ["cpu"]`; the engine grants each resource to one node run
at a time across all graphs of the daemon, and the others wait in a visible
FIFO queue without burning their timeout. Each name must match
`^[a-z][a-z0-9-]{0,31}$`. The lease is held per node run (per member run for
an ensemble) and released when the run ends for any reason.

| Tool | Description |
|---|---|
| `mutex` | Action `list`: every granted lease (resource, holder run/graph/node, `acquired_at`) and every queued waiter (run/graph/node, resources, `requested_at`) in FIFO order |

## Spec backlog (9)

Every tool that takes a `spec_id` also accepts the spec's code (e.g. `FIX-7`),
resolved within the caller's or the graph's workdir.

| Tool | Description |
|---|---|
| `spec_create` | Create a standalone spec (not bound to any graph). Supports `spec_type` (Conventional Commits type: `feat`, `fix`, etc.) and `spec_scope` (e.g. `tui`) to enable automatic code generation and deterministic commit messages. A conventional name such as `fix(tui): open F1` sets the type and scope and stores the subject alone. A spec created without a type still succeeds but returns a `warning`: it will have no code and no commit message |
| `spec_list` | List specs, filterable by workdir, status, and `spec_type` |
| `spec_update` | Update a spec's name, description, workdir tag, `spec_type`, or `spec_scope` |
| `spec_delete` | Delete an unbound spec |
| `spec_set_status` | Admin transition: complete, skip, or reopen a standalone spec |
| `spec_section_get` | Extract one canonical section (`<objective>`, `<constraints>`, …) from a spec body |
| `spec_convert` | Convert legacy heading-format spec bodies to the tagged `<spec>` format (skips tagged and running specs) |
| `spec_note_write` | Write or replace the caller's note for the current spec: `run_id` + `body` resolves the owner from the active run (node, or its ensemble when a member) and refuses a finished run; `spec_id` + `body` with no `run_id` writes the `operator` note. Body ≤ 2 500 chars |
| `spec_notes_get` | Read every note of a spec (`spec_id`), newest first, with owner, author model, `updated_at` and body |

Spec bodies use the tagged `<spec>` format with all seven canonical section
tags — `<objective>`, `<functional_requirements>`,
`<non_functional_requirements>`, `<constraints>`, `<guidelines>`, `<in_scope>`,
`<out_of_scope>` — with markdown inside each section. Writes that do not parse
are rejected naming the offending tag; legacy heading-format bodies remain
readable until `spec_convert` migrates them.

## Spec queues (6)

| Tool | Description |
|---|---|
| `queue_create` | Create an ordered queue of specs |
| `queue_add_spec` | Append a spec to a queue — a grouped add lands after the group's last pending member |
| `queue_list` | List a queue's members, or all queues |
| `queue_remove_spec` | Remove a spec from a queue |
| `queue_reorder` | Full replacement of a queue's order |
| `queue_set_group` | Set or clear a member's context group |

## Node blueprints (3)

| Tool | Description |
|---|---|
| `blueprint_list` | List all builtins and custom blueprints |
| `blueprint_create` | Create a reusable node config template |
| `blueprint_delete` | Delete a custom blueprint (builtins protected) |

## Project (4)

| Tool | Description |
|---|---|
| `project_search` | Search the project registry |
| `project_update` | Update a project's registry entry |
| `project_remap` | Remap a moved/renamed workdir instead of orphaning history |
| `project_register` | Explicitly register a project directory (no marker file needed) |

## RAG (2)

| Tool | Description |
|---|---|
| `rag_search` | Semantic search over indexed documents (rate-limited) |
| `rag_compact` | Compact the vector store now: merge fragments and prune old versions |

## Platform registry (1)

| Tool | Description |
|---|---|
| `registry_refresh` | Fetch the platform registry and apply it to config.toml now, ignoring the 24-hour interval. Takes no parameters; returns the report as JSON — `changed` and `kept` per platform, the `added`, `updated` and `deleted` platform names, and `up_to_date` |

## Protocol (1)

| Tool | Description |
|---|---|
| `get_tools` | Scope-sensitive action protocols with recommended tool sets; the `session_start` scope also returns the caller's own session id |

## Interactive sessions (1)

| Tool | Description |
|---|---|
| `session_list` | List live interactive sessions with the exact ids an interactive graph hook's `target_session_id` accepts, each with platform, workdir, name, and whether a TUI is attached; pass `session_id` to look one up |

## Scheduled sends (3)

| Tool | Description |
|---|---|
| `scheduled_send_create` | Schedule a prompt for future delivery to a session (own session by default) |
| `scheduled_send_list` | List a session's pending scheduled sends |
| `scheduled_send_cancel` | Cancel a pending scheduled send by id |
