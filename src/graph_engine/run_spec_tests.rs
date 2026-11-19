//! Directed-start activity message tests (extracted from run_spec.rs).

use super::*;

/// Two-node chain (`first` -> `second`) with a directed start at
/// `second`, so the recorded line names one skipped node and no reused
/// outputs.
fn directed_label_fixture(
    code: Option<&str>,
) -> (
    tempfile::TempDir,
    Database,
    Graph,
    GraphSpec,
    Vec<GraphNode>,
    Vec<GraphEdge>,
    DirectedStart,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let lp = Graph {
        trigger_prompt: None,
        id: "g1".to_string(),
        name: "Graph".to_string(),
        description: None,
        workdir: "/tmp/directed-label".to_string(),
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
        archived: false,
        paused_by_reconciliation: false,
        infra_node_id: None,
        allow_dirty_start: false,
        require_spec_type: false,
    };
    let spec = GraphSpec {
        intent: None,
        id: "s1".to_string(),
        graph_id: Some(lp.id.clone()),
        name: "Add retry".to_string(),
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
        spec_type: None,
        spec_scope: None,
        spec_code: code.map(str::to_string),
        review_state: crate::domain::graphs::SpecReviewState::Approved,
        review_tier: None,
        review_findings: None,
        created_by_session: None,
    };
    let nodes = vec![
        GraphNode {
            id: "n-a".to_string(),
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            name: "first".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({}),
            position: 1,
            created_at: chrono::Utc::now(),
        },
        GraphNode {
            id: "n-b".to_string(),
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            name: "second".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({}),
            position: 2,
            created_at: chrono::Utc::now(),
        },
    ];
    let edges = vec![GraphEdge {
        id: "e1".to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        from_node: "n-a".to_string(),
        to_node: "n-b".to_string(),
        condition: GraphEdgeCondition::Pass,
    }];
    let start = DirectedStart {
        spec_id: spec.id.clone(),
        node_arg: "n-b".to_string(),
    };
    (dir, db, lp, spec, nodes, edges, start)
}

fn directed_activity_message(code: Option<&str>) -> String {
    let (_dir, db, lp, spec, nodes, edges, start) = directed_label_fixture(code);
    let ensembles: Vec<EnsembleDetails> = Vec::new();
    let runs: Vec<GraphNodeRun> = Vec::new();
    record_directed_start(
        &db,
        &lp,
        "/tmp/directed-label",
        &spec,
        (&nodes, &edges, &ensembles),
        &runs,
        &start,
    );
    let activity = db
        .list_activity_log_entries("/tmp/directed-label", 10)
        .unwrap();
    assert_eq!(activity.len(), 1);
    activity.into_iter().next().unwrap().message
}

#[test]
fn directed_start_activity_shows_code_before_name() {
    let message = directed_activity_message(Some("FIX-7"));
    assert!(
        message.contains("[FIX-7] 'Add retry'"),
        "code before name: {message}"
    );
    assert!(
        !message.contains("''"),
        "the label carries its own quotes: {message}"
    );
}

#[test]
fn directed_start_activity_keeps_legacy_text_without_code() {
    assert_eq!(
        directed_activity_message(None),
        "Directed run: spec 'Add retry' starts at node 'second'; \
         skipped 1 node(s) [first]; reused outputs: none; \
         iteration counters restarted at 1."
    );
}
