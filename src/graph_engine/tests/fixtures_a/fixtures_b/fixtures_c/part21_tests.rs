//! Part 21 of graph-engine `tests`: a failed graph never tears down its
//! sandbox automatically — the row is marked `kept` with a cleanup note,
//! and the worktree stays in place for the operator.
use super::*;

/// `fail_graph` must leave a sandboxed run's worktree and branch alone,
/// marking the row `kept` with a note that names the worktree and the
/// failure summary (the 2026-10-27 incident: a hook-refused commit wiped
/// 29 + 21 uncommitted files that had passed gates and review).
#[tokio::test]
async fn fail_graph_keeps_sandbox_for_operator() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let worktree = tempfile::tempdir().expect("worktree dir");
    let sandbox = crate::domain::sandbox::Sandbox {
        id: "sb-failed-keep".to_string(),
        project_hash: "abcd1234".to_string(),
        base_branch: "main".to_string(),
        sandbox_branch: "canopy/sandbox-t".to_string(),
        worktree_path: worktree.path().to_path_buf(),
        cli_name: "opencode".to_string(),
        original_workdir: "/home/user/project".to_string(),
        created_at: chrono::Utc::now(),
    };
    db.insert_sandbox_run(&sandbox, "graph", &graph_id)
        .expect("insert sandbox run");

    engine
        .fail_graph(&graph_id, None, Some("Spec"), None, "hook refused", &[])
        .await
        .expect("fail graph");

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed);
    let row = db.get_sandbox_run("sb-failed-keep").unwrap().unwrap();
    assert_eq!(row.status, "kept");
    let note = row.cleanup_error.expect("cleanup note recorded");
    assert!(
        note.contains(&worktree.path().to_string_lossy().into_owned()),
        "note must name the worktree dir: {note}"
    );
    assert!(
        note.contains("hook refused"),
        "note must name the failure summary: {note}"
    );
    assert!(
        worktree.path().exists(),
        "the worktree must still be in place"
    );
}
