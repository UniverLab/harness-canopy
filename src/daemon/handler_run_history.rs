//! MCP handler tools split out of `daemon::handler` (node-run history tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_seeds_skills::redact_sensitive_value;
use crate::daemon::handler_validate::resolve_prefix_or_error;
use crate::daemon::helpers::{error_result, EarlyReturn};
use crate::daemon::params::*;
use crate::db::graphs::events::GraphEventRow;
use crate::db::Database;
use crate::domain::graphs::GraphNodeRun;
use crate::or_early_return;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use std::collections::HashMap;

/// CM46: the event `kind` vocabulary the `graph_events` table enforces —
/// validated here too so a bad filter fails with a clear message instead of
/// an empty page.
const GRAPH_EVENT_KINDS: &[&str] = &[
    "graph_started",
    "graph_paused",
    "graph_resumed",
    "graph_completed",
    "graph_failed",
    "graph_blocked",
    "spec_started",
    "spec_completed",
    "spec_failed",
    "spec_skipped",
    "node_started",
    "node_finished",
    "hook_fired",
    "config_changed",
    "note",
    "mutex_waiting",
    "mutex_acquired",
];

#[tool_router(router = tool_router_run_history, vis = "pub(crate)")]
impl TaskTriggerHandler {
    pub(crate) async fn graph_node_runs_list_impl(
        &self,
        params: GraphNodeRunsListParams,
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
                    |db, id| db.resolve_spec_id_by_prefix(id, None),
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

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        )]))
    }

    pub(crate) async fn graph_node_run_get_impl(
        &self,
        params: GraphNodeRunGetParams,
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

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&graph_node_run_detail_json(
                &run,
                node_name.as_deref(),
                resumed_from_run_id.as_deref(),
            ))
            .unwrap_or_default(),
        )]))
    }

    pub(crate) async fn graph_events_impl(
        &self,
        params: GraphEventsParams,
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
        if self
            .db
            .get_graph(&graph_id)
            .map_err(internal_error)?
            .is_none()
        {
            return Ok(error_result(&format!("Graph '{}' not found.", graph_id)));
        }
        if let Some(kinds) = params.kinds.as_deref() {
            for kind in kinds {
                if !GRAPH_EVENT_KINDS.contains(&kind.as_str()) {
                    return Ok(error_result(&format!("Unknown event kind '{kind}'.")));
                }
            }
        }
        let (resolved_spec_id, since_id) =
            or_early_return!(resolve_event_filters(&self.db, &graph_id, &params));

        let limit = params.limit.unwrap_or(200).clamp(1, 200) as usize;
        let rows: Vec<GraphEventRow> = self
            .db
            .list_graph_events(
                &graph_id,
                since_id,
                resolved_spec_id.as_deref(),
                params.kinds.as_deref(),
                limit,
            )
            .map_err(internal_error)?;

        // Batched name maps (one query each), never a lookup per row.
        let spec_names: HashMap<String, String> = self
            .db
            .list_graph_specs(&graph_id)
            .map_err(internal_error)?
            .into_iter()
            .map(|spec| (spec.id, spec.name))
            .collect();
        let node_names: HashMap<String, String> = self
            .db
            .list_graph_nodes_for_graph(&graph_id)
            .map_err(internal_error)?
            .into_iter()
            .map(|node| (node.id, node.name))
            .collect();

        let include_payload = params.include_payload.unwrap_or(false);
        let events: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| event_row_json(row, &spec_names, &node_names, include_payload))
            .collect();

        let next_since_id = if rows.len() == limit {
            rows.last().map(|row| row.id)
        } else {
            None
        };
        let body = serde_json::json!({
            "graph_id": graph_id,
            "events": events,
            "next_since_id": next_since_id,
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        )]))
    }
}

