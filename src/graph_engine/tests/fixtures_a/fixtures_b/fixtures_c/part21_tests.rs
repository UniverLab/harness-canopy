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

// ── A spec bound while the graph runs is not lost ────────────────────
//
// The 2026-10-26 incident: spec `1b594ab0` was bound to graph
// `graphs-catalogue` at position 6 while spec 4 was running; the run
// finished as `graph_completed` with `Completed (5/6 specs).` and the new
// spec stayed `pending` forever. These four tests drive the queue advance
// from a live read and close the completion path that reported it.

/// Poll until `spec_id` reports `Running` — the window a mid-run bind has to
/// land in to be visible to the live advance.
async fn wait_for_spec_running(db: &Database, spec_id: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let status = db.get_graph_spec(spec_id).unwrap().map(|spec| spec.status);
        if status == Some(GraphSpecStatus::Running) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "spec '{spec_id}' never started (status {status:?})"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// A check node that appends `label` to `log` and then blocks until `gate`
/// exists: it keeps its spec `running` for as long as the test needs to bind
/// another spec into the graph.
fn blocking_record_node(
    id: &str,
    spec_id: &str,
    label: &str,
    log: &std::path::Path,
    gate: &std::path::Path,
) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": format!(
                "echo {label} >> \"{log}\"; while [ ! -f \"{gate}\" ]; do sleep 0.02; done; printf APPROVED",
                log = log.display(),
                gate = gate.display(),
            ),
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    }
}

fn logged_lines(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn activity_messages(db: &Database, workdir: &str) -> Vec<String> {
    db.list_activity_log_entries(workdir, 200)
        .unwrap()
        .into_iter()
        .map(|entry| entry.message)
        .collect()
}

/// Every `Completed (n/m specs).` line in `messages`, as `(n, m)`.
fn completion_counts(messages: &[String]) -> Vec<(usize, usize)> {
    messages
        .iter()
        .filter_map(|message| {
            let rest = message.strip_prefix("Completed (")?;
            let (done, rest) = rest.split_once('/')?;
            let (total, _) = rest.split_once(' ')?;
            Some((done.parse().ok()?, total.parse().ok()?))
        })
        .collect()
}

/// FR1/FR2: a spec bound at a position *after* the current one while the run
/// is in flight is picked up by the next advance and runs in position order.
/// The snapshot the engine used to capture at run start never contained it —
/// the graph finished `Completed (2/3 specs).` and left it pending, which is
/// exactly the 2026-10-26 loss.
#[tokio::test]
async fn spec_bound_mid_run_at_a_later_position_runs_in_position_order() {
    let (dir, db, engine, graph_id, spec_a) = graph_fixture().unwrap();
    let log = dir.path().join("order.log");
    let gate = dir.path().join("gate");
    db.insert_graph_node(&blocking_record_node("a-check", &spec_a, "A", &log, &gate))
        .unwrap();
    let spec_b = second_spec(&graph_id, "spec-b", 2);
    db.insert_graph_spec(&spec_b).unwrap();
    db.insert_graph_node(&record_node("b-check", &spec_b.id, &log, "B"))
        .unwrap();

    let engine = Arc::new(engine);
    let run = tokio::spawn({
        let engine = Arc::clone(&engine);
        let graph_id = graph_id.clone();
        async move {
            engine
                .run_graph(graph_id, None, None, None, None, None)
                .await
        }
    });
    wait_for_spec_running(&db, &spec_a).await;
    // The live window: bind spec C at position 5 while spec A is still running.
    let spec_c = second_spec(&graph_id, "spec-c", 5);
    db.insert_graph_spec(&spec_c).unwrap();
    db.insert_graph_node(&record_node("c-check", &spec_c.id, &log, "C"))
        .unwrap();
    std::fs::write(&gate, "go").unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(30), run)
        .await
        .expect("the dispatch must finish")
        .unwrap()
        .unwrap();

    assert_eq!(
        logged_lines(&log),
        vec!["A".to_string(), "B".to_string(), "C".to_string()],
        "the mid-run bind runs after the specs already at lower positions"
    );
    for spec_id in [&spec_a, &spec_b.id, &spec_c.id] {
        let spec = db.get_graph_spec(spec_id).unwrap().unwrap();
        assert_eq!(
            spec.status,
            GraphSpecStatus::Completed,
            "spec '{spec_id}' must have run"
        );
    }
    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Completed
    );
    assert!(
        activity_messages(&db, &dir.path().to_string_lossy())
            .iter()
            .any(|message| message == "Completed (3/3 specs)."),
        "the graph reports all three specs, the mid-run one included"
    );
}

