use super::*;
use crate::domain::graphs::{Ensemble, EnsembleKind};

fn node(id: &str, name: &str, position: i64, config: Value) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: None,
        graph_id: Some("g".to_string()),
        name: name.to_string(),
        kind: GraphNodeKind::Agent,
        config,
        position,
        created_at: chrono::Utc::now(),
    }
}

fn edge(from: &str, to: &str, condition: GraphEdgeCondition) -> GraphEdge {
    GraphEdge {
        id: format!("e-{from}-{to}"),
        spec_id: None,
        graph_id: Some("g".to_string()),
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition,
    }
}

fn run(
    node_id: &str,
    status: GraphRunStatus,
    output: Option<Value>,
    input: Option<Value>,
) -> GraphNodeRun {
    GraphNodeRun {
        id: format!("run-{node_id}"),
        graph_id: "g".to_string(),
        spec_id: "spec-1".to_string(),
        node_id: node_id.to_string(),
        status,
        input,
        output,
        started_at: chrono::Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    }
}

fn spec(id: &str) -> GraphSpec {
    GraphSpec {
        intent: None,
        id: id.to_string(),
        graph_id: Some("g".to_string()),
        name: "S".to_string(),
        description: None,
        position: 0,
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
        review_state: crate::domain::graphs::SpecReviewState::Approved,
        review_tier: None,
        review_findings: None,
        created_by_session: None,
    }
}

fn ensemble_details(
    id: &str,
    name: &str,
    entry: &str,
    members: &[&str],
    join: &str,
) -> EnsembleDetails {
    EnsembleDetails {
        ensemble: Ensemble {
            mutex: Vec::new(),
            id: id.to_string(),
            spec_id: None,
            graph_id: Some("g".to_string()),
            name: name.to_string(),
            prompt_template: "do the thing {{output:A}}".to_string(),
            join_node_id: join.to_string(),
            entry_from_node: entry.to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: members.len() as i64,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 10,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: "done".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            commit_rights: false,
            created_at: chrono::Utc::now(),

            crew_id: None,
            crew_name: None,
            crew_version_applied: None,
        },
        members: members
            .iter()
            .enumerate()
            .map(|(position, node_id)| EnsembleMember {
                ensemble_id: id.to_string(),
                node_id: node_id.to_string(),
                position: position as i64,
                platform: "opencode".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            })
            .collect(),
    }
}

fn abc_graph() -> (Vec<GraphNode>, Vec<GraphEdge>) {
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node("B", "B", 1, Value::Null),
        node("C", "C", 2, Value::Null),
    ];
    let edges = vec![
        edge("A", "B", GraphEdgeCondition::Pass),
        edge("B", "C", GraphEdgeCondition::Pass),
    ];
    (nodes, edges)
}

#[test]
fn directed_start_reuses_earlier_outputs_and_restarts_iterations() {
    let s = spec("spec-1");
    let (nodes, edges) = abc_graph();
    let runs = vec![run(
        "A",
        GraphRunStatus::Pass,
        Some(serde_json::json!({"a": "out"})),
        None,
    )];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    let (cursor, node_outputs, _previous, iterations) =
        resolve_spec_start(&nodes, &edges, &s, &runs, &[], false, Some(&directed)).unwrap();
    assert_eq!(cursor, SpecCursor::Node("B".to_string()));
    assert_eq!(
        node_outputs.get("A"),
        Some(&serde_json::json!({"a": "out"}))
    );
    assert!(iterations.is_empty());
}

#[test]
fn directed_start_excludes_target_from_reused_outputs() {
    let (nodes, edges) = abc_graph();
    let runs = vec![
        run(
            "A",
            GraphRunStatus::Pass,
            Some(serde_json::json!({"a": "out"})),
            None,
        ),
        run(
            "B",
            GraphRunStatus::Pass,
            Some(serde_json::json!({"b": "out"})),
            None,
        ),
    ];
    let reused = reused_output_records(&nodes, &edges, &["B".to_string()], &runs);
    assert_eq!(reused, vec![("A".to_string(), "run-A".to_string())]);
    assert!(!reused.iter().any(|(name, _)| name == "B"));
}

#[test]
fn directed_start_refuses_failed_upstream_prerequisite() {
    let s = spec("spec-1");
    let (nodes, edges) = abc_graph();
    let runs = vec![run(
        "A",
        GraphRunStatus::Fail,
        Some(serde_json::json!({"a": "out"})),
        None,
    )];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    let err =
        resolve_spec_start(&nodes, &edges, &s, &runs, &[], false, Some(&directed)).unwrap_err();
    assert!(err.to_string().contains("prerequisite node 'A'"), "{err}");
    assert!(err.to_string().contains("fail"), "{err}");
}

