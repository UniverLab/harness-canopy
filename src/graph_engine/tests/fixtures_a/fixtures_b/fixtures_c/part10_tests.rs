//! Part 10 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

/// CH1: a graph reaching `failed` fires `on_failed` with `{{blocker}}`
/// and `{{node}}` bound; the graph stays `Failed`.
#[tokio::test]
async fn graph_engine_on_failed_fires_with_blocker_and_node() {
    let fake_home = setup_test_cli_home();
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "exit 1",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // If {{blocker}}/{{node}} were unbound the render would fail and the
    // hook run would be Fail; Pass proves both bound.
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: Some("test-cli".to_string()),
        model: None,
        effort: None,
        prompt: Some("echo \"{{blocker}} {{node}}\" > /dev/null".to_string()),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(crate::domain::graphs::GraphHookEvent::OnFailed, vec![hook]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);
    drop(fake_home);

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed);
    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(
        hook_runs[0].event,
        crate::domain::graphs::GraphHookEvent::OnFailed
    );
    assert_eq!(hook_runs[0].hook_index, 0);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Pass);
}

/// CH1: `block_graph` fires `on_blocked` once with the ending node name.
#[tokio::test]
async fn graph_engine_on_blocked_fires_once_with_node_name() {
    let fake_home = setup_test_cli_home();
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    // Seed a top-level agent node + a running run so `ending_node_name`
    // resolves to a human-readable name.
    db.insert_graph_node(&GraphNode {
        id: "node-agent".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "reviewer".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "test-cli", "prompt": "hi"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_run(&crate::domain::graphs::GraphNodeRun {
        id: "run-1".to_string(),
        graph_id: graph_id.clone(),
        spec_id: "spec-test".to_string(),
        node_id: "node-agent".to_string(),
        status: GraphRunStatus::Running,
        iteration: 1,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        pid: None,
        boot_id: crate::system::boot_id(),
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: Some("test-cli".to_string()),
        model: None,
        effort: None,
        prompt: Some("echo \"{{blocker}} {{node}}\" > /dev/null".to_string()),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(crate::domain::graphs::GraphHookEvent::OnBlocked, vec![hook]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let _home = HomeGuard::set(fake_home.path());
    engine
        .block_graph(&graph_id, None, "needs human ruling")
        .await
        .unwrap();
    drop(_home);
    drop(fake_home);

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Paused);
    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(
        hook_runs[0].event,
        crate::domain::graphs::GraphHookEvent::OnBlocked
    );
    assert_eq!(hook_runs[0].hook_index, 0);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Pass);
}

/// CH1: two hooks on one event both run in declaration order; the first
/// failing neither stops the second nor changes the graph's status.
#[tokio::test]
async fn graph_engine_two_hooks_run_in_order_despite_first_failing() {
    let fake_home = setup_test_cli_home();
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
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

    let marker = dir.path().join("second_hook.marker");
    let failing = crate::domain::graphs::GraphCompletionHook {
        platform: Some("test-cli".to_string()),
        model: None,
        effort: None,
        prompt: Some("exit 1".to_string()),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let passing = crate::domain::graphs::GraphCompletionHook {
        platform: Some("test-cli".to_string()),
        model: None,
        effort: None,
        prompt: Some(format!("touch \"{}\"", marker.display())),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnCompleted,
        vec![failing, passing],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);
    drop(fake_home);

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);
    assert!(
        marker.exists(),
        "second hook must run despite first failing"
    );
    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 2);
    assert_eq!(hook_runs[0].hook_index, 0);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Fail);
    assert_eq!(hook_runs[1].hook_index, 1);
    assert_eq!(hook_runs[1].status, GraphRunStatus::Pass);
}

/// CH1: a hook registered after its event already fired does not run.
#[tokio::test]
async fn graph_engine_hook_registered_after_completion_does_not_fire() {
    let fake_home = setup_test_cli_home();
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
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

    // Complete first with no hooks configured.
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    assert!(db
        .list_graph_completion_hook_runs(&graph_id)
        .unwrap()
        .is_empty());

    // Register after the fact — must not fire retroactively.
    let marker = dir.path().join("late_hook.marker");
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: Some("test-cli".to_string()),
        model: None,
        effort: None,
        prompt: Some(format!("touch \"{}\"", marker.display())),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnCompleted,
        vec![hook],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let _home = HomeGuard::set(fake_home.path());
    drop(_home);
    drop(fake_home);
    // No further dispatch happens here; the registration itself must not
    // create a run.
    assert!(db
        .list_graph_completion_hook_runs(&graph_id)
        .unwrap()
        .is_empty());
    assert!(!marker.exists());
}

/// CH3: firing an interactive hook inserts one scheduled send addressed
/// to the configured session, with the rendered prompt and a populated,
/// restorable builder state.
#[tokio::test]
async fn interactive_hook_enqueues_rendered_scheduled_send() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let workdir = db.get_graph(&graph_id).unwrap().unwrap().workdir.clone();
    db.insert_interactive_session(
        "session-live",
        "operator",
        "claude",
        &workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    let hook = interactive_hook_fixture(
        "session-live",
        "Graph {{graph_name}} failed: {{blocker}} on {{node}}",
    );
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(crate::domain::graphs::GraphHookEvent::OnFailed, vec![hook]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let ctx = HookContext {
        graph_name: &lp.name,
        workdir: &lp.workdir,
        completed_specs: &[],
        spec_name: None,
        spec_id: None,
        blocker: Some("build broke"),
        node_name: Some("builder"),
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    engine
        .fire_hooks(&lp, crate::domain::graphs::GraphHookEvent::OnFailed, &ctx)
        .await;

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Pass);

    let due = db.list_due_scheduled_sends(chrono::Utc::now()).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].target_session_id, "session-live");
    assert_eq!(due[0].prompt, "Graph Graph failed: build broke on builder");
    let state_json = due[0]
        .builder_state
        .as_deref()
        .expect("builder_state populated");
    assert!(!state_json.is_empty());
    let state: crate::tui::PersistedBuilderState =
        serde_json::from_str(state_json).expect("builder state deserializes");
    let mut dialog = crate::tui::SimplePromptDialog::new();
    state.restore_into(&mut dialog);
    assert_eq!(
        dialog.get_section_content("instruction_1"),
        "Graph Graph failed: build broke on builder"
    );
}

