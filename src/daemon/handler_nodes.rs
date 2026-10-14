//! MCP handler tools split out of `daemon::handler` (node tools and node-config validation).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_edges::{resolve_graph_target, GraphTarget};
use crate::daemon::handler_formatting::{
    build_id_result, build_node_update_response_with_changes, internal_error,
};
use crate::daemon::handler_helpers::record_config_change_if_running;
use crate::daemon::handler_specs::validate_at_least_one_bool;
use crate::daemon::handler_validate::{
    json_value_kind_name, parse_router_routes, resolve_prefix_or_error, validate_node_exists,
    validate_node_kind, validate_node_not_ensemble_owned, validate_node_position_conflict,
    validate_non_empty, validate_not_join_kind, validate_topology_mutation_allowed,
};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::blueprints::merge_blueprint_config;
use crate::domain::blueprints::Blueprint;
use crate::domain::graphs::{
    validate_router_edges_declared, validate_router_route_coverage, validate_router_routes,
    GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind,
};
use rmcp::model::{CallToolResult, Content};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_nodes, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_add_node",
        description = "Add a graph node to an existing graph spec, or (via graph_id instead of spec_id) to the graph's top-level graph. Provide either a full 'config' (with 'kind'), or a 'blueprint' name (see blueprint_list) optionally shallow-merged with 'config_overrides'. An agent node also accepts `infra_retry_limit`/`infra_crash_max_seconds`/`infra_backoff_seconds` (defaults 2/60/30) to override the infra-crash retry budget for this node alone, taking precedence over its platform's config.toml value. A router node with {\"decision\": true} classifies via the platform's decision model instead of spawning an agent (optional \"min_confidence\" 0-1, default 0); e.g. routes quota_with_reset/transient_glitch/other with fallback other."
    )]
    pub(crate) async fn graph_add_node(
        &self,
        Parameters(params): Parameters<GraphAddNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name.trim();
        if let Err(e) = validate_non_empty(name, "Graph node name") {
            return Ok(error_result(&e));
        }
        let target = match resolve_graph_target(
            &self.db,
            params.spec_id.as_deref(),
            params.graph_id.as_deref(),
        ) {
            Ok(target) => target,
            Err(e) => return Ok(error_result(&e)),
        };

        let (kind, config) = match resolve_node_kind_and_config(
            &self.db,
            params.kind.as_deref(),
            params.config,
            params.blueprint.as_deref(),
            params.config_overrides,
        ) {
            Ok(result) => result,
            Err(e) => return Ok(error_result(&e)),
        };
        if let Err(e) = validate_not_join_kind(kind) {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_node_config(kind, &config) {
            return Ok(error_result(&e));
        }
        if let Some(msg) = reject_interactive_only_platform(&config) {
            return Ok(error_result(&msg));
        }

        let (spec_id, graph_id, next_position) = match &target {
            GraphTarget::Spec(spec_id) => {
                let next_position = self
                    .db
                    .list_graph_nodes(spec_id)
                    .map_err(internal_error)?
                    .last()
                    .map(|node| node.position + 1)
                    .unwrap_or(1);
                (Some(spec_id.clone()), None, next_position)
            }
            GraphTarget::Graph(graph_id) => {
                let next_position = self
                    .db
                    .list_graph_nodes_for_graph(graph_id)
                    .map_err(internal_error)?
                    .last()
                    .map(|node| node.position + 1)
                    .unwrap_or(1);
                (None, Some(graph_id.clone()), next_position)
            }
        };

        let node = GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id,
            graph_id: graph_id.clone(),
            name: name.to_string(),
            kind,
            config,
            position: next_position,
            created_at: chrono::Utc::now(),
        };
        self.db.insert_graph_node(&node).map_err(internal_error)?;

        // CM2: auto-wire a `Error` edge to the graph's infra node if set.
        if let Some(ref lid) = graph_id {
            if let Ok(Some(lp)) = self.db.get_graph(lid) {
                if let Some(ref infra_node_id) = lp.infra_node_id {
                    let edge = GraphEdge {
                        id: uuid::Uuid::new_v4().to_string(),
                        spec_id: node.spec_id.clone(),
                        graph_id: Some(lid.clone()),
                        from_node: node.id.clone(),
                        to_node: infra_node_id.clone(),
                        condition: GraphEdgeCondition::Error,
                    };
                    let _ = self.db.insert_graph_edge(&edge);
                }
            }
        }

        Ok(build_id_result(&node.id, "node_id"))
    }

    #[tool(
        name = "graph_update_node",
        description = "Update an existing graph node. Config merges with the stored config by default (partial update: keys present are changed, keys absent are left as-is). Set `config_replace` to `true` to replace the entire config instead. Router nodes accept \"decision\"/\"min_confidence\" (see graph_add_node)."
    )]
    pub(crate) async fn graph_update_node(
        &self,
        Parameters(params): Parameters<GraphUpdateNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        let node_id = match resolve_prefix_or_error(
            &self.db,
            params.node_id.trim(),
            Database::resolve_graph_node_id_by_prefix,
            "node",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let node = match validate_node_exists(&self.db, &node_id) {
            Ok(node) => node,
            Err(e) => return Ok(error_result(&e)),
        };
        if let Err(e) = validate_node_not_ensemble_owned(&self.db, &node_id) {
            return Ok(error_result(&e));
        }
        // CM15 / FR5: `kind` and `position` are graph TOPOLOGY — the running
        // engine resolves routing, entry-node selection and router-route
        // coverage from a per-run snapshot and never re-reads them mid-run.
        // Only node CONFIG (platform/model/effort/prompt/timeout/commit_rights/
        // resume) is read fresh per dispatch. Accepting a kind/position change on
        // a running graph would silently fail to affect routing — the exact
        // "silent success" this spec removes — so refuse it, naming the graph's
        // state. `name` and `config` stay editable while running.
        if params.kind.is_some() || params.position.is_some() {
            if let Err(e) = validate_topology_mutation_allowed(
                &self.db,
                node.spec_id.as_deref(),
                node.graph_id.as_deref(),
            ) {
                return Ok(error_result(&e));
            }
        }

        let name = match params.name.as_deref().map(str::trim) {
            Some("") => return Ok(error_result("Graph node name must not be empty.")),
            Some(value) => Some(value),
            None => None,
        };
        let kind = match params.kind.as_deref().map(str::trim) {
            Some("") => return Ok(error_result("Graph node kind must not be empty.")),
            Some(value) => match validate_node_kind(value) {
                Ok(kind) => Some(kind),
                Err(e) => return Ok(error_result(&e)),
            },
            None => None,
        };
        if let Some(kind) = kind {
            if let Err(e) = validate_not_join_kind(kind) {
                return Ok(error_result(&e));
            }
        }

        if let Some(position) = params.position {
            if let Err(e) = validate_node_position_conflict(&self.db, &node, &node_id, position) {
                return Ok(error_result(&e));
            }
        }

        if let Err(e) = validate_at_least_one_bool(
            &[
                name.is_some(),
                kind.is_some(),
                params.config.is_some(),
                params.position.is_some(),
            ],
            "graph_update_node",
        ) {
            return Ok(error_result(&e));
        }

        let config_replace = params.config_replace.unwrap_or(false);
        let (effective_config, changed_keys) = match (&params.config, config_replace) {
            (Some(provided), false) => {
                let mut merged = node.config.as_object().cloned().unwrap_or_default();
                let mut changed = Vec::new();
                for (key, value) in provided {
                    let old_value = merged.get(key);
                    if old_value != Some(value) {
                        changed.push(key.clone());
                    }
                    merged.insert(key.clone(), value.clone());
                }
                (serde_json::Value::Object(merged), changed)
            }
            (Some(provided), true) => {
                let mut changed = Vec::new();
                let default_map = serde_json::Map::new();
                let stored_map = node.config.as_object().unwrap_or(&default_map);
                for (key, value) in provided {
                    if stored_map.get(key) != Some(value) {
                        changed.push(key.clone());
                    }
                }
                for key in stored_map.keys() {
                    if !provided.contains_key(key) {
                        changed.push(key.clone());
                    }
                }
                (serde_json::Value::Object(provided.clone()), changed)
            }
            (None, _) => (node.config.clone(), Vec::new()),
        };

        if kind.is_some() || params.config.is_some() {
            let effective_kind = kind.unwrap_or(node.kind);
            if let Err(e) = validate_node_config(effective_kind, &effective_config) {
                return Ok(error_result(&e));
            }
            if let Some(msg) = reject_interactive_only_platform(&effective_config) {
                return Ok(error_result(&msg));
            }
            // A router's routes must stay consistent with whatever edges
            // already name them: no edge left pointing at a route that was
            // just removed, and (once wiring has started) no declared route
            // left without one — see `validate_router_route_coverage`.
            if effective_kind == GraphNodeKind::Router {
                let (routes, _fallback) = match parse_router_routes(&effective_config) {
                    Ok(parsed) => parsed,
                    Err(e) => return Ok(error_result(&e)),
                };
                let edges = match (&node.spec_id, &node.graph_id) {
                    (Some(spec_id), _) => {
                        self.db.list_graph_edges(spec_id).map_err(internal_error)?
                    }
                    (_, Some(graph_id)) => self
                        .db
                        .list_graph_edges_for_graph(graph_id)
                        .map_err(internal_error)?,
                    (None, None) => Vec::new(),
                };
                if let Err(e) = validate_router_edges_declared(&routes, &node_id, &edges) {
                    return Ok(error_result(&e));
                }
                if let Err(e) = validate_router_route_coverage(&routes, &node_id, &edges) {
                    return Ok(error_result(&e));
                }
            }
        }

        let config_to_store = if params.config.is_some() {
            Some(&effective_config)
        } else {
            None
        };

        self.db
            .update_graph_node_details(&node_id, name, kind, config_to_store, params.position)
            .map_err(internal_error)?;

        // CM46: a node edit on a running graph is narratable engine history.
        let owning_graph_id = node.graph_id.clone().or_else(|| {
            node.spec_id.as_deref().and_then(|sid| {
                self.db
                    .get_graph_spec(sid)
                    .ok()
                    .flatten()
                    .and_then(|spec| spec.graph_id)
            })
        });
        record_config_change_if_running(
            &self.db,
            owning_graph_id.as_deref(),
            node.spec_id.as_deref(),
            Some(&node_id),
            "node",
            format!("Node '{}' config updated.", name.unwrap_or(&node.name)),
        );

        Ok(build_node_update_response_with_changes(
            &node_id,
            &changed_keys,
        ))
    }

    #[tool(
        name = "graph_audit_node_configs",
        description = "Scan every graph node in the database for a config key its kind will never read (e.g. 'prompt' on an agent node, which the engine silently ignores in favor of 'prompt_template'). Write-time validation (graph_add_node/graph_update_node) rejects this going forward; this tool finds nodes that predate it and are still silently degraded. Also lists every agent node/member whose effective infra-retry budget (infra_retry_limit/infra_crash_max_seconds/infra_backoff_seconds) differs from its platform's own value."
    )]
    pub(crate) async fn graph_audit_node_configs(&self) -> Result<CallToolResult, McpError> {
        let nodes = self.db.list_all_graph_nodes().map_err(internal_error)?;

        let mut flagged = Vec::new();
        for node in &nodes {
            let Some(map) = node.config.as_object() else {
                continue;
            };
            let unknown = unknown_config_keys(node.kind, map);
            if unknown.is_empty() {
                continue;
            }
            // A spec-scoped node's own row has no `graph_id` (only the spec
            // does) — resolve it so a flagged node can be traced back to the
            // graph that owns it without a second round-trip.
            let graph_id = match &node.graph_id {
                Some(graph_id) => Some(graph_id.clone()),
                None => match &node.spec_id {
                    Some(spec_id) => self
                        .db
                        .get_graph_spec(spec_id)
                        .map_err(internal_error)?
                        .and_then(|spec| spec.graph_id),
                    None => None,
                },
            };
            flagged.push(serde_json::json!({
                "node_id": node.id,
                "name": node.name,
                "kind": node.kind.display_str(),
                "spec_id": node.spec_id,
                "graph_id": graph_id,
                "unknown_keys": unknown,
            }));
        }

        let mut infra_overrides = Vec::new();
        for node in &nodes {
            if node.kind != GraphNodeKind::Agent {
                continue;
            }
            let effective = crate::graph_engine::resolve_node_infra_config(node);
            let platform = node
                .config
                .get("platform")
                .or_else(|| node.config.get("cli"))
                .and_then(serde_json::Value::as_str);
            let baseline = crate::graph_engine::resolve_platform_only_infra_config(platform);
            let differs = effective.retry_limit.value != baseline.retry_limit.value
                || effective.crash_max_seconds.value != baseline.crash_max_seconds.value
                || effective.backoff_seconds.value != baseline.backoff_seconds.value;
            if !differs {
                continue;
            }
            let graph_id = match &node.graph_id {
                Some(graph_id) => Some(graph_id.clone()),
                None => match &node.spec_id {
                    Some(spec_id) => self
                        .db
                        .get_graph_spec(spec_id)
                        .map_err(internal_error)?
                        .and_then(|spec| spec.graph_id),
                    None => None,
                },
            };
            infra_overrides.push(serde_json::json!({
                "node_id": node.id,
                "name": node.name,
                "spec_id": node.spec_id,
                "graph_id": graph_id,
                "platform": platform,
                "infra_retry_limit": { "value": effective.retry_limit.value, "source": effective.retry_limit.source, "platform_value": baseline.retry_limit.value },
                "infra_crash_max_seconds": { "value": effective.crash_max_seconds.value, "source": effective.crash_max_seconds.source, "platform_value": baseline.crash_max_seconds.value },
                "infra_backoff_seconds": { "value": effective.backoff_seconds.value, "source": effective.backoff_seconds.source, "platform_value": baseline.backoff_seconds.value },
            }));
        }

        let commit_rights_nodes: Vec<serde_json::Value> = nodes
            .iter()
            .filter(|n| n.config.get("commit_rights").and_then(|v| v.as_bool()) == Some(true))
            .map(|n| {
                serde_json::json!({
                    "node_id": n.id,
                    "name": n.name,
                    "spec_id": n.spec_id,
                    "graph_id": n.graph_id,
                })
            })
            .collect();
        let all_ensembles = self.db.list_all_ensembles().map_err(internal_error)?;
        let commit_rights_ensembles: Vec<serde_json::Value> = all_ensembles
            .iter()
            .filter(|e| e.ensemble.commit_rights)
            .map(|e| {
                serde_json::json!({
                    "ensemble_id": e.ensemble.id,
                    "name": e.ensemble.name,
                    "spec_id": e.ensemble.spec_id,
                    "graph_id": e.ensemble.graph_id,
                })
            })
            .collect();

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "flagged_nodes": flagged,
                "commit_rights_nodes": commit_rights_nodes,
                "commit_rights_ensembles": commit_rights_ensembles,
                "infra_config_overrides": infra_overrides,
            }))
            .unwrap_or_default(),
        )]))
    }
}

