//! Import refuses an unknown custom prompt preset instead of letting the node
//! run on an empty prompt (see `prompts::resolve_prompt_preset`).
use super::*;
use tempfile::tempdir;

/// A one-agent document naming `preset`, plus the check node it exits to —
/// enough structure that a successful import is a real two-node graph.
fn doc_with_agent_preset(preset: &str) -> GraphExportDocument {
    GraphExportDocument {
        prompt_presets: Vec::new(),
        format_version: 1,
        name: "shared".to_string(),
        description: None,
        nodes: vec![
            GraphExportNode {
                name: "worker".to_string(),
                kind: GraphNodeKind::Agent,
                position: 1,
                config: serde_json::json!({
                    "platform": "claude",
                    "prompt_preset": preset,
                }),
            },
            GraphExportNode {
                name: "next".to_string(),
                kind: GraphNodeKind::Check,
                position: 2,
                config: serde_json::json!({"command": "true"}),
            },
        ],
        edges: vec![GraphExportEdge {
            from_node: "worker".to_string(),
            to_node: "next".to_string(),
            condition: GraphEdgeCondition::Always,
        }],
        ensembles: vec![],
        infra_node: None,
        hooks: Default::default(),
    }
}

/// The failure this spec exists for: a preset the local prompts dir has no
/// file for must reject the import, naming the preset (not a bare "invalid
/// graph").
#[test]
fn import_rejects_unknown_custom_preset_and_names_it() {
    let dir = tempdir().unwrap();
    let doc = doc_with_agent_preset("missing-custom-x");

    let err = build_import_plan_with_prompts(&doc, "g", dir.path()).unwrap_err();

    assert!(
        err.contains("missing-custom-x"),
        "error must name the unknown preset: {err}"
    );
    assert!(
        err.starts_with("Unknown prompt preset(s):"),
        "error must say what kind of problem it is: {err}"
    );
    assert!(
        err.contains(&dir.path().display().to_string()),
        "error must say WHERE to put the missing preset file: {err}"
    );
    assert!(
        err.contains("implementer, reviewer, resilience"),
        "error must list the builtins that need no file: {err}"
    );
}

/// A custom preset whose file IS present locally resolves at spawn time and
/// therefore imports normally.
#[test]
fn import_accepts_custom_preset_present_locally() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("local-custom.md"), "do the work").unwrap();
    let doc = doc_with_agent_preset("local-custom");

    let plan = build_import_plan_with_prompts(&doc, "g", dir.path())
        .expect("a locally present custom preset must import");

    assert_eq!(plan.nodes.len(), 2);
}

/// Builtins are always known — they are seeded on daemon start and fall back
/// to embedded constants — so a fresh prompts dir (nothing written yet) must
/// still accept them.
#[test]
fn import_builtin_preset_passes_on_fresh_prompts_dir() {
    let dir = tempdir().unwrap();
    let doc = doc_with_agent_preset("implementer");

    build_import_plan_with_prompts(&doc, "g", dir.path())
        .expect("a builtin preset needs no file on disk");
}

/// `implementer`, `reviewer` and `resilience` are all known without a single
/// file on disk — the check is not hardcoded to one name.
#[test]
fn all_three_builtins_known_on_fresh_dir() {
    let dir = tempdir().unwrap();
    let mut doc = doc_with_agent_preset("implementer");
    for (position, name) in [(3, "reviewer"), (4, "resilience")] {
        doc.nodes.push(GraphExportNode {
            name: name.to_string(),
            kind: GraphNodeKind::Agent,
            position,
            config: serde_json::json!({
                "platform": "claude",
                "prompt_preset": name,
            }),
        });
    }

    reject_unknown_prompt_presets(&doc, dir.path()).expect("all three builtins are known");
}

/// Every unknown preset is named, not just the first one found — the operator
/// adds them all in one pass.
#[test]
fn error_names_every_unknown_preset() {
    let dir = tempdir().unwrap();
    let mut doc = doc_with_agent_preset("missing-one");
    doc.nodes.push(GraphExportNode {
        name: "second".to_string(),
        kind: GraphNodeKind::Agent,
        position: 3,
        config: serde_json::json!({
            "platform": "claude",
            "prompt_preset": "missing-two",
        }),
    });

    let err = build_import_plan_with_prompts(&doc, "g", dir.path()).unwrap_err();

    assert!(err.contains("missing-one"), "{err}");
    assert!(err.contains("missing-two"), "{err}");
    assert!(
        err.contains("missing-one, missing-two"),
        "unknown presets must be sorted and joined: {err}"
    );
}

/// The check is about `prompt_preset` only: a node carrying an explicit
/// `prompt_template`, or no prompt at all, imports even though the prompts
/// dir is empty.
#[test]
fn nodes_without_a_preset_are_not_checked() {
    let dir = tempdir().unwrap();
    let doc = GraphExportDocument {
        prompt_presets: Vec::new(),
        format_version: 1,
        name: "shared".to_string(),
        description: None,
        nodes: vec![
            GraphExportNode {
                name: "worker".to_string(),
                kind: GraphNodeKind::Agent,
                position: 1,
                config: serde_json::json!({
                    "platform": "claude",
                    "prompt_template": "{{spec_content}}",
                }),
            },
            GraphExportNode {
                name: "next".to_string(),
                kind: GraphNodeKind::Check,
                position: 2,
                config: serde_json::json!({"command": "true"}),
            },
        ],
        edges: vec![GraphExportEdge {
            from_node: "worker".to_string(),
            to_node: "next".to_string(),
            condition: GraphEdgeCondition::Always,
        }],
        ensembles: vec![],
        infra_node: None,
        hooks: Default::default(),
    };

    build_import_plan_with_prompts(&doc, "g", dir.path()).expect("no preset referenced");
}

/// A whitespace-only preset is not a reference: the spawn path skips it and
/// takes the default template, so import must not refuse a document the
/// engine can run.
#[test]
fn whitespace_only_preset_is_not_a_reference() {
    let dir = tempdir().unwrap();
    let doc = doc_with_agent_preset("   ");

    build_import_plan_with_prompts(&doc, "g", dir.path())
        .expect("a blank preset falls back to the default template at spawn");
}

/// `spec_ids` round-trips through the export document: the serialized form
/// carries the field, parsing keeps it, and the import plan restores the
/// hooks verbatim.
#[test]
fn export_import_round_trips_hook_spec_ids() {
    use crate::domain::graphs::GraphCompletionHook;
    let (mut graph, nodes, edges) = simple_graph();
    let hook = GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some("true".to_string()),
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,
        crew: None,
        spec_ids: Some(vec!["spec-a".to_string()]),
    };
    graph.hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnSpecCompleted,
        vec![hook.clone()],
    );

    let doc = build_export_document(&graph, &nodes, &edges, &[]).unwrap();
    let value = serde_json::to_value(&doc).unwrap();
    assert_eq!(
        value["hooks"]["on_spec_completed"][0]["spec_ids"],
        serde_json::json!(["spec-a"]),
        "the serialized document must carry spec_ids"
    );

    let parsed = parse_export_document_value(&value).unwrap();
    assert_eq!(
        parsed
            .hooks
            .get(&crate::domain::graphs::GraphHookEvent::OnSpecCompleted),
        Some(&vec![hook.clone()]),
        "parsing must keep the field"
    );

    let plan = build_import_plan(&parsed, "new-graph").unwrap();
    assert_eq!(
        plan.hooks
            .get(&crate::domain::graphs::GraphHookEvent::OnSpecCompleted),
        Some(&vec![hook]),
        "import must restore the hooks verbatim"
    );
}