/// Resolve the spec filter and the numeric `since` cursor for the list
/// query. Returns `(resolved_spec_id, since_id)`.
fn resolve_event_filters(
    db: &Database,
    graph_id: &str,
    params: &GraphEventsParams,
) -> Result<(Option<String>, Option<i64>), EarlyReturn> {
    let resolved_spec_id = match &params.spec_id {
        Some(sid) => {
            match resolve_prefix_or_error(
                db,
                sid.trim(),
                |db, id| db.resolve_spec_id_by_prefix(id, None),
                "spec",
            ) {
                Ok(id) => Some(id),
                Err(e) => return Err(EarlyReturn::ok(e)),
            }
        }
        None => None,
    };
    // CM46: `--since <RFC3339>` is translated to the numeric cursor the
    // list query speaks. An unknown/unparsable timestamp yields no events.
    let since_id = match (params.since_id, params.since_ts.as_deref()) {
        (Some(id), _) => Some(id),
        (None, Some(ts)) => {
            match db
                .first_graph_event_id_at_or_after(graph_id, ts)
                .map_err(internal_error)?
            {
                Some(id) => Some(id - 1),
                None => Some(i64::MAX),
            }
        }
        (None, None) => None,
    };
    Ok((resolved_spec_id, since_id))
}

fn event_row_json(
    row: &GraphEventRow,
    spec_names: &HashMap<String, String>,
    node_names: &HashMap<String, String>,
    include_payload: bool,
) -> serde_json::Value {
    let spec_name = row
        .spec_id
        .as_ref()
        .and_then(|id| spec_names.get(id))
        .cloned();
    let node_name = row
        .node_id
        .as_ref()
        .and_then(|id| node_names.get(id))
        .cloned();
    let mut obj = serde_json::Map::from_iter([
        ("id".to_string(), serde_json::json!(row.id)),
        ("created_at".to_string(), serde_json::json!(row.created_at)),
        ("kind".to_string(), serde_json::json!(row.kind)),
        ("spec_name".to_string(), serde_json::json!(spec_name)),
        ("node_name".to_string(), serde_json::json!(node_name)),
        ("message".to_string(), serde_json::json!(row.message)),
    ]);
    if include_payload {
        let payload = row
            .payload
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .unwrap_or(serde_json::Value::Null);
        obj.insert("payload".to_string(), payload);
    }
    serde_json::Value::Object(obj)
}

/// CB43: the pair resolved at dispatch, as JSON object entries. Keys are
/// omitted (not null) when the run predates the recording, so old rows read
/// exactly as before.
fn executed_pair_json(run: &GraphNodeRun) -> serde_json::Map<String, serde_json::Value> {
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
    executed
}

/// A waiting run's queue entries (what it waits for and who holds each
/// resource) — the only surface `graph` has for the queue.
/// Empty otherwise so settled rows read exactly as before.
fn waiting_for_json(
    db: &Database,
    run: &GraphNodeRun,
) -> Vec<crate::db::mutex_store::MutexWaitDetail> {
    if run.status == crate::domain::graphs::GraphRunStatus::Waiting {
        db.get_graph_run_waiting_for(&run.id).unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// One row of `graph` — deliberately excludes `input`/`output`
/// (can be large; a caller wanting them calls `graph` with this
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
    let executed = executed_pair_json(run);
    let spec = db.get_graph_spec(&run.spec_id).ok().flatten();
    let spec_end_dirty = spec.as_ref().and_then(|s| s.spec_end_dirty);
    let spec_end_dirty_paths = spec.as_ref().and_then(|s| s.spec_end_dirty_paths.clone());
    let waiting_for = waiting_for_json(db, run);
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
        if !waiting_for.is_empty() {
            obj.insert("waiting_for".to_string(), serde_json::json!(waiting_for));
        }
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
        if !waiting_for.is_empty() {
            obj.insert("waiting_for".to_string(), serde_json::json!(waiting_for));
        }
        serde_json::Value::Object(obj)
    }
}

/// The `graph` response: everything `graph_node_run_summary_json`
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
