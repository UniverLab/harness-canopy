---
title: MCP Tools
description: All 34 MCP tools exposed by the canopy daemon, by category.
order: 17
---

# MCP Tools

The daemon exposes **34 MCP tools** over Streamable HTTP (port 7755) and
stdio. Connect any MCP-capable AI CLI with `canopy mcp`.

Every HTTP request under `/mcp` must present the daemon token as an
`Authorization: Bearer <token>` header, where the token is the content of
`~/.canopy/daemon.token` (created at daemon start, mode 0600). Requests
with no header, a wrong token or a malformed header get `401` with
`WWW-Authenticate: Bearer realm="canopy"` and the body
`{"error":"missing or invalid daemon token"}` — the body is never read
before authentication. `canopy bridge`, the TUI and the `canopy graph` /
`canopy spec` CLI paths send the header automatically; external MCP
clients configured over HTTP must add it themselves. Existing harness MCP
configs that still use the old unauthenticated URL form are rewritten to
the stdio `canopy bridge` form by `canopy setup` and `canopy mcp`, with a
one-time `<config>.canopy-bak` backup before the first rewrite.

HTTP sessions are bounded: a session with no request for 30 minutes is closed,
and at most 64 live sessions are kept — when a 65th client initializes, the
least recently used session is closed and the daemon logs one
`mcp session evicted: cap (live=64)` INFO line. A request on an expired or
evicted session gets the session-not-found `404`; `canopy bridge` replays its
captured `initialize` on that 404, so it reconnects on its own.

Writes wait up to 5 seconds for a concurrent writer. If the database is still
busy after that, the call fails with `<tool>: database busy after 5 s — nothing
was written, retry`. Nothing changed, so retry the same call.

Resource tools (every tool that dispatches on `action` — `spec`, `graph`,
`agent`, …) publish **typed parameters** in their MCP input schema: `action`
is a string enum of the tool's actions, and every parameter any action
accepts is declared with its JSON type and description, so MCP clients build
correctly typed arguments from `tools/list` alone. The root schema's only
`required` is `action` — which parameters a call needs depends on its action
and is listed in the tool description (and in every refusal message). As a
tolerance for clients that stringify arguments, a string that parses to a
declared object, array, integer, number or boolean (e.g. `"5"` for a
`limit`) is coerced back to that type before validation; anything else is
refused with the usual per-field error. A null value for a parameter an
action does not declare is ignored.

Graph-node sessions (agent and ensemble nodes launched by the graph engine) get
a fixed node profile of 10 tools in `tools/list` (`CANOPY_MCP_PROFILE=node`), so
every node stays under the 128-tool cap of OpenAI-compatible providers. Calls to
any other tool still work. All other sessions keep the full list.

## Agent management (4)

