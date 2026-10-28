//! Helper functions for spec handler tools — result builders and JSON formatters.

use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_formatting::{build_id_result, build_json_result};
use crate::daemon::handler_run_exec::graph_run_blocker;
use crate::db::Database;
use crate::domain::graphs::{GraphRunStatus, GraphSpec};
use crate::domain::spec_type::{commit_message, SpecNaming};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

/// SPECCODE: create response for `spec_edit create` — an untyped, unconventional
/// create succeeds but answers with a `warning` naming the consequence (no
/// code, no prebuilt commit message) instead of silently dropping the data.
pub(crate) fn spec_create_result(spec_id: &str, naming: &SpecNaming) -> CallToolResult {
    if naming.spec_type.is_none() {
        let mut map = serde_json::Map::new();
        map.insert("spec_id".to_string(), serde_json::json!(spec_id));
        map.insert(
            "warning".to_string(),
            serde_json::json!(
                "Spec created without a type: it will have no code (e.g. FIX-7) and no commit \nmessage. Pass a Conventional Commits type (feat, fix, docs, style, refactor, perf, test, build, ci, chore, revert) or a conventional title like 'fix(tui): open F1 from inside a session'."
            ),
        );
        build_json_result(&serde_json::Value::Object(map))
    } else {
        build_id_result(spec_id, "spec_id")
    }
}

pub(crate) fn validate_at_least_one_bool(updates: &[bool], field_name: &str) -> Result<(), String> {
    if updates.iter().all(|&b| !b) {
        Err(format!(
            "{field_name} requires at least one field to update."
        ))
    } else {
        Ok(())
    }
}

/// Summary JSON for `spec list` — no run/blocker info, since a standalone
/// (unassigned) spec has never run. Compare [`graph_spec_details_json`],
/// which adds that runtime detail for specs already inside a graph.
pub(crate) fn spec_summary_json(spec: &GraphSpec, include_descriptions: bool) -> serde_json::Value {
    // SPECCODE: type/scope/code/commit_message are always surfaced (null
    // for a legacy spec with no type) — check nodes and agents build
    // deterministic commits from these.
    let spec_type = spec.spec_type.map(|t| serde_json::json!(t));
    let commit_message = spec
        .spec_type
        .map(|t| commit_message(t, spec.spec_scope.as_deref(), &spec.name));
    if include_descriptions {
        let mut obj = serde_json::json!({
            "id": spec.id,
            "graph_id": spec.graph_id,
            "name": spec.name,
            "description": spec.description,
            "workdir": spec.workdir,
            "position": spec.position,
            "parallelizable": spec.parallelizable,
            "status": spec.status.as_str(),
            "type": spec_type,
            "scope": spec.spec_scope,
            "code": spec.spec_code,
            "commit_message": commit_message,
        });
        if let Some(via) = &spec.completed_via {
            obj["completed_via"] = serde_json::json!(via);
        }
        obj
    } else {
        serde_json::json!({
            "id": spec.id,
            "graph_id": spec.graph_id,
            "name": spec.name,
            "status": spec.status.as_str(),
            "workdir": spec.workdir,
            "type": spec_type,
            "scope": spec.spec_scope,
            "code": spec.spec_code,
            "commit_message": commit_message,
        })
    }
}

pub(crate) struct SpecRunInfo {
    pub(crate) name: String,
    pub(crate) current_node: Option<String>,
    pub(crate) blocker: Option<String>,
    /// C19: how many separate graph executions this spec has failed with a
    /// genuine verdict — the persisted counter behind the cross-run attempt
    /// budget. Surfaced so an operator can see a spec approaching the limit
    /// before it actually blocks the graph, not just after.
    pub(crate) cross_run_attempts: i64,
    /// What the current run is waiting for, when it is `waiting` for named
    /// mutexes — one entry per resource with its current holder. Empty
    /// otherwise.
    pub(crate) waiting_for: Vec<crate::db::mutex_store::MutexWaitDetail>,
}

pub(crate) fn build_spec_run_info(
    db: &Database,
    spec: &GraphSpec,
) -> Result<SpecRunInfo, McpError> {
    let runs = db
        .list_graph_runs_for_spec(&spec.id)
        .map_err(internal_error)?;
    // A mutex-waiting run is the spec's current run exactly like a running
    // one — it is in flight, not history.
    let current_run = runs
        .iter()
        .rev()
        .find(|run| {
            matches!(
                run.status,
                GraphRunStatus::Running | GraphRunStatus::Waiting
            )
        })
        .or_else(|| runs.last());
    let current_node = current_run.map(|run| run.node_id.clone());
    let waiting_for = match current_run {
        Some(run) if run.status == GraphRunStatus::Waiting => db
            .get_graph_run_waiting_for(&run.id)
            .map_err(internal_error)?,
        _ => Vec::new(),
    };
    let blocker = runs.last().and_then(graph_run_blocker);
    let cross_run_attempts = db
        .get_graph_spec_cross_run_attempts(&spec.id)
        .map_err(internal_error)?;
    Ok(SpecRunInfo {
        name: spec.name.clone(),
        current_node,
        blocker,
        cross_run_attempts,
        waiting_for,
    })
}
