//! Lifecycle tests (split from `lifecycle` so that file stays under the
//! 800-line gate): panic-guard behaviour, pause races, sandbox resume and
//! workdir persistence.
use super::*;
use crate::application::notification_service::DefaultNotificationService;

fn test_db() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = Database::new(&dir.path().join("test.db")).expect("open db");
    (dir, db)
}

#[tokio::test]
async fn run_guarded_reports_a_str_panic() {
    let error = run_guarded::<(), _>(async {
        panic!("str-panic-payload");
    })
    .await
    .expect_err("a panicked future must become an Err");
    let message = error.to_string();
    assert!(message.contains("panicked"), "message: {message}");
    assert!(message.contains("str-panic-payload"), "message: {message}");
}

#[tokio::test]
async fn run_guarded_reports_a_string_panic() {
    let payload = String::from("string-panic-payload");
    let error = run_guarded::<(), _>(async move {
        panic!("{payload}");
    })
    .await
    .expect_err("a panicked future must become an Err");
    let message = error.to_string();
    assert!(message.contains("panicked"), "message: {message}");
    assert!(
        message.contains("string-panic-payload"),
        "message: {message}"
    );
}

#[tokio::test]
async fn run_guarded_returns_ok_unchanged() {
    let value = run_guarded::<i32, _>(async { Ok(42) })
        .await
        .expect("an Ok future stays Ok");
    assert_eq!(value, 42);
}

#[tokio::test]
async fn run_guarded_returns_err_unchanged() {
    let error = run_guarded::<(), _>(async { Err(anyhow!("boom-err")) })
        .await
        .expect_err("an Err future stays Err");
    assert!(error.to_string().contains("boom-err"), "error: {error:#}");
}

