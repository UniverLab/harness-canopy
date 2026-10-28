//! The six graph resource tools: one `#[tool]` method per resource, each
//! dispatching on `action` through the shared [`ToolSpec`] contract in
//! [`crate::daemon::actions`]. Every action validates first (one refusal
//! listing the whole action table), then deserializes the same JSON — minus
//! the dispatch key — into the existing per-operation params struct and
//! calls the renamed `<tool>_<action>` `_impl` method. No behaviour lives
//! here; the logic is the old tools' own.

use crate::daemon::actions::{action_params, ActionObject, ActionSpec, ToolSpec};
use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::params_extract::Parameters;
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

// ── Action tables ────────────────────────────────────────────────────

static GRAPH: ToolSpec = ToolSpec {
    name: "graph",
    actions: &[
        ActionSpec {
            name: "list",
            required: &[],
            optional: &["workdir", "include_archived"],
        },
        ActionSpec {
            name: "get",
            required: &["graph_id"],
            optional: &[],
        },
        ActionSpec {
            name: "export",
            required: &["graph_id"],
            optional: &[],
        },
        ActionSpec {
            name: "validate",
            required: &[],
            optional: &["path", "document"],
        },
        ActionSpec {
            name: "audit",
            required: &[],
            optional: &[],
        },
        ActionSpec {
            name: "events",
            required: &["graph_id"],
            optional: &[
                "since_id",
                "since_ts",
                "spec_id",
                "kinds",
                "limit",
                "include_payload",
            ],
        },
        ActionSpec {
            name: "node_runs",
            required: &["graph_id"],
            optional: &["spec_id", "node_id", "limit", "offset", "compact"],
        },
        ActionSpec {
            name: "node_run",
            required: &["run_id"],
            optional: &[],
        },
    ],
};

static GRAPH_EDIT: ToolSpec = ToolSpec {
    name: "graph_edit",
    actions: &[
        ActionSpec {
            name: "create",
            required: &["name", "workdir"],
            optional: &[
                "description",
                "trigger",
                "infra_node_id",
                "allow_dirty_start",
                "require_spec_type",
            ],
        },
        ActionSpec {
            name: "update",
            required: &["graph_id"],
            optional: &[
                "name",
                "description",
                "workdir",
                "trigger",
                "on_completed",
                "hooks",
                "infra_node_id",
                "allow_dirty_start",
                "require_spec_type",
            ],
        },
        ActionSpec {
            name: "archive",
            required: &["graph_id"],
            optional: &[],
        },
        ActionSpec {
            name: "restore",
            required: &["graph_id"],
            optional: &[],
        },
        ActionSpec {
            name: "import",
            required: &["document", "workdir"],
            optional: &["name"],
        },
        ActionSpec {
            name: "install",
            required: &["ref", "workdir"],
            optional: &["name", "overwrite_presets", "accept"],
        },
        ActionSpec {
            name: "attach_spec",
            required: &["graph_id", "name", "position", "parallelizable"],
            optional: &["description", "spec_type", "spec_scope"],
        },
        ActionSpec {
            name: "detach_spec",
            required: &["graph_id", "spec_id"],
            optional: &[],
        },
        ActionSpec {
            name: "update_spec",
            required: &["spec_id"],
            optional: &[
                "name",
                "description",
                "position",
                "parallelizable",
                "spec_type",
                "spec_scope",
            ],
        },
    ],
};

static GRAPH_NODE: ToolSpec = ToolSpec {
    name: "graph_node",
    actions: &[
        ActionSpec {
            name: "add",
            required: &["name"],
            optional: &[
                "spec_id",
                "graph_id",
                "kind",
                "config",
                "blueprint",
                "node",
                "config_overrides",
            ],
        },
        ActionSpec {
            name: "update",
            required: &["node_id"],
            optional: &["name", "kind", "config", "config_replace", "position"],
        },
        ActionSpec {
            name: "delete",
            required: &["node_id"],
            optional: &[],
        },
        ActionSpec {
            name: "copy",
            required: &["source_node_id"],
            optional: &[
                "spec_id",
                "graph_id",
                "name",
                "config_overrides",
                "entry_from_node",
                "entry_condition",
                "on_pass_to",
                "on_fail_to",
            ],
        },
    ],
};

