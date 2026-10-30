//! Part 15 of `endpoint_tests`: shareable node documents end to end through
//! the MCP surface — `node_install`/`node_list`/`node_remove` against a local
//! HTTP stub standing in for the catalogue, and `graph_node` /
//! `graph_ensemble` instantiating a node document by name.

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
    accept: Option<bool>,
) -> rmcp::model::CallToolResult {
    install_node_with_as(handler, reference, None, None, overwrite_presets, accept).await
}

async fn install_node_with_as(
    handler: &TaskTriggerHandler,
    reference: &str,
    as_name: Option<String>,
    replace: Option<bool>,
    overwrite_presets: Option<bool>,
    accept: Option<bool>,
) -> rmcp::model::CallToolResult {
    call_resource_action(
        handler,
        "node",
        "install",
        params_id(NodeInstallParams {
            reference: reference.to_string(),
            as_name,
            replace,
            overwrite_presets,
            accept,
        }),
    )
    .await
    .unwrap()
}

/// An install that already carries the user's acceptance — what every
/// "actually install it" test passes.
async fn install_node_accepted(
    handler: &TaskTriggerHandler,
    reference: &str,
    overwrite_presets: Option<bool>,
) -> rmcp::model::CallToolResult {
    install_node(handler, reference, overwrite_presets, Some(true)).await
}

async fn remove_node(handler: &TaskTriggerHandler, name: &str) -> rmcp::model::CallToolResult {
    call_resource_action(
        handler,
        "node",
        "remove",
        params_id(NodeRemoveParams {
            name: name.to_string(),
        }),
    )
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

/// A call without `accept: true` installs nothing: the review comes back,
/// the nodes dir and the prompts dir are untouched.
#[tokio::test]
async fn node_install_without_accept_returns_the_review_and_writes_nothing() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let doc = serde_json::json!({
        "format_version": 1,
        "name": "checker",
        "kind": "check",
        "config": {"command": "chmod 777 /srv/app", "timeout_seconds": 60},
    });
    let stub = stub_serving("/i/node/checker", &doc.to_string());
    let (_home, _guard, nodes_dir, prompts_dir) = node_home(&stub.base_url());

    let pending = install_node(&handler, "checker", None, None).await;
    assert!(!is_err(&pending), "{}", text(&pending));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&pending)).unwrap();
    assert_eq!(body["needs_review"], true);
    let review = &body["review"];
    assert_eq!(review["source"], "official");
    let commands = review["commands"].as_array().expect("commands list");
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert_eq!(commands[0]["label"], "node \"checker\"");
    assert_eq!(commands[0]["command"], "chmod 777 /srv/app");
    let attention = review["attention"].as_array().expect("attention list");
    assert!(
        attention.iter().any(|mark| mark["pattern"] == "chmod 777"),
        "{attention:?}"
    );

    assert!(
        !nodes_dir.join("checker.json").exists(),
        "a call without accept writes no node document"
    );
    assert!(
        !prompts_dir.join("auditor-preset.md").exists(),
        "a call without accept writes no preset"
    );
}

/// With `accept: true` the same review comes back inside the report, and the
/// document and its preset land on disk.
#[tokio::test]
async fn node_install_with_accept_echoes_the_review_and_writes() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let stub = stub_serving("/i/node/auditor", &auditor_doc().to_string());
    let (_home, _guard, nodes_dir, prompts_dir) = node_home(&stub.base_url());

    let installed = install_node_accepted(&handler, "auditor", None).await;
    assert!(!is_err(&installed), "{}", text(&installed));
    let report: serde_json::Value = serde_json::from_str(&raw_text(&installed)).unwrap();
    let review = &report["review"];
    assert_eq!(review["source"], "official");
    assert_eq!(review["name"], "auditor");
    assert!(
        review["prompts"]
            .as_array()
            .expect("prompts")
            .iter()
            .any(|prompt| prompt["label"] == "node \"auditor\""),
        "{review}"
    );
    assert!(nodes_dir.join("auditor.json").exists());
    assert!(prompts_dir.join("auditor-preset.md").exists());
}

