//! Part 02 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

/// The commit-rights blocker states what canopy observed — HEAD moved
/// from h0 to h1 while the node ran — and lists the new commits so a
/// commit belonging to another process (a leftover session, a hook, a
/// human) is visible at once. It never claims the node committed: the
/// engine cannot know who moved HEAD.
#[tokio::test]
async fn commit_rights_blocker_lists_commits() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let h0 = git_head(dir.path());
    empty_commit(dir.path(), "alpha change", "2026-10-16T10:00:00Z");
    empty_commit(dir.path(), "beta change", "2026-10-17T11:00:00Z");
    let h1 = git_head(dir.path());

    let exec = commit_rights_failure(
        "Node 'worker'",
        "node-1",
        &h0,
        &h1,
        dir.path().to_str().unwrap(),
        serde_json::json!({}),
    )
    .await;

    db.insert_graph_node(&rights_node(&spec_id, "worker", "true", None, 1))
        .unwrap();
    let run_id = "run-head-move".to_string();
    db.insert_graph_run(&GraphNodeRun {
        id: run_id.clone(),
        graph_id: "wf-test".to_string(),
        spec_id,
        node_id: "worker".to_string(),
        status: GraphRunStatus::Fail,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();
    engine
        .record_terminal_blocker(&run_id, "worker", &exec)
        .unwrap();

    let run = db.get_graph_run(&run_id).unwrap().unwrap();
    let blocker = run
        .output
        .as_ref()
        .and_then(|output| output.get("blocker"))
        .and_then(Value::as_str)
        .unwrap();
    assert!(
        blocker.contains(&commit_line(dir.path(), "HEAD~1", "alpha change")),
        "blocker must list the first new commit: {blocker}"
    );
    assert!(
        blocker.contains(&commit_line(dir.path(), "HEAD", "beta change")),
        "blocker must list the second new commit: {blocker}"
    );
    assert!(
        blocker.contains(&format!(
            "Node 'worker': HEAD moved from {h0} to {h1} while it ran"
        )),
        "the blocker must state the observed move while that node ran: {blocker}"
    );
    assert!(
        blocker.contains("may come from this node or from another process"),
        "the blocker must leave open who moved HEAD: {blocker}"
    );
    assert!(
        blocker.contains("commit_rights: true") && blocker.contains("left in place"),
        "the existing guidance must stay: {blocker}"
    );
    assert!(
        !blocker.contains("committed"),
        "the blocker must not claim the node committed: {blocker}"
    );

    let message = exec
        .output
        .get("commit_rights_violation")
        .and_then(|v| v.get("message"))
        .and_then(Value::as_str)
        .unwrap();
    assert!(
        message.contains(&format!(
            "Node 'worker': HEAD moved from {h0} to {h1} while it ran"
        )),
        "the run's own output must state which node was running: {message}"
    );
    assert!(
        !message.contains("committed"),
        "the violation message must not claim the node committed: {message}"
    );
}

/// Seven commits between the HEADs: the listing caps at 5 and reports
/// the remainder as `and 2 more`, so a long range never buries the
/// blocker — and the capped-out commits are never named.
#[tokio::test]
async fn commit_list_caps_at_five() {
    let (dir, _db, _engine, _graph_id, _spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let h0 = git_head(dir.path());
    for n in 1..=7 {
        empty_commit(dir.path(), &format!("commit-{n}"), "2026-10-16T12:00:00Z");
    }
    let h1 = git_head(dir.path());

    let exec = commit_rights_failure(
        "Node 'worker'",
        "node-1",
        &h0,
        &h1,
        dir.path().to_str().unwrap(),
        serde_json::json!({}),
    )
    .await;

    let message = exec
        .output
        .get("commit_rights_violation")
        .and_then(|v| v.get("message"))
        .and_then(Value::as_str)
        .unwrap();
    assert!(message.contains("commit-7 ("), "newest listed: {message}");
    assert!(
        message.contains("commit-3 ("),
        "5th newest listed: {message}"
    );
    assert!(
        message.contains("and 2 more"),
        "remainder reported: {message}"
    );
    assert!(!message.contains("commit-2"), "capped out: {message}");
    assert!(!message.contains("commit-1"), "capped out: {message}");
    assert!(
        !message.contains("committed"),
        "the message must not claim the node committed: {message}"
    );
}

#[tokio::test]
async fn designated_committer_that_commits_passes() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());

    db.insert_graph_node(&rights_node(
        &spec_id,
        "committer",
        COMMIT_CMD,
        Some(true),
        1,
    ))
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
    assert!(runs[0]
        .output
        .as_ref()
        .is_none_or(|o| o.get("commit_rights_violation").is_none()));
}

