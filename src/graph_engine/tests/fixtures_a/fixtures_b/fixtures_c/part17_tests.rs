//! Part 17 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

/// C30: a cascade ensemble with exactly ONE member is legal
/// (`validate_ensemble_members` allows min 1), and its `kickoff` node has
/// exactly one outgoing edge — to that sole member. The lone edge must
/// resolve to the ensemble (so the member runs via `execute_ensemble` and
/// the join settles), not be dispatched as a bare node (which would then
/// dispatch `join1` as an ordinary node and error out).
#[tokio::test]
async fn ensemble_single_member_cascade_runs_end_to_end() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let ok_path = write_self_reporting_member_script(
        dir.path(),
        "casc-ok.sh",
        "sleep 1",
        &db_path,
        "casc-ok",
        "pass",
        None,
    );
    let fake_home = setup_multi_cli_home(&[("casc-ok", ok_path.as_str())]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Cascade,
        &[("casc-ok", "casc-ok")],
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
        join.status,
        GraphRunStatus::Pass,
        "a 1-member cascade must fan out via the ensemble and settle the join as Pass"
    );
    assert_eq!(
        join.output.as_ref().unwrap()["kind"],
        "cascade",
        "the join output may only carry `kind` when it came from execute_ensemble_cascade — \
             a bare node never writes a join run at all"
    );
    assert!(
        member_runs(&db, &spec_id, "casc-ok")
            .iter()
            .any(|r| r.status == GraphRunStatus::Pass),
        "the sole member must be recorded as an ensemble member run that passed"
    );
    assert!(
        member_runs(&db, &spec_id, "done-pass")
            .iter()
            .any(|r| r.status == GraphRunStatus::Pass),
        "the passing join must route onward on its Pass edge"
    );
}

/// C30 (round_robin half): the identical contract for a 1-member
/// round-robin ensemble.
#[tokio::test]
async fn ensemble_single_member_round_robin_runs_end_to_end() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let ok_path = write_self_reporting_member_script(
        dir.path(),
        "rr-ok.sh",
        "sleep 1",
        &db_path,
        "rr-ok",
        "pass",
        None,
    );
    let fake_home = setup_multi_cli_home(&[("rr-ok", ok_path.as_str())]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-ok", "rr-ok")],
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
        join.status,
        GraphRunStatus::Pass,
        "a 1-member round-robin must fan out via the ensemble and settle the join as Pass"
    );
    assert_eq!(
        join.output.as_ref().unwrap()["kind"],
        "round_robin",
        "the join output may only carry `kind` when it came from execute_ensemble_round_robin"
    );
    assert!(
        member_runs(&db, &spec_id, "rr-ok")
            .iter()
            .any(|r| r.status == GraphRunStatus::Pass),
        "the sole member must be recorded as an ensemble member run that passed"
    );
    assert!(
        member_runs(&db, &spec_id, "done-pass")
            .iter()
            .any(|r| r.status == GraphRunStatus::Pass),
        "the passing join must route onward on its Pass edge"
    );
}

/// C3: a self-reported fail stops the walk — no fallthrough to the next
/// member. A reported fail is not infra and not retried.
#[tokio::test]
async fn cm13_round_robin_self_reported_fail_stops_walk_no_fallthrough() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);

    let saysno_path = write_self_reporting_member_script(
        dir.path(),
        "saysno.sh",
        "sleep 1",
        &db_path,
        "rr-saysno",
        "fail",
        None,
    );
    let second_path = write_member_script(dir.path(), "second.sh", "printf ok");

    let fake_home = setup_multi_cli_home(&[
        ("rr-saysno", saysno_path.as_str()),
        ("rr-second", second_path.as_str()),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-saysno", "rr-saysno"), ("rr-second", "rr-second")],
    );

    let _home = HomeGuard::set(fake_home.path());
    let _result = engine
        .run_graph(_graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
        join.status,
        GraphRunStatus::Fail,
        "ensemble must fail on the self-reported fail"
    );
    let out = join.output.as_ref().unwrap();
    assert_eq!(
        out["members_tried"].as_u64().unwrap(),
        1,
        "walk must stop at the first member (reported fail)"
    );
    assert!(
        member_runs(&db, &spec_id, "rr-second").is_empty(),
        "second member must never run (no fallthrough)"
    );
    assert_eq!(
        member_runs(&db, &spec_id, "rr-saysno").len(),
        1,
        "reported fail must not be retried as infra"
    );
}