/// A 410 block report from the catalogue aborts with the reason and the
/// issue, and nothing is written.
#[tokio::test]
async fn node_install_reports_a_410_block_with_its_reason_and_writes_nothing() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let stub = StubCatalogue::start(
        [(
            "/i/node/auditor".to_string(),
            StubRoute {
                status: 410,
                headers: Vec::new(),
                body: br#"{"error":"blocked","reason":"ships a backdoor","issue":"CAT-7"}"#
                    .to_vec(),
            },
        )]
        .into_iter()
        .collect(),
    );
    let (_home, _guard, nodes_dir, _prompts_dir) = node_home(&stub.base_url());

    let refused = install_node_accepted(&handler, "auditor", None).await;
    assert!(is_err(&refused), "{}", text(&refused));
    let message = text(&refused);
    assert!(
        message.contains(
            "auditor was removed from the catalogue after a community report: \
             ships a backdoor (CAT-7)"
        ),
        "{message}"
    );
    assert!(!nodes_dir.join("auditor.json").exists());
}

/// `node_install` writes the document under ~/.canopy/nodes and its bundled
/// preset under ~/.canopy/prompts, and `node_list` reports it as installed.
#[tokio::test]
async fn node_install_writes_the_document_and_its_preset() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let stub = stub_serving("/i/node/auditor", &auditor_doc().to_string());
    let (_home, _guard, nodes_dir, prompts_dir) = node_home(&stub.base_url());

    let installed = install_node_accepted(&handler, "auditor", None).await;
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

    let listed = call_resource_action(&handler, "node", "list", ())
        .await
        .unwrap();
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

    let installed = install_node_accepted(&handler, "auditor", None).await;
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

/// `graph_node` instantiates a node document by name and shallow-merges
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

    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Node Document Graph".to_string(),
            description: None,
            workdir,
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let added = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "audit".to_string(),
            kind: None,
            config: None,
            node: Some("auditor".to_string()),
            config_overrides: Some(overrides(&[("platform", "claude"), ("model", "opus")])),
        }),
    )
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

    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "No Harness".to_string(),
            description: None,
            workdir,
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let added = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id),
            name: "audit".to_string(),
            kind: None,
            config: None,
            node: Some("auditor".to_string()),
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&added), "{}", text(&added));
    let message = text(&added);
    assert!(message.contains("auditor"), "{message}");
    assert!(message.contains("platform"), "{message}");
    assert!(db.list_all_graph_nodes().unwrap().is_empty());
}

/// `graph_ensemble` instantiates `ensemble-proposers`: the document
/// supplies the shared prompt and member count, the caller supplies each
/// member's harness.
#[tokio::test]
async fn graph_add_ensemble_from_ensemble_proposers() {
    let (dir, db, handler) = endpoint_test_handler();
    let stub = StubCatalogue::start(std::collections::HashMap::new());
    let (_home, _guard, _nodes_dir, _prompts_dir) = node_home(&stub.base_url());
    let workdir = dir.path().to_string_lossy().to_string();

    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Proposers Graph".to_string(),
            description: None,
            workdir,
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let kickoff = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "kickoff".to_string(),
            kind: Some("check".to_string()),
            config: Some(check_node_config("true")),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let kickoff_id = extract_id(&kickoff, "node_id");
    let downstream = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "downstream".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let downstream_id = extract_id(&downstream, "node_id");

    let members = vec![
        openrouter_member("model-a"),
        openrouter_member("model-b"),
        openrouter_member("model-c"),
    ];
    let ensemble = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            kind: None,
            name: "Proposers".to_string(),
            prompt_template: None,
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
        }),
    )
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
    let missing = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: None,
            graph_id: Some(graph_id),
            kind: None,
            name: "No Harness".to_string(),
            prompt_template: None,
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
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&missing), "{}", text(&missing));
    let message = text(&missing);
    assert!(message.contains("ensemble-proposers"), "{message}");
    assert!(message.contains("platform"), "{message}");
}

/// A draft installs across the two calls with a single fetch: the review
/// call consumes it and parks the body, the accept call reuses it. The
/// review and the report both name the source as `draft`.
#[tokio::test]
async fn node_install_draft_reviews_then_installs_with_one_fetch() {
    const DRAFT_ID: &str = "nodedraftabcdefghijklmnopq";
    let reference = format!("draft:{DRAFT_ID}");
    let (_dir, _db, handler) = endpoint_test_handler();
    let stub = stub_serving(&format!("/i/draft/{DRAFT_ID}"), &auditor_doc().to_string());
    let (_home, _guard, nodes_dir, prompts_dir) = node_home(&stub.base_url());

    let pending = install_node(&handler, &reference, None, None).await;
    assert!(!is_err(&pending), "{}", text(&pending));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&pending)).unwrap();
    assert_eq!(body["needs_review"], true);
    assert_eq!(body["review"]["source"], "draft");

    let installed = install_node_accepted(&handler, &reference, None).await;
    assert!(!is_err(&installed), "{}", text(&installed));
    let report: serde_json::Value = serde_json::from_str(&raw_text(&installed)).unwrap();
    assert_eq!(report["name"], "auditor");
    assert_eq!(report["source"], "draft");
    assert!(nodes_dir.join("auditor.json").exists());
    assert!(prompts_dir.join("auditor-preset.md").exists());

    let hits = stub.hits();
    assert_eq!(hits.len(), 1, "the accept call reuses the parked body");
    assert_eq!(hits[0].target, format!("/i/draft/{DRAFT_ID}"));
}

