use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Error as IoError, ErrorKind};

use crate::db::Database;
use crate::domain::graphs::{
    ArchiveGraphOutcome, Graph, GraphCompletionHook, GraphCompletionHookRun, GraphDetails,
    GraphEdge, GraphEdgeCondition, GraphHookEvent, GraphNode, GraphNodeKind, GraphNodeRun,
    GraphResetOutcome, GraphRunStatus, GraphSpec, GraphSpecDetails, GraphSpecStatus, GraphStatus,
    SpecAdminStatusOutcome,
};
use crate::domain::models::Trigger;

mod crud;
mod hooks;
mod queue;
mod reconcile;
mod runs;
mod specs;
mod telemetry;
mod topology;

/// CB43: one platform+model pair this installation has run in the window —
/// when it last ran, how many times, and how the most recent run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentModelUsage {
    pub platform: String,
    pub model: Option<String>,
    pub last_run: DateTime<Utc>,
    pub count: i64,
    pub last_outcome: String,
}

/// CB43/CB46: shared window for installation recent usage.
pub const RECENT_USAGE_WINDOW_DAYS: i64 = 30;

#[derive(Debug, Clone)]
pub struct RecentPairSource {
    pub platform: String,
    pub model: Option<String>,
    pub started_at: DateTime<Utc>,
    pub status: String,
    pub source: RecentPairSourceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecentPairSourceKind {
    GraphNode {
        graph_id: String,
        graph_name: String,
        node_id: String,
        node_name: String,
    },
    CompletionHook {
        graph_id: String,
        graph_name: String,
        event: String,
        hook_index: i64,
    },
    BackgroundAgent {
        agent_id: String,
        prompt: String,
    },
    SubagentSpawn {
        workdir: String,
    },
}

/// One run sample during [`Database::list_recent_platform_model_usage`]:
/// when it started and how it ended.
struct RecentUsageSample {
    started_at: DateTime<Utc>,
    status: String,
}

/// A graph node/edge must target exactly one of `spec_id`/`graph_id` — never
/// both (ambiguous ownership) and never neither (orphaned row no graph
/// would ever load). Checked here, before the row ever reaches the DB, so
/// callers get an actionable message instead of a raw `CHECK constraint
/// failed` from SQLite.
fn validate_single_target(spec_id: Option<&str>, graph_id: Option<&str>) -> Result<()> {
    match (spec_id, graph_id) {
        (Some(_), Some(_)) => Err(anyhow!(
            "Graph node/edge must target exactly one of spec_id or graph_id, not both."
        )),
        (None, None) => Err(anyhow!(
            "Graph node/edge must target exactly one of spec_id or graph_id."
        )),
        _ => Ok(()),
    }
}

/// CT3: byte bound for one served tail stream — mirrors the engine's
/// 64KB check-output truncation so a chunk flood (e.g. one 100KB line)
/// can never blow the TUI's memory through the line cap alone.
const TAIL_MAX_BYTES: usize = 64 * 1024;

/// CT3: keep the last [`TAIL_MAX_BYTES`] bytes of `text` on a char
/// boundary. Applied before the line cap so giant single lines are
/// bounded too.
fn take_last_bytes(text: &str) -> &str {
    if text.len() <= TAIL_MAX_BYTES {
        return text;
    }
    let mut start = text.len() - TAIL_MAX_BYTES;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// CT3: keep the last `max_lines` lines of `text`. Line-oriented (not
/// byte-oriented) so the dialog's on-screen cap bounds memory without
/// splitting a line; `max_lines == 0` yields an empty string.
fn take_last_lines(text: &str, max_lines: usize) -> String {
    if max_lines == 0 || text.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn map_graph_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Graph> {
    let trigger = row
        .get::<_, Option<String>>(5)?
        .as_deref()
        .map(decode_graph_trigger)
        .transpose()?;
    let hooks = decode_graph_hooks(
        row.get::<_, Option<String>>(17)?.as_deref(),
        row.get::<_, Option<String>>(11)?.as_deref(),
    )?;
    Ok(Graph {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        workdir: row.get(3)?,
        status: GraphStatus::from_str(&row.get::<_, String>(4)?),
        trigger,
        created_at: from_timestamp(row.get(6)?)?,
        started_at: row
            .get::<_, Option<i64>>(7)?
            .map(from_timestamp)
            .transpose()?,
        completed_at: row
            .get::<_, Option<i64>>(8)?
            .map(from_timestamp)
            .transpose()?,
        autorun_at: row
            .get::<_, Option<i64>>(9)?
            .map(from_timestamp)
            .transpose()?,
        active_run_queue_id: row.get(10)?,
        hooks,
        auto_continue_at: row
            .get::<_, Option<i64>>(12)?
            .map(from_timestamp)
            .transpose()?,
        auto_continue_action: row.get(13)?,
        archived: row.get(14)?,
        paused_by_reconciliation: row.get(15)?,
        infra_node_id: row.get(16)?,
        allow_dirty_start: row.get(18)?,
    })
}

/// Encode a graph trigger into the `(trigger_type, trigger_config)` column pair,
/// mirroring how agents persist their trigger: a short type label plus the full
/// trigger serialized as JSON.
fn encode_graph_trigger(trigger: Option<&Trigger>) -> Result<(Option<String>, Option<String>)> {
    match trigger {
        Some(trigger) => Ok((
            Some(trigger.type_str().to_string()),
            Some(serde_json::to_string(trigger)?),
        )),
        None => Ok((None, None)),
    }
}

/// Decode the `trigger_config` JSON back into a [`Trigger`].
fn decode_graph_trigger(raw: &str) -> rusqlite::Result<Trigger> {
    serde_json::from_str(raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error))
    })
}