static GRAPH_EDGE: ToolSpec = ToolSpec {
    name: "graph_edge",
    actions: &[
        ActionSpec {
            name: "add",
            required: &["from_node", "to_node", "condition"],
            optional: &["spec_id", "graph_id", "route"],
        },
        ActionSpec {
            name: "update",
            required: &["edge_id", "condition"],
            optional: &["route", "to_node"],
        },
        ActionSpec {
            name: "delete",
            required: &["edge_id"],
            optional: &[],
        },
    ],
};

static GRAPH_ENSEMBLE: ToolSpec = ToolSpec {
    name: "graph_ensemble",
    actions: &[
        ActionSpec {
            name: "add",
            required: &["name", "from_node", "condition", "on_pass_to"],
            optional: &[
                "spec_id",
                "graph_id",
                "kind",
                "prompt_template",
                "members",
                "blueprint",
                "node",
                "min_pass",
                "straggler_timeout_minutes",
                "quorum_grace_minutes",
                "timeout_minutes",
                "infra_retry_limit",
                "infra_crash_max_seconds",
                "infra_backoff_seconds",
                "on_fail_to",
                "commit_rights",
                "crew",
                "mutex",
            ],
        },
        ActionSpec {
            name: "update",
            required: &["ensemble_id"],
            optional: &[
                "kind",
                "prompt_template",
                "members",
                "crew",
                "min_pass",
                "straggler_timeout_minutes",
                "quorum_grace_minutes",
                "timeout_minutes",
                "infra_retry_limit",
                "infra_crash_max_seconds",
                "infra_backoff_seconds",
                "on_pass_to",
                "on_fail_to",
                "from_node",
                "condition",
                "add_entry_from",
                "add_entry_condition",
                "remove_entry_from",
                "commit_rights",
                "mutex",
            ],
        },
        ActionSpec {
            name: "delete",
            required: &["ensemble_id"],
            optional: &[],
        },
        ActionSpec {
            name: "copy",
            required: &["source_ensemble_id"],
            optional: &[
                "spec_id",
                "graph_id",
                "name",
                "prompt_template",
                "members",
                "min_pass",
                "timeout_minutes",
                "straggler_timeout_minutes",
                "quorum_grace_minutes",
                "from_node",
                "condition",
                "on_pass_to",
                "on_fail_to",
            ],
        },
    ],
};

static GRAPH_CONTROL: ToolSpec = ToolSpec {
    name: "graph_control",
    actions: &[
        ActionSpec {
            name: "run",
            required: &["graph_id"],
            optional: &["queue_id", "workdir", "idea", "sandbox", "start_node_id"],
        },
        ActionSpec {
            name: "pause",
            required: &["graph_id"],
            optional: &["interrupt"],
        },
        ActionSpec {
            name: "continue",
            required: &["graph_id", "mode"],
            optional: &["node_id", "spec_id"],
        },
        ActionSpec {
            name: "reset",
            required: &["graph_id"],
            optional: &["specs", "clear_notes"],
        },
        ActionSpec {
            name: "preflight",
            required: &["graph_id"],
            optional: &["timeout_seconds", "reviewer"],
        },
        ActionSpec {
            name: "schedule_continue",
            required: &["graph_id"],
            optional: &["at", "mode"],
        },
        ActionSpec {
            name: "schedule_autorun",
            required: &["graph_id"],
            optional: &["at", "quota_reset_message"],
        },
    ],
};

// ── The six resource tools ───────────────────────────────────────────

/// Test seam: the `graph` tool's action table (so callers can assert the
/// wire contract of e.g. the `validate` action without an MCP round trip).
#[cfg(test)]
pub(crate) fn graph_action(name: &str) -> Option<&'static crate::daemon::actions::ActionSpec> {
    GRAPH.action(name)
}

/// Test seam: the `graph_edit` tool's action table.
#[cfg(test)]
pub(crate) fn graph_edit_action(name: &str) -> Option<&'static crate::daemon::actions::ActionSpec> {
    GRAPH_EDIT.action(name)
}