/// Accepting a draft with no parked body re-fetches, and the catalogue's
/// 404 says the draft is gone.
#[tokio::test]
async fn node_install_draft_accept_without_a_parked_body_reports_it_gone() {
    const DRAFT_ID: &str = "nodedraftmissingdocabcdefg";
    let reference = format!("draft:{DRAFT_ID}");
    let (_dir, _db, handler) = endpoint_test_handler();
    let stub = StubCatalogue::start(std::collections::HashMap::new());
    let (_home, _guard, nodes_dir, _prompts_dir) = node_home(&stub.base_url());

    let installed = install_node_accepted(&handler, &reference, None).await;
    assert!(is_err(&installed), "{}", text(&installed));
    assert!(
        text(&installed).contains(
            "draft not found: drafts last 30 minutes and are deleted by their first install"
        ),
        "{}",
        text(&installed)
    );
    assert!(!nodes_dir.join("auditor.json").exists());
}

fn community_auditor_doc() -> serde_json::Value {
    serde_json::json!({
        "format_version": 1,
        "name": "auditor",
        "kind": "agent",
        "description": "Alice's audit node.",
        "config": {"prompt_template": "alice audits", "timeout_minutes": 10},
        "preset": {"name": "auditor-preset", "content": "alice audits well"},
    })
}

fn stub_serving_official_and_community(official: &str, community: &str) -> StubCatalogue {
    StubCatalogue::start(
        [
            ("/i/node/auditor".to_string(), StubRoute::ok(official)),
            (
                "/i/alice/lab/node/auditor".to_string(),
                StubRoute::ok(community),
            ),
        ]
        .into_iter()
        .collect(),
    )
}

fn node_list_entry(body: &serde_json::Value, name: &str) -> serde_json::Value {
    body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("{name} is listed: {body}"))
        .clone()
}

/// Installing the official `auditor` and then Alice's `auditor` refuses the
/// second install, naming both sources and the way out — the first copy
/// (and its preset) is untouched, and the list shows which source owns the
/// name.
#[tokio::test]
async fn node_install_from_another_source_refuses_and_keeps_the_first() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let stub = stub_serving_official_and_community(
        &auditor_doc().to_string(),
        &community_auditor_doc().to_string(),
    );
    let (_home, _guard, nodes_dir, prompts_dir) = node_home(&stub.base_url());

    let installed = install_node_accepted(&handler, "auditor", None).await;
    assert!(!is_err(&installed), "{}", text(&installed));

    let refused = install_node_accepted(&handler, "alice/lab/auditor", None).await;
    assert!(is_err(&refused), "{}", text(&refused));
    let message = text(&refused);
    assert!(message.contains("'auditor'"), "{message}");
    assert!(message.contains("'alice/lab/auditor'"), "{message}");
    assert!(message.contains("--as"), "{message}");
    assert!(message.contains("--replace"), "{message}");

    let stored =
        nodes::parse_document(&std::fs::read_to_string(nodes_dir.join("auditor.json")).unwrap())
            .unwrap();
    assert_eq!(stored.config["prompt_template"], "audit this");
    assert_eq!(stored.catalogue_source.as_deref(), Some("auditor"));
    assert_eq!(
        std::fs::read_to_string(prompts_dir.join("auditor-preset.md")).unwrap(),
        "audit well"
    );

    let listed = call_resource_action(&handler, "node", "list", ())
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&raw_text(&listed)).unwrap();
    let entry = node_list_entry(&body, "auditor");
    assert_eq!(entry["source"], "installed");
    assert_eq!(entry["catalogue_source"], "auditor");
}

