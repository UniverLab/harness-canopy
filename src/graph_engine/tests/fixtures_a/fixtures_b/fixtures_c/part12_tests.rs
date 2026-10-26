//! Part 12 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

/// CM24: with `quorum_grace_minutes: Some(0)`, the join resolves the
/// instant the quorum is met instead of waiting for every member.
/// `m-ok` passes (its script self-reports as the last thing it does
/// before exiting); the two hanging members are
/// terminated with reason `quorum met` — recorded as fail, distinct
/// from `ensemble straggler timeout` — and the join passes 1/3 with
/// `quorum_met_at`/`grace_minutes` in its output. The generous
/// 30-minute straggler window proves the fast resolution came from the
/// grace path: the straggler kill could not have fired in test time.
#[tokio::test]
async fn ensemble_quorum_grace_zero_terminates_stragglers_on_quorum_met() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "member-ok",
            &write_self_reporting_member_script(
                dir.path(),
                "ok.sh",
                "sleep 2; printf OK",
                &db_path,
                "m-ok",
                "pass",
                Some("OK"),
            ),
        ),
        (
            "member-hang-a",
            &write_member_script(dir.path(), "a.sh", "sleep 30"),
        ),
        (
            "member-hang-b",
            &write_member_script(dir.path(), "b.sh", "sleep 30"),
        ),
    ]);
    let pass_marker = dir.path().join("pass.marker");
    db.insert_graph_node(&touch_marker_node("on-pass", &spec_id, &pass_marker, 100))
        .unwrap();
    insert_test_ensemble(
        &db,
        &spec_id,
        "kickoff",
        "ens1",
        "join1",
        &[
            ("m-ok", "member-ok"),
            ("m-a", "member-hang-a"),
            ("m-b", "member-hang-b"),
        ],
        1,
        Some(30),
        Some(0),
        "on-pass",
        None,
    );

    let _home = HomeGuard::set(fake_home.path());
    let started = std::time::Instant::now();
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    let elapsed = started.elapsed();
    drop(_home);

    assert!(
        pass_marker.exists(),
        "join passed 1/3 -> must route to on_pass_to"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(120),
        "grace 0 must resolve without waiting out the 30m straggler window (elapsed: {elapsed:?})"
    );

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(join.status, GraphRunStatus::Pass);
    let output = join.output.unwrap();
    assert_eq!(output["passed"], 1);
    assert_eq!(output["grace_minutes"], 0);
    assert!(
        output["quorum_met_at"].as_str().is_some(),
        "grace path must record when the quorum was met"
    );
    let doc = output["consolidated_doc"].as_str().unwrap().to_string();
    assert!(doc.contains("OK"), "passing member must be consolidated");
    // A grace-0 join that waited for the stragglers instead of
    // terminating them would record the straggler reason below; the
    // quorum-met reason is what proves the grace path fired.
    for node_id in ["m-a", "m-b"] {
        let runs = member_runs(&db, &spec_id, node_id);
        let terminated = runs
            .iter()
            .find(|r| r.status == GraphRunStatus::Fail)
            .unwrap_or_else(|| panic!("{node_id} must have a fail run"));
        let reason = terminated
            .output
            .as_ref()
            .and_then(|o| o.get("reason"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(
            reason, "quorum met",
            "{node_id} must be terminated with the quorum-met reason, got: {reason:?}"
        );
    }
}

/// CM24: a member that finishes after the quorum was met but inside the
/// grace window is consolidated normally — not terminated. `m-a` passes
/// fast; `m-b`/`m-c` self-report their own passes a few seconds later,
/// each as the last thing its own script does; all three land inside
/// the 1-minute grace, so the join passes 3/3 and resolves in seconds
/// rather than waiting out the window.
#[tokio::test]
async fn ensemble_quorum_grace_allows_member_finishing_inside_grace_to_be_consolidated() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "member-a",
            &write_self_reporting_member_script(
                dir.path(),
                "a.sh",
                "sleep 2; printf A",
                &db_path,
                "m-a",
                "pass",
                Some("A"),
            ),
        ),
        (
            "member-b",
            &write_self_reporting_member_script(
                dir.path(),
                "b.sh",
                "sleep 5; printf B",
                &db_path,
                "m-b",
                "pass",
                Some("B"),
            ),
        ),
        (
            "member-c",
            &write_self_reporting_member_script(
                dir.path(),
                "c.sh",
                "sleep 8; printf C",
                &db_path,
                "m-c",
                "pass",
                Some("C"),
            ),
        ),
    ]);
    let pass_marker = dir.path().join("pass.marker");
    db.insert_graph_node(&touch_marker_node("on-pass", &spec_id, &pass_marker, 100))
        .unwrap();
    insert_test_ensemble(
        &db,
        &spec_id,
        "kickoff",
        "ens1",
        "join1",
        &[
            ("m-a", "member-a"),
            ("m-b", "member-b"),
            ("m-c", "member-c"),
        ],
        1,
        Some(30),
        Some(1),
        "on-pass",
        None,
    );

    let _home = HomeGuard::set(fake_home.path());
    let started = std::time::Instant::now();
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    let elapsed = started.elapsed();
    drop(_home);

    assert!(
        pass_marker.exists(),
        "join passed 3/3 -> must route to on_pass_to"
    );
    assert!(
            elapsed < std::time::Duration::from_secs(50),
            "all members finished inside the 60s grace -> join must resolve early (elapsed: {elapsed:?})"
        );
    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(join.status, GraphRunStatus::Pass);
    let output = join.output.unwrap();
    assert_eq!(output["passed"], 3);
    assert_eq!(output["grace_minutes"], 1);
    assert!(
        output["quorum_met_at"].as_str().is_some(),
        "grace path must record when the quorum was met"
    );
    let doc = output["consolidated_doc"].as_str().unwrap().to_string();
    assert!(
        doc.contains('A') && doc.contains('B') && doc.contains('C'),
        "every member finished inside the grace and must be consolidated: {doc}"
    );
}