/// CH3: the enqueued message carries its provenance — graph and event —
/// as structure, not as prose inside the prompt.
#[tokio::test]
async fn interactive_hook_persists_structured_provenance() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let workdir = db.get_graph(&graph_id).unwrap().unwrap().workdir.clone();
    db.insert_interactive_session(
        "session-live",
        "operator",
        "claude",
        &workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    let hook = interactive_hook_fixture("session-live", "Graph {{graph_name}} finished");
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnCompleted,
        vec![hook],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let completed = vec![("Spec".to_string(), "ok".to_string())];
    let ctx = HookContext {
        graph_name: &lp.name,
        workdir: &lp.workdir,
        completed_specs: &completed,
        spec_name: None,
        spec_id: None,
        blocker: None,
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    engine
        .fire_hooks(
            &lp,
            crate::domain::graphs::GraphHookEvent::OnCompleted,
            &ctx,
        )
        .await;

    let due = db.list_due_scheduled_sends(chrono::Utc::now()).unwrap();
    assert_eq!(due.len(), 1);
    let provenance = due[0].provenance.as_ref().expect("provenance populated");
    assert_eq!(provenance.kind, "hook");
    assert_eq!(provenance.graph_id, graph_id);
    assert_eq!(provenance.event, "on_completed");
    // Origin stays out of the delivered text.
    assert!(!due[0].prompt.contains(&graph_id));
    assert!(!due[0].prompt.contains("on_completed"));
}

/// CH3: a hook targeting an unknown or dead session fails with the id in
/// the message, inserts nothing, and leaves the graph's status unchanged.
#[tokio::test]
async fn interactive_hook_missing_target_fails_without_graph_status_change() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let status_before = db.get_graph(&graph_id).unwrap().unwrap().status;

    let hook = interactive_hook_fixture("session-gone", "Graph {{graph_name}} finished");
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnCompleted,
        vec![hook],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let completed = vec![("Spec".to_string(), "ok".to_string())];
    let ctx = HookContext {
        graph_name: &lp.name,
        workdir: &lp.workdir,
        completed_specs: &completed,
        spec_name: None,
        spec_id: None,
        blocker: None,
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    engine
        .fire_hooks(
            &lp,
            crate::domain::graphs::GraphHookEvent::OnCompleted,
            &ctx,
        )
        .await;

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Fail);
    let summary = hook_runs[0].summary.as_deref().unwrap_or("");
    assert!(
        summary.contains("session-gone"),
        "failure must name the session id, got: {summary}"
    );
    assert!(db
        .list_due_scheduled_sends(chrono::Utc::now())
        .unwrap()
        .is_empty());
    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        status_before
    );
}

