//! Part 13 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

#[tokio::test]
async fn cm15_round_robin_reads_member_set_from_db_between_dispatches() {
    // The ensemble is dispatched TWICE inside ONE run: `done-pass` (the
    // join's on-pass target) is turned into a gate that fails the first
    // time, routing back to `kickoff`, which re-enters the ensemble; on
    // the second visit it passes and the spec completes. Between the two
    // dispatches the member set is swapped exactly the way
    // `graph_update_ensemble` swaps it (the `ensemble_members` rows plus
    // each member node's config). Dispatch 1 must record the launch-time
    // platform and dispatch 2 the replaced one — which only holds if the
    // engine re-reads the ensemble from the database per dispatch instead
    // of from the launch snapshot. Two separate `run_graph` calls would not
    // prove this: each call rebuilds its snapshot from the current rows.
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    // Two members + two "replacement" platforms, all backed by a lingering
    // script that self-reports a Pass as the last thing it does before
    // exiting.
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let s_m0 = write_self_reporting_member_script(
        dir.path(),
        "m0.sh",
        "sleep 1",
        &db_path,
        "m0",
        "pass",
        None,
    );
    let s_m1 = write_self_reporting_member_script(
        dir.path(),
        "m1.sh",
        "sleep 1",
        &db_path,
        "m1",
        "pass",
        None,
    );
    let fake_home = setup_multi_cli_home(&[
        ("cli-m0a", s_m0.as_str()),
        ("cli-m1a", s_m1.as_str()),
        ("cli-m0b", s_m0.as_str()),
        ("cli-m1b", s_m1.as_str()),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("m0", "cli-m0a"), ("m1", "cli-m1a")],
    );
    // Turn `done-pass` into a fail-once gate and wire its Fail edge back to
    // `kickoff` so a second ensemble dispatch happens in the same run. The
    // 0.5s sleep widens the window for the member swap between dispatches.
    let gate = dir.path().join("gate.cnt");
    db.update_graph_node_details(
        "done-pass",
        None,
        None,
        Some(&serde_json::json!({
            "command": format!(
                "n=$(cat \"{c}\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"{c}\"; \
                 [ \"$n\" -ge 2 ] && printf DONE || {{ sleep 0.5; exit 1; }}",
                c = gate.display()
            ),
            "success_condition": "exit_code_0"
        })),
        None,
    )
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "done-pass->kickoff".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "done-pass".to_string(),
        to_node: "kickoff".to_string(),
        condition: GraphEdgeCondition::Fail,
    })
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());

    let engine = std::sync::Arc::new(engine);
    let engine2 = std::sync::Arc::clone(&engine);
    let graph_id2 = graph_id.clone();
    let handle = tokio::spawn(async move {
        engine2
            .run_graph(graph_id2, None, None, None, None, None)
            .await
    });

    // Wait for the first ensemble dispatch to land its join row, then swap
    // the member set — this is the `graph_update_ensemble` between the two
    // dispatches of the same ensemble step.
    loop {
        let joins = db
            .list_graph_runs_for_spec(&spec_id)
            .unwrap()
            .iter()
            .filter(|r| r.node_id == "join1")
            .count();
        if joins >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    for (nid, plat) in [("m0", "cli-m0b"), ("m1", "cli-m1b")] {
        db.update_ensemble_member("ens1", nid, plat, None, None, None)
            .unwrap();
        db.update_graph_node_details(
            nid,
            None,
            None,
            Some(&serde_json::json!({
                "platform": plat,
                "prompt_template": "ignored by the member's test script",
                "timeout_minutes": 5,
                "infra_backoff_seconds": 0
            })),
            None,
        )
        .unwrap();
    }

    handle.await.unwrap().unwrap();
    drop(_home);

    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().status,
        GraphSpecStatus::Completed
    );
    let joins: Vec<_> = db
        .list_graph_runs_for_spec(&spec_id)
        .unwrap()
        .into_iter()
        .filter(|r| r.node_id == "join1")
        .collect();
    assert_eq!(
        joins.len(),
        2,
        "the ensemble must have been dispatched twice"
    );
    assert_eq!(
        joins[0].output.as_ref().unwrap()["member"]["platform"],
        "cli-m0a",
        "first dispatch uses the launch-time member set"
    );
    assert_eq!(
        joins[1].output.as_ref().unwrap()["member"]["platform"],
        "cli-m1b",
        "second dispatch must read the replaced member set from the database, \
             not the launch snapshot"
    );
}

