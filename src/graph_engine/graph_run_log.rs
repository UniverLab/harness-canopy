//! Append-only per-graph run log for daemon-initiated runs.
//!
//! A block is written once at dispatch settle and contains the trigger header,
//! one line per node run recorded during the dispatch, and a trailing blank line.

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::db::Database;

const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024;

/// Daemon trigger that initiated a graph dispatch/run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphRunTrigger {
    Cron,
    Watch,
    Autorun,
    AutoContinue,
}

impl GraphRunTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cron => "cron",
            Self::Watch => "watch",
            Self::Autorun => "autorun",
            Self::AutoContinue => "auto-continue",
        }
    }
}

/// Append one settled daemon-initiated run block to the graph's log file.
///
/// `run_started_at` is truncated to whole seconds: node-run rows persist
/// second-precision timestamps, and one dispatch appends from several settle
/// paths (per-outcome plus the dispatch settle) with slightly different
/// clock readings — the shared truncation keeps every header for one
/// dispatch byte-identical so the repeat is skipped instead of logged twice.
pub fn append_graph_run_block(
    db: &Database,
    graph_id: &str,
    trigger: GraphRunTrigger,
    run_started_at: DateTime<Utc>,
) -> Result<()> {
    let run_started_at =
        DateTime::<Utc>::from_timestamp(run_started_at.timestamp(), 0).unwrap_or(run_started_at);
    let Some(graph) = db.get_graph(graph_id)? else {
        return Ok(());
    };
    let log_path = graph.log_path();
    let node_lines = collect_node_run_lines(db, graph_id, run_started_at)?;
    let path = Path::new(&log_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    rotate_log_if_needed(path)?;

    let header = format!(
        "--- [trigger {}] {} {} ---",
        trigger.as_str(),
        graph_id,
        run_started_at.to_rfc3339()
    );
    if header_already_present(path, &header)? {
        return Ok(());
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{header}")?;
    for line in node_lines {
        writeln!(file, "{line}")?;
    }
    writeln!(file)?;
    Ok(())
}

fn header_already_present(path: &Path, header: &str) -> Result<bool> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(false);
    };
    Ok(content.lines().any(|line| line == header))
}

fn collect_node_run_lines(
    db: &Database,
    graph_id: &str,
    since: DateTime<Utc>,
) -> Result<Vec<String>> {
    let runs = db.list_graph_runs_for_graph(graph_id)?;
    let mut selected: Vec<_> = runs
        .into_iter()
        .filter(|run| run.started_at.timestamp() >= since.timestamp())
        .collect();
    selected.sort_by_key(|run| run.started_at);
    let mut lines = Vec::with_capacity(selected.len());
    for run in selected {
        let node_name = resolve_node_name(db, graph_id, &run.spec_id, &run.node_id);
        lines.push(format!("{}: {}", node_name, run.status.as_str()));
    }
    Ok(lines)
}

fn resolve_node_name(db: &Database, graph_id: &str, spec_id: &str, node_id: &str) -> String {
    if let Ok(nodes) = db.list_graph_nodes(spec_id) {
        if let Some(node) = nodes.into_iter().find(|n| n.id == node_id) {
            return node.name;
        }
    }
    if let Ok(nodes) = db.list_graph_nodes_for_graph(graph_id) {
        if let Some(node) = nodes.into_iter().find(|n| n.id == node_id) {
            return node.name;
        }
    }
    if let Ok(Some(spec)) = db.get_graph_spec(spec_id) {
        if let Some(spec_graph_id) = spec.graph_id.as_deref() {
            if let Ok(nodes) = db.list_graph_nodes_for_graph(spec_graph_id) {
                if let Some(node) = nodes.into_iter().find(|n| n.id == node_id) {
                    return node.name;
                }
            }
        }
    }
    node_id.to_string()
}

