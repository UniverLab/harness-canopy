//! v6 bundles every custom `prompt_preset` the document's agent nodes
//! reference into `prompt_presets` (name + full content), and import
//! installs the bundle — never overwriting a user's edited preset file.
use super::*;
use crate::domain::prompts::resolve_prompt_preset;
use tempfile::tempdir;

/// A minimal two-node document (agent `worker` -> check `next`) whose agent
/// references `preset`, at `format_version` `version`.
fn doc_with_preset(preset: &str, version: i64) -> GraphExportDocument {
    GraphExportDocument {
        format_version: version,
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
        prompt_presets: Vec::new(),
    }
}

/// A two-node graph whose agent node references `preset` as its
/// `prompt_preset`, for exercising the export side.
fn graph_with_preset(preset: &str) -> (Graph, Vec<GraphNode>, Vec<GraphEdge>) {
    let lp = make_graph("preset-graph");
    let nodes = vec![
        make_node(
            "n1",
            "worker",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "claude", "prompt_preset": preset}),
            1,
        ),
        make_node(
            "n2",
            "next",
            GraphNodeKind::Check,
            serde_json::json!({"command": "true"}),
            2,
        ),
    ];
    let edges = vec![make_edge("e1", "n1", "n2", GraphEdgeCondition::Always)];
    (lp, nodes, edges)
}

/// The headline: export of a graph using a custom preset embeds the preset's
/// full content in `prompt_presets`, so the document is self-contained.
#[test]
fn export_bundles_custom_preset_content() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("my-reviewer.md"), "Review it harshly").unwrap();
    let (lp, nodes, edges) = graph_with_preset("my-reviewer");

    let doc = build_export_document_with_pending_and_prompts(
        &lp,
        &nodes,
        &edges,
        &[],
        &HashMap::new(),
        dir.path(),
    )
    .unwrap();

    assert_eq!(
        doc.prompt_presets,
        vec![GraphExportPromptPreset {
            name: "my-reviewer".to_string(),
            content: "Review it harshly".to_string(),
        }]
    );
}

/// Builtins are seeded everywhere and never bundled; a custom preset whose
/// file is missing locally cannot be bundled either — both leave the bundle
/// empty, and an empty bundle is omitted from the serialized document.
#[test]
fn export_omits_builtin_and_missing() {
    let dir = tempdir().unwrap();
    let lp = make_graph("omit-graph");
    let nodes = vec![
        make_node(
            "n1",
            "builtin-user",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "claude", "prompt_preset": "implementer"}),
            1,
        ),
        make_node(
            "n2",
            "missing-user",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "claude", "prompt_preset": "no-such-file"}),
            2,
        ),
        make_node(
            "n3",
            "next",
            GraphNodeKind::Check,
            serde_json::json!({"command": "true"}),
            3,
        ),
    ];
    let edges = vec![
        make_edge("e1", "n1", "n3", GraphEdgeCondition::Always),
        make_edge("e2", "n2", "n3", GraphEdgeCondition::Always),
    ];

    let doc = build_export_document_with_pending_and_prompts(
        &lp,
        &nodes,
        &edges,
        &[],
        &HashMap::new(),
        dir.path(),
    )
    .unwrap();

    assert!(doc.prompt_presets.is_empty());
    let value = serde_json::to_value(&doc).unwrap();
    assert!(
        value.get("prompt_presets").is_none(),
        "an empty bundle must be omitted from the document: {value}"
    );
}

/// Import on a clean prompts dir installs the bundle, and the node's preset
/// then resolves at spawn time — `resolve_prompt_preset` is exactly what the
/// engine reads when it spawns the agent.
#[test]
fn import_installs_bundle_on_clean_dir() {
    let dir = tempdir().unwrap();
    let mut doc = doc_with_preset("shipped-preset", 6);
    doc.prompt_presets = vec![GraphExportPromptPreset {
        name: "shipped-preset".to_string(),
        content: "Do the work thoroughly".to_string(),
    }];

    let plan = build_import_plan_with_prompts(&doc, "g", dir.path())
        .expect("a bundled preset must import on a clean prompts dir");

    assert_eq!(plan.nodes.len(), 2);
    let installed = dir.path().join("shipped-preset.md");
    assert_eq!(
        std::fs::read_to_string(&installed).unwrap(),
        "Do the work thoroughly"
    );
    assert_eq!(
        resolve_prompt_preset(dir.path(), "shipped-preset"),
        "Do the work thoroughly",
        "the spawned agent must resolve the bundled preset"
    );
}

