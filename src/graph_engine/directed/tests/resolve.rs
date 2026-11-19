use super::*;

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
    assert!(join_err
        .to_string()
        .contains("belongs to ensemble 'ens-name'"));
    assert!(join_err
        .to_string()
        .contains("Target the ensemble by its name ('ens-name')"));
    let member_err = resolve_directed_target("M1", &nodes, &edges, &ensembles).unwrap_err();
    assert!(member_err
        .to_string()
        .contains("belongs to ensemble 'ens-name'"));
    assert!(member_err
        .to_string()
        .contains("Target the ensemble by its name ('ens-name')"));
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