fn rotate_log_if_needed(path: &Path) -> Result<()> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if meta.len() < MAX_LOG_SIZE {
        return Ok(());
    }
    let old = path.with_extension("log.old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(path, old)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::graphs::{
        Graph, GraphNode, GraphNodeKind, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus,
        GraphStatus,
    };
    use chrono::Utc;
    use std::sync::Mutex;
    use tempfile::tempdir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn graph(id: &str) -> Graph {
        Graph {
            trigger_prompt: None,
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            archived: false,
            paused_by_reconciliation: false,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
            enabled: true,
            enable_at: None,
            expires_at: None,
        }
    }

    fn seed(
        db: &Database,
        graph_id: &str,
        run_id: &str,
        node_name: &str,
        status: GraphRunStatus,
        started_at: DateTime<Utc>,
    ) {
        db.insert_graph(&graph(graph_id)).unwrap();
        db.insert_graph_spec(&GraphSpec {
            intent: None,
            id: "spec".to_string(),
            graph_id: Some(graph_id.to_string()),
            name: "spec".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Running,
            started_at: Some(started_at),
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
            spec_type: None,
            spec_scope: None,
            spec_code: None,
            review_state: crate::domain::graphs::SpecReviewState::Approved,
            review_tier: None,
            review_findings: None,
            created_by_session: None,
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "node".to_string(),
            spec_id: Some("spec".to_string()),
            graph_id: None,
            name: node_name.to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 0,
            created_at: started_at,
        })
        .unwrap();
        db.insert_graph_run(&GraphNodeRun {
            id: run_id.to_string(),
            graph_id: graph_id.to_string(),
            spec_id: "spec".to_string(),
            node_id: "node".to_string(),
            status,
            input: None,
            output: None,
            started_at,
            completed_at: Some(started_at),
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
    fn append_graph_run_block_writes_header_lines_and_blank_line() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        let db_dir = tempdir().unwrap();
        let db = Database::new(&db_dir.path().join("t.db")).unwrap();
        let started = Utc::now();
        seed(&db, "g-log", "r1", "Build", GraphRunStatus::Pass, started);

        append_graph_run_block(&db, "g-log", GraphRunTrigger::Cron, started).unwrap();
        let log_path = db.get_graph("g-log").unwrap().unwrap().log_path();
        let content = std::fs::read_to_string(&log_path).unwrap();
        // Headers carry whole seconds: node-run rows persist second
        // precision, so sub-second clock readings are truncated.
        let truncated = chrono::DateTime::<Utc>::from_timestamp(started.timestamp(), 0).unwrap();
        assert!(content.starts_with(&format!(
            "--- [trigger cron] g-log {} ---\n",
            truncated.to_rfc3339()
        )));
        assert!(content.contains("Build: pass\n"));
        assert!(content.ends_with("\n\n"));

        if let Some(p) = prev {
            std::env::set_var("HOME", p);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn append_graph_run_block_writes_one_block_per_dispatch() {
        // One dispatch appends from several settle paths (per-outcome plus
        // the dispatch settle) with slightly different clock readings: the
        // full-precision claimed_at and the second-precision graph
        // started_at. Both must collapse into a single block.
        let _guard = ENV_LOCK.lock().unwrap();
        let home = tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        let db_dir = tempdir().unwrap();
        let db = Database::new(&db_dir.path().join("t.db")).unwrap();
        let started = Utc::now();
        seed(&db, "g-once", "r1", "Build", GraphRunStatus::Pass, started);

        append_graph_run_block(&db, "g-once", GraphRunTrigger::Cron, started).unwrap();
        let truncated = chrono::DateTime::<Utc>::from_timestamp(started.timestamp(), 0).unwrap();
        append_graph_run_block(&db, "g-once", GraphRunTrigger::Cron, truncated).unwrap();

        let log_path = db.get_graph("g-once").unwrap().unwrap().log_path();
        let content = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            content
                .lines()
                .filter(|line| line.starts_with("--- [trigger"))
                .count(),
            1,
            "one dispatch must append exactly one block, got:\n{content}"
        );

        if let Some(p) = prev {
            std::env::set_var("HOME", p);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn append_graph_run_block_rotates_at_5mb() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        let db_dir = tempdir().unwrap();
        let db = Database::new(&db_dir.path().join("t.db")).unwrap();
        let started = Utc::now();
        seed(&db, "g-rot", "r1", "Build", GraphRunStatus::Pass, started);
        let log_path = db.get_graph("g-rot").unwrap().unwrap().log_path();
        std::fs::create_dir_all(std::path::Path::new(&log_path).parent().unwrap()).unwrap();
        std::fs::write(&log_path, vec![b'x'; (5 * 1024 * 1024) as usize]).unwrap();

        append_graph_run_block(&db, "g-rot", GraphRunTrigger::Watch, started).unwrap();
        let old = std::path::Path::new(&log_path).with_extension("log.old");
        assert!(old.exists());
        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("--- [trigger watch] g-rot"));

        if let Some(p) = prev {
            std::env::set_var("HOME", p);
        } else {
            std::env::remove_var("HOME");
        }
    }
}