/// A bundled preset whose name exists locally with DIFFERENT content
/// refuses the import, naming the preset — and nothing is installed: the
/// local file stays byte-identical and the other bundled entry's file is
/// never created.
#[test]
fn import_conflict_refuses_and_keeps_file() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("mine.md"), "my own edited text").unwrap();
    let mut doc = doc_with_preset("mine", 6);
    // The conflicting entry is LAST, so every other bundled entry already
    // validated — the refusal must still install nothing at all.
    doc.prompt_presets = vec![
        GraphExportPromptPreset {
            name: "sibling".to_string(),
            content: "harmless".to_string(),
        },
        GraphExportPromptPreset {
            name: "mine".to_string(),
            content: "the document's text".to_string(),
        },
    ];

    let err = build_import_plan_with_prompts(&doc, "g", dir.path())
        .expect_err("differing local content must refuse the import");

    assert!(err.contains("mine"), "error must name the preset: {err}");
    assert!(
        err.contains("refusing to overwrite"),
        "error must say why: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("mine.md")).unwrap(),
        "my own edited text",
        "the local preset file must stay byte-identical"
    );
    assert!(
        !dir.path().join("sibling.md").exists(),
        "a refused import must install nothing"
    );
}

/// A bundled preset whose name exists locally with IDENTICAL content is a
/// no-op: the import proceeds and the file is untouched.
#[test]
fn import_identical_local_content_is_noop() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("twin.md"), "same text").unwrap();
    let mut doc = doc_with_preset("twin", 6);
    doc.prompt_presets = vec![GraphExportPromptPreset {
        name: "twin".to_string(),
        content: "same text".to_string(),
    }];

    let plan = build_import_plan_with_prompts(&doc, "g", dir.path())
        .expect("identical local content must be a no-op");

    assert_eq!(plan.nodes.len(), 2);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("twin.md")).unwrap(),
        "same text"
    );
}

/// Older documents without the bundle, and a v6 document that bundles zero
/// presets, still import: the bundle is optional at every version.
#[test]
fn import_v5_and_v6_without_bundle_still_import() {
    // v5: no bundle field at all, preset known from a local file.
    let v5 = tempdir().unwrap();
    std::fs::write(v5.path().join("legacy.md"), "legacy text").unwrap();
    let doc = doc_with_preset("legacy", 5);
    build_import_plan_with_prompts(&doc, "g", v5.path())
        .expect("a v5 document must import unchanged");

    // v6: an explicit zero-preset bundle, preset known from a local file.
    let v6 = tempdir().unwrap();
    std::fs::write(v6.path().join("local-six.md"), "local").unwrap();
    let doc = doc_with_preset("local-six", 6);
    build_import_plan_with_prompts(&doc, "g", v6.path())
        .expect("a v6 document bundling zero presets must import");
}

/// The bundle must never smuggle in anything that could clobber or escape
/// the prompts dir: builtin names, duplicate entries, path-escaping names,
/// and empty content are all refusals.
#[test]
fn import_refuses_builtin_duplicate_or_unsafe_entries() {
    let mut builtin = doc_with_preset("implementer", 6);
    builtin.prompt_presets = vec![GraphExportPromptPreset {
        name: "implementer".to_string(),
        content: "not allowed".to_string(),
    }];
    let err = build_import_plan_with_prompts(&builtin, "g", tempdir().unwrap().path())
        .expect_err("bundling a builtin must be refused");
    assert!(err.contains("implementer"), "{err}");
    assert!(err.contains("builtin"), "{err}");

    let mut duplicate = doc_with_preset("dup", 6);
    duplicate.prompt_presets = vec![
        GraphExportPromptPreset {
            name: "dup".to_string(),
            content: "one".to_string(),
        },
        GraphExportPromptPreset {
            name: "dup".to_string(),
            content: "two".to_string(),
        },
    ];
    let err = build_import_plan_with_prompts(&duplicate, "g", tempdir().unwrap().path())
        .expect_err("a duplicate bundled name must be refused");
    assert!(err.contains("dup"), "{err}");
    assert!(err.contains("more than once"), "{err}");

    let mut unsafe_name = doc_with_preset("worker", 6);
    unsafe_name.prompt_presets = vec![GraphExportPromptPreset {
        name: "../escape".to_string(),
        content: "outside".to_string(),
    }];
    let err = build_import_plan_with_prompts(&unsafe_name, "g", tempdir().unwrap().path())
        .expect_err("a path-escaping bundled name must be refused");
    assert!(err.contains("../escape"), "{err}");

    let mut empty_content = doc_with_preset("hollow", 6);
    empty_content.prompt_presets = vec![GraphExportPromptPreset {
        name: "hollow".to_string(),
        content: String::new(),
    }];
    let err = build_import_plan_with_prompts(&empty_content, "g", tempdir().unwrap().path())
        .expect_err("empty bundled content must be refused");
    assert!(err.contains("hollow"), "{err}");
    assert!(err.contains("empty"), "{err}");
}
