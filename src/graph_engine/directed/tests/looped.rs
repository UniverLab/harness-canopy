use super::*;

/// The canopy-engine-next shape: Start → Route tier → Plan check →
/// Implementer → Reviewer → Full gates → Auditor members → Auditor join →
/// Full gates after auditor → Commit spec, with fail back-edges from
/// Reviewer, Full gates and the Auditor join to the Implementer, a Fail edge
/// from Route tier to Plan check, and the Resilience error loop: any node
/// may error into Resilience, which passes back to Route tier. Resilience
/// is load-bearing — without it a Route tier start never sees the Auditor
/// pre-fix, because only the error back-path threads the later nodes
/// through the start.
fn looped_graph() -> (Vec<GraphNode>, Vec<GraphEdge>, Vec<EnsembleDetails>) {
    let nodes = vec![
        node("s", "Start", 0, Value::Null),
        node("rt", "Route tier", 1, Value::Null),
        node("pc", "Plan check", 2, Value::Null),
        node("im", "Implementer", 3, Value::Null),
        node("rv", "Reviewer", 4, Value::Null),
        node("fg", "Full gates", 5, Value::Null),
        node("aud_m", "Auditor member", 6, Value::Null),
        node("aud", "Auditor join", 7, Value::Null),
        node("fg2", "Full gates after auditor", 8, Value::Null),
        node("cs", "Commit spec", 9, Value::Null),
        node("res", "Resilience", 10, Value::Null),
    ];
    let edges = vec![
        edge("s", "rt", GraphEdgeCondition::Pass),
        edge("rt", "pc", GraphEdgeCondition::Fail),
        edge("pc", "im", GraphEdgeCondition::Pass),
        edge("im", "rv", GraphEdgeCondition::Pass),
        edge("rv", "fg", GraphEdgeCondition::Pass),
        edge("fg", "aud_m", GraphEdgeCondition::Always),
        edge("aud_m", "aud", GraphEdgeCondition::Always),
        edge("aud", "fg2", GraphEdgeCondition::Pass),
        edge("fg2", "cs", GraphEdgeCondition::Pass),
        edge("rv", "im", GraphEdgeCondition::Fail),
        edge("fg", "im", GraphEdgeCondition::Fail),
        edge("aud", "im", GraphEdgeCondition::Fail),
        edge("rt", "res", GraphEdgeCondition::Error),
        edge("aud", "res", GraphEdgeCondition::Error),
        edge("res", "rt", GraphEdgeCondition::Pass),
    ];
    let ensembles = vec![ensemble_details("ens", "Auditor", "fg", &["aud_m"], "aud")];
    (nodes, edges, ensembles)
}

fn pass_run(node_id: &str) -> GraphNodeRun {
    run(
        node_id,
        GraphRunStatus::Pass,
        Some(serde_json::json!({"o": "out"})),
        None,
    )
}

/// a) Starting at Route tier no longer asks for the Auditor verdict: the
/// Auditor sits after Route tier and only reaches it through the error
/// back-path. Breaks pre-fix, which refuses with "no recorded verdict".
#[test]
fn directed_start_at_route_tier_ignores_auditor_behind_back_edge() {
    let s = spec("spec-1");
    let (nodes, edges, ensembles) = looped_graph();
    let runs = vec![pass_run("aud_m")];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "Route tier".to_string(),
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
    .expect("the Auditor is not a prerequisite of Route tier");
}

/// b) A start inside the loop keeps the loop nodes that precede it on the
/// entry's path and drops those only reachable through it. Resilience stays
/// upstream of later starts: it is reachable from the entry without passing
/// the start and it reaches the start.
#[test]
fn directed_upstream_names_only_nodes_before_the_start() {
    let (_, edges, _) = looped_graph();
    let upstream = directed_upstream_node_ids(&edges, &["fg".to_string()], "s");
    let mut got: Vec<&str> = upstream.iter().map(String::as_str).collect();
    got.sort_unstable();
    assert_eq!(got, vec!["im", "pc", "res", "rt", "rv", "s"]);
    let upstream =
        directed_upstream_node_ids(&edges, &["aud_m".to_string(), "aud".to_string()], "s");
    let mut got: Vec<&str> = upstream.iter().map(String::as_str).collect();
    got.sort_unstable();
    assert_eq!(got, vec!["fg", "im", "pc", "res", "rt", "rv", "s"]);
}

/// c) Starting at the Implementer still requires Plan check: it really
/// does lie before it on the entry's path.
#[test]
fn directed_start_at_implementer_still_requires_plan_check() {
    let s = spec("spec-1");
    let (nodes, edges, ensembles) = looped_graph();
    let runs = vec![run(
        "pc",
        GraphRunStatus::Fail,
        Some(serde_json::json!({"o": "out"})),
        None,
    )];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "Implementer".to_string(),
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
    assert!(err.to_string().contains("'Plan check'"), "{err}");
}

/// d) Starting at Commit spec with a Fail Route tier run (Fail edge to Plan
/// check) and Pass everywhere else is accepted: the Fail is routed toward
/// the start.
#[test]
fn directed_start_at_commit_spec_accepts_routed_route_tier_fail() {
    let s = spec("spec-1");
    let (nodes, edges, ensembles) = looped_graph();
    let mut runs = vec![
        run(
            "rt",
            GraphRunStatus::Fail,
            Some(serde_json::json!({"o": "out"})),
            None,
        ),
        pass_run("pc"),
        pass_run("im"),
        pass_run("rv"),
        pass_run("fg"),
        pass_run("aud_m"),
        pass_run("aud"),
        pass_run("fg2"),
    ];
    runs[0].id = "run-rt".to_string();
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "Commit spec".to_string(),
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
    .expect("a Fail routed by a fail edge toward the start is met");
}

/// e) The same start with an Interrupted upstream run is still refused.
#[test]
fn directed_start_at_commit_spec_refuses_interrupted_upstream() {
    let s = spec("spec-1");
    let (nodes, edges, ensembles) = looped_graph();
    let runs = vec![
        run(
            "rt",
            GraphRunStatus::Interrupted,
            Some(serde_json::json!({"o": "out"})),
            None,
        ),
        pass_run("pc"),
    ];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "Commit spec".to_string(),
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
        err.to_string().contains("last run was interrupted"),
        "{err}"
    );
}

/// f) Reused outputs never seed a node only reachable through the start:
/// starting at Route tier seeds only Start, even when everything passed.
#[test]
fn directed_reuse_never_seeds_nodes_only_reachable_through_start() {
    let s = spec("spec-1");
    let (nodes, edges, ensembles) = looped_graph();
    let runs = vec![
        pass_run("s"),
        pass_run("rt"),
        pass_run("pc"),
        pass_run("im"),
        pass_run("rv"),
        pass_run("fg"),
        pass_run("aud_m"),
        pass_run("aud"),
        pass_run("fg2"),
    ];
    let directed = DirectedStart {
        spec_id: s.id.clone(),
        node_arg: "Route tier".to_string(),
    };
    let (_, node_outputs, _, _) = resolve_spec_start(
        &nodes,
        &edges,
        &s,
        &runs,
        &ensembles,
        false,
        Some(&directed),
    )
    .unwrap();
    assert_eq!(node_outputs.keys().collect::<Vec<_>>(), vec!["Start"]);
    let target = resolve_directed_target("Route tier", &nodes, &edges, &ensembles).unwrap();
    let reused =
        reused_output_records(&nodes, &edges, &target.start_node_ids, &target.entry, &runs);
    assert_eq!(reused, vec![("Start".to_string(), "run-s".to_string())]);
}