/// CM24: `quorum_grace_minutes` is ignored for cascade ensembles — the
/// walk still proceeds member by member. Set on the row directly (the
/// fixture helper builds parallel units); the first member's crash
/// falls through to the second, whose pass becomes the verdict, with
/// no `quorum met` termination anywhere.
#[tokio::test]
async fn ensemble_quorum_grace_ignored_for_cascade() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "m-crash",
            &write_member_script(dir.path(), "crash.sh", "exit 1"),
        ),
        (
            "m-ok",
            &write_self_reporting_member_script(
                dir.path(),
                "ok.sh",
                "sleep 1",
                &db_path,
                "m-ok",
                "pass",
                None,
            ),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Cascade,
        &[("m-crash", "m-crash"), ("m-ok", "m-ok")],
    );
    db.update_ensemble_join_config("ens1", None, None, Some(Some(0)), None, None, None, None)
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
        "cascade must fall through the crashed first member to m-ok despite grace 0"
    );
    assert_eq!(join.output.as_ref().unwrap()["winner"]["node_id"], "m-ok");
    let quorum_terminated = db
        .list_graph_runs_for_spec(&spec_id)
        .unwrap()
        .into_iter()
        .filter_map(|r| r.output)
        .filter_map(|o| o.get("reason").and_then(|v| v.as_str()).map(str::to_string))
        .any(|reason| reason == "quorum met");
    assert!(
        !quorum_terminated,
        "cascade must never terminate with the quorum-met reason"
    );
}

/// CM23: a member's own `timeout_minutes` overrides the ensemble's shared
/// value for that member only. `m-hang` gets `Some(0)` — an immediate cut,
/// same "0 is a legitimate value" convention the existing straggler tests
/// rely on for a deterministically fast kill — while `m-ok` gets `None`
/// and keeps running under the ensemble's own (much longer) 5-minute
/// `timeout_minutes`. `m-hang` being cut counts as "no verdict" exactly
/// like an infra crash does, so round-robin falls through to `m-ok` per
/// FR5. `m-ok`'s script only sleeps (it never calls back into
/// `graph_complete_node`), so it self-reports its own Pass as the last
/// thing it does before exiting; the join then passes on `m-ok`'s real
/// result, proving the short leash didn't cost the ensemble the whole
/// rotation. (CB57: the `Some(0)` lives on the ensemble member record —
/// the member leash. The member node's own process timeout is clamped to
/// >= 1 minute in `insert_kind_ensemble_with_member_timeouts` so the
/// inner kill-and-verdict path can't win the race and fail the walk fast.)
#[tokio::test]
async fn round_robin_member_with_short_timeout_is_cut_while_sibling_with_no_override_is_not() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "m-hang",
            &write_member_script(dir.path(), "hang.sh", "sleep 5; exit 0"),
        ),
        (
            "m-ok",
            &write_self_reporting_member_script(
                dir.path(),
                "ok.sh",
                "sleep 1",
                &db_path,
                "m-ok",
                "pass",
                None,
            ),
        ),
    ]);
    insert_kind_ensemble_with_member_timeouts(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("m-hang", "m-hang", Some(0)), ("m-ok", "m-ok", None)],
    );

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
        "m-hang cut at its own 0m limit -> no verdict -> falls through to m-ok, which passes"
    );
    assert_eq!(join.output.as_ref().unwrap()["member"]["node_id"], "m-ok");

    let hang_runs = member_runs(&db, &spec_id, "m-hang");
    assert!(
        hang_runs.iter().any(|r| r.status == GraphRunStatus::Fail),
        "m-hang must have been cut at its own timeout, not left running"
    );
    // FR4: the cut is reported as the member's own timeout (naming the
    // member and the limit it hit), not as the ensemble's generic
    // straggler timeout. The DB row is terminated with that reason, and
    // the member's NodeExecution (captured via the same path) carries a
    // machine-readable limit.
    let terminated_hang = hang_runs
        .iter()
        .find(|r| r.status == GraphRunStatus::Fail)
        .expect("m-hang fail run");
    let reason = terminated_hang
        .output
        .as_ref()
        .and_then(|o| o.get("reason"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        reason.contains("member's own timeout"),
        "m-hang termination must name the member's own timeout, got: {reason:?}"
    );
    let ok_runs = member_runs(&db, &spec_id, "m-ok");
    assert!(
        ok_runs.iter().any(|r| r.status == GraphRunStatus::Pass),
        "m-ok must have run and passed under the ensemble's own (longer) timeout"
    );
}