#[tool_router(router = tool_router_graph_tools, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph",
        description = "Read graphs, specs, node runs, and events. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `list` (required: none; optional: workdir, include_archived) — list graphs, optionally filtered by workdir. \
        `get` (required: graph_id; optional: none) — return a graph with its ordered specs, nodes, and edges. \
        `export` (required: graph_id; optional: none) — export the graph as a portable document. \
        `validate` (required: none; optional: path, document) — validate a graph or node JSON document. \
        `audit` (required: none; optional: none) — audit every node config for known misconfigurations. \
        `events` (required: graph_id; optional: since_id, since_ts, spec_id, kinds, limit, include_payload) — read the graph's durable event log. \
        `node_runs` (required: graph_id; optional: spec_id, node_id, limit, offset, compact) — list the graph's node runs. \
        `node_run` (required: run_id; optional: none) — read one node run in full. \
        Hooks are not retroactive — a hook registered after its event has already happened does not fire."
    )]
    pub(crate) async fn graph(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = GRAPH.validate(&params)?;
        match action {
            "list" => self.graph_list_impl(action_params(&params)?).await,
            "get" => self.graph_get_impl(action_params(&params)?).await,
            "export" => self.graph_export_impl(action_params(&params)?).await,
            "validate" => self.graph_validate_impl(action_params(&params)?).await,
            "audit" => self.graph_audit_node_configs_impl().await,
            "events" => self.graph_events_impl(action_params(&params)?).await,
            "node_runs" => {
                self.graph_node_runs_list_impl(action_params(&params)?)
                    .await
            }
            "node_run" => self.graph_node_run_get_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "graph_edit",
        description = "Create, change, and archive graphs and their specs. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `create` (required: name, workdir; optional: description, trigger, infra_node_id, allow_dirty_start, require_spec_type) — create a graph container. \
        `update` (required: graph_id; optional: name, description, workdir, trigger, on_completed, hooks, infra_node_id, allow_dirty_start, require_spec_type) — update graph metadata and hooks. \
        `archive` (required: graph_id; optional: none) — archive a graph out of the main list (restorable). \
        `restore` (required: graph_id; optional: none) — restore an archived graph to the main list. \
        `import` (required: document, workdir; optional: name) — import an exported graph document. \
        `install` (required: ref, workdir; optional: name, overwrite_presets, accept) — install a graph from the catalogue. \
        `attach_spec` (required: graph_id, name, position, parallelizable; optional: description, spec_type, spec_scope) — bind a spec to the graph. \
        `detach_spec` (required: graph_id, spec_id; optional: none) — unbind a spec from the graph. \
        `update_spec` (required: spec_id; optional: name, description, position, parallelizable, spec_type, spec_scope) — update a bound spec. \
        Hooks are not retroactive — a hook registered after its event has already happened does not fire."
    )]
    pub(crate) async fn graph_edit(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = GRAPH_EDIT.validate(&params)?;
        match action {
            "create" => self.graph_create_impl(action_params(&params)?).await,
            "update" => self.graph_update_impl(action_params(&params)?).await,
            "archive" => self.graph_archive_impl(action_params(&params)?).await,
            "restore" => self.graph_restore_impl(action_params(&params)?).await,
            "import" => self.graph_import_impl(action_params(&params)?).await,
            "install" => self.graph_install_impl(action_params(&params)?).await,
            "attach_spec" => self.graph_add_spec_impl(action_params(&params)?).await,
            "detach_spec" => self.graph_remove_spec_impl(action_params(&params)?).await,
            "update_spec" => self.graph_update_spec_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "graph_node",
        description = "Author nodes in a spec's or a graph's top-level graph. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `add` (required: name; optional: spec_id, graph_id, kind, config, blueprint, node, config_overrides) — add a node (kind+config, or blueprint/node template, or node document + config_overrides). \
        `update` (required: node_id; optional: name, kind, config, config_replace, position) — update a node's name, kind, or config. \
        `delete` (required: node_id; optional: none) — delete a node and cascade its edges. \
        `copy` (required: source_node_id; optional: spec_id, graph_id, name, config_overrides, entry_from_node, entry_condition, on_pass_to, on_fail_to) — copy a node's config to a new node. \
        No `graph_node` action completes a node or reports a blocker — graph node sessions keep using graph_complete_node / graph_report_blocker."
    )]
    pub(crate) async fn graph_node(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = GRAPH_NODE.validate(&params)?;
        match action {
            "add" => self.graph_add_node_impl(action_params(&params)?).await,
            "update" => self.graph_update_node_impl(action_params(&params)?).await,
            "delete" => self.graph_delete_node_impl(action_params(&params)?).await,
            "copy" => self.graph_copy_node_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "graph_edge",
        description = "Wire nodes together with pass/fail/always/route edges. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `add` (required: from_node, to_node, condition; optional: spec_id, graph_id, route) — add an edge between two nodes. \
        `update` (required: edge_id, condition; optional: route, to_node) — change an edge's condition, route, or target. \
        `delete` (required: edge_id; optional: none) — delete one edge."
    )]
    pub(crate) async fn graph_edge(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = GRAPH_EDGE.validate(&params)?;
        match action {
            "add" => self.graph_add_edge_impl(action_params(&params)?).await,
            "update" => self.graph_update_edge_impl(action_params(&params)?).await,
            "delete" => self.graph_delete_edge_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "graph_ensemble",
        description = "Author multi-member ensembles (parallel, cascade, round_robin). Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `add` (required: name, from_node, condition, on_pass_to; optional: spec_id, graph_id, kind, prompt_template, members, blueprint, node, min_pass, straggler_timeout_minutes, quorum_grace_minutes, timeout_minutes, infra_retry_limit, infra_crash_max_seconds, infra_backoff_seconds, on_fail_to, commit_rights, crew, mutex) — add an ensemble wired from a node. \
        `update` (required: ensemble_id; optional: kind, prompt_template, members, crew, min_pass, straggler_timeout_minutes, quorum_grace_minutes, timeout_minutes, infra_retry_limit, infra_crash_max_seconds, infra_backoff_seconds, on_pass_to, on_fail_to, from_node, condition, add_entry_from, add_entry_condition, remove_entry_from, commit_rights, mutex) — update an ensemble's config and wiring. \
        `delete` (required: ensemble_id; optional: none) — delete an ensemble as one unit. \
        `copy` (required: source_ensemble_id; optional: spec_id, graph_id, name, prompt_template, members, min_pass, timeout_minutes, straggler_timeout_minutes, quorum_grace_minutes, from_node, condition, on_pass_to, on_fail_to) — copy an ensemble's config to a new ensemble."
    )]
    pub(crate) async fn graph_ensemble(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = GRAPH_ENSEMBLE.validate(&params)?;
        match action {
            "add" => self.graph_add_ensemble_impl(action_params(&params)?).await,
            "update" => {
                self.graph_update_ensemble_impl(action_params(&params)?)
                    .await
            }
            "delete" => {
                self.graph_delete_ensemble_impl(action_params(&params)?)
                    .await
            }
            "copy" => self.graph_copy_ensemble_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "graph_control",
        description = "Run, pause, continue, reset, preflight, and schedule graphs. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `run` (required: graph_id; optional: queue_id, workdir, idea, sandbox, start_node_id) — launch a graph run. \
        `pause` (required: graph_id; optional: interrupt) — pause a running graph. \
        `continue` (required: graph_id, mode; optional: node_id, spec_id) — continue a paused/failed graph (mode: retry_current_node, skip_next_spec, or run_from_node). \
        `reset` (required: graph_id; optional: specs, clear_notes) — reset a completed/failed graph back to pending. \
        `preflight` (required: graph_id; optional: timeout_seconds, reviewer) — probe a graph's platforms and audit its design. \
        `schedule_continue` (required: graph_id; optional: at, mode) — schedule a one-shot deferred continue for a paused graph (mode: retry_current_node or skip_next_spec, default retry_current_node), or cancel a pending one. \
        `schedule_autorun` (required: graph_id; optional: at, quota_reset_message) — schedule a one-shot resume for a graph, or cancel a pending one."
    )]
    pub(crate) async fn graph_control(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = GRAPH_CONTROL.validate(&params)?;
        match action {
            "run" => self.graph_run_impl(action_params(&params)?).await,
            "pause" => self.graph_pause_impl(action_params(&params)?).await,
            "continue" => self.graph_continue_impl(action_params(&params)?).await,
            "reset" => self.graph_reset_impl(action_params(&params)?).await,
            "preflight" => self.graph_preflight_impl(action_params(&params)?).await,
            "schedule_continue" => {
                self.graph_schedule_continue_impl(action_params(&params)?)
                    .await
            }
            "schedule_autorun" => {
                self.graph_schedule_autorun_impl(action_params(&params)?)
                    .await
            }
            _ => unreachable!("validated action"),
        }
    }
}
