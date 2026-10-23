//! MCP handler tools split out of `daemon::handler` (edge/node-topology tools and graph-target resolution).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_validate::{
    delete_graph_edge_checked, delete_graph_node_checked, resolve_prefix_or_error,
    retarget_graph_edge, validate_edge_condition_with_route, validate_edge_exists,
    validate_graph_exists, validate_node_not_ensemble_owned, validate_route_edge_target,
    validate_spec_exists,
};
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::{GraphEdge, GraphNode};
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_edges, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_add_edge",
        description = "Connect two nodes with a routing condition, inside a graph spec's graph or (via graph_id instead of spec_id) the graph's top-level graph."
    )]
    pub(crate) async fn graph_add_edge(
        &self,
        Parameters(params): Parameters<GraphAddEdgeParams>,
    ) -> Result<CallToolResult, McpError> {
        let condition = match validate_edge_condition_with_route(
            params.condition.trim(),
            params.route.as_deref(),
        ) {
            Ok(c) => c,
            Err(e) => return Ok(error_result(&e)),
        };
        let from_node = match resolve_prefix_or_error(
            &self.db,
            params.from_node.trim(),
            Database::resolve_graph_node_id_by_prefix,
            "node",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let to_node = match resolve_prefix_or_error(
            &self.db,
            params.to_node.trim(),
            Database::resolve_graph_node_id_by_prefix,
            "node",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let target = match resolve_graph_target(
            &self.db,
            params.spec_id.as_deref(),
            params.graph_id.as_deref(),
        ) {
            Ok(target) => target,
            Err(e) => return Ok(error_result(&e)),
        };

        let nodes = match &target {
            GraphTarget::Spec(spec_id) => {
                self.db.list_graph_nodes(spec_id).map_err(internal_error)?
            }
            GraphTarget::Graph(graph_id) => self
                .db
                .list_graph_nodes_for_graph(graph_id)
                .map_err(internal_error)?,
        };
        let has_from = nodes.iter().any(|node| node.id == from_node);
        let has_to = nodes.iter().any(|node| node.id == to_node);
        if !has_from || !has_to {
            return Ok(error_result(
                "Both graph edge endpoints must belong to the same spec or graph as the edge.",
            ));
        }
        if let Err(e) = validate_node_not_ensemble_owned(&self.db, &from_node) {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_node_not_ensemble_owned(&self.db, &to_node) {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_route_edge_target(&self.db, &from_node, &condition) {
            return Ok(error_result(&e));
        }

        let (spec_id, graph_id) = match target {
            GraphTarget::Spec(spec_id) => (Some(spec_id), None),
            GraphTarget::Graph(graph_id) => (None, Some(graph_id)),
        };
        let edge = GraphEdge {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id,
            graph_id,
            from_node,
            to_node,
            condition,
        };
        self.db.insert_graph_edge(&edge).map_err(internal_error)?;

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&serde_json::json!({ "ok": true })).unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "graph_update_edge",
        description = "Update the routing condition and/or destination node of an existing graph edge. Omit to_node to leave the edge's target unchanged; provide it to retarget the edge in place (preserving its id and run history) instead of recreating it."
    )]
    pub(crate) async fn graph_update_edge(
        &self,
        Parameters(params): Parameters<GraphUpdateEdgeParams>,
    ) -> Result<CallToolResult, McpError> {
        let edge_id = match resolve_prefix_or_error(
            &self.db,
            params.edge_id.trim(),
            Database::resolve_edge_id_by_prefix,
            "edge",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let edge = match validate_edge_exists(&self.db, &edge_id) {
            Ok(e) => e,
            Err(e) => return Ok(error_result(&e)),
        };
        if let Err(e) = validate_node_not_ensemble_owned(&self.db, &edge.from_node) {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_node_not_ensemble_owned(&self.db, &edge.to_node) {
            return Ok(error_result(&e));
        }
        let condition = match validate_edge_condition_with_route(
            params.condition.trim(),
            params.route.as_deref(),
        ) {
            Ok(c) => c,
            Err(e) => return Ok(error_result(&e)),
        };
        if let Err(e) = validate_route_edge_target(&self.db, &edge.from_node, &condition) {
            return Ok(error_result(&e));
        }

        let to_node = match &params.to_node {
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    match resolve_prefix_or_error(
                        &self.db,
                        trimmed,
                        Database::resolve_graph_node_id_by_prefix,
                        "node",
                    ) {
                        Ok(id) => Some(id),
                        Err(e) => return Ok(e),
                    }
                }
            }
            None => None,
        };
        let target_changed = to_node
            .as_deref()
            .is_some_and(|value| value != edge.to_node);
        if target_changed {
            if let Err(e) = retarget_graph_edge(&self.db, &edge.id, to_node.as_deref().unwrap()) {
                return Ok(error_result(&e));
            }
        }

        let condition_changed = edge.condition != condition;
        if condition_changed {
            self.db
                .update_graph_edge_condition(&edge.id, &condition)
                .map_err(internal_error)?;
        }

        if !condition_changed && !target_changed {
            return Ok(success_result(&format!(
                "Graph edge '{}' already uses condition '{}' and target unchanged.",
                edge.id,
                condition.as_str()
            )));
        }

        Ok(success_result(&format!(
            "Graph edge '{}' updated.",
            edge.id
        )))
    }

    #[tool(
        name = "graph_delete_edge",
        description = "Delete a single graph edge by id. Rejected while the owning graph is running — pause it first."
    )]
    pub(crate) async fn graph_delete_edge(
        &self,
        Parameters(params): Parameters<GraphDeleteEdgeParams>,
    ) -> Result<CallToolResult, McpError> {
        let edge_id = match resolve_prefix_or_error(
            &self.db,
            params.edge_id.trim(),
            Database::resolve_edge_id_by_prefix,
            "edge",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        match delete_graph_edge_checked(&self.db, &edge_id) {
            Ok(edge) => Ok(success_result(&format!(
                "Graph edge '{}' deleted.",
                edge.id
            ))),
            Err(e) => Ok(error_result(&e)),
        }
    }

    #[tool(
        name = "graph_delete_node",
        description = "Delete a graph node by id, cascading to every edge that names it as from_node or to_node. Rejected if the node is the graph's entry point, or while the owning graph is running — pause it first."
    )]
    pub(crate) async fn graph_delete_node(
        &self,
        Parameters(params): Parameters<GraphDeleteNodeParams>,
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
        match delete_graph_node_checked(&self.db, &node_id) {
            Ok(node) => Ok(success_result(&format!(
                "Graph node '{}' deleted.",
                node.id
            ))),
            Err(e) => Ok(error_result(&e)),
        }
    }
}