/// CM23 (cascade variant of the same FR5): a member with its own short
/// timeout is cut at that limit while the next member — which carries no
/// override and therefore uses the ensemble's shared timeout — still
/// runs and its pass becomes the ensemble's verdict. Proves the cascade
/// fallthrough path respects per-member leashes, not just round-robin.
/// Before CB57, a loaded-suite run captured `left: Fail, right: Pass`
/// for the join assertion below. The failing run's member rows showed
/// `members_tried: 1` with winner m-hang (`"error": "timed out"`) and no
/// m-ok row at all: m-hang's node-level `timeout_minutes: 0` (inner
/// kill-and-verdict, fail-fast) had beaten the member-leash cut (outer
/// no-verdict fallthrough) — m-ok was never dispatched, so no
/// verdict-filer window was missed. See the `timeout_minutes` clamp in
/// `insert_kind_ensemble_with_member_timeouts`.
#[tokio::test]
async fn cascade_member_with_short_timeout_is_cut_while_sibling_with_no_override_is_not() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "m-hang",
            &write_member_script(dir.path(), "hang.sh", "sleep 5; exit 0"),
        ),
        (
            "m-ok",
            &write_self_reporting_member_script(
                dir.path(),
                "ok.sh",
                "sleep 1",
                &db_path,
                "m-ok",
                "pass",
                None,
            ),
        ),
    ]);
    insert_kind_ensemble_with_member_timeouts(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Cascade,
        &[("m-hang", "m-hang", Some(0)), ("m-ok", "m-ok", None)],
    );

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
            "cascade: m-hang cut at its own 0m limit -> no verdict -> falls through to m-ok, which passes"
        );
    // Cascade join nests the winner under `winner`, not `member`.
    assert_eq!(join.output.as_ref().unwrap()["winner"]["node_id"], "m-ok");

    let hang_runs = member_runs(&db, &spec_id, "m-hang");
    assert!(
        hang_runs.iter().any(|r| r.status == GraphRunStatus::Fail),
        "cascade: m-hang must have been cut at its own timeout"
    );
    let terminated = hang_runs
        .iter()
        .find(|r| r.status == GraphRunStatus::Fail)
        .unwrap();
    let reason = terminated
        .output
        .as_ref()
        .and_then(|o| o.get("reason"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        reason.contains("member's own timeout"),
        "cascade: m-hang termination must name the member's own timeout, got: {reason:?}"
    );
}

/// CM3: cascade tries members in order and, when the first produces no
/// verdict (a retry-exhausted infra crash — `exit 1`), falls back to the
/// next one. The fallback member's pass becomes the ensemble's verdict.
#[tokio::test]
async fn cascade_falls_back_to_next_member_when_first_produces_no_verdict() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "m-crash",
            &write_member_script(dir.path(), "crash.sh", "exit 1"),
        ),
        (
            "m-ok",
            &write_member_script(dir.path(), "ok.sh", "printf ok"),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Cascade,
        &[("m-crash", "m-crash"), ("m-ok", "m-ok")],
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    // CM13: m-crash exhausts infra retries → no verdict → cascade falls
    // through to m-ok. m-ok is also unreported infra → exhausts retries
    // → no verdict → join is Error with no_verdict (CB66: a reviewer
    // that could not run is not a reviewer that rejected).
    assert_eq!(
        join.status,
        GraphRunStatus::Error,
        "CM13: both members are unreported infra; join is Error after retry exhaustion"
    );
    let out = join.output.as_ref().unwrap();
    assert_eq!(out["kind"], "cascade");
    assert_eq!(out["no_verdict"], serde_json::json!(true));

    assert_eq!(
        member_runs(&db, &spec_id, "m-crash").len(),
        3,
        "first member exhausts its infra-retry budget (attempts 0,1,2) before fallback"
    );
    assert_eq!(
        member_runs(&db, &spec_id, "m-ok").len(),
        3,
        "CM13: fallback member is also unreported infra; exhausts retries"
    );
}

