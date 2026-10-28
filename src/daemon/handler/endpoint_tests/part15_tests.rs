//! Part 15 of `endpoint_tests`: shareable node documents end to end through
//! the MCP surface — `node_install`/`node_list`/`node_remove` against a local
//! HTTP stub standing in for the catalogue, and `graph_add_node` /
//! `graph_add_ensemble` instantiating a node document by name.

use super::*;
use crate::daemon::graph_install::stub::{StubCatalogue, StubRoute};
use crate::domain::nodes;

/// RAII guard: points `CANOPY_HOME_OVERRIDE` at `path` for the test body,
/// restoring the previous value on drop. Safe under `cargo nextest` (one
/// process per test) but would race under plain `cargo test`.
struct OverrideHome {
    prev: Option<std::ffi::OsString>,
}

impl OverrideHome {
    fn set(path: &std::path::Path) -> Self {
        let prev = std::env::var_os("CANOPY_HOME_OVERRIDE");
        unsafe { std::env::set_var("CANOPY_HOME_OVERRIDE", path) };
        OverrideHome { prev }
    }
}

impl Drop for OverrideHome {
    fn drop(&mut self) {
        match &self.prev {
            Some(value) => unsafe { std::env::set_var("CANOPY_HOME_OVERRIDE", value) },
            None => unsafe { std::env::remove_var("CANOPY_HOME_OVERRIDE") },
        }
    }
}

/// A temp home whose `config.toml` points the catalogue at `catalogue_url`,
/// with empty nodes and prompts dirs. Returns the home (kept alive by the
/// caller), the guard, and both dirs.
fn node_home(
    catalogue_url: &str,
) -> (
    tempfile::TempDir,
    OverrideHome,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let home = tempdir().unwrap();
    let canopy = home.path().join(".canopy");
    let nodes = canopy.join("nodes");
    let prompts = canopy.join("prompts");
    std::fs::create_dir_all(&nodes).unwrap();
    std::fs::create_dir_all(&prompts).unwrap();
    std::fs::write(
        canopy.join("config.toml"),
        format!("[graphs]\ncatalogue_url = \"{catalogue_url}\"\n"),
    )
    .unwrap();
    let guard = OverrideHome::set(home.path());
    (home, guard, nodes, prompts)
}

fn auditor_doc() -> serde_json::Value {
    serde_json::json!({
        "format_version": 1,
        "name": "auditor",
        "kind": "agent",
        "description": "A shareable audit node.",
        "config": {"prompt_template": "audit this", "timeout_minutes": 10},
        "preset": {"name": "auditor-preset", "content": "audit well"},
    })
}

fn stub_serving(path: &str, body: &str) -> StubCatalogue {
    StubCatalogue::start(
        [(path.to_string(), StubRoute::ok(body))]
            .into_iter()
            .collect(),
    )
}

async fn install_node(
    handler: &TaskTriggerHandler,
    reference: &str,
    overwrite_presets: Option<bool>,
) -> rmcp::model::CallToolResult {
    handler
        .node_install(Parameters(NodeInstallParams {
            reference: reference.to_string(),
            overwrite_presets,
        }))
        .await
        .unwrap()
}

async fn remove_node(handler: &TaskTriggerHandler, name: &str) -> rmcp::model::CallToolResult {
    handler
        .node_remove(Parameters(NodeRemoveParams {
            name: name.to_string(),
        }))
        .await
        .unwrap()
}

fn overrides(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), serde_json::json!(value)))
        .collect()
}

fn openrouter_member(model: &str) -> EnsembleMemberParams {
    EnsembleMemberParams {
        platform: "openrouter".to_string(),
        model: Some(model.to_string()),
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }
}

/// `node_install` writes the document under ~/.canopy/nodes and its bundled
/// preset under ~/.canopy/prompts, and `node_list` reports it as installed.
#[tokio::test]
async fn node_install_writes_the_document_and_its_preset() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let stub = stub_serving("/i/node/auditor", &auditor_doc().to_string());
    let (_home, _guard, nodes_dir, prompts_dir) = node_home(&stub.base_url());

    let installed = install_node(&handler, "auditor", None).await;
    assert!(!is_err(&installed), "{}", text(&installed));
    let report: serde_json::Value = serde_json::from_str(&raw_text(&installed)).unwrap();
    assert_eq!(report["name"], "auditor");
    assert_eq!(report["kind"], "agent");
    assert_eq!(
        report["source_url"],
        format!("{}/i/node/auditor", stub.base_url())
    );
    assert_eq!(
        report["presets_written"],
        serde_json::json!(["auditor-preset"])
    );

    let raw = std::fs::read_to_string(nodes_dir.join("auditor.json")).unwrap();
    let stored = nodes::parse_document(&raw).unwrap();
    assert_eq!(stored.name, "auditor");
    assert_eq!(stored.config["prompt_template"], "audit this");
    assert_eq!(
        std::fs::read_to_string(prompts_dir.join("auditor-preset.md")).unwrap(),
        "audit well"
    );

    let listed = handler.node_list().await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&raw_text(&listed)).unwrap();
    let entry = body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "auditor")
        .expect("auditor is listed");
    assert_eq!(entry["source"], "installed");
    let builtin = body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "implementer")
        .expect("builtin listed offline");
    assert_eq!(builtin["source"], "builtin");
}

