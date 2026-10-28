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
use crate::db::Database;
use crate::domain::graphs::{Graph, GraphStatus};
use rmcp::model::CallToolResult;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_transfer, vis = "pub(crate)")]
impl TaskTriggerHandler {
    pub(crate) async fn graph_export_impl(
        &self,
        params: GraphExportParams,
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

    pub(crate) async fn graph_import_impl(
        &self,
        params: GraphImportParams,
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

        match self
            .import_document_value(&document, workdir, params.name.as_deref(), "graph_edit")
            .await
        {
            ImportedDocument::Done(value) => Ok(build_json_result(&value)),
            ImportedDocument::Rejected(result) => Ok(result),
            ImportedDocument::Failed(error) => Err(error),
        }
    }

    pub(crate) async fn graph_install_impl(
        &self,
        params: GraphInstallParams,
    ) -> Result<CallToolResult, McpError> {
        let workdir = params.workdir.trim();
        if let Err(e) = validate_non_empty(workdir, "Graph workdir") {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_absolute_dir(workdir) {
            return Ok(error_result(&e));
        }

        // The fetch is blocking I/O, so it runs off the async runtime. It
        // writes nothing: the review comes first. A draft is consumed by
        // the review fetch, so the accept call reuses the parked body —
        // with no parked body it re-fetches and the 404 says the draft
        // is gone.
        let accepted = params.accept == Some(true);
        let reference = params.reference.clone();
        let cached = if accepted {
            crate::daemon::graph_install::draft_id(&reference)
                .and_then(|id| crate::daemon::draft_pending::take(&id))
        } else {
            None
        };
        let fetched = tokio::task::spawn_blocking(move || match cached {
            Some((body, source_url)) => {
                crate::daemon::graph_install::graph_from_body(&reference, &body, source_url)
            }
            None => crate::daemon::graph_install::fetch_graph(
                &crate::daemon::graph_install::catalogue_url(),
                &reference,
            ),
        })
        .await
        .map_err(|error| internal_error(error.to_string()))?;
        let fetched = match fetched {
            Ok(fetched) => fetched,
            Err(message) => return Ok(error_result(&message)),
        };

        let review = crate::domain::nodes::review::review_graph(&fetched.document, &fetched.source);
        if !accepted {
            if let Some(id) = crate::daemon::graph_install::draft_id(&params.reference) {
                crate::daemon::draft_pending::store(
                    &id,
                    fetched.body.clone(),
                    fetched.source_url.clone(),
                );
            }
            return Ok(build_json_result(&serde_json::json!({
                "needs_review": true,
                "review": review,
            })));
        }

        // Only after the caller accepted does anything touch disk.
        let overwrite = params.overwrite_presets.unwrap_or(false);
        let document = fetched.document.clone();
        let presets_written = tokio::task::spawn_blocking(move || {
            let canopy_dir = crate::domain::prompts::canopy_dir();
            crate::daemon::graph_install::write_presets(
                &document,
                overwrite,
                &crate::domain::prompts::prompts_dir(&canopy_dir),
            )
        })
        .await
        .map_err(|error| internal_error(error.to_string()))?;
        let presets_written = match presets_written {
            Ok(written) => written,
            Err(message) => return Ok(error_result(&message)),
        };

        let name = params.name.clone();
        match self
            .import_document_value(&fetched.document, workdir, name.as_deref(), "graph_edit")
            .await
        {
            ImportedDocument::Done(mut value) => {
                value["workdir"] = serde_json::json!(workdir);
                value["ref"] = serde_json::json!(params.reference);
                value["source_url"] = serde_json::json!(fetched.source_url);
                value["source"] = serde_json::json!(fetched.source);
                value["presets_written"] = serde_json::json!(presets_written);
                value["review"] = serde_json::to_value(&review).unwrap_or_default();
                value["next"] = serde_json::json!(install_next_step(&value));
                Ok(build_json_result(&value))
            }
            ImportedDocument::Rejected(result) => Ok(result),
            ImportedDocument::Failed(error) => Err(error),
        }
    }

    pub(crate) async fn graph_validate_impl(
        &self,
        params: GraphValidateParams,
    ) -> Result<CallToolResult, McpError> {
        match crate::daemon::graph_validate::validate_request(params.path, params.document) {
            Ok(report) => Ok(build_json_result(&report)),
            Err(e) => Ok(error_result(&e)),
        }
    }
}

/// The outcome of importing one parsed document: a success payload, a
/// caller-facing refusal, or an infrastructure failure.
enum ImportedDocument {
    Done(serde_json::Value),
    Rejected(CallToolResult),
    Failed(McpError),
}

impl TaskTriggerHandler {
    /// Import one parsed export document: the body `graph_edit` and
    /// `graph_edit` share — same plan, same pending crews, same
    /// unique-name resolution. `operation` names the caller for errors.
    async fn import_document_value(
        &self,
        document: &crate::domain::graph_transfer::GraphExportDocument,
        workdir: &str,
        name: Option<&str>,
        operation: &str,
    ) -> ImportedDocument {
        if let Err(message) = check_import_node_configs(document) {
            return ImportedDocument::Rejected(error_result(&message));
        }

        let desired_name = name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| document.name.trim());
        if let Err(message) = validate_non_empty(desired_name, "Graph name") {
            return ImportedDocument::Rejected(error_result(&message));
        }

        let graph_id = uuid::Uuid::new_v4().to_string();
        let plan = match crate::domain::graph_transfer::build_import_plan(document, &graph_id) {
            Ok(plan) => plan,
            Err(message) => return ImportedDocument::Rejected(error_result(&message)),
        };
        if let Err(message) = check_import_templates(document, &plan) {
            return ImportedDocument::Rejected(error_result(&message));
        }
        let missing_platform = crate::domain::graph_transfer::agent_nodes_missing_platform(&plan);

        let existing_names: Vec<String> = match self.db.list_graphs(Some(workdir), true) {
            Ok(graphs) => graphs.into_iter().map(|lp| lp.name).collect(),
            Err(error) => return ImportedDocument::Failed(internal_error(error)),
        };
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

        let missing_crews = match self.db.import_graph(&lp, &plan) {
            Ok(missing) => missing,
            Err(error) => return ImportedDocument::Failed(write_error(operation, &error)),
        };
        if crate::domain::project::should_auto_register(std::path::Path::new(workdir)) {
            if let Err(error) = self.db.register_project_path(std::path::Path::new(workdir)) {
                tracing::debug!(
                    "Could not register imported graph's project at {workdir}: {error}"
                );
            }
        }
        self.activate_graph_trigger(&lp).await;

        ImportedDocument::Done(serde_json::json!({
            "graph_id": lp.id,
            "name": lp.name,
            "nodes_missing_platform": missing_platform,
            "missing_crews": missing_crews,
            "terminals": plan.terminals,
        }))
    }
}

/// The per-node config checks import runs before building its plan: known
/// keys for the kind, plus the `mutex` shape.
fn check_import_node_configs(
    document: &crate::domain::graph_transfer::GraphExportDocument,
) -> Result<(), String> {
    for node in &document.nodes {
        if let Some(map) = node.config.as_object() {
            validate_known_config_keys(node.kind, map)?;
        }
        crate::daemon::handler_validate::validate_node_mutex_config(&node.name, &node.config)?;
    }
    Ok(())
}

/// Same write-time binding check as the node/ensemble authoring paths,
/// before anything is persisted (all-or-nothing): every document node,
/// ensemble shared prompt and member override. The domain plan builder
/// is untouched — this lives in the handler like the other import-time
/// validations above.
fn check_import_templates(
    document: &crate::domain::graph_transfer::GraphExportDocument,
    plan: &crate::domain::graph_transfer::GraphImportPlan,
) -> Result<(), String> {
    let mut node_names: Vec<String> = document.nodes.iter().map(|n| n.name.clone()).collect();
    for ensemble_plan in &plan.ensembles {
        node_names.extend(ensemble_plan.member_nodes.iter().map(|n| n.name.clone()));
        node_names.push(ensemble_plan.join_node.name.clone());
    }
    let ensembles: Vec<(String, Option<String>)> = document
        .ensembles
        .iter()
        .zip(plan.ensembles.iter())
        .map(|(doc, ensemble_plan)| (doc.name.clone(), Some(ensemble_plan.join_node.name.clone())))
        .collect();
    for doc_node in &document.nodes {
        crate::daemon::handler_templates::check_node_templates(
            doc_node.kind,
            &doc_node.config,
            &doc_node.name,
            &node_names,
            &ensembles,
        )?;
    }
    for doc_ensemble in &document.ensembles {
        let owner = format!("Ensemble '{}' shared prompt template", doc_ensemble.name);
        crate::daemon::handler_templates::check_template_bindings(
            &owner,
            &doc_ensemble.prompt_template,
            crate::graph_engine::AGENT_PROMPT_BINDINGS,
            &node_names,
            &ensembles,
        )?;
        check_import_member_overrides(doc_ensemble, &node_names, &ensembles)?;
    }
    Ok(())
}

fn check_import_member_overrides(
    doc_ensemble: &crate::domain::graph_transfer::GraphExportEnsemble,
    node_names: &[String],
    ensembles: &[(String, Option<String>)],
) -> Result<(), String> {
    for (index, member) in doc_ensemble.members.iter().enumerate() {
        let Some(template) = member.prompt_override.as_deref() else {
            continue;
        };
        let owner = format!(
            "Ensemble '{}' member {} prompt override",
            doc_ensemble.name,
            index + 1
        );
        crate::daemon::handler_templates::check_template_bindings(
            &owner,
            template,
            crate::graph_engine::AGENT_PROMPT_BINDINGS,
            node_names,
            ensembles,
        )?;
    }
    Ok(())
}

/// The `next` step of a `graph_edit` response: which nodes still need a
/// harness assigned, and the command to assign them.
fn install_next_step(value: &serde_json::Value) -> String {
    let graph_id = value.get("graph_id").and_then(|id| id.as_str());
    let missing: Vec<&str> = value
        .get("nodes_missing_platform")
        .and_then(serde_json::Value::as_array)
        .map(|nodes| nodes.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    match (graph_id, missing.as_slice()) {
        (Some(id), []) => format!("All agent nodes have a harness; start the graph with graph_control run (graph_id {id})."),
        (Some(id), missing) => format!(
            "Assign a platform/model to {} with graph_node update, then start the graph with graph_control run (graph_id {id}).",
            missing.join(", ")
        ),
        _ => "Assign every node a platform/model with graph_node update, then start the graph with graph_control run.".to_string(),
    }
}