/// C3 (cascade): the same contract as
/// [`cm13_round_robin_self_reported_fail_stops_walk_no_fallthrough`] but
/// for a cascade ensemble — a member that self-reports a `fail` is a
/// usable verdict, so the cascade stops on it and never falls through to
/// the next member. Requirement 5 names cascade and round-robin together;
/// this pins the cascade half.
#[tokio::test]
async fn cm13_cascade_self_reported_fail_stops_walk_no_fallthrough() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);

    let saysno_path = write_self_reporting_member_script(
        dir.path(),
        "saysno.sh",
        "sleep 1",
        &db_path,
        "c-saysno",
        "fail",
        None,
    );
    let second_path = write_member_script(dir.path(), "second.sh", "printf ok");

    let fake_home = setup_multi_cli_home(&[
        ("c-saysno", saysno_path.as_str()),
        ("c-second", second_path.as_str()),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Cascade,
        &[("c-saysno", "c-saysno"), ("c-second", "c-second")],
    );

    let _home = HomeGuard::set(fake_home.path());
    let _result = engine
        .run_graph(_graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
        join.status,
        GraphRunStatus::Fail,
        "cascade must fail on the self-reported fail"
    );
    let out = join.output.as_ref().unwrap();
    assert_eq!(out["kind"], "cascade");
    assert_eq!(
        out["members_tried"].as_u64().unwrap(),
        1,
        "cascade must stop at the first member (reported fail)"
    );
    assert!(
        member_runs(&db, &spec_id, "c-second").is_empty(),
        "the next member must never run (no fallthrough)"
    );
    assert_eq!(
        member_runs(&db, &spec_id, "c-saysno").len(),
        1,
        "a reported fail must not be retried as infra"
    );
}

/// CB67: `timeout_minutes: 1` (60s) is "the timeout"; half of it is 30s.
/// The fixture carries the legacy `straggler_timeout_minutes: Some(0)`
/// shape (pre-CB67 `graph_ensemble` accepted it on any kind). Before
/// CB67 the no-override fallback was `effective_straggler_timeout_minutes()`
/// — i.e. that 0 — so every member was bounded to a zero-second window
/// and cut instantly; the join never sees m3 and this test fails. After
/// CB67, sequential dispatch never consults the field (FR1): each member
/// attempt is bounded only by its own effective timeout — the ensemble's
/// `timeout_minutes` here — timed fresh from that attempt's own start,
/// and every infra retry re-arms that same budget fresh. m1 crashes with
/// no verdict at once (falls through). m2 sleeps 40s (past half) then
/// exits 1, no verdict (falls through). m3 sleeps 45s (past half) and
/// crashes on attempt 0, then its infra retry gets a FRESH 60s — not
/// what's left of attempt 0's clock (the pre-CB67 single-timeout-wrapping-
/// -the-whole-retry-loop bug) and not the ensemble's — sleeps 40s (past
/// half) and files a real Pass. The join passes with m3 as winner.
#[tokio::test]
async fn round_robin_later_member_is_not_starved_by_earlier_members_elapsed_time() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let marker = dir.path().join("m3.first-attempt.marker");
    let fake_home = setup_multi_cli_home(&[
            ("m1", &write_member_script(dir.path(), "m1.sh", "exit 1")),
            (
                "m2",
                &write_member_script(dir.path(), "m2.sh", "sleep 40; exit 1"),
            ),
            (
                "m3",
                &write_member_script(
                    dir.path(),
                    "m3.sh",
                    &format!(
                        "if [ -f '{marker}' ]; then\nsleep 40\n{invocation}else\nsleep 45\ntouch '{marker}'\nexit 1\nfi",
                        marker = marker.display(),
                        invocation = self_report_invocation(&db_path, "m3", "pass", None),
                    ),
                ),
            ),
        ]);
    insert_kind_ensemble_with_timeout(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        1,
        Some(0),
        &[("m1", "m1"), ("m2", "m2"), ("m3", "m3")],
    );
    // m3 alone may retry (attempt 0 crashes without a verdict; attempt 1
    // must get a fresh member timeout, not attempt 0's leftovers).
    db.update_graph_node_details(
        "m3",
        None,
        None,
        Some(&serde_json::json!({
            "platform": "m3",
            "prompt_template": "ignored by the member's test script",
            "timeout_minutes": 1,
            "infra_backoff_seconds": 0,
            "infra_retry_limit": 1,
        })),
        None,
    )
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
            join.status,
            GraphRunStatus::Pass,
            "m3 must not be cut by the ensemble's (nonexistent) deadline, by m1+m2's elapsed time, or by its own attempt 0's elapsed time; it gets its own fresh timeout per attempt"
        );
    assert_eq!(join.output.as_ref().unwrap()["member"]["node_id"], "m3");
    assert!(
        member_runs(&db, &spec_id, "m3")
            .iter()
            .any(|r| r.status == GraphRunStatus::Pass),
        "m3 must have actually passed, not been cut"
    );
}

