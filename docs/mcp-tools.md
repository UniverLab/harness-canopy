---
title: MCP Tools
description: All 95 MCP tools exposed by the canopy daemon, by category.
order: 11
---

# MCP Tools

The daemon exposes **95 MCP tools** over Streamable HTTP (port 7755) and
stdio. Connect any MCP-capable AI CLI with `canopy mcp`.

## Agent management (17)

| Tool | Description |
|---|---|
| `agent_add` | Create a cron-scheduled background agent |
| `agent_watch` | Create a file-watcher-triggered agent |
| `agent_list` | List registered agents |
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
| `subagent_spawn` | Launch an ephemeral subagent, asynchronously or blocking |
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
| `skill_list` | List skills available from the dynamic skill store and configured catalogs |
| `skill_get` | Fetch a skill's full instructions by name, cloning or refreshing it on demand |

## Graph engine (34)

| Tool | Description |
|---|---|
| `graph_create` | Create a graph container |
| `graph_update` | Update graph metadata and hooks |
| `graph_add_spec` | Add an ordered spec to a graph |
| `graph_update_spec` | Update a graph spec |
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
| `graph_get` | Get a graph with its specs, nodes, and edges |
| `graph_list` | List graphs |
| `graph_run` | Run a graph |
| `graph_reset` | Reset a graph's pending specs |
| `graph_schedule_autorun` | Schedule a graph to resume or run |
| `graph_pause` | Pause a running graph |
| `graph_continue` | Continue a paused graph |
| `graph_complete_node` | Complete an active graph node |
| `graph_report_blocker` | Report a blocker for an active graph node |
| `graph_export` | Export a graph design |
| `graph_import` | Import a graph design |
| `graph_archive` | Archive a graph |
| `graph_restore` | Restore an archived graph |
| `graph_node_runs_list` | List a graph's node run history |
| `graph_node_run_get` | Fetch a graph node run's full input and output |
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
counted in the 95 tools above.

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

## Spec backlog (7)

| Tool | Description |
|---|---|
| `spec_create` | Create a standalone spec (not bound to any graph) |
| `spec_list` | List specs, filterable by workdir and status |
| `spec_update` | Update a spec's name, description, or workdir tag |
| `spec_delete` | Delete an unbound spec |
| `spec_set_status` | Admin transition: complete, skip, or reopen a standalone spec |
| `spec_section_get` | Extract one canonical section (`<objective>`, `<constraints>`, …) from a spec body |
| `spec_convert` | Convert legacy heading-format spec bodies to the tagged `<spec>` format (skips tagged and running specs) |

Spec bodies use the tagged `<spec>` format with all seven canonical section
tags — `<objective>`, `<functional_requirements>`,
`<non_functional_requirements>`, `<constraints>`, `<guidelines>`, `<in_scope>`,
`<out_of_scope>` — with markdown inside each section. Writes that do not parse
are rejected naming the offending tag; legacy heading-format bodies remain
readable until `spec_convert` migrates them.

## Spec queues (5)

| Tool | Description |
|---|---|
| `queue_create` | Create an ordered queue of specs |
| `queue_add_spec` | Append a spec to the end of a queue |
| `queue_list` | List a queue's members, or all queues |
| `queue_remove_spec` | Remove a spec from a queue |
| `queue_reorder` | Full replacement of a queue's order |

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

## RAG (1)

| Tool | Description |
|---|---|
| `rag_search` | Semantic search over indexed documents (rate-limited) |

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