/// Where a graph node/edge belongs: a spec's own graph, or a graph's
/// top-level graph (shared across every spec in that graph).
#[derive(Debug)]
pub(crate) enum GraphTarget {
    Spec(String),
    Graph(String),
}

/// Resolve `spec_id`/`graph_id` MCP params into exactly one validated
/// [`GraphTarget`]. Empty/whitespace-only strings are treated as absent, so
/// a client that always sends both fields (one blank) still gets a clean
/// "provide exactly one" error instead of a confusing "not found".
pub(crate) fn resolve_graph_target(
    db: &Database,
    spec_id: Option<&str>,
    graph_id: Option<&str>,
) -> Result<GraphTarget, String> {
    let spec_id = spec_id.map(str::trim).filter(|s| !s.is_empty());
    let graph_id = graph_id.map(str::trim).filter(|s| !s.is_empty());
    match (spec_id, graph_id) {
        (Some(_), Some(_)) => {
            Err("Provide exactly one of spec_id or graph_id, not both.".to_string())
        }
        (None, None) => Err("Provide exactly one of spec_id or graph_id.".to_string()),
        (Some(spec_id), None) => {
            let resolved = db
                .resolve_spec_id_by_prefix(spec_id, None)
                .map_err(|e| e.to_string())?;
            let full_id = resolved.as_deref().unwrap_or(spec_id);
            validate_spec_exists(db, full_id)?;
            Ok(GraphTarget::Spec(full_id.to_string()))
        }
        (None, Some(graph_id)) => {
            let resolved = db
                .resolve_graph_id_by_prefix(graph_id)
                .map_err(|e| e.to_string())?;
            let full_id = resolved.as_deref().unwrap_or(graph_id);
            validate_graph_exists(db, full_id)?;
            Ok(GraphTarget::Graph(full_id.to_string()))
        }
    }
}

/// A graph target split into `(spec_id, graph_id, existing_nodes)` — exactly one
/// of the ids is set. Returned by [`graph_target_parts`].
pub(crate) type GraphParts = (Option<String>, Option<String>, Vec<GraphNode>);

/// Resolve the target graph for a copy: an explicit `spec_id`/`graph_id`, or —
/// when both are absent — the source's own graph. Cross-graph copies are
/// allowed; the source graph is only a default, never a constraint.
pub(crate) fn resolve_copy_target(
    db: &Database,
    spec_id: Option<&str>,
    graph_id: Option<&str>,
    source_spec_id: &Option<String>,
    source_graph_id: &Option<String>,
) -> Result<GraphTarget, String> {
    let spec_id = spec_id.map(str::trim).filter(|s| !s.is_empty());
    let graph_id = graph_id.map(str::trim).filter(|s| !s.is_empty());
    match (spec_id, graph_id) {
        (Some(_), Some(_)) => Err("Provide at most one of spec_id or graph_id.".to_string()),
        (Some(spec_id), None) => {
            let resolved = db
                .resolve_spec_id_by_prefix(spec_id, None)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("Spec '{spec_id}' not found."))?;
            validate_spec_exists(db, &resolved)?;
            Ok(GraphTarget::Spec(resolved))
        }
        (None, Some(graph_id)) => {
            let resolved = db
                .resolve_graph_id_by_prefix(graph_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("Graph '{graph_id}' not found."))?;
            validate_graph_exists(db, &resolved)?;
            Ok(GraphTarget::Graph(resolved))
        }
        (None, None) => {
            if let Some(spec_id) = source_spec_id {
                Ok(GraphTarget::Spec(spec_id.clone()))
            } else if let Some(graph_id) = source_graph_id {
                Ok(GraphTarget::Graph(graph_id.clone()))
            } else {
                Err(
                    "Source belongs to no graph and no target spec_id/graph_id was given."
                        .to_string(),
                )
            }
        }
    }
}

/// Split a [`GraphTarget`] into `(spec_id, graph_id)` (exactly one set) plus the
/// target graph's current nodes — used by the copy planners to place the copy
/// at the next free position and to validate wiring targets against that graph.
pub(crate) fn graph_target_parts(
    db: &Database,
    target: &GraphTarget,
) -> Result<GraphParts, String> {
    match target {
        GraphTarget::Spec(spec_id) => {
            let nodes = db.list_graph_nodes(spec_id).map_err(|e| e.to_string())?;
            Ok((Some(spec_id.clone()), None, nodes))
        }
        GraphTarget::Graph(graph_id) => {
            let nodes = db
                .list_graph_nodes_for_graph(graph_id)
                .map_err(|e| e.to_string())?;
            Ok((None, Some(graph_id.clone()), nodes))
        }
    }
}