/// CM28 / NFR1(b): an ensemble member's commit must pass the B37 watch
/// once the ensemble itself is the designated committer. Two members are
/// used (rather than one) because `select_next_step` only resolves to
/// `SpecCursor::Ensemble` when the entry edge fans out to more than one
/// distinct target — a single-member roster would instead execute its
/// one member as an ordinary node, never exercising the ensemble commit
/// watch this test targets.
#[tokio::test]
async fn ensemble_member_commit_passes_when_ensemble_holds_commit_rights() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let head_before = git_head(dir.path());

    let workdir = dir.path().to_string_lossy().to_string();
    let fake_home = setup_multi_cli_home(&[
        (
            "member-committer",
            // The fake CLI declares `supports_working_dir: false`, so the
            // member inherits the test runner's cwd (the harness-canopy
            // checkout, whose gitkit hook blocks foreign commits) — target
            // this test's repo explicitly with `git -C`.
            &write_member_script(
                dir.path(),
                "member.sh",
                &format!("git -C '{workdir}' commit -q --allow-empty -m member; printf ok; exit 0"),
            ),
        ),
        (
            "member-plain",
            &write_member_script(dir.path(), "plain.sh", "printf ok; exit 0"),
        ),
    ]);

    db.insert_graph_node(&rights_node(&spec_id, "done", "true", None, 100))
        .unwrap();
    insert_test_ensemble(
        &db,
        &spec_id,
        "kickoff",
        "ens1",
        "join1",
        &[("m1", "member-committer"), ("m2", "member-plain")],
        1,
        None,
        None,
        "done",
        None,
    );
    db.update_ensemble_commit_rights("ens1", true).unwrap();

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    assert_ne!(
        git_head(dir.path()),
        head_before,
        "the member must really have committed"
    );
    let join = join_run(&db, &spec_id, "join1");
    assert!(
        join.output
            .as_ref()
            .is_none_or(|o| o.get("commit_rights_violation").is_none()),
        "a member of a commit-rights ensemble must not be flagged"
    );
}

