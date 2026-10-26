//! Database integration tests (top-level helpers; test bodies live in sibling files).
use super::*;
use crate::db::intelligence::{IntelligenceNodeInput, IntelligenceRelationInput};
use crate::domain::graphs::{
    Graph, GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphNodeRun, GraphRunStatus,
    GraphSpec, GraphSpecStatus, GraphStatus, SpecAdminStatusOutcome,
};
use crate::domain::models::{Agent, Cli, RunLog, RunStatus, Trigger, TriggerType, WatchEvent};
use crate::domain::queues::Queue;
use crate::domain::spec_type::SpecType;
use crate::domain::sync::{
    IntentPayload, MessageKind, MissionImpact, StatusPayload, WorkspaceStatus,
};
use chrono::{Duration, TimeZone, Utc};
use tempfile::{tempdir, NamedTempFile};

/// Create an in-memory-like DB backed by a temp file (`SQLite` needs a real file for WAL).
fn test_db() -> Database {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Database::new(&path).expect("create test db")
}

fn sample_cron_agent(id: &str) -> Agent {
    Agent {
        id: id.to_string(),
        prompt: "Run tests".to_string(),
        trigger: Some(Trigger::Cron {
            schedule_expr: "0 9 * * *".to_string(),
        }),
        cli: Cli::new("opencode"),
        model: None,
        effort: None,
        working_dir: Some("/tmp/project".to_string()),
        enabled: true,
        enable_at: None,
        created_at: Utc::now(),
        log_path: "/tmp/test.log".to_string(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    }
}

fn sample_watch_agent(id: &str) -> Agent {
    Agent {
        id: id.to_string(),
        prompt: "Handle file change".to_string(),
        trigger: Some(Trigger::Watch {
            path: "/tmp/watched".to_string(),
            events: vec![WatchEvent::Create, WatchEvent::Modify],
            debounce_seconds: 5,
            recursive: true,
        }),
        cli: Cli::new("kiro"),
        model: Some("claude-4".to_string()),
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: Utc::now(),
        log_path: format!("/tmp/{}.log", id),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    }
}

fn sample_manual_agent(id: &str) -> Agent {
    Agent {
        id: id.to_string(),
        prompt: "Manual task".to_string(),
        trigger: None,
        cli: Cli::new("opencode"),
        model: None,
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: Utc::now(),
        log_path: "/tmp/manual.log".to_string(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    }
}

fn sample_graph(id: &str) -> Graph {
    Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        require_spec_type: false,
        id: id.to_string(),
        name: "Auth graph".to_string(),
        description: Some("Implements auth in ordered specs".to_string()),
        workdir: "/tmp/project".to_string(),
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
    }
}

fn sample_graph_spec(graph_id: &str, id: &str, position: i64) -> GraphSpec {
    GraphSpec {
        id: id.to_string(),
        graph_id: Some(graph_id.to_string()),
        name: format!("Spec {position}"),
        description: Some("Do a slice of the feature".to_string()),
        position,
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
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    }
}

fn sample_graph_node(spec_id: &str, id: &str, position: i64) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: format!("Node {position}"),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "cargo test",
            "success_condition": "exit_code_0"
        }),
        position,
        created_at: Utc::now(),
    }
}

fn sample_standalone_spec(id: &str, workdir: Option<&str>) -> GraphSpec {
    GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: format!("Backlog {id}"),
        description: Some(
            "Functional Requirements:\n- A\n\nNon-Functional Requirements:\n- B\n\nObjective:\n- C\n\nConstraints:\n- D\n\nGuidelines:\n- E\n\nIn Scope:\n- F\n\nOut of Scope:\n- G".to_string(),
        ),
        position: 0,
        parallelizable: false,
        status: GraphSpecStatus::Pending,
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
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    }
}

fn graph_with_trigger(id: &str, trigger: Option<Trigger>) -> Graph {
    Graph {
        trigger,
        ..sample_graph(id)
    }
}

fn sample_queue(id: &str) -> Queue {
    Queue {
        id: id.to_string(),
        name: format!("{id} name"),
        created_at: Utc::now(),
    }
}

// ── RS3: context groups within a queue ──────────────────────────