| Tool | Description |
|---|---|
| `agent` | Read agents, logs, and daemon health. Actions: `list`, `get` (needs `id` — the full stored definition as JSON, including the untruncated prompt), `logs` (`id`, optional `lines`/`since`), `status` — pass `action` plus the params the refusal/table lists |
| `agent_edit` | Create, change, and run agents. Actions: `add` (`id`, `prompt`, `schedule` — a 5-field cron expression), `watch` (`id`, `path`, `events`, `prompt`), `update`, `remove`, `enable`, `disable`, `schedule` (`id`, `at` — a one-shot enable at a future ISO 8601 time), `run` — the old one-operation agent tools, now one tool with an `action` parameter and the same per-action params, except `action` itself, renamed `delivery_action` (`headless` or `send_to_session`) |
| `agent_report` | Report execution status for scheduled tasks — the only way to report a run |
| `subagent` | Launch ephemeral subagents and collect their results. Actions: `spawn` (`prompt`, optional `cli`/`workdir`/`model`/`effort`/`mcp_servers`/`timeout_minutes`/`ttl_minutes`/`blocking` — async by default, or `blocking: true` to wait and return the result directly; optional `notify_session`, a live interactive session id or name to notify when a non-blocking subagent ends — omitted from a live session defaults to that session, omitted from anywhere else notifies nobody, and `blocking: true` with `notify_session` is refused), `collect` (`id`) — pass `action` plus the params the refusal/table lists. An async `spawn` response carries `"notify"` (the notified session's name, or `"none"`). The completion notice arrives as a scheduled prompt — `[subagent <id>] status: <finished\|failed\|timed_out> (<elapsed>s)`, the first 2,000 chars of output (stdout on success, else stderr, falling back to stdout when stderr is empty), and `full result: subagent collect <id>` — through the same delivery rules as every other automated message; the full result stays collectable until its TTL. A notify target in another workdir is refused at spawn time naming both workdirs |

## Harness (1)

| Tool | Description |
|---|---|
| `harness` | Inspect model catalogs, probe platform liveness, refresh the platform registry, and manage the availability roster. Actions: `models` (optional `platform`/`refresh`/`full` — every returned model id copies verbatim into a model field), `probe` (test platform+model liveness before a `graph_control` `run`; the verdict reads the response content, not the exit code), `probe_recent` (sweep recently-used platform+model pairs before a run), `refresh_registry` (fetch the platform registry and apply metadata/new-CLI updates to config.toml now, ignoring the 24-hour interval; takes no parameters and returns the report as JSON — `changed` and `kept` per platform, the `added`, `updated` and `deleted` platform names, `pending` executable changes held for human approval via `canopy registry apply`, and `up_to_date`), `roster_list`, `roster_set`, `roster_clear`, `roster_check` — pass `action` plus the params the refusal/table lists. Probes spend real tokens/quota: call them explicitly, never automatically or on a schedule |

The roster declares which platform/model pairs may run. The engine consults
it at dispatch — an unavailable member is skipped, an unavailable single node
or agent hook blocks the spec — and never infers availability from failures,
exit codes or quota messages. Only the `roster_*` actions (and the CLI, which calls them)
write the table.

The most specific entry wins: exact model, then the longest `*` prefix, then the
platform-wide entry. An `unavailable` entry past its `until` reads as available
(expired) — expiry is evaluated at read time, never by a timer.

## Intelligence (4)

| Tool | Description |
|---|---|
| `intelligence_get_context` | Curated project context for session start (`scope`: `light` or `full`). `light` stays under 12,000 characters: replaces `body` with a 300-char `body_excerpt`, drops `metadata`, caps `knowledge` and `project_knowledge` at 5 nodes, trims `activity` `message` (200 chars) and `payload` (300 chars), adds a `truncated` count when items are dropped, and includes a `hint` to read full nodes with `intelligence_search`; `full` returns complete nodes |
| `intelligence_upsert` | Persist a fact, pattern or session summary |
| `intelligence_search` | Full-text search across nodes |
| `intelligence_admin` | Prune and link the knowledge graph. Actions: `delete_node` (`node_id`, optional `project_hash`), `delete_relation` (`edge_id`, optional `project_hash`), `walk` (`node_id`, optional `depth`/`compact`), `list_projects` (optional `query`/`limit`), `link_projects` (`from_project_hash`, `to_project_hash`, optional `relation`/`weight`) — pass `action` plus the params the refusal/table lists |

The multi-agent sync tools are retired: coordination now goes through
`session_list` (see every live session), `scheduled_prompts` (wake a
session later instead of polling), and `intelligence_upsert` /
`intelligence_search` (persist and find shared findings).

## Graph engine (9)

| Tool | Description |
|---|---|
| `graph` | Read graphs, specs, node runs, events, and metrics. Actions: `list`, `get`, `export`, `validate`, `audit`, `events`, `node_runs`, `node_run`, `metrics` — pass `action` plus the params the refusal/table lists (e.g. `get` needs `graph_id`; `node_run` needs `run_id`). `get` also accepts `compact` — a summary sized for agent context: counts, hook fields, and one-line node/ensemble/spec listings with no configs or prompts — and `node_id`/`ensemble`, which return the full record of one node (by id or unique id prefix) or ensemble (by id, unique id prefix, or exact name); `compact` cannot be combined with `node_id` or `ensemble`, and the two selectors are mutually exclusive. `get` and `list` also return trigger lifecycle fields `enabled`, `enable_at`, `expires_at`, the derived `log_path` (`~/.canopy/logs/graph-<id>.log`), and derived `last_run_at`/`last_run_ok` for the newest run (any launch source). `get` also reports the graph's newest sandbox as `{id, status}` (`null` when it never ran sandboxed). `metrics` (optional: `since` default `7d`, `graph_id`, `by` default `graph`) aggregates run counts, pass rate, and durations over `graph_runs` without loading `input`/`output`, returning `{window:{since,until}, by, groups:[{key,runs,passed,failed,interrupted,waiting,error,running,pass_rate,duration_s:{avg,p95,max}}]}` |
| `graph_edit` | Create, change, and archive graphs and their specs. Actions: `create`, `update`, `archive`, `restore`, `import`, `install` (with `as`/`replace` for bundled presets owned by another source), `attach_spec`, `detach_spec`, `update_spec` — the old one-operation graph authoring tools, now one tool with an `action` parameter and the same per-action params. `create` accepts `enabled` (default `true`), `enable_at` and `expires_at` (RFC3339). `update` accepts `enabled`, and `enable_at`/`expires_at` as tri-state fields (omit to leave unchanged, `null` to clear, value to set). `attach_spec` and `update_spec` parse a conventional name and enforce the same subject rules as `spec_edit` (empty, multi-line, or repeated-header subjects are refused). When spec review is enabled, `attach_spec` creates a spec queued for review (it may be unreviewed) and content changes through `update_spec` reset its review state; when the validation queue for your session drains, you receive exactly one message with the verdict and findings |
| `graph_node` | Author graph nodes. Actions: `add` (kind+config, or node document + `config_overrides`), `update`, `delete`, `copy`. Never completes a node or reports a blocker — node sessions keep using `graph_complete_node` / `graph_report_blocker` |
| `graph_edge` | Wire graph nodes. Actions: `add`, `update`, `delete` (params unchanged: `from_node`/`to_node`/`condition`/`route`, `edge_id`, …) |
| `graph_ensemble` | Author multi-member ensembles (parallel, cascade, round_robin, random). Actions: `add`, `update`, `delete`, `copy`; `add`/`update` accept `read_only` (fingerprint the workdir on every member run and fail with `read_only_violation` on changes; refused together with `commit_rights`). Other params: `members`, `min_pass`, `on_pass_to`, … |
| `graph_control` | Run and manage graph execution. Actions: `run` (optional `workdir` override for the run), `pause`, `continue` (required `mode`: retry_current_node / skip_next_spec / run_from_node; if the current node is an ensemble member or join, `retry_current_node` restarts the ensemble; optional `workdir` to replace the run's workdir), `reset`, `preflight`, `schedule_continue` (optional `mode`: retry_current_node / skip_next_spec), `schedule_autorun`. Manual `run`/`continue` still work for a disabled trigger graph and never append to the per-graph log. The workdir a run is launched with is persisted with the run and every continuation of it keeps using it; `graph` `get`/`list` show it as `run_workdir` when it differs from the stored workdir. A bound-spec run (no `queue_id`) refuses if a pending bound spec is not review-approved |
| `graph_complete_node` | Complete an active graph node. `run_id`/`node_id` may be omitted inside a node run — they resolve from the `CANOPY_NODE_RUN_ID`/`CANOPY_NODE_ID` environment the engine sets on every agent process; a supplied id with the right 8-char prefix but a wrong tail is corrected to the environment's run (the reply says `Corrected ...`), while any other unknown id fails naming the env run id |
| `graph_report_blocker` | Report a blocker for an active graph node (same optional-ids resolution and prefix correction as `graph_complete_node`) |
| `sandbox` | List, inspect, land and discard the sandbox worktrees a sandboxed graph run leaves behind. Actions: `list` (optional `graph_id`/`status` — every sandbox with its id, path, graph, branch, base branch, original workdir, status, whether its branch holds unique commits, how far behind its base it is, and whether its worktree is still registered), `get` (`id` — one sandbox by id or unambiguous prefix), `land` (`id` — merges onto the base branch, marks it `merged` and removes the worktree; refuses rather than forcing when the base checkout is on another branch, has uncommitted changes, or the merge conflicts, naming the conflicting paths, and leaves the sandbox untouched), `discard` (`id`, optional `force` — removes the worktree and branch, marks it `discarded`; refuses unique commits unless `force: true`) — pass `action` plus the params the refusal/table lists. The same operations as `canopy sandbox`, which is the CLI form ([Sandboxes](cli-reference.md#sandboxes)) |

A `graph_control` `pause` while a node runs records a pending pause instead
of pausing at once: `graph` `get`/`list` keep reporting status `running`
with `pause_pending: true` and the waited-for node names in
`pause_waiting_for`, a second `pause` is a no-op answering `pause already
pending`, and `continue`/`run` cancel it — see
[Pending pauses](graphs-operations.md#pending-pauses).

## Crews (1)

| Tool | Description |
|---|---|
| `crew` | Manage crews and read every graph's harness/model assignments. Actions: `list` (every crew with version, ordered members and binding counts), `get` (one crew and every binding of it), `create` (from an ordered member list, or seeded from an ensemble's members — pass exactly one of `members` or `from_ensemble`), `update` (replace the member list, set the description, and/or rename; bound ensembles pick it up at their next dispatch), `delete` (refused while any ensemble, node or hook still binds it), `assignments` (every graph's harness/model assignments with platform, model, timeout, crew and availability; `group_by: "pair"` summarises per platform/model pair — no prompt text is returned) — pass `action` plus the params the refusal/table lists |

## Configuration (1)

| Tool | Description |
|---|---|
| `config` | Read and change canopy settings. Actions: `get` (one setting by its dotted key, e.g. `clean.retention_days`), `list` (every setting with group, kind, default, effective value, source, apply mode and status; optional `group` filter), `set` (validated against the setting's type and range first — nothing is written on refusal; returns the new effective value and what the user must do: `applied`, `applies on next run`, `restart the daemon`, or `restart the TUI`), `reset` (one setting back to its registry default) — pass `action` plus the params the refusal/table lists. The built-in adversarial spec review is configured here through `spec_review.enabled`, `spec_review.platform`, `spec_review.model` and `spec_review.max_parallel`; enabling it is refused while the platform is empty, is not in the platform registry, or its platform/model pair is unavailable in the roster ([Settings reference](cli-reference.md#settings-reference)) |

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
counted in the 35 tools above.

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

An interactive hook targets a session by `target_session_id`, by
`target_session_name`, or by both when they resolve to the same session (a
`target_session_id` holding no live UUID falls back to a name lookup) — see [Graphs](graphs-hooks-and-queues.md#event-keyed-hooks)
for how name resolution and its failure modes work. `session_list` remains
the way to see both the names and ids of every live session a hook can
target.

## Named mutexes

A node (agent, check, gate) or ensemble declares the shared resources it
needs with `mutex: ["cpu"]`; the engine grants each resource to one node run
at a time across all graphs of the daemon, and the others wait in a visible
FIFO queue without burning their timeout. Each name must match
`^[a-z][a-z0-9-]{0,31}$`. The lease is held per node run (per member run for
an ensemble) and released when the run ends for any reason. Inspect the live
leases and FIFO waiters with `canopy mutex list [--json]`.

## Spec backlog (2)

Every tool that takes a `spec_id` also accepts the spec's code (e.g. `FIX-7`),
resolved within the caller's or the graph's workdir.

| Tool | Description |
|---|---|
| `spec` | Read specs, sections, and notes. Actions: `list` (filterable by workdir, status, and `spec_type`, with limit/offset pagination; each spec carries its `intent`, `review_state`, `review_tier`, and `review_findings`), `get` (one spec with its full description, `intent`, `review_state`, `review_tier`, and `review_findings`), `section` (one canonical section from a spec body), `notes` (every note of a spec, newest first, with owner, author model, `updated_at` and body) — pass `action` plus the params the refusal/table lists |
| `spec_edit` | Create, change, and transition specs. Actions: `create` (a standalone spec — supports `spec_type`/`spec_scope` for automatic code generation and deterministic commit messages; a conventional name such as `fix(tui): open F1` sets the type and scope and stores the subject alone, while an untyped create still succeeds but returns a `warning`; a create without a `workdir` adds a `warning_workdir` line because its code was drawn from the empty-workdir counter), `update` (a conventional name is parsed exactly like `create`: explicit `spec_type`/`spec_scope` win, the stored name is the subject alone, and a type change never re-codes a spec that already has a code; setting or changing `workdir` on an unbound spec with no run re-codes it from the new workdir's counter, while a graph-bound or already-run spec keeps its code), `delete` (refused while bound to a graph), `set_status` (admin transition: complete, skip, or reopen), `note_write` (`run_id` + `body`, or `spec_id` + `body` for the `operator` note — `spec_id` is for the operator only: a caller bound to an active node run is refused and told to write its own note with `run_id`; body ≤ 2 500 chars). The stored subject is refused when it is empty, contains a line break, or still begins with a Conventional Commits header (the repeated `fix(tui): fix(tui): ...` prefix), naming the corrected subject — the same shared rule as `graph_edit` `attach_spec`/`update_spec`. `create` and `update` accept a short `intent` (≤ 500 chars) describing what the spec must achieve: required while `spec_review.enabled` is on, optional while it is off. When spec review is enabled, `create` queues the spec for review and content changes through `update` reset its review state; `review_state`, `review_tier`, and `review_findings` are read-only. When the validation queue for your session drains, you receive exactly one message carrying the verdict and findings for every spec reviewed since the last drain (specs with no recorded session produce no message) |

Spec bodies use the tagged `<spec>` format with all seven canonical section
tags — `<objective>`, `<functional_requirements>`,
`<non_functional_requirements>`, `<constraints>`, `<guidelines>`, `<in_scope>`,
`<out_of_scope>` — with markdown inside each section. Writes that do not parse
are rejected naming the offending tag; legacy heading-format bodies remain
readable.

## Spec queues (1)

| Tool | Description |
|---|---|
| `queue` | Author ordered spec queues. Actions: `create`, `list` (a queue's members, or all queues), `add_spec` (a grouped add lands after the group's last pending member, and is refused if the spec is not review-approved), `remove_spec`, `reorder` (full replacement of the order), `set_group` — pass `action` plus the params the refusal/table lists |

## Node documents (1)

| Tool | Description |
|---|---|
| `node` | Manage shareable node documents. Actions: `list` (installed documents and the builtins embedded in the binary, marking each `installed` or `builtin` with the catalogue entry that installed it), `get` (one document with its kind, source, and full config), `install` (a node document from the catalogue by `ref` — a bare `<slug>` is the official UniverLab entry, `<owner>/<repo>/<slug>` a community one, each with an optional `@<ref>`, or `draft:<id>` for a one-use editor draft — with its bundled preset; `as?`, `replace?`, `overwrite_presets?`, `accept?`. A name already installed from another source is refused: pass `as` to install under another local name, or `replace: true` to overwrite. Without `accept: true` it writes nothing and returns `{needs_review: true, review}` (commands, prompts, attention marks) for the agent to show its user first; with `accept: true` it installs and echoes the review), `remove` (an installed document; a builtin with no override is refused) — pass `action` plus the params the refusal/table lists ([Nodes](graphs-nodes.md#nodes)) |

Node documents replaced the retired blueprint tools outright: `graph_node`
and `graph_ensemble` `add` take `node`, `kind` + `config`, or — for
ensembles — a crew.

## Catalogue (1)

| Tool | Description |
|---|---|
| `catalogue` | Search the community graphs catalogue before building your own. Actions: `search` (`q`, optional `sort`/`page`/`limit` — free-text search over community graphs and nodes; `sort` is `top` (default) or `new`, `page` starts at 1, `limit` (default 10, max 50) truncates the returned page client-side and deeper access uses `page`; returns `items` with `q`/`sort`/`page`/`total`/`truncated`, each item carrying kind, slug, `repo` (owner/repo), `official`, stars and description — the install ref is the bare slug when official, else `repo/slug` — plus the model verdict when the catalogue exposes it) — pass `action` plus the params the refusal/table lists |

## Project (1)

| Tool | Description |
|---|---|
| `project` | Manage the project registry. Actions: `register` (`path` — explicitly register a directory, no marker file needed), `remap` (`project_hash`, `new_path`, optional `dry_run`/`force` — remap a moved/renamed workdir instead of orphaning history), `search` (`query` — search by name/description), `update` (`project_hash`, optional `description`/`tags`) — pass `action` plus the params the refusal/table lists |

## RAG (1)

| Tool | Description |
|---|---|
| `rag` | Search indexed personal content and maintain the vector store. Actions: `search` (`query`, optional `agent_id`/`limit` — semantic search, rate-limited), `compact` (merge fragments and prune old versions now) — pass `action` plus the params the refusal/table lists |

## Protocol (1)

| Tool | Description |
|---|---|
| `get_tools` | Scope-sensitive action protocols with recommended tool sets; the `session_start` scope also returns the caller's own session id |

## Interactive sessions (2)

| Tool | Description |
|---|---|
| `session_list` | List live interactive sessions with the exact ids an interactive graph hook's `target_session_id` accepts, each with platform, workdir, name, and `attached` — whether the session's CLI process is still alive on this host, which stays `yes` for a daemon-hosted session even while no TUI is open; pass `session_id` to look one up. A resumed session appears once, with the id and name it already had — never a dead row plus a freshly-named one; only a name held by another live session mints a fresh name, keeping the id |
| `session_rename` | Rename a live interactive session. `target` accepts a live session id or name; `new_name` must be 1-32 chars of lowercase `a-z`, digits, or `-`, and must not be held by another live session. Returns `id`, `old_name`, and `new_name`. |

## Scheduled prompts (1)

| Tool | Description |
|---|---|
| `scheduled_prompts` | Schedule a prompt for future delivery to a session (own session by default), and list and cancel pending scheduled sends. Actions: `create` (`prompt`, exactly one of `at`/`in_seconds`, optional `target_session_id` or `target_session_name`; `target_session_id` may also carry a live session name when no live session has that UUID; if both are given they must resolve to the same session), `list` (a session's pending sends, soonest first; optional `session_id`), `cancel` (`id`) — pass `action` plus the params the refusal/table lists. Name resolution is deterministic: an ambiguous name is refused listing `name (id)` matches sorted by id, an unknown name is refused listing live interactive session names sorted alphabetically (or `none`). |

## Session shelf (3)

| Tool | Description |
|---|---|
| `session_send_file` | Copy 1 to 10 files onto the session shelf (own session by default, `session_id` to target another, host shelf when the caller has no session). Any file type is accepted — content sniffing only decides how it is displayed — and each file is at most 20 MiB. Only files inside the calling session's own workdir — even when `session_id` targets another shelf; a caller with no session may only send from the shelf directory — or the shelf directory are accepted; anything else is refused with code `forbidden` (`path is outside this session's workdir`). Secret-looking names (`daemon.token`, `*.key`, `*.pem`, `id_*`) and files under the canopy data directory outside the shelf are always refused, even inside the workdir. One bad path never blocks the others: the result lists `items` (`file_id`, `name`, `mime`, `size`) and per-path `errors` (`path`, `code`, `message`), with codes `not_found`, `not_a_file`, `too_large`, `forbidden`, `io`. An optional `caption` (at most 500 characters) is stored with every file of the call and shown in the TUI |
| `session_list_files` | List one session shelf newest first (own session by default, `session_id` to target another, host shelf when the caller has no session). Each item carries `file_id`, `name`, `size`, `mime`, `origin` (`agent` or `user`) and `created_at` |
| `session_read_file` | Read one shelf file by `file_id`: `file_id`, `name`, `mime`, `size`, `enc` (`text` or `b64`), `content` and `truncated`. Text files come back as UTF-8; binary files as base64 capped at 1 MiB, with `truncated: true` past the cap |

## Remote control (1)

| Tool | Description |
|---|---|
| `remote` | Remote control of this hub from the browser. Actions: `status` (sign-in state, whether remote is enabled, the relay URL, the relay phase — `connected`, `connecting`, `token rejected: sign in again`, `relay rejected: see remote status`, `off` — the live device ids and every paired device with its paired-at and last-seen times and live flag), `disconnect` (`device_id`: close the paired device's live peer at once, keeping the pairing so it may reconnect), `trust_remove` (`login`: forget a GitHub profile and close its live guest peers at once; returns `{ "removed": "<login>", "closed": <n> }` where `n` is the number of guest peers closed). Pairing a device, trusting a guest, revoking a device and switching remote on are not agent actions: use the CLI or the TUI |