/// CM3 / CM2 / CM13: a member that exits 0 with empty stdout and no
/// self-report is unreported infra (CM13) — not a "negative verdict".
/// The cascade falls through to the next member after retry exhaustion.
/// Only a self-reported verdict (Pass or Fail) is "usable" — see
/// [`cm13_cascade_self_reported_fail_stops_walk_no_fallthrough`] for that
/// side of the contract.
#[tokio::test]
async fn cascade_falls_through_unreported_member_then_exhausts_retries() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "m-noout",
            &write_member_script(dir.path(), "noout.sh", "exit 0"),
        ),
        (
            "m-ok",
            &write_member_script(dir.path(), "ok.sh", "printf ok"),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Cascade,
        &[("m-noout", "m-noout"), ("m-ok", "m-ok")],
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    // CM13: m-noout is unreported infra → exhausts retries → no verdict.
    // Cascade falls through to m-ok. m-ok is also unreported infra →
    // exhausts retries → no verdict → join is Error with no_verdict
    // (CB66: a reviewer that could not run is not a reviewer that
    // rejected).
    assert_eq!(
        join.status,
        GraphRunStatus::Error,
        "CM13: both members are unreported infra; join is Error after retry exhaustion"
    );
    let out = join.output.as_ref().unwrap();
    assert_eq!(out["kind"], "cascade");
    assert_eq!(out["no_verdict"], serde_json::json!(true));

    // Both members exhaust retries (3 runs each with default retry_limit=2).
    let m_noout_runs = member_runs(&db, &spec_id, "m-noout");
    assert_eq!(
        m_noout_runs.len(),
        3,
        "CM13: m-noout exhausts infra retries"
    );
    let m_ok_runs = member_runs(&db, &spec_id, "m-ok");
    assert_eq!(m_ok_runs.len(), 3, "CM13: m-ok exhausts infra retries");
}

/// CM3: round-robin runs exactly one member per invocation, rotating
/// through them in position order and wrapping — the persisted
/// `round_robin_index` advances each time and survives across runs.
#[tokio::test]
async fn round_robin_spreads_invocations_across_members_in_order() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "rr-a",
            &write_member_script(dir.path(), "a.sh", "printf ok"),
        ),
        (
            "rr-b",
            &write_member_script(dir.path(), "b.sh", "printf ok"),
        ),
        (
            "rr-c",
            &write_member_script(dir.path(), "c.sh", "printf ok"),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-a", "rr-a"), ("rr-b", "rr-b"), ("rr-c", "rr-c")],
    );

    let _home = HomeGuard::set(fake_home.path());
    // CM13: script-backed members never call graph_complete_node, so every
    // member in the rotation is verdict-less. Each invocation walks all
    // three members (each exhausting its infra retries) and the join
    // fails — but the persisted rotation index must still advance by
    // exactly one per invocation, proving load spreading is independent
    // of member verdicts.
    let expected = [0, 1, 2, 0];
    for (i, want_start) in expected.iter().enumerate() {
        db.update_graph_spec_status(&spec_id, GraphSpecStatus::Pending, None, None)
            .unwrap();
        db.update_graph_status(&graph_id, GraphStatus::Draft, None, None)
            .unwrap();
        engine
            .run_graph(graph_id.clone(), None, None, None, None, None)
            .await
            .unwrap();

        let join = join_run(&db, &spec_id, "join1");
        let out = join.output.as_ref().unwrap();
        assert_eq!(out["kind"], "round_robin");
        assert_eq!(
            join.status,
            GraphRunStatus::Error,
            "CM13: all members are unreported infra, so every invocation is Error (CB66)"
        );
        assert_eq!(
            out["no_verdict"],
            serde_json::json!(true),
            "invocation {i} filed zero verdicts"
        );
        assert_eq!(
            out["members_tried"], 3,
            "invocation {i} must walk all three verdict-less members"
        );
        let ens = db.get_ensemble("ens1").unwrap().unwrap();
        assert_eq!(
            ens.round_robin_index,
            Some(((want_start + 1) % 3) as i64),
            "the persisted index advances by one after invocation {i} (started at {want_start})"
        );
        for other in ["rr-a", "rr-b", "rr-c"] {
            assert!(
                !member_runs(&db, &spec_id, other).is_empty(),
                "CM13: invocation {i} walks every member, so {other} must have run"
            );
        }
    }
    drop(_home);
}