/// CB67 (cascade variant of the round-robin test above): same setup, same
/// requirements — the cascade fallthrough path must arm no ensemble-wide
/// deadline either (the legacy `straggler_timeout_minutes: Some(0)`
/// fixture would cut every member instantly pre-CB67), each member gets
/// its own fresh timeout from its own attempt's start, and m3's infra
/// retry re-arms that budget fresh instead of inheriting attempt 0's
/// leftovers. See the round-robin variant's doc comment for the full
/// pre/post-fix story.
#[tokio::test]
async fn cascade_later_member_is_not_starved_by_earlier_members_elapsed_time() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let marker = dir.path().join("m3.first-attempt.marker");
    let fake_home = setup_multi_cli_home(&[
            ("m1", &write_member_script(dir.path(), "m1.sh", "exit 1")),
            (
                "m2",
                &write_member_script(dir.path(), "m2.sh", "sleep 40; exit 1"),
            ),
            (
                "m3",
                &write_member_script(
                    dir.path(),
                    "m3.sh",
                    &format!(
                        "if [ -f '{marker}' ]; then\nsleep 40\n{invocation}else\nsleep 45\ntouch '{marker}'\nexit 1\nfi",
                        marker = marker.display(),
                        invocation = self_report_invocation(&db_path, "m3", "pass", None),
                    ),
                ),
            ),
        ]);
    insert_kind_ensemble_with_timeout(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Cascade,
        1,
        Some(0),
        &[("m1", "m1"), ("m2", "m2"), ("m3", "m3")],
    );
    // m3 alone may retry (attempt 0 crashes without a verdict; attempt 1
    // must get a fresh member timeout, not attempt 0's leftovers).
    db.update_graph_node_details(
        "m3",
        None,
        None,
        Some(&serde_json::json!({
            "platform": "m3",
            "prompt_template": "ignored by the member's test script",
            "timeout_minutes": 1,
            "infra_backoff_seconds": 0,
            "infra_retry_limit": 1,
        })),
        None,
    )
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
            join.status,
            GraphRunStatus::Pass,
            "m3 must not be cut by the ensemble's (nonexistent) deadline, by m1+m2's elapsed time, or by its own attempt 0's elapsed time; it gets its own fresh timeout per attempt"
        );
    assert_eq!(join.output.as_ref().unwrap()["winner"]["node_id"], "m3");
    assert!(
        member_runs(&db, &spec_id, "m3")
            .iter()
            .any(|r| r.status == GraphRunStatus::Pass),
        "m3 must have actually passed, not been cut"
    );
}

/// CB66.1: a round-robin ensemble whose members all crash (exit 1, never
/// reporting) settles with no verdict: the join is `Error` carrying
/// `no_verdict` + per-member reasons, and the run routes through the
/// `Error` edge — never the `Fail` edge.
#[tokio::test]
async fn cb66_no_verdict_round_robin_routes_error_edge_not_fail() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let crash_a = write_member_script(dir.path(), "a.sh", "exit 1");
    let crash_b = write_member_script(dir.path(), "b.sh", "exit 1");
    let fake_home = setup_multi_cli_home(&[("rr-a", crash_a.as_str()), ("rr-b", crash_b.as_str())]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-a", "rr-a"), ("rr-b", "rr-b")],
    );
    db.insert_graph_node(&GraphNode {
        id: "done-error".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "done-error".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "printf ok", "success_condition": "exit_code_0" }),
        position: 102,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "join1->done-error".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "join1".to_string(),
        to_node: "done-error".to_string(),
        condition: GraphEdgeCondition::Error,
    })
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    result.unwrap();

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
        join.status,
        GraphRunStatus::Error,
        "a verdictless join must be Error, not Fail"
    );
    let out = join.output.as_ref().unwrap();
    assert_eq!(out["no_verdict"], serde_json::json!(true));
    let members = out["members"]
        .as_array()
        .expect("no-verdict join lists members");
    assert_eq!(members.len(), 2, "both members must be listed");
    for m in members {
        assert!(
            m.get("node")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty()),
            "member entry must name the node: {m:?}"
        );
        assert!(
            m.get("platform")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty()),
            "member entry must name the platform: {m:?}"
        );
        assert!(
            m.get("reason")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty()),
            "member entry must carry a reason: {m:?}"
        );
    }
    assert!(
        member_runs(&db, &spec_id, "done-error")
            .iter()
            .any(|r| r.status == GraphRunStatus::Pass),
        "the Error edge must route to done-error"
    );
    assert!(
        member_runs(&db, &spec_id, "done-fail").is_empty(),
        "the Fail edge must never fire on a no-verdict exit"
    );
    assert!(
        member_runs(&db, &spec_id, "done-pass").is_empty(),
        "the Pass edge must never fire on a no-verdict exit"
    );
}

/// CB66.2: same verdictless fixture with NO `Error` edge — the graph
/// pauses with a blocker naming both members instead of following `Fail`.
#[tokio::test]
async fn cb66_no_verdict_without_error_edge_pauses_with_blocker() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let crash_a = write_member_script(dir.path(), "a.sh", "exit 1");
    let crash_b = write_member_script(dir.path(), "b.sh", "exit 1");
    let fake_home = setup_multi_cli_home(&[("rr-a", crash_a.as_str()), ("rr-b", crash_b.as_str())]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-a", "rr-a"), ("rr-b", "rr-b")],
    );

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    result.unwrap();

    let lp = db.get_graph(&graph_id).unwrap().expect("graph must exist");
    assert_eq!(
        lp.status,
        GraphStatus::Paused,
        "a no-verdict exit with no Error edge must pause, not fail"
    );
    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(join.status, GraphRunStatus::Error);
    let blocker = join
        .output
        .as_ref()
        .and_then(|o| o.get("blocker"))
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        blocker.contains("rr-a") && blocker.contains("rr-b"),
        "blocker must name both members, got: {blocker:?}"
    );
    assert!(
        blocker.to_lowercase().contains("verdict"),
        "blocker must say no verdict was filed, got: {blocker:?}"
    );
    assert!(
        member_runs(&db, &spec_id, "done-fail").is_empty(),
        "the Fail edge must never fire on a no-verdict exit"
    );
}