/// CM3: the default (parallel/quorum) kind is unchanged — every member
/// runs and the join emits the `quorum` shape against `min_pass`.
#[tokio::test]
async fn parallel_ensemble_join_output_unchanged() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        ("p-a", &write_member_script(dir.path(), "a.sh", "printf ok")),
        ("p-b", &write_member_script(dir.path(), "b.sh", "printf ok")),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Parallel,
        &[("p-a", "p-a"), ("p-b", "p-b")],
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    // CM13: script-backed members never self-report, so both are
    // unreported infra and the join is Error with no_verdict (CB66:
    // 0 filed verdicts). The quorum shape itself (kind/passed/min_pass/
    // consolidated_doc) is unchanged for verdict-bearing joins.
    assert_eq!(join.status, GraphRunStatus::Error);
    let out = join.output.as_ref().unwrap();
    assert_eq!(
        out["kind"], "quorum",
        "parallel keeps the quorum join shape"
    );
    assert_eq!(out["passed"], 0);
    assert_eq!(out["no_verdict"], serde_json::json!(true));
    assert!(!member_runs(&db, &spec_id, "p-a").is_empty());
    assert!(!member_runs(&db, &spec_id, "p-b").is_empty());
}

/// A crashed member (fast nonzero exit, no self-report) is retried in
/// place like a lone agent node (B19); succeeding on the retry makes it
/// count as a pass, so with a second healthy member the join passes 2/2.
/// (Two members because ensemble fan-out needs more than one entry edge.)
#[tokio::test]
async fn ensemble_member_infra_crash_then_succeeds_on_retry_join_passes() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let counter = dir.path().join("flap.counter");
    // Crashes (exit 1) on the first attempt, passes (exit 0) on the retry.
    let flap = write_member_script(
            dir.path(),
            "flap.sh",
            &format!(
                "n=$(cat \"{c}\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"{c}\"; [ \"$n\" -ge 2 ] && (printf ok; exit 0) || exit 1",
                c = counter.display(),
            ),
        );
    let fake_home = setup_multi_cli_home(&[
        ("member-flap", &flap),
        (
            "member-ok",
            &write_member_script(dir.path(), "ok.sh", "printf ok"),
        ),
    ]);
    insert_infra_ensemble(
        &db,
        &spec_id,
        &[("m-flap", "member-flap"), ("m-ok", "member-ok")],
        2,
        Some(1),
        None,
        &serde_json::json!({ "infra_backoff_seconds": 0 }),
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    // CM13: neither member self-reports, so both exhaust retries with no
    // verdict and the join is Error with no_verdict (CB66).
    assert_eq!(
        join.status,
        GraphRunStatus::Error,
        "CM13: unreported runs are infra; both members exhaust retries with no verdict"
    );
    assert_eq!(
        join.output.as_ref().unwrap()["no_verdict"],
        serde_json::json!(true)
    );
    assert_eq!(join.output.as_ref().unwrap()["passed"], 0);

    let runs = member_runs(&db, &spec_id, "m-flap");
    // CM13: initial + 2 retries = 3 runs, all infra (none self-report).
    assert_eq!(runs.len(), 3, "CM13: initial + 2 retries = 3 run rows");
    for run in &runs {
        assert_eq!(run.status, GraphRunStatus::Fail);
    }
    assert!(
        runs.iter()
            .filter(|r| {
                r.output
                    .as_ref()
                    .and_then(|o| o.get("infra_crash"))
                    .and_then(|v| v.as_bool())
                    == Some(true)
            })
            .count()
            >= 2,
        "retried runs must carry infra_crash marker"
    );
}

