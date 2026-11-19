//! Part 30 of the graph-engine tests: a live ensemble kind/crew change
//! lands on the next dispatch, never the in-flight one.
use super::*;

/// An ensemble that dispatches twice in one run: kickoff fans out to the
/// round-robin members, the join routes to a fail-once gate, and the gate's
/// fail edge bounces back to kickoff for the second dispatch. `m_a`/`m_b`
/// are the member platforms; the gate sleeps through its first (failing)
/// run so the test can land a kind/crew change mid-run deterministically.
fn insert_bounce_ensemble(db: &Database, spec_id: &str, gate_counter: &std::path::Path) {
    let now = chrono::Utc::now();
    insert_bounce_plain_nodes(db, spec_id, gate_counter, now);
    // Member and join nodes are built as values (never inserted here):
    // `insert_ensemble_unit` below inserts them together with the edges
    // and the ensemble row in one transaction.
    let member_nodes = ["m1", "m2"]
        .iter()
        .enumerate()
        .map(|(i, node_id)| GraphNode {
            id: node_id.to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: format!("member-{}", i + 1),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": if i == 0 { "m-a" } else { "m-b" },
                "prompt_template": "ignored by the member's test script",
                "timeout_minutes": 5,
                "infra_backoff_seconds": 0,
            }),
            position: 2 + i as i64,
            created_at: now,
        })
        .collect::<Vec<_>>();
    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "join".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({ "ensemble_id": "ens1" }),
        position: 50,
        created_at: now,
    };
    let edges = [
        ("kickoff->m1", "kickoff", "m1", GraphEdgeCondition::Always),
        ("kickoff->m2", "kickoff", "m2", GraphEdgeCondition::Always),
        ("m1->join1", "m1", "join1", GraphEdgeCondition::Always),
        ("m2->join1", "m2", "join1", GraphEdgeCondition::Always),
        ("join1->gate", "join1", "gate", GraphEdgeCondition::Pass),
        ("gate->done", "gate", "done", GraphEdgeCondition::Pass),
        ("gate->kickoff", "gate", "kickoff", GraphEdgeCondition::Fail),
    ]
    .into_iter()
    .map(|(id, from, to, condition)| GraphEdge {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition,
    })
    .collect::<Vec<_>>();
    db.insert_ensemble_unit(
        &bounce_ensemble_row(spec_id, now),
        &bounce_ensemble_members(),
        &member_nodes,
        &join_node,
        &edges,
    )
    .unwrap();
}

/// kickoff, the fail-once gate and done: plain nodes outside the unit.
fn insert_bounce_plain_nodes(
    db: &Database,
    spec_id: &str,
    gate_counter: &std::path::Path,
    now: chrono::DateTime<chrono::Utc>,
) {
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "printf ok", "success_condition": "exit_code_0" }),
        position: 1,
        created_at: now,
    })
    .unwrap();
    // Fails once (sleeping so the mid-run change lands first), then passes.
    db.insert_graph_node(&GraphNode {
        id: "gate".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "gate".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": format!(
                "n=$(cat \"{c}\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"{c}\"; \
                 [ \"$n\" -ge 2 ] && exit 0 || {{ sleep 2; exit 1; }}",
                c = gate_counter.display()
            ),
            "success_condition": "exit_code_0"
        }),
        position: 51,
        created_at: now,
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "done".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "done".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "printf ok", "success_condition": "exit_code_0" }),
        position: 100,
        created_at: now,
    })
    .unwrap();
}

fn bounce_ensemble_row(
    spec_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> crate::domain::graphs::Ensemble {
    crate::domain::graphs::Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        read_only: false,
        id: "ens1".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "Bounced".to_string(),
        prompt_template: "ignored by the member's test script".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 1,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 5,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "gate".to_string(),
        on_fail_to: None,
        kind: crate::domain::graphs::EnsembleKind::RoundRobin,
        round_robin_index: Some(0),
        created_at: now,
        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    }
}

fn bounce_ensemble_members() -> Vec<crate::domain::graphs::EnsembleMember> {
    [("m1", "m-a", 0), ("m2", "m-b", 1)]
        .into_iter()
        .map(
            |(node_id, platform, position)| crate::domain::graphs::EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: node_id.to_string(),
                position,
                platform: platform.to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
        )
        .collect()
}

