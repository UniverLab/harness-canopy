//! Part 23 of graph-engine `tests`: recovering a spec that was interrupted
//! mid-run (a WSL shutdown, a killed daemon). The dirt on the workdir is the
//! resuming spec's own work, and a *skipped* spec's stale record — from
//! another graph, a standalone row, or this graph itself — must never make
//! that ambiguous (FR1).
use super::*;

/// FR1: a spec left `Skipped` never ran, so whatever its row recorded about
/// the workdir is not "previous work" that blocks. Here the skipped records
/// even name the file that is still dirty — the exact shape that refused the
/// real recovery ("spec 'update zstd…' (graph 'standalone', skipped …)").
/// Two rows pin the two filters: a standalone one (excluded by graph scope)
/// and one bound to this graph (excluded by status alone — without the
/// status filter this test refuses).
#[tokio::test]
async fn dirty_start_check_ignores_a_skipped_specs_stale_dirty_record() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();

    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-skip",
        "SkippedElsewhere",
        &workdir,
        GraphSpecStatus::Skipped,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-skip", Some(1), &["x.txt".to_string()])
        .unwrap();
    let mut skip_same = dirty_predecessor_spec_named(
        "spec-skip-same",
        "SkippedSameGraph",
        &workdir,
        GraphSpecStatus::Skipped,
    );
    skip_same.graph_id = Some(graph_id.clone());
    skip_same.position = 2;
    db.insert_graph_spec(&skip_same).unwrap();
    db.set_graph_spec_end_state("spec-skip-same", Some(1), &["x.txt".to_string()])
        .unwrap();
    // The spec being resumed: its own latest run recorded x.txt and y.txt.
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-b",
        "Beta",
        &workdir,
        GraphSpecStatus::Interrupted,
    ))
    .unwrap();
    db.set_graph_spec_end_state(
        "spec-b",
        Some(2),
        &["x.txt".to_string(), "y.txt".to_string()],
    )
    .unwrap();
    insert_run_row(
        &db,
        &graph_id,
        "spec-b",
        "node-b",
        1,
        chrono::Utc::now().timestamp() - 60,
    )
    .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, Some("spec-b"), false)
        .await
        .unwrap()
        .expect("resuming its own dirt must produce a notice");
    assert!(
        !notice.refuse,
        "a skipped spec's stale dirty record must never block: {}",
        notice.message
    );
    assert!(
        !notice.message.contains("SkippedElsewhere"),
        "the skipped spec must not be named as a blocker: {}",
        notice.message
    );
    assert!(
        !notice.message.contains("SkippedSameGraph"),
        "the same-graph skipped spec must not be named as a blocker: {}",
        notice.message
    );
    assert!(
        notice.message.contains("Beta"),
        "the resuming spec must be named: {}",
        notice.message
    );
}

/// FR1: when the current spec resumes and the workdir holds exactly the paths
/// its own latest run recorded, the dirty work is its own — no refusal, even
/// though an interrupted spec elsewhere on the workdir also recorded dirt.
#[tokio::test]
async fn dirty_start_check_resumes_its_own_recorded_dirty_paths() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();

    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-int",
        "InterruptedElsewhere",
        &workdir,
        GraphSpecStatus::Interrupted,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-int", Some(1), &["x.txt".to_string()])
        .unwrap();
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-b",
        "Beta",
        &workdir,
        GraphSpecStatus::Interrupted,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-b", Some(1), &["x.txt".to_string()])
        .unwrap();
    insert_run_row(
        &db,
        &graph_id,
        "spec-b",
        "node-b",
        1,
        chrono::Utc::now().timestamp() - 60,
    )
    .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, Some("spec-b"), false)
        .await
        .unwrap()
        .expect("resuming its own dirt must produce a notice");
    assert!(
        !notice.refuse,
        "the workdir holds exactly this spec's own recorded dirt: {}",
        notice.message
    );
    assert!(
        notice.message.contains("resuming its own work"),
        "message must state the resume reason: {}",
        notice.message
    );
}