/// CM28 / NFR1(c): with an ensemble holding commit rights, a plain node
/// outside the ensemble that commits is still flagged and routed via the
/// fail edge — the ensemble's right does not leak to unrelated nodes.
#[tokio::test]
async fn non_member_commit_flagged_when_only_an_ensemble_holds_commit_rights() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let head_before = git_head(dir.path());

    db.insert_graph_node(&rights_node(&spec_id, "worker", COMMIT_CMD, None, 0))
        .unwrap();
    db.insert_graph_node(&rights_node(&spec_id, "triage", "true", None, 100))
        .unwrap();
    db.insert_graph_node(&rights_node(&spec_id, "ens_done", "true", None, 4))
        .unwrap();

    insert_test_ensemble(
        &db,
        &spec_id,
        "ens_kickoff",
        "ens1",
        "ens_join",
        &[("m1", "unused")],
        1,
        None,
        None,
        "ens_done",
        None,
    );
    db.update_ensemble_commit_rights("ens1", true).unwrap();

    db.insert_graph_edge(&GraphEdge {
        id: "e-pass".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "worker".to_string(),
        to_node: "ens_kickoff".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "worker".to_string(),
        to_node: "triage".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let worker = runs.iter().find(|r| r.node_id == "worker").unwrap();
    assert_eq!(
        worker.status,
        GraphRunStatus::Fail,
        "committing without commit rights must be a deterministic fail"
    );
    assert!(
        worker
            .output
            .as_ref()
            .and_then(|o| o.get("commit_rights_violation"))
            .is_some(),
        "the violation must be recorded on the run's output"
    );
    assert!(
        runs.iter().any(|r| r.node_id == "triage"),
        "the fail edge must have been taken"
    );
    assert!(
        !runs
            .iter()
            .any(|r| r.node_id == "ens_kickoff" || r.node_id == "m1" || r.node_id == "ens_join"),
        "the ensemble must never have been reached"
    );
    assert_ne!(
        git_head(dir.path()),
        head_before,
        "the illegal commit is reported, never rewritten away"
    );
}

#[tokio::test]
async fn node_that_changes_files_without_committing_is_unaffected() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let head_before = git_head(dir.path());

    db.insert_graph_node(&rights_node(
        &spec_id,
        "worker",
        "printf changed > README.md",
        None,
        1,
    ))
    .unwrap();
    db.insert_graph_node(&rights_node(&spec_id, "committer", "true", Some(true), 2))
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-pass".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "worker".to_string(),
        to_node: "committer".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let worker = runs.iter().find(|r| r.node_id == "worker").unwrap();
    assert_eq!(worker.status, GraphRunStatus::Pass);
    assert_eq!(git_head(dir.path()), head_before);
}

#[tokio::test]
async fn graph_designating_no_committer_keeps_todays_behavior() {
    // Enforcement is opt-in per graph: without a single `commit_rights`
    // node there is no way to tell the designated committer from a
    // violator, so an existing graph must behave exactly as before.
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());

    db.insert_graph_node(&rights_node(&spec_id, "worker", COMMIT_CMD, None, 1))
        .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
}

#[tokio::test]
async fn commit_rights_enforcement_skips_non_git_workdirs() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&rights_node(&spec_id, "worker", "true", None, 1))
        .unwrap();
    db.insert_graph_node(&rights_node(&spec_id, "committer", "true", Some(true), 2))
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-pass".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "worker".to_string(),
        to_node: "committer".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Completed);
}

#[test]
fn commit_rights_are_explicit_configuration_only() {
    let named_committer = rights_node("s", "Commit and push", "true", None, 1);
    assert!(
        !node_has_commit_rights(&named_committer, &[]),
        "a node's name must never grant it commit rights"
    );
    assert!(!graph_enforces_commit_rights(
        std::slice::from_ref(&named_committer),
        &[]
    ));

    let designated = rights_node("s", "committer", "true", Some(true), 1);
    assert!(node_has_commit_rights(&designated, &[]));
    assert!(graph_enforces_commit_rights(
        &[named_committer, designated],
        &[]
    ));

    assert!(!node_has_commit_rights(
        &rights_node("s", "n", "true", Some(false), 1),
        &[]
    ));
}