/// A directed run at an ensemble lists only the nodes upstream of it as
/// skipped — never nodes downstream, even when a bypass edge plus a back
/// edge make a downstream node look both reachable-from-entry and
/// able-to-reach-the-start (the 3f087a76 incident listed Tail and the
/// Implementer as skipped).
#[test]
fn skipped_excludes_nodes_downstream_of_start() {
    let nodes = vec![
        node("s", "Start", 0, Value::Null),
        node("l", "Lock", 1, Value::Null),
        node("m1", "M1", 2, Value::Null),
        node("m2", "M2", 3, Value::Null),
        node("j", "Join", 4, Value::Null),
        node("t", "Tail", 5, Value::Null),
    ];
    let edges = vec![
        edge("s", "l", GraphEdgeCondition::Pass),
        edge("l", "m1", GraphEdgeCondition::Pass),
        edge("l", "m2", GraphEdgeCondition::Pass),
        edge("m1", "j", GraphEdgeCondition::Pass),
        edge("m2", "j", GraphEdgeCondition::Pass),
        edge("j", "t", GraphEdgeCondition::Pass),
        edge("l", "t", GraphEdgeCondition::Pass),
        edge("t", "m1", GraphEdgeCondition::Pass),
    ];
    let skipped = skipped_node_names(
        &nodes,
        &edges,
        &["m1".to_string(), "m2".to_string(), "j".to_string()],
        "s",
    );
    assert_eq!(skipped, vec!["Start".to_string(), "Lock".to_string()]);
}

#[test]
fn directed_start_accepts_failed_upstream_routed_by_fail_edge() {
    let s = spec("spec-1");
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node("B", "B", 1, Value::Null),
    ];
    let edges = vec![edge("A", "B", GraphEdgeCondition::Fail)];
    let runs = vec![run(
        "A",
        GraphRunStatus::Fail,
        Some(serde_json::json!({"a": "out"})),
        None,
    )];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    resolve_spec_start(&nodes, &edges, &s, &runs, &[], false, Some(&directed))
        .expect("a fail edge routes the failure to the start node");
}

#[test]
fn directed_start_refuses_failed_upstream_with_only_pass_edge() {
    let s = spec("spec-1");
    let (nodes, edges) = abc_graph();
    let runs = vec![run(
        "A",
        GraphRunStatus::Fail,
        Some(serde_json::json!({"a": "out"})),
        None,
    )];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    let err =
        resolve_spec_start(&nodes, &edges, &s, &runs, &[], false, Some(&directed)).unwrap_err();
    assert!(
        err.to_string()
            .contains("no fail edge leads from it toward the start node 'B'"),
        "{err}"
    );
}

#[test]
fn directed_start_accepts_failed_upstream_routed_by_always_edge() {
    let s = spec("spec-1");
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node("B", "B", 1, Value::Null),
    ];
    let edges = vec![edge("A", "B", GraphEdgeCondition::Always)];
    let runs = vec![run(
        "A",
        GraphRunStatus::Fail,
        Some(serde_json::json!({"a": "out"})),
        None,
    )];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    resolve_spec_start(&nodes, &edges, &s, &runs, &[], false, Some(&directed))
        .expect("an always edge routes the failure to the start node");
}

#[test]
fn directed_start_accepts_errored_upstream_routed_by_error_edge() {
    let s = spec("spec-1");
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node("B", "B", 1, Value::Null),
    ];
    let edges = vec![edge("A", "B", GraphEdgeCondition::Error)];
    let runs = vec![run(
        "A",
        GraphRunStatus::Error,
        Some(serde_json::json!({"a": "out"})),
        None,
    )];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    resolve_spec_start(&nodes, &edges, &s, &runs, &[], false, Some(&directed))
        .expect("an error edge routes the error to the start node");
}

#[test]
fn directed_start_refuses_errored_upstream_with_only_always_edge() {
    let s = spec("spec-1");
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node("B", "B", 1, Value::Null),
    ];
    let edges = vec![edge("A", "B", GraphEdgeCondition::Always)];
    let runs = vec![run(
        "A",
        GraphRunStatus::Error,
        Some(serde_json::json!({"a": "out"})),
        None,
    )];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    let err =
        resolve_spec_start(&nodes, &edges, &s, &runs, &[], false, Some(&directed)).unwrap_err();
    assert!(
        err.to_string()
            .contains("no error edge leads from it toward the start node 'B'"),
        "{err}"
    );
}