/// CM42 (FR1/FR2/FR5): the incident end to end. A two-spec graph runs
/// `Designer` to `Completed`, then `Implementer`'s round-robin ensemble
/// exits with every member unreported and no `Error` edge. The block must
/// leave the spec `Interrupted` (resumable), not `Failed`; the blocker
/// must name the ensemble and BOTH ways out (`graph_control
/// retry_current_node`, `graph_control`); and after a `graph_ensemble`
/// member swap plus `graph_control retry_current_node` (the validation
/// call `graph_control` makes, then the same `is_resume: true` dispatch
/// `resume_background` fires) the REWIRED member runs, the already-passed
/// Designer node gets no new run row, and the spec and graph complete.
#[tokio::test]
async fn no_verdict_block_is_resumable_via_retry_current_node() {
    let (dir, db, engine, graph_id, designer_id) = graph_fixture().unwrap();
    db.update_graph_spec_details(&designer_id, Some("Designer"), None, Some(1), None)
        .unwrap();
    let implementer = GraphSpec {
        graph_id: Some(graph_id.clone()),
        name: "Implementer".to_string(),
        ..standalone_spec("spec-implementer", 2)
    };
    db.insert_graph_spec(&implementer).unwrap();

    // Designer: one check node that passes on its own — the work the old
    // `graph_control`-only escape hatch made every resume pay for again.
    db.insert_graph_node(&GraphNode {
        id: "designer".to_string(),
        spec_id: Some(designer_id.clone()),
        graph_id: None,
        name: "designer".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf ok",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // Implementer: a round-robin ensemble whose two members both exit
    // unreported (CM13: script-backed doubles never call
    // `graph_complete_node`), with no `Error` edge to absorb the join.
    let crash_a = write_member_script(dir.path(), "a.sh", "exit 1");
    let crash_b = write_member_script(dir.path(), "b.sh", "exit 1");
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let rewired = write_self_reporting_member_script(
        dir.path(),
        "rewired.sh",
        "printf ok",
        &db_path,
        "m-old-1",
        "pass",
        None,
    );
    let fake_home = setup_multi_cli_home(&[
        ("old-a", crash_a.as_str()),
        ("old-b", crash_b.as_str()),
        ("m-new-1", rewired.as_str()),
    ]);
    insert_kind_ensemble(
        &db,
        &implementer.id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("m-old-1", "old-a"), ("m-old-2", "old-b")],
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    // ── After the block ────────────────────────────────────────────
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Paused,
        "the no-verdict exit pauses the graph"
    );
    let designer_after = db.get_graph_spec(&designer_id).unwrap().unwrap();
    assert_eq!(
        designer_after.status,
        GraphSpecStatus::Completed,
        "Designer must have passed before the block"
    );
    let implementer_after = db.get_graph_spec(&implementer.id).unwrap().unwrap();
    assert_eq!(
        implementer_after.status,
        GraphSpecStatus::Interrupted,
        "FR1: a no-verdict block leaves the spec interrupted (resumable), never failed"
    );

    let join = join_run(&db, &implementer.id, "join1");
    assert_eq!(join.status, GraphRunStatus::Error);
    let blocker = crate::daemon::handler_run_exec::graph_run_blocker(&join)
        .expect("FR1: the blocked run records a blocker");
    assert!(
        blocker.contains("Switched Ensemble"),
        "the blocker names the blocked node (the ensemble): {blocker}"
    );
    assert!(
        blocker.contains(
            "`graph_control continue` with mode `retry_current_node` resumes at this ensemble"
        ),
        "FR5: the blocked text names retry_current_node as the way to resume here: {blocker}"
    );
    assert!(
        blocker.contains("`graph_control reset` restarts the spec from Start"),
        "FR5: the blocked text names graph_control reset as the way to restart the spec: {blocker}"
    );
    assert!(
        blocker.contains("`graph_control continue` with mode `run_from_node`"),
        "the blocked text names run_from_node as a way back in: {blocker}"
    );

    // `graph_control retry_current_node` must accept this spec…
    crate::daemon::handler_run_exec::handle_retry_current_node(&db, &graph_id)
        .expect("FR2: an interrupted no-verdict spec is resumable by graph_control");

    // …and `graph_ensemble`'s member swap: the surviving slot keeps
    // its node row, the crew is now platform `m-new-1`, the other member
    // (node row included) is gone.
    db.replace_ensemble_members(
        "ens1",
        &[EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m-old-1".to_string(),
            position: 0,
            platform: "m-new-1".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }],
        &[GraphNode {
            id: "m-old-1".to_string(),
            spec_id: Some(implementer.id.clone()),
            graph_id: None,
            name: "member-1".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": "m-new-1",
                "prompt_template": "ignored by the member's test script",
                "timeout_minutes": 5,
                "infra_backoff_seconds": 0,
            }),
            position: 2,
            created_at: chrono::Utc::now(),
        }],
    )
    .unwrap();

    let designer_runs_before = member_runs(&db, &designer_id, "designer").len();
    let kickoff_runs_before = member_runs(&db, &implementer.id, "kickoff").len();
    let member_runs_before = member_runs(&db, &implementer.id, "m-old-1").len();

    // The resume `graph_control` fires (`retry_current_node` validates,
    // then `resume_background` dispatches with `is_resume: true`).
    let _home = HomeGuard::set(fake_home.path());
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
    drop(_home);

    // ── After the resume ───────────────────────────────────────────
    let member_runs_after = member_runs(&db, &implementer.id, "m-old-1");
    assert!(
        member_runs_after.len() > member_runs_before,
        "FR2: the rewired member gets a fresh run row on resume"
    );
    let resumed_join = member_runs(&db, &implementer.id, "join1")
        .into_iter()
        .find(|run| run.status == GraphRunStatus::Pass)
        .expect("FR2: the resumed ensemble reaches a verdict and records a passing join");
    assert_eq!(
        resumed_join.output.as_ref().unwrap()["member"]["platform"],
        "m-new-1",
        "FR2: the resumed dispatch uses the rewired member list, not the blocked one"
    );
    assert_eq!(
        member_runs(&db, &implementer.id, "kickoff").len(),
        kickoff_runs_before,
        "re-entry happens at the blocked ensemble, not at the spec's entry node"
    );
    assert_eq!(
        member_runs(&db, &designer_id, "designer").len(),
        designer_runs_before,
        "FR2: the Designer spec already passed and must not run again"
    );
    let implementer_final = db.get_graph_spec(&implementer.id).unwrap().unwrap();
    assert_eq!(
        implementer_final.status,
        GraphSpecStatus::Completed,
        "the resumed spec runs to completion"
    );
    let lp_final = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp_final.status, GraphStatus::Completed);
}