/// FR2: a spec bound at a position *before* the current one while it runs is
/// the next spec the graph runs — the advance restarts its scan at position 0,
/// so the newcomer comes right after the spec that was in flight.
#[tokio::test]
async fn spec_bound_mid_run_at_a_lower_position_runs_next() {
    let (dir, db, engine, graph_id, spec_a) = graph_fixture().unwrap();
    let log = dir.path().join("order.log");
    let gate = dir.path().join("gate");
    db.insert_graph_node(&blocking_record_node("a-check", &spec_a, "A", &log, &gate))
        .unwrap();
    let spec_b = second_spec(&graph_id, "spec-b", 2);
    db.insert_graph_spec(&spec_b).unwrap();
    db.insert_graph_node(&record_node("b-check", &spec_b.id, &log, "B"))
        .unwrap();

    let engine = Arc::new(engine);
    let run = tokio::spawn({
        let engine = Arc::clone(&engine);
        let graph_id = graph_id.clone();
        async move {
            engine
                .run_graph(graph_id, None, None, None, None, None)
                .await
        }
    });
    wait_for_spec_running(&db, &spec_a).await;
    let spec_c = second_spec(&graph_id, "spec-c", 0);
    db.insert_graph_spec(&spec_c).unwrap();
    db.insert_graph_node(&record_node("c-check", &spec_c.id, &log, "C"))
        .unwrap();
    std::fs::write(&gate, "go").unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(30), run)
        .await
        .expect("the dispatch must finish")
        .unwrap()
        .unwrap();

    assert_eq!(
        logged_lines(&log),
        vec!["A".to_string(), "C".to_string(), "B".to_string()],
        "the spec bound before the current position runs right after it"
    );
    let spec_c = db.get_graph_spec(&spec_c.id).unwrap().unwrap();
    assert_eq!(
        spec_c.status,
        GraphSpecStatus::Completed,
        "the spec bound at position 0 must have run"
    );
}

/// FR3: a graph that would settle while a bound spec is still `pending` never
/// reports completion. It fails, naming the pending spec id, and `on_completed`
/// hooks do not fire.
#[tokio::test]
async fn a_pending_bound_spec_is_never_reported_as_a_completed_graph() {
    let (dir, db, engine, graph_id, spec_a) = graph_fixture().unwrap();
    db.update_graph_spec_status(
        &spec_a,
        GraphSpecStatus::Completed,
        None,
        Some(chrono::Utc::now()),
    )
    .unwrap();
    let pending = second_spec(&graph_id, "spec-pending", 2);
    db.insert_graph_spec(&pending).unwrap();
    let hook_marker = dir.path().join("on-completed-fired");
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnCompleted,
        vec![crate::domain::graphs::GraphCompletionHook {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: Some(format!("touch {}", hook_marker.display())),
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(1),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
            crew: None,
        }],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();
    let claimed_at = chrono::Utc::now();
    assert!(
        db.claim_graph_for_run(&graph_id, claimed_at).unwrap(),
        "the fixture graph must be claimable"
    );

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let ctx = DispatchCtx {
        lp: &lp,
        graph_id: &graph_id,
        workdir: &workdir,
        is_resume: false,
        directed: None,
        claimed_at,
    };
    engine.finish_dispatch(&ctx, None, None, &[]).await.unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Failed,
        "a pending bound spec must not settle as a completed graph"
    );
    let events = db
        .list_graph_events(&graph_id, None, None, None, 200)
        .unwrap();
    assert!(
        !events.iter().any(|event| event.kind == "graph_completed"),
        "no graph_completed may be recorded: {events:?}"
    );
    assert!(
        events
            .iter()
            .filter(|event| event.kind == "graph_failed")
            .any(|event| event.message.contains(&pending.id)),
        "the failure must name the pending spec id '{}': {:?}",
        pending.id,
        events
            .iter()
            .map(|event| (event.kind.clone(), event.message.clone()))
            .collect::<Vec<_>>()
    );
    assert!(
        !hook_marker.exists(),
        "on_completed hooks must not fire for a graph that never completed"
    );
}

