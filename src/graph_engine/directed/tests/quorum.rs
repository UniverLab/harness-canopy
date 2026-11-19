use super::*;

#[test]
fn directed_start_accepts_failed_quorum_routed_by_join_fail_edge() {
    let s = spec("spec-1");
    let (nodes, mut edges, ensembles) = round_robin_between_a_and_b();
    for edge in &mut edges {
        if edge.from_node == "j" {
            edge.condition = GraphEdgeCondition::Fail;
        }
    }
    let runs = vec![
        run("m1", GraphRunStatus::Fail, Some(serde_json::json!(1)), None),
        run("m3", GraphRunStatus::Pass, Some(serde_json::json!(3)), None),
        run(
            "j",
            GraphRunStatus::Fail,
            Some(serde_json::json!(false)),
            None,
        ),
    ];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    resolve_spec_start(
        &nodes,
        &edges,
        &s,
        &runs,
        &ensembles,
        false,
        Some(&directed),
    )
    .expect("a join fail edge routes the failed verdict to the start node");
}

#[test]
fn directed_start_refuses_error_and_interrupted_upstream_prerequisites() {
    for status in [GraphRunStatus::Error, GraphRunStatus::Interrupted] {
        let s = spec("spec-1");
        let (nodes, edges) = abc_graph();
        let mut r = run("A", status, Some(serde_json::json!({"a": "out"})), None);
        r.id = "run-A".to_string();
        let runs = vec![r];
        let directed = DirectedStart {
            spec_id: s.id.clone(),
            node_arg: "B".to_string(),
        };
        let err =
            resolve_spec_start(&nodes, &edges, &s, &runs, &[], false, Some(&directed)).unwrap_err();
        assert!(err.to_string().contains("prerequisite node 'A'"), "{err}");
    }
}

/// FR2: a round-robin ensemble passes through whichever member carried the
/// vote. Member 1 failing under a passed quorum is not an unmet
/// prerequisite for a directed start past the ensemble.
#[test]
fn directed_start_past_a_passed_round_robin_ensemble_ignores_a_failed_member() {
    let s = spec("spec-1");
    let (nodes, edges, ensembles) = round_robin_between_a_and_b();
    let runs = vec![
        run("m1", GraphRunStatus::Fail, Some(serde_json::json!(1)), None),
        run("m3", GraphRunStatus::Pass, Some(serde_json::json!(3)), None),
        run(
            "j",
            GraphRunStatus::Pass,
            Some(serde_json::json!(true)),
            None,
        ),
    ];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    resolve_spec_start(
        &nodes,
        &edges,
        &s,
        &runs,
        &ensembles,
        false,
        Some(&directed),
    )
    .expect("a passed quorum satisfies the ensemble prerequisite");
}

/// FR1: the reported incident shape — a round-robin rotation where member 1
/// failed (pass 1, quorum failed with it) and member 2 then passed (pass 2,
/// quorum passed). The ensemble's latest verdict is a pass, so a directed
/// start downstream is accepted: member 1 is judged by that latest verdict,
/// not by its own failure nor by the failed intermediate quorum. Breaks if
/// members are judged by their own runs, or by a same-iteration join.
#[test]
fn directed_start_past_passed_two_member_ensemble_accepts_failed_member() {
    let s = spec("spec-1");
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node("m1", "M1", 1, Value::Null),
        node("m2", "M2", 2, Value::Null),
        node("j", "Join", 3, Value::Null),
        node("B", "B", 4, Value::Null),
    ];
    let edges = vec![
        edge("A", "m1", GraphEdgeCondition::Always),
        edge("A", "m2", GraphEdgeCondition::Always),
        edge("m1", "j", GraphEdgeCondition::Always),
        edge("m2", "j", GraphEdgeCondition::Always),
        edge("j", "B", GraphEdgeCondition::Pass),
    ];
    let mut details = ensemble_details("ens", "rr", "A", &["m1", "m2"], "j");
    details.ensemble.kind = EnsembleKind::RoundRobin;
    details.ensemble.min_pass = 1;
    let ensembles = vec![details];
    let mut m1 = run("m1", GraphRunStatus::Fail, Some(serde_json::json!(1)), None);
    m1.iteration = 1;
    let mut j1 = run(
        "j",
        GraphRunStatus::Fail,
        Some(serde_json::json!(false)),
        None,
    );
    j1.iteration = 1;
    j1.id = "run-j-1".to_string();
    let mut m2 = run("m2", GraphRunStatus::Pass, Some(serde_json::json!(2)), None);
    m2.iteration = 2;
    let mut j2 = run(
        "j",
        GraphRunStatus::Pass,
        Some(serde_json::json!(true)),
        None,
    );
    j2.iteration = 2;
    j2.id = "run-j-2".to_string();
    let runs = vec![m1, j1, m2, j2];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    resolve_spec_start(
        &nodes,
        &edges,
        &s,
        &runs,
        &ensembles,
        false,
        Some(&directed),
    )
    .expect("a passed ensemble satisfies every node downstream of it");
}