#[test]
fn graph_enforces_commit_rights_when_only_an_ensemble_carries_it() {
    let ensemble = crate::domain::graphs::Ensemble {
        mutex: Vec::new(),
        id: "ens1".to_string(),
        spec_id: Some("s".to_string()),
        graph_id: None,
        name: "Test Ensemble".to_string(),
        prompt_template: "p".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: crate::domain::graphs::GraphEdgeCondition::Always,
        min_pass: 1,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 5,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "done".to_string(),
        on_fail_to: None,
        kind: crate::domain::graphs::EnsembleKind::Parallel,
        round_robin_index: None,
        commit_rights: true,
        created_at: chrono::Utc::now(),

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let member = EnsembleMember {
        ensemble_id: "ens1".to_string(),
        node_id: "m1".to_string(),
        position: 0,
        platform: "claude".to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    };
    let details = EnsembleDetails {
        ensemble,
        members: vec![member],
    };

    let nodes = vec![
        rights_node("s", "m1", "true", None, 1),
        rights_node("s", "other", "true", None, 2),
    ];

    assert!(graph_enforces_commit_rights(
        &nodes,
        std::slice::from_ref(&details)
    ));
    assert!(
        node_has_commit_rights(&nodes[0], std::slice::from_ref(&details)),
        "a member resolves commit rights through its owning ensemble"
    );
    assert!(
        !node_has_commit_rights(&nodes[1], std::slice::from_ref(&details)),
        "an unrelated node must not inherit another ensemble's commit rights"
    );
}

// ── CB39: spec_start_head ancestry verification ──────────────────────

#[tokio::test]
async fn committer_that_amends_previous_commit_fails_as_infrastructure() {
    // Two committing nodes in one spec: the second rewrites history so
    // spec_start_head is no longer reachable. The ancestry check must
    // fail the second node's run as infrastructure and route via Error.
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());

    // committer1: commits normally (pass edge -> committer2)
    db.insert_graph_node(&rights_node(
        &spec_id,
        "committer1",
        "git commit -q --allow-empty -m 'spec work'",
        Some(true),
        1,
    ))
    .unwrap();
    // committer2: create a disconnected root commit so spec_start_head
    // (captured before committer1 ran) is no longer reachable.
    db.insert_graph_node(&rights_node(
            &spec_id,
            "committer2",
            "git checkout --orphan fresh && git rm -rf . && echo disconnected > file.txt && git add . && git commit -m 'disconnected root' && git checkout -B main && git checkout main",
            Some(true),
            2,
        ))
        .unwrap();
    // triage: reached via Error edge from committer2
    db.insert_graph_node(&rights_node(&spec_id, "triage", "true", None, 3))
        .unwrap();
    // done: reached via Pass edge from committer2 (should NOT be reached)
    db.insert_graph_node(&rights_node(&spec_id, "done", "true", None, 4))
        .unwrap();

    db.insert_graph_edge(&GraphEdge {
        id: "e-pass-12".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "committer1".to_string(),
        to_node: "committer2".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-pass-2d".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "committer2".to_string(),
        to_node: "done".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-error-2t".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "committer2".to_string(),
        to_node: "triage".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Error,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();

    // committer1 passed normally
    let c1 = runs.iter().find(|r| r.node_id == "committer1").unwrap();
    assert_eq!(c1.status, GraphRunStatus::Pass);

    // committer2 failed — ancestry violation
    let c2 = runs.iter().find(|r| r.node_id == "committer2").unwrap();
    assert_eq!(c2.status, GraphRunStatus::Fail);
    let output = c2.output.as_ref().unwrap();
    let violation = output.get("ancestry_violation").unwrap();
    assert!(
        violation
            .get("spec_start_head")
            .and_then(|v| v.as_str())
            .is_some(),
        "violation must name the recorded spec_start_head"
    );
    assert!(
        violation
            .get("current_head")
            .and_then(|v| v.as_str())
            .is_some(),
        "violation must name the current HEAD"
    );
    assert!(
        violation
            .get("message")
            .and_then(|v| v.as_str())
            .is_some_and(|m| m.contains("no longer an ancestor")),
        "the reason must be stated in plain words"
    );

    // Error edge was taken — triage ran
    assert!(
        runs.iter().any(|r| r.node_id == "triage"),
        "the Error edge must have been taken to triage"
    );
    // done must NOT have run
    assert!(
        !runs.iter().any(|r| r.node_id == "done"),
        "the Pass edge must not have been taken to done"
    );

    // spec_committed_head must NOT have been updated to the disconnected
    // root — it still holds committer1's value (or none if committer1
    // didn't commit). The ancestry check prevents masking the problem.
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    if let Some(ref ch) = spec.spec_committed_head {
        assert_ne!(
            ch,
            &git_head(dir.path()),
            "spec_committed_head must not be the disconnected root"
        );
    }
}

#[tokio::test]
async fn committer_that_commits_normally_passes_and_records_committed_head() {
    // A single committing node that commits normally: ancestry check
    // passes, spec_committed_head is written, no ancestry_violation.
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());

    db.insert_graph_node(&rights_node(
        &spec_id,
        "committer",
        "git commit -q --allow-empty -m 'work'",
        Some(true),
        1,
    ))
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
    assert!(
        runs[0]
            .output
            .as_ref()
            .is_none_or(|o| o.get("ancestry_violation").is_none()),
        "no ancestry violation on a clean commit"
    );
    // spec_committed_head must be written
    assert!(
        spec.spec_committed_head.is_some(),
        "spec_committed_head must be recorded after a clean commit"
    );
    assert_eq!(
        spec.spec_committed_head.as_deref(),
        Some(git_head(dir.path()).as_str()),
        "spec_committed_head must match the actual HEAD"
    );
}

#[tokio::test]
async fn ancestry_check_skipped_in_non_git_workdir() {
    // Non-git workdir: ancestry check must be skipped (no error).
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&rights_node(&spec_id, "worker", "true", None, 1))
        .unwrap();
    db.insert_graph_node(&rights_node(&spec_id, "committer", "true", Some(true), 2))
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-pass".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "worker".to_string(),
        to_node: "committer".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
    assert!(
        runs[0]
            .output
            .as_ref()
            .is_none_or(|o| o.get("ancestry_violation").is_none()),
        "non-git workdir must not produce an ancestry violation"
    );
}