/// `as` installs both copies side by side — the community copy under
/// another local name with its preset renamed with it — and `graph_node
/// add` resolves each name to its own document.
#[tokio::test]
async fn node_install_as_installs_both_and_each_resolves_in_a_graph() {
    let (dir, db, handler) = endpoint_test_handler();
    let stub = stub_serving_official_and_community(
        &auditor_doc().to_string(),
        &community_auditor_doc().to_string(),
    );
    let (_home, _guard, nodes_dir, prompts_dir) = node_home(&stub.base_url());

    let installed = install_node_accepted(&handler, "auditor", None).await;
    assert!(!is_err(&installed), "{}", text(&installed));
    let installed = install_node_with_as(
        &handler,
        "alice/lab/auditor",
        Some("alice-auditor".to_string()),
        None,
        None,
        Some(true),
    )
    .await;
    assert!(!is_err(&installed), "{}", text(&installed));
    let report: serde_json::Value = serde_json::from_str(&raw_text(&installed)).unwrap();
    assert_eq!(report["name"], "alice-auditor");

    let stored = nodes::parse_document(
        &std::fs::read_to_string(nodes_dir.join("alice-auditor.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(stored.config["prompt_template"], "alice audits");
    assert_eq!(
        stored.catalogue_source.as_deref(),
        Some("alice/lab/auditor")
    );
    assert_eq!(
        std::fs::read_to_string(prompts_dir.join("alice-auditor-preset.md")).unwrap(),
        "alice audits well"
    );
    assert_eq!(
        std::fs::read_to_string(prompts_dir.join("auditor-preset.md")).unwrap(),
        "audit well",
        "the official preset is untouched"
    );

    let listed = call_resource_action(&handler, "node", "list", ())
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&raw_text(&listed)).unwrap();
    assert_eq!(
        node_list_entry(&body, "auditor")["catalogue_source"],
        "auditor"
    );
    assert_eq!(
        node_list_entry(&body, "alice-auditor")["catalogue_source"],
        "alice/lab/auditor"
    );

    let workdir = dir.path().to_string_lossy().to_string();
    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Two Auditors".to_string(),
            description: None,
            workdir,
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");
    for (node_name, document) in [("audit", "auditor"), ("alice-audit", "alice-auditor")] {
        let added = call_graph_action(
            &handler,
            "graph_node",
            "add",
            params_id(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: node_name.to_string(),
                kind: None,
                config: None,
                node: Some(document.to_string()),
                config_overrides: Some(overrides(&[("platform", "claude")])),
            }),
        )
        .await
        .unwrap();
        assert!(!is_err(&added), "{document}: {}", text(&added));
    }
    let stored = db.list_graph_nodes_for_graph(&graph_id).unwrap();
    let official = stored
        .iter()
        .find(|node| node.name == "audit")
        .expect("official node stored");
    assert_eq!(official.config["prompt_template"], "audit this");
    let community = stored
        .iter()
        .find(|node| node.name == "alice-audit")
        .expect("community node stored");
    assert_eq!(community.config["prompt_template"], "alice audits");
}

/// Reinstalling from the same source updates the document and its preset in
/// place, with no overwrite flag.
#[tokio::test]
async fn node_reinstall_from_the_same_source_updates() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let stub_v1 = stub_serving("/i/node/auditor", &auditor_doc().to_string());
    let (home, _guard, nodes_dir, prompts_dir) = node_home(&stub_v1.base_url());

    let installed = install_node_accepted(&handler, "auditor", None).await;
    assert!(!is_err(&installed), "{}", text(&installed));

    let mut v2 = auditor_doc();
    v2["config"]["prompt_template"] = serde_json::json!("audit harder");
    v2["preset"] = serde_json::json!({"name": "auditor-preset", "content": "audit very well"});
    let stub_v2 = stub_serving("/i/node/auditor", &v2.to_string());
    std::fs::write(
        home.path().join(".canopy").join("config.toml"),
        format!("[graphs]\ncatalogue_url = \"{}\"\n", stub_v2.base_url()),
    )
    .unwrap();

    let reinstalled = install_node_accepted(&handler, "auditor", None).await;
    assert!(!is_err(&reinstalled), "{}", text(&reinstalled));
    let stored =
        nodes::parse_document(&std::fs::read_to_string(nodes_dir.join("auditor.json")).unwrap())
            .unwrap();
    assert_eq!(stored.config["prompt_template"], "audit harder");
    assert_eq!(stored.catalogue_source.as_deref(), Some("auditor"));
    assert_eq!(
        std::fs::read_to_string(prompts_dir.join("auditor-preset.md")).unwrap(),
        "audit very well"
    );
}
