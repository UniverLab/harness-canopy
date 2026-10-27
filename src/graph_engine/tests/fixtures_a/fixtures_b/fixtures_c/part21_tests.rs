//! Part 21 of graph-engine `tests`: a failed graph never tears down its
//! sandbox automatically — the row is marked `kept` with a cleanup note,
//! the worktree and branch stay in place for the operator, and a resume
//! runs inside that worktree instead of falling back to the real checkout
//! (the 2026-10-27 incident: a hook-refused commit wiped 29 + 21
//! uncommitted files that had passed gates and review).
use super::*;

/// A real sandbox over `repo`: its own worktree root, a
/// `canopy/sandbox-*` branch — the shape a failed run actually leaves
/// behind, unlike a bare tempdir.
async fn real_sandbox(
    repo: &std::path::Path,
) -> (tempfile::TempDir, crate::domain::sandbox::Sandbox) {
    let root = tempfile::tempdir().expect("worktrees root");
    let base = root.path().join("worktrees");
    let sandbox = crate::domain::sandbox::create_sandbox_in(
        &base,
        &repo.to_string_lossy(),
        "opencode",
        "protocol",
    )
    .await
    .expect("create sandbox");
    (root, sandbox)
}

fn branch_exists(repo: &std::path::Path, branch: &str) -> bool {
    let output = std::process::Command::new("git")
        .args(["branch", "--list", branch])
        .current_dir(repo)
        .output()
        .expect("git branch --list");
    String::from_utf8_lossy(&output.stdout).contains(branch)
}