/// FR1: the carve-out is only for *its own* paths. An interrupted spec whose
/// recorded dirt is another file still blocks, and the refusal names it.
#[tokio::test]
async fn dirty_start_check_refuses_another_specs_overlapping_dirty_paths() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();

    // Bound to this graph: only the same graph's (or active queue's) specs
    // can block; a record from elsewhere on the workdir is ignored.
    let mut blocker = dirty_predecessor_spec_named(
        "spec-int",
        "InterruptedElsewhere",
        &workdir,
        GraphSpecStatus::Interrupted,
    );
    blocker.graph_id = Some(graph_id.clone());
    blocker.position = 2;
    db.insert_graph_spec(&blocker).unwrap();
    db.set_graph_spec_end_state("spec-int", Some(1), &["x.txt".to_string()])
        .unwrap();
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-b",
        "Beta",
        &workdir,
        GraphSpecStatus::Interrupted,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-b", Some(1), &["y.txt".to_string()])
        .unwrap();
    insert_run_row(
        &db,
        &graph_id,
        "spec-b",
        "node-b",
        1,
        chrono::Utc::now().timestamp() - 60,
    )
    .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, Some("spec-b"), false)
        .await
        .unwrap()
        .expect("expected a dirty-start notice");
    assert!(
        notice.refuse,
        "another spec's recorded dirt is not this spec's own: {}",
        notice.message
    );
    assert!(
        notice.message.contains("InterruptedElsewhere"),
        "the blocking spec must be named: {}",
        notice.message
    );
    assert!(notice.message.contains("not launched"));
}

/// FR1: a record naming files that are no longer dirty does not block — the
/// dirt on the workdir is not the dirt that spec left.
#[tokio::test]
async fn dirty_start_check_ignores_a_record_whose_paths_are_gone() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();

    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-fail",
        "GoneElsewhere",
        &workdir,
        GraphSpecStatus::Failed,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-fail", Some(1), &["gone.txt".to_string()])
        .unwrap();
    std::fs::write(dir.path().join("y.txt"), "y").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, None, false)
        .await
        .unwrap();
    assert!(
        notice.is_none(),
        "a non-overlapping record must not block (nor produce a notice)"
    );
}

/// FR1: a stale *failed* record from a standalone backlog spec — overlapping
/// the very file still dirty — never blocks a spec resuming its own work.
/// The guard only weighs this graph's (and its active queue's) specs.
#[tokio::test]
async fn dirty_start_check_ignores_failed_standalone_specs_overlapping_record() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();

    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-stale",
        "StaleElsewhere",
        &workdir,
        GraphSpecStatus::Failed,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-stale", Some(1), &["x.txt".to_string()])
        .unwrap();
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-b",
        "Beta",
        &workdir,
        GraphSpecStatus::Interrupted,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-b", Some(1), &["y.txt".to_string()])
        .unwrap();
    insert_run_row(
        &db,
        &graph_id,
        "spec-b",
        "node-b",
        1,
        chrono::Utc::now().timestamp() - 60,
    )
    .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, Some("spec-b"), false)
        .await
        .unwrap()
        .expect("resuming its own dirt must produce a notice");
    assert!(
        !notice.refuse,
        "a standalone spec's stale record must never block: {}",
        notice.message
    );
    assert!(
        !notice.message.contains("StaleElsewhere"),
        "the stale spec must not be named as a blocker: {}",
        notice.message
    );
    assert!(
        notice.message.contains("Beta"),
        "the resuming spec must be named: {}",
        notice.message
    );
}