#[test]
fn directed_start_refuses_interrupted_upstream_in_every_shape() {
    for condition in [GraphEdgeCondition::Fail, GraphEdgeCondition::Error] {
        let s = spec("spec-1");
        let nodes = vec![
            node("A", "A", 0, Value::Null),
            node("B", "B", 1, Value::Null),
        ];
        let edges = vec![edge("A", "B", condition)];
        let runs = vec![run(
            "A",
            GraphRunStatus::Interrupted,
            Some(serde_json::json!({"a": "out"})),
            None,
        )];
        let directed = DirectedStart {
            spec_id: s.id.clone(),
            node_arg: "B".to_string(),
        };
        let err =
            resolve_spec_start(&nodes, &edges, &s, &runs, &[], false, Some(&directed)).unwrap_err();
        assert!(
            err.to_string().contains("last run was interrupted"),
            "{err}"
        );
        assert!(!err.to_string().contains("no interrupted edge"), "{err}");
    }
}

/// FR3: the routing edge's destination must be *another* node of the
/// upstream set or a start node. A fail self-loop retries the node
/// itself — it does not route the failure toward the start.
#[test]
fn directed_start_refuses_failed_upstream_whose_only_route_is_a_self_edge() {
    let s = spec("spec-1");
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node("B", "B", 1, Value::Null),
    ];
    let edges = vec![
        edge("A", "A", GraphEdgeCondition::Fail),
        edge("A", "B", GraphEdgeCondition::Pass),
    ];
    let runs = vec![run(
        "A",
        GraphRunStatus::Fail,
        Some(serde_json::json!({"a": "out"})),
        None,
    )];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "B".to_string(),
    };
    let err =
        resolve_spec_start(&nodes, &edges, &s, &runs, &[], false, Some(&directed)).unwrap_err();
    assert!(
        err.to_string()
            .contains("no fail edge leads from it toward the start node 'B'"),
        "{err}"
    );
}

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

/// The node/edge shape of a round-robin ensemble sitting between A and B:
/// A → m1/m2/m3 → j → B.
fn round_robin_between_a_and_b() -> (Vec<GraphNode>, Vec<GraphEdge>, Vec<EnsembleDetails>) {
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node("m1", "M1", 1, Value::Null),
        node("m2", "M2", 2, Value::Null),
        node("m3", "M3", 3, Value::Null),
        node("j", "Join", 4, Value::Null),
        node("B", "B", 5, Value::Null),
    ];
    let edges = vec![
        edge("A", "m1", GraphEdgeCondition::Always),
        edge("A", "m2", GraphEdgeCondition::Always),
        edge("A", "m3", GraphEdgeCondition::Always),
        edge("m1", "j", GraphEdgeCondition::Always),
        edge("m2", "j", GraphEdgeCondition::Always),
        edge("m3", "j", GraphEdgeCondition::Always),
        edge("j", "B", GraphEdgeCondition::Pass),
    ];
    let mut details = ensemble_details("ens", "rr", "A", &["m1", "m2", "m3"], "j");
    details.ensemble.kind = EnsembleKind::RoundRobin;
    details.ensemble.min_pass = 1;
    (nodes, edges, vec![details])
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

#[test]
fn directed_start_at_ensemble_resolves_to_ensemble_cursor() {
    let s = spec("spec-1");
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node("m1", "M1", 1, Value::Null),
        node("m2", "M2", 2, Value::Null),
        node("j", "Join", 3, Value::Null),
    ];
    let edges = vec![
        edge("A", "m1", GraphEdgeCondition::Always),
        edge("A", "m2", GraphEdgeCondition::Always),
        edge("m1", "j", GraphEdgeCondition::Always),
        edge("m2", "j", GraphEdgeCondition::Always),
    ];
    let ensembles = vec![ensemble_details("ens", "ens-name", "A", &["m1", "m2"], "j")];

    let by_name = resolve_directed_target("ens-name", &nodes, &edges, &ensembles).unwrap();
    assert_eq!(by_name.cursor, SpecCursor::Ensemble("ens".to_string()));

    let by_entry = resolve_directed_target("A", &nodes, &edges, &ensembles).unwrap();
    assert_eq!(by_entry.cursor, SpecCursor::Node("A".to_string()));
    assert!(
        !by_entry.skipped_nodes.contains(&"A".to_string()),
        "the named node must not be listed as skipped: {:?}",
        by_entry.skipped_nodes
    );

    let runs = vec![];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "ens-name".to_string(),
    };
    let (cursor, _, _, _) = resolve_spec_start(
        &nodes,
        &edges,
        &s,
        &runs,
        &ensembles,
        false,
        Some(&directed),
    )
    .unwrap();
    assert_eq!(cursor, SpecCursor::Ensemble("ens".to_string()));
}