/// CB66.3 (FR3): one member crashes, the other files a real `fail`
/// verdict — the join stays `Fail` and routes to F, not E.
#[tokio::test]
async fn cb66_verdict_plus_crash_stays_fail() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let crash_path = write_member_script(dir.path(), "crash.sh", "exit 1");
    let fail_path = write_self_reporting_member_script(
        dir.path(),
        "saysno.sh",
        "sleep 1",
        &db_path,
        "rr-b",
        "fail",
        None,
    );
    let fake_home =
        setup_multi_cli_home(&[("rr-a", crash_path.as_str()), ("rr-b", fail_path.as_str())]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-a", "rr-a"), ("rr-b", "rr-b")],
    );
    db.insert_graph_node(&GraphNode {
        id: "done-error".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "done-error".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "printf ok", "success_condition": "exit_code_0" }),
        position: 102,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "join1->done-error".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "join1".to_string(),
        to_node: "done-error".to_string(),
        condition: GraphEdgeCondition::Error,
    })
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let _result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
        join.status,
        GraphRunStatus::Fail,
        "a genuine fail verdict plus a crash must stay Fail"
    );
    let out = join.output.as_ref().unwrap();
    assert!(
        !out.get("no_verdict")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "a verdict-bearing join must not carry no_verdict"
    );
    assert_eq!(
        out["members_tried"].as_u64().unwrap(),
        2,
        "the walk must fall through the crasher to the failing member"
    );
    assert!(
        !member_runs(&db, &spec_id, "done-fail").is_empty(),
        "the Fail edge must fire on a genuine fail verdict"
    );
    assert!(
        member_runs(&db, &spec_id, "done-error").is_empty(),
        "the Error edge must not fire when a verdict exists"
    );
}