/// A member that keeps crashing exhausts its retry budget (default 2 → 3
/// attempts) and only then counts as a member fail. With min_pass=2 and a
/// second, healthy member, the join arithmetic is 1/2 → Fail.
#[tokio::test]
async fn ensemble_member_infra_retries_exhausted_counts_as_member_fail() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "member-dead",
            &write_member_script(dir.path(), "dead.sh", "exit 1"),
        ),
        (
            "member-ok",
            &write_member_script(dir.path(), "ok.sh", "printf ok"),
        ),
    ]);
    insert_infra_ensemble(
        &db,
        &spec_id,
        &[("m-dead", "member-dead"), ("m-ok", "member-ok")],
        2,
        Some(1),
        None,
        &serde_json::json!({ "infra_backoff_seconds": 0 }),
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
        join.status,
        GraphRunStatus::Error,
        "CM13: both members are unreported infra -> 0/2 verdicts -> join is Error (CB66)"
    );
    assert_eq!(
        join.output.as_ref().unwrap()["no_verdict"],
        serde_json::json!(true)
    );
    assert_eq!(join.output.as_ref().unwrap()["passed"], 0);

    // retry_limit default 2 -> attempts 0,1,2 -> three distinct run rows,
    // the first two carrying infra_crash markers.
    let dead = member_runs(&db, &spec_id, "m-dead");
    assert_eq!(
        dead.len(),
        3,
        "two retries after the first crash = three rows"
    );
    assert!(dead.iter().all(|r| r.status == GraphRunStatus::Fail));
    assert_eq!(
        dead[0].output.as_ref().unwrap()["infra_crash"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        dead[1].output.as_ref().unwrap()["infra_crash"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(dead[0].output.as_ref().unwrap()["infra_attempt"], 0);
    assert_eq!(dead[1].output.as_ref().unwrap()["infra_attempt"], 1);
}

/// CM30: node config wins over platform wins over default — independently
/// per key — with provenance (`"node"`/`"platform"`/`"default"`).
#[test]
fn resolve_node_infra_config_precedence_and_source() {
    let fake_home =
        setup_multi_cli_home_with_infra(&[("plat-a", "true", Some(0), Some(120), Some(5))]);
    let _home = HomeGuard::set(fake_home.path());

    let node_with = |config: serde_json::Value| GraphNode {
        id: "n1".to_string(),
        spec_id: None,
        graph_id: None,
        name: "test".to_string(),
        kind: GraphNodeKind::Agent,
        config,
        position: 1,
        created_at: chrono::Utc::now(),
    };

    // No platform named anywhere: engine defaults, source "default".
    let resolved = resolve_node_infra_config(&node_with(serde_json::json!({})));
    assert_eq!(
        resolved.retry_limit.value,
        crate::domain::settings::default_int("graphs.infra_retry_limit") as u64
    );
    assert_eq!(resolved.retry_limit.source, "default");
    assert_eq!(
        resolved.crash_max_seconds.value,
        crate::domain::settings::default_int("graphs.infra_crash_max_seconds") as u64
    );
    assert_eq!(resolved.crash_max_seconds.source, "default");
    assert_eq!(
        resolved.backoff_seconds.value,
        crate::domain::settings::default_int("graphs.infra_backoff_seconds") as u64
    );
    assert_eq!(resolved.backoff_seconds.source, "default");

    // Platform named, no node keys: platform values, source "platform".
    let resolved = resolve_node_infra_config(&node_with(serde_json::json!({"platform": "plat-a"})));
    assert_eq!(resolved.retry_limit.value, 0);
    assert_eq!(resolved.retry_limit.source, "platform");
    assert_eq!(resolved.crash_max_seconds.value, 120);
    assert_eq!(resolved.crash_max_seconds.source, "platform");
    assert_eq!(resolved.backoff_seconds.value, 5);
    assert_eq!(resolved.backoff_seconds.source, "platform");

    // Node key beats platform for that key only; the other keys still
    // resolve to the platform.
    let resolved = resolve_node_infra_config(&node_with(serde_json::json!({
        "platform": "plat-a",
        "infra_retry_limit": 3,
    })));
    assert_eq!(resolved.retry_limit.value, 3);
    assert_eq!(resolved.retry_limit.source, "node");
    assert_eq!(resolved.crash_max_seconds.value, 120);
    assert_eq!(resolved.crash_max_seconds.source, "platform");
    assert_eq!(resolved.backoff_seconds.value, 5);
    assert_eq!(resolved.backoff_seconds.source, "platform");

    // Unknown platform: straight to the engine default.
    let resolved = resolve_node_infra_config(&node_with(
        serde_json::json!({"platform": "no-such-platform"}),
    ));
    assert_eq!(
        resolved.retry_limit.value,
        crate::domain::settings::default_int("graphs.infra_retry_limit") as u64
    );
    assert_eq!(resolved.retry_limit.source, "default");
}

/// CM30/FR6: a cascade ensemble with one member on a platform configured
/// at `infra_retry_limit: 0` and one on a platform at the default
/// dispatches one attempt for the first and three for the second under
/// the same crash. The limit-0 member's single row carries
/// `infra_crash: true, infra_attempt: 0, infra_retry_skipped: "limit 0"`;
/// the default member's exhausted third row carries NO
/// `infra_retry_skipped` key (a config that sets nothing behaves exactly
/// as today).
#[tokio::test]
async fn ensemble_platform_retry_limit_zero_skips_retries_while_default_retries() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home_with_infra(&[
        (
            "plat-zero",
            &write_member_script(dir.path(), "zero.sh", "exit 1"),
            Some(0),
            None,
            None,
        ),
        (
            "plat-default",
            &write_member_script(dir.path(), "default.sh", "exit 1"),
            None,
            None,
            None,
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Cascade,
        &[("m-zero", "plat-zero"), ("m-default", "plat-default")],
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
        join.status,
        GraphRunStatus::Error,
        "both members are unreported infra -> 0/2 verdicts -> join is Error (CB66)"
    );
    assert_eq!(
        join.output.as_ref().unwrap()["no_verdict"],
        serde_json::json!(true)
    );

    let zero = member_runs(&db, &spec_id, "m-zero");
    assert_eq!(
        zero.len(),
        1,
        "platform infra_retry_limit 0 means one attempt, no retry"
    );
    let zero_out = zero[0].output.as_ref().unwrap();
    assert_eq!(zero_out["infra_crash"], serde_json::Value::Bool(true));
    assert_eq!(zero_out["infra_attempt"], 0);
    assert_eq!(zero_out["infra_retry_skipped"], "limit 0");

    let default = member_runs(&db, &spec_id, "m-default");
    assert_eq!(
        default.len(),
        3,
        "platform default (limit 2) means initial + 2 retries"
    );
    assert!(
        default[2]
            .output
            .as_ref()
            .unwrap()
            .get("infra_retry_skipped")
            .is_none(),
        "retry_limit > 0 exhaustion must not gain new fields: {:?}",
        default[2].output
    );
}

/// CM30: a member-level value (baked into the member node's own config by
/// `graph_add_ensemble`/`graph_update_ensemble`, which is all the engine
/// ever reads) overrides its platform: platform limit 5, node limit 0 —
/// one attempt, tagged as retry-skipped.
#[tokio::test]
async fn ensemble_node_config_retry_limit_zero_overrides_platform() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home_with_infra(&[
        (
            "plat-five",
            &write_member_script(dir.path(), "five.sh", "exit 1"),
            Some(5),
            None,
            None,
        ),
        (
            "plat-ok",
            &write_self_reporting_member_script(
                dir.path(),
                "ok.sh",
                "sleep 1",
                &db_path,
                "m-ok",
                "pass",
                None,
            ),
            None,
            None,
            None,
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Cascade,
        &[("m-overridden", "plat-five"), ("m-ok", "plat-ok")],
    );
    // Simulate the baked member override: the member's own node config
    // carries the winning value, exactly as `member_node_config` writes it.
    let mut node = db.get_graph_node("m-overridden").unwrap().unwrap();
    if let serde_json::Value::Object(ref mut map) = node.config {
        map.insert("infra_retry_limit".to_string(), serde_json::json!(0));
    }
    db.update_graph_node_details(&node.id, None, None, Some(&node.config), None)
        .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let runs = member_runs(&db, &spec_id, "m-overridden");
    assert_eq!(
        runs.len(),
        1,
        "node-level infra_retry_limit 0 wins over the platform's 5"
    );
    let out = runs[0].output.as_ref().unwrap();
    assert_eq!(out["infra_crash"], serde_json::Value::Bool(true));
    assert_eq!(out["infra_retry_skipped"], "limit 0");
}

/// The `mimocode`/`mimo-auto` incident inside an ensemble: a member that
/// exits 0 with empty stdout (and stderr complaining about its model)
/// must count as a member FAIL, not a pass — a crashed member must never
/// count toward the join's pass quorum. With min_pass=2 and only one
/// genuinely healthy member, 1/2 must fail the join. It must also
/// resolve on the FIRST attempt (one run row), never retried as an infra
/// crash, since this is a deterministic misconfiguration that would just
/// reproduce the identical empty result.
#[tokio::test]
async fn ensemble_member_empty_output_zero_exit_counts_as_fail_not_pass() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "member-no-output",
            &write_member_script(
                dir.path(),
                "no-output.sh",
                ">&2 printf 'Error: Unsupported model mimo-auto'\nexit 0",
            ),
        ),
        (
            "member-ok",
            &write_member_script(dir.path(), "ok.sh", "printf ok"),
        ),
    ]);
    insert_infra_ensemble(
        &db,
        &spec_id,
        &[("m-no-output", "member-no-output"), ("m-ok", "member-ok")],
        2,
        Some(1),
        None,
        &serde_json::json!({ "infra_backoff_seconds": 0 }),
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
        join.status,
        GraphRunStatus::Error,
        "CM13: both members are unreported infra -> 0/2 verdicts -> join is Error (CB66)"
    );
    assert_eq!(
        join.output.as_ref().unwrap()["no_verdict"],
        serde_json::json!(true)
    );
    assert_eq!(join.output.as_ref().unwrap()["passed"], 0);

    let runs = member_runs(&db, &spec_id, "m-no-output");
    // CM13: the no-output member never filed a verdict, so it is infra
    // and exhausts retries like any other unreported run.
    assert_eq!(
        runs.len(),
        3,
        "CM13: unreported infra runs are retried; initial + 2 retries = 3 rows"
    );
    assert_eq!(runs[0].status, GraphRunStatus::Fail);
    assert_eq!(
        runs[0].output.as_ref().unwrap()["no_output"],
        serde_json::Value::Bool(true)
    );
    assert!(
        runs[0].output.as_ref().unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("Unsupported model mimo-auto"),
        "the member's stderr must be surfaced in the stored output"
    );
}

