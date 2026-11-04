#[cfg(test)]
use super::*;
use crate::domain::graphs::{
    Graph, GraphCompletionHook, GraphCompletionHookRun, GraphHookEvent, GraphRunStatus, GraphStatus,
};
use std::collections::BTreeMap;

fn hook_fixture(platform: &str, prompt: &str) -> GraphCompletionHook {
    GraphCompletionHook {
        platform: Some(platform.to_string()),
        model: Some("test-model".to_string()),
        effort: None,
        prompt: Some(prompt.to_string()),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(5),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    }
}

fn graph_with_hooks(id: &str, hooks: BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>>) -> Graph {
    Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        require_spec_type: false,
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
        hooks,
    }
}

fn sample_hook_run(event: GraphHookEvent, hook_index: i64) -> GraphCompletionHookRun {
    GraphCompletionHookRun {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: "test-graph".to_string(),
        event,
        hook_index,
        status: GraphRunStatus::Running,
        output: None,
        summary: None,
        started_at: Utc::now(),
        completed_at: None,
        pid: None,
        boot_id: None,
        executed_platform: None,
        executed_model: None,
        hook_config: None,
    }
}

#[test]
fn insert_and_get_graph_with_hooks() {
    let db = test_db();
    let mut hooks = BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnCompleted,
        vec![hook_fixture("claude", "done: {{graph_name}}")],
    );
    hooks.insert(
        GraphHookEvent::OnFailed,
        vec![hook_fixture("mimo", "failed: {{blocker}}")],
    );
    let lp = graph_with_hooks("graph1", hooks);
    db.insert_graph(&lp).unwrap();

    let retrieved = db.get_graph("graph1").unwrap().unwrap();
    assert_eq!(retrieved.hooks.len(), 2);
    assert_eq!(
        retrieved
            .hooks
            .get(&GraphHookEvent::OnCompleted)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        retrieved
            .hooks
            .get(&GraphHookEvent::OnFailed)
            .unwrap()
            .first()
            .unwrap()
            .prompt,
        Some("failed: {{blocker}}".to_string())
    );
}

#[test]
fn empty_hooks_map_roundtrips() {
    let db = test_db();
    let lp = graph_with_hooks("graph1", BTreeMap::new());
    db.insert_graph(&lp).unwrap();

    let retrieved = db.get_graph("graph1").unwrap().unwrap();
    assert!(retrieved.hooks.is_empty());
}

#[test]
fn update_graph_hooks_replaces_map() {
    let db = test_db();
    let lp = graph_with_hooks("graph1", BTreeMap::new());
    db.insert_graph(&lp).unwrap();

    let mut new_hooks = BTreeMap::new();
    new_hooks.insert(
        GraphHookEvent::OnBlocked,
        vec![hook_fixture("opencode", "blocked: {{blocker}}")],
    );
    db.update_graph_hooks("graph1", &new_hooks).unwrap();

    let retrieved = db.get_graph("graph1").unwrap().unwrap();
    assert_eq!(retrieved.hooks.len(), 1);
    assert!(retrieved.hooks.contains_key(&GraphHookEvent::OnBlocked));
    assert!(!retrieved.hooks.contains_key(&GraphHookEvent::OnCompleted));
}

#[test]
fn update_graph_completion_hook_preserves_other_events() {
    let db = test_db();
    let mut hooks = BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnFailed,
        vec![hook_fixture("mimo", "failed")],
    );
    let lp = graph_with_hooks("graph1", hooks);
    db.insert_graph(&lp).unwrap();

    // Update only on_completed via the legacy path
    db.update_graph_completion_hook("graph1", Some(&hook_fixture("claude", "completed")))
        .unwrap();

    let retrieved = db.get_graph("graph1").unwrap().unwrap();
    // on_completed should be set
    assert!(retrieved.hooks.contains_key(&GraphHookEvent::OnCompleted));
    // on_failed should still be there
    assert!(retrieved.hooks.contains_key(&GraphHookEvent::OnFailed));
}

#[test]
fn hook_run_includes_event_and_index() {
    let db = test_db();
    let lp = graph_with_hooks("graph1", BTreeMap::new());
    db.insert_graph(&lp).unwrap();

    let mut run = sample_hook_run(GraphHookEvent::OnCompleted, 0);
    run.graph_id = "graph1".to_string();
    db.insert_graph_completion_hook_run(&run).unwrap();

    let runs = db.list_graph_completion_hook_runs("graph1").unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].event, GraphHookEvent::OnCompleted);
    assert_eq!(runs[0].hook_index, 0);
}

