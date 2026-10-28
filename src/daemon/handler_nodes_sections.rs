//! Section helpers for the `graph_node` add/update/audit tools, split out
//! so their home module stays within the size gate. Behaviour unchanged.

use crate::daemon::handler_edges::GraphTarget;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_helpers::record_config_change_if_running;
use crate::daemon::handler_node_lib::{resolve_node_document, resolve_node_kind_and_config};
use crate::daemon::handler_nodes::{unknown_config_keys, validate_node_config};
use crate::daemon::handler_templates::check_updated_node;
use crate::daemon::handler_validate::{
    parse_router_routes, reject_interactive_only_platform, resolve_prefix_or_error,
    validate_node_exists, validate_node_kind, validate_node_mutex_config,
    validate_node_not_ensemble_owned, validate_not_join_kind, validate_topology_mutation_allowed,
};
use crate::daemon::helpers::{error_result, EarlyReturn};
use crate::daemon::params::*;
use crate::db::Database;
use crate::domain::graphs::{
    validate_router_edges_declared, validate_router_route_coverage, GraphEdge, GraphEdgeCondition,
    GraphNode, GraphNodeKind,
};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

pub(crate) fn resolve_add_node_kind_config(
    db: &Database,
    node: Option<&str>,
    kind: Option<&str>,
    config: Option<serde_json::Map<String, serde_json::Value>>,
    blueprint: Option<&str>,
    config_overrides: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<(GraphNodeKind, serde_json::Value), CallToolResult> {
    let node_name = node.map(str::trim).filter(|value| !value.is_empty());
    match node_name {
        Some(node_name) => {
            if config.is_some() || blueprint.is_some() {
                return Err(error_result(
                    "Pass one of 'config', 'blueprint', or 'node', never more than one.",
                ));
            }
            if kind.is_some() {
                return Err(error_result(
                    "Pass 'node' without 'kind': the node document already declares its kind.",
                ));
            }
            match resolve_node_document(node_name, config_overrides) {
                Ok(result) => Ok(result),
                Err(e) => Err(error_result(&e)),
            }
        }
        None => match resolve_node_kind_and_config(db, kind, config, blueprint, config_overrides) {
            Ok(result) => Ok(result),
            Err(e) => Err(error_result(&e)),
        },
    }
}

pub(crate) fn next_node_position(
    db: &Database,
    target: &GraphTarget,
) -> Result<(Option<String>, Option<String>, i64), McpError> {
    match target {
        GraphTarget::Spec(spec_id) => {
            let next_position = db
                .list_graph_nodes(spec_id)
                .map_err(internal_error)?
                .last()
                .map(|node| node.position + 1)
                .unwrap_or(1);
            Ok((Some(spec_id.clone()), None, next_position))
        }
        GraphTarget::Graph(graph_id) => {
            let next_position = db
                .list_graph_nodes_for_graph(graph_id)
                .map_err(internal_error)?
                .last()
                .map(|node| node.position + 1)
                .unwrap_or(1);
            Ok((None, Some(graph_id.clone()), next_position))
        }
    }
}

/// CM2: auto-wire an `Error` edge to the graph's infra node if set.
pub(crate) fn autowire_infra_edge(db: &Database, node: &GraphNode, graph_id: &Option<String>) {
    if let Some(lid) = graph_id {
        if let Ok(Some(lp)) = db.get_graph(lid) {
            if let Some(ref infra_node_id) = lp.infra_node_id {
                let edge = GraphEdge {
                    id: uuid::Uuid::new_v4().to_string(),
                    spec_id: node.spec_id.clone(),
                    graph_id: Some(lid.clone()),
                    from_node: node.id.clone(),
                    to_node: infra_node_id.clone(),
                    condition: GraphEdgeCondition::Error,
                };
                let _ = db.insert_graph_edge(&edge);
            }
        }
    }
}

pub(crate) fn resolve_updatable_node(
    db: &Database,
    params: &GraphUpdateNodeParams,
) -> Result<(String, GraphNode), EarlyReturn> {
    let node_id = match resolve_prefix_or_error(
        db,
        params.node_id.trim(),
        Database::resolve_graph_node_id_by_prefix,
        "node",
    ) {
        Ok(id) => id,
        Err(e) => return Err(EarlyReturn::ok(e)),
    };
    let node = match validate_node_exists(db, &node_id) {
        Ok(node) => node,
        Err(e) => return Err(EarlyReturn::tool_error(&e)),
    };
    if let Err(e) = validate_node_not_ensemble_owned(db, &node_id) {
        return Err(EarlyReturn::tool_error(&e));
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
            db,
            node.spec_id.as_deref(),
            node.graph_id.as_deref(),
        ) {
            return Err(EarlyReturn::tool_error(&e));
        }
    }
    Ok((node_id, node))
}