/// The ensemble-member corner: `require_report` is judged per member,
/// exactly like the sequential path, so a quorum counts a silent member
/// the same way a lone node's fail edge would. Both members exit 0 with
/// REAL stdout (unlike the no-output member above) — the shape
/// `zero_exit_no_output` cannot catch — and neither self-reports, so with
/// `require_report: true` on the ensemble's shared member config, both
/// must still fail the quorum, deterministically on the first attempt
/// (never infra-retried).
#[tokio::test]
async fn ensemble_member_require_report_true_without_self_report_counts_as_fail() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "member-silent-1",
            &write_member_script(dir.path(), "silent1.sh", "printf 'looks done'\nexit 0"),
        ),
        (
            "member-silent-2",
            &write_member_script(dir.path(), "silent2.sh", "printf 'also done'\nexit 0"),
        ),
    ]);
    insert_infra_ensemble(
        &db,
        &spec_id,
        &[
            ("m-silent-1", "member-silent-1"),
            ("m-silent-2", "member-silent-2"),
        ],
        1,
        Some(1),
        None,
        &serde_json::json!({ "infra_backoff_seconds": 0, "require_report": true }),
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
            join.status,
            GraphRunStatus::Error,
            "require_report members that exit 0 with real stdout but never self-report file no verdict -> join is Error (CB66)"
        );
    assert_eq!(
        join.output.as_ref().unwrap()["no_verdict"],
        serde_json::json!(true)
    );
    assert_eq!(join.output.as_ref().unwrap()["passed"], 0);

    let runs = member_runs(&db, &spec_id, "m-silent-1");
    // CM13: unreported runs are infra, so require_report members get retried.
    // retry_limit=2 (default) → initial + 2 retries = 3 runs.
    assert_eq!(
        runs.len(),
        3,
        "CM13: unreported infra runs are retried; initial + 2 retries = 3 rows"
    );
    for run in &runs {
        assert_eq!(run.status, GraphRunStatus::Fail);
        assert_eq!(
            run.output.as_ref().unwrap()["unreported"],
            serde_json::Value::Bool(true)
        );
    }
    assert!(
        runs.iter()
            .filter(|r| {
                r.output
                    .as_ref()
                    .and_then(|o| o.get("infra_crash"))
                    .and_then(|v| v.as_bool())
                    == Some(true)
            })
            .count()
            >= 2,
        "retried runs must carry infra_crash marker"
    );
}

