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

/// A preset several nodes share is bundled exactly once, and an empty preset
/// file is never bundled: import refuses an empty entry, so export must not
/// emit a document this build would reject on a clean machine.
#[test]
fn export_dedupes_shared_preset_and_skips_empty_file() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("shared.md"), "shared text").unwrap();
    std::fs::write(dir.path().join("blank.md"), "").unwrap();
    let lp = make_graph("dedupe-graph");
    let agent = |name: &str, preset: &str, position: i64| {
        make_node(
            name,
            name,
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "claude", "prompt_preset": preset}),
            position,
        )
    };
    let nodes = vec![
        agent("aria", "shared", 1),
        agent("bo", "shared", 2),
        agent("cy", "blank", 3),
        agent("di", "implementer", 4),
        agent("ed", "gone", 5),
        make_node(
            "end",
            "end",
            GraphNodeKind::Check,
            serde_json::json!({"command": "true"}),
            6,
        ),
    ];
    let edges = ["aria", "bo", "cy", "di", "ed"]
        .iter()
        .enumerate()
        .map(|(index, from)| {
            make_edge(
                &format!("e{index}"),
                from,
                "end",
                GraphEdgeCondition::Always,
            )
        })
        .collect::<Vec<_>>();

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
            name: "shared".to_string(),
            content: "shared text".to_string(),
        }]
    );
}

/// The requirement end to end: what export writes is what import installs on
/// a machine that never had the preset, and the imported node's spawn then
/// resolves that prompt (`resolve_prompt_preset` is what the engine reads).
#[test]
fn export_then_import_installs_the_preset_on_a_clean_machine() {
    let source = tempdir().unwrap();
    std::fs::write(source.path().join("my-reviewer.md"), "Review it harshly").unwrap();
    let (lp, nodes, edges) = graph_with_preset("my-reviewer");
    let doc = build_export_document_with_pending_and_prompts(
        &lp,
        &nodes,
        &edges,
        &[],
        &HashMap::new(),
        source.path(),
    )
    .unwrap();

    // Through the serialized form a shared file/CLI document travels in.
    let json = serde_json::to_string(&doc).unwrap();
    let parsed = parse_export_document_str(&json).unwrap();

    let clean = tempdir().unwrap();
    let plan = build_import_plan_with_prompts(&parsed, "g", clean.path())
        .expect("a document that bundles its preset must import without it");
    assert_eq!(plan.nodes.len(), 2);
    assert_eq!(
        std::fs::read_to_string(clean.path().join("my-reviewer.md")).unwrap(),
        "Review it harshly"
    );
    assert_eq!(
        resolve_prompt_preset(clean.path(), "my-reviewer"),
        "Review it harshly",
        "the imported node's spawn must find the bundled prompt"
    );
}

/// Import on a clean prompts dir installs the bundle and the node's preset
/// then resolves at spawn time. Import has exactly one path — the plan
/// builder both MCP and CLI go through — so a plan that succeeded is a plan
/// whose presets are on disk.
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
    assert_eq!(
        std::fs::read_to_string(dir.path().join("shipped-preset.md")).unwrap(),
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

/// `install` on its own never writes outside the prompts dir and never
/// overwrites: it re-validates first, so a direct call with a path-escaping
/// name, a builtin, or a differing local file is a refusal that changes
/// nothing — not just when reached through the plan builder.
#[test]
fn install_standalone_refuses_unsafe_builtin_or_conflicting() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("mine.md"), "my own edited text").unwrap();

    let conflicting = vec![GraphExportPromptPreset {
        name: "mine".to_string(),
        content: "the document's text".to_string(),
    }];
    let err = install(&conflicting, dir.path())
        .expect_err("a differing local file must refuse a standalone install");
    assert!(err.contains("mine"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("mine.md")).unwrap(),
        "my own edited text"
    );

    let escaping = vec![GraphExportPromptPreset {
        name: "../escape".to_string(),
        content: "outside".to_string(),
    }];
    let err = install(&escaping, dir.path())
        .expect_err("a path-escaping name must refuse a standalone install");
    assert!(err.contains("../escape"), "{err}");
    assert!(
        !dir.path().join("../escape.md").exists(),
        "nothing may be written outside the prompts dir"
    );

    let builtin = vec![GraphExportPromptPreset {
        name: "reviewer".to_string(),
        content: "not allowed".to_string(),
    }];
    let err =
        install(&builtin, dir.path()).expect_err("a builtin must refuse a standalone install");
    assert!(err.contains("reviewer"), "{err}");

    let identical = vec![GraphExportPromptPreset {
        name: "mine".to_string(),
        content: "my own edited text".to_string(),
    }];
    install(&identical, dir.path()).expect("identical content must be a no-op");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("mine.md")).unwrap(),
        "my own edited text"
    );
}

/// A catalogue install stamps every preset it writes with its source, and a
/// later install from the same source updates the file in place — no
/// overwrite flag needed for a reinstall.
#[test]
fn install_for_install_stamps_the_source_and_updates_it_in_place() {
    let dir = tempdir().unwrap();
    let first = vec![GraphExportPromptPreset {
        name: "custom".to_string(),
        content: "v1".to_string(),
    }];
    install_for_install(&first, dir.path(), "my-graph", false).unwrap();
    assert_eq!(
        read_owners(dir.path()).get("custom").map(String::as_str),
        Some("my-graph")
    );
    let sidecar = std::fs::read_to_string(dir.path().join(".sources.json")).unwrap();
    assert!(sidecar.contains("my-graph"), "{sidecar}");

    let second = vec![GraphExportPromptPreset {
        name: "custom".to_string(),
        content: "v2".to_string(),
    }];
    install_for_install(&second, dir.path(), "my-graph", false)
        .expect("the same source updates in place");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("custom.md")).unwrap(),
        "v2"
    );
    assert_eq!(
        read_owners(dir.path()).get("custom").map(String::as_str),
        Some("my-graph")
    );
}

