//! MCP handler tools split out of `daemon::handler` (ensemble exit wiring helpers).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_ensemble_plan::{resolve_chained_exit_ensemble, EnsembleExitTarget};
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_validate::{
    validate_node_not_ensemble_owned, validate_topology_mutation_allowed,
};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::db::Database;
use crate::domain::graphs::{EnsembleDetails, GraphEdgeCondition, GraphNode};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

/// Shared context for the exit-wiring sub-steps (read-only: exit ops only
/// ever retarget the join's outgoing edges).
pub(crate) struct ExitWiring<'a> {
    db: &'a Database,
    details: &'a EnsembleDetails,
    owner_nodes: &'a [GraphNode],
    ensemble_id: &'a str,
}

/// Resolve one caller-named exit target: another ensemble's id chains the
/// quorum into it, otherwise a plain node id in the ensemble's graph.
pub(crate) fn resolve_exit_target(
    db: &Database,
    ensemble_id: &str,
    details: &EnsembleDetails,
    owner_nodes: &[GraphNode],
    raw: &str,
    field: &str,
) -> Result<EnsembleExitTarget, String> {
    match resolve_chained_exit_ensemble(
        db,
        raw.trim(),
        ensemble_id,
        details.ensemble.spec_id.as_deref(),
        details.ensemble.graph_id.as_deref(),
    ) {
        Ok(Some(target)) => Ok(target),
        Ok(None) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(format!("{field} must not be empty."));
            }
            if !owner_nodes.iter().any(|node| node.id == trimmed) {
                return Err(format!(
                    "Graph node '{trimmed}' not found in the ensemble's graph."
                ));
            }
            if let Err(e) = validate_node_not_ensemble_owned(db, trimmed) {
                return Err(format!(
                        "Cannot wire an ensemble's exit directly into another ensemble's members/quorum: {e} To route into the whole ensemble, pass its ensemble id as {field} instead of a member/quorum node id."
                    ));
            }
            Ok(EnsembleExitTarget::Node(trimmed.to_string()))
        }
        Err(e) => Err(e),
    }
}

impl TaskTriggerHandler {
    pub(crate) async fn apply_exit_pass_target(
        &self,
        exit: &ExitWiring<'_>,
        target_raw: &str,
    ) -> Result<Result<(), CallToolResult>, McpError> {
        let target = match resolve_exit_target(
            exit.db,
            exit.ensemble_id,
            exit.details,
            exit.owner_nodes,
            target_raw,
            "on_pass_to",
        ) {
            Ok(target) => target,
            Err(e) => return Ok(Err(error_result(&e))),
        };
        if let Err(e) = exit.db.retarget_ensemble_exit(
            exit.ensemble_id,
            &GraphEdgeCondition::Pass,
            &target.edge_targets(),
            Some(&target.row_target()),
        ) {
            return Ok(Err(error_result(&e.to_string())));
        }
        Ok(Ok(()))
    }

    pub(crate) async fn apply_exit_fail_target(
        &self,
        exit: &ExitWiring<'_>,
        on_fail_to: &Option<String>,
    ) -> Result<Result<(), CallToolResult>, McpError> {
        match on_fail_to
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            None => {
                // Clear to a dead end on fail.
                exit.db
                    .delete_graph_edges_from_node_with_condition(
                        &exit.details.ensemble.join_node_id,
                        &GraphEdgeCondition::Fail,
                    )
                    .map_err(internal_error)?;
                exit.db
                    .update_ensemble_exit_wiring(exit.ensemble_id, None, Some(None))
                    .map_err(internal_error)?;
            }
            Some(target_raw) => {
                let target = match resolve_exit_target(
                    exit.db,
                    exit.ensemble_id,
                    exit.details,
                    exit.owner_nodes,
                    target_raw,
                    "on_fail_to",
                ) {
                    Ok(target) => target,
                    Err(e) => return Ok(Err(error_result(&e))),
                };
                if let Err(e) = exit.db.retarget_ensemble_exit(
                    exit.ensemble_id,
                    &GraphEdgeCondition::Fail,
                    &target.edge_targets(),
                    Some(&target.row_target()),
                ) {
                    return Ok(Err(error_result(&e.to_string())));
                }
            }
        }
        Ok(Ok(()))
    }

    pub(crate) async fn apply_ensemble_exit_wiring(
        &self,
        ensemble_id: &str,
        params: &GraphUpdateEnsembleParams,
        details: &EnsembleDetails,
    ) -> Result<Result<(), CallToolResult>, McpError> {
        // ── exit wiring: on_pass_to / on_fail_to ──────────────────────
        // Each target is a plain node id — or another ensemble's id, chaining
        // the quorum directly into that ensemble (one edge per member, no
        // intermediate node). Both go through the atomic
        // `retarget_ensemble_exit`, so a failed retarget leaves the previous
        // edges intact.
        if params.on_pass_to.is_none() && params.on_fail_to.is_none() {
            return Ok(Ok(()));
        }
        // CM15 / FR5: exit edges are graph TOPOLOGY. The engine routes a
        // finished quorum with `select_next_step` against the per-run
        // `edges` snapshot (see `GraphEngine::run_spec`), which is captured
        // once at launch and never re-read — so an `on_pass_to`/`on_fail_to`
        // retarget mid-run would return success and change nothing until the
        // next launch. Refuse it while running, exactly as entry wiring is
        // refused above, and name the graph's state.
        if let Err(e) = validate_topology_mutation_allowed(
            &self.db,
            details.ensemble.spec_id.as_deref(),
            details.ensemble.graph_id.as_deref(),
        ) {
            return Ok(Err(error_result(&e)));
        }
        let owner_nodes = match (&details.ensemble.spec_id, &details.ensemble.graph_id) {
            (Some(spec_id), None) => self.db.list_graph_nodes(spec_id).map_err(internal_error)?,
            (None, Some(graph_id)) => self
                .db
                .list_graph_nodes_for_graph(graph_id)
                .map_err(internal_error)?,
            _ => Vec::new(),
        };
        let exit = ExitWiring {
            db: &self.db,
            details,
            owner_nodes: &owner_nodes,
            ensemble_id,
        };
        if let Some(on_pass_to) = params.on_pass_to.as_deref() {
            match self.apply_exit_pass_target(&exit, on_pass_to).await? {
                Ok(()) => {}
                Err(e) => return Ok(Err(e)),
            }
        }
        if let Some(on_fail_to) = params.on_fail_to.as_ref() {
            match self.apply_exit_fail_target(&exit, on_fail_to).await? {
                Ok(()) => {}
                Err(e) => return Ok(Err(e)),
            }
        }
        Ok(Ok(()))
    }
}