/// CB66.4 (FR4): a no-verdict exit is not a review cycle — bouncing back
/// into the ensemble through the `Error` edge reuses the same iteration.
#[tokio::test]
async fn cb66_no_verdict_does_not_consume_iteration() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let crash_a = write_member_script(dir.path(), "a.sh", "exit 1");
    let crash_b = write_member_script(dir.path(), "b.sh", "exit 1");
    let fake_home = setup_multi_cli_home(&[("rr-a", crash_a.as_str()), ("rr-b", crash_b.as_str())]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-a", "rr-a"), ("rr-b", "rr-b")],
    );
    let counter = dir.path().join("gate-counter");
    db.insert_graph_node(&counter_gate_node("gate", &spec_id, &counter, 60))
        .unwrap();
    let done_marker = dir.path().join("done.marker");
    db.insert_graph_node(&touch_marker_node("done", &spec_id, &done_marker, 61))
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "join1->gate".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "join1".to_string(),
        to_node: "gate".to_string(),
        condition: GraphEdgeCondition::Error,
    })
    .unwrap();
    for edge in bounce_edges_into_ensemble(&spec_id, "gate", &["rr-a", "rr-b"]) {
        db.insert_graph_edge(&edge).unwrap();
    }
    db.insert_graph_edge(&GraphEdge {
        id: "gate->done".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "gate".to_string(),
        to_node: "done".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    result.unwrap();

    let joins = member_runs(&db, &spec_id, "join1");
    assert_eq!(
        joins.len(),
        2,
        "the gate must bounce exactly once back into the ensemble"
    );
    for run in &joins {
        assert_eq!(run.status, GraphRunStatus::Error);
        assert_eq!(
            run.iteration, 1,
            "both no-verdict visits must share iteration 1 (no consumption)"
        );
    }
    assert!(
        !member_runs(&db, &spec_id, "done").is_empty(),
        "flow must continue through the error edge to done"
    );
}

/// CB66.5: an `Error` status resolves only `Error` edges — never `Fail`
/// or `Always` — and yields `None` when no `Error` edge exists.
#[test]
fn select_next_step_error_matches_only_error_edges() {
    let edge = |id: &str, to: &str, condition| GraphEdge {
        id: id.to_string(),
        spec_id: Some("spec".to_string()),
        graph_id: None,
        from_node: "n1".to_string(),
        to_node: to.to_string(),
        condition,
    };
    let edges = vec![
        edge("e-error", "e", GraphEdgeCondition::Error),
        edge("e-fail", "f", GraphEdgeCondition::Fail),
        edge("e-always", "a", GraphEdgeCondition::Always),
    ];
    let sel = select_next_step(&edges, &[], "n1", GraphRunStatus::Error)
        .unwrap()
        .expect("Error must resolve the Error edge");
    assert_eq!(sel.cursor, SpecCursor::Node("e".to_string()));
    let bare = vec![
        edge("e-fail", "f", GraphEdgeCondition::Fail),
        edge("e-always", "a", GraphEdgeCondition::Always),
    ];
    assert!(
        select_next_step(&bare, &[], "n1", GraphRunStatus::Error)
            .unwrap()
            .is_none(),
        "Error with no Error edge must match nothing (never Fail/Always)"
    );
}

/// CB66.6: a join carrying `no_verdict` counts as infra for attempt
/// accounting.
#[test]
fn execution_is_infra_failure_no_verdict() {
    assert!(execution_is_infra_failure(
        &serde_json::json!({"no_verdict": true})
    ));
    assert!(!execution_is_infra_failure(
        &serde_json::json!({"kind": "quorum"})
    ));
}

/// C4: `run_self_reported` is the single source of truth for
/// "did this run file a verdict?" — pins the shared helper in place.
#[test]
fn run_self_reported_is_the_only_report_check() {
    let mut run = GraphNodeRun {
        id: "run-test".to_string(),
        graph_id: "loop1".to_string(),
        spec_id: "spec1".to_string(),
        node_id: "node1".to_string(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };
    assert!(!run_self_reported(&run));
    run.status = GraphRunStatus::Pass;
    assert!(run_self_reported(&run));
    run.status = GraphRunStatus::Fail;
    assert!(run_self_reported(&run));
}

#[test]
fn resolve_dispatch_workdir_prefers_sandbox_then_override_then_graph() {
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "g".to_string(),
        name: "Graph".to_string(),
        description: None,
        workdir: "/graph".to_string(),
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
        require_spec_type: false,
    };
    assert_eq!(
        GraphEngine::resolve_dispatch_workdir(&lp, None, &None),
        "/graph"
    );
    assert_eq!(
        GraphEngine::resolve_dispatch_workdir(&lp, Some("/override".to_string()), &None),
        "/override"
    );
    let sandbox = crate::domain::sandbox::Sandbox {
        id: "s".to_string(),
        project_hash: "h".to_string(),
        base_branch: "main".to_string(),
        sandbox_branch: "sandbox".to_string(),
        worktree_path: std::path::PathBuf::from("/sandbox"),
        cli_name: "claude".to_string(),
        original_workdir: "/graph".to_string(),
        created_at: chrono::Utc::now(),
    };
    assert_eq!(
        GraphEngine::resolve_dispatch_workdir(&lp, Some("/override".to_string()), &Some(sandbox)),
        "/sandbox"
    );
}

#[test]
fn hook_kind_label_covers_all_four_kinds() {
    let base = || crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let mut interactive = base();
    interactive.target_session_id = Some("sess".to_string());
    interactive.prompt = Some("hi".to_string());
    assert_eq!(GraphEngine::hook_kind_label(&interactive), "interactive");
    let mut command = base();
    command.command = Some("echo hi".to_string());
    assert_eq!(GraphEngine::hook_kind_label(&command), "command");
    let mut graph = base();
    graph.target_graph_id = Some("g".to_string());
    assert_eq!(GraphEngine::hook_kind_label(&graph), "graph");
    let mut agent = base();
    agent.platform = Some("claude".to_string());
    assert_eq!(GraphEngine::hook_kind_label(&agent), "agent");
}

#[test]
fn hook_outcome_text_pass_exit_code_and_summary() {
    let passed = HookExecution {
        status: GraphRunStatus::Pass,
        output: serde_json::json!({}),
        summary: "ok".to_string(),
    };
    assert_eq!(GraphEngine::hook_outcome_text(&passed), "passed");
    let exited = HookExecution {
        status: GraphRunStatus::Fail,
        output: serde_json::json!({"exit_code": 3}),
        summary: "boom".to_string(),
    };
    assert_eq!(GraphEngine::hook_outcome_text(&exited), "failed: exit 3");
    let failed = HookExecution {
        status: GraphRunStatus::Fail,
        output: serde_json::json!({}),
        summary: "boom".to_string(),
    };
    assert_eq!(GraphEngine::hook_outcome_text(&failed), "failed: boom");
}

#[test]
fn cascade_is_resumed_start_failed_only_for_resumed_genuine_fail() {
    assert!(!cascade_is_resumed_start_failed(
        false,
        true,
        false,
        GraphRunStatus::Fail
    ));
    assert!(!cascade_is_resumed_start_failed(
        true,
        false,
        false,
        GraphRunStatus::Fail
    ));
    assert!(!cascade_is_resumed_start_failed(
        true,
        true,
        true,
        GraphRunStatus::Fail
    ));
    assert!(!cascade_is_resumed_start_failed(
        true,
        true,
        false,
        GraphRunStatus::Pass
    ));
    assert!(cascade_is_resumed_start_failed(
        true,
        true,
        false,
        GraphRunStatus::Fail
    ));
}

#[test]
fn budget_key_distinguishes_node_and_ensemble() {
    assert_eq!(budget_key(&SpecCursor::Node("n1".to_string())), "n1");
    assert_eq!(
        budget_key(&SpecCursor::Ensemble("e1".to_string())),
        "ensemble:e1"
    );
}

#[test]
fn set_join_run_blocker_is_noop_without_runs() {
    let (_dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    engine
        .set_join_run_blocker(&spec_id, "join1", "nothing to annotate")
        .unwrap();
    assert!(
        db.list_graph_runs_for_spec(&spec_id).unwrap().is_empty(),
        "a spec with no runs must stay run-free after the noop"
    );
}

#[test]
fn load_ensemble_member_nodes_reports_missing_member() {
    let (_dir, db, _engine, _graph_id, _spec_id) = graph_fixture().unwrap();
    let err = load_ensemble_member_nodes(&db, &missing_member_details()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Ensemble member node 'ghost-node' not found."
    );
}

#[test]
fn ensemble_member_failure_lines_names_each_member_or_defaults() {
    let output = serde_json::json!({
        "members": [
            {"node": "a", "platform": "p", "reason": "boom"},
            {},
        ]
    });
    assert_eq!(
        GraphEngine::ensemble_member_failure_lines(&output),
        vec![
            "a (p): boom".to_string(),
            "? (?): no reason recorded".to_string(),
        ]
    );
    assert!(
        GraphEngine::ensemble_member_failure_lines(&serde_json::json!({})).is_empty(),
        "an output without a members array must yield no lines"
    );
}

#[test]
fn ensemble_name_for_join_falls_back_to_node_id() {
    let details = missing_member_details();
    let ensembles = std::slice::from_ref(&details);
    assert_eq!(
        GraphEngine::ensemble_name_for_join(ensembles, "join1"),
        "missing"
    );
    assert_eq!(
        GraphEngine::ensemble_name_for_join(ensembles, "unknown-node"),
        "unknown-node"
    );
}

// ── CB82: daemon shutdown must not route the killed node's fail edge ──

/// CB82: a check node whose process is killed while the shutdown flag is
/// set traverses no edge — its run output is the interrupted marker and
/// the fail-edge target never launches. Breaks if the `run_spec` (3.4)
/// guard is removed (B runs, A output is plain check-fail JSON).
#[tokio::test]
async fn shutdown_check_kill_traverses_no_edge_and_marks_interrupted() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&rights_node(&spec_id, "check-a", "false", None, 1))
        .unwrap();
    db.insert_graph_node(&rights_node(&spec_id, "check-b", "true", None, 2))
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "check-a".to_string(),
        to_node: "check-b".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    // Simulate "shutdown began mid-run": the routing decision is
    // synchronous, so a pre-set flag deterministically covers the kill
    // without timing flakes.
    engine.mark_shutting_down();
    engine
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert!(
        runs.iter().all(|r| r.node_id != "check-b"),
        "shutdown must not route the fail edge: check-b never launched, runs={runs:?}"
    );
    let run_a = runs
        .iter()
        .find(|r| r.node_id == "check-a")
        .expect("check-a ran");
    assert_eq!(run_a.status, GraphRunStatus::Fail);
    let output = run_a.output.as_ref().expect("interrupted run has output");
    assert_eq!(
        output.get("interrupted").and_then(|v| v.as_bool()),
        Some(true),
        "killed run must carry the interrupted marker, output={output}"
    );
    assert_eq!(
        output.get("reason").and_then(|v| v.as_str()),
        Some("daemon shutdown")
    );
    assert!(
        run_was_terminated_out_of_band(run_a),
        "a late completion of the killed run must also route no edge"
    );

    // CB82 backstop (3.5): even an already-settled step routes no edge
    // while the flag is set — covers parallel-quorum and no-verdict
    // joins, which never pass through the `run_spec` guard above.
    let lp = db.get_graph(&runs[0].graph_id).unwrap().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    let nodes_by_id: HashMap<&str, &GraphNode> = HashMap::new();
    let edges: Vec<GraphEdge> = Vec::new();
    let ensembles: Vec<EnsembleDetails> = Vec::new();
    let names: Vec<String> = Vec::new();
    let ctx = SpecStepCtx {
        lp: &lp,
        spec: &spec,
        workdir: "/tmp",
        queue_id: None,
        enforce_commit_rights: false,
        edges: &edges,
        ensembles: &ensembles,
        nodes_by_id: &nodes_by_id,
        all_node_names: &names,
        spec_start_head: None,
        spec_group: None,
    };
    let mut state = SpecRunState {
        cursor: SpecCursor::Node("check-a".to_string()),
        node_outputs: HashMap::new(),
        resume_previous_output: None,
        iterations: HashMap::new(),
        previous_node_name: None,
        spec_committed_head: None,
        resumable_sessions: HashMap::new(),
        round_robin_pins: HashMap::new(),
    };
    let step = NodeStepResult {
        execution: NodeExecution {
            status: GraphRunStatus::Fail,
            output: serde_json::json!({"exit_code": 1}),
            summary: "check-a failed".to_string(),
        },
        from_node_id: "check-a".to_string(),
        run_id: None,
        had_infra_crash: false,
    };
    let flow = engine
        .apply_spec_step(&ctx, &mut state, step)
        .await
        .unwrap();
    assert!(
        matches!(flow, SpecLoopFlow::Done(SpecExecutionOutcome::Superseded)),
        "apply_spec_step must stop silently during shutdown, routing no edge"
    );
}

