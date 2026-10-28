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
        description = "Export a graph's design — name, description, nodes, edges, and ensembles — as a portable JSON document, so it can be shared as a file and recreated elsewhere with graph_import. Never includes ids, workdir, specs, or run/status state. Includes platform/model for every agent node and ensemble member, and each ensemble's kind ('cascade'/'round_robin'; omitted for the 'parallel' default); format_version 6. Bundles the full text of every custom prompt_preset the document's agent nodes reference in 'prompt_presets' (name + content), so the importing machine gets working nodes instead of name-only references; builtin presets (implementer, reviewer, resilience) are never bundled. A crew-bound ensemble exports its 'crew' name and no members, and a crew-bound agent node exports its 'crew' key. Ensembles carry every entry source: the primary in 'entry_from_node'/'entry_condition' plus 'extra_entry_sources' (node + condition) for sources added with 'add_entry_from' (omitted when there are none)."
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
        description = "Create a new graph from an exported document (the object graph_export returns). Always creates a new graph — never updates or overwrites an existing one; if the name is already taken in workdir, a numeric suffix is applied and the response says which name was used. Validates the document exactly as graph_add_node/graph_add_edge/graph_add_ensemble would, all-or-nothing: nothing is written if any part is rejected. It also refuses an agent node's prompt_preset when this machine cannot resolve it — no `<name>.md` in the prompts dir, not bundled in the document, and not a builtin (implementer, reviewer, resilience) — naming every unknown preset in the error, because the imported node would otherwise run on an empty prompt. A document's bundled 'prompt_presets' entries are installed into the prompts dir before the graph is created when no file with that name exists; a local file with identical content is a no-op, and one with different content refuses the import, naming the preset — a user's edited preset is never overwritten. Accepts format_version 1 (members with no binding, reported as missing a platform), 2 (bindings restored), 3 (graph/error vocabulary), 4 (every ensemble entry source: 'extra_entry_sources' restores each 'add_entry_from' source with its own condition), and 5 (crew-bound ensembles and nodes: 'crew' restores the binding when a crew of that name exists locally, else the response lists it under 'missing_crews' and the ensemble cannot dispatch until the crew is created), and 6 (ensemble 'mutex': restored per member run; older documents import with none). Restores each ensemble's kind, commit_rights, mutex, timeouts, member overrides, and entry/exit wiring exactly; a document field this build does not know how to restore — including an unknown 'kind' value — is a refusal naming the field, never a silent default. The response lists every agent node left without a platform, and every crew an imported ensemble references that does not exist locally, so the caller knows what to fill in before running the graph."
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

        match self
            .import_document_value(&document, workdir, params.name.as_deref(), "graph_import")
            .await
        {
            ImportedDocument::Done(value) => Ok(build_json_result(&value)),
            ImportedDocument::Rejected(result) => Ok(result),
            ImportedDocument::Failed(error) => Err(error),
        }
    }

    #[tool(
        name = "graph_install",
        description = "Install a graph from the catalogue in one command: fetch the published export document for `ref` — `<slug>` for an official graph or `<owner>/<repo>/<slug>` for a community one, each with an optional `@<ref>` version suffix — through `graphs.catalogue_url` (default https://graphs.univerlab.org), so the catalogue counts the install, and validate it exactly as `graph_validate` would (any problem aborts the install). BEFORE ANYTHING IS WRITTEN the call returns {needs_review: true, review: {source, commands, prompts, attention}}: every shell command (check/gate config.command and hook command, labelled by node or hook), every agent instruction (prompt_template, prompt_override, bundled or builtin preset content, hook prompt — first 3 lines with a total line count), and the attention marks for dangerous command shapes (pipe to shell, sudo, rm -rf, chmod 777, git push/--force, sensitive-path write, eval, base64 -d, pipe to python/node). AN AGENT MUST SHOW THIS REVIEW TO ITS USER before calling again with accept: true; without accept nothing is installed, and official documents are reviewed the same as community ones. With accept: true the bundled `prompt_presets` are installed into the prompts dir (a local file with different content aborts the install unless `overwrite_presets` is true), the document is imported exactly as `graph_import` would (always a new graph in `workdir`, a numeric suffix when the name is taken), and the response carries the same review plus the graph id and name, the workdir, the presets written, the agent nodes still needing a platform, and the next command to assign them."
    )]
    pub(crate) async fn graph_install(
        &self,
        Parameters(params): Parameters<GraphInstallParams>,
    ) -> Result<CallToolResult, McpError> {
        let workdir = params.workdir.trim();
        if let Err(e) = validate_non_empty(workdir, "Graph workdir") {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_absolute_dir(workdir) {
            return Ok(error_result(&e));
        }

        // The fetch is blocking I/O, so it runs off the async runtime. It
        // writes nothing: the review comes first.
        let reference = params.reference.clone();
        let fetched = tokio::task::spawn_blocking(move || {
            crate::daemon::graph_install::fetch_graph(
                &crate::daemon::graph_install::catalogue_url(),
                &reference,
            )
        })
        .await
        .map_err(|error| internal_error(error.to_string()))?;
        let fetched = match fetched {
            Ok(fetched) => fetched,
            Err(message) => return Ok(error_result(&message)),
        };

        let review = crate::domain::nodes::review::review_graph(&fetched.document, &fetched.source);
        if params.accept != Some(true) {
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
            .import_document_value(&fetched.document, workdir, name.as_deref(), "graph_install")
            .await
        {
            ImportedDocument::Done(mut value) => {
                value["workdir"] = serde_json::json!(workdir);
                value["ref"] = serde_json::json!(params.reference);
                value["source_url"] = serde_json::json!(fetched.source_url);
                value["presets_written"] = serde_json::json!(presets_written);
                value["review"] = serde_json::to_value(&review).unwrap_or_default();
                value["next"] = serde_json::json!(install_next_step(&value));
                Ok(build_json_result(&value))
            }
            ImportedDocument::Rejected(result) => Ok(result),
            ImportedDocument::Failed(error) => Err(error),
        }
    }

    #[tool(
        name = "graph_validate",
        description = "Validate a shared graph or node document without importing it — no database access, no workdir needed. Provide exactly one of `path` (a JSON file) or `document` (the object itself). A graph document (has `nodes`) runs every structural check `graph_import` runs — format_version, known config keys, ensemble shape, template bindings — plus the sharing rule that every `prompt_preset` a node names is bundled in the document or a builtin (implementer, reviewer, resilience). A node document (`format_version`, `name`, `kind`, `config`, optional `description`/`preset`) checks the slug name, a known authorable kind (agent, check, gate, router, or ensemble), known config keys, no harness keys (`platform`/`model`/`cli`) anywhere in the config, an ensemble document's prompt/members/min_pass shape, and a bundled preset name matching the config's `prompt_preset`. Returns `ok` with the kind, name, node/edge/ensemble counts and the agent nodes that will need a harness — or `ok: false` with every problem found, each with a JSON path and a message."
    )]
    pub(crate) async fn graph_validate(
        &self,
        Parameters(params): Parameters<GraphValidateParams>,
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
    /// Import one parsed export document: the body `graph_import` and
    /// `graph_install` share — same plan, same pending crews, same
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

/// The `next` step of a `graph_install` response: which nodes still need a
/// harness assigned, and the command to assign them.
fn install_next_step(value: &serde_json::Value) -> String {
    let graph_id = value.get("graph_id").and_then(|id| id.as_str());
    let missing: Vec<&str> = value
        .get("nodes_missing_platform")
        .and_then(serde_json::Value::as_array)
        .map(|nodes| nodes.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    match (graph_id, missing.as_slice()) {
        (Some(id), []) => format!("All agent nodes have a harness; start the graph with graph_run (graph_id {id})."),
        (Some(id), missing) => format!(
            "Assign a platform/model to {} with graph_update_node, then start the graph with graph_run (graph_id {id}).",
            missing.join(", ")
        ),
        _ => "Assign every node a platform/model with graph_update_node, then start the graph with graph_run.".to_string(),
    }
}