// ── B10: spec_start_head frozen-per-attempt, amend-proof ─────────────

#[tokio::test]
async fn graph_engine_resume_dispatch_reuses_persisted_spec_start_head_even_if_stale() {
    // `resume_background` (`graph_continue`, autorun's plain resume) is
    // the one path allowed to inherit a spec's already-persisted
    // baseline while it's still `running` — this is what makes resuming
    // a daemon-restart-interrupted spec keep comparing against the HEAD
    // it started at, not whatever HEAD happens to be at resume time.
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());

    db.update_graph_spec_status(&spec_id, GraphSpecStatus::Running, None, None)
        .unwrap();
    db.set_graph_spec_start_head(&spec_id, Some("deadbeef"))
        .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "test \"{{spec_start_head}}\" = \"deadbeef\" && printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    engine
        .run_graph_dispatch(
            graph_id.clone(),
            None,
            None,
            DispatchMode::resume(),
            None,
            None,
        )
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    assert_eq!(
        spec.spec_start_head.as_deref(),
        Some("deadbeef"),
        "a resumed dispatch must reuse the persisted baseline, not recapture"
    );
}

#[tokio::test]
async fn graph_engine_fresh_relaunch_recaptures_even_when_spec_still_shows_running() {
    // The 2026-07-11 incident: a spec left `running` by a prior,
    // never-reset attempt kept having its stale baseline reused across
    // later relaunches ("12:56 relaunch compared against b9e8928, the
    // HEAD of the ORIGINAL 07:58 launch, two relaunches earlier"). A
    // fresh dispatch — `graph_run`/`start_background_run`, including
    // relaunching a `paused` graph directly instead of via
    // `graph_continue` — must never inherit that: it re-captures
    // regardless of the spec's leftover `running` status.
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let real_head = git_head(dir.path());

    // Simulate the abandoned attempt: still `running`, with a baseline
    // that has nothing to do with the current, real HEAD.
    db.update_graph_spec_status(&spec_id, GraphSpecStatus::Running, None, None)
        .unwrap();
    db.set_graph_spec_start_head(&spec_id, Some("deadbeef"))
        .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "test \"{{spec_start_head}}\" != \"deadbeef\" && printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // `run_graph` is the fresh-dispatch entry point (same one `graph_run`
    // uses) — no `is_resume` flag reaches it.
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    assert_eq!(
        spec.spec_start_head.as_deref(),
        Some(real_head.as_str()),
        "a fresh relaunch must recapture the real current HEAD, not inherit the stale value"
    );
}