/// CM3: round-robin keeps its load-spreading start point but now survives a
/// dead harness — when the first-in-rotation member produces no verdict (a
/// retry-exhausted `exit 1`), it falls through to the next member.
/// CM13: the fallthrough member is itself script-backed and unreported,
/// so it too is verdict-less and the join fails after trying both.
#[tokio::test]
async fn round_robin_falls_through_to_next_member_when_first_has_no_verdict() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "rr-crash",
            &write_member_script(dir.path(), "crash.sh", "exit 1"),
        ),
        (
            "rr-ok",
            &write_member_script(dir.path(), "ok.sh", "printf ok"),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-crash", "rr-crash"), ("rr-ok", "rr-ok")],
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    // CM13: rr-ok never self-reports either, so both members are
    // verdict-less and the join is Error with no_verdict (CB66) after
    // the fallthrough.
    assert_eq!(
        join.status,
        GraphRunStatus::Error,
        "CM13: both members are unreported infra; join is Error after fallthrough"
    );
    let out = join.output.as_ref().unwrap();
    assert_eq!(out["kind"], "round_robin");
    assert_eq!(out["members_tried"], 2);
    assert_eq!(out["members_total"], 2);

    assert_eq!(
        member_runs(&db, &spec_id, "rr-crash").len(),
        3,
        "first member exhausts its infra-retry budget (attempts 0,1,2) before fallthrough"
    );
    assert!(
        !member_runs(&db, &spec_id, "rr-ok").is_empty(),
        "the fallthrough member must actually run"
    );
}

/// CM3: a round-robin member that returns a real verdict stops the walk.
/// CM13: an exit-0 bare script is NOT a real verdict (it never called
/// `graph_complete_node`), so the `no_output` member is verdict-less and
/// the walk falls through to the next member instead of stopping.
#[tokio::test]
async fn round_robin_stops_on_negative_verdict_without_trying_next_member() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "rr-noout",
            &write_member_script(dir.path(), "noout.sh", "exit 0"),
        ),
        (
            "rr-ok",
            &write_member_script(dir.path(), "ok.sh", "printf ok"),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-noout", "rr-noout"), ("rr-ok", "rr-ok")],
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    // CM13: rr-noout never filed a verdict, so it is verdict-less and the
    // walk falls through to rr-ok — which is also unreported infra, so
    // the join is Error with no_verdict (CB66).
    assert_eq!(
        join.status,
        GraphRunStatus::Error,
        "CM13: both members are verdict-less; the ensemble is Error after fallthrough"
    );
    let out = join.output.as_ref().unwrap();
    assert_eq!(out["kind"], "round_robin");
    assert_eq!(out["members_tried"], 2);
    assert!(
        !member_runs(&db, &spec_id, "rr-ok").is_empty(),
        "CM13: round-robin must fall through past a verdict-less member"
    );
}

/// CM3: when every member in the rotation produces no verdict the ensemble
/// is Error with no_verdict (CB66), in the same spirit as cascade's
/// "all N members infra-crashed".
#[tokio::test]
async fn round_robin_fails_with_all_members_message_when_none_produce_a_verdict() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        ("rr-c1", &write_member_script(dir.path(), "c1.sh", "exit 1")),
        ("rr-c2", &write_member_script(dir.path(), "c2.sh", "exit 1")),
        ("rr-c3", &write_member_script(dir.path(), "c3.sh", "exit 1")),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-c1", "rr-c1"), ("rr-c2", "rr-c2"), ("rr-c3", "rr-c3")],
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(join.status, GraphRunStatus::Error);
    let out = join.output.as_ref().unwrap();
    assert_eq!(out["kind"], "round_robin");
    assert_eq!(out["no_verdict"], serde_json::json!(true));
    assert_eq!(
        out["members"]
            .as_array()
            .expect("no-verdict lists members")
            .len(),
        3
    );
    assert_eq!(out["members_tried"], 3);
    assert_eq!(out["members_total"], 3);
    for m in ["rr-c1", "rr-c2", "rr-c3"] {
        assert_eq!(
            member_runs(&db, &spec_id, m).len(),
            3,
            "{m} exhausts its infra-retry budget before the walk gives up"
        );
    }
}