/// CH3: a message enqueued with no TUI running stays pending — the
/// engine only enqueues, so the due row must still be there (not failed)
/// after firing returns.
#[tokio::test]
async fn interactive_hook_is_pending_without_tui() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let workdir = db.get_graph(&graph_id).unwrap().unwrap().workdir.clone();
    db.insert_interactive_session(
        "session-live",
        "operator",
        "claude",
        &workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    let hook = interactive_hook_fixture("session-live", "Graph {{graph_name}} finished");
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnCompleted,
        vec![hook],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let completed = vec![("Spec".to_string(), "ok".to_string())];
    let ctx = HookContext {
        graph_name: &lp.name,
        workdir: &lp.workdir,
        completed_specs: &completed,
        spec_name: None,
        spec_id: None,
        blocker: None,
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    // No TUI delivery object exists anywhere in this test — firing only
    // enqueues.
    engine
        .fire_hooks(
            &lp,
            crate::domain::graphs::GraphHookEvent::OnCompleted,
            &ctx,
        )
        .await;

    let due = db.list_due_scheduled_sends(chrono::Utc::now()).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].target_session_id, "session-live");
    assert!(db
        .list_failed_scheduled_sends_for_workdir(&workdir)
        .unwrap()
        .is_empty());
}

/// CM27: a name-targeted interactive hook resolves the live session's
/// current id at fire time and delivers there, recording both the name
/// it was aimed at and the id it resolved to.
#[tokio::test]
async fn interactive_hook_name_resolves_single_live_match() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let workdir = db.get_graph(&graph_id).unwrap().unwrap().workdir.clone();
    db.insert_interactive_session(
        "dc4aa96d-live",
        "marasmius",
        "claude",
        &workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    let hook = interactive_hook_name_fixture(
        "marasmius",
        "Graph {{graph_name}} failed: {{blocker}} on {{node}}",
    );
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(crate::domain::graphs::GraphHookEvent::OnFailed, vec![hook]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let ctx = HookContext {
        graph_name: &lp.name,
        workdir: &lp.workdir,
        completed_specs: &[],
        spec_name: None,
        spec_id: None,
        blocker: Some("build broke"),
        node_name: Some("builder"),
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    engine
        .fire_hooks(&lp, crate::domain::graphs::GraphHookEvent::OnFailed, &ctx)
        .await;

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Pass);

    let due = db.list_due_scheduled_sends(chrono::Utc::now()).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].target_session_id, "dc4aa96d-live");
    let provenance = due[0].provenance.as_ref().expect("provenance populated");
    assert_eq!(provenance.target_session_name.as_deref(), Some("marasmius"));
}

