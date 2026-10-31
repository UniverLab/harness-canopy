//! Part 05 of `tests`: pending-pause endpoint behaviour (pause no-op,
//! continue/run cancellation, read-surface keys).
use super::*;

/// A `Running` graph with one `Running` check run under it — the state a
/// wait-for-completion pause observes.
fn insert_running_graph_with_node(db: &Database, id: &str, workdir: &str) {
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        workdir: workdir.to_string(),
        status: GraphStatus::Running,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    })
    .unwrap();
    let mut spec = standalone_spec(&format!("{id}-spec"));
    spec.graph_id = Some(id.to_string());
    spec.status = GraphSpecStatus::Running;
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&GraphNode {
        id: format!("{id}-node"),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "Implementer".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "true", "success_condition": "exit_code_0" }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: format!("{id}-run"),
        graph_id: id.to_string(),
        spec_id: spec.id,
        node_id: format!("{id}-node"),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
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

/// [`insert_running_graph_with_node`] plus a pending pause — requested
/// straight through the DB compare-and-set, so no `pause_requested` event
/// exists unless the call under test writes one.
fn insert_pausing_graph(db: &Database, id: &str, workdir: &str) {
    insert_running_graph_with_node(db, id, workdir);
    assert!(db.request_pause_pending(id).unwrap());
}

/// A second pause while one is pending answers "pause already pending" and
/// writes no duplicate `pause_requested` event.
#[tokio::test]
async fn second_pause_while_pending_answers_already_pending() {
    use crate::daemon::params::GraphPauseParams;

    let (dir, db, handler) = queue_test_handler();
    let stored = dir.path().to_string_lossy().to_string();
    insert_pausing_graph(&db, "graph-pp", &stored);

    let second = handler
        .graph_pause_impl(GraphPauseParams {
            graph_id: "graph-pp".to_string(),
            interrupt: None,
        })
        .await
        .unwrap();
    assert!(!second.is_error.unwrap_or(false), "{:?}", second.content);
    assert!(
        queue_raw_text(&second).contains("pause already pending"),
        "the no-op names itself: {}",
        queue_raw_text(&second)
    );

    let pending = db
        .list_graph_events(
            "graph-pp",
            None,
            None,
            Some(&["pause_requested".to_string()]),
            50,
        )
        .unwrap();
    assert!(
        pending.is_empty(),
        "the no-op second pause writes no event, got {pending:?}"
    );
}

/// Continue while a pause is pending cancels it: back to `Running` with a
/// `pause_cancelled` event, the live run untouched, no new dispatch.
#[tokio::test]
async fn continue_while_pause_pending_cancels_and_keeps_running() {
    use crate::daemon::params::GraphContinueParams;

    let (dir, db, handler) = queue_test_handler();
    let stored = dir.path().to_string_lossy().to_string();
    insert_pausing_graph(&db, "graph-pc", &stored);

    let continued = handler
        .graph_continue_impl(GraphContinueParams {
            graph_id: "graph-pc".to_string(),
            mode: "retry_current_node".to_string(),
            node_id: None,
            spec_id: None,
            workdir: None,
        })
        .await
        .unwrap();
    assert!(
        !continued.is_error.unwrap_or(false),
        "{:?}",
        continued.content
    );
    assert!(
        queue_raw_text(&continued).contains("cancelled"),
        "the answer names the cancellation: {}",
        queue_raw_text(&continued)
    );
    assert_eq!(
        db.get_graph("graph-pc").unwrap().unwrap().status,
        GraphStatus::Running,
    );
    let running = db.list_running_graph_runs("graph-pc").unwrap();
    assert_eq!(running.len(), 1, "the live run is untouched");
    assert_eq!(running[0].id, "graph-pc-run");
    let cancelled = db
        .list_graph_events(
            "graph-pc",
            None,
            None,
            Some(&["pause_cancelled".to_string()]),
            50,
        )
        .unwrap();
    assert_eq!(
        cancelled.len(),
        1,
        "one pause_cancelled event, got {cancelled:?}"
    );
    assert_eq!(cancelled[0].message, "Pending pause cancelled.");
}

/// Run while a pause is pending cancels it the same way — and launches
/// nothing, since a dispatch is already live.
#[tokio::test]
async fn run_while_pause_pending_cancels_without_dispatch() {
    use crate::daemon::params::GraphRunParams;

    let (dir, db, handler) = queue_test_handler();
    let stored = dir.path().to_string_lossy().to_string();
    insert_pausing_graph(&db, "graph-pr", &stored);

    let launched = handler
        .graph_run_impl(GraphRunParams {
            graph_id: "graph-pr".to_string(),
            queue_id: None,
            workdir: None,
            idea: None,
            sandbox: None,
            start_node_id: None,
        })
        .await
        .unwrap();
    assert!(
        !launched.is_error.unwrap_or(false),
        "{:?}",
        launched.content
    );
    assert!(
        queue_raw_text(&launched).contains("cancelled"),
        "the answer names the cancellation: {}",
        queue_raw_text(&launched)
    );
    assert_eq!(
        db.get_graph("graph-pr").unwrap().unwrap().status,
        GraphStatus::Running,
    );
    assert_eq!(
        db.list_graph_runs_for_graph("graph-pr").unwrap().len(),
        1,
        "cancelling the pending pause launches no new run"
    );
    let cancelled = db
        .list_graph_events(
            "graph-pr",
            None,
            None,
            Some(&["pause_cancelled".to_string()]),
            50,
        )
        .unwrap();
    assert_eq!(
        cancelled.len(),
        1,
        "one pause_cancelled event, got {cancelled:?}"
    );
}

/// Both read surfaces report a pending pause as `running` plus
/// `pause_pending`/`pause_waiting_for` — and a plain running graph reports
/// the keys cleared.
#[test]
fn pending_pause_read_surfaces_report_running_with_waiting_names() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let stored = dir.path().to_string_lossy().to_string();
    insert_pausing_graph(&db, "graph-pj", &stored);
    insert_running_graph_with_node(&db, "graph-rj", &stored);

    let lp = db.get_graph("graph-pj").unwrap().unwrap();
    let summary = build_graph_summary_json(&db, &lp).unwrap();
    assert_eq!(summary["status"], serde_json::json!("running"));
    assert_eq!(summary["pause_pending"], serde_json::json!(true));
    assert_eq!(
        summary["pause_waiting_for"],
        serde_json::json!(["Implementer"])
    );

    let details = db.get_graph_details("graph-pj").unwrap().unwrap();
    let details_json = graph_details_json(&db, &details).unwrap();
    assert_eq!(details_json["status"], serde_json::json!("running"));
    assert_eq!(details_json["pause_pending"], serde_json::json!(true));
    assert_eq!(
        details_json["pause_waiting_for"],
        serde_json::json!(["Implementer"])
    );

    let lp = db.get_graph("graph-rj").unwrap().unwrap();
    let summary = build_graph_summary_json(&db, &lp).unwrap();
    assert_eq!(summary["status"], serde_json::json!("running"));
    assert_eq!(summary["pause_pending"], serde_json::json!(false));
    assert_eq!(summary["pause_waiting_for"], serde_json::json!([]));
}
