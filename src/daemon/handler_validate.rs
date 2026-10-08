//! MCP handler tools split out of `daemon::handler` (existence, topology, and config validation helpers).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::helpers::error_result;
use crate::db::Database;
use crate::domain::graphs::{
    GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphSpec, GraphSpecStatus,
    GraphStatus, RouterRoute,
};
use rmcp::model::CallToolResult;

pub(crate) fn validate_non_empty(value: &str, field: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field} must not be empty."))
    } else {
        Ok(())
    }
}

pub(crate) fn validate_absolute_dir(path: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);
    if !p.is_absolute() {
        return Err("Graph workdir must be an absolute path.".into());
    }
    if !p.is_dir() {
        return Err("Graph workdir must point to an existing directory.".into());
    }
    Ok(())
}

pub(crate) fn validate_spec_workdir(path: &str) -> Result<(), String> {
    if !std::path::Path::new(path).is_absolute() {
        return Err("Spec workdir must be an absolute path.".into());
    }
    Ok(())
}

pub(crate) fn resolve_prefix_or_error(
    db: &Database,
    raw: &str,
    resolver: fn(&Database, &str) -> anyhow::Result<Option<String>>,
    kind_name: &str,
) -> Result<String, CallToolResult> {
    match resolver(db, raw) {
        Ok(Some(full_id)) => Ok(full_id),
        Ok(None) => Err(error_result(&format!("{kind_name} '{raw}' not found."))),
        Err(e) => Err(error_result(&e.to_string())),
    }
}

pub(crate) fn validate_graph_exists(db: &Database, graph_id: &str) -> Result<(), String> {
    db.get_graph(graph_id)
        .map_err(|e| e.to_string())?
        .is_some()
        .then_some(())
        .ok_or_else(|| format!("Graph '{graph_id}' not found."))
}

