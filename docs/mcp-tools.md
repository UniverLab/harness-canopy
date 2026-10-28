---
title: MCP Tools
description: All 57 MCP tools exposed by the canopy daemon, by category.
order: 11
---

# MCP Tools

The daemon exposes **57 MCP tools** over Streamable HTTP (port 7755) and
stdio. Connect any MCP-capable AI CLI with `canopy mcp`.

Writes wait up to 5 seconds for a concurrent writer. If the database is still
busy after that, the call fails with `<tool>: database busy after 5 s — nothing
was written, retry`. Nothing changed, so retry the same call.

Graph-node sessions (agent and ensemble nodes launched by the graph engine) get
a fixed node profile of 17 tools in `tools/list` (`CANOPY_MCP_PROFILE=node`), so
every node stays under the 128-tool cap of OpenAI-compatible providers. Calls to
any other tool still work. All other sessions keep the full list.

## Agent management (5)

| Tool | Description |
|---|---|
| `agent` | Read agents, logs, and daemon health. Actions: `list`, `get` (needs `id` — the full stored definition as JSON, including the untruncated prompt), `logs` (`id`, optional `lines`/`since`), `status` — pass `action` plus the params the refusal/table lists |
| `agent_edit` | Create, change, and run agents. Actions: `add` (`id`, `prompt`, `schedule` — a 5-field cron expression), `watch` (`id`, `path`, `events`, `prompt`), `update`, `remove`, `enable`, `disable`, `schedule` (`id`, `at` — a one-shot enable at a future ISO 8601 time), `run` — the old one-operation agent tools, now one tool with an `action` parameter and the same per-action params, except `action` itself, renamed `delivery_action` (`headless` or `send_to_session`) |
| `agent_report` | Report execution status for scheduled tasks — the only way to report a run |
| `subagent_spawn` | Launch an ephemeral subagent, asynchronously or blocking; the subagent runs with the given `workdir` as its working directory (default: the caller's project), and a missing or non-directory `workdir` is refused before anything spawns, naming the path |
| `subagent_collect` | Collect an ephemeral subagent result and mark it for cleanup |

## Harness (1)

| Tool | Description |
|---|---|
| `harness` | Inspect model catalogs, probe platform liveness, refresh the platform registry, and manage the availability roster. Actions: `models` (optional `platform`/`refresh`/`full` — every returned model id copies verbatim into a model field), `probe` (test platform+model liveness before a `graph_control` `run`; the verdict reads the response content, not the exit code), `probe_recent` (sweep recently-used platform+model pairs before a run), `refresh_registry` (fetch the platform registry and apply it to config.toml now, ignoring the 24-hour interval; takes no parameters and returns the report as JSON — `changed` and `kept` per platform, the `added`, `updated` and `deleted` platform names, and `up_to_date`), `roster_list`, `roster_set`, `roster_clear`, `roster_check` — pass `action` plus the params the refusal/table lists. Probes spend real tokens/quota: call them explicitly, never automatically or on a schedule |

The roster declares which platform/model pairs may run. The engine consults
it at dispatch — an unavailable member is skipped, an unavailable single node
or agent hook blocks the spec — and never infers availability from failures,
exit codes or quota messages. Only the `roster_*` actions (and the CLI, which calls them)
write the table.

The most specific entry wins: exact model, then the longest `*` prefix, then the
platform-wide entry. An `unavailable` entry past its `until` reads as available
(expired) — expiry is evaluated at read time, never by a timer.

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

## Graph engine (8)

| Tool | Description |
|---|---|
| `graph` | Read graphs, specs, node runs, and events. Actions: `list`, `get`, `export`, `validate`, `audit`, `events`, `node_runs`, `node_run` — pass `action` plus the params the refusal/table lists (e.g. `get` needs `graph_id`; `node_run` needs `run_id`) |
| `graph_edit` | Create, change, and archive graphs and their specs. Actions: `create`, `update`, `archive`, `restore`, `import`, `install`, `attach_spec`, `detach_spec`, `update_spec` — the old one-operation graph authoring tools, now one tool with an `action` parameter and the same per-action params |
| `graph_node` | Author graph nodes. Actions: `add` (kind+config, blueprint, or node document + `config_overrides`), `update`, `delete`, `copy`. Never completes a node or reports a blocker — node sessions keep using `graph_complete_node` / `graph_report_blocker` |
| `graph_edge` | Wire graph nodes. Actions: `add`, `update`, `delete` (params unchanged: `from_node`/`to_node`/`condition`/`route`, `edge_id`, …) |
| `graph_ensemble` | Author multi-member ensembles. Actions: `add`, `update`, `delete`, `copy` (params unchanged: `members`, `min_pass`, `on_pass_to`, …) |
| `graph_control` | Run and manage graph execution. Actions: `run`, `pause`, `continue` (required `mode`: retry_current_node / skip_next_spec / run_from_node), `reset`, `preflight`, `schedule_continue` (optional `mode`: retry_current_node / skip_next_spec), `schedule_autorun` |
| `graph_complete_node` | Complete an active graph node |
| `graph_report_blocker` | Report a blocker for an active graph node |

## Crews (1)

| Tool | Description |
|---|---|
| `crew` | Manage crews and read every graph's harness/model assignments. Actions: `list` (every crew with version, ordered members and binding counts), `get` (one crew and every binding of it), `create` (from an ordered member list, or seeded from an ensemble's members — pass exactly one of `members` or `from_ensemble`), `update` (replace the member list, set the description, and/or rename; bound ensembles pick it up at their next dispatch), `delete` (refused while any ensemble, node or hook still binds it), `assignments` (every graph's harness/model assignments with platform, model, timeout, crew and availability; `group_by: "pair"` summarises per platform/model pair — no prompt text is returned) — pass `action` plus the params the refusal/table lists |

## Configuration (1)

| Tool | Description |
|---|---|
| `config` | Read and change canopy settings. Actions: `get` (one setting by its dotted key, e.g. `clean.retention_days`), `list` (every setting with group, kind, default, effective value, source, apply mode and status; optional `group` filter), `set` (validated against the setting's type and range first — nothing is written on refusal; returns the new effective value and what the user must do: `applied`, `applies on next run`, `restart the daemon`, or `restart the TUI`), `reset` (one setting back to its registry default) — pass `action` plus the params the refusal/table lists |

Every authoring tool that stores prompt text — `graph_node` `add`,
`graph_node` `update`, `graph_ensemble` `add`, `graph_ensemble` `update`,
`graph_edit` `import` — refuses a template whose `{{…}}` placeholder nothing can
bind, naming the node or ensemble, the placeholder and the forms accepted;
`{{output:<name>}}` must name a node in the same graph, never an ensemble
(use its quorum node), and a literal marker must be escaped as `\{{...}}`.
`graph_control` `preflight` reports templates written before this validation as
`graph_warnings`.

See [Graphs](graphs.md).

<!-- RETIRED-VOCAB-BEGIN: 2.x alias history — this subsection names the retired term on purpose. -->

### Deprecated names (2.x)

Kept callable for the whole 3.x line, but excluded from tool listings
(`tools/list`) so nothing new adopts them — call the 3.x name instead. Not
counted in the 57 tools above.

| Deprecated name | Renamed to (3.0.0) |
|---|---|
| `loop_complete_node` | `graph_complete_node` |
| `loop_report_blocker` | `graph_report_blocker` |

A successful call through either carries an extra `deprecated`
field naming the rename. No other `loop_*` tool has a 3.x alias — every
other 2.x `loop_*` MCP tool name and the `canopy loop` CLI subcommand
(now `canopy graph`) are gone for good; `graph_control` `preflight` and `canopy
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
all hook modes. Unknown marker names are rejected during `graph_edit` `update`, so a
hook never fires with a marker left literal.

An interactive hook targets a session by `target_session_id` or by
`target_session_name` (exactly one) — see [Graphs](graphs.md#event-keyed-hooks)
for how name resolution and its failure modes work. `session_list` remains
the way to see both the names and ids of every live session a hook can
target.

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

## Spec backlog (3)

Every tool that takes a `spec_id` also accepts the spec's code (e.g. `FIX-7`),
resolved within the caller's or the graph's workdir.

| Tool | Description |
|---|---|
| `spec` | Read specs, sections, and notes. Actions: `list` (filterable by workdir, status, and `spec_type`, with limit/offset pagination), `get` (one spec with its full description), `section` (one canonical section from a spec body), `notes` (every note of a spec, newest first, with owner, author model, `updated_at` and body) — pass `action` plus the params the refusal/table lists |
| `spec_edit` | Create, change, and transition specs. Actions: `create` (a standalone spec — supports `spec_type`/`spec_scope` for automatic code generation and deterministic commit messages; a conventional name such as `fix(tui): open F1` sets the type and scope and stores the subject alone, while an untyped create still succeeds but returns a `warning`), `update`, `delete` (refused while bound to a graph), `set_status` (admin transition: complete, skip, or reopen), `note_write` (`run_id` + `body`, or `spec_id` + `body` for the `operator` note; body ≤ 2 500 chars) — the old one-operation spec tools, now one tool with an `action` parameter and the same per-action params |
| `spec_convert` | Convert legacy heading-format spec bodies to the tagged `<spec>` format (skips tagged and running specs) |

Spec bodies use the tagged `<spec>` format with all seven canonical section
tags — `<objective>`, `<functional_requirements>`,
`<non_functional_requirements>`, `<constraints>`, `<guidelines>`, `<in_scope>`,
`<out_of_scope>` — with markdown inside each section. Writes that do not parse
are rejected naming the offending tag; legacy heading-format bodies remain
readable until `spec_convert` migrates them.

## Spec queues (1)

| Tool | Description |
|---|---|
| `queue` | Author ordered spec queues. Actions: `create`, `list` (a queue's members, or all queues), `add_spec` (a grouped add lands after the group's last pending member), `remove_spec`, `reorder` (full replacement of the order), `set_group` — pass `action` plus the params the refusal/table lists |

## Node blueprints (3)

| Tool | Description |
|---|---|
| `blueprint_list` | List all builtins and custom blueprints |
| `blueprint_create` | Create a reusable node config template |
| `blueprint_delete` | Delete a custom blueprint (builtins protected) |

## Node documents (3)

| Tool | Description |
|---|---|
| `node_list` | List installed node documents and the builtins embedded in the binary, marking each `installed` or `builtin` |
| `node_install` | Install a node document from the catalogue by ref (`<slug>`, `<owner>/<repo>/<slug>`, each with an optional `@<ref>`, or `draft:<id>` for a one-use editor draft), with its bundled preset — `overwrite_presets?`, `accept?`. Without `accept: true` it writes nothing and returns `{needs_review: true, review}` (commands, prompts, attention marks) for the agent to show its user first; with `accept: true` it installs and echoes the review ([Nodes](graphs.md#nodes)) |
| `node_remove` | Remove an installed node document (a builtin with no override is refused) |

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

## Protocol (1)

| Tool | Description |
|---|---|
| `get_tools` | Scope-sensitive action protocols with recommended tool sets; the `session_start` scope also returns the caller's own session id |

## Interactive sessions (1)

| Tool | Description |
|---|---|
| `session_list` | List live interactive sessions with the exact ids an interactive graph hook's `target_session_id` accepts, each with platform, workdir, name, and whether a TUI is attached; pass `session_id` to look one up. A conversation resumed under a harness conversation id canopy knows appears once, with the id and name it already had — never a dead row plus a freshly-named one |

## Scheduled sends (3)

| Tool | Description |
|---|---|
| `scheduled_send_create` | Schedule a prompt for future delivery to a session (own session by default) |
| `scheduled_send_list` | List a session's pending scheduled sends |
| `scheduled_send_cancel` | Cancel a pending scheduled send by id |
