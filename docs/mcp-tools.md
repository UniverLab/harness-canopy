---
title: MCP Tools
description: All 88 MCP tools exposed by the canopy daemon, by category.
order: 11
---

# MCP Tools

The daemon exposes **88 MCP tools** over Streamable HTTP (port 7755) and
stdio. Connect any MCP-capable AI CLI with `canopy mcp`.

## Agent management (15)

| Tool | Description |
|---|---|
| `agent_add` | Create a cron-scheduled background agent |
| `agent_watch` | Create a file-watcher-triggered agent |
| `agent_list` | List registered agents |
| `agent_remove` | Remove an agent |
| `agent_enable` / `agent_disable` | Toggle an agent |
| `agent_schedule_enable` | Schedule a one-shot enable at a future time |
| `agent_run` | Run an agent immediately |
| `agent_status` | Daemon health and agent status |
| `agent_models` | List available models per CLI |
| `agent_logs` | Read execution logs |
| `agent_update` | Update an agent's definition |
| `agent_report` | Report execution status for scheduled tasks |
| `agent_probe` | Test platform+model liveness before graph_run |
| `agent_probe_recent` | Sweep recently-used platform+model pairs before a run |

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
| `create_seed` / `list_seeds` / `remove_seed` | Manage the seed nursery |

## Graph engine (33)

`graph_create`, `graph_update`, `graph_add_spec`,
`graph_update_spec`, `graph_add_node`, `graph_update_node`,
`graph_add_edge`, `graph_update_edge`, `graph_delete_edge`,
`graph_delete_node`, `graph_add_ensemble`,
`graph_update_ensemble`, `graph_delete_ensemble`, `graph_get`,
`graph_list`, `graph_run`, `graph_reset`, `graph_schedule_autorun`,
`graph_pause`, `graph_continue`,
`graph_complete_node`, `graph_report_blocker`,
`graph_export`, `graph_import`, `graph_archive`, `graph_restore`,
`graph_node_runs_list`, `graph_node_run_get`,
`graph_copy_node`, `graph_copy_ensemble`,
`graph_audit_node_configs`, `graph_schedule_continue`,
`graph_preflight` — see [Graphs](graphs.md).

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
Agent and interactive prompt hooks keep literal marker substitution and are not
shell-quoted.

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
