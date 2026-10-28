//! MCP handler tools split out of `daemon::handler` (ensemble entry wiring helpers).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_validate::{
    validate_edge_condition, validate_node_not_ensemble_owned, validate_topology_mutation_allowed,
};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::db::Database;
use crate::domain::graphs::{EnsembleDetails, GraphEdgeCondition, GraphNode};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

/// Shared context for the entry-wiring sub-steps: the database, the call's
/// params, the owner nodes loaded before the member update, and the ensemble
/// id. Bundled to stay within the argument limit; entry ops move edges, never
/// nodes, so `owner_nodes` stays current. The mutable ensemble details travel
/// as a separate parameter.
pub(crate) struct EntryWiring<'a> {
    db: &'a Database,
    params: &'a GraphUpdateEnsembleParams,
    owner_nodes: &'a [GraphNode],
    ensemble_id: &'a str,
}

/// Resolve one caller-named entry source to a node id in the ensemble's
/// graph, refusing ensemble-owned nodes with the chaining hint.
pub(crate) fn resolve_entry_source(
    db: &Database,
    owner_nodes: &[GraphNode],
    raw: &str,
    field: &str,
) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err(format!("{field} must not be empty."));
    }
    let id = db
        .resolve_graph_node_id_by_prefix(raw.trim())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Graph node '{}' not found.", raw.trim()))?;
    if !owner_nodes.iter().any(|node| node.id == id) {
        return Err(format!(
            "Graph node '{id}' not found in the ensemble's graph."
        ));
    }
    if let Err(e) = validate_node_not_ensemble_owned(db, &id) {
        return Err(format!(
            "Cannot wire an ensemble's entry from an ensemble-owned node: {e} To chain ensembles, route the upstream ensemble's exit into this one instead (graph_ensemble on_pass_to/on_fail_to accept an ensemble id)."
        ));
    }
    Ok(id)
}

impl TaskTriggerHandler {
    pub(crate) async fn apply_entry_replace(
        &self,
        db_entry: &EntryWiring<'_>,
        details: &mut EnsembleDetails,
        raw_from: &str,
    ) -> Result<Result<(), CallToolResult>, McpError> {
        let from_node =
            match resolve_entry_source(db_entry.db, db_entry.owner_nodes, raw_from, "from_node") {
                Ok(id) => id,
                Err(e) => return Ok(Err(error_result(&e))),
            };
        let entry_condition = match db_entry.params.condition.as_deref() {
            Some(raw) => match validate_edge_condition(raw.trim()) {
                Ok(condition) => condition,
                Err(e) => return Ok(Err(error_result(&e))),
            },
            None => details.ensemble.entry_condition.clone(),
        };
        if let Err(e) =
            db_entry
                .db
                .rewire_ensemble_entry(db_entry.ensemble_id, &from_node, &entry_condition)
        {
            return Ok(Err(error_result(&e.to_string())));
        }
        details.ensemble.entry_from_node = from_node;
        details.ensemble.entry_condition = entry_condition;
        Ok(Ok(()))
    }

    pub(crate) async fn apply_entry_remove(
        &self,
        db_entry: &EntryWiring<'_>,
        details: &mut EnsembleDetails,
        raw_remove: &str,
    ) -> Result<Result<(), CallToolResult>, McpError> {
        let remove_from = match resolve_entry_source(
            db_entry.db,
            db_entry.owner_nodes,
            raw_remove,
            "remove_entry_from",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(Err(error_result(&e))),
        };
        if let Err(e) = db_entry
            .db
            .remove_ensemble_entry_source(db_entry.ensemble_id, &remove_from)
        {
            return Ok(Err(error_result(&e.to_string())));
        }
        if details.ensemble.entry_from_node == remove_from {
            *details = db_entry
                .db
                .get_ensemble_details(db_entry.ensemble_id)
                .map_err(internal_error)?
                .ok_or_else(|| {
                    internal_error(format!(
                        "Ensemble '{}' vanished mid-update.",
                        db_entry.ensemble_id
                    ))
                })?;
        }
        Ok(Ok(()))
    }