#[test]
fn directed_start_refuses_join_and_member() {
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node("m1", "M1", 1, Value::Null),
        node("m2", "M2", 2, Value::Null),
        node("j", "Join", 3, Value::Null),
    ];
    let edges = vec![edge("A", "m1", GraphEdgeCondition::Always)];
    let ensembles = vec![ensemble_details("ens", "ens-name", "A", &["m1", "m2"], "j")];

    let join_err = resolve_directed_target("j", &nodes, &edges, &ensembles).unwrap_err();
    assert!(join_err.to_string().contains("ens-name"));
    let member_err = resolve_directed_target("M1", &nodes, &edges, &ensembles).unwrap_err();
    assert!(member_err.to_string().contains("ens-name"));
}

#[test]
fn directed_start_refuses_missing_output_reference() {
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node(
            "B",
            "B",
            1,
            serde_json::json!({ "prompt_template": "{{output:A}} plus work" }),
        ),
    ];
    let edges = vec![edge("A", "B", GraphEdgeCondition::Pass)];
    let node_outputs = HashMap::new();
    let err = check_reachable_outputs(
        &nodes,
        &edges,
        &[],
        &["B".to_string()],
        &node_outputs,
        std::path::Path::new("/nonexistent-prompts"),
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("'A'"), "message: {message}");
    assert!(message.contains("'B'"), "message: {message}");
}

#[test]
fn directed_refusal_sees_preset_body_references() {
    // A preset file is what `resolve_prompt_preset` reads at spawn time —
    // its body's `{{output:A}}` must be validated exactly like an explicit
    // `prompt_template`'s, or a directed run would silently hand the agent
    // "(not yet executed)" for a reference the operator believed reused.
    let dir = tempfile::tempdir().unwrap();
    let prompts = dir.path().join("prompts");
    std::fs::create_dir_all(&prompts).unwrap();
    std::fs::write(
        prompts.join("custom.md"),
        "reuse the earlier output: {{output:A}}",
    )
    .unwrap();
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node(
            "B",
            "B",
            1,
            serde_json::json!({ "prompt_preset": "custom" }),
        ),
    ];
    let edges = vec![edge("A", "B", GraphEdgeCondition::Pass)];

    let err = check_reachable_outputs(
        &nodes,
        &edges,
        &[],
        &["B".to_string()],
        &HashMap::new(),
        &prompts,
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("'A'"), "message: {message}");
    assert!(message.contains("'B'"), "message: {message}");

    let mut node_outputs = HashMap::new();
    node_outputs.insert("A".to_string(), serde_json::json!({ "a": 1 }));
    check_reachable_outputs(
        &nodes,
        &edges,
        &[],
        &["B".to_string()],
        &node_outputs,
        &prompts,
    )
    .unwrap();
}

#[test]
fn directed_start_lists_skipped_nodes() {
    let (nodes, edges) = abc_graph();
    let skipped = skipped_node_names(&nodes, &edges, &["C".to_string()], "A");
    assert_eq!(skipped, vec!["A".to_string(), "B".to_string()]);
}

#[test]
fn directed_skipped_nodes_exclude_dead_end_side_branches() {
    // A → B → C(start), plus a side branch A → E → F that never reaches
    // C: E and F are reachable from the entry but are NOT between Start
    // and the start node, so they must not be reported as skipped.
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node("B", "B", 1, Value::Null),
        node("C", "C", 2, Value::Null),
        node("E", "E", 3, Value::Null),
        node("F", "F", 4, Value::Null),
    ];
    let edges = vec![
        edge("A", "B", GraphEdgeCondition::Pass),
        edge("B", "C", GraphEdgeCondition::Pass),
        edge("A", "E", GraphEdgeCondition::Pass),
        edge("E", "F", GraphEdgeCondition::Pass),
    ];
    let skipped = skipped_node_names(&nodes, &edges, &["C".to_string()], "A");
    assert_eq!(skipped, vec!["A".to_string(), "B".to_string()]);
}

#[test]
fn directed_output_refs_ignore_escaped_markers() {
    assert!(output_refs("\\{{output:A}}").is_empty());
    assert_eq!(output_refs("{{output:A}}"), vec!["A".to_string()]);
}

#[test]
fn plain_mode_has_no_directed_start() {
    assert!(DispatchMode::fresh().directed.is_none());
    assert!(DispatchMode::resume().directed.is_none());
}
