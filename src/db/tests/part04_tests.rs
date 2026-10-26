//! Part 04 of `db::tests` top-level tests (split from `src/db/tests.rs`, pure move).
use super::*;

#[test]
fn test_list_related_projects_finds_linked_projects() {
    let db = test_db();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("proj-a".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Project A".to_string()),
        body: Some("A".to_string()),
        body_replace: None,
        metadata: Some(Some(serde_json::json!({"hash": "hash-a"}))),
        project_hash: Some(Some("hash-a".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("proj-b".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Project B".to_string()),
        body: Some("B".to_string()),
        body_replace: None,
        metadata: Some(Some(serde_json::json!({"hash": "hash-b"}))),
        project_hash: Some(Some("hash-b".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("proj-c".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Project C".to_string()),
        body: Some("C".to_string()),
        body_replace: None,
        metadata: Some(Some(serde_json::json!({"hash": "hash-c"}))),
        project_hash: Some(Some("hash-c".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    db.link_projects("hash-a", "hash-b", "depends_on", Some(2.0))
        .unwrap();
    db.link_projects("hash-a", "hash-c", "relates_to", Some(1.0))
        .unwrap();

    let related = db.list_related_projects("hash-a", 10).unwrap();
    assert_eq!(related.len(), 2);

    let titles: Vec<_> = related.iter().map(|(n, _)| n.title.as_str()).collect();
    assert!(titles.contains(&"Project B"));
    assert!(titles.contains(&"Project C"));
}

#[test]
fn test_list_related_projects_returns_empty_for_unlinked_project() {
    let db = test_db();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("proj-lone".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Lone Project".to_string()),
        body: Some("No relations".to_string()),
        body_replace: None,
        metadata: Some(Some(serde_json::json!({"hash": "hash-lone"}))),
        project_hash: Some(Some("hash-lone".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    let related = db.list_related_projects("hash-lone", 10).unwrap();
    assert!(related.is_empty());
}

// ── Seed session binding tests ──────────────────────────────────────

#[test]
fn seed_bind_and_resolve() {
    let db = test_db();
    db.insert_interactive_session(
        "session-abc",
        "test-session",
        "opencode",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    db.bind_session_to_seed("session-abc", "seed-oak").unwrap();
    let resolved = db.resolve_session_seed("session-abc").unwrap();
    assert_eq!(resolved, Some("seed-oak".to_string()));
}

#[test]
fn seed_resolve_missing_returns_none() {
    let db = test_db();

    let resolved = db.resolve_session_seed("nonexistent-session").unwrap();
    assert!(resolved.is_none());
}

#[test]
fn seed_bind_replaces_existing() {
    let db = test_db();
    db.insert_interactive_session(
        "session-abc",
        "test-session",
        "opencode",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    db.bind_session_to_seed("session-abc", "seed-oak").unwrap();
    db.bind_session_to_seed("session-abc", "seed-pine").unwrap();

    let resolved = db.resolve_session_seed("session-abc").unwrap();
    assert_eq!(resolved, Some("seed-pine".to_string()));
}

#[test]
fn seed_unbind_removes_binding() {
    let db = test_db();
    db.insert_interactive_session(
        "session-abc",
        "test-session",
        "opencode",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    db.bind_session_to_seed("session-abc", "seed-oak").unwrap();
    db.unbind_session_seed("session-abc").unwrap();

    let resolved = db.resolve_session_seed("session-abc").unwrap();
    assert!(resolved.is_none());
}

#[test]
fn seed_unbind_nonexistent_is_ok() {
    let db = test_db();

    let result = db.unbind_session_seed("nonexistent-session");
    assert!(result.is_ok());
}

#[test]
fn seed_get_sessions_for_seed_empty() {
    let db = test_db();

    let sessions = db.get_sessions_for_seed("seed-oak").unwrap();
    assert!(sessions.is_empty());
}

#[test]
fn seed_multiple_sessions_for_same_seed() {
    let db = test_db();
    db.insert_interactive_session(
        "session-1",
        "s1",
        "opencode",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.insert_interactive_session(
        "session-2",
        "s2",
        "opencode",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.insert_interactive_session(
        "session-3",
        "s3",
        "opencode",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    db.bind_session_to_seed("session-1", "seed-oak").unwrap();
    db.bind_session_to_seed("session-2", "seed-oak").unwrap();
    db.bind_session_to_seed("session-3", "seed-pine").unwrap();

    // Verify bindings exist
    assert!(db.resolve_session_seed("session-1").unwrap().is_some());
    assert!(db.resolve_session_seed("session-2").unwrap().is_some());
    assert!(db.resolve_session_seed("session-3").unwrap().is_some());
}

#[test]
fn interactive_session_pid_round_trips_through_get_active_sessions() {
    let db = test_db();
    db.insert_interactive_session(
        "session-with-pid",
        "with-pid",
        "opencode",
        "/tmp",
        None,
        Some(4321),
        "interactive",
        None,
    )
    .unwrap();
    db.insert_interactive_session(
        "session-without-pid",
        "without-pid",
        "opencode",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    let sessions = db.get_active_sessions().unwrap();

    let with_pid = sessions
        .iter()
        .find(|s| s.id == "session-with-pid")
        .expect("session-with-pid present");
    assert_eq!(with_pid.pid, Some(4321));

    let without_pid = sessions
        .iter()
        .find(|s| s.id == "session-without-pid")
        .expect("session-without-pid present");
    assert_eq!(without_pid.pid, None);
}

#[test]
fn get_active_sessions_excludes_bridge_sessions() {
    let db = test_db();
    db.insert_interactive_session(
        "bridge-session",
        "standalone",
        "bridge",
        "/tmp",
        Some("canopy bridge"),
        Some(4321),
        "bridge",
        None,
    )
    .unwrap();
    db.insert_interactive_session(
        "chat-session",
        "chat",
        "opencode",
        "/tmp",
        None,
        Some(1234),
        "interactive",
        None,
    )
    .unwrap();

    let sessions = db.get_active_sessions().unwrap();

    assert!(sessions.iter().all(|s| s.id != "bridge-session"));
    assert!(sessions.iter().any(|s| s.id == "chat-session"));
}

#[test]
fn get_active_sessions_by_type_returns_only_matching_bridge_rows() {
    let db = test_db();
    db.insert_interactive_session(
        "bridge-session",
        "standalone",
        "bridge",
        "/tmp",
        Some("canopy bridge"),
        Some(4321),
        "bridge",
        None,
    )
    .unwrap();
    db.insert_interactive_session(
        "chat-session",
        "chat",
        "opencode",
        "/tmp",
        None,
        Some(1234),
        "interactive",
        None,
    )
    .unwrap();

    let bridges = db.get_active_sessions_by_type("bridge").unwrap();

    assert_eq!(bridges.len(), 1);
    assert_eq!(bridges[0].id, "bridge-session");
}

#[test]
fn session_marking_is_per_row_not_a_mass_pre_pass() {
    // Simulates a crash partway through the auto-resume graph: two active
    // sessions exist, but only the first gets handled (marked orphaned)
    // before the "crash". The old `mark_orphaned_sessions` mass pre-pass
    // would have flipped both to 'orphaned' up front; the per-session
    // primitives must leave the untouched second session 'active'.
    let db = test_db();
    db.insert_interactive_session(
        "session-1",
        "first",
        "opencode",
        "/tmp",
        None,
        Some(111),
        "interactive",
        None,
    )
    .unwrap();
    db.insert_interactive_session(
        "session-2",
        "second",
        "opencode",
        "/tmp",
        None,
        Some(222),
        "interactive",
        None,
    )
    .unwrap();

    // Only the first session is handled before the simulated crash.
    db.mark_session_orphaned("session-1").unwrap();

    let active = db.get_active_sessions().unwrap();
    assert!(active.iter().all(|s| s.id != "session-1"));
    assert!(
        active.iter().any(|s| s.id == "session-2"),
        "untouched second session must still be 'active', not orphaned"
    );

    let orphaned = db.get_orphaned_sessions().unwrap();
    assert!(orphaned.iter().any(|s| s.id == "session-1"));
    assert!(orphaned.iter().all(|s| s.id != "session-2"));
}

#[test]
fn mark_session_orphaned_is_a_noop_once_already_resumed() {
    // Guards the "only transitions rows that are still 'active'" contract:
    // once a session has been marked 'resumed' it must not be flippable
    // back to 'orphaned' by a stray call.
    let db = test_db();
    db.insert_interactive_session(
        "session-1",
        "first",
        "opencode",
        "/tmp",
        None,
        Some(111),
        "interactive",
        None,
    )
    .unwrap();

    db.mark_session_resumed("session-1").unwrap();
    db.mark_session_orphaned("session-1").unwrap();

    let orphaned = db.get_orphaned_sessions().unwrap();
    assert!(orphaned.iter().all(|s| s.id != "session-1"));
}

#[test]
fn boot_id_migration_is_idempotent_and_a_pre_boot_id_database_opens_cleanly() {
    // Simulate a database written before the boot_id column existed:
    // interactive_sessions has `pid` but no `boot_id`.
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    {
        let conn = rusqlite::Connection::open(&path).expect("open raw legacy db");
        conn.execute_batch(
            "CREATE TABLE interactive_sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                cli TEXT NOT NULL,
                working_dir TEXT NOT NULL,
                args TEXT,
                started_at TEXT NOT NULL,
                exited_at TEXT,
                exit_code INTEGER,
                status TEXT NOT NULL DEFAULT 'active',
                session_type TEXT NOT NULL DEFAULT 'interactive',
                pid INTEGER
             );
             INSERT INTO interactive_sessions
                 (id, name, cli, working_dir, started_at, status, session_type, pid)
                 VALUES ('legacy-session', 'legacy', 'opencode', '/tmp', '2023-01-01T00:00:00Z', 'active', 'interactive', 4242);",
        )
        .expect("seed legacy schema");
    }

    // Opening the DB (Database::new runs the migration) must succeed, add
    // the boot_id column, and leave the existing row queryable with a NULL
    // boot_id (legacy rows are always safe to resume — see should_resume_session).
    let db = Database::new(&path).expect("open pre-boot_id db, running migration");
    let sessions = db.get_active_sessions().unwrap();
    let legacy = sessions
        .iter()
        .find(|s| s.id == "legacy-session")
        .expect("legacy row still present after migration");
    assert_eq!(legacy.boot_id, None);
    assert_eq!(legacy.pid, Some(4242));
    drop(db);

    // Reopening after the migration already ran must be a no-op: same data,
    // no error (idempotent).
    let db = Database::new(&path).expect("reopen db after migration already applied");
    let sessions = db.get_active_sessions().unwrap();
    assert!(sessions.iter().any(|s| s.id == "legacy-session"));
}

#[test]
fn legacy_bridge_rows_are_reclassified_by_migration() {
    // Simulate a row left over from before session_type = 'bridge' existed:
    // old builds stored the bridge sidecar with session_type = 'interactive'.
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Database::new(&path).expect("create test db");
    db.insert_interactive_session(
        "legacy-bridge",
        "standalone",
        "bridge",
        "/tmp",
        Some("canopy bridge"),
        Some(4321),
        "interactive",
        None,
    )
    .unwrap();
    drop(db);

    // Re-running the migration (as happens on every Database::new) must
    // reclassify the legacy row, and running it again must be a no-op.
    drop(Database::new(&path).expect("reopen test db"));
    let db = Database::new(&path).expect("reopen test db again after no-op migration");

    assert_eq!(
        db.get_session_type("legacy-bridge").unwrap().as_deref(),
        Some("bridge")
    );
    assert!(db
        .get_active_sessions()
        .unwrap()
        .iter()
        .all(|s| s.id != "legacy-bridge"));
}

#[test]
fn registering_project_creates_intelligence_root_node() {
    let db = test_db();
    let dir_a = tempdir().expect("tempdir a");
    let dir_b = tempdir().expect("tempdir b");

    let a = db.register_project_path(dir_a.path()).expect("register a");
    let b = db.register_project_path(dir_b.path()).expect("register b");

    let projects = db
        .list_intelligence_projects(None, 10)
        .expect("list project nodes");
    assert_eq!(
        projects.len(),
        2,
        "each registered project gets a root node"
    );

    let edge = db
        .link_projects(&a.hash, &b.hash, "relates_to", None)
        .expect("link via hashes resolves the auto-created nodes");
    assert_eq!(edge.relation, "relates_to");

    let graph = db
        .walk_intelligence_graph(&format!("project:{}", a.hash), 2)
        .expect("walk")
        .expect("root exists");
    assert!(
        graph
            .nodes
            .iter()
            .any(|n| n.id == format!("project:{}", b.hash)),
        "linked project is reachable from the root"
    );
}

#[test]
fn backfill_recreates_missing_project_nodes() {
    let db = test_db();
    let dir = tempdir().expect("tempdir");
    let project = db.register_project_path(dir.path()).expect("register");

    let node_id = format!("project:{}", project.hash);
    db.delete_intelligence_node(&node_id)
        .expect("simulate legacy db without project nodes");
    assert!(db
        .list_intelligence_projects(None, 10)
        .expect("list")
        .is_empty());

    let created = db.backfill_project_nodes().expect("backfill");
    assert_eq!(created, 1);
    assert_eq!(
        db.list_intelligence_projects(None, 10).expect("list").len(),
        1
    );
}

// ── B25: Administrative spec completion ──────────────────────────────

#[test]
fn set_spec_admin_status_transitions_each_status() {
    let db = test_db();

    for target_status in &[
        GraphSpecStatus::Completed,
        GraphSpecStatus::Skipped,
        GraphSpecStatus::Pending,
    ] {
        let mut spec = sample_graph_spec("unused", &format!("spec-{:?}", target_status), 1);
        spec.graph_id = None;
        db.insert_graph_spec(&spec).unwrap();

        let outcome = db
            .set_spec_admin_status(&spec.id, *target_status, "test reason")
            .unwrap();

        assert!(
            matches!(outcome, SpecAdminStatusOutcome::Success),
            "transition to {:?} failed",
            target_status
        );

        let spec_after = db.get_graph_spec(&spec.id).unwrap().unwrap();
        assert_eq!(spec_after.status, *target_status);
        assert_eq!(
            spec_after.completed_via,
            Some("admin".to_string()),
            "completed_via should be 'admin' for {:?}",
            target_status
        );
        assert_eq!(
            spec_after.completed_via_reason,
            Some("test reason".to_string()),
            "completed_via_reason should match for {:?}",
            target_status
        );

        if *target_status != GraphSpecStatus::Pending {
            assert!(
                spec_after.completed_via_at.is_some(),
                "completed_via_at should be set for {:?}",
                target_status
            );
        } else {
            assert!(
                spec_after.completed_via_at.is_none(),
                "completed_via_at should be None for Pending"
            );
        }
    }
}

#[test]
fn set_spec_admin_status_rejects_missing_spec() {
    let db = test_db();
    let outcome = db
        .set_spec_admin_status("nonexistent", GraphSpecStatus::Completed, "reason")
        .unwrap();
    assert!(
        matches!(outcome, SpecAdminStatusOutcome::NotFound),
        "should reject missing spec"
    );
}

#[test]
fn set_spec_admin_status_rejects_graph_bound_spec() {
    let db = test_db();
    let lp = sample_graph("graph-bound-test");
    db.insert_graph(&lp).unwrap();

    let spec = sample_graph_spec(&lp.id, "spec-bound", 1);
    db.insert_graph_spec(&spec).unwrap();

    let outcome = db
        .set_spec_admin_status(&spec.id, GraphSpecStatus::Completed, "reason")
        .unwrap();

    assert!(
        matches!(outcome, SpecAdminStatusOutcome::NotStandalone(ref id) if id == &lp.id),
        "should reject graph-bound spec"
    );
}

#[test]
fn unbind_graph_spec_clears_graph_id() {
    let db = test_db();
    let lp = sample_graph("graph-unbind");
    db.insert_graph(&lp).unwrap();
    let spec = sample_graph_spec(&lp.id, "spec-unbind", 1);
    let name = spec.name.clone();
    let description = spec.description.clone();
    db.insert_graph_spec(&spec).unwrap();

    assert!(db.unbind_graph_spec(&spec.id).unwrap());

    let after = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert!(after.graph_id.is_none());
    assert_eq!(after.name, name);
    assert_eq!(after.description, description);
}

#[test]
fn unbind_graph_spec_noop_for_standalone() {
    let db = test_db();
    let mut spec = sample_graph_spec("unused", "spec-standalone", 1);
    spec.graph_id = None;
    db.insert_graph_spec(&spec).unwrap();

    assert!(!db.unbind_graph_spec(&spec.id).unwrap());
}

#[test]
fn unbind_graph_spec_preserves_execution_history() {
    let db = test_db();
    let lp = sample_graph("graph-unbind-hist");
    db.insert_graph(&lp).unwrap();
    let spec = sample_graph_spec(&lp.id, "spec-unbind-hist", 1);
    db.insert_graph_spec(&spec).unwrap();
    let node = sample_graph_node(&spec.id, "node-unbind-hist", 1);
    db.insert_graph_node(&node).unwrap();
    let run = GraphNodeRun {
        id: "run-unbind-hist".to_string(),
        graph_id: lp.id,
        spec_id: spec.id.clone(),
        node_id: node.id,
        status: GraphRunStatus::Pass,
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
    };
    db.insert_graph_run(&run).unwrap();

    assert!(db.unbind_graph_spec(&spec.id).unwrap());

    let runs = db.list_graph_runs_for_spec(&spec.id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, "run-unbind-hist");
}

#[test]
fn set_spec_admin_status_rejects_active_run() {
    let db = test_db();
    let mut spec = sample_graph_spec("unused", "spec-with-run", 1);
    spec.graph_id = None;
    db.insert_graph_spec(&spec).unwrap();

    let lp = sample_graph("graph-for-run");
    db.insert_graph(&lp).unwrap();

    let node = sample_graph_node(&spec.id, "node-for-run", 1);
    db.insert_graph_node(&node).unwrap();

    let run = GraphNodeRun {
        id: "run-active".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id,
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
    };
    db.insert_graph_run(&run).unwrap();

    let outcome = db
        .set_spec_admin_status(&spec.id, GraphSpecStatus::Completed, "reason")
        .unwrap();

    assert!(
        matches!(outcome, SpecAdminStatusOutcome::ActiveRun { ref graph_id, ref run_id }
            if graph_id == &lp.id && run_id == "run-active"),
        "should reject spec with active run"
    );
}

#[test]
fn set_spec_admin_status_propagates_to_queue_selection() {
    let db = test_db();
    let mut spec = sample_graph_spec("unused", "spec-queue-prop", 1);
    spec.graph_id = None;
    db.insert_graph_spec(&spec).unwrap();

    let queue = Queue {
        id: "queue-test".to_string(),
        name: "queue-test".to_string(),
        created_at: Utc::now(),
    };
    db.insert_queue(&queue).unwrap();
    db.append_queue_member("queue-test", &spec.id, None)
        .unwrap();

    let before = db.queue_next_pending_spec_id("queue-test").unwrap();
    assert_eq!(before.as_deref(), Some(spec.id.as_str()));

    let outcome = db
        .set_spec_admin_status(&spec.id, GraphSpecStatus::Completed, "reason")
        .unwrap();
    assert!(matches!(outcome, SpecAdminStatusOutcome::Success));

    let after = db.queue_next_pending_spec_id("queue-test").unwrap();
    assert_eq!(
        after, None,
        "queue should not select completed spec anymore"
    );
}

/// `Interrupted` must be exactly as selectable as `Pending` — a spec cut
/// short by an external event (daemon restart, crash) is not less runnable
/// than a spec that never started, and queue order between the two statuses
/// is preserved (position, not status, decides who goes first).
#[test]
fn queue_next_pending_spec_id_picks_interrupted_spec_in_position_order() {
    let db = test_db();
    let queue = Queue {
        id: "queue-interrupted".to_string(),
        name: "queue-interrupted".to_string(),
        created_at: Utc::now(),
    };
    db.insert_queue(&queue).unwrap();

    let mut ahead = sample_graph_spec("unused", "spec-ahead-completed", 1);
    ahead.graph_id = None;
    ahead.status = GraphSpecStatus::Completed;
    db.insert_graph_spec(&ahead).unwrap();
    db.append_queue_member("queue-interrupted", &ahead.id, None)
        .unwrap();

    let mut interrupted = sample_graph_spec("unused", "spec-interrupted", 2);
    interrupted.graph_id = None;
    interrupted.status = GraphSpecStatus::Interrupted;
    db.insert_graph_spec(&interrupted).unwrap();
    db.append_queue_member("queue-interrupted", &interrupted.id, None)
        .unwrap();

    let mut pending = sample_graph_spec("unused", "spec-pending-after", 3);
    pending.graph_id = None;
    pending.status = GraphSpecStatus::Pending;
    db.insert_graph_spec(&pending).unwrap();
    db.append_queue_member("queue-interrupted", &pending.id, None)
        .unwrap();

    // Position 2 (`interrupted`) is picked before position 3 (`pending`) —
    // selection follows queue order, not a preference between the two
    // equally-runnable statuses.
    assert_eq!(
        db.queue_next_pending_spec_id("queue-interrupted")
            .unwrap()
            .as_deref(),
        Some(interrupted.id.as_str())
    );
}

#[test]
fn queue_running_spec_id_returns_first_running_member() {
    let db = test_db();
    let lp = sample_graph("wf-queue-running");
    db.insert_graph(&lp).unwrap();

    let mut spec_a = sample_graph_spec("unused", "spec-a", 1);
    spec_a.graph_id = None;
    spec_a.status = GraphSpecStatus::Running;
    let mut spec_b = sample_graph_spec("unused", "spec-b", 2);
    spec_b.graph_id = None;
    spec_b.status = GraphSpecStatus::Pending;
    db.insert_graph_spec(&spec_a).unwrap();
    db.insert_graph_spec(&spec_b).unwrap();

    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: Utc::now(),
    })
    .unwrap();
    db.append_queue_member("queue-1", &spec_a.id, None).unwrap();
    db.append_queue_member("queue-1", &spec_b.id, None).unwrap();

    assert_eq!(
        db.queue_running_spec_id("queue-1").unwrap().as_deref(),
        Some(spec_a.id.as_str())
    );
}

#[test]
fn queue_running_spec_id_returns_none_when_no_running_member() {
    let db = test_db();
    let lp = sample_graph("wf-queue-no-running");
    db.insert_graph(&lp).unwrap();

    let mut spec = sample_graph_spec("unused", "spec-p", 1);
    spec.graph_id = None;
    spec.status = GraphSpecStatus::Pending;
    db.insert_graph_spec(&spec).unwrap();

    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: Utc::now(),
    })
    .unwrap();
    db.append_queue_member("queue-1", &spec.id, None).unwrap();

    assert_eq!(db.queue_running_spec_id("queue-1").unwrap(), None);
}

#[test]
fn reconcile_stranded_queue_specs_resets_running_spec_with_no_active_run() {
    let db = test_db();
    let mut lp = sample_graph("wf-stranded");
    lp.status = GraphStatus::Paused;
    lp.active_run_queue_id = Some("queue-1".to_string());
    db.insert_graph(&lp).unwrap();

    let mut spec = sample_graph_spec("unused", "spec-stranded", 1);
    spec.graph_id = None;
    spec.status = GraphSpecStatus::Running;
    db.insert_graph_spec(&spec).unwrap();

    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: Utc::now(),
    })
    .unwrap();
    db.append_queue_member("queue-1", &spec.id, None).unwrap();

    assert_eq!(db.reconcile_stranded_queue_specs().unwrap(), 1);

    let spec_after = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(spec_after.status, GraphSpecStatus::Pending);
    assert!(spec_after.started_at.is_none());
    assert!(spec_after.spec_start_head.is_none());
}

#[test]
fn reconcile_stranded_queue_specs_preserves_spec_with_active_run_in_current_boot() {
    let db = test_db();
    let mut lp = sample_graph("wf-stranded-live");
    lp.status = GraphStatus::Paused;
    lp.active_run_queue_id = Some("queue-1".to_string());
    db.insert_graph(&lp).unwrap();

    let mut spec = sample_graph_spec("unused", "spec-live", 1);
    spec.graph_id = None;
    spec.status = GraphSpecStatus::Running;
    db.insert_graph_spec(&spec).unwrap();

    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: Utc::now(),
    })
    .unwrap();
    db.append_queue_member("queue-1", &spec.id, None).unwrap();

    let node = sample_graph_node(&spec.id, "node-live", 1);
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: "run-live".to_string(),
        graph_id: lp.id,
        spec_id: spec.id.clone(),
        node_id: node.id,
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: Some(crate::system::boot_id().unwrap_or_default()),
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();

    assert_eq!(
        db.reconcile_stranded_queue_specs().unwrap(),
        0,
        "spec with active run in current boot must not be reset"
    );

    let spec_after = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(spec_after.status, GraphSpecStatus::Running);
}

#[test]
fn reconcile_stranded_queue_specs_is_idempotent() {
    let db = test_db();
    let mut lp = sample_graph("wf-stranded-idem");
    lp.status = GraphStatus::Paused;
    lp.active_run_queue_id = Some("queue-1".to_string());
    db.insert_graph(&lp).unwrap();

    let mut spec = sample_graph_spec("unused", "spec-idem", 1);
    spec.graph_id = None;
    spec.status = GraphSpecStatus::Running;
    db.insert_graph_spec(&spec).unwrap();

    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: Utc::now(),
    })
    .unwrap();
    db.append_queue_member("queue-1", &spec.id, None).unwrap();

    assert_eq!(db.reconcile_stranded_queue_specs().unwrap(), 1);
    assert_eq!(
        db.reconcile_stranded_queue_specs().unwrap(),
        0,
        "second pass must find nothing to reset"
    );
}

#[test]
fn reconcile_orphaned_graphs_marks_interrupted_and_leaves_dirty_worktree_untouched() {
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let head = git_head(dir.path());
    // Stand in for a truncated write left behind by the killed process.
    std::fs::write(dir.path().join("truncated.rs"), "fn broken(").unwrap();
    assert!(!git_is_clean(dir.path()));

    let db = test_db();
    let data_dir = tempdir().unwrap();
    let mut lp = sample_graph("wf-orphan-interrupted-dirty");
    lp.status = GraphStatus::Running;
    lp.workdir = dir.path().to_string_lossy().to_string();
    let mut spec = sample_graph_spec(&lp.id, "spec-orphan-interrupted-dirty", 1);
    spec.status = GraphSpecStatus::Running;
    spec.spec_start_head = Some(head);
    let node = sample_graph_node(&spec.id, "node-orphan-interrupted-dirty", 1);
    let run = GraphNodeRun {
        id: "run-orphan-interrupted-dirty".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
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
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&run).unwrap();

    assert_eq!(
        db.reconcile_orphaned_graphs(data_dir.path()).unwrap().len(),
        1
    );

    assert!(
        !git_is_clean(dir.path()),
        "the engine must never touch git — uncommitted changes stay exactly as the interrupted run left them"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("truncated.rs")).unwrap(),
        "fn broken(",
        "the partial write itself must be untouched, not just 'still dirty'"
    );

    let run_after = db.get_graph_run(&run.id).unwrap().unwrap();
    let output = run_after.output.as_ref().expect("output recorded");
    assert_eq!(output.get("interrupted"), Some(&serde_json::json!(true)));
    assert!(
        output.get("quarantine").is_none(),
        "there is no quarantine mechanism left to report"
    );

    let spec_after = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(spec_after.status, GraphSpecStatus::Interrupted);
}

#[test]
fn reconcile_orphaned_graphs_marks_interrupted_for_clean_worktree_too() {
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let head = git_head(dir.path());
    assert!(git_is_clean(dir.path()));

    let db = test_db();
    let data_dir = tempdir().unwrap();
    let mut lp = sample_graph("wf-orphan-interrupted-clean");
    lp.status = GraphStatus::Running;
    lp.workdir = dir.path().to_string_lossy().to_string();
    let mut spec = sample_graph_spec(&lp.id, "spec-orphan-interrupted-clean", 1);
    spec.status = GraphSpecStatus::Running;
    spec.spec_start_head = Some(head);
    let node = sample_graph_node(&spec.id, "node-orphan-interrupted-clean", 1);
    let run = GraphNodeRun {
        id: "run-orphan-interrupted-clean".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
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
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&run).unwrap();

    assert_eq!(
        db.reconcile_orphaned_graphs(data_dir.path()).unwrap().len(),
        1
    );

    assert!(git_is_clean(dir.path()), "still nothing to touch");

    let run_after = db.get_graph_run(&run.id).unwrap().unwrap();
    let output = run_after.output.as_ref().expect("output recorded");
    assert_eq!(output.get("interrupted"), Some(&serde_json::json!(true)));
    assert!(output.get("quarantine").is_none());

    let spec_after = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(
        spec_after.status,
        GraphSpecStatus::Interrupted,
        "a spec becomes interrupted regardless of whether the worktree happened to be dirty"
    );
}

/// The engine must not require git at all: a scratch workdir that was never
/// `git init`ed still gets the spec marked `Interrupted`, with whatever
/// partial work it holds left completely alone.
#[test]
fn reconcile_orphaned_graphs_marks_interrupted_in_non_git_workdir() {
    let dir = tempdir().unwrap();
    // Deliberately no `init_git_repo` — this workdir is not a repository.
    std::fs::write(dir.path().join("truncated.rs"), "fn broken(").unwrap();

    let db = test_db();
    let data_dir = tempdir().unwrap();
    let mut lp = sample_graph("wf-orphan-interrupted-no-git");
    lp.status = GraphStatus::Running;
    lp.workdir = dir.path().to_string_lossy().to_string();
    let mut spec = sample_graph_spec(&lp.id, "spec-orphan-interrupted-no-git", 1);
    spec.status = GraphSpecStatus::Running;
    spec.spec_start_head = None;
    let node = sample_graph_node(&spec.id, "node-orphan-interrupted-no-git", 1);
    let run = GraphNodeRun {
        id: "run-orphan-interrupted-no-git".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
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
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&run).unwrap();

    assert_eq!(
        db.reconcile_orphaned_graphs(data_dir.path()).unwrap().len(),
        1
    );

    assert_eq!(
        std::fs::read_to_string(dir.path().join("truncated.rs")).unwrap(),
        "fn broken(",
        "partial work in a non-git workdir must be left exactly as found"
    );

    let run_after = db.get_graph_run(&run.id).unwrap().unwrap();
    let output = run_after.output.as_ref().expect("output recorded");
    assert_eq!(output.get("interrupted"), Some(&serde_json::json!(true)));
    assert!(output.get("quarantine").is_none());

    let spec_after = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(spec_after.status, GraphSpecStatus::Interrupted);
}

#[test]
fn reconcile_orphaned_graphs_interrupted_marking_is_idempotent_across_two_passes() {
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let head = git_head(dir.path());
    std::fs::write(dir.path().join("truncated.rs"), "fn broken(").unwrap();

    let db = test_db();
    let data_dir = tempdir().unwrap();
    let mut lp = sample_graph("wf-orphan-interrupted-idempotent");
    lp.status = GraphStatus::Running;
    lp.workdir = dir.path().to_string_lossy().to_string();
    let mut spec = sample_graph_spec(&lp.id, "spec-orphan-interrupted-idempotent", 1);
    spec.status = GraphSpecStatus::Running;
    spec.spec_start_head = Some(head);
    let node = sample_graph_node(&spec.id, "node-orphan-interrupted-idempotent", 1);
    let run = GraphNodeRun {
        id: "run-orphan-interrupted-idempotent".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
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
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&run).unwrap();

    assert_eq!(
        db.reconcile_orphaned_graphs(data_dir.path()).unwrap().len(),
        1
    );
    let spec_after_first = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(spec_after_first.status, GraphSpecStatus::Interrupted);

    // Second pass: the graph is already `Paused`, so it's not even a
    // candidate — nothing should change again, and the worktree stays
    // exactly as it was after the first pass.
    assert_eq!(
        db.reconcile_orphaned_graphs(data_dir.path()).unwrap().len(),
        0
    );
    let spec_after_second = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(spec_after_second.status, GraphSpecStatus::Interrupted);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("truncated.rs")).unwrap(),
        "fn broken(",
        "a second pass must not touch the worktree"
    );
}

#[test]
fn reconcile_stranded_queue_specs_marks_spec_interrupted_and_leaves_worktree_untouched() {
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let head = git_head(dir.path());
    std::fs::write(dir.path().join("truncated.rs"), "fn broken(").unwrap();

    let db = test_db();
    let mut lp = sample_graph("wf-stranded-interrupted");
    lp.status = GraphStatus::Paused;
    lp.workdir = dir.path().to_string_lossy().to_string();
    lp.active_run_queue_id = Some("queue-1".to_string());
    db.insert_graph(&lp).unwrap();

    let mut spec = sample_graph_spec("unused", "spec-stranded-interrupted", 1);
    spec.graph_id = None;
    spec.status = GraphSpecStatus::Running;
    spec.spec_start_head = Some(head);
    db.insert_graph_spec(&spec).unwrap();

    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: Utc::now(),
    })
    .unwrap();
    db.append_queue_member("queue-1", &spec.id, None).unwrap();

    let node = sample_graph_node(&spec.id, "node-stranded-interrupted", 1);
    db.insert_graph_node(&node).unwrap();
    // A `graph_runs` row left `running` by a daemon that died before a
    // graceful path (e.g. `graph_report_blocker`) could finalize it — its
    // boot id is stale, proving it from a recorded fact rather than content.
    db.insert_graph_run(&GraphNodeRun {
        id: "run-stranded-interrupted".to_string(),
        graph_id: lp.id,
        spec_id: spec.id.clone(),
        node_id: node.id,
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: Some("some-other-boot-that-is-not-current".to_string()),
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();

    assert_eq!(db.reconcile_stranded_queue_specs().unwrap(), 1);

    assert!(
        !git_is_clean(dir.path()),
        "the engine must never touch git here either"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("truncated.rs")).unwrap(),
        "fn broken("
    );

    let run_after = db
        .get_graph_run("run-stranded-interrupted")
        .unwrap()
        .unwrap();
    assert_ne!(run_after.status, GraphRunStatus::Running);
    let output = run_after.output.as_ref().expect("output recorded");
    assert_eq!(output.get("interrupted"), Some(&serde_json::json!(true)));
    assert!(output.get("quarantine").is_none());

    // The key behaviour change (B36 → this spec): a genuinely interrupted
    // spec is marked `Interrupted`, not silently reset to `Pending` — it
    // used to be indistinguishable from a spec that never started.
    let spec_after = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(spec_after.status, GraphSpecStatus::Interrupted);
}

#[test]
fn reconcile_stranded_queue_specs_leaves_worktree_untouched_for_healthy_run() {
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let head = git_head(dir.path());
    std::fs::write(dir.path().join("in-progress.rs"), "fn still_writing(").unwrap();

    let db = test_db();
    let mut lp = sample_graph("wf-stranded-healthy");
    lp.status = GraphStatus::Paused;
    lp.workdir = dir.path().to_string_lossy().to_string();
    lp.active_run_queue_id = Some("queue-1".to_string());
    db.insert_graph(&lp).unwrap();

    let mut spec = sample_graph_spec("unused", "spec-stranded-healthy", 1);
    spec.graph_id = None;
    spec.status = GraphSpecStatus::Running;
    spec.spec_start_head = Some(head);
    db.insert_graph_spec(&spec).unwrap();

    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: Utc::now(),
    })
    .unwrap();
    db.append_queue_member("queue-1", &spec.id, None).unwrap();

    let node = sample_graph_node(&spec.id, "node-stranded-healthy", 1);
    db.insert_graph_node(&node).unwrap();
    // Genuinely still in flight: carries the *current* boot id, so it must
    // never be treated as interrupted, and its worktree must not be touched.
    db.insert_graph_run(&GraphNodeRun {
        id: "run-stranded-healthy".to_string(),
        graph_id: lp.id,
        spec_id: spec.id,
        node_id: node.id,
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: Some(crate::system::boot_id().unwrap_or_default()),
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();

    assert_eq!(
        db.reconcile_stranded_queue_specs().unwrap(),
        0,
        "a genuinely live run must not be reconciled"
    );

    assert!(
        !git_is_clean(dir.path()),
        "a healthy run's worktree changes must never be touched"
    );

    let run_after = db.get_graph_run("run-stranded-healthy").unwrap().unwrap();
    assert_eq!(run_after.status, GraphRunStatus::Running);
}