/// A preset conflict aborts the install: no preset file is overwritten and no
/// node document is written.
#[tokio::test]
async fn node_install_preset_conflict_writes_nothing() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let stub = stub_serving("/i/node/auditor", &auditor_doc().to_string());
    let (_home, _guard, nodes_dir, prompts_dir) = node_home(&stub.base_url());
    std::fs::write(prompts_dir.join("auditor-preset.md"), "old content").unwrap();

    let installed = install_node(&handler, "auditor", None).await;
    assert!(is_err(&installed), "{}", text(&installed));
    assert!(
        text(&installed).contains("different content"),
        "{}",
        text(&installed)
    );
    assert_eq!(
        std::fs::read_to_string(prompts_dir.join("auditor-preset.md")).unwrap(),
        "old content"
    );
    assert!(
        !nodes_dir.join("auditor.json").exists(),
        "a conflict aborts before the document is written"
    );
}

/// `node_remove` refuses a builtin with no installed copy, naming the
/// override that would make it removable.
#[tokio::test]
async fn node_remove_refuses_a_builtin() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let stub = StubCatalogue::start(std::collections::HashMap::new());
    let (_home, _guard, _nodes_dir, _prompts_dir) = node_home(&stub.base_url());

    let removed = remove_node(&handler, "implementer").await;
    assert!(is_err(&removed), "{}", text(&removed));
    let message = text(&removed);
    assert!(message.contains("builtin"), "{message}");
    assert!(message.contains("override"), "{message}");
}

/// An installed copy of a builtin's name is an override: it is removable, and
/// the builtin resolves again afterwards.
#[tokio::test]
async fn node_remove_deletes_an_installed_override() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let stub = StubCatalogue::start(std::collections::HashMap::new());
    let (_home, _guard, nodes_dir, _prompts_dir) = node_home(&stub.base_url());
    let document = nodes::builtin("implementer").unwrap();
    nodes::write(&nodes_dir, &document).unwrap();

    let removed = remove_node(&handler, "implementer").await;
    assert!(!is_err(&removed), "{}", text(&removed));
    let report: serde_json::Value = serde_json::from_str(&raw_text(&removed)).unwrap();
    assert_eq!(report["name"], "implementer");
    assert_eq!(report["removed"], true);
    assert!(!nodes_dir.join("implementer.json").exists());
    let resolved = nodes::resolve(&nodes_dir, "implementer").unwrap().unwrap();
    assert_eq!(resolved.source, nodes::Source::Builtin);
}

/// `graph_add_node` instantiates a node document by name and shallow-merges
/// `config_overrides` on top: the document's prompt survives, the caller's
/// harness lands in the stored node.
#[tokio::test]
async fn graph_add_node_from_a_node_document_carries_the_overrides() {
    let (dir, db, handler) = endpoint_test_handler();
    let stub = StubCatalogue::start(std::collections::HashMap::new());
    let (_home, _guard, nodes_dir, _prompts_dir) = node_home(&stub.base_url());
    let document = nodes::parse_document(&auditor_doc().to_string()).unwrap();
    nodes::write(&nodes_dir, &document).unwrap();
    let workdir = dir.path().to_string_lossy().to_string();

    let created = handler
        .graph_create(Parameters(GraphCreateParams {
            name: "Node Document Graph".to_string(),
            description: None,
            workdir,
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }))
        .await
        .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let added = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "audit".to_string(),
            kind: None,
            config: None,
            blueprint: None,
            node: Some("auditor".to_string()),
            config_overrides: Some(overrides(&[("platform", "claude"), ("model", "opus")])),
        }))
        .await
        .unwrap();
    assert!(!is_err(&added), "{}", text(&added));

    let stored = db.list_graph_nodes_for_graph(&graph_id).unwrap();
    let node = stored
        .iter()
        .find(|node| node.name == "audit")
        .expect("node stored");
    assert_eq!(node.kind, crate::domain::graphs::GraphNodeKind::Agent);
    assert_eq!(node.config["prompt_template"], "audit this");
    assert_eq!(node.config["platform"], "claude");
    assert_eq!(node.config["model"], "opus");
    assert_eq!(node.config["timeout_minutes"], 10);
}

