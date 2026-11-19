use super::*;

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
    let reused = reused_output_records(&nodes, &edges, &["B".to_string()], "A", &runs);
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