/// Whether an agent node's config names a harness — a non-empty `platform`
/// or `cli` field. Shared by [`validate_node_config`]'s `Agent` arm (the
/// general "this config can run" gate) and `resolve_node_kind_and_config`'s
/// blueprint branch (which needs the same check before merge, to give a
/// blueprint-specific error instead of the generic one).
pub(crate) fn config_has_agent_harness(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    let has_non_empty_str = |field: &str| {
        map.get(field)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    has_non_empty_str("platform") || has_non_empty_str("cli") || has_non_empty_str("crew")
}

/// `graph_add_node`/`graph_update_node`'s interactive-only gate (CM35):
/// `Some(reason)` when the node's effective config names a harness marked
/// `headless = false`. Extracts the platform exactly the way
/// [`validate_node_config`]'s `Agent` arm (via [`config_has_agent_harness`])
/// does — a non-empty `platform` (or `cli`) string; a config of another kind
/// carries no platform, so this returns `None` for it. An unconfigured
/// platform is untouched (see `interactive_only_rejection`).
fn reject_interactive_only_platform(config: &serde_json::Value) -> Option<String> {
    let map = config.as_object()?;
    let platform = ["platform", "cli"]
        .iter()
        .filter_map(|field| map.get(*field).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .find(|s| !s.is_empty())?;
    crate::daemon::handler_helpers::interactive_only_rejection(platform)
}

/// Every config key an `agent` node is actually read for — see
/// `execute_agent_node`/`resolve_node_prompt_template` in `graph_engine.rs`.
/// Notably absent: `prompt`. An agent's prompt is `prompt_template` (or
/// `prompt_preset`); `prompt` is a plain node never reads, and a node that
/// carries it silently runs on the bare fallback template instead — the
/// defect this allowlist exists to catch. `commit_rights` (B37) is accepted
/// on every kind since the engine checks it uniformly regardless of which
/// node in the graph actually moves HEAD. `require_report` (boolean, default
/// `false`) exists because a harness can exit 0 having done nothing — see
/// `agent_finished_execution` in `graph_engine.rs`. CM30:
/// `infra_retry_limit`/`infra_crash_max_seconds`/`infra_backoff_seconds`
/// override the infra-crash retry budget for this node alone (validated as
/// non-negative integers); absent, the platform's config.toml value applies,
/// then the engine default (2/60/30).
pub(crate) const AGENT_CONFIG_KEYS: &[&str] = &[
    "platform",
    "cli",
    "crew",
    "model",
    "effort",
    "timeout_minutes",
    "resume",
    "resume_prompt",
    "prompt_template",
    "prompt_preset",
    "require_report",
    "commit_rights",
    "skills",
    "infra_retry_limit",
    "infra_crash_max_seconds",
    "infra_backoff_seconds",
];

/// Every config key a `check` node is read for — see `execute_check_node`.
pub(crate) const CHECK_CONFIG_KEYS: &[&str] = &[
    "command",
    "success_condition",
    "timeout_seconds",
    "commit_rights",
];

/// Every config key a `gate` node is read for — see `execute_gate_node`.
pub(crate) const GATE_CONFIG_KEYS: &[&str] = &["evaluate", "value", "commit_rights"];

/// Every config key a `router` node is read for — see `parse_router_routes`
/// and `parse_router_decision`.
pub(crate) const ROUTER_CONFIG_KEYS: &[&str] = &[
    "routes",
    "fallback",
    "platform",
    "cli",
    "model",
    "timeout_minutes",
    "prompt_template",
    "decision",
    "min_confidence",
    "commit_rights",
];

/// The config keys a node's kind is actually read for, or `None` for `Join`
/// (engine-managed — see `validate_node_config`'s `Join` arm — so there is
/// no caller-supplied key to check against). Shared by
/// [`validate_known_config_keys`] (write-time rejection) and
/// [`unknown_config_keys`] (read-only detection of already-stored nodes via
/// the `graph_audit_node_configs` tool), so the two can never name a
/// different accepted set for the same kind.
pub(crate) fn allowed_config_keys(kind: GraphNodeKind) -> Option<&'static [&'static str]> {
    match kind {
        GraphNodeKind::Agent => Some(AGENT_CONFIG_KEYS),
        GraphNodeKind::Check => Some(CHECK_CONFIG_KEYS),
        GraphNodeKind::Gate => Some(GATE_CONFIG_KEYS),
        GraphNodeKind::Router => Some(ROUTER_CONFIG_KEYS),
        GraphNodeKind::Join => None,
    }
}

/// Every key in `map` that `kind` will never read, sorted. Empty for `Join`
/// (see [`allowed_config_keys`]) and for a config that only carries
/// recognized keys. Pure read-only classification — no error message, no
/// early return — so it doubles as both [`validate_known_config_keys`]'s
/// rejection check and `graph_audit_node_configs`'s detection query over
/// nodes that predate this validation.
pub(crate) fn unknown_config_keys(
    kind: GraphNodeKind,
    map: &serde_json::Map<String, serde_json::Value>,
) -> Vec<String> {
    let Some(allowed) = allowed_config_keys(kind) else {
        return Vec::new();
    };
    let mut unknown: Vec<String> = map
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect();
    unknown.sort_unstable();
    unknown
}

/// Reject any config key a node's kind will never read. This is what makes
/// `{ "platform": "claude", "prompt": "..." }` on an agent node fail loudly
/// at write time instead of being accepted, stored, and silently run on the
/// bare fallback template (`resolve_node_prompt_template`'s default) — the
/// exact incident this check exists to prevent. `prompt` on an agent node
/// gets its own message naming `prompt_template` as the field that actually
/// gets read; every other unrecognized key gets the generic message naming
/// what the kind does accept.
pub(crate) fn validate_known_config_keys(
    kind: GraphNodeKind,
    map: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let unknown = unknown_config_keys(kind, map);
    let Some(bad_key) = unknown.first() else {
        return Ok(());
    };
    // `allowed_config_keys` is `None` only for `Join`, for which
    // `unknown_config_keys` always returns empty — so reaching here means
    // it returned `Some`.
    let allowed = allowed_config_keys(kind).expect("non-empty unknown keys implies Some(allowed)");
    let mut accepted = allowed.to_vec();
    accepted.sort_unstable();

    if kind == GraphNodeKind::Agent && bad_key == "prompt" {
        return Err(format!(
            "Graph node config for kind 'agent' does not read a 'prompt' key; use 'prompt_template' (or 'prompt_preset') instead. Accepted keys: {}.",
            accepted.join(", ")
        ));
    }
    Err(format!(
        "Graph node config for kind '{}' has unrecognized key '{bad_key}'. Accepted keys: {}.",
        kind.as_str(),
        accepted.join(", ")
    ))
}

/// Validate that a graph node's config is a JSON object with the fields its
/// kind needs at execution time. This exists because a double-encoded config
/// (e.g. `"{\"platform\": \"mimo\"}"` instead of `{"platform": "mimo"}`) used
/// to be accepted at creation time and only surfaced as an engine crash
/// ("Agent node ... is missing a platform/cli") mid-run, long after the node
/// was saved. It also rejects any key the node's kind will never read (see
/// [`validate_known_config_keys`]) — the same "surfaced at write time, not
/// run time" guarantee, for the case where the config shape is otherwise
/// valid but carries a key like `prompt` that a typo or a wrong mental model
/// (confusing it with the graph completion hook's own `prompt` field) put
/// there instead of `prompt_template`.
pub(crate) fn validate_node_config(
    kind: GraphNodeKind,
    config: &serde_json::Value,
) -> Result<(), String> {
    let Some(map) = config.as_object() else {
        return Err(format!(
            "Graph node config must be a JSON object, not {}. Pass an object (e.g. {{\"platform\": \"claude\"}}) rather than a JSON-encoded string.",
            json_value_kind_name(config)
        ));
    };

    validate_known_config_keys(kind, map)?;

    let has_non_empty_str = |field: &str| {
        map.get(field)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };

    match kind {
        GraphNodeKind::Agent => {
            if !config_has_agent_harness(map) {
                return Err(
                    "Graph node config for kind 'agent' must include a non-empty 'platform' (or 'cli' or 'crew') field.".to_string(),
                );
            }
            // CM49: `crew` names the harness the same way `platform` does —
            // pass one or the other, never both.
            if has_non_empty_str("crew")
                && (has_non_empty_str("platform") || has_non_empty_str("cli"))
            {
                return Err(
                    "Graph node config must pass 'crew' or 'platform', never both.".to_string(),
                );
            }
            for key in [
                "infra_retry_limit",
                "infra_crash_max_seconds",
                "infra_backoff_seconds",
            ] {
                if let Some(value) = map.get(key) {
                    if value.as_u64().is_none() {
                        return Err(format!(
                            "Graph node config field '{key}' must be a non-negative integer."
                        ));
                    }
                }
            }
        }
        GraphNodeKind::Check => {
            if !has_non_empty_str("command") {
                return Err(
                    "Graph node config for kind 'check' must include a non-empty 'command' field."
                        .to_string(),
                );
            }
        }
        GraphNodeKind::Gate => {
            let evaluate = map
                .get("evaluate")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("output_contains");
            if evaluate == "output_contains" && !has_non_empty_str("value") {
                return Err(
                    "Graph node config for kind 'gate' must include a non-empty 'value' field when 'evaluate' is 'output_contains'.".to_string(),
                );
            }
        }
        // A join node's config is engine-managed (see `graph_add_ensemble`) —
        // there is nothing for a caller to validate, and callers can never
        // reach this arm anyway since `graph_add_node`/`graph_update_node`
        // refuse `kind: "join"` outright.
        GraphNodeKind::Join => {}
        GraphNodeKind::Router => {
            let (routes, fallback) = parse_router_routes(config)?;
            validate_router_routes(&routes, &fallback)?;
            if let Some(value) = map.get("decision") {
                if value.as_bool().is_none() {
                    return Err("Graph node config field 'decision' must be a boolean.".to_string());
                }
            }
            if let Some(value) = map.get("min_confidence") {
                let n = value.as_f64().ok_or_else(|| {
                    "Graph node config field 'min_confidence' must be a number from 0 to 1."
                        .to_string()
                })?;
                if !(0.0..=1.0).contains(&n) || n.is_nan() {
                    return Err(
                        "Graph node config field 'min_confidence' must be a number from 0 to 1."
                            .to_string(),
                    );
                }
            }
            if map.get("decision").and_then(serde_json::Value::as_bool) == Some(true) {
                let platform = map
                    .get("platform")
                    .or_else(|| map.get("cli"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty());
                if let Some(name) = platform {
                    let home = std::env::var_os("CANOPY_HOME_OVERRIDE")
                        .map(std::path::PathBuf::from)
                        .or_else(dirs::home_dir);
                    if let Some(home) = home {
                        let canopy_config =
                            crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
                        if let Some(cli_config) = canopy_config.get_cli(name) {
                            if cli_config.decision_args.is_none() {
                                return Err(format!(
                                    "Router node uses decision mode but platform '{}' has no decision-model invocation in the registry.",
                                    name
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Look up a blueprint by name, or an actionable error listing every
/// available blueprint name (builtin and custom) so the caller can pick a
/// valid one without a separate `blueprint_list` round trip.
pub(crate) fn validate_blueprint_exists(db: &Database, name: &str) -> Result<Blueprint, String> {
    match db.get_blueprint_by_name(name).map_err(|e| e.to_string())? {
        Some(blueprint) => Ok(blueprint),
        None => {
            let available = db
                .list_blueprints()
                .map_err(|e| e.to_string())?
                .iter()
                .map(|b| b.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "Unknown blueprint '{name}'. Available blueprints: {available}."
            ))
        }
    }
}

/// Resolve a `graph_add_node` call's kind/config, either from an explicit
/// `kind`+`config` pair or from a named blueprint (optionally shallow-merged
/// with `config_overrides`). Exactly one of `config`/`blueprint` must be
/// usable — this is the "blueprint as an alternative to a full config"
/// surface described in R7.
pub(crate) fn resolve_node_kind_and_config(
    db: &Database,
    kind: Option<&str>,
    config: Option<serde_json::Map<String, serde_json::Value>>,
    blueprint: Option<&str>,
    config_overrides: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<(GraphNodeKind, serde_json::Value), String> {
    let blueprint_name = blueprint.map(str::trim).filter(|s| !s.is_empty());
    let explicit_kind = kind.map(str::trim).filter(|s| !s.is_empty());

    match blueprint_name {
        Some(blueprint_name) => {
            let bp = validate_blueprint_exists(db, blueprint_name)?;
            let node_kind = match explicit_kind {
                Some(explicit) => validate_node_kind(explicit)?,
                None => bp.kind,
            };
            let overrides = config_overrides.map(serde_json::Value::Object);
            let merged = merge_blueprint_config(&bp.config, overrides.as_ref());
            // Blueprints intentionally carry no platform/cli (see
            // `builtin_blueprint_specs`) — which harness runs a node is the
            // caller's decision, supplied here via `config_overrides`, never
            // a default this resolver falls back to. Catching the missing
            // case here (rather than only via `validate_node_config`'s
            // generic message) lets the error name the blueprint and say
            // *why* the field is missing instead of just that it is.
            if node_kind == GraphNodeKind::Agent {
                let has_harness = merged.as_object().is_some_and(config_has_agent_harness);
                if !has_harness {
                    return Err(format!(
                        "Blueprint '{blueprint_name}' intentionally does not provide a platform (or cli) — which harness runs a node is the caller's decision, not the blueprint's. Pass config_overrides with a non-empty 'platform' (and 'model' if that platform needs one) to select the harness."
                    ));
                }
            }
            Ok((node_kind, merged))
        }
        None => {
            let node_kind = validate_node_kind(
                explicit_kind
                    .ok_or_else(|| "Provide 'kind' when not using a blueprint.".to_string())?,
            )?;
            let config = config
                .map(serde_json::Value::Object)
                .ok_or_else(|| "Provide either 'config' or 'blueprint'.".to_string())?;
            Ok((node_kind, config))
        }
    }
}