/// A preset owned by another source is never overwritten silently: the
/// refusal names the preset, both sources, and the way out — and writes
/// nothing, not even the presets that would have installed.
#[test]
fn install_for_install_refuses_a_preset_owned_by_another_source() {
    let dir = tempdir().unwrap();
    let official = vec![GraphExportPromptPreset {
        name: "custom".to_string(),
        content: "official text".to_string(),
    }];
    install_for_install(&official, dir.path(), "my-graph", false).unwrap();

    let community = vec![
        GraphExportPromptPreset {
            name: "aaa".to_string(),
            content: "aaa content".to_string(),
        },
        GraphExportPromptPreset {
            name: "custom".to_string(),
            content: "community text".to_string(),
        },
    ];
    let error = install_for_install(&community, dir.path(), "alice/lab/my-graph", false)
        .expect_err("another source must refuse");
    assert!(error.contains("'custom'"), "{error}");
    assert!(error.contains("different content"), "{error}");
    assert!(error.contains("'my-graph'"), "{error}");
    assert!(error.contains("'alice/lab/my-graph'"), "{error}");
    assert!(error.contains("--as"), "{error}");
    assert!(error.contains("--replace"), "{error}");
    assert!(error.contains("--overwrite-presets"), "{error}");
    assert!(
        !dir.path().join("aaa.md").exists(),
        "a refusal writes nothing, not even the clean presets"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("custom.md")).unwrap(),
        "official text"
    );
    assert_eq!(
        read_owners(dir.path()).get("custom").map(String::as_str),
        Some("my-graph")
    );

    install_for_install(&community, dir.path(), "alice/lab/my-graph", true)
        .expect("overwrite replaces regardless of owner");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("custom.md")).unwrap(),
        "community text"
    );
    assert_eq!(
        read_owners(dir.path()).get("custom").map(String::as_str),
        Some("alice/lab/my-graph")
    );
}

/// A differing file with no recorded owner — hand-written, or installed
/// before sources were recorded — is refused as a local file.
#[test]
fn install_for_install_refuses_a_local_file_as_local() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("custom.md"), "my own edited text").unwrap();
    let bundled = vec![GraphExportPromptPreset {
        name: "custom".to_string(),
        content: "bundled text".to_string(),
    }];
    let error = install_for_install(&bundled, dir.path(), "my-graph", false)
        .expect_err("a local file must refuse");
    assert!(error.contains("local"), "{error}");
    assert!(error.contains("'my-graph'"), "{error}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("custom.md")).unwrap(),
        "my own edited text"
    );
}

/// Identical content is a no-op that adopts nothing: a matching local file
/// gains no owner, and an owned file keeps its owner.
#[test]
fn install_for_install_leaves_identical_files_and_their_owners_alone() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("mine.md"), "same text").unwrap();
    let matching = vec![GraphExportPromptPreset {
        name: "mine".to_string(),
        content: "same text".to_string(),
    }];
    install_for_install(&matching, dir.path(), "my-graph", false).unwrap();
    assert!(
        !read_owners(dir.path()).contains_key("mine"),
        "a matching local file is never adopted"
    );

    let owned = vec![GraphExportPromptPreset {
        name: "theirs".to_string(),
        content: "their text".to_string(),
    }];
    install_for_install(&owned, dir.path(), "acme/widgets/my-graph", false).unwrap();
    install_for_install(&owned, dir.path(), "acme/widgets/my-graph", false).unwrap();
    assert_eq!(
        read_owners(dir.path()).get("theirs").map(String::as_str),
        Some("acme/widgets/my-graph")
    );
}

/// A missing sidecar — or one that does not parse — is an empty map: every
/// file is simply local.
#[test]
fn read_owners_treats_missing_and_corrupt_sidecars_as_empty() {
    let dir = tempdir().unwrap();
    assert!(read_owners(dir.path()).is_empty());
    std::fs::write(dir.path().join(".sources.json"), "not json{").unwrap();
    assert!(read_owners(dir.path()).is_empty());
    std::fs::write(dir.path().join(".sources.json"), "[1, 2]").unwrap();
    assert!(read_owners(dir.path()).is_empty());
}

/// Stamping records the listed presets and keeps every other owner.
#[test]
fn stamp_owners_keeps_owners_it_was_not_asked_to_change() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path()).unwrap();
    stamp_owners(
        dir.path(),
        &[GraphExportPromptPreset {
            name: "custom".to_string(),
            content: "custom text".to_string(),
        }],
        "my-graph",
    )
    .unwrap();
    stamp_owners(
        dir.path(),
        &[GraphExportPromptPreset {
            name: "other".to_string(),
            content: "other text".to_string(),
        }],
        "alice/lab/other",
    )
    .unwrap();
    let owners = read_owners(dir.path());
    assert_eq!(owners.get("custom").map(String::as_str), Some("my-graph"));
    assert_eq!(
        owners.get("other").map(String::as_str),
        Some("alice/lab/other")
    );
}