/// FR6: a hook addressed to the name of a session that ended and was then
/// resumed is delivered to the resumed process. The first fire reproduces
/// the incident exactly (the finished session is not live: the hook fails
/// naming the name, nothing enqueued); the resume of the same harness
/// conversation reactivates the row under its kept id, and the second fire
/// enqueues to that same id — never to a freshly minted one.
#[tokio::test]
async fn interactive_hook_delivers_to_a_resumed_session_by_name() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let workdir = db.get_graph(&graph_id).unwrap().unwrap().workdir.clone();
    db.insert_interactive_session(
        "sess-orch",
        "marasmius",
        "claude",
        &workdir,
        Some("--continue"),
        Some(111),
        "interactive",
        Some("boot-old"),
    )
    .unwrap();
    db.set_session_harness_conversation_id("sess-orch", "conv-abc")
        .unwrap();
    db.finish_interactive_session("sess-orch", 0).unwrap();

    let hook = interactive_hook_name_fixture("marasmius", "Graph {{graph_name}} finished");
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnCompleted,
        vec![hook],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let completed = vec![("Spec".to_string(), "ok".to_string())];
    let ctx = HookContext {
        graph_name: &lp.name,
        workdir: &lp.workdir,
        completed_specs: &completed,
        spec_name: None,
        spec_id: None,
        blocker: None,
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };

    // Finished session: the hook fails with the incident's exact wording.
    engine
        .fire_hooks(
            &lp,
            crate::domain::graphs::GraphHookEvent::OnCompleted,
            &ctx,
        )
        .await;
    let runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, GraphRunStatus::Fail);
    let summary = runs[0].summary.as_deref().unwrap_or("");
    assert!(
        summary.contains("does not exist or is no longer live"),
        "got: {summary}"
    );
    assert!(db
        .list_due_scheduled_sends(chrono::Utc::now())
        .unwrap()
        .is_empty());

    // The resume of that harness conversation keeps the row's id and name.
    let outcome = db
        .register_interactive_session(&crate::db::session::SessionRegistration {
            id: "fresh-uuid",
            name: "populus",
            cli: "claude",
            working_dir: &workdir,
            args: Some("--continue"),
            pid: Some(4242),
            session_type: "interactive",
            boot_id: Some("boot-new"),
            harness_conversation_id: Some("conv-abc"),
        })
        .unwrap();
    assert_eq!(
        outcome,
        crate::db::session::RegistrationOutcome::Reactivated {
            id: "sess-orch".to_string(),
            name: "marasmius".to_string(),
        }
    );

    // Same hook, same name: now it delivers — to the kept id.
    engine
        .fire_hooks(
            &lp,
            crate::domain::graphs::GraphHookEvent::OnCompleted,
            &ctx,
        )
        .await;
    let runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(
        runs[1].status,
        GraphRunStatus::Pass,
        "{:?}",
        runs[1].summary
    );
    let due = db.list_due_scheduled_sends(chrono::Utc::now()).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(
        due[0].target_session_id, "sess-orch",
        "delivery must land on the resumed session's kept id"
    );
    let provenance = due[0].provenance.as_ref().expect("provenance populated");
    assert_eq!(provenance.target_session_name.as_deref(), Some("marasmius"));
}

/// CM27: a name-targeted hook with no live match fails naming the name,
/// enqueues nothing, and does not fall back to any other session.
#[tokio::test]
async fn interactive_hook_name_fails_when_no_live_match() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();

    let hook = interactive_hook_name_fixture("ghost", "Graph {{graph_name}} finished");
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnCompleted,
        vec![hook],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let status_before = lp.status;
    let completed = vec![("Spec".to_string(), "ok".to_string())];
    let ctx = HookContext {
        graph_name: &lp.name,
        workdir: &lp.workdir,
        completed_specs: &completed,
        spec_name: None,
        spec_id: None,
        blocker: None,
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    engine
        .fire_hooks(
            &lp,
            crate::domain::graphs::GraphHookEvent::OnCompleted,
            &ctx,
        )
        .await;

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Fail);
    let summary = hook_runs[0].summary.as_deref().unwrap_or("");
    assert!(
        summary.contains("ghost"),
        "failure must name the session name, got: {summary}"
    );
    assert!(db
        .list_due_scheduled_sends(chrono::Utc::now())
        .unwrap()
        .is_empty());
    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        status_before
    );
}

