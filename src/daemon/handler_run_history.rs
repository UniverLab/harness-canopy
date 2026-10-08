//! MCP handler tools split out of `daemon::handler` (node-run history tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_seeds_skills::redact_sensitive_value;
use crate::daemon::handler_validate::resolve_prefix_or_error;
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::GraphNodeRun;
use rmcp::model::{CallToolResult, Content};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_run_history, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_node_runs_list",
        description = "List a graph's node run history — what `graph_get` omits (it only carries a graph's *bound* specs, empty for a queue-driven run). Defaults to the most recent runs first, so the run that failed a `failed` graph is normally the very first result without knowing any node id ahead of time. Narrow to one spec or node with `spec_id`/`node_id`. Output/input are omitted here since they can be large; fetch a specific run's full output with graph_node_run_get using its `id`."
    )]
    pub(crate) async fn graph_node_runs_list(
        &self,
        Parameters(params): Parameters<GraphNodeRunsListParams>,
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
        let resolved_spec_id = match &params.spec_id {
            Some(sid) => {
                match resolve_prefix_or_error(
                    &self.db,
                    sid.trim(),
                    Database::resolve_spec_id_by_prefix,
                    "spec",
                ) {
                    Ok(id) => Some(id),
                    Err(e) => return Ok(e),
                }
            }
            None => None,
        };
        let resolved_node_id = match &params.node_id {
            Some(nid) => {
                match resolve_prefix_or_error(
                    &self.db,
                    nid.trim(),
                    Database::resolve_graph_node_id_by_prefix,
                    "node",
                ) {
                    Ok(id) => Some(id),
                    Err(e) => return Ok(e),
                }
            }
            None => None,
        };

        if self
            .db
            .get_graph(&graph_id)
            .map_err(internal_error)?
            .is_none()
        {
            return Ok(error_result(&format!("Graph '{}' not found.", graph_id)));
        }

        let limit = params.limit.unwrap_or(20).clamp(1, 200) as i64;
        let offset = params.offset.unwrap_or(0) as i64;

        let compact = params.compact.unwrap_or(false);

        let runs = self
            .db
            .list_graph_node_runs(
                &graph_id,
                resolved_spec_id.as_deref(),
                resolved_node_id.as_deref(),
                limit,
                offset,
            )
            .map_err(internal_error)?;

        let total = self
            .db
            .count_graph_node_runs_filtered(
                &graph_id,
                resolved_spec_id.as_deref(),
                resolved_node_id.as_deref(),
            )
            .map_err(internal_error)?;

        let out = runs
            .iter()
            .map(|run| graph_node_run_summary_json(&self.db, run, compact))
            .collect::<Vec<_>>();

        let returned = out.len() as i64;
        let remaining = (total - offset).saturating_sub(returned);
        let truncated = remaining > 0;

        let mut body = serde_json::json!({
            "graph_id": graph_id,
            "limit": limit,
            "offset": offset,
            "runs": out,
            "total": total,
        });
        if truncated {
            body["truncated"] = serde_json::json!(true);
            body["omitted"] = serde_json::json!(remaining);
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "graph_node_run_get",
        description = "Fetch one node run's full stored input/output by run id (see graph_node_runs_list). This is the second step of failure diagnosis: list to find the offending run, then fetch its output here — the exact stderr/stdout/reported_output the engine recorded, including `infra_attempt`/`infra_crash` markers when present. Secret-shaped substrings (API keys, tokens, private key blocks) are redacted before the output crosses this boundary."
    )]
    pub(crate) async fn graph_node_run_get(
        &self,
        Parameters(params): Parameters<GraphNodeRunGetParams>,
    ) -> Result<CallToolResult, McpError> {
        let run_id = match resolve_prefix_or_error(
            &self.db,
            params.run_id.trim(),
            Database::resolve_run_id_by_prefix,
            "run",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let Some(run) = self.db.get_graph_run(&run_id).map_err(internal_error)? else {
            return Ok(error_result(&format!("Node run '{}' not found.", run_id)));
        };
        let node_name = self
            .db
            .get_graph_node(&run.node_id)
            .map_err(internal_error)?
            .map(|node| node.name);
        let resumed_from_run_id = match run.session_id.as_deref() {
            Some(sid) => self
                .db
                .find_resumed_from_run_id(&run.id, &run.node_id, sid)
                .map_err(internal_error)?,
            None => None,
        };

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&graph_node_run_detail_json(
                &run,
                node_name.as_deref(),
                resumed_from_run_id.as_deref(),
            ))
            .unwrap_or_default(),
        )]))
    }
}