/// An agent node document carries no harness, so instantiating one without an
/// override is refused naming the document — never a silent default.
#[tokio::test]
async fn graph_add_node_from_a_node_document_without_harness_is_refused() {
    let (dir, db, handler) = endpoint_test_handler();
    let stub = StubCatalogue::start(std::collections::HashMap::new());
    let (_home, _guard, nodes_dir, _prompts_dir) = node_home(&stub.base_url());
    let document = nodes::parse_document(&auditor_doc().to_string()).unwrap();
    nodes::write(&nodes_dir, &document).unwrap();
    let workdir = dir.path().to_string_lossy().to_string();

    let created = handler
        .graph_create(Parameters(GraphCreateParams {
            name: "No Harness".to_string(),
            description: None,
            workdir,
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }))
        .await
        .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let added = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id),
            name: "audit".to_string(),
            kind: None,
            config: None,
            blueprint: None,
            node: Some("auditor".to_string()),
            config_overrides: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&added), "{}", text(&added));
    let message = text(&added);
    assert!(message.contains("auditor"), "{message}");
    assert!(message.contains("platform"), "{message}");
    assert!(db.list_all_graph_nodes().unwrap().is_empty());
}

/// `graph_add_ensemble` instantiates `ensemble-proposers`: the document
/// supplies the shared prompt and member count, the caller supplies each
/// member's harness.
#[tokio::test]
async fn graph_add_ensemble_from_ensemble_proposers() {
    let (dir, db, handler) = endpoint_test_handler();
    let stub = StubCatalogue::start(std::collections::HashMap::new());
    let (_home, _guard, _nodes_dir, _prompts_dir) = node_home(&stub.base_url());
    let workdir = dir.path().to_string_lossy().to_string();

    let created = handler
        .graph_create(Parameters(GraphCreateParams {
            name: "Proposers Graph".to_string(),
            description: None,
            workdir,
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }))
        .await
        .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let kickoff = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "kickoff".to_string(),
            kind: Some("check".to_string()),
            config: Some(check_node_config("true")),
            blueprint: None,
            node: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let kickoff_id = extract_id(&kickoff, "node_id");
    let downstream = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "downstream".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            node: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let downstream_id = extract_id(&downstream, "node_id");

    let members = vec![
        openrouter_member("model-a"),
        openrouter_member("model-b"),
        openrouter_member("model-c"),
    ];
    let ensemble = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            kind: None,
            name: "Proposers".to_string(),
            prompt_template: None,
            blueprint: None,
            node: Some("ensemble-proposers".to_string()),
            members: Some(members.clone()),
            condition: "always".to_string(),
            from_node: kickoff_id,
            on_pass_to: downstream_id,
            on_fail_to: None,
            min_pass: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&ensemble), "{}", text(&ensemble));

    let ensembles = db.list_ensembles_for_graph(&graph_id).unwrap();
    assert_eq!(ensembles.len(), 1);
    let stored = &ensembles[0].ensemble;
    assert_eq!(ensembles[0].members.len(), 3);
    assert_eq!(stored.min_pass, 3, "the document's default is every member");
    assert_eq!(ensembles[0].members[0].platform, "openrouter");
    assert_eq!(
        ensembles[0].members[0].model.as_deref(),
        Some("model-a"),
        "the caller's member harness reaches the stored member"
    );
    assert!(
        stored.prompt_template.contains("Draft a complete solution"),
        "{}",
        stored.prompt_template
    );

    // The document carries no harnesses: omitting `members` is refused,
    // naming the document instead of inventing a platform.
    let missing = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: None,
            graph_id: Some(graph_id),
            kind: None,
            name: "No Harness".to_string(),
            prompt_template: None,
            blueprint: None,
            node: Some("ensemble-proposers".to_string()),
            members: None,
            condition: "always".to_string(),
            from_node: extract_id(&kickoff, "node_id"),
            on_pass_to: extract_id(&downstream, "node_id"),
            on_fail_to: None,
            min_pass: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            crew: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&missing), "{}", text(&missing));
    let message = text(&missing);
    assert!(message.contains("ensemble-proposers"), "{message}");
    assert!(message.contains("platform"), "{message}");
}