#[tokio::test]
async fn graph_engine_check_retry_baseline_stays_frozen_across_reviewer_commits() {
    // Placeholder captured at spec entry must be stable across every
    // node execution of that attempt, including check retries after
    // reviewer iterations — even while the reviewer keeps committing new
    // work in between.
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let initial_head = git_head(dir.path());
    let baseline_log = dir.path().join("baseline.log");
    let counter = dir.path().join("counter");

    // "review": always commits a bit more work and passes.
    db.insert_graph_node(&GraphNode {
            id: "node-review".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "review".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": "echo more >> work.txt && git add -A && git commit -q -m more && printf APPROVED",
                "success_condition": "exit_code_0"
            }),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    // "check": logs the substituted baseline every time it runs, and
    // only passes on its third invocation — forcing review<->check to
    // iterate a few times within the same spec attempt.
    db.insert_graph_node(&GraphNode {
            id: "node-check".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "check".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": format!(
                    "echo '{{{{spec_start_head}}}}' >> \"{log}\"; n=$(cat \"{counter}\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"{counter}\"; [ \"$n\" -ge 3 ] && printf APPROVED || exit 1",
                    log = baseline_log.display(),
                    counter = counter.display(),
                ),
                "success_condition": "exit_code_0"
            }),
            position: 2,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-review-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-review".to_string(),
        to_node: "node-check".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-check-review".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-check".to_string(),
        to_node: "node-review".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    assert_eq!(spec.spec_start_head.as_deref(), Some(initial_head.as_str()));

    let logged = std::fs::read_to_string(&baseline_log).unwrap();
    let lines: Vec<&str> = logged.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "check must have retried exactly twice before passing"
    );
    for line in lines {
        assert_eq!(
            line, initial_head,
            "the substituted baseline must never move across retries, even though \
                 the reviewer committed between every one of them"
        );
    }
}