pub(crate) fn validate_spec_exists(db: &Database, spec_id: &str) -> Result<GraphSpec, String> {
    db.get_graph_spec(spec_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Spec '{spec_id}' not found."))
}

pub(crate) fn validate_node_exists(db: &Database, node_id: &str) -> Result<GraphNode, String> {
    db.get_graph_node(node_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Graph node '{node_id}' not found."))
}

pub(crate) fn validate_edge_exists(db: &Database, edge_id: &str) -> Result<GraphEdge, String> {
    db.get_graph_edge(edge_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Graph edge '{edge_id}' not found."))
}

pub(crate) fn validate_position_conflict(
    db: &Database,
    graph_id: Option<&str>,
    exclude_spec_id: &str,
    position: i64,
) -> Result<(), String> {
    // A standalone spec (no graph yet) has no sibling positions to conflict
    // with — position only matters once it's ordered within a graph.
    let Some(graph_id) = graph_id else {
        return Ok(());
    };
    let conflict = db
        .list_graph_specs(graph_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .any(|s| s.id != exclude_spec_id && s.position == position);
    if conflict {
        Err(format!(
            "Graph '{graph_id}' already has a spec at position {position}."
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn validate_spec_deletable(spec: &GraphSpec) -> Result<(), String> {
    if let Some(graph_id) = &spec.graph_id {
        return Err(format!(
            "Spec '{}' is bound to graph '{graph_id}'; use graph_remove_spec to unbind it from the graph first (or delete the graph), then delete the spec.",
            spec.id
        ));
    }
    Ok(())
}

pub(crate) fn validate_spec_unbindable(_db: &Database, spec: &GraphSpec) -> Result<(), String> {
    if spec.graph_id.is_none() {
        return Err(format!(
            "Spec '{}' is not bound to any graph; nothing to unbind.",
            spec.id
        ));
    }
    if spec.status == GraphSpecStatus::Running {
        return Err(format!(
            "Spec '{}' is currently running and cannot be unbound from its graph; wait for it to finish, or pause the graph, first.",
            spec.id
        ));
    }
    Ok(())
}

pub(crate) fn validate_node_position_conflict(
    db: &Database,
    node: &GraphNode,
    exclude_node_id: &str,
    position: i64,
) -> Result<(), String> {
    let (siblings, owner) = if let Some(spec_id) = &node.spec_id {
        (
            db.list_graph_nodes(spec_id).map_err(|e| e.to_string())?,
            format!("Spec '{spec_id}'"),
        )
    } else if let Some(graph_id) = &node.graph_id {
        (
            db.list_graph_nodes_for_graph(graph_id)
                .map_err(|e| e.to_string())?,
            format!("Graph '{graph_id}'"),
        )
    } else {
        return Ok(());
    };
    let conflict = siblings
        .iter()
        .any(|n| n.id != exclude_node_id && n.position == position);
    if conflict {
        Err(format!(
            "{owner} already has a node at position {position}."
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn validate_edge_condition(condition: &str) -> Result<GraphEdgeCondition, String> {
    GraphEdgeCondition::from_str(condition.trim()).ok_or_else(|| {
        "Graph edge condition must be one of: pass, fail, always, error.".to_string()
    })
}

pub(crate) fn validate_edge_condition_with_route(
    condition: &str,
    route: Option<&str>,
) -> Result<GraphEdgeCondition, String> {
    if condition.trim() == "route" {
        let label = route
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                "Graph edge condition 'route' requires a non-empty 'route' label.".to_string()
            })?;
        return Ok(GraphEdgeCondition::Route(label.to_string()));
    }
    validate_edge_condition(condition).map_err(|_| {
        "Graph edge condition must be one of: pass, fail, always, route, error.".to_string()
    })
}

pub(crate) fn validate_route_edge_target(
    db: &Database,
    from_node_id: &str,
    condition: &GraphEdgeCondition,
) -> Result<(), String> {
    let Some(label) = condition.route_label() else {
        return Ok(());
    };
    let from_node = validate_node_exists(db, from_node_id)?;
    if from_node.kind != GraphNodeKind::Router {
        return Err(format!(
            "Edge condition 'route' requires from_node '{from_node_id}' to be a router node, not '{}'.",
            from_node.kind.display_str()
        ));
    }
    let (routes, _fallback) = parse_router_routes(&from_node.config)?;
    if !routes.iter().any(|route| route.label == label) {
        return Err(format!(
            "Edge names undeclared route '{label}'. Router '{from_node_id}' declares: {}.",
            routes
                .iter()
                .map(|route| route.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(())
}

pub(crate) fn validate_node_kind(kind: &str) -> Result<GraphNodeKind, String> {
    GraphNodeKind::from_str(kind.trim())
        .ok_or_else(|| "Graph node kind must be one of: agent, check, gate, router.".to_string())
}

pub(crate) fn validate_not_join_kind(kind: GraphNodeKind) -> Result<(), String> {
    if kind == GraphNodeKind::Join {
        return Err(
            "Graph node kind 'quorum' is engine-managed; it can only be created via graph_add_ensemble."
                .to_string(),
        );
    }
    Ok(())
}

pub(crate) fn validate_node_not_ensemble_owned(db: &Database, node_id: &str) -> Result<(), String> {
    if let Some(details) = db
        .get_ensemble_by_member_node(node_id)
        .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "Node '{node_id}' is a member of ensemble '{}' ('{}'); edit it via graph_update_ensemble instead (entry wiring: from_node/add_entry_from/remove_entry_from; commit rights: the commit_rights field, applying to whichever member the strategy runs), and delete the whole unit with graph_delete_ensemble instead of this node.",
            details.ensemble.id, details.ensemble.name
        ));
    }
    if let Some(details) = db
        .get_ensemble_by_join_node(node_id)
        .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "Node '{node_id}' is the quorum of ensemble '{}' ('{}'); edit it via graph_update_ensemble instead (exit wiring: on_pass_to/on_fail_to), and delete the whole unit with graph_delete_ensemble instead of this node.",
            details.ensemble.id, details.ensemble.name
        ));
    }
    Ok(())
}

pub(crate) fn validate_node_not_ensemble_referenced(
    db: &Database,
    node_id: &str,
) -> Result<(), String> {
    let refs = db
        .ensembles_referencing_node(node_id)
        .map_err(|e| e.to_string())?;
    if refs.is_empty() {
        return Ok(());
    }
    let parts: Vec<String> = refs
        .iter()
        .map(|(details, role)| {
            format!(
                "Node '{node_id}' is referenced by ensemble '{}' ('{}') as {role}; detach it first with graph_update_ensemble (entry wiring: from_node/add_entry_from/remove_entry_from for entry_from_node/entry_source; exit wiring: on_pass_to/on_fail_to for on_pass_to/on_fail_to). Deleting this node would silently delete the ensemble and orphan its members/join.",
                details.ensemble.name, details.ensemble.id
            )
        })
        .collect();
    Err(parts.join("; "))
}

pub(crate) fn resolve_owning_graph_status(
    db: &Database,
    spec_id: Option<&str>,
    graph_id: Option<&str>,
) -> Result<Option<(String, GraphStatus)>, String> {
    if let Some(graph_id) = graph_id {
        let lp = db
            .get_graph(graph_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Graph '{graph_id}' not found."))?;
        return Ok(Some((lp.id, lp.status)));
    }
    if let Some(spec_id) = spec_id {
        let spec = validate_spec_exists(db, spec_id)?;
        if let Some(graph_id) = &spec.graph_id {
            let lp = db
                .get_graph(graph_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("Graph '{graph_id}' not found."))?;
            return Ok(Some((lp.id, lp.status)));
        }
    }
    Ok(None)
}

pub(crate) fn validate_topology_mutation_allowed(
    db: &Database,
    spec_id: Option<&str>,
    graph_id: Option<&str>,
) -> Result<(), String> {
    if let Some((graph_id, GraphStatus::Running)) =
        resolve_owning_graph_status(db, spec_id, graph_id)?
    {
        return Err(format!(
            "Graph '{graph_id}' is running; call graph_pause first, then retry this topology change."
        ));
    }
    Ok(())
}

pub(crate) fn validate_edge_retarget_destination(
    db: &Database,
    edge: &GraphEdge,
    to_node: &str,
) -> Result<(), String> {
    let nodes = if let Some(spec_id) = &edge.spec_id {
        db.list_graph_nodes(spec_id).map_err(|e| e.to_string())?
    } else if let Some(graph_id) = &edge.graph_id {
        db.list_graph_nodes_for_graph(graph_id)
            .map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };
    if !nodes.iter().any(|n| n.id == to_node) {
        return Err(format!(
            "Node '{to_node}' does not belong to the same graph as edge '{}'; retargeting across specs/graphs is not allowed.",
            edge.id
        ));
    }
    Ok(())
}

pub(crate) fn validate_node_not_entry_point(db: &Database, node: &GraphNode) -> Result<(), String> {
    let (nodes, edges) = if let Some(spec_id) = &node.spec_id {
        (
            db.list_graph_nodes(spec_id).map_err(|e| e.to_string())?,
            db.list_graph_edges(spec_id).map_err(|e| e.to_string())?,
        )
    } else if let Some(graph_id) = &node.graph_id {
        (
            db.list_graph_nodes_for_graph(graph_id)
                .map_err(|e| e.to_string())?,
            db.list_graph_edges_for_graph(graph_id)
                .map_err(|e| e.to_string())?,
        )
    } else {
        return Ok(());
    };
    if let Ok(entry_id) = crate::graph_engine::find_entry_node(&nodes, &edges, "graph") {
        if entry_id == node.id {
            return Err(format!(
                "Node '{}' is the entry point of its graph; deleting it would leave the graph \
                 unable to run. Wire a different node as the entry first.",
                node.id
            ));
        }
    }
    Ok(())
}

pub(crate) fn retarget_graph_edge(
    db: &Database,
    edge_id: &str,
    to_node: &str,
) -> Result<GraphEdge, String> {
    let edge = validate_edge_exists(db, edge_id)?;
    validate_topology_mutation_allowed(db, edge.spec_id.as_deref(), edge.graph_id.as_deref())?;
    validate_node_not_ensemble_owned(db, &edge.from_node)?;
    validate_node_not_ensemble_owned(db, to_node)?;
    validate_edge_retarget_destination(db, &edge, to_node)?;
    db.update_graph_edge_target(&edge.id, to_node)
        .map_err(|e| e.to_string())?;
    Ok(GraphEdge {
        to_node: to_node.to_string(),
        ..edge
    })
}

pub(crate) fn delete_graph_edge_checked(db: &Database, edge_id: &str) -> Result<GraphEdge, String> {
    let edge = validate_edge_exists(db, edge_id)?;
    validate_topology_mutation_allowed(db, edge.spec_id.as_deref(), edge.graph_id.as_deref())?;
    db.delete_graph_edge(&edge.id).map_err(|e| e.to_string())?;
    Ok(edge)
}

pub(crate) fn delete_graph_node_checked(db: &Database, node_id: &str) -> Result<GraphNode, String> {
    let node = validate_node_exists(db, node_id)?;
    validate_topology_mutation_allowed(db, node.spec_id.as_deref(), node.graph_id.as_deref())?;
    validate_node_not_ensemble_owned(db, &node.id)?;
    validate_node_not_ensemble_referenced(db, &node.id)?;
    validate_node_not_entry_point(db, &node)?;
    db.delete_graph_node(&node.id).map_err(|e| e.to_string())?;
    Ok(node)
}

pub(crate) fn validate_spec_status(status: &str) -> Result<GraphSpecStatus, String> {
    match status.trim().to_lowercase().as_str() {
        "pending" => Ok(GraphSpecStatus::Pending),
        "running" => Ok(GraphSpecStatus::Running),
        "completed" => Ok(GraphSpecStatus::Completed),
        "failed" => Ok(GraphSpecStatus::Failed),
        "skipped" => Ok(GraphSpecStatus::Skipped),
        "interrupted" => Ok(GraphSpecStatus::Interrupted),
        _ => Err(
            "Spec status must be one of: pending, running, completed, failed, skipped, interrupted."
                .to_string(),
        ),
    }
}

pub(crate) fn validate_spec_set_status_target(status: &str) -> Result<GraphSpecStatus, String> {
    match status.trim().to_lowercase().as_str() {
        "pending" => Ok(GraphSpecStatus::Pending),
        "completed" => Ok(GraphSpecStatus::Completed),
        "skipped" => Ok(GraphSpecStatus::Skipped),
        _ => Err("Spec set_status target must be one of: pending, completed, skipped.".to_string()),
    }
}

pub(crate) fn json_value_kind_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a JSON-encoded string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

pub(crate) fn parse_router_routes(
    config: &serde_json::Value,
) -> Result<(Vec<RouterRoute>, String), String> {
    let map = config.as_object().ok_or_else(|| {
        format!(
            "Graph node config must be a JSON object, not {}. Pass an object (e.g. {{\"routes\": [...]}}) rather than a JSON-encoded string.",
            json_value_kind_name(config)
        )
    })?;
    let routes_value = map.get("routes").ok_or_else(|| {
        "Graph node config for kind 'router' must include a 'routes' array.".to_string()
    })?;
    let routes_array = routes_value
        .as_array()
        .ok_or_else(|| "Graph node config field 'routes' must be an array.".to_string())?;
    let routes = routes_array
        .iter()
        .map(|entry| {
            let obj = entry.as_object().ok_or_else(|| {
                "Each router route must be an object with 'label' and 'description' fields."
                    .to_string()
            })?;
            let label = obj
                .get("label")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let description = obj
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(RouterRoute { label, description })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let fallback = map
        .get("fallback")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok((routes, fallback))
}