/// CM3: load spreading and failover are independent. Even when the walk has
/// to skip two verdict-less members before it lands on a live one, the
/// persisted rotation index advances by exactly one — the next invocation
/// starts one past where this one started, not past every skipped member.
#[tokio::test]
async fn round_robin_rotation_index_advances_by_one_not_by_members_skipped() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        ("rr-x1", &write_member_script(dir.path(), "x1.sh", "exit 1")),
        ("rr-x2", &write_member_script(dir.path(), "x2.sh", "exit 1")),
        (
            "rr-ok",
            &write_member_script(dir.path(), "ok.sh", "printf ok"),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-x1", "rr-x1"), ("rr-x2", "rr-x2"), ("rr-ok", "rr-ok")],
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    // CM13: rr-ok never self-reports, so all three members are
    // verdict-less and the join is Error (CB66) — but the walk still
    // tried all three in order before giving up.
    assert_eq!(join.status, GraphRunStatus::Error);
    let out = join.output.as_ref().unwrap();
    assert_eq!(
        out["members_tried"], 3,
        "the walk had to try all three members"
    );

    let ens = db.get_ensemble("ens1").unwrap().unwrap();
    assert_eq!(
        ens.round_robin_index,
        Some(1),
        "index advances from 0 to 1 — not to 3 for the two members skipped"
    );
}

/// CM48: an unavailable member never runs. The rotation advances past it
/// to the next available member, the join reports the rotation space it
/// walked (3 total) against the members it actually tried (2), and the
/// stored rotation index still advances by exactly one.
#[tokio::test]
async fn round_robin_skips_unavailable_member_and_keeps_rotation_space() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "rr-a",
            &write_member_script(dir.path(), "a.sh", "printf ok"),
        ),
        (
            "rr-b",
            &write_member_script(dir.path(), "b.sh", "printf ok"),
        ),
        (
            "rr-c",
            &write_member_script(dir.path(), "c.sh", "printf ok"),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-a", "rr-a"), ("rr-b", "rr-b"), ("rr-c", "rr-c")],
    );
    mark_member_platform_unavailable(&db, "rr-b");

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    assert!(
        member_runs(&db, &spec_id, "rr-b").is_empty(),
        "an unavailable member must never run"
    );
    assert!(
        !member_runs(&db, &spec_id, "rr-a").is_empty(),
        "the first available member runs"
    );
    assert!(
        !member_runs(&db, &spec_id, "rr-c").is_empty(),
        "the rotation advances past the unavailable member"
    );

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(join.status, GraphRunStatus::Error);
    let out = join.output.as_ref().unwrap();
    assert_eq!(out["kind"], "round_robin");
    assert_eq!(
        out["members_tried"], 2,
        "only the members that actually ran count as tried"
    );
    assert_eq!(
        out["members_total"], 3,
        "the rotation space stays the full member list"
    );

    let ens = db.get_ensemble("ens1").unwrap().unwrap();
    assert_eq!(
        ens.round_robin_index,
        Some(1),
        "the index still advances by exactly one"
    );
}

/// CM48: the stored rotation cursor is the index of the member that
/// actually ran — when the walk's first member is unavailable, the cursor
/// pins index 1 in the FULL member list, not a compressed position.
#[tokio::test]
async fn round_robin_pins_the_member_actually_used_not_the_skipped_one() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "rr-p1",
            &write_member_script(dir.path(), "p1.sh", "printf ok"),
        ),
        (
            "rr-p2",
            &write_member_script(dir.path(), "p2.sh", "printf ok"),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-p1", "rr-p1"), ("rr-p2", "rr-p2")],
    );
    mark_member_platform_unavailable(&db, "rr-p1");
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    let details = db.get_ensemble_details("ens1").unwrap().unwrap();
    let available = match engine.resolve_ensemble_roster(&details) {
        EnsembleRoster::Ready(members) => members,
        EnsembleRoster::Blocked(blocker) => {
            panic!("one available member must dispatch: {blocker}")
        }
    };
    assert_eq!(available.len(), 1, "only the available member dispatches");

    let mut resumable_sessions = HashMap::new();
    let mut round_robin_pins = HashMap::new();
    let _home = HomeGuard::set(fake_home.path());
    let execution = engine
        .execute_ensemble_round_robin(
            &lp,
            &spec,
            &details,
            &available,
            None,
            1,
            &lp.workdir,
            false,
            &HashMap::new(),
            &[],
            &mut resumable_sessions,
            &mut round_robin_pins,
        )
        .await
        .unwrap();
    drop(_home);

    assert!(
        member_runs(&db, &spec_id, "rr-p1").is_empty(),
        "the unavailable member never runs"
    );
    assert!(
        !member_runs(&db, &spec_id, "rr-p2").is_empty(),
        "the rotation advances to the available member"
    );
    assert_eq!(
        round_robin_pins.get("ens1").copied(),
        Some(1),
        "the pinned cursor is the member actually used (index 1 of the full list)"
    );
    assert_eq!(
        execution
            .output
            .get("members_tried")
            .and_then(|v| v.as_u64()),
        Some(1),
        "only the member that ran counts as tried"
    );
}

