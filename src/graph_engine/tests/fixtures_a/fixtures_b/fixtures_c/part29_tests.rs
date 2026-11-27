//! Part 29 of graph-engine `tests`: the `random` ensemble kind draws a
//! uniformly shuffled order over the roster-available members, falls through
//! on both an infrastructure crash and a real failure verdict, reports every
//! tried cause when all fail, and picks up crew edits.
use super::*;
use rand::rngs::StdRng;
use rand::SeedableRng as _;

/// The first seed whose shuffled draw of `len` members visits `target` first.
fn seed_drawing_first(target: usize, len: usize) -> u64 {
    for seed in 0..10_000u64 {
        let mut rng = StdRng::seed_from_u64(seed);
        if draw_order(len, &mut rng)[0] == target {
            return seed;
        }
    }
    panic!("no seed draws index {target} first out of {len}");
}

fn mark_platform_unavailable(db: &Database, platform: &str) {
    db.roster_upsert(crate::db::roster_entries::NewRosterEntry {
        platform,
        model_pattern: None,
        state: crate::db::roster_entries::RosterState::Unavailable,
        reason: Some("quota"),
        until: None,
    })
    .unwrap();
}

/// Requirement 2: an infrastructure failure falls through to the next
/// candidate. The seed fixes the crash member first, so a walk that stopped
/// there would fail the join; the fall-through runs the passing member and
/// reports both as tried.
#[tokio::test]
async fn random_ensemble_falls_through_an_infra_crash() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "r-crash",
            &write_member_script(dir.path(), "r-crash.sh", "exit 1"),
        ),
        (
            "r-pass",
            &write_self_reporting_member_script(
                dir.path(),
                "r-pass.sh",
                "sleep 1",
                &db_path,
                "m-pass",
                "pass",
                None,
            ),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Random,
        &[("m-crash", "r-crash"), ("m-pass", "r-pass")],
    );
    let engine = engine.with_random_seed(seed_drawing_first(0, 2));

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
        "the random walk must fall through the crashed first draw"
    );
    let output = join.output.as_ref().unwrap();
    assert_eq!(output["kind"], "random");
    assert_eq!(output["winner"]["node_id"], "m-pass");
    assert_eq!(output["members_tried"], 2);
    assert!(
        member_runs(&db, &spec_id, "m-crash")
            .iter()
            .any(|run| run.status == GraphRunStatus::Fail),
        "the crashed member must have actually run and failed"
    );
}

/// Requirement 2: unlike cascade, a real failure verdict also falls through.
/// The seed puts the failing member first; if random stopped on it the join
/// would be `Fail`.
#[tokio::test]
async fn random_ensemble_falls_through_a_real_failure_verdict() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "r-fail",
            &write_self_reporting_member_script(
                dir.path(),
                "r-fail.sh",
                "sleep 1",
                &db_path,
                "m-fail",
                "fail",
                None,
            ),
        ),
        (
            "r-pass",
            &write_self_reporting_member_script(
                dir.path(),
                "r-pass.sh",
                "sleep 1",
                &db_path,
                "m-pass",
                "pass",
                None,
            ),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Random,
        &[("m-fail", "r-fail"), ("m-pass", "r-pass")],
    );
    let engine = engine.with_random_seed(seed_drawing_first(0, 2));

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
        "a real failure verdict must fall through to the untried passing member"
    );
    let output = join.output.as_ref().unwrap();
    assert_eq!(output["winner"]["node_id"], "m-pass");
    assert_eq!(output["members_tried"], 2);
    assert!(
        member_runs(&db, &spec_id, "m-fail")
            .iter()
            .any(|run| run.status == GraphRunStatus::Fail),
        "the failing member must have filed its own fail verdict"
    );
}