#[tokio::test]
async fn cm15_node_redispatch_reads_config_from_db_not_launch_snapshot() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let ran_log = dir.path().join("ran.log");
    let counter = dir.path().join("cnt");

    // node-a: writes a config-derived marker each time it runs. Its command is
    // rewritten (below) between its first and second dispatch.
    db.insert_graph_node(&GraphNode {
        id: "node-a".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "a".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": format!("printf A1 >> \"{}\"", ran_log.display()),
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    // node-b: fails once (sleeping 0.5s to widen the update window), then passes.
    db.insert_graph_node(&GraphNode {
        id: "node-b".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "b".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": format!(
                "n=$(cat \"{c}\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"{c}\"; \
                 [ \"$n\" -ge 2 ] && printf APPROVED || {{ sleep 0.5; exit 1; }}",
                c = counter.display()
            ),
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "a->b".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-a".to_string(),
        to_node: "node-b".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "b->a".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-b".to_string(),
        to_node: "node-a".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    let engine = std::sync::Arc::new(engine);
    let engine2 = std::sync::Arc::clone(&engine);
    let graph_id2 = graph_id.clone();
    let handle = tokio::spawn(async move {
        engine2
            .run_graph(graph_id2, None, None, None, None, None)
            .await
    });

    // Wait until node-a has completed exactly once, then rewrite its command —
    // this is the `graph_update_node` between two dispatches. Full-config replace,
    // matching update_graph_node_details semantics.
    loop {
        let done = db
            .list_graph_runs_for_spec(&spec_id)
            .unwrap()
            .iter()
            .filter(|r| r.node_id == "node-a" && r.status != GraphRunStatus::Running)
            .count();
        if done >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    db.update_graph_node_details(
        "node-a",
        None,
        None,
        Some(&serde_json::json!({
            "command": format!("printf A2 >> \"{}\"", ran_log.display()),
            "success_condition": "exit_code_0"
        })),
        None,
    )
    .unwrap();

    handle.await.unwrap().unwrap();

    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().status,
        GraphSpecStatus::Completed
    );
    let a_runs: Vec<_> = db
        .list_graph_runs_for_spec(&spec_id)
        .unwrap()
        .into_iter()
        .filter(|r| r.node_id == "node-a")
        .collect();
    assert_eq!(a_runs.len(), 2, "node-a must have been dispatched twice");
    // First dispatch used the launch-time command, second used the updated one.
    assert!(a_runs[0].output.as_ref().unwrap()["command"]
        .as_str()
        .unwrap()
        .contains("A1"));
    assert!(a_runs[1].output.as_ref().unwrap()["command"]
        .as_str()
        .unwrap()
        .contains("A2"));
    // Ground truth: the second dispatch actually executed the new command.
    assert_eq!(std::fs::read_to_string(&ran_log).unwrap(), "A1A2");
}

#[tokio::test]
async fn cm15_config_change_during_a_running_node_does_not_alter_that_run() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-slow".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "slow".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "sleep 2 && printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let engine = std::sync::Arc::new(engine);
    let engine2 = std::sync::Arc::clone(&engine);
    let graph_id2 = graph_id.clone();
    let handle = tokio::spawn(async move {
        engine2
            .run_graph(graph_id2, None, None, None, None, None)
            .await
    });

    // Let the single dispatch get into `sleep 2`, then swap its command.
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    db.update_graph_node_details(
        "node-slow",
        None,
        None,
        Some(&serde_json::json!({
            "command": "printf CHANGED && false",
            "success_condition": "exit_code_0"
        })),
        None,
    )
    .unwrap();

    handle.await.unwrap().unwrap();

    let runs: Vec<_> = db
        .list_graph_runs_for_spec(&spec_id)
        .unwrap()
        .into_iter()
        .filter(|r| r.node_id == "node-slow")
        .collect();
    assert_eq!(runs.len(), 1, "the node must have been dispatched once");
    let out = runs[0].output.as_ref().unwrap();
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
    assert_eq!(
        out["command"], "sleep 2 && printf APPROVED",
        "the in-flight run keeps the command it started with (FR3/FR4)"
    );
    assert!(out["stdout"].as_str().unwrap().contains("APPROVED"));
    assert!(!out["stdout"].as_str().unwrap().contains("CHANGED"));
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().status,
        GraphSpecStatus::Completed
    );
}

#[tokio::test]
async fn graph_engine_regression_reviewer_commit_between_implement_and_check_uses_precommit_baseline(
) {
    // Regression for the 18:19 incident shape: the reviewer commits as
    // part of this spec's own work, then the check node runs — it must
    // evaluate against the baseline captured *before* that commit and
    // pass, never see its own attempt's commit as "no movement".
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let pre_commit_head = git_head(dir.path());

    db.insert_graph_node(&GraphNode {
            id: "node-implement".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "implement".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": "echo change >> work.txt && git add -A && git commit -q -m change && printf APPROVED",
                "success_condition": "exit_code_0"
            }),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    db.insert_graph_node(&GraphNode {
            id: "node-check".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "check".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": "test \"$(git rev-parse HEAD)\" != \"{{spec_start_head}}\" && printf APPROVED",
                "success_condition": "exit_code_0"
            }),
            position: 2,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-implement-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-implement".to_string(),
        to_node: "node-check".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    assert_eq!(
        spec.spec_start_head.as_deref(),
        Some(pre_commit_head.as_str()),
        "the baseline must stay the pre-commit HEAD, never re-resolved after the \
             reviewer's own commit"
    );

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let check_run = runs
        .iter()
        .find(|r| r.node_id == "node-check")
        .expect("check node must have run");
    assert_eq!(check_run.status, GraphRunStatus::Pass);
}