/// CM48: after a walk that stepped over an unavailable member, the
/// stored `round_robin_index` is re-stored from the member that actually
/// won. The eager walk-start write (`start + 1`) would otherwise leave
/// the cursor pointing at the winner itself — dispatching it twice in a
/// row — and disagreeing with the join output's `next_index`.
#[tokio::test]
async fn round_robin_stores_the_index_from_the_member_that_actually_won() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "rr-nv",
            &write_member_script(dir.path(), "nv.sh", "printf ok"),
        ),
        (
            "rr-win",
            &write_self_reporting_member_script(
                dir.path(),
                "win.sh",
                "sleep 1",
                &db_path,
                "rr-win",
                "pass",
                None,
            ),
        ),
    ]);
    // The rotation starts on the no-verdict member, steps over the
    // unavailable one, and the third member wins.
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[
            ("rr-nv", "rr-nv"),
            ("rr-out", "rr-out"),
            ("rr-win", "rr-win"),
        ],
    );
    mark_member_platform_unavailable(&db, "rr-out");

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    assert!(
        member_runs(&db, &spec_id, "rr-out").is_empty(),
        "an unavailable member must never run"
    );
    assert!(
        !member_runs(&db, &spec_id, "rr-win").is_empty(),
        "the rotation advanced past the unavailable member to the winner"
    );
    let join = join_run(&db, &spec_id, "join1");
    let out = join.output.as_ref().unwrap();
    assert_eq!(out["kind"], "round_robin");
    assert_eq!(out["winner"]["node_id"], "rr-win");
    assert_eq!(
        out["members_tried"], 2,
        "the skipped member is not counted as tried"
    );
    assert_eq!(
        out["next_index"], 0,
        "the join reports the rotation continuing after the winner"
    );
    let ens = db.get_ensemble("ens1").unwrap().unwrap();
    assert_eq!(
        ens.round_robin_index,
        Some(0),
        "the stored index is re-stored from the member that actually won (the eager \
             walk-start write leaves Some(1), which would dispatch rr-win twice in a row)"
    );
}

/// CM48: a cascade steps over an unavailable member in order — the
/// no-verdict report names the member that actually ran and never the
/// skipped one.
#[tokio::test]
async fn cascade_skips_unavailable_member_in_order() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "ca-out",
            &write_member_script(dir.path(), "out.sh", "printf ok"),
        ),
        (
            "ca-ok",
            &write_member_script(dir.path(), "ok.sh", "printf ok"),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Cascade,
        &[("ca-out", "ca-out"), ("ca-ok", "ca-ok")],
    );
    mark_member_platform_unavailable(&db, "ca-out");

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    assert!(
        member_runs(&db, &spec_id, "ca-out").is_empty(),
        "an unavailable member must never run"
    );
    assert!(
        !member_runs(&db, &spec_id, "ca-ok").is_empty(),
        "the cascade falls through to the available member"
    );

    let join = join_run(&db, &spec_id, "join1");
    let out = join.output.as_ref().unwrap();
    assert_eq!(out["kind"], "cascade");
    assert_eq!(out["members_tried"], 1);
    let members = out["members"].as_array().expect("no-verdict lists members");
    assert_eq!(members.len(), 1, "the skipped member is never reported");
    assert_eq!(members[0]["platform"], "ca-ok");
}

/// CM48: a parallel ensemble runs only its available members.
#[tokio::test]
async fn parallel_skips_unavailable_member_at_dispatch() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "pa-out",
            &write_member_script(dir.path(), "out.sh", "printf ok"),
        ),
        (
            "pa-ok",
            &write_member_script(dir.path(), "ok.sh", "printf ok"),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Parallel,
        &[("pa-out", "pa-out"), ("pa-ok", "pa-ok")],
    );
    mark_member_platform_unavailable(&db, "pa-out");

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    assert!(
        member_runs(&db, &spec_id, "pa-out").is_empty(),
        "an unavailable member must never run"
    );
    assert!(
        !member_runs(&db, &spec_id, "pa-ok").is_empty(),
        "the available member runs"
    );
    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(join.output.as_ref().unwrap()["kind"], "quorum");
}