/// CB82: after simulated reconciliation, `retry_current_node` re-runs the
/// interrupted check itself — not the ensemble/node on its fail edge —
/// with the same input it had. Breaks if the reconcile sweep (3.8) is
/// missing (spec stays `Running`) or any later fail-edge row exists.
#[test]
fn reconcile_then_retry_reruns_interrupted_check_not_fail_edge() {
    let (dir, db, _engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&rights_node(&spec_id, "check-a", "false", None, 1))
        .unwrap();
    db.insert_graph_node(&rights_node(&spec_id, "check-b", "true", None, 2))
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "check-a".to_string(),
        to_node: "check-b".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    // The engine's shutdown finalizer row: Fail + interrupted marker,
    // with the input check-a saw (a check node re-runs the same command
    // from config; an ensemble member keeps this `previous_feedback`).
    let input = serde_json::json!({"previous_feedback": "do it again"});
    db.insert_graph_run(&GraphNodeRun {
        id: "run-a".to_string(),
        graph_id: graph_id.clone(),
        spec_id: spec_id.clone(),
        node_id: "check-a".to_string(),
        status: GraphRunStatus::Fail,
        input: Some(input.clone()),
        output: Some(serde_json::json!({"interrupted": true, "reason": "daemon shutdown"})),
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
    db.update_graph_status(&graph_id, GraphStatus::Running, None, None)
        .unwrap();
    db.update_graph_spec_status(&spec_id, GraphSpecStatus::Running, None, None)
        .unwrap();

    let reconciled = db.reconcile_orphaned_graphs(dir.path()).unwrap();
    assert_eq!(reconciled.len(), 1);
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Paused);
    assert!(
        lp.paused_by_reconciliation,
        "reconciliation must flag its own pause"
    );
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(
            spec.status,
            GraphSpecStatus::Interrupted,
            "a spec whose latest run is shutdown-interrupted must reconcile to Interrupted, not stay Running"
        );

    // The `retry_current_node` path: `resolve_spec_start` with
    // `is_resume=true` must yield the interrupted node itself (never the
    // fail-edge target) with its original input.
    let spec_details = db.get_graph_spec_details(&spec_id).unwrap().unwrap();
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert!(
        runs.iter().all(|r| r.node_id != "check-b"),
        "no fail-edge row may exist after a shutdown kill"
    );
    let ensembles: Vec<EnsembleDetails> = Vec::new();
    let (cursor, _outputs, previous, _iterations) = resolve_spec_start(
        &spec_details.nodes,
        &spec_details.edges,
        &spec,
        &runs,
        &ensembles,
        true,
        None,
    )
    .unwrap();
    assert_eq!(cursor, SpecCursor::Node("check-a".to_string()));
    assert_eq!(previous, Some(input));
}