pub(crate) fn parse_node_name_kind(
    params: &GraphUpdateNodeParams,
) -> Result<(Option<&str>, Option<GraphNodeKind>), CallToolResult> {
    let name = match params.name.as_deref().map(str::trim) {
        Some("") => return Err(error_result("Graph node name must not be empty.")),
        Some(value) => Some(value),
        None => None,
    };
    let kind = match params.kind.as_deref().map(str::trim) {
        Some("") => return Err(error_result("Graph node kind must not be empty.")),
        Some(value) => match validate_node_kind(value) {
            Ok(kind) => Some(kind),
            Err(e) => return Err(error_result(&e)),
        },
        None => None,
    };
    if let Some(kind) = kind {
        if let Err(e) = validate_not_join_kind(kind) {
            return Err(error_result(&e));
        }
    }
    Ok((name, kind))
}

pub(crate) fn merge_node_config(
    stored: &serde_json::Value,
    provided: Option<&serde_json::Map<String, serde_json::Value>>,
    config_replace: bool,
) -> (serde_json::Value, Vec<String>) {
    match (provided, config_replace) {
        (Some(provided), false) => {
            let mut merged = stored.as_object().cloned().unwrap_or_default();
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
            let stored_map = stored.as_object().unwrap_or(&default_map);
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
        (None, _) => (stored.clone(), Vec::new()),
    }
}

pub(crate) fn validate_effective_node_config(
    db: &Database,
    node: &GraphNode,
    node_id: &str,
    kind: Option<GraphNodeKind>,
    name: Option<&str>,
    effective_config: &serde_json::Value,
    config_present: bool,
) -> Result<(), EarlyReturn> {
    if kind.is_some() || config_present {
        let effective_kind = kind.unwrap_or(node.kind);
        if let Err(e) = validate_node_config(effective_kind, effective_config) {
            return Err(EarlyReturn::tool_error(&e));
        }
        let effective_name = name.unwrap_or(&node.name);
        if let Err(e) = validate_node_mutex_config(effective_name, effective_config) {
            return Err(EarlyReturn::tool_error(&e));
        }
        if let Some(msg) = reject_interactive_only_platform(effective_config) {
            return Err(EarlyReturn::tool_error(&msg));
        }
        // A router's routes must stay consistent with whatever edges
        // already name them: no edge left pointing at a route that was
        // just removed, and (once wiring has started) no declared route
        // left without one — see `validate_router_route_coverage`.
        if effective_kind == GraphNodeKind::Router {
            let (routes, _fallback) = match parse_router_routes(effective_config) {
                Ok(parsed) => parsed,
                Err(e) => return Err(EarlyReturn::tool_error(&e)),
            };
            let edges = match (&node.spec_id, &node.graph_id) {
                (Some(spec_id), _) => db.list_graph_edges(spec_id).map_err(internal_error)?,
                (_, Some(graph_id)) => db
                    .list_graph_edges_for_graph(graph_id)
                    .map_err(internal_error)?,
                (None, None) => Vec::new(),
            };
            if let Err(e) = validate_router_edges_declared(&routes, node_id, &edges) {
                return Err(EarlyReturn::tool_error(&e));
            }
            if let Err(e) = validate_router_route_coverage(&routes, node_id, &edges) {
                return Err(EarlyReturn::tool_error(&e));
            }
        }
    }
    if config_present {
        if let Err(e) = check_updated_node(db, node, kind, name, effective_config) {
            return Err(EarlyReturn::tool_error(&e));
        }
    }
    Ok(())
}

/// CM46: a node edit on a running graph is narratable engine history.
pub(crate) fn record_node_config_change(
    db: &Database,
    node: &GraphNode,
    node_id: &str,
    name: Option<&str>,
) {
    let owning_graph_id = node.graph_id.clone().or_else(|| {
        node.spec_id.as_deref().and_then(|sid| {
            db.get_graph_spec(sid)
                .ok()
                .flatten()
                .and_then(|spec| spec.graph_id)
        })
    });
    record_config_change_if_running(
        db,
        owning_graph_id.as_deref(),
        node.spec_id.as_deref(),
        Some(node_id),
        "node",
        format!("Node '{}' config updated.", name.unwrap_or(&node.name)),
    );
}

pub(crate) fn audit_unknown_key_nodes(
    db: &Database,
    nodes: &[GraphNode],
) -> Result<Vec<serde_json::Value>, McpError> {
    let mut flagged = Vec::new();
    for node in nodes {
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
                Some(spec_id) => db
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
    Ok(flagged)
}

pub(crate) fn audit_infra_override_nodes(
    db: &Database,
    nodes: &[GraphNode],
) -> Result<Vec<serde_json::Value>, McpError> {
    let mut infra_overrides = Vec::new();
    for node in nodes {
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
                Some(spec_id) => db
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
    Ok(infra_overrides)
}