/// Seed a `graph_runs` row for `spec_id`/`node_id` carrying `session_id`, then
/// stamp `spec_id`'s terminal status — the shape a completed/failed grouped
/// sibling leaves behind for `group_session_for_node` to read.
fn seed_group_sibling(
    db: &Database,
    graph_id: &str,
    spec_id: &str,
    node_id: &str,
    session_id: Option<&str>,
    status: GraphSpecStatus,
) {
    let run = GraphNodeRun {
        id: format!("run-{spec_id}-{node_id}"),
        graph_id: graph_id.to_string(),
        spec_id: spec_id.to_string(),
        node_id: node_id.to_string(),
        status: GraphRunStatus::Pass,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: Some(Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: session_id.map(str::to_string),
        executed_platform: None,
        executed_model: None,
    };
    db.insert_graph_run(&run).unwrap();
    db.update_graph_spec_status(spec_id, status, Some(Utc::now()), Some(Utc::now()))
        .unwrap();
}

/// Scaffold a graph with two grouped members (`spec-a`, `spec-b`) and one
/// grouped/ungrouped setup, returning the db. Both share a single graph-level
/// node id `node-impl`.
fn rs3_fixture() -> Database {
    let db = test_db();
    let lp = sample_graph("wf-rs3");
    db.insert_graph(&lp).unwrap();
    for id in ["spec-a", "spec-b", "spec-c"] {
        db.insert_graph_spec(&sample_standalone_spec(id, None))
            .unwrap();
    }
    // A graph-level node the runs can reference (graph_runs FK to graph_nodes).
    let mut node = sample_graph_node("spec-a", "node-impl", 1);
    node.spec_id = None;
    node.graph_id = Some(lp.id.clone());
    db.insert_graph_node(&node).unwrap();
    let mut review = sample_graph_node("spec-a", "node-review", 2);
    review.spec_id = None;
    review.graph_id = Some(lp.id);
    db.insert_graph_node(&review).unwrap();
    db.insert_queue(&sample_queue("queue-1")).unwrap();
    db
}

// ── B36: interrupted-run marking (no git stash) ──────────────────────────

fn init_git_repo(path: &std::path::Path) {
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .expect("git command failed to run");
        assert!(status.success(), "git {:?} failed", args);
    };
    run(&["init", "-q"]);
    run(&["config", "user.name", "Test"]);
    run(&["config", "user.email", "test@example.com"]);
    std::fs::write(path.join("README.md"), "test").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "init"]);
}

fn git_head(path: &std::path::Path) -> String {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .expect("git rev-parse failed to run");
    assert!(output.status.success(), "git rev-parse HEAD failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn git_is_clean(path: &std::path::Path) -> bool {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output()
        .expect("git status failed to run");
    output.stdout.is_empty()
}

// ── CM9: typed project graph ──────────────────────────────────────────

/// Register a project row at an on-disk path (hash derived like production).
fn cm9_register_at(db: &Database, path: &std::path::Path) -> String {
    let s = path.to_string_lossy().to_string();
    let hash = crate::domain::project::workdir_hash(&s);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| s.clone());
    db.upsert_project(&crate::domain::project::Project {
        hash: hash.clone(),
        path: s,
        name,
        description: None,
        tags: None,
        indexed_at: None,
        created_at: chrono::Utc::now().timestamp(),
    })
    .unwrap();
    hash
}

fn cm9_put_fact(db: &Database, id: &str, hash: &str, title: &str) {
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some(id.to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some(title.to_string()),
        body: Some(format!("{title} body")),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some(hash.to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();
}

// ── CB43: record which model actually ran ──────────────────────────────

/// CB43 fixtures: a graph + spec + agent node plus one `Running` node run
/// carrying `executed_platform`/`executed_model`. Returns the ids.
fn cb43_setup(db: &Database, pair: (Option<&str>, Option<&str>)) -> (String, String, String) {
    let lp = sample_graph("graph-cb43");
    db.insert_graph(&lp).unwrap();
    let spec = sample_graph_spec(&lp.id, "spec-cb43", 1);
    db.insert_graph_spec(&spec).unwrap();
    let node = sample_graph_node(&spec.id, "node-cb43", 1);
    db.insert_graph_node(&node).unwrap();
    let run = GraphNodeRun {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id,
        status: GraphRunStatus::Pass,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: Some(Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: pair.0.map(str::to_string),
        executed_model: pair.1.map(str::to_string),
    };
    db.insert_graph_run(&run).unwrap();
    (lp.id, spec.id, run.id)
}

fn cb43_node_run(
    graph_id: &str,
    spec_id: &str,
    node_id: &str,
    platform: Option<&str>,
    model: Option<&str>,
    status: GraphRunStatus,
    started_at: chrono::DateTime<Utc>,
) -> GraphNodeRun {
    GraphNodeRun {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: graph_id.to_string(),
        spec_id: spec_id.to_string(),
        node_id: node_id.to_string(),
        status,
        input: None,
        output: None,
        started_at,
        completed_at: Some(started_at),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: platform.map(str::to_string),
        executed_model: model.map(str::to_string),
    }
}

/// Splits `s` on commas that are not inside parentheses — used to pull
/// apart a `CREATE TABLE (...)` body into its column/constraint defs
/// without being confused by a `CHECK (...)` clause's own nested parens.
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Returns the index (into `s`) of the `)` that matches the `(` at
/// `open_pos`, scanning forward and tracking nesting depth.
fn find_matching_paren(s: &str, open_pos: usize) -> usize {
    let mut depth = 0i32;
    for (i, c) in s[open_pos..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return open_pos + i;
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced parens starting at byte {open_pos}");
}

mod graph_crew_tests;
#[cfg(test)]
mod hooks_tests;
mod part01_tests;
mod part02_tests;
mod part03_tests;
mod part04_tests;
mod part05_tests;
mod part06_tests;
mod part07_tests;
