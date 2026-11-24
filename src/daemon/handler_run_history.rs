//! MCP handler tools split out of `daemon::handler` (node-run history tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_seeds_skills::redact_sensitive_value;
use crate::daemon::handler_validate::resolve_prefix_or_error;
use crate::daemon::helpers::{error_result, EarlyReturn};
use crate::daemon::params::*;
use crate::db::graphs::events::GraphEventRow;
use crate::db::graphs::GraphMetricsRunRow;
use crate::db::Database;
use crate::domain::graphs::{GraphNodeRun, GraphRunStatus};
use crate::or_early_return;
use crate::system::host_metrics_reader::parse_time;
use chrono::Utc;
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
    "pause_requested",
    "pause_cancelled",
    "ensemble_changed",
];

#[tool_router(router = tool_router_run_history, vis = "pub(crate)", allow_empty)]
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

    pub(crate) async fn graph_metrics_impl(
        &self,
        params: GraphMetricsParams,
    ) -> Result<CallToolResult, McpError> {
        let by = params.by.as_deref().unwrap_or("graph");
        if !matches!(by, "graph" | "node" | "model") {
            return Ok(error_result(&format!(
                "Invalid `by` '{by}'. Expected one of: graph, node, model."
            )));
        }

        let until = Utc::now();
        let since_raw = params.since.as_deref().unwrap_or("7d");
        let since = parse_time(since_raw, until)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;

        let resolved_graph_id = match params.graph_id.as_deref().map(str::trim) {
            Some(gid) if !gid.is_empty() => match resolve_graph_name_or_id(&self.db, gid) {
                Ok(id) => Some(id),
                Err(e) => return Ok(e),
            },
            _ => None,
        };

        let rows = self
            .db
            .list_graph_metrics_rows(
                since.timestamp(),
                until.timestamp(),
                resolved_graph_id.as_deref(),
            )
            .map_err(internal_error)?;

        let graph_names = build_name_map(
            &self.db,
            rows.iter().map(|r| r.graph_id.clone()),
            |db, id| Ok(db.get_graph(id)?.map(|g| g.name)),
        )
        .map_err(internal_error)?;
        let node_names = if by == "node" {
            build_name_map(
                &self.db,
                rows.iter().map(|r| r.node_id.clone()),
                |db, id| Ok(db.get_graph_node(id)?.map(|n| n.name)),
            )
            .map_err(internal_error)?
        } else {
            HashMap::new()
        };

        let groups = aggregate_graph_metrics(&rows, by, &graph_names, &node_names);
        let body = serde_json::json!({
            "window": {
                "since": since.to_rfc3339(),
                "until": until.to_rfc3339(),
            },
            "by": by,
            "groups": groups,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        )]))
    }
}