/// Encode a graph's hooks map as JSON for the `hooks` column. `None` (no
/// hooks configured) stores `NULL` — exactly today's (pre-N2) row shape.
fn encode_graph_hooks(
    hooks: &std::collections::BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>>,
) -> Result<Option<String>> {
    if hooks.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(hooks)?))
}

/// Decode the `hooks` column JSON back into the event-keyed map. Falls
/// back to the legacy `on_completed` column when `hooks` is NULL/empty.
fn decode_graph_hooks(
    hooks_raw: Option<&str>,
    on_completed_raw: Option<&str>,
) -> rusqlite::Result<std::collections::BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>>> {
    if let Some(raw) = hooks_raw {
        if !raw.is_empty() {
            return serde_json::from_str(raw).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    11,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            });
        }
    }
    // Fallback: legacy `on_completed` column
    if let Some(raw) = on_completed_raw {
        let hook = decode_graph_completion_hook(raw)?;
        let mut map = std::collections::BTreeMap::new();
        map.insert(GraphHookEvent::OnCompleted, vec![hook]);
        return Ok(map);
    }
    Ok(std::collections::BTreeMap::new())
}

/// Encode a graph's `on_completed` hook config as JSON for the `on_completed`
/// column. `None` (no hook configured) stores `NULL` — exactly today's
/// (pre-N2) row shape.
fn encode_graph_completion_hook(hook: Option<&GraphCompletionHook>) -> Result<Option<String>> {
    hook.map(serde_json::to_string)
        .transpose()
        .map_err(Into::into)
}