/// CM27: two live sessions sharing a name is refused rather than
/// guessing which one was meant — the failure lists both ids.
#[tokio::test]
async fn interactive_hook_name_fails_when_multiple_live_matches() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let workdir = db.get_graph(&graph_id).unwrap().unwrap().workdir.clone();
    db.insert_interactive_session(
        "twin-a",
        "twin",
        "claude",
        &workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.insert_interactive_session(
        "twin-b",
        "twin",
        "claude",
        &workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    let hook = interactive_hook_name_fixture("twin", "Graph {{graph_name}} finished");
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnCompleted,
        vec![hook],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let completed = vec![("Spec".to_string(), "ok".to_string())];
    let ctx = HookContext {
        graph_name: &lp.name,
        workdir: &lp.workdir,
        completed_specs: &completed,
        spec_name: None,
        spec_id: None,
        blocker: None,
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    engine
        .fire_hooks(
            &lp,
            crate::domain::graphs::GraphHookEvent::OnCompleted,
            &ctx,
        )
        .await;

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Fail);
    let summary = hook_runs[0].summary.as_deref().unwrap_or("");
    assert!(summary.contains("twin-a"), "got: {summary}");
    assert!(summary.contains("twin-b"), "got: {summary}");
    assert!(db
        .list_due_scheduled_sends(chrono::Utc::now())
        .unwrap()
        .is_empty());
}

/// CM27: an id-configured hook is byte-for-byte unaffected by name
/// resolution — its provenance carries no target_session_name.
#[tokio::test]
async fn interactive_hook_id_based_hook_unaffected_by_name_resolution() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let workdir = db.get_graph(&graph_id).unwrap().unwrap().workdir.clone();
    db.insert_interactive_session(
        "session-live",
        "operator",
        "claude",
        &workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    let hook = interactive_hook_fixture(
        "session-live",
        "Graph {{graph_name}} failed: {{blocker}} on {{node}}",
    );
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(crate::domain::graphs::GraphHookEvent::OnFailed, vec![hook]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let ctx = HookContext {
        graph_name: &lp.name,
        workdir: &lp.workdir,
        completed_specs: &[],
        spec_name: None,
        spec_id: None,
        blocker: Some("build broke"),
        node_name: Some("builder"),
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    engine
        .fire_hooks(&lp, crate::domain::graphs::GraphHookEvent::OnFailed, &ctx)
        .await;

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Pass);

    let due = db.list_due_scheduled_sends(chrono::Utc::now()).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].target_session_id, "session-live");
    let provenance = due[0].provenance.as_ref().expect("provenance populated");
    assert!(provenance.target_session_name.is_none());
}

// ── CH2: command hook tests ────────────────────────────────────────

/// A command hook on `on_spec_completed` runs once per spec, in the
/// graph's workdir.
#[tokio::test]
async fn command_hook_on_spec_completed_runs_once_per_spec() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    // Second spec with its own check node.
    let spec2 = crate::domain::graphs::GraphSpec {
            id: "spec-test-2".to_string(),
            graph_id: Some(graph_id.clone()),
            name: "Spec2".to_string(),
            description: Some(
                "Functional Requirements:\n- A\n\nNon-Functional Requirements:\n- B\n\nObjective:\n- C\n\nConstraints:\n- D\n\nGuidelines:\n- E\n\nIn Scope:\n- F\n\nOut of Scope:\n- G".to_string(),
            ),
            position: 2,
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
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        };
    db.insert_graph_spec(&spec2).unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check-1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check1".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check-2".to_string(),
        spec_id: Some("spec-test-2".to_string()),
        graph_id: None,
        name: "check2".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // Command hook: touch a marker file named after the spec.
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some("touch {{spec_name}}.marker".to_string()),
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnSpecCompleted,
        vec![hook],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    result.unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);

    let spec1_marker = dir.path().join("Spec.marker");
    let spec2_marker = dir.path().join("Spec2.marker");
    assert!(
        spec1_marker.exists(),
        "on_spec_completed hook must have run for spec1"
    );
    assert!(
        spec2_marker.exists(),
        "on_spec_completed hook must have run for spec2"
    );

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 2, "hook must fire once per spec");
    assert!(hook_runs.iter().all(|r| r.status == GraphRunStatus::Pass));
}

