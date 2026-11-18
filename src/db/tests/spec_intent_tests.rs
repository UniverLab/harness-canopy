//! Spec review: the `intent` column's migration and read/write path.

use super::*;

fn spec(id: &str, graph_id: Option<&str>, intent: Option<&str>) -> GraphSpec {
    GraphSpec {
        intent: intent.map(str::to_string),
        id: id.to_string(),
        graph_id: graph_id.map(str::to_string),
        name: format!("Spec {id}"),
        description: Some("description".to_string()),
        position: if graph_id.is_some() { 1 } else { 0 },
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
        review_state: crate::domain::graphs::SpecReviewState::Approved,
        review_tier: None,
        review_findings: None,
    }
}

/// A database created before `intent` existed keeps every row, and the
/// migration fills `intent` with NULL rather than inventing one.
#[test]
fn intent_migration_adds_the_column_and_keeps_existing_rows_null() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.db");
    {
        let db = Database::new(&path).unwrap();
        db.insert_graph_spec(&spec("legacy", None, None)).unwrap();
    }
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute("ALTER TABLE graph_specs DROP COLUMN intent", [])
            .expect("simulate the pre-intent schema");
        let has_intent: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_specs') WHERE name = 'intent'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_intent, 0);
    }

    let db = Database::new(&path).unwrap();
    let migrated = db.get_graph_spec("legacy").unwrap().unwrap();
    assert_eq!(migrated.name, "Spec legacy");
    assert_eq!(migrated.intent, None, "pre-existing rows migrate to NULL");

    // Idempotent on reopen, and the new column is writable.
    db.set_graph_spec_intent("legacy", Some("ship it")).unwrap();
    drop(db);
    let db = Database::new(&path).unwrap();
    assert_eq!(
        db.get_graph_spec("legacy")
            .unwrap()
            .unwrap()
            .intent
            .as_deref(),
        Some("ship it")
    );
}

/// `insert`, `get` and `list` all carry the intent — and a spec without one
/// reads back as `None`, not an empty string.
#[test]
fn intent_round_trips_through_insert_get_and_list() {
    let db = test_db();
    db.insert_graph_spec(&spec("with", None, Some("make the intent visible")))
        .unwrap();
    db.insert_graph_spec(&spec("without", None, None)).unwrap();

    assert_eq!(
        db.get_graph_spec("with")
            .unwrap()
            .unwrap()
            .intent
            .as_deref(),
        Some("make the intent visible")
    );
    assert_eq!(db.get_graph_spec("without").unwrap().unwrap().intent, None);

    let listed = db.list_specs(None, None, true, None).unwrap();
    let with = listed.iter().find(|row| row.id == "with").unwrap();
    let without = listed.iter().find(|row| row.id == "without").unwrap();
    assert_eq!(with.intent.as_deref(), Some("make the intent visible"));
    assert_eq!(without.intent, None);

    // Graph-bound specs read it too (a different SELECT list).
    db.insert_graph(&graph_fixture("g1")).unwrap();
    db.insert_graph_spec(&spec("bound", Some("g1"), Some("bound intent")))
        .unwrap();
    let bound = db.list_graph_specs("g1").unwrap();
    assert_eq!(
        bound[0].intent.as_deref(),
        Some("bound intent"),
        "list_graph_specs must select intent"
    );
}

/// An interrupted spec's workdir comes from the graph, not from `intent`:
/// `intent` is a new column before the trailing `g.workdir`, so a stale index
/// would resolve the workdir to the spec's intent text.
#[test]
fn interrupted_specs_resolve_the_graph_workdir_not_the_intent() {
    let db = test_db();
    db.insert_graph(&graph_fixture("g1")).unwrap();
    db.insert_graph_spec(&spec("bound", Some("g1"), Some("bound intent")))
        .unwrap();
    db.update_graph_spec_status("bound", GraphSpecStatus::Interrupted, None, None)
        .unwrap();

    let interrupted = db.list_interrupted_specs_missing_end_dirty().unwrap();
    let (spec, workdir) = interrupted
        .iter()
        .find(|(row, _)| row.id == "bound")
        .expect("interrupted spec is captured");
    assert_eq!(spec.intent.as_deref(), Some("bound intent"));
    assert_eq!(
        workdir.as_deref(),
        Some("/tmp"),
        "the resolved workdir must be the graph's, not the intent text"
    );
}

fn graph_fixture(id: &str) -> Graph {
    Graph {
        trigger_prompt: None,
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: id.to_string(),
        name: "Carrier".to_string(),
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
        require_spec_type: false,
    }
}
