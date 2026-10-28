//! Part 23 of graph-engine `tests`: recovering a spec that was interrupted
//! mid-run (a WSL shutdown, a killed daemon). The dirt on the workdir is the
//! resuming spec's own work, and a *skipped* spec's stale record from another
//! graph must never make that ambiguous (FR1).
use super::*;

/// FR1: a spec left `Skipped` never ran, so whatever its row recorded about
/// the workdir is not "previous work" that blocks. Here the skipped record
/// even names the file that is still dirty — the exact shape that refused the
/// real recovery ("spec 'update zstd…' (graph 'standalone', skipped …)").
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