/// FR1: a failed spec of *another* graph on the same workdir — overlapping
/// the dirt still in the tree — never blocks a spec resuming its own work.
#[tokio::test]
async fn dirty_start_check_ignores_other_graphs_overlapping_dirty_record() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();

    // G1: another graph on the same git workdir, carrying spec A.
    db.insert_graph(&Graph {
        id: "g1".to_string(),
        name: "Graph1".to_string(),
        description: None,
        workdir: workdir.clone(),
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
    })
    .unwrap();
    let mut spec_a =
        dirty_predecessor_spec_named("spec-a", "Alpha", &workdir, GraphSpecStatus::Failed);
    spec_a.graph_id = Some("g1".to_string());
    spec_a.workdir = None;
    db.insert_graph_spec(&spec_a).unwrap();
    db.set_graph_spec_end_state("spec-a", Some(1), &["x.txt".to_string()])
        .unwrap();
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-b",
        "Beta",
        &workdir,
        GraphSpecStatus::Interrupted,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-b", Some(1), &["y.txt".to_string()])
        .unwrap();
    insert_run_row(
        &db,
        &graph_id,
        "spec-b",
        "node-b",
        1,
        chrono::Utc::now().timestamp() - 60,
    )
    .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, Some("spec-b"), false)
        .await
        .unwrap()
        .expect("resuming its own dirt must produce a notice");
    assert!(
        !notice.refuse,
        "another graph's stale record must never block: {}",
        notice.message
    );
    assert!(
        !notice.message.contains("Alpha"),
        "the other graph's spec must not be named as a blocker: {}",
        notice.message
    );
    assert!(
        notice.message.contains("Beta"),
        "the resuming spec must be named: {}",
        notice.message
    );
}

/// FR1: a failed member of the graph's *active* queue run still blocks a
/// fresh next spec when its recorded dirt overlaps the tree — queue members
/// are standalone rows, but they are this run's specs.
#[tokio::test]
async fn dirty_start_check_refuses_active_queue_members_overlapping_dirty_paths() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();

    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-member",
        "QueueMember",
        &workdir,
        GraphSpecStatus::Failed,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-member", Some(1), &["x.txt".to_string()])
        .unwrap();
    let next = standalone_spec("queue-next", 2);
    db.insert_graph_spec(&next).unwrap();
    insert_queue_with_members(&db, "queue-1", &["spec-member", &next.id]);
    db.set_graph_active_run_queue(&graph_id, Some("queue-1"))
        .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, Some(&next.id), false)
        .await
        .unwrap()
        .expect("expected a dirty-start notice");
    assert!(
        notice.refuse,
        "an active queue member's overlapping dirt blocks: {}",
        notice.message
    );
    assert!(
        notice.message.contains("QueueMember"),
        "the blocking member must be named: {}",
        notice.message
    );
    assert!(notice.message.contains("not launched"));
}

/// FR1 end to end: a stale standalone failure on the workdir does not pause
/// a queue run — the single member completes and no blocker is stored.
#[tokio::test]
async fn queue_run_completes_past_a_stale_standalone_dirty_record() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    // The fixture's database lives in the worktree: ignore it before the
    // repo commits, or every write to it reads as a dirty tree.
    std::fs::write(
        dir.path().join(".gitignore"),
        format!("{TEST_DB_FILENAME}*\n"),
    )
    .unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-stale",
        "StaleElsewhere",
        &workdir,
        GraphSpecStatus::Failed,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-stale", Some(1), &["x.txt".to_string()])
        .unwrap();
    let next = standalone_spec("queue-next", 1);
    db.insert_graph_spec(&next).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&next.id]);
    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();

    engine
        .run_graph_dispatch(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            DispatchMode::fresh(),
            None,
            None,
        )
        .await
        .unwrap();

    let spec = db.get_graph_spec(&next.id).unwrap().unwrap();
    assert_eq!(
        spec.status,
        GraphSpecStatus::Completed,
        "the member must complete past the stale record"
    );
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert!(
        !matches!(lp.status, GraphStatus::Paused),
        "the guard must not pause on a stale outside record: {:?}",
        lp.status
    );
    assert!(
        db.graph_blocker(&graph_id).unwrap().is_none(),
        "no blocker may be stored for the stale record"
    );
}