/// One row of `graph_node_runs_list` — deliberately excludes `input`/`output`
/// (can be large; a caller wanting them calls `graph_node_run_get` with this
/// row's `id`) but resolves `node_name` so a caller isn't left matching a
/// bare node id back to the graph by hand.
pub(crate) fn graph_node_run_summary_json(
    db: &Database,
    run: &GraphNodeRun,
    compact: bool,
) -> serde_json::Value {
    let node_name = db
        .get_graph_node(&run.node_id)
        .ok()
        .flatten()
        .map(|node| node.name);
    // CB43: the pair resolved at dispatch. Keys are omitted (not null) when
    // the run predates the recording, so old rows read exactly as before.
    let mut executed = serde_json::Map::new();
    if let Some(platform) = run.executed_platform.as_deref() {
        executed.insert(
            "platform".to_string(),
            serde_json::Value::String(platform.to_string()),
        );
    }
    if let Some(model) = run.executed_model.as_deref() {
        executed.insert(
            "model".to_string(),
            serde_json::Value::String(model.to_string()),
        );
    }
    let spec = db.get_graph_spec(&run.spec_id).ok().flatten();
    let spec_end_dirty = spec.as_ref().and_then(|s| s.spec_end_dirty);
    let spec_end_dirty_paths = spec.as_ref().and_then(|s| s.spec_end_dirty_paths.clone());
    if compact {
        let spec_name = spec.as_ref().map(|s| s.name.clone());
        let mut obj = serde_json::Map::from_iter([
            ("id".to_string(), serde_json::json!(run.id)),
            (
                "node_name".to_string(),
                node_name
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
            ),
            ("status".to_string(), serde_json::json!(run.status.as_str())),
            ("iteration".to_string(), serde_json::json!(run.iteration)),
            (
                "spec_name".to_string(),
                spec_name
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
            ),
            (
                "started_at".to_string(),
                serde_json::json!(run.started_at.to_rfc3339()),
            ),
            (
                "completed_at".to_string(),
                serde_json::json!(run.completed_at.map(|value| value.to_rfc3339())),
            ),
            (
                "spec_end_dirty".to_string(),
                spec_end_dirty
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null),
            ),
            (
                "spec_end_dirty_paths".to_string(),
                spec_end_dirty_paths
                    .map(|paths| serde_json::json!(paths))
                    .unwrap_or(serde_json::Value::Null),
            ),
        ]);
        obj.extend(executed);
        serde_json::Value::Object(obj)
    } else {
        let mut obj = serde_json::Map::from_iter([
            ("id".to_string(), serde_json::json!(run.id)),
            ("spec_id".to_string(), serde_json::json!(run.spec_id)),
            ("node_id".to_string(), serde_json::json!(run.node_id)),
            (
                "node_name".to_string(),
                node_name
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
            ),
            ("status".to_string(), serde_json::json!(run.status.as_str())),
            ("iteration".to_string(), serde_json::json!(run.iteration)),
            (
                "started_at".to_string(),
                serde_json::json!(run.started_at.to_rfc3339()),
            ),
            (
                "completed_at".to_string(),
                serde_json::json!(run.completed_at.map(|value| value.to_rfc3339())),
            ),
            ("session_id".to_string(), serde_json::json!(run.session_id)),
            (
                "spec_end_dirty".to_string(),
                spec_end_dirty
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null),
            ),
            (
                "spec_end_dirty_paths".to_string(),
                spec_end_dirty_paths
                    .map(|paths| serde_json::json!(paths))
                    .unwrap_or(serde_json::Value::Null),
            ),
        ]);
        obj.extend(executed);
        serde_json::Value::Object(obj)
    }
}

/// The `graph_node_run_get` response: everything `graph_node_run_summary_json`
/// carries, plus the full `input`/`output` the engine stored — redacted
/// (see [`redact_sensitive_value`]) since either can carry secret-shaped
/// content echoed by the wrapped CLI. `output` preserves whatever the engine
/// wrote verbatim otherwise, including the B19 `infra_attempt`/`infra_crash`
/// markers a caller needs to tell an infra retry from a semantic failure.
pub(crate) fn graph_node_run_detail_json(
    run: &GraphNodeRun,
    node_name: Option<&str>,
    resumed_from_run_id: Option<&str>,
) -> serde_json::Value {
    let mut obj = serde_json::Map::from_iter([
        ("id".to_string(), serde_json::json!(run.id)),
        ("graph_id".to_string(), serde_json::json!(run.graph_id)),
        ("spec_id".to_string(), serde_json::json!(run.spec_id)),
        ("node_id".to_string(), serde_json::json!(run.node_id)),
        ("node_name".to_string(), serde_json::json!(node_name)),
        ("status".to_string(), serde_json::json!(run.status.as_str())),
        ("iteration".to_string(), serde_json::json!(run.iteration)),
        (
            "input".to_string(),
            run.input
                .as_ref()
                .map(redact_sensitive_value)
                .unwrap_or(serde_json::Value::Null),
        ),
        (
            "output".to_string(),
            run.output
                .as_ref()
                .map(redact_sensitive_value)
                .unwrap_or(serde_json::Value::Null),
        ),
        (
            "started_at".to_string(),
            serde_json::json!(run.started_at.to_rfc3339()),
        ),
        (
            "completed_at".to_string(),
            serde_json::json!(run.completed_at.map(|value| value.to_rfc3339())),
        ),
        ("session_id".to_string(), serde_json::json!(run.session_id)),
        (
            "resumed_from_run_id".to_string(),
            serde_json::json!(resumed_from_run_id),
        ),
    ]);
    // CB43: omit (not null) when the run predates the recording.
    if let Some(platform) = run.executed_platform.as_deref() {
        obj.insert(
            "platform".to_string(),
            serde_json::Value::String(platform.to_string()),
        );
    }
    if let Some(model) = run.executed_model.as_deref() {
        obj.insert(
            "model".to_string(),
            serde_json::Value::String(model.to_string()),
        );
    }
    serde_json::Value::Object(obj)
}