/// FR3/FR4: the completion message can no longer say `Completed (n/m specs).`
/// with fewer done than total — a full run still reports `(2/2)`, and a
/// dispatch that settles with a spec bound after the queue drained records no
/// completion line at all instead of `(2/3)`.
#[tokio::test]
async fn the_completion_message_never_reports_fewer_done_than_total() {
    let (dir, db, engine, graph_id, spec_a) = graph_fixture().unwrap();
    let log = dir.path().join("order.log");
    db.insert_graph_node(&record_node("a-check", &spec_a, &log, "A"))
        .unwrap();
    let spec_b = second_spec(&graph_id, "spec-b", 2);
    db.insert_graph_spec(&spec_b).unwrap();
    db.insert_graph_node(&record_node("b-check", &spec_b.id, &log, "B"))
        .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let messages = activity_messages(&db, &workdir);
    assert!(
        messages.iter().any(|m| m == "Completed (2/2 specs)."),
        "a full bound-spec run still reports its exact progress: {messages:?}"
    );
    assert_eq!(
        completion_counts(&messages),
        vec![(2, 2)],
        "no completion line may report fewer done than total: {messages:?}"
    );

    // Bind a third spec after the queue drained, then let the dispatch settle
    // again: the old code reported `Completed (2/3 specs).` here.
    let spec_c = second_spec(&graph_id, "spec-c", 3);
    db.insert_graph_spec(&spec_c).unwrap();
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let ctx = DispatchCtx {
        lp: &lp,
        graph_id: &graph_id,
        workdir: &workdir,
        is_resume: false,
        directed: None,
        claimed_at: chrono::Utc::now(),
    };
    engine.finish_dispatch(&ctx, None, None, &[]).await.unwrap();

    let messages = activity_messages(&db, &workdir);
    assert_eq!(
        completion_counts(&messages),
        vec![(2, 2)],
        "a pending bound spec must never be counted into a completion line: {messages:?}"
    );
}

// ── A directed run keeps the verdicts it does not touch ──────────────
//
// `graph_continue run_from_node` / `graph_run --from` accept a `completed`
// spec (docs/graphs.md) and start past the bound specs before their target.
// The live pick must therefore run the target whatever its status, and must
// not rewrite an earlier `completed` into `skipped` — that erases the
// verdict, drops the spec out of the done count, and re-creates the
// `Completed (n/m)` line with n < m that FR4 exists to make impossible.

/// FR2/directed: the directed target runs even when its spec is already
/// `completed` — the documented `graph_continue run_from_node` on a graph
/// whose specs are done. Handing a completed target to the terminal skip
/// makes the operator's directed run silently do nothing.
#[tokio::test]
async fn a_directed_run_at_a_completed_bound_spec_still_runs_it() {
    let (dir, db, engine, graph_id, spec_a) = graph_fixture().unwrap();
    let log = dir.path().join("order.log");
    db.insert_graph_node(&record_node("a-check", &spec_a, &log, "A"))
        .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(
        logged_lines(&log),
        vec!["A".to_string()],
        "the first run completes the spec"
    );
    assert_eq!(
        db.get_graph_spec(&spec_a).unwrap().unwrap().status,
        GraphSpecStatus::Completed
    );

    // The operator re-runs that same, now completed, spec from its node.
    engine
        .run_graph_dispatch(
            graph_id.clone(),
            None,
            None,
            DispatchMode::resume_from(DirectedStart {
                spec_id: spec_a.clone(),
                node_arg: "a-check".to_string(),
            }),
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        logged_lines(&log),
        vec!["A".to_string(), "A".to_string()],
        "the directed target must run again even though its spec is completed"
    );
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Completed,
        "the directed run settles the graph"
    );
}

/// FR4: a directed run that starts past an already-`completed` bound spec
/// keeps that spec's verdict — and the completion line keeps reporting
/// n == m. Rewriting the earlier `completed` to `skipped` erases the
/// completion, drops it out of `spec_progress`'s done count, and produces
/// exactly the `Completed (n/m)` line with n < m FR4 forbids.
#[tokio::test]
async fn a_directed_run_keeps_an_earlier_completed_bound_spec_completed() {
    let (dir, db, engine, graph_id, spec_a) = graph_fixture().unwrap();
    let log = dir.path().join("order.log");
    db.insert_graph_node(&record_node("a-check", &spec_a, &log, "A"))
        .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(logged_lines(&log), vec!["A".to_string()]);

    // Spec B is bound only after A completed: A already sits before the
    // directed target and is terminal when the directed run starts.
    let spec_b = second_spec(&graph_id, "spec-b", 2);
    db.insert_graph_spec(&spec_b).unwrap();
    db.insert_graph_node(&record_node("b-check", &spec_b.id, &log, "B"))
        .unwrap();

    engine
        .run_graph_dispatch(
            graph_id.clone(),
            None,
            None,
            DispatchMode::resume_from(DirectedStart {
                spec_id: spec_b.id.clone(),
                node_arg: "b-check".to_string(),
            }),
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        logged_lines(&log),
        vec!["A".to_string(), "B".to_string()],
        "the directed target runs past the completed spec, which does not re-run"
    );
    let a = db.get_graph_spec(&spec_a).unwrap().unwrap();
    assert_eq!(
        a.status,
        GraphSpecStatus::Completed,
        "a completed spec before the directed target keeps its verdict"
    );
    let messages = activity_messages(&db, &dir.path().to_string_lossy());
    let counts = completion_counts(&messages);
    assert!(
        counts.contains(&(2, 2)),
        "the run reports every spec completed: {counts:?} / {messages:?}"
    );
    assert!(
        counts.iter().all(|(done, total)| done == total),
        "no completion line may report fewer done than total: {counts:?}"
    );
}