#[tokio::test]
async fn graph_engine_committed_check_passes_when_this_runs_committer_commits() {
    // Test 1 (spec TESTS REQUIRED): a spec that commits — the check
    // passes.
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());

    db.insert_graph_node(&rights_node(
        &spec_id,
        "committer",
        COMMIT_CMD,
        Some(true),
        1,
    ))
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": COMMITTED_CHECK_CMD,
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-committer-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "committer".to_string(),
        to_node: "node-check".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Always,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    assert_eq!(
        spec.spec_committed_head.as_deref(),
        Some(git_head(dir.path()).as_str()),
        "the committer's own resulting HEAD must be recorded"
    );

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let check_run = runs.iter().find(|r| r.node_id == "node-check").unwrap();
    assert_eq!(check_run.status, GraphRunStatus::Pass);
}

#[tokio::test]
async fn graph_engine_committed_check_fails_when_nothing_committed_and_tree_clean() {
    // Test 2 (spec TESTS REQUIRED): a spec that commits nothing, clean
    // tree — the check fails.
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());

    // Carries commit rights but never actually commits — the exact
    // shape of the bug this gate exists to catch: an approval with no
    // commit behind it.
    db.insert_graph_node(&rights_node(&spec_id, "committer", "true", Some(true), 1))
        .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": COMMITTED_CHECK_CMD,
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-committer-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "committer".to_string(),
        to_node: "node-check".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Always,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(
        spec.spec_committed_head, None,
        "no commit landed, so there is nothing to record"
    );
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let check_run = runs.iter().find(|r| r.node_id == "node-check").unwrap();
    assert_eq!(check_run.status, GraphRunStatus::Fail);
}

#[tokio::test]
async fn graph_engine_regression_concurrent_commit_from_outside_this_run_fails_committed_check() {
    // Test 3 (spec TESTS REQUIRED) — the regression test for the
    // concurrent-worktree case: a commit made by something other than
    // this spec's run, with the spec having committed nothing, must
    // fail. This is the 2026-08-19 incident shape: a human (or another
    // agent) commits into the same worktree while a graph is running on
    // it. The superseded `HEAD != spec_start_head` comparison is
    // satisfied by exactly this, which is the bug this test guards
    // against.
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let baseline = git_head(dir.path());

    // Simulate a spec already mid-attempt with its baseline already
    // captured and persisted — same setup the B10 resume tests use, so
    // a genuine resume (daemon restart mid-run) reuses it rather than
    // recapturing.
    db.update_graph_spec_status(&spec_id, GraphSpecStatus::Running, None, None)
        .unwrap();
    db.set_graph_spec_start_head(&spec_id, Some(&baseline))
        .unwrap();

    // The concurrent-worktree case itself: something other than this
    // run's own committer lands a commit while the spec is in flight.
    std::fs::write(dir.path().join("unrelated.txt"), "external change").unwrap();
    assert!(std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Someone Else",
            "-c",
            "user.email=someone@example.com",
            "commit",
            "-q",
            "-m",
            "concurrent, unrelated commit"
        ])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    let concurrent_head = git_head(dir.path());
    assert_ne!(
        concurrent_head, baseline,
        "the concurrent commit must actually have moved HEAD for this test to mean anything"
    );

    // This spec's own graph never commits anything — just the check.
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": COMMITTED_CHECK_CMD,
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // A resumed dispatch — same path a daemon restart mid-run takes.
    engine
        .run_graph_dispatch(
            graph_id.clone(),
            None,
            None,
            DispatchMode::resume(),
            None,
            None,
        )
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(
        spec.spec_committed_head, None,
        "no node this run trusts to commit ever ran, so `spec_committed_head` \
             must stay unset even though HEAD moved"
    );

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let check_run = runs.iter().find(|r| r.node_id == "node-check").unwrap();
    assert_eq!(
        check_run.status,
        GraphRunStatus::Fail,
        "the superseded `HEAD != spec_start_head` comparison alone would have passed \
             here (HEAD moved to the concurrent commit) — the strengthened check must not"
    );

    // Proof this is a real regression test, not a vacuous one: the old
    // comparison really would have been satisfied.
    assert_ne!(git_head(dir.path()), baseline);
}