/// A command hook exiting non-zero while writing to stdout and stderr
/// records all three, and the graph's status is untouched.
#[tokio::test]
async fn command_hook_nonzero_exit_records_output_and_does_not_affect_graph() {
    let fake_home = setup_test_cli_home();
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
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

    // Command hook that exits non-zero and writes to both streams.
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some("echo out; echo err >&2; exit 3".to_string()),
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnCompleted,
        vec![hook],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    drop(fake_home);
    result.unwrap();

    // Graph completed successfully despite hook failure.
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Fail);

    let output = hook_runs[0].output.as_ref().unwrap();
    assert_eq!(output["exit_code"], 3);
    assert_eq!(output["stdout"], "out");
    assert_eq!(output["stderr"], "err");
    assert_eq!(output["command"], "echo out; echo err >&2; exit 3");
}

/// A placeholder in the command is substituted before execution.
#[tokio::test]
async fn command_hook_placeholder_substitution() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.update_graph_spec_details(&spec_id, Some("A live session's id"), None, None, None)
        .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
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

    let output_file = dir.path().join("placeholder_output");
    let output_path = output_file.to_string_lossy().to_string();
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some(format!(
            "printf '%s' '{{{{spec_name}}}}' > \"{}\"",
            output_path
        )),
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnSpecCompleted,
        vec![hook],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    result.unwrap();

    let content = std::fs::read_to_string(&output_file).unwrap();
    assert_eq!(
        content, "A live session's id",
        "placeholder must be substituted"
    );
}

#[tokio::test]
async fn command_hook_graph_failure_without_node_renders_none_and_blocker() {
    let (dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let output_file = dir.path().join("graph_failure_hook_output");
    let output_path = output_file.to_string_lossy().to_string();
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some(format!(
            "printf '%s %s' {{{{node}}}} {{{{blocker}}}} > \"{}\"",
            output_path
        )),
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(GraphHookEvent::OnFailed, vec![hook]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let graph = db.get_graph(&graph_id).unwrap().unwrap();
    let ctx = HookContext {
        graph_name: &graph.name,
        workdir: &graph.workdir,
        completed_specs: &[],
        spec_name: None,
        spec_id: None,
        blocker: Some("Reviewer join has ambiguous outgoing edges"),
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    engine
        .fire_hooks(&graph, GraphHookEvent::OnFailed, &ctx)
        .await;

    assert_eq!(
        std::fs::read_to_string(output_file).unwrap(),
        "(none) Reviewer join has ambiguous outgoing edges"
    );
    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Pass);
}

#[tokio::test]
async fn command_hook_env_var_preserves_shell_metacharacters() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let spec_name = "x$(echo pwned)";
    db.update_graph_spec_details(&spec_id, Some(spec_name), None, None, None)
        .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
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

    let output_file = dir.path().join("env_output");
    let output_path = output_file.to_string_lossy().to_string();
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some(format!(
            "printf '%s' \"$CANOPY_HOOK_SPEC_NAME\" > \"{}\"",
            output_path
        )),
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(GraphHookEvent::OnSpecCompleted, vec![hook]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(output_file).unwrap(), spec_name);
    assert!(!dir.path().join("pwned").exists());
    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Pass);
    assert_eq!(hook_runs[0].output.as_ref().unwrap()["exit_code"], 0);
}