#[test]
fn graph_hook_run_config_round_trips() {
    let db = test_db();
    let lp = graph_with_hooks("graph1", BTreeMap::new());
    db.insert_graph(&lp).unwrap();
    let mut run = sample_hook_run(GraphHookEvent::OnCompleted, 0);
    run.graph_id = "graph1".to_string();
    run.hook_config = Some(hook_fixture("claude", "done: {{graph_name}}"));
    db.insert_graph_completion_hook_run(&run).unwrap();

    let runs = db.list_graph_completion_hook_runs("graph1").unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].hook_config, run.hook_config);
}

#[test]
fn multiple_hook_runs_ordered_by_started_at() {
    let db = test_db();
    let lp = graph_with_hooks("graph1", BTreeMap::new());
    db.insert_graph(&lp).unwrap();

    let mut run1 = sample_hook_run(GraphHookEvent::OnCompleted, 0);
    run1.graph_id = "graph1".to_string();
    run1.started_at = Utc::now() - chrono::Duration::seconds(10);
    db.insert_graph_completion_hook_run(&run1).unwrap();

    let mut run2 = sample_hook_run(GraphHookEvent::OnFailed, 0);
    run2.graph_id = "graph1".to_string();
    run2.started_at = Utc::now();
    db.insert_graph_completion_hook_run(&run2).unwrap();

    let runs = db.list_graph_completion_hook_runs("graph1").unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].event, GraphHookEvent::OnCompleted);
    assert_eq!(runs[1].event, GraphHookEvent::OnFailed);
}

#[test]
fn hook_run_update_records_result() {
    let db = test_db();
    let lp = graph_with_hooks("graph1", BTreeMap::new());
    db.insert_graph(&lp).unwrap();

    let mut run = sample_hook_run(GraphHookEvent::OnSpecCompleted, 2);
    run.graph_id = "graph1".to_string();
    db.insert_graph_completion_hook_run(&run).unwrap();

    let output = serde_json::json!({"result": "ok"});
    db.update_graph_completion_hook_run_result(
        &run.id,
        GraphRunStatus::Pass,
        Some(&output),
        Some("hook succeeded"),
        Some(Utc::now()),
    )
    .unwrap();

    let runs = db.list_graph_completion_hook_runs("graph1").unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
    assert_eq!(runs[0].event, GraphHookEvent::OnSpecCompleted);
    assert_eq!(runs[0].hook_index, 2);
    assert_eq!(runs[0].summary.as_deref(), Some("hook succeeded"));
}

#[test]
fn legacy_on_completed_column_falls_back_to_hooks_map() {
    let db = test_db();
    let lp = graph_with_hooks("graph1", BTreeMap::new());
    db.insert_graph(&lp).unwrap();
    // Simulate a pre-CH1 row: hooks NULL, legacy on_completed set.
    let legacy = serde_json::to_string(&hook_fixture("claude", "done: {{graph_name}}")).unwrap();
    {
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "UPDATE graphs SET hooks = NULL, on_completed = ?1 WHERE id = 'graph1'",
            rusqlite::params![legacy],
        )
        .unwrap();
    }

    let retrieved = db.get_graph("graph1").unwrap().unwrap();
    assert_eq!(retrieved.hooks.len(), 1);
    let completed = retrieved
        .hooks
        .get(&GraphHookEvent::OnCompleted)
        .expect("legacy hook must appear under on_completed");
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].platform.as_deref(), Some("claude"));
}

#[test]
fn multiple_hooks_preserve_declaration_order() {
    let db = test_db();
    let lp = graph_with_hooks("graph1", BTreeMap::new());
    db.insert_graph(&lp).unwrap();
    let mut hooks = BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnCompleted,
        vec![
            hook_fixture("claude", "first"),
            hook_fixture("mimo", "second"),
        ],
    );
    db.update_graph_hooks("graph1", &hooks).unwrap();

    let retrieved = db.get_graph("graph1").unwrap().unwrap();
    let completed = retrieved.hooks.get(&GraphHookEvent::OnCompleted).unwrap();
    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0].platform.as_deref(), Some("claude"));
    assert_eq!(completed[1].platform.as_deref(), Some("mimo"));
}