/// Decode the `on_completed` column JSON back into a [`GraphCompletionHook`].
fn decode_graph_completion_hook(raw: &str) -> rusqlite::Result<GraphCompletionHook> {
    serde_json::from_str(raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(11, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn map_graph_spec_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphSpec> {
    Ok(GraphSpec {
        id: row.get(0)?,
        graph_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        position: row.get(4)?,
        parallelizable: row.get(5)?,
        status: GraphSpecStatus::from_str(&row.get::<_, String>(6)?),
        started_at: row
            .get::<_, Option<i64>>(7)?
            .map(from_timestamp)
            .transpose()?,
        completed_at: row
            .get::<_, Option<i64>>(8)?
            .map(from_timestamp)
            .transpose()?,
        spec_start_head: row.get(9)?,
        spec_start_dirty: row.get(10)?,
        workdir: row.get(11)?,
        completed_via: row.get(12)?,
        completed_via_reason: row.get(13)?,
        completed_via_at: row
            .get::<_, Option<i64>>(14)?
            .map(from_timestamp)
            .transpose()?,
        spec_committed_head: row.get(15)?,
        spec_end_dirty: row.get(16)?,
        spec_end_dirty_paths: row
            .get::<_, Option<String>>(17)?
            .map(|raw| raw.lines().map(str::to_string).collect()),
    })
}

fn map_graph_node_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNode> {
    let kind = GraphNodeKind::from_str(&row.get::<_, String>(4)?).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(IoError::new(
                ErrorKind::InvalidData,
                "Invalid graph node kind",
            )),
        )
    })?;
    let config_raw: String = row.get(5)?;
    let config = parse_json_value(&config_raw)?;

    Ok(GraphNode {
        id: row.get(0)?,
        spec_id: row.get(1)?,
        graph_id: row.get(2)?,
        name: row.get(3)?,
        kind,
        config,
        position: row.get(6)?,
        created_at: from_timestamp(row.get(7)?)?,
    })
}

fn map_graph_edge_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphEdge> {
    let condition_tag: String = row.get(5)?;
    let route_label: Option<String> = row.get(6)?;
    let condition =
        GraphEdgeCondition::from_parts(&condition_tag, route_label).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(IoError::new(
                    ErrorKind::InvalidData,
                    "Invalid graph edge condition",
                )),
            )
        })?;

    Ok(GraphEdge {
        id: row.get(0)?,
        spec_id: row.get(1)?,
        graph_id: row.get(2)?,
        from_node: row.get(3)?,
        to_node: row.get(4)?,
        condition,
    })
}

fn active_graph_run_for_spec_locked(
    conn: &rusqlite::Connection,
    spec_id: &str,
) -> rusqlite::Result<Option<GraphNodeRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
         FROM graph_runs
         WHERE spec_id = ?1 AND status = 'running'
         ORDER BY started_at DESC
         LIMIT 1",
    )?;
    stmt.query_row(params![spec_id], map_graph_run_row)
        .optional()
}

fn map_graph_run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNodeRun> {
    Ok(GraphNodeRun {
        id: row.get(0)?,
        graph_id: row.get(1)?,
        spec_id: row.get(2)?,
        node_id: row.get(3)?,
        status: GraphRunStatus::from_str(&row.get::<_, String>(4)?),
        input: row
            .get::<_, Option<String>>(5)?
            .as_deref()
            .map(parse_json_value)
            .transpose()?,
        output: row
            .get::<_, Option<String>>(6)?
            .as_deref()
            .map(parse_json_value)
            .transpose()?,
        started_at: from_timestamp(row.get(7)?)?,
        completed_at: row
            .get::<_, Option<i64>>(8)?
            .map(from_timestamp)
            .transpose()?,
        iteration: row.get(9)?,
        pid: row.get(10)?,
        boot_id: row.get(11)?,
        session_id: row.get(12)?,
        executed_platform: row.get(13)?,
        executed_model: row.get(14)?,
    })
}

fn map_graph_completion_hook_run_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<GraphCompletionHookRun> {
    let event_str: String = row.get(9)?;
    let event = GraphHookEvent::from_str(&event_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            9,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid graph hook event",
            )),
        )
    })?;
    Ok(GraphCompletionHookRun {
        id: row.get(0)?,
        graph_id: row.get(1)?,
        event,
        hook_index: row.get(10)?,
        status: GraphRunStatus::from_str(&row.get::<_, String>(2)?),
        output: row
            .get::<_, Option<String>>(3)?
            .as_deref()
            .map(parse_json_value)
            .transpose()?,
        summary: row.get(4)?,
        started_at: from_timestamp(row.get(5)?)?,
        completed_at: row
            .get::<_, Option<i64>>(6)?
            .map(from_timestamp)
            .transpose()?,
        pid: row.get(7)?,
        boot_id: row.get(8)?,
        executed_platform: row.get(11)?,
        executed_model: row.get(12)?,
    })
}