    pub(crate) async fn apply_entry_add(
        &self,
        db_entry: &EntryWiring<'_>,
        raw_add: &str,
    ) -> Result<Result<(), CallToolResult>, McpError> {
        let add_from = match resolve_entry_source(
            db_entry.db,
            db_entry.owner_nodes,
            raw_add,
            "add_entry_from",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(Err(error_result(&e))),
        };
        let add_condition = match db_entry.params.add_entry_condition.as_deref() {
            Some(raw) => match validate_edge_condition(raw.trim()) {
                Ok(condition) => condition,
                Err(e) => return Ok(Err(error_result(&e))),
            },
            None => GraphEdgeCondition::Always,
        };
        if let Err(e) =
            db_entry
                .db
                .add_ensemble_entry_source(db_entry.ensemble_id, &add_from, &add_condition)
        {
            return Ok(Err(error_result(&e.to_string())));
        }
        Ok(Ok(()))
    }

    pub(crate) async fn apply_ensemble_entry_wiring(
        &self,
        ensemble_id: &str,
        params: &GraphUpdateEnsembleParams,
        details: &mut EnsembleDetails,
        owner_nodes: &[GraphNode],
    ) -> Result<Result<(), CallToolResult>, McpError> {
        // ── entry wiring: from_node/condition (replace), add_entry_from,
        // remove_entry_from ─────────────────────────────────────────────
        // Entry edges are the only ensemble wiring `graph_edge` can never
        // express (members refuse direct edges), so they are all managed here
        // as one unit: the caller names source nodes, never member ids.
        if params.from_node.is_none()
            && params.condition.is_none()
            && params.add_entry_from.is_none()
            && params.add_entry_condition.is_none()
            && params.remove_entry_from.is_none()
        {
            return Ok(Ok(()));
        }
        if let Err(e) = validate_topology_mutation_allowed(
            &self.db,
            details.ensemble.spec_id.as_deref(),
            details.ensemble.graph_id.as_deref(),
        ) {
            return Ok(Err(error_result(&e)));
        }
        if params.from_node.is_some()
            && (params.add_entry_from.is_some() || params.remove_entry_from.is_some())
        {
            return Ok(Err(error_result(
                "from_node replaces every entry edge on its own; pass add_entry_from/remove_entry_from in a separate graph_ensemble call.",
            )));
        }
        if params.condition.is_some() && params.from_node.is_none() {
            return Ok(Err(error_result(
                "condition only applies alongside from_node; pass both, or omit condition to keep the current entry condition.",
            )));
        }
        if params.add_entry_condition.is_some() && params.add_entry_from.is_none() {
            return Ok(Err(error_result(
                "add_entry_condition only applies alongside add_entry_from; pass both, or omit add_entry_condition (it defaults to always).",
            )));
        }
        // `owner_nodes` from the member block above is still in scope and
        // still current — entry ops move edges, never nodes.
        let wiring = EntryWiring {
            db: &self.db,
            params,
            owner_nodes,
            ensemble_id,
        };
        if let Some(raw_from) = params.from_node.as_deref() {
            match self.apply_entry_replace(&wiring, details, raw_from).await? {
                Ok(()) => {}
                Err(e) => return Ok(Err(e)),
            }
        }
        if let Some(raw_remove) = params.remove_entry_from.as_deref() {
            match self
                .apply_entry_remove(&wiring, details, raw_remove)
                .await?
            {
                Ok(()) => {}
                Err(e) => return Ok(Err(e)),
            }
        }
        if let Some(raw_add) = params.add_entry_from.as_deref() {
            match self.apply_entry_add(&wiring, raw_add).await? {
                Ok(()) => {}
                Err(e) => return Ok(Err(e)),
            }
        }
        Ok(Ok(()))
    }
}
