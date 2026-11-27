//! One-time DB repair for `graph_specs.name` rows that still carry a
//! conventional header prefix.

use super::*;

fn spec_row(
    id: &str,
    name: &str,
    spec_type: Option<SpecType>,
    spec_scope: Option<&str>,
    spec_code: Option<&str>,
) -> GraphSpec {
    GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: name.to_string(),
        description: Some("description".to_string()),
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
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_type,
        spec_scope: spec_scope.map(str::to_string),
        spec_code: spec_code.map(str::to_string),
        intent: None,
        review_state: crate::domain::graphs::SpecReviewState::Approved,
        review_tier: None,
        review_findings: None,
        created_by_session: None,
    }
}

#[test]
fn migration_repairs_typed_prefixed_rows_and_is_idempotent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.db");
    {
        let db = Database::new(&path).unwrap();
        db.insert_graph_spec(&spec_row(
            "typed-prefixed",
            "fix(tui): fix(tui): open a session",
            Some(SpecType::Fix),
            Some("tui"),
            Some("FIX-109"),
        ))
        .unwrap();
        db.insert_graph_spec(&spec_row(
            "untyped-prefixed",
            "fix(tui): untyped verbatim",
            None,
            None,
            None,
        ))
        .unwrap();
        db.insert_graph_spec(&spec_row(
            "typed-clean",
            "already clean",
            Some(SpecType::Feat),
            Some("cli"),
            Some("FEAT-1"),
        ))
        .unwrap();

        let conn = db.conn.lock().unwrap();
        conn.execute("UPDATE graph_specs SET updated_at = 1000", [])
            .unwrap();
    }

    // Reopening runs `Database::init` -> `migrate_strip_repeated_spec_prefix`.
    let db = Database::new(&path).unwrap();
    let repaired = db.get_graph_spec("typed-prefixed").unwrap().unwrap();
    assert_eq!(repaired.name, "open a session");
    assert_eq!(repaired.spec_type, Some(SpecType::Fix));
    assert_eq!(repaired.spec_scope.as_deref(), Some("tui"));
    assert_eq!(repaired.spec_code.as_deref(), Some("FIX-109"));

    let untyped = db.get_graph_spec("untyped-prefixed").unwrap().unwrap();
    assert_eq!(untyped.name, "fix(tui): untyped verbatim");

    let clean = db.get_graph_spec("typed-clean").unwrap().unwrap();
    assert_eq!(clean.name, "already clean");

    let conn = db.conn.lock().unwrap();
    let repaired_updated_at: i64 = conn
        .query_row(
            "SELECT updated_at FROM graph_specs WHERE id = 'typed-prefixed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let untyped_updated_at: i64 = conn
        .query_row(
            "SELECT updated_at FROM graph_specs WHERE id = 'untyped-prefixed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(repaired_updated_at > 1000);
    assert_eq!(untyped_updated_at, 1000);

    // Second run changes nothing and reports 0.
    let second_count = Database::repair_repeated_spec_prefixes(&conn).unwrap();
    assert_eq!(second_count, 0);
}