/// FR2: when the ensemble's own quorum result failed, the prerequisite
/// is unmet and the refusal names the ensemble — not a member or the
/// join node.
#[test]
fn directed_start_past_a_failed_round_robin_quorum_is_refused() {
    let s = spec("spec-1");
    let (nodes, edges, ensembles) = round_robin_between_a_and_b();
    let runs = vec![
        run("m1", GraphRunStatus::Fail, Some(serde_json::json!(1)), None),
        run("m3", GraphRunStatus::Pass, Some(serde_json::json!(3)), None),
        run(
            "j",
            GraphRunStatus::Fail,
            Some(serde_json::json!(false)),
            None,
        ),
    ];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    let err = resolve_spec_start(
        &nodes,
        &edges,
        &s,
        &runs,
        &ensembles,
        false,
        Some(&directed),
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("prerequisite ensemble 'rr' last run was fail"),
        "{err}"
    );
    assert!(
        !err.to_string().contains("'M1'"),
        "the ensemble verdict, not a member, is the prerequisite: {err}"
    );
    assert!(
        !err.to_string().contains("'Join'"),
        "the refusal names the ensemble, not its join node: {err}"
    );
}

/// FR2: a quorum only judges the pass it settled. Here the member ran
/// again after the last join — a shutdown between a member and its join —
/// so that attempt has no verdict: it still refuses the start, naming the
/// ensemble and the missing verdict instead of hiding behind an older
/// passed quorum.
#[test]
fn directed_start_refuses_a_failed_member_of_an_unsettled_ensemble_pass() {
    let s = spec("spec-1");
    let (nodes, edges, ensembles) = round_robin_between_a_and_b();
    let mut settled = run(
        "j",
        GraphRunStatus::Pass,
        Some(serde_json::json!(true)),
        None,
    );
    settled.iteration = 1;
    let mut newer_attempt = run("m1", GraphRunStatus::Fail, Some(serde_json::json!(1)), None);
    newer_attempt.iteration = 2;
    let runs = vec![settled, newer_attempt];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    let err = resolve_spec_start(
        &nodes,
        &edges,
        &s,
        &runs,
        &ensembles,
        false,
        Some(&directed),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("prerequisite ensemble 'rr'"),
        "the unsettled attempt refuses against the ensemble: {err}"
    );
    assert!(
        err.to_string().contains("no recorded verdict"),
        "with no covering join the attempt has no verdict: {err}"
    );
}

/// FR2: the unsettled pass refuses too. The member passed after the last
/// join — a shutdown between the member and its join — so the ensemble has
/// no verdict for this attempt: accepting would seed the stale join output
/// the previous pass left behind. Breaks if an unsettled member falls back
/// to its own run instead of refusing against the ensemble.
#[test]
fn directed_start_refuses_a_passed_member_of_an_unsettled_ensemble_pass() {
    let s = spec("spec-1");
    let (nodes, edges, ensembles) = round_robin_between_a_and_b();
    let mut settled = run(
        "j",
        GraphRunStatus::Pass,
        Some(serde_json::json!(true)),
        None,
    );
    settled.iteration = 1;
    let mut newer_attempt = run("m1", GraphRunStatus::Pass, Some(serde_json::json!(1)), None);
    newer_attempt.iteration = 2;
    let runs = vec![settled, newer_attempt];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    let err = resolve_spec_start(
        &nodes,
        &edges,
        &s,
        &runs,
        &ensembles,
        false,
        Some(&directed),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("prerequisite ensemble 'rr'"),
        "the unsettled attempt refuses against the ensemble: {err}"
    );
    assert!(
        err.to_string().contains("no recorded verdict"),
        "with no covering join the attempt has no verdict: {err}"
    );
}