/// Requirement 2: when every candidate fails, the join reports a cause for
/// each tried member (including real failures), not just the last one.
#[tokio::test]
async fn random_ensemble_all_fail_reports_every_cause() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "r-fail-a",
            &write_self_reporting_member_script(
                dir.path(),
                "r-fail-a.sh",
                "sleep 1",
                &db_path,
                "m-a",
                "fail",
                None,
            ),
        ),
        (
            "r-crash",
            &write_member_script(dir.path(), "r-crash.sh", "exit 1"),
        ),
        (
            "r-fail-b",
            &write_self_reporting_member_script(
                dir.path(),
                "r-fail-b.sh",
                "sleep 1",
                &db_path,
                "m-b",
                "fail",
                None,
            ),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Random,
        &[
            ("m-a", "r-fail-a"),
            ("m-crash", "r-crash"),
            ("m-b", "r-fail-b"),
        ],
    );
    let engine = engine.with_random_seed(7);

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(join.status, GraphRunStatus::Fail);
    let output = join.output.as_ref().unwrap();
    assert_eq!(output["kind"], "random");
    assert_eq!(output["members_tried"], 3);
    assert_eq!(
        output["error"], "no member produced a passing verdict",
        "a real failure must not be reported as a no-verdict crash"
    );
    let members = output["members"].as_array().unwrap();
    assert_eq!(
        members.len(),
        3,
        "every tried member's cause must be reported: {members:?}"
    );
    for member in members {
        assert!(
            member["reason"].as_str().is_some_and(|r| !r.is_empty()),
            "each tried member needs a reason: {member}"
        );
    }
}

/// Requirement 2/FR: a roster-excluded member is never drawn, and never runs.
#[tokio::test]
async fn random_ensemble_never_draws_a_roster_excluded_member() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "r-excluded",
            &write_self_reporting_member_script(
                dir.path(),
                "r-excluded.sh",
                "sleep 1",
                &db_path,
                "m-excluded",
                "pass",
                None,
            ),
        ),
        (
            "r-avail",
            &write_self_reporting_member_script(
                dir.path(),
                "r-avail.sh",
                "sleep 1",
                &db_path,
                "m-avail",
                "pass",
                None,
            ),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Random,
        &[("m-excluded", "r-excluded"), ("m-avail", "r-avail")],
    );
    mark_platform_unavailable(&db, "r-excluded");
    let engine = engine.with_random_seed(0);

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    let output = join.output.as_ref().unwrap();
    assert_eq!(output["winner"]["node_id"], "m-avail");
    assert!(
        member_runs(&db, &spec_id, "m-excluded").is_empty(),
        "a roster-excluded member must never be drawn or run"
    );
}

/// Requirement 3: a crew-bound random ensemble follows the crew's members,
/// including an edit made while the graph is running.
#[tokio::test]
async fn random_crew_update_reaches_the_ensemble() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "seed-a",
            &write_member_script(dir.path(), "seed-a.sh", "printf ok"),
        ),
        (
            "seed-b",
            &write_member_script(dir.path(), "seed-b.sh", "printf ok"),
        ),
        (
            "new-a",
            &write_self_reporting_member_script(
                dir.path(),
                "new-a.sh",
                "printf ok",
                &db_path,
                "m1",
                "pass",
                None,
            ),
        ),
        (
            "new-b",
            &write_self_reporting_member_script(
                dir.path(),
                "new-b.sh",
                "printf ok",
                &db_path,
                "m2",
                "pass",
                None,
            ),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Random,
        &[("m1", "seed-a"), ("m2", "seed-b")],
    );
    db.crew_create(
        "impl",
        None,
        &[test_crew_member("seed-a"), test_crew_member("seed-b")],
    )
    .unwrap();
    db.bind_ensemble_to_crew("ens1", "impl").unwrap();
    db.crew_update_resolved(
        "impl",
        Some(&[
            test_unresolved_crew_member("new-a"),
            test_unresolved_crew_member("new-b"),
        ]),
        None,
        None,
    )
    .unwrap();
    let engine = engine.with_random_seed(0);

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let after = db.get_ensemble_details("ens1").unwrap().unwrap();
    assert_eq!(
        after
            .members
            .iter()
            .map(|member| member.platform.as_str())
            .collect::<Vec<_>>(),
        vec!["new-a", "new-b"],
        "the lazy dispatch sync must rewrite a random ensemble's members"
    );
    assert_eq!(after.ensemble.crew_version_applied, Some(2));
    assert_eq!(
        join_run(&db, &spec_id, "join1").status,
        GraphRunStatus::Pass
    );
}