fn resolve_graph_name_or_id(db: &Database, raw: &str) -> Result<String, CallToolResult> {
    match db.resolve_graph_id_by_prefix(raw) {
        Ok(Some(id)) => return Ok(id),
        Ok(None) => {}
        Err(e) => return Err(error_result(&e.to_string())),
    }
    let graphs = db
        .list_graphs(None, true)
        .map_err(|e| error_result(&format!("Failed to list graphs: {e}")))?;
    let matches: Vec<_> = graphs.into_iter().filter(|g| g.name == raw).collect();
    match matches.len() {
        1 => Ok(matches.into_iter().next().unwrap().id),
        0 => Err(error_result(&format!("graph '{raw}' not found."))),
        _ => Err(error_result(&format!(
            "'{raw}' matches multiple graphs: {}",
            matches
                .iter()
                .map(|g| format!("{} ({})", g.name, &g.id[..8.min(g.id.len())]))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn build_name_map(
    db: &Database,
    ids: impl Iterator<Item = String>,
    lookup: impl Fn(&Database, &str) -> anyhow::Result<Option<String>>,
) -> anyhow::Result<HashMap<String, String>> {
    let mut ids: Vec<String> = ids.collect();
    ids.sort();
    ids.dedup();
    let mut out = HashMap::new();
    for id in ids {
        let name = lookup(db, &id)?.unwrap_or_else(|| id.clone());
        out.insert(id, name);
    }
    Ok(out)
}

#[derive(Debug, Default)]
struct MetricsAgg {
    runs: u64,
    passed: u64,
    failed: u64,
    interrupted: u64,
    waiting: u64,
    error: u64,
    running: u64,
    durations_s: Vec<i64>,
}

pub(crate) fn aggregate_graph_metrics(
    rows: &[GraphMetricsRunRow],
    by: &str,
    graph_names: &HashMap<String, String>,
    node_names: &HashMap<String, String>,
) -> Vec<serde_json::Value> {
    let mut map: HashMap<String, (String, MetricsAgg)> = HashMap::new();

    for row in rows {
        let (group_id, display_key) = metrics_group_key(row, by, graph_names, node_names);
        let entry = map
            .entry(group_id)
            .or_insert_with(|| (display_key, MetricsAgg::default()));
        let agg = &mut entry.1;
        agg.runs += 1;
        let status = GraphRunStatus::from_str(&row.status);
        match status {
            GraphRunStatus::Pass => agg.passed += 1,
            GraphRunStatus::Fail => agg.failed += 1,
            GraphRunStatus::Interrupted => agg.interrupted += 1,
            GraphRunStatus::Waiting => agg.waiting += 1,
            GraphRunStatus::Error => agg.error += 1,
            GraphRunStatus::Running => agg.running += 1,
        }
        // A run still running is excluded from durations even if a
        // `completed_at` timestamp is present on the row.
        if !matches!(status, GraphRunStatus::Running) {
            if let Some(completed_at) = row.completed_at {
                let dur = (completed_at - row.started_at).max(0);
                agg.durations_s.push(dur);
            }
        }
    }

    let mut groups: Vec<(String, MetricsAgg)> = map.into_values().collect();
    groups.sort_by(|a, b| b.1.runs.cmp(&a.1.runs).then_with(|| a.0.cmp(&b.0)));

    groups
        .into_iter()
        .map(|(key, mut agg)| metrics_group_json(&key, &mut agg))
        .collect()
}

fn metrics_group_key(
    row: &GraphMetricsRunRow,
    by: &str,
    graph_names: &HashMap<String, String>,
    node_names: &HashMap<String, String>,
) -> (String, String) {
    match by {
        "node" => {
            let g = graph_names
                .get(&row.graph_id)
                .cloned()
                .unwrap_or_else(|| row.graph_id.clone());
            let n = node_names
                .get(&row.node_id)
                .cloned()
                .unwrap_or_else(|| row.node_id.clone());
            (
                format!("{}\0{}", row.graph_id, row.node_id),
                format!("{g}/{n}"),
            )
        }
        "model" => {
            let p = row.executed_platform.as_deref();
            let m = row.executed_model.as_deref();
            let key = match (p, m) {
                (None, None) => "unknown".to_string(),
                (Some(p), Some(m)) => format!("{p}/{m}"),
                (Some(p), None) => format!("{p}/unknown"),
                (None, Some(m)) => format!("unknown/{m}"),
            };
            (key.clone(), key)
        }
        _ => {
            let g = graph_names
                .get(&row.graph_id)
                .cloned()
                .unwrap_or_else(|| row.graph_id.clone());
            (row.graph_id.clone(), g)
        }
    }
}

fn metrics_group_json(key: &str, agg: &mut MetricsAgg) -> serde_json::Value {
    let completed = agg.passed + agg.failed + agg.interrupted + agg.error;
    let pass_rate = if completed == 0 {
        serde_json::Value::Null
    } else {
        serde_json::json!(agg.passed as f64 / completed as f64)
    };

    let (avg, p95, max) = duration_stats(&mut agg.durations_s);
    serde_json::json!({
        "key": key,
        "runs": agg.runs,
        "passed": agg.passed,
        "failed": agg.failed,
        "interrupted": agg.interrupted,
        "waiting": agg.waiting,
        "error": agg.error,
        "running": agg.running,
        "pass_rate": pass_rate,
        "duration_s": {
            "avg": avg,
            "p95": p95,
            "max": max,
        }
    })
}

fn duration_stats(durations_s: &mut [i64]) -> (f64, i64, i64) {
    if durations_s.is_empty() {
        return (0.0, 0, 0);
    }
    durations_s.sort_unstable();
    let len = durations_s.len();
    let sum: i64 = durations_s.iter().sum();
    let avg = sum as f64 / len as f64;
    let max = *durations_s.last().unwrap_or(&0);
    // nearest-rank: ceil(p/100 * n), 1-indexed
    let rank = ((0.95 * len as f64).ceil() as usize).max(1);
    let p95 = durations_s[rank - 1];
    (avg, p95, max)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn row(
        graph_id: &str,
        node_id: &str,
        status: &str,
        started_at: i64,
        completed_at: Option<i64>,
        platform: Option<&str>,
        model: Option<&str>,
    ) -> GraphMetricsRunRow {
        GraphMetricsRunRow {
            graph_id: graph_id.to_string(),
            node_id: node_id.to_string(),
            status: status.to_string(),
            started_at,
            completed_at,
            executed_platform: platform.map(str::to_string),
            executed_model: model.map(str::to_string),
        }
    }

    #[test]
    fn metrics_aggregation_buckets_sum_and_unknown_maps_running() {
        let rows = vec![
            row(
                "g1",
                "n1",
                "pass",
                10,
                Some(20),
                Some("opencode"),
                Some("m1"),
            ),
            row(
                "g1",
                "n1",
                "fail",
                10,
                Some(30),
                Some("opencode"),
                Some("m1"),
            ),
            row(
                "g1",
                "n1",
                "interrupted",
                10,
                Some(15),
                Some("opencode"),
                Some("m1"),
            ),
            row(
                "g1",
                "n1",
                "waiting",
                10,
                None,
                Some("opencode"),
                Some("m1"),
            ),
            row(
                "g1",
                "n1",
                "error",
                10,
                Some(12),
                Some("opencode"),
                Some("m1"),
            ),
            row(
                "g1",
                "n1",
                "running",
                10,
                None,
                Some("opencode"),
                Some("m1"),
            ),
            row(
                "g1",
                "n1",
                "weird_legacy",
                10,
                None,
                Some("opencode"),
                Some("m1"),
            ),
        ];
        let mut graph_names = HashMap::new();
        graph_names.insert("g1".to_string(), "Graph One".to_string());
        let node_names = HashMap::new();
        let groups = aggregate_graph_metrics(&rows, "graph", &graph_names, &node_names);
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        let runs = g["runs"].as_u64().unwrap();
        let passed = g["passed"].as_u64().unwrap();
        let failed = g["failed"].as_u64().unwrap();
        let interrupted = g["interrupted"].as_u64().unwrap();
        let waiting = g["waiting"].as_u64().unwrap();
        let error = g["error"].as_u64().unwrap();
        let running = g["running"].as_u64().unwrap();
        assert_eq!(runs, 7);
        assert_eq!(passed, 1);
        assert_eq!(failed, 1);
        assert_eq!(interrupted, 1);
        assert_eq!(waiting, 1);
        assert_eq!(error, 1);
        assert_eq!(running, 2); // running + unknown legacy
        assert_eq!(
            runs,
            passed + failed + interrupted + waiting + error + running
        );
    }

    #[test]
    fn metrics_running_excluded_from_durations_and_pass_rate_null() {
        let rows = vec![
            row("g1", "n1", "running", 100, None, None, None),
            row("g1", "n1", "waiting", 100, None, None, None),
            // A running row carrying a `completed_at` still counts as running
            // and must not leak its duration into the aggregates.
            row("g1", "n1", "running", 100, Some(160), None, None),
        ];
        let mut graph_names = HashMap::new();
        graph_names.insert("g1".to_string(), "G".to_string());
        let groups = aggregate_graph_metrics(&rows, "graph", &graph_names, &HashMap::new());
        let g = &groups[0];
        assert_eq!(g["running"].as_u64().unwrap(), 2);
        assert!(g["pass_rate"].is_null());
        assert_eq!(g["duration_s"]["avg"].as_f64().unwrap(), 0.0);
        assert_eq!(g["duration_s"]["p95"].as_i64().unwrap(), 0);
        assert_eq!(g["duration_s"]["max"].as_i64().unwrap(), 0);
    }

    #[test]
    fn metrics_p95_nearest_rank_integer_seconds() {
        let mut rows = Vec::new();
        for i in 1..=20 {
            rows.push(row("g1", "n1", "pass", 0, Some(i), None, None));
        }
        let mut graph_names = HashMap::new();
        graph_names.insert("g1".to_string(), "G".to_string());
        let groups = aggregate_graph_metrics(&rows, "graph", &graph_names, &HashMap::new());
        let dur = &groups[0]["duration_s"];
        assert_eq!(dur["p95"].as_i64().unwrap(), 19);
        assert_eq!(dur["max"].as_i64().unwrap(), 20);
        assert!((dur["avg"].as_f64().unwrap() - 10.5).abs() < 1e-9);
    }

    #[test]
    fn metrics_grouping_by_node_and_model() {
        let rows = vec![
            row(
                "g1",
                "n1",
                "pass",
                0,
                Some(10),
                Some("opencode"),
                Some("m1"),
            ),
            row("g1", "n2", "fail", 0, Some(10), Some("opencode"), None),
            row("g2", "n1", "error", 0, Some(10), None, None),
            row("g2", "n3", "pass", 0, Some(10), None, Some("m2")),
        ];
        let mut graph_names = HashMap::new();
        graph_names.insert("g1".to_string(), "Alpha".to_string());
        graph_names.insert("g2".to_string(), "Beta".to_string());
        let mut node_names = HashMap::new();
        node_names.insert("n1".to_string(), "Build".to_string());
        node_names.insert("n2".to_string(), "Test".to_string());
        node_names.insert("n3".to_string(), "Deploy".to_string());

        let by_node = aggregate_graph_metrics(&rows, "node", &graph_names, &node_names);
        let node_keys: Vec<&str> = by_node.iter().map(|g| g["key"].as_str().unwrap()).collect();
        assert!(node_keys.contains(&"Alpha/Build"));
        assert!(node_keys.contains(&"Alpha/Test"));
        assert!(node_keys.contains(&"Beta/Build"));
        assert!(node_keys.contains(&"Beta/Deploy"));

        let by_model = aggregate_graph_metrics(&rows, "model", &graph_names, &node_names);
        let model_keys: Vec<&str> = by_model
            .iter()
            .map(|g| g["key"].as_str().unwrap())
            .collect();
        assert!(model_keys.contains(&"opencode/m1"));
        assert!(model_keys.contains(&"opencode/unknown"));
        assert!(model_keys.contains(&"unknown"));
        assert!(model_keys.contains(&"unknown/m2"));
    }

    #[test]
    fn metrics_node_grouping_uses_ids_not_names() {
        let rows = vec![
            row("g1", "n1", "pass", 0, Some(10), None, None),
            row("g1", "n2", "fail", 0, Some(10), None, None),
        ];
        let mut graph_names = HashMap::new();
        graph_names.insert("g1".to_string(), "G".to_string());
        let mut node_names = HashMap::new();
        node_names.insert("n1".to_string(), "Same".to_string());
        node_names.insert("n2".to_string(), "Same".to_string());
        let groups = aggregate_graph_metrics(&rows, "node", &graph_names, &node_names);
        assert_eq!(groups.len(), 2, "same-name nodes must not merge");
    }

    #[test]
    fn metrics_json_shape() {
        let rows = vec![row("g1", "n1", "pass", 0, Some(5), Some("p"), Some("m"))];
        let mut graph_names = HashMap::new();
        graph_names.insert("g1".to_string(), "G".to_string());
        let groups = aggregate_graph_metrics(&rows, "graph", &graph_names, &HashMap::new());
        let g = &groups[0];
        assert!(g.get("key").is_some());
        assert!(g.get("runs").is_some());
        assert!(g.get("passed").is_some());
        assert!(g.get("failed").is_some());
        assert!(g.get("interrupted").is_some());
        assert!(g.get("waiting").is_some());
        assert!(g.get("error").is_some());
        assert!(g.get("running").is_some());
        assert!(g.get("pass_rate").is_some());
        let dur = &g["duration_s"];
        assert!(dur.get("avg").is_some());
        assert!(dur.get("p95").is_some());
        assert!(dur.get("max").is_some());
    }
}