fn from_timestamp(value: i64) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(value, 0).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(IoError::new(
                ErrorKind::InvalidData,
                "Invalid timestamp value",
            )),
        )
    })
}

fn parse_json_value(raw: &str) -> rusqlite::Result<Value> {
    serde_json::from_str(raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graphs::{Graph, GraphStatus};
    use chrono::Utc;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    fn test_db() -> Database {
        let dir = tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn sample_graph(id: &str) -> Graph {
        Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: id.to_string(),
            name: format!("Graph {id}"),
            description: None,
            workdir: "/tmp/test".to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: BTreeMap::new(),
        }
    }

    #[test]
    fn insert_and_get_graph() {
        let db = test_db();
        let graph_obj = sample_graph("loop1");
        db.insert_graph(&graph_obj).unwrap();

        let retrieved = db.get_graph("loop1").unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.id, "loop1");
        assert_eq!(retrieved.name, "Graph loop1");
    }

    #[test]
    fn get_graph_not_found() {
        let db = test_db();
        let result = db.get_graph("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn delete_graph() {
        let db = test_db();
        let graph_obj = sample_graph("loop1");
        db.insert_graph(&graph_obj).unwrap();

        db.delete_graph("loop1").unwrap();
        let retrieved = db.get_graph("loop1").unwrap();
        assert!(retrieved.is_none());
    }

    #[test]
    fn list_graphs_empty() {
        let db = test_db();
        let graphs = db.list_graphs(None, false).unwrap();
        assert!(graphs.is_empty());
    }

    #[test]
    fn list_graphs_with_graphs() {
        let db = test_db();
        let loop1 = sample_graph("loop1");
        let loop2 = sample_graph("loop2");
        db.insert_graph(&loop1).unwrap();
        db.insert_graph(&loop2).unwrap();

        let graphs = db.list_graphs(None, false).unwrap();
        assert_eq!(graphs.len(), 2);
    }

    #[test]
    fn update_graph_status() {
        let db = test_db();
        let graph_obj = sample_graph("loop1");
        db.insert_graph(&graph_obj).unwrap();

        db.update_graph_status("loop1", GraphStatus::Running, None, None)
            .unwrap();
        let retrieved = db.get_graph("loop1").unwrap().unwrap();
        assert_eq!(retrieved.status, GraphStatus::Running);
    }

    #[test]
    fn schedule_and_clear_graph_autorun() {
        let db = test_db();
        let graph_obj = sample_graph("loop1");
        db.insert_graph(&graph_obj).unwrap();

        let scheduled_at = Utc::now() + chrono::Duration::hours(1);
        db.schedule_graph_autorun("loop1", scheduled_at).unwrap();

        let retrieved = db.get_graph("loop1").unwrap().unwrap();
        assert!(retrieved.autorun_at.is_some());

        db.clear_graph_autorun("loop1").unwrap();
        let retrieved = db.get_graph("loop1").unwrap().unwrap();
        assert!(retrieved.autorun_at.is_none());
    }

    #[test]
    fn list_pending_autorun_graphs_empty() {
        let db = test_db();
        let graphs = db.list_pending_autorun_graphs().unwrap();
        assert!(graphs.is_empty());
    }

    #[test]
    fn list_pending_autorun_graphs_with_scheduled() {
        let db = test_db();
        let graph_obj = sample_graph("loop1");
        db.insert_graph(&graph_obj).unwrap();

        let scheduled_at = Utc::now() - chrono::Duration::hours(1);
        db.schedule_graph_autorun("loop1", scheduled_at).unwrap();

        let graphs = db.list_pending_autorun_graphs().unwrap();
        assert_eq!(graphs.len(), 1);
    }

    #[test]
    fn validate_single_target_both_none() {
        let result = validate_single_target(None, None);
        assert!(result.is_err());
    }

    #[test]
    fn validate_single_target_both_some() {
        let result = validate_single_target(Some("spec1"), Some("loop1"));
        assert!(result.is_err());
    }

    #[test]
    fn validate_single_target_spec_only() {
        let result = validate_single_target(Some("spec1"), None);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_single_target_graph_only() {
        let result = validate_single_target(None, Some("loop1"));
        assert!(result.is_ok());
    }

    #[test]
    fn encode_and_decode_graph_trigger_none() {
        let (type_str, config) = encode_graph_trigger(None).unwrap();
        assert!(type_str.is_none());
        assert!(config.is_none());
    }

    #[test]
    fn parse_json_value_valid() {
        let result = parse_json_value(r#"{"key": "value"}"#);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_json_value_invalid() {
        let result = parse_json_value("invalid json");
        assert!(result.is_err());
    }

    #[test]
    fn list_graph_last_run_times_empty_when_no_runs() {
        let db = test_db();
        let graph_obj = sample_graph("loop1");
        db.insert_graph(&graph_obj).unwrap();

        let times = db.list_graph_last_run_times().unwrap();
        assert!(
            times.is_empty(),
            "a graph with no recorded runs must not appear in the map"
        );
    }

    #[test]
    fn list_graph_last_run_times_returns_max_started_at_per_graph() {
        let db = test_db();
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES ('loop1', 'l1', '/tmp', 'draft', 0)",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES ('loop2', 'l2', '/tmp', 'draft', 0)",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES ('spec1', 'loop1', 's1', 0, 0, 'pending')",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at) VALUES ('n1', 'spec1', NULL, 'node1', 'agent', '{}', 0, 0)",
            params![],
        )
        .unwrap();
        // Two runs on loop1 — the later one (started_at 500) must win.
        conn.execute(
            "INSERT INTO graph_runs (id, graph_id, spec_id, node_id, status, started_at, iteration) VALUES ('run1', 'loop1', 'spec1', 'n1', 'success', 100, 1)",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_runs (id, graph_id, spec_id, node_id, status, started_at, iteration) VALUES ('run2', 'loop1', 'spec1', 'n1', 'success', 500, 2)",
            params![],
        )
        .unwrap();
        drop(conn);

        let times = db.list_graph_last_run_times().unwrap();
        assert_eq!(times.len(), 1, "loop2 has no runs, so must be absent");
        assert_eq!(
            times.get("loop1").unwrap().timestamp(),
            500,
            "must report the MAX started_at, not the first run"
        );
        assert!(!times.contains_key("loop2"));
    }

    /// Seeds loop1 (spec1: nodes n1/n2, spec2: node n3) and loop2 (spec3:
    /// node n4) with runs at increasing `started_at`, so tests can assert
    /// default ordering, spec/node filters, and paging against a fixture
    /// that mirrors a real multi-spec, multi-node graph.
    fn seed_node_runs_fixture(db: &Database) {
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES ('loop1', 'l1', '/tmp', 'running', 0)",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES ('loop2', 'l2', '/tmp', 'running', 0)",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES ('spec1', 'loop1', 's1', 0, 0, 'pending')",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES ('spec2', 'loop1', 's2', 1, 0, 'pending')",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES ('spec3', 'loop2', 's3', 0, 0, 'pending')",
            params![],
        )
        .unwrap();
        for (node_id, spec_id, position) in [
            ("n1", "spec1", 0),
            ("n2", "spec1", 1),
            ("n3", "spec2", 0),
            ("n4", "spec3", 0),
        ] {
            conn.execute(
                "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at) VALUES (?1, ?2, NULL, ?1, 'agent', '{}', ?3, 0)",
                params![node_id, spec_id, position],
            )
            .unwrap();
        }
        // loop1: run1 (spec1/n1, t=100), run2 (spec1/n2, t=200), run3
        // (spec2/n3, t=300, fail). loop2: run4 (spec3/n4, t=400) — must never
        // leak into a loop1 query.
        for (id, graph_id, spec_id, node_id, status, started_at) in [
            ("run1", "loop1", "spec1", "n1", "pass", 100),
            ("run2", "loop1", "spec1", "n2", "pass", 200),
            ("run3", "loop1", "spec2", "n3", "fail", 300),
            ("run4", "loop2", "spec3", "n4", "pass", 400),
        ] {
            conn.execute(
                "INSERT INTO graph_runs (id, graph_id, spec_id, node_id, status, started_at, iteration) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
                params![id, graph_id, spec_id, node_id, status, started_at],
            )
            .unwrap();
        }
        drop(conn);
    }

    #[test]
    fn list_graph_node_runs_defaults_to_most_recent_first_scoped_to_graph() {
        let db = test_db();
        seed_node_runs_fixture(&db);

        let runs = db.list_graph_node_runs("loop1", None, None, 10, 0).unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["run3", "run2", "run1"],
            "most-recent-started_at run must lead, and loop2's run4 must never appear"
        );
    }

    #[test]
    fn list_graph_node_runs_filters_by_spec_and_node() {
        let db = test_db();
        seed_node_runs_fixture(&db);

        let by_spec = db
            .list_graph_node_runs("loop1", Some("spec1"), None, 10, 0)
            .unwrap();
        assert_eq!(
            by_spec.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["run2", "run1"]
        );

        let by_node = db
            .list_graph_node_runs("loop1", None, Some("n3"), 10, 0)
            .unwrap();
        assert_eq!(
            by_node.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["run3"]
        );

        let by_both = db
            .list_graph_node_runs("loop1", Some("spec1"), Some("n1"), 10, 0)
            .unwrap();
        assert_eq!(
            by_both.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["run1"]
        );
    }

    #[test]
    fn list_graph_node_runs_pages_with_limit_and_offset() {
        let db = test_db();
        seed_node_runs_fixture(&db);

        let first_page = db.list_graph_node_runs("loop1", None, None, 2, 0).unwrap();
        assert_eq!(
            first_page.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["run3", "run2"]
        );

        let second_page = db.list_graph_node_runs("loop1", None, None, 2, 2).unwrap();
        assert_eq!(
            second_page
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            vec!["run1"],
            "offset must skip past the first page's runs, not repeat them"
        );
    }

    #[test]
    fn list_all_graph_nodes_returns_every_node_regardless_of_spec_or_graph_scope() {
        let db = test_db();
        let lp = sample_graph("graph-1");
        db.insert_graph(&lp).unwrap();
        let spec = GraphSpec {
            id: "spec-1".to_string(),
            graph_id: Some(lp.id.clone()),
            name: "Spec".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        db.insert_graph_spec(&spec).unwrap();

        let spec_scoped = GraphNode {
            id: "node-spec".to_string(),
            spec_id: Some(spec.id),
            graph_id: None,
            name: "Spec Node".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 1,
            created_at: Utc::now(),
        };
        let graph_scoped = GraphNode {
            id: "node-graph".to_string(),
            spec_id: None,
            graph_id: Some(lp.id),
            name: "Graph Node".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 1,
            created_at: Utc::now(),
        };
        db.insert_graph_node(&spec_scoped).unwrap();
        db.insert_graph_node(&graph_scoped).unwrap();

        let all_ids: Vec<String> = db
            .list_all_graph_nodes()
            .unwrap()
            .into_iter()
            .map(|node| node.id)
            .collect();
        assert_eq!(all_ids.len(), 2);
        assert!(all_ids.contains(&"node-spec".to_string()));
        assert!(all_ids.contains(&"node-graph".to_string()));
    }

    #[test]
    fn find_dirty_predecessor_spec_excludes_given_id_and_completed_status() {
        let db = test_db();
        let workdir = "/tmp/dirty-predecessor-test";
        let mk = |id: &str, status: GraphSpecStatus, workdir: Option<&str>| GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: id.to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: workdir.map(str::to_string),
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        // Completed shares the workdir but is never a candidate; Interrupted
        // sits on an unrelated workdir so it can never be mistaken for this
        // workdir's predecessor either — leaving exactly one qualifying spec.
        db.insert_graph_spec(&mk(
            "spec-completed",
            GraphSpecStatus::Completed,
            Some(workdir),
        ))
        .unwrap();
        db.insert_graph_spec(&mk("spec-failed", GraphSpecStatus::Failed, Some(workdir)))
            .unwrap();
        db.insert_graph_spec(&mk(
            "spec-interrupted",
            GraphSpecStatus::Interrupted,
            Some("/tmp/unrelated-workdir"),
        ))
        .unwrap();

        let found = db
            .find_dirty_predecessor_spec(workdir, None)
            .unwrap()
            .expect("the failed spec on this workdir should be found");
        assert_eq!(found.id, "spec-failed");

        let excluded = db
            .find_dirty_predecessor_spec(workdir, Some("spec-failed"))
            .unwrap();
        assert!(
            excluded.is_none(),
            "excluding the only qualifying spec must fall through to None"
        );
    }

    // ── CT3: live tail storage ──────────────────────────────────────

    fn tail_test_run(db: &Database, run_id: &str) {
        use crate::domain::graphs::GraphRunStatus;
        db.insert_graph(&sample_graph("tail-graph")).unwrap();
        db.insert_graph_spec(&crate::domain::graphs::GraphSpec {
            id: "tail-spec".to_string(),
            graph_id: Some("tail-graph".to_string()),
            name: "Tail spec".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: crate::domain::graphs::GraphSpecStatus::Running,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();
        db.insert_graph_node(&crate::domain::graphs::GraphNode {
            id: "tail-node".to_string(),
            spec_id: Some("tail-spec".to_string()),
            graph_id: None,
            name: "Tail node".to_string(),
            kind: crate::domain::graphs::GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 0,
            created_at: Utc::now(),
        })
        .unwrap();
        db.insert_graph_run(&crate::domain::graphs::GraphNodeRun {
            id: run_id.to_string(),
            graph_id: "tail-graph".to_string(),
            spec_id: "tail-spec".to_string(),
            node_id: "tail-node".to_string(),
            status: GraphRunStatus::Running,
            input: None,
            output: None,
            started_at: Utc::now(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .unwrap();
    }

    #[test]
    fn append_and_get_graph_run_tail_returns_chunks_in_order() {
        let db = test_db();
        tail_test_run(&db, "run-tail-1");
        db.append_graph_run_output("run-tail-1", "stdout", "out-a\n")
            .unwrap();
        db.append_graph_run_output("run-tail-1", "stderr", "err-a\n")
            .unwrap();
        db.append_graph_run_output("run-tail-1", "stdout", "out-b\n")
            .unwrap();

        let (stdout, stderr) = db.get_graph_run_tail("run-tail-1", 1000).unwrap();
        assert_eq!(stdout, "out-a\nout-b");
        assert_eq!(stderr, "err-a");
    }

    #[test]
    fn get_graph_run_tail_caps_lines_to_max() {
        let db = test_db();
        tail_test_run(&db, "run-tail-2");
        for i in 0..10 {
            db.append_graph_run_output("run-tail-2", "stdout", &format!("line {i}\n"))
                .unwrap();
        }
        let (stdout, _) = db.get_graph_run_tail("run-tail-2", 3).unwrap();
        assert_eq!(stdout, "line 7\nline 8\nline 9");
    }

    #[test]
    fn set_graph_run_tail_snapshot_serves_post_completion_tail() {
        let db = test_db();
        tail_test_run(&db, "run-tail-3");
        // Zero-output hang: no chunks at all — the snapshot is the only
        // source, and an empty one must read back as empty, not error.
        db.set_graph_run_tail_snapshot("run-tail-3", Some("final out"), Some(""))
            .unwrap();
        let (stdout, stderr) = db.get_graph_run_tail("run-tail-3", 1000).unwrap();
        assert_eq!(stdout, "final out");
        assert_eq!(stderr, "");
    }

    #[test]
    fn append_graph_run_output_rejects_unknown_stream() {
        let db = test_db();
        tail_test_run(&db, "run-tail-4");
        assert!(
            db.append_graph_run_output("run-tail-4", "stdin", "x")
                .is_err(),
            "CHECK constraint must reject streams other than stdout/stderr"
        );
    }
}