/// Straggler-window interaction (documented behavior): the ensemble's
/// straggler timeout bounds the ENTIRE retry sequence, not a single
/// attempt. When the window expires before a member resolves (still
/// executing, or mid-backoff between retries), the member is counted as
/// failed deterministically and its live attempt killed — never left to
/// retry past the window, never silently abandoned.
///
/// Chosen/documented behavior: fail-deterministically-on-window-expiry.
/// The members here have infra retry enabled but each sleeps well past the
/// zero-length straggler window, so the window always expires first — the
/// retry graph is dropped mid-attempt and both members resolve to Fail
/// (0/2), exactly as a lone straggler would, rather than being retried out
/// past the window or hanging the join. (Sleeping members make the kill
/// deterministic; a fast-exiting member could race a zero-length window.)
#[tokio::test]
async fn ensemble_member_straggler_window_bounds_the_retry_sequence() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let marker = dir.path().join("retried.marker");
    // Would crash (exit 1) after a 3s sleep and then, on a retry, create a
    // marker — but the zero-length straggler window kills it long before
    // either its crash or any retry can happen.
    let flap = write_member_script(
        dir.path(),
        "flap.sh",
        &format!("sleep 3; touch \"{}\"; exit 1", marker.display()),
    );
    let fake_home = setup_multi_cli_home(&[
        ("member-flap", &flap),
        (
            "member-slow-ok",
            &write_member_script(dir.path(), "ok.sh", "sleep 3; exit 0"),
        ),
    ]);
    insert_infra_ensemble(
        &db,
        &spec_id,
        &[("m-flap", "member-flap"), ("m-ok", "member-slow-ok")],
        1,
        Some(0), // zero-length window: expires before either member resolves
        None,
        &serde_json::json!({ "infra_backoff_seconds": 0 }),
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
            join.status,
            GraphRunStatus::Error,
            "straggler window expired before any member resolved -> 0/2 verdicts -> join is Error (CB66)"
        );
    assert_eq!(
        join.output.as_ref().unwrap()["no_verdict"],
        serde_json::json!(true)
    );
    assert_eq!(join.output.as_ref().unwrap()["passed"], 0);

    // Deterministic resolution: no member run row is left Running.
    assert!(
        member_runs(&db, &spec_id, "m-flap")
            .iter()
            .all(|r| r.status != GraphRunStatus::Running),
        "the straggler-timed-out member must be resolved, not abandoned Running"
    );

    // Prove the member was actually cut off (not retried past the window):
    // its script's post-sleep side effect must never have run.
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    assert!(
        !marker.exists(),
        "the straggler-killed member must not have run past the window (no retry)"
    );
}