async fn wait_for_join_runs(db: &Database, spec_id: &str, want: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let done = db
            .list_graph_runs_for_spec(spec_id)
            .unwrap()
            .iter()
            .filter(|run| run.node_id == "join1" && run.status != GraphRunStatus::Running)
            .count();
        if done >= want {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "join1 never reached {want} completed runs"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// A kind change plus a crew bind between two dispatches of one running
/// spec: the first dispatch runs round-robin, the second runs the new
/// cascade kind over the crew's members.
/// Fake CLI homes for the bounce test: `m-a`/`m-b` (pre-change members)
/// append `A`/`B` to the run log, `new-a`/`new-b` (crew members) append
/// `N`/`O`; every script self-reports its verdict for its node.
fn bounce_cli_home(
    dir: &tempfile::TempDir,
    db_path: &std::path::Path,
    run_log: &std::path::Path,
) -> tempfile::TempDir {
    let marker = run_log.display().to_string();
    setup_multi_cli_home(&[
        (
            "m-a",
            &write_self_reporting_member_script(
                dir.path(),
                "m-a.sh",
                &format!("echo -n A >> \"{marker}\"; printf ok"),
                db_path,
                "m1",
                "pass",
                None,
            ),
        ),
        (
            "m-b",
            &write_self_reporting_member_script(
                dir.path(),
                "m-b.sh",
                &format!("echo -n B >> \"{marker}\"; printf ok"),
                db_path,
                "m2",
                "pass",
                None,
            ),
        ),
        (
            "new-a",
            &write_self_reporting_member_script(
                dir.path(),
                "new-a.sh",
                &format!("echo -n N >> \"{marker}\"; printf ok"),
                db_path,
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
                &format!("echo -n O >> \"{marker}\"; printf ok"),
                db_path,
                "m2",
                "pass",
                None,
            ),
        ),
    ])
}

#[cfg(unix)]
#[tokio::test]
async fn live_kind_and_crew_change_apply_at_next_dispatch() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let run_log = dir.path().join("run.log");
    let gate_counter = dir.path().join("gate-counter");
    let fake_home = bounce_cli_home(&dir, &db_path, &run_log);
    insert_bounce_ensemble(&db, &spec_id, &gate_counter);
    db.crew_create(
        "stage",
        None,
        &[test_crew_member("new-a"), test_crew_member("new-b")],
    )
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let engine = std::sync::Arc::new(engine);
    let handle = tokio::spawn({
        let engine = std::sync::Arc::clone(&engine);
        let graph_id = graph_id.clone();
        async move {
            engine
                .run_graph(graph_id, None, None, None, None, None)
                .await
        }
    });

    // d1 (round-robin) is done; the gate sleeps through its failing run,
    // so the change below lands before the bounce re-dispatches.
    wait_for_join_runs(&db, &spec_id, 1).await;
    db.update_ensemble_kind(
        "ens1",
        Some(crate::domain::graphs::EnsembleKind::Cascade),
        None,
    )
    .unwrap();
    db.bind_ensemble_to_crew("ens1", "stage").unwrap();

    handle.await.unwrap().unwrap();
    drop(_home);

    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().status,
        GraphSpecStatus::Completed
    );
    // d1 ran the round-robin head (m-a); d2 ran the cascade head over the
    // crew's members (new-a) and stopped — nobody else ran.
    assert_eq!(std::fs::read_to_string(&run_log).unwrap(), "AN");
    // d1 advanced the cursor to 1; d2 (cascade) left it alone.
    assert_eq!(
        db.get_ensemble_details("ens1")
            .unwrap()
            .unwrap()
            .ensemble
            .round_robin_index,
        Some(1),
    );
    assert_eq!(
        db.list_graph_runs_for_spec(&spec_id)
            .unwrap()
            .iter()
            .filter(|run| run.node_id == "gate")
            .count(),
        2,
        "gate must fail once and bounce, then pass",
    );
}

/// A kind change in the middle of a parallel dispatch does not alter that
/// dispatch: every member runs to the quorum it started with.
#[cfg(unix)]
#[tokio::test]
async fn in_flight_parallel_dispatch_keeps_its_kind() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "slow-a",
            &write_self_reporting_member_script(
                dir.path(),
                "slow-a.sh",
                "sleep 1; printf ok",
                &dir.path().join(TEST_DB_FILENAME),
                "m1",
                "pass",
                None,
            ),
        ),
        (
            "slow-b",
            &write_self_reporting_member_script(
                dir.path(),
                "slow-b.sh",
                "sleep 1; printf ok",
                &dir.path().join(TEST_DB_FILENAME),
                "m2",
                "pass",
                None,
            ),
        ),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Parallel,
        &[("m1", "slow-a"), ("m2", "slow-b")],
    );

    let _home = HomeGuard::set(fake_home.path());
    let engine = std::sync::Arc::new(engine);
    let handle = tokio::spawn({
        let engine = std::sync::Arc::clone(&engine);
        let graph_id = graph_id.clone();
        async move {
            engine
                .run_graph(graph_id, None, None, None, None, None)
                .await
        }
    });

    // Both members are in flight; switch to cascade mid-dispatch.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let running = db
            .list_graph_runs_for_spec(&spec_id)
            .unwrap()
            .iter()
            .filter(|run| {
                (run.node_id == "m1" || run.node_id == "m2")
                    && run.status == GraphRunStatus::Running
            })
            .count();
        if running >= 2 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "members never ran concurrently"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    db.update_ensemble_kind(
        "ens1",
        Some(crate::domain::graphs::EnsembleKind::Cascade),
        None,
    )
    .unwrap();

    handle.await.unwrap().unwrap();
    drop(_home);

    // The in-flight dispatch stayed parallel: both members ran to Pass.
    for member in ["m1", "m2"] {
        let runs: Vec<_> = db
            .list_graph_runs_for_spec(&spec_id)
            .unwrap()
            .into_iter()
            .filter(|run| run.node_id == member)
            .collect();
        assert_eq!(runs.len(), 1, "{member} must run exactly once");
        assert_eq!(runs[0].status, GraphRunStatus::Pass);
    }
    assert_eq!(
        join_run(&db, &spec_id, "join1").status,
        GraphRunStatus::Pass
    );
    // The change itself landed for the next dispatch.
    assert_eq!(
        db.get_ensemble_details("ens1")
            .unwrap()
            .unwrap()
            .ensemble
            .kind,
        crate::domain::graphs::EnsembleKind::Cascade,
    );
}
