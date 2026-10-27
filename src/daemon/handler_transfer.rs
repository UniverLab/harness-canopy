//! MCP handler tools split out of `daemon::handler` (graph export/import tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::{build_json_result, internal_error, write_error};
use crate::daemon::handler_nodes::validate_known_config_keys;
use crate::daemon::handler_validate::{
    resolve_prefix_or_error, validate_absolute_dir, validate_non_empty,
};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::{Graph, GraphStatus};
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_transfer, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_export",
        description = "Export a graph's design — name, description, nodes, edges, and ensembles — as a portable JSON document, so it can be shared as a file and recreated elsewhere with graph_import. Never includes ids, workdir, specs, or run/status state. Includes platform/model for every agent node and ensemble member, and each ensemble's kind ('cascade'/'round_robin'; omitted for the 'parallel' default); format_version 5. A crew-bound ensemble exports its 'crew' name and no members, and a crew-bound agent node exports its 'crew' key. Ensembles carry every entry source: the primary in 'entry_from_node'/'entry_condition' plus 'extra_entry_sources' (node + condition) for sources added with 'add_entry_from' (omitted when there are none)."
    )]
    pub(crate) async fn graph_export(
        &self,
        Parameters(params): Parameters<GraphExportParams>,
    ) -> Result<CallToolResult, McpError> {
        let graph_id = match resolve_prefix_or_error(
            &self.db,
            params.graph_id.trim(),
            Database::resolve_graph_id_by_prefix,
            "graph",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let Some(lp) = self.db.get_graph(&graph_id).map_err(internal_error)? else {
            return Ok(error_result(&format!("Graph '{graph_id}' not found.")));
        };
        let graph_nodes = self
            .db
            .list_graph_nodes_for_graph(&graph_id)
            .map_err(internal_error)?;
        let graph_edges = self
            .db
            .list_graph_edges_for_graph(&graph_id)
            .map_err(internal_error)?;
        let ensembles = self
            .db
            .list_ensembles_for_graph(&graph_id)
            .map_err(internal_error)?;
        // CM49: a crew-bound ensemble whose members are not materialised yet
        // has no entry-fan-out edges; its entry wiring lives in the pending
        // rows an import recorded, so export must read them alongside.
        let pending_entry_sources = self
            .db
            .pending_entry_sources_for_graph(&graph_id)
            .map_err(internal_error)?;

        let document = match crate::domain::graph_transfer::build_export_document_with_pending(
            &lp,
            &graph_nodes,
            &graph_edges,
            &ensembles,
            &pending_entry_sources,
        ) {
            Ok(document) => document,
            Err(e) => return Ok(error_result(&e)),
        };

        Ok(build_json_result(
            &serde_json::to_value(&document).map_err(internal_error)?,
        ))
    }

    #[tool(
        name = "graph_import",
        description = "Create a new graph from an exported document (the object graph_export returns). Always creates a new graph — never updates or overwrites an existing one; if the name is already taken in workdir, a numeric suffix is applied and the response says which name was used. Validates the document exactly as graph_add_node/graph_add_edge/graph_add_ensemble would, all-or-nothing: nothing is written if any part is rejected. It also refuses an agent node's prompt_preset when this machine cannot resolve it — no `<name>.md` in the prompts dir and not a builtin (implementer, reviewer, resilience) — naming every unknown preset in the error, because the imported node would otherwise run on an empty prompt. Accepts format_version 1 (members with no binding, reported as missing a platform), 2 (bindings restored), 3 (graph/error vocabulary), 4 (every ensemble entry source: 'extra_entry_sources' restores each 'add_entry_from' source with its own condition), and 5 (crew-bound ensembles and nodes: 'crew' restores the binding when a crew of that name exists locally, else the response lists it under 'missing_crews' and the ensemble cannot dispatch until the crew is created), and 6 (ensemble 'mutex': restored per member run; older documents import with none). Restores each ensemble's kind, commit_rights, mutex, timeouts, member overrides, and entry/exit wiring exactly; a document field this build does not know how to restore — including an unknown 'kind' value — is a refusal naming the field, never a silent default. The response lists every agent node left without a platform, and every crew an imported ensemble references that does not exist locally, so the caller knows what to fill in before running the graph."
    )]
    pub(crate) async fn graph_import(
        &self,
        Parameters(params): Parameters<GraphImportParams>,
    ) -> Result<CallToolResult, McpError> {
        let workdir = params.workdir.trim();
        if let Err(e) = validate_non_empty(workdir, "Graph workdir") {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_absolute_dir(workdir) {
            return Ok(error_result(&e));
        }

        let document =
            match crate::domain::graph_transfer::parse_export_document_value(&params.document) {
                Ok(document) => document,
                Err(e) => return Ok(error_result(&e)),
            };

        for node in &document.nodes {
            if let Some(map) = node.config.as_object() {
                if let Err(e) = validate_known_config_keys(node.kind, map) {
                    return Ok(error_result(&e));
                }
            }
            if let Err(e) = crate::daemon::handler_validate::validate_node_mutex_config(
                &node.name,
                &node.config,
            ) {
                return Ok(error_result(&e));
            }
        }

        let desired_name = params
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(document.name.trim());
        if let Err(e) = validate_non_empty(desired_name, "Graph name") {
            return Ok(error_result(&e));
        }

        let graph_id = uuid::Uuid::new_v4().to_string();
        let plan = match crate::domain::graph_transfer::build_import_plan(&document, &graph_id) {
            Ok(plan) => plan,
            Err(e) => return Ok(error_result(&e)),
        };
        let missing_platform = crate::domain::graph_transfer::agent_nodes_missing_platform(&plan);

        let existing_names: Vec<String> = self
            .db
            .list_graphs(Some(workdir), true)
            .map_err(internal_error)?
            .into_iter()
            .map(|lp| lp.name)
            .collect();
        let final_name =
            crate::domain::graph_transfer::resolve_unique_graph_name(&existing_names, desired_name);

        let lp = Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: plan.infra_node_id.clone(),
            id: graph_id.clone(),
            name: final_name.clone(),
            description: document
                .description
                .clone()
                .filter(|value| !value.trim().is_empty()),
            workdir: workdir.to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        };

        let missing_crews = self
            .db
            .import_graph(&lp, &plan)
            .map_err(|e| write_error("graph_import", &e))?;
        if crate::domain::project::should_auto_register(std::path::Path::new(workdir)) {
            if let Err(error) = self.db.register_project_path(std::path::Path::new(workdir)) {
                tracing::debug!(
                    "Could not register imported graph's project at {workdir}: {error}"
                );
            }
        }
        self.activate_graph_trigger(&lp).await;

        Ok(build_json_result(&serde_json::json!({
            "graph_id": lp.id,
            "name": lp.name,
            "nodes_missing_platform": missing_platform,
            "missing_crews": missing_crews,
            "terminals": plan.terminals,
        })))
    }
}