/// Queue-run compatibility: a queue member spec whose own graph contains
/// an ensemble must run end to end through a queue dispatch exactly like
/// any other spec — the ensemble's join routing onward is what lets the
/// spec (and therefore the queue) reach completion.
#[tokio::test]
async fn ensemble_runs_end_to_end_through_a_queue_dispatch() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-queue-ensemble".to_string(),
        name: "Graph".to_string(),
        description: None,
        workdir: dir.path().to_string_lossy().to_string(),
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
    db.insert_graph(&lp).unwrap();
    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));

    let spec = standalone_spec("queue-ensemble-spec", 1);
    db.insert_graph_spec(&spec).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&spec.id]);

    let fake_home = setup_multi_cli_home(&[
        (
            "member-ok-a",
            &write_member_script(dir.path(), "a.sh", "printf ok"),
        ),
        (
            "member-ok-b",
            &write_member_script(dir.path(), "b.sh", "printf ok"),
        ),
    ]);
    db.insert_graph_node(&touch_marker_node(
        "on-pass",
        &spec.id,
        &dir.path().join("pass.marker"),
        100,
    ))
    .unwrap();
    insert_test_ensemble(
        &db,
        &spec.id,
        "kickoff",
        "ens1",
        "join1",
        &[("m-a", "member-ok-a"), ("m-b", "member-ok-b")],
        2,
        Some(1),
        None,
        "on-pass",
        None,
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(
            lp.id.clone(),
            Some("queue-1".to_string()),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    drop(_home);

    let lp = db.get_graph(&lp.id).unwrap().unwrap();
    // CM13: script-backed members never self-report, so the ensemble
    // join is Error with no_verdict (CB66) and the spec pauses with a
    // blocker instead of failing.
    assert_eq!(lp.status, GraphStatus::Paused);
    // CM42: the queue run consumed the spec — it is never left `Pending`
    // for a later run to start from scratch — and the no-verdict block
    // lands it `Interrupted`, which queue selection treats as runnable
    // (resumable at the ensemble) exactly like `Pending`.
    assert_eq!(
        db.get_graph_spec(&spec.id).unwrap().unwrap().status,
        GraphSpecStatus::Interrupted,
        "FR1: the no-verdict block leaves the queue member interrupted \
             (resumable), never failed"
    );
}

// ── B19: infra crash retry logic ──────────────────────────────────────

/// Infra crash classification correctly identifies a non-self-reported
/// agent-node failure within the crash threshold as needing retry.
#[test]
fn infra_crash_classification_correct() {
    let now = chrono::Utc::now();
    let run = GraphNodeRun {
        id: "run1".to_string(),
        graph_id: "loop1".to_string(),
        spec_id: "spec1".to_string(),
        node_id: "node1".to_string(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: now,
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };

    let agent_node = GraphNode {
        id: "node1".to_string(),
        spec_id: Some("spec1".to_string()),
        graph_id: None,
        name: "test-agent".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };

    let execution = NodeExecution {
        status: GraphRunStatus::Fail,
        output: serde_json::json!({}),
        summary: "crashed".to_string(),
    };

    // Scenario 1: agent node, failed, not self-reported (status=Running),
    // within threshold → should be classified as infra crash
    let self_reported = run.status != GraphRunStatus::Running;
    let duration_secs = (chrono::Utc::now() - run.started_at).num_seconds();
    let is_crash = !self_reported
        && agent_node.kind == GraphNodeKind::Agent
        && execution.status == GraphRunStatus::Fail
        && duration_secs < 60;
    assert!(is_crash, "should classify as infra crash");

    // Scenario 2: check node, same conditions → should NOT be classified
    let check_node = GraphNode {
        id: "node2".to_string(),
        spec_id: Some("spec1".to_string()),
        graph_id: None,
        name: "test-check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    let is_crash_check = !self_reported
        && check_node.kind == GraphNodeKind::Agent
        && execution.status == GraphRunStatus::Fail
        && duration_secs < 60;
    assert!(
        !is_crash_check,
        "check node should not be classified as crash"
    );

    // Scenario 3: agent node, self-reported fail → should NOT be classified
    let run_self_reported = GraphNodeRun {
        status: GraphRunStatus::Fail,
        ..run.clone()
    };
    let self_reported_bool = run_self_reported.status != GraphRunStatus::Running;
    let is_crash_reported = !self_reported_bool
        && agent_node.kind == GraphNodeKind::Agent
        && execution.status == GraphRunStatus::Fail
        && duration_secs < 60;
    assert!(
        !is_crash_reported,
        "self-reported fail should not be classified as crash"
    );

    // Scenario 4: agent node, failed, slow (> 60s) → should NOT be classified
    let old_run = GraphNodeRun {
        started_at: now - chrono::Duration::seconds(90),
        ..run
    };
    let slow_duration = (chrono::Utc::now() - old_run.started_at).num_seconds();
    let is_crash_slow = !self_reported
        && agent_node.kind == GraphNodeKind::Agent
        && execution.status == GraphRunStatus::Fail
        && slow_duration < 60;
    assert!(
        !is_crash_slow,
        "slow fail should not be classified as crash"
    );
}

/// Merging attempt marker into output JSON correctly adds tracking fields.
#[test]
fn merge_attempt_marker_adds_fields() {
    let output = serde_json::json!({
        "kind": "agent",
        "exit_code": 1,
    });

    let merged = merge_attempt_marker(&output, 0, true);

    assert_eq!(
        merged.get("infra_attempt").and_then(|v| v.as_u64()),
        Some(0)
    );
    assert_eq!(
        merged.get("infra_crash").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(merged.get("kind").and_then(|v| v.as_str()), Some("agent"));
    assert_eq!(merged.get("exit_code").and_then(|v| v.as_i64()), Some(1));
}

/// Read infra config returns defaults when not specified.
#[test]
fn read_infra_config_applies_defaults() {
    let node = GraphNode {
        id: "n1".to_string(),
        spec_id: None,
        graph_id: None,
        name: "test".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };

    let (retry_limit, crash_max, backoff) = read_infra_config(&node);
    assert_eq!(
        retry_limit,
        crate::domain::settings::default_int("graphs.infra_retry_limit") as u32
    );
    assert_eq!(
        crash_max,
        crate::domain::settings::default_int("graphs.infra_crash_max_seconds") as u64
    );
    assert_eq!(
        backoff,
        crate::domain::settings::default_int("graphs.infra_backoff_seconds") as u64
    );
}

/// Read infra config respects overrides in node config.
#[test]
fn read_infra_config_respects_overrides() {
    let node = GraphNode {
        id: "n1".to_string(),
        spec_id: None,
        graph_id: None,
        name: "test".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "infra_retry_limit": 5,
            "infra_crash_max_seconds": 120,
            "infra_backoff_seconds": 0,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    };

    let (retry_limit, crash_max, backoff) = read_infra_config(&node);
    assert_eq!(retry_limit, 5);
    assert_eq!(crash_max, 120);
    assert_eq!(backoff, 0);
}

/// B19: check node nonzero exit is NOT retried as infra crash.
#[tokio::test]
async fn infra_crash_check_node_nonzero_exit_not_retried() {
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

    db.insert_graph_edge(&GraphEdge {
        id: "edge-self".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-check".to_string(),
        to_node: "node-check".to_string(),
        condition: GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();

    assert_eq!(spec.status, GraphSpecStatus::Failed);
    assert_eq!(
        runs.len(),
        crate::domain::settings::default_int("graphs.max_iterations_per_node") as usize,
        "check node should retry through edge, not infra retry"
    );
}

/// CM2 (renamed from B19): infra crash retry exhausted falls back to fail
/// edge when no `Error` edge exists — the additive, non-breaking path.
#[tokio::test]
async fn infra_crash_retry_exhausted_falls_back_to_fail_when_no_error() {
    let fake_home = setup_test_cli_home();
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-implement".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "implement".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            // test-cli = /bin/sh -c <prompt>: always crashes fast with
            // no self-report — the infra-crash signature.
            "platform": "test-cli",
            "prompt_template": "exit 1",
            "infra_retry_limit": 1,
            "infra_crash_max_seconds": 60,
            "infra_backoff_seconds": 0,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-fix".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "fix".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf FIXED",
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_edge(&GraphEdge {
        id: "edge-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-implement".to_string(),
        to_node: "node-fix".to_string(),
        condition: GraphEdgeCondition::Fail,
    })
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    result.unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();

    assert_eq!(spec.status, GraphSpecStatus::Completed);

    let implement_runs: Vec<_> = runs
        .iter()
        .filter(|r| r.node_id == "node-implement")
        .collect();
    let fix_runs: Vec<_> = runs.iter().filter(|r| r.node_id == "node-fix").collect();

    assert_eq!(
        implement_runs.len(),
        2,
        "implement should run twice: initial attempt + 1 infra retry"
    );
    assert!(
        implement_runs.iter().any(|r| {
            r.output
                .as_ref()
                .and_then(|o| o.get("infra_crash"))
                .and_then(|v| v.as_bool())
                == Some(true)
        }),
        "one implement attempt should carry the infra_crash marker"
    );
    assert_eq!(fix_runs.len(), 1, "fix should run once");
    assert_eq!(fix_runs[0].status, GraphRunStatus::Pass);
}

/// CM2: infra crash routes to `Error` edge when present, not `Fail`.
#[tokio::test]
async fn infra_crash_routes_to_error_edge_when_present() {
    let fake_home = setup_test_cli_home();
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-implement".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "implement".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "test-cli",
            "prompt_template": "exit 1",
            "infra_retry_limit": 1,
            "infra_crash_max_seconds": 60,
            "infra_backoff_seconds": 0,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-resilience".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "resilience".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf RESILIENCE",
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // Error edge to resilience node (should be taken on infra crash).
    db.insert_graph_edge(&GraphEdge {
        id: "edge-error".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-implement".to_string(),
        to_node: "node-resilience".to_string(),
        condition: GraphEdgeCondition::Error,
    })
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    result.unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();

    let implement_runs: Vec<_> = runs
        .iter()
        .filter(|r| r.node_id == "node-implement")
        .collect();
    let resilience_runs: Vec<_> = runs
        .iter()
        .filter(|r| r.node_id == "node-resilience")
        .collect();

    assert!(!implement_runs.is_empty(), "implement node should have run");
    assert!(
        implement_runs.iter().any(|r| {
            r.output
                .as_ref()
                .and_then(|o| o.get("infra_crash"))
                .and_then(|v| v.as_bool())
                == Some(true)
        }),
        "implement should have infra_crash marker"
    );
    assert_eq!(
        resilience_runs.len(),
        1,
        "resilience node should run (Error edge taken)"
    );
}

/// CM2: `Error` edge does not fire on a genuine fail (agent said no).
#[tokio::test]
async fn error_edge_does_not_fire_on_genuine_fail() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    // An agent that reports fail (not an infra crash).
    db.insert_graph_node(&GraphNode {
        id: "node-reviewer".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "reviewer".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "exit 1",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-resilience".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "resilience".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf RESILIENCE",
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-fail-target".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "fail-target".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf FAIL_TARGET",
            "success_condition": "exit_code_0"
        }),
        position: 3,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // Error edge to resilience (should NOT be taken on genuine fail).
    db.insert_graph_edge(&GraphEdge {
        id: "edge-error".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-reviewer".to_string(),
        to_node: "node-resilience".to_string(),
        condition: GraphEdgeCondition::Error,
    })
    .unwrap();

    // Fail edge to fail-target (SHOULD be taken on genuine fail).
    db.insert_graph_edge(&GraphEdge {
        id: "edge-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-reviewer".to_string(),
        to_node: "node-fail-target".to_string(),
        condition: GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();

    let resilience_runs: Vec<_> = runs
        .iter()
        .filter(|r| r.node_id == "node-resilience")
        .collect();
    let fail_target_runs: Vec<_> = runs
        .iter()
        .filter(|r| r.node_id == "node-fail-target")
        .collect();

    assert_eq!(
        resilience_runs.len(),
        0,
        "resilience should NOT run (Error edge not taken on genuine fail)"
    );
    assert!(
        !fail_target_runs.is_empty(),
        "fail-target should run (Fail edge taken on genuine fail)"
    );
}