/// Process group children must die with the parent: spawn a check node
/// that forks a grandchild via `sh -c` subshell, then verify the
/// grandchild is gone after the check is terminated.
#[cfg(unix)]
#[tokio::test]
async fn process_group_children_die_with_parent() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let marker = dir.path().join("grandchild_alive");

    // The check node forks a grandchild that sleeps and touches a
    // marker file. If the process group kill works, the grandchild
    // dies before the marker appears.
    db.insert_graph_node(&GraphNode {
        id: "check-pg".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check-pg".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": format!(
                "( sleep 60; touch \"{}\" ) & exit 1",
                marker.display()
            ),
            "success_condition": "exit_code_0",
            "timeout_seconds": 3,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    // Wait for the grace period + a bit extra for the grandchild.
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;

    assert!(
        !marker.exists(),
        "grandchild process should have been killed by process-group termination; \
             marker file should not exist"
    );
}

/// CB57 (FR5): the re-executed verdict writer. A fake member script
/// re-runs the test binary itself (`self_report_exe`, filtered with
/// `--exact graph_engine::tests::fixtures_a::fixtures_b::fixtures_c::part10_tests::self_report_verdict_writer
/// --nocapture` — see `self_report_invocation`) and this test files
/// that member's own self-report straight into the run's database
/// using the real `get_active_graph_run_for_node` +
/// `update_graph_run_result` — the same calls `graph_complete_node`
/// makes; no SQL is hand-mirrored. Node id, status and stdout come
/// from `CANOPY_TEST_SELF_REPORT_NODE_ID`, `_STATUS` and `_STDOUT`
/// (when `_STDOUT` is set — even to the empty string — its value is
/// filed as the row's `stdout`, matching the old binary's shape). With
/// `CANOPY_TEST_SELF_REPORT_DB` unset — every ordinary suite run —
/// this test returns immediately.
///
/// The ordering guarantee it inherits from the deleted binary: the
/// engine always inserts the run row with status `running` before it
/// spawns the member process, and never reads that row back until the
/// process has exited — so a write done here, inside the grandchild
/// the member's script waits on, is ordered before the engine's read
/// by process-exit semantics, not by winning a race against a timer.
/// Panics if no running row exists: that would mean the ordering
/// above broke — a real defect to see in test output, not something
/// to retry past.
#[test]
fn self_report_verdict_writer() {
    let Some(db_path) = std::env::var_os("CANOPY_TEST_SELF_REPORT_DB") else {
        return;
    };
    let node_id = std::env::var("CANOPY_TEST_SELF_REPORT_NODE_ID")
        .expect("a script that sets CANOPY_TEST_SELF_REPORT_DB also sets the node id");
    let status = match std::env::var("CANOPY_TEST_SELF_REPORT_STATUS")
        .expect("a script that sets CANOPY_TEST_SELF_REPORT_DB also sets the status")
        .as_str()
    {
        "pass" => GraphRunStatus::Pass,
        "fail" => GraphRunStatus::Fail,
        other => panic!("self_report_verdict_writer: bad status {other:?}"),
    };
    let stdout_text = std::env::var("CANOPY_TEST_SELF_REPORT_STDOUT").ok();

    let file_verdict = || -> Result<()> {
        let db = Database::new(&std::path::PathBuf::from(&db_path))?;
        let run = db
            .get_active_graph_run_for_node(&node_id)?
            .ok_or_else(|| anyhow!("no running graph_run row for node {node_id:?}"))?;
        let mut output = serde_json::json!({ "test_self_report": true });
        if let Some(text) = &stdout_text {
            output["stdout"] = Value::String(text.clone());
        }
        let updated = db.update_graph_run_result(
            run.id.as_str(),
            status,
            Some(&output),
            Some(chrono::Utc::now()),
        )?;
        if !updated {
            bail!(
                "graph_run row {} vanished between lookup and update",
                run.id
            );
        }
        Ok(())
    };

    // Deviation D1: the deleted binary opened its own connection with
    // a 5 s SQLite busy timeout; `Database::new` sets none, and its
    // open-time seeding performs short write transactions that can
    // collide with the engine's writes under a loaded suite. Retry
    // ONLY on "database is locked" (SQLITE_BUSY), waiting — this is
    // the busy handler, not a verdict retry: the filing still happens
    // before the member process exits (what orders it against the
    // engine's read), a missing running row fails on the first
    // attempt, and any other error panics immediately. Budget ~5 s.
    for attempt in 0..50u32 {
        match file_verdict() {
            Ok(()) => return,
            Err(e) if attempt < 49 && e.to_string().contains("database is locked") => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => panic!("self_report_verdict_writer: {e}"),
        }
    }
}