/// Poll until `graph_id` reaches `want`, yielding to the task
/// `resume_background` spawns (it never blocks its caller).
async fn wait_for_graph_status(db: &Database, graph_id: &str, want: GraphStatus) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let lp = db.get_graph(graph_id).unwrap().unwrap();
        if lp.status == want {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "graph '{graph_id}' never reached {want:?} (status {:?})",
            lp.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Poll until `sandbox_id`'s row carries a cleanup note containing `needle`.
/// `fail_graph` writes the graph status first and this note last, so waiting
/// on the note means the whole failure path — activity included — landed.
async fn wait_for_cleanup_note(db: &Database, sandbox_id: &str, needle: &str) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let note = db
            .get_sandbox_run(sandbox_id)
            .unwrap()
            .and_then(|row| row.cleanup_error)
            .unwrap_or_default();
        if note.contains(needle) {
            return note;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "sandbox '{sandbox_id}' never recorded '{needle}' (note: {note})"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// FR3/FR5: `fail_graph` leaves a sandboxed run's worktree, branch and
/// uncommitted files alone, marking the row `kept` with a note that names
/// the worktree and the failure summary. The sandbox here has no unique
/// commits — exactly the shape the old teardown read as "nothing to keep"
/// before force-removing it.
#[tokio::test]
async fn fail_graph_keeps_sandbox_with_uncommitted_work() {
    let (dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let (_root, sandbox) = real_sandbox(dir.path()).await;
    std::fs::write(sandbox.worktree_path.join("wip.txt"), "uncommitted work")
        .expect("write uncommitted file");
    db.insert_sandbox_run(&sandbox, "graph", &graph_id)
        .expect("insert sandbox run");
    let row = db.get_sandbox_run(&sandbox.id).unwrap().unwrap();
    assert_eq!(
        crate::domain::sandbox::has_unique_commits(&row),
        Some(false),
        "the incident's shape: uncommitted work, no unique commits"
    );

    engine
        .fail_graph(&graph_id, None, Some("Spec"), None, "hook refused", &[])
        .await
        .expect("fail graph");

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed);
    let row = db.get_sandbox_run(&sandbox.id).unwrap().unwrap();
    assert_eq!(row.status, "kept");
    let note = row.cleanup_error.expect("cleanup note recorded");
    assert!(
        note.contains(&sandbox.worktree_path.to_string_lossy().into_owned()),
        "note must name the worktree dir: {note}"
    );
    assert!(
        note.contains("hook refused"),
        "note must name the failure summary: {note}"
    );
    assert_eq!(
        std::fs::read_to_string(sandbox.worktree_path.join("wip.txt")).unwrap(),
        "uncommitted work",
        "the uncommitted work must survive the failure"
    );
    assert!(
        branch_exists(dir.path(), &sandbox.sandbox_branch),
        "the sandbox branch must still exist"
    );
}

/// FR4: continuing a failed graph whose sandbox is `kept` runs the next
/// node inside that worktree — never against the user's real checkout.
/// The node fails on purpose so the run never reaches the completed-path
/// teardown: what is under test is where it ran.
#[tokio::test]
async fn resume_runs_inside_the_kept_worktree() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let (_root, sandbox) = real_sandbox(dir.path()).await;
    db.insert_sandbox_run(&sandbox, "graph", &graph_id)
        .expect("insert sandbox run");
    db.update_sandbox_run_status(&sandbox.id, "kept")
        .expect("mark kept");
    db.update_graph_status(&graph_id, GraphStatus::Paused, None, None)
        .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "pwd > ran-here.txt && exit 1",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    Arc::new(engine).resume_background(graph_id.clone(), None);

    wait_for_graph_status(&db, &graph_id, GraphStatus::Failed).await;
    let ran_in = std::fs::read_to_string(sandbox.worktree_path.join("ran-here.txt"))
        .expect("the resumed node must have run inside the kept worktree");
    assert_eq!(
        std::fs::canonicalize(ran_in.trim()).unwrap(),
        std::fs::canonicalize(&sandbox.worktree_path).unwrap(),
        "the node's cwd must be the kept worktree"
    );
    let row = db.get_sandbox_run(&sandbox.id).unwrap().unwrap();
    assert_eq!(row.status, "kept", "the failed run keeps its sandbox");
    assert!(sandbox.worktree_path.exists());
}

/// FR4: a resume that cannot find its sandbox refuses — naming the
/// sandbox id — instead of silently running against the original workdir.
#[tokio::test]
async fn resume_refuses_when_the_sandbox_worktree_is_gone() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let gone = dir.path().join("gone-worktree");
    let sandbox = crate::domain::sandbox::Sandbox {
        id: "sb-gone".to_string(),
        project_hash: "abcd1234".to_string(),
        base_branch: "main".to_string(),
        sandbox_branch: "canopy/sandbox-gone".to_string(),
        worktree_path: gone,
        cli_name: "opencode".to_string(),
        original_workdir: dir.path().to_string_lossy().to_string(),
        created_at: chrono::Utc::now(),
    };
    db.insert_sandbox_run(&sandbox, "graph", &graph_id)
        .expect("insert sandbox run");
    db.update_sandbox_run_status(&sandbox.id, "kept")
        .expect("mark kept");
    db.update_graph_status(&graph_id, GraphStatus::Paused, None, None)
        .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    Arc::new(engine).resume_background(graph_id.clone(), None);

    wait_for_graph_status(&db, &graph_id, GraphStatus::Failed).await;
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed);
    let note = wait_for_cleanup_note(&db, "sb-gone", "kept for the operator").await;
    assert!(
        note.contains("sb-gone"),
        "the refusal must name the sandbox id: {note}"
    );
    let workdir = dir.path().to_string_lossy().to_string();
    let messages: Vec<String> = db
        .list_activity_log_entries(&workdir, 50)
        .unwrap()
        .into_iter()
        .map(|entry| entry.message)
        .collect();
    assert!(
        messages.iter().any(|m| m.contains("sb-gone")),
        "the refusal must reach the graph's activity log: {messages:?}"
    );
    assert!(
        db.list_graph_runs_for_spec(&spec_id).unwrap().is_empty(),
        "a refused resume must not run a single node"
    );
}