async fn wait_for_status(db: &Database, graph_id: &str, want: GraphStatus) {
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

/// Insert a second graph on the same DB/workdir that completes on its own:
/// one passing check node bound to one pending spec.
fn insert_passing_graph(
    db: &Database,
    template_graph: &Graph,
    template_spec: &GraphSpec,
    workdir: &str,
) -> String {
    let graph_id = format!("second-{}", uuid::Uuid::new_v4());
    let mut graph = template_graph.clone();
    graph.id = graph_id.clone();
    graph.name = "Second".to_string();
    graph.workdir = workdir.to_string();
    graph.status = GraphStatus::Draft;
    graph.started_at = None;
    graph.completed_at = None;
    graph.active_run_queue_id = None;
    graph.hooks.clear();
    db.insert_graph(&graph).unwrap();

    let spec_id = format!("second-spec-{}", uuid::Uuid::new_v4());
    let mut spec = template_spec.clone();
    spec.id = spec_id.clone();
    spec.graph_id = Some(graph_id.clone());
    spec.name = "Second spec".to_string();
    spec.status = GraphSpecStatus::Pending;
    spec.started_at = None;
    spec.completed_at = None;
    db.insert_graph_spec(&spec).unwrap();

    db.insert_graph_node(&GraphNode {
        id: format!("second-check-{}", uuid::Uuid::new_v4()),
        spec_id: Some(spec_id),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    graph_id
}

/// Insert a second, already-completed spec on the same graph so the test
/// can prove a panic leaves spec statuses alone (FR4).
fn insert_completed_spec(db: &Database, template_spec_id: &str) -> String {
    let mut spec = db.get_graph_spec(template_spec_id).unwrap().unwrap();
    let done_id = format!("done-spec-{}", uuid::Uuid::new_v4());
    spec.id = done_id.clone();
    spec.name = "Done spec".to_string();
    spec.position = 0;
    spec.status = GraphSpecStatus::Completed;
    spec.completed_at = Some(chrono::Utc::now());
    db.insert_graph_spec(&spec).unwrap();
    done_id
}

/// A panic inside `run_graph_dispatch` must reach the existing `Err` arm:
/// the graph ends `Failed` with the panic text as its failure reason, the
/// `on_failed` hook fires once, the hook-launched flag is cleared, no spec
/// is marked completed, and the engine can start another graph afterwards.
#[tokio::test]
async fn a_panicking_dispatch_fails_the_graph_and_leaves_the_engine_usable() {
    let (dir, db, engine, graph_id, spec_id) = crate::graph_engine::tests::graph_fixture().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let done_spec_id = insert_completed_spec(&db, &spec_id);
    db.mark_graph_as_hook_launched(&graph_id).unwrap();
    let marker = dir.path().join("on-failed-fired");
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnFailed,
        vec![GraphCompletionHook {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: Some(format!("touch {}", marker.display())),
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(1),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
            crew: None,
            spec_ids: None,
        }],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let mut engine = engine;
    engine.panic_at_dispatch = Some(|| panic!("boom-x"));
    Arc::new(engine).start_background_with_trigger(graph_id.clone(), None);
    wait_for_status(&db, &graph_id, GraphStatus::Failed).await;

    let events = db
        .list_graph_events(&graph_id, None, None, None, 200)
        .unwrap();
    let failed = events
        .iter()
        .find(|event| event.kind == "graph_failed")
        .unwrap_or_else(|| panic!("the panic must record a graph_failed event: {events:?}"));
    assert!(
        failed.message.contains("panicked"),
        "failure reason must name the panic: {}",
        failed.message
    );
    assert!(
        failed.message.contains("boom-x"),
        "failure reason must carry the panic payload: {}",
        failed.message
    );

    // `fail_graph` sets the status before it fires the hooks, so wait for
    // the command hook's own effect instead of racing it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !marker.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(marker.exists(), "the on_failed command hook must have run");
    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    let failed_hooks = hook_runs
        .iter()
        .filter(|run| run.event == GraphHookEvent::OnFailed)
        .count();
    assert_eq!(failed_hooks, 1, "on_failed must fire exactly once");
    assert!(
        !db.is_graph_hook_launched(&graph_id).unwrap(),
        "the panic path must still clear the hook-launched flag"
    );
    // FR4: the panic must never mark a spec completed nor rewrite a
    // completed spec — the in-flight spec stays resumable via retry.
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(
        spec.status,
        GraphSpecStatus::Pending,
        "the panic must leave the in-flight spec pending, not completed"
    );
    let done = db.get_graph_spec(&done_spec_id).unwrap().unwrap();
    assert_eq!(
        done.status,
        GraphSpecStatus::Completed,
        "the panic must not touch the already-completed spec"
    );

    let template_graph = db.get_graph(&graph_id).unwrap().unwrap();
    let template_spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    let second_id = insert_passing_graph(&db, &template_graph, &template_spec, &workdir);
    let second_engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));
    second_engine
        .run_graph_mode(
            second_id.clone(),
            None,
            None,
            None,
            None,
            DispatchMode::fresh(),
        )
        .await
        .expect("a second graph must run after the panic");
    let second = db.get_graph(&second_id).unwrap().unwrap();
    assert_eq!(second.status, GraphStatus::Completed);
}

fn test_sandbox(id: &str, worktree: &std::path::Path) -> Sandbox {
    Sandbox {
        id: id.to_string(),
        project_hash: "abcd1234".to_string(),
        base_branch: "main".to_string(),
        sandbox_branch: "canopy/sandbox-test".to_string(),
        worktree_path: worktree.to_path_buf(),
        cli_name: "opencode".to_string(),
        original_workdir: "/home/user/project".to_string(),
        created_at: chrono::Utc::now(),
    }
}

#[test]
fn none_without_row() {
    let (_dir, db) = test_db();
    assert!(resolve_resume_sandbox(&db, "graph-1").unwrap().is_none());
}

#[test]
fn lost_pause_race_accepts_pausing_and_paused_only() {
    use GraphStatus::*;
    for status in [Pausing, Paused] {
        assert!(
            lost_pause_race_accepted(Some(status)),
            "{status:?} left behind by the race winner still accepts the pause"
        );
    }
    for status in [
        None,
        Some(Draft),
        Some(Running),
        Some(Completed),
        Some(Failed),
    ] {
        assert!(
            !lost_pause_race_accepted(status),
            "{status:?} after a lost pause race refuses the pause"
        );
    }
}

#[test]
fn finds_kept() {
    let (_dir, db) = test_db();
    let worktree = tempfile::tempdir().expect("worktree dir");
    let sandbox = test_sandbox("sb-kept", worktree.path());
    db.insert_sandbox_run(&sandbox, "graph", "graph-1")
        .expect("insert sandbox run");
    db.update_sandbox_run_status("sb-kept", "kept")
        .expect("mark kept");

    let found = resolve_resume_sandbox(&db, "graph-1")
        .expect("kept sandbox resolves")
        .expect("some sandbox");
    assert_eq!(found.id, "sb-kept");
    assert_eq!(found.worktree_path, worktree.path());
}

#[test]
fn refuses_missing_worktree() {
    let (_dir, db) = test_db();
    let dir = tempfile::tempdir().expect("parent dir");
    let gone = dir.path().join("gone");
    let sandbox = test_sandbox("sb-gone", &gone);
    db.insert_sandbox_run(&sandbox, "graph", "graph-1")
        .expect("insert sandbox run");

    let err = resolve_resume_sandbox(&db, "graph-1").expect_err("missing worktree refuses");
    assert!(
        err.contains("sb-gone"),
        "refusal must name the sandbox id: {err}"
    );
}