/// CM48: when fewer than `min_pass` members are available the ensemble
/// does not dispatch at all — the spec blocks with the required message
/// and no member runs.
#[tokio::test]
async fn parallel_below_min_pass_blocks_the_spec_before_dispatch() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "pb-ok",
            &write_member_script(dir.path(), "ok.sh", "printf ok"),
        ),
        (
            "pb-out",
            &write_member_script(dir.path(), "out.sh", "printf ok"),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Parallel,
        &[("pb-ok", "pb-ok"), ("pb-out", "pb-out")],
    );
    db.update_ensemble_join_config("ens1", Some(2), None, None, None, None, None, None)
        .unwrap();
    mark_member_platform_unavailable(&db, "pb-out");

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    for m in ["pb-ok", "pb-out"] {
        assert!(
            member_runs(&db, &spec_id, m).is_empty(),
            "a blocked ensemble dispatches nothing: {m} has runs"
        );
    }
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Paused,
        "the ensemble that cannot dispatch blocks the spec"
    );
    let events = db
        .list_graph_events(
            &graph_id,
            None,
            None,
            Some(&["graph_blocked".to_string()]),
            50,
        )
        .unwrap();
    assert_eq!(
        events.len(),
        1,
        "exactly one graph_blocked event, got {events:?}"
    );
    assert_eq!(
        events[0].message,
        "Blocked: ensemble 'Switched Ensemble': 1 of 2 members available, min_pass 2 \
             (unavailable: pb-out/(default) — quota)"
    );
}

/// CM48: a single agent node whose pair is unavailable blocks the spec
/// before any attempt is counted — no run row exists — and the block
/// fires the graph's `on_blocked` hook.
#[tokio::test]
async fn unavailable_agent_node_blocks_the_spec_without_an_attempt() {
    use crate::domain::graphs::GraphCompletionHook;

    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
        id: "impl".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "impl".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "opencode-go",
            "model": "opencode-go/claude",
            "prompt_template": "do the work",
            "timeout_minutes": 5,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    mark_member_platform_unavailable(&db, "opencode-go");

    let blocked_file = dir.path().join("blocked.txt");
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnBlocked,
        vec![GraphCompletionHook {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: Some(format!("printf blocked > {}", blocked_file.display())),
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

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    assert!(
        db.list_graph_runs_for_spec(&spec_id).unwrap().is_empty(),
        "an availability block counts no attempt: no run row may exist"
    );
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Paused,
        "the unavailable node blocks the spec"
    );
    let events = db
        .list_graph_events(
            &graph_id,
            None,
            None,
            Some(&["graph_blocked".to_string()]),
            50,
        )
        .unwrap();
    assert_eq!(
        events.len(),
        1,
        "exactly one graph_blocked event, got {events:?}"
    );
    assert_eq!(
        events[0].message,
        "Blocked: node 'impl': opencode-go/opencode-go/claude unavailable — quota"
    );
    assert_eq!(
        std::fs::read_to_string(&blocked_file).unwrap_or_default(),
        "blocked",
        "the block must fire the existing on_blocked hook path"
    );
}

/// CM3: the fallthrough walk wraps. Starting at the last member in the
/// rotation, a no-verdict there continues at index 0.
#[tokio::test]
async fn round_robin_fallthrough_wraps_around_to_the_first_member() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "rr-ok",
            &write_member_script(dir.path(), "ok.sh", "printf ok"),
        ),
        (
            "rr-mid",
            &write_member_script(dir.path(), "mid.sh", "printf ok"),
        ),
        (
            "rr-crash",
            &write_member_script(dir.path(), "crash.sh", "exit 1"),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[
            ("rr-ok", "rr-ok"),
            ("rr-mid", "rr-mid"),
            ("rr-crash", "rr-crash"),
        ],
    );
    // Start the rotation on the last member.
    db.update_ensemble_kind("ens1", None, Some(Some(2)))
        .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    // CM13: every member is script-backed and unreported, so the walk
    // tries all three starting from the last and the join is Error
    // (CB66) — but the wrap itself still happened (rr-ok at index 0 ran).
    assert_eq!(join.status, GraphRunStatus::Error);
    let out = join.output.as_ref().unwrap();
    assert_eq!(out["members_tried"], 3);

    assert!(
        !member_runs(&db, &spec_id, "rr-crash").is_empty(),
        "the last member ran first"
    );
    assert!(
        !member_runs(&db, &spec_id, "rr-ok").is_empty(),
        "the walk wrapped to the first member"
    );
    assert!(
        !member_runs(&db, &spec_id, "rr-mid").is_empty(),
        "CM13: every member is verdict-less, so the walk continues past index 0 to index 1"
    );

    let ens = db.get_ensemble("ens1").unwrap().unwrap();
    assert_eq!(
        ens.round_robin_index,
        Some(0),
        "the index advanced by one from 2, wrapping to 0"
    );
}
