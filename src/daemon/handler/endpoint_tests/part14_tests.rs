//! Part 14 of `endpoint_tests`: `graph_install` end to end through the MCP
//! surface — a local HTTP stub stands in for the catalogue, and
//! `CANOPY_HOME_OVERRIDE` points the config (`[graphs] catalogue_url`) and
//! the prompts dir at a temp home.

use super::*;
use crate::daemon::graph_install::stub::{StubCatalogue, StubRoute};

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

fn install_doc() -> serde_json::Value {
    serde_json::json!({
        "format_version": 6,
        "name": "shared",
        "nodes": [
            {"name": "start", "kind": "agent", "position": 1,
             "config": {"prompt_template": "do it"}},
            {"name": "finish", "kind": "check", "position": 2,
             "config": {"command": "true"}},
        ],
        "edges": [
            {"from_node": "start", "to_node": "finish", "condition": "always"},
        ],
        "ensembles": [],
    })
}

/// A temp home whose `config.toml` points the catalogue at `catalogue_url`,
/// with an empty prompts dir. Returns the home (kept alive by the caller)
/// and the prompts dir.
fn install_home(catalogue_url: &str) -> (tempfile::TempDir, OverrideHome, std::path::PathBuf) {
    let home = tempdir().unwrap();
    let canopy = home.path().join(".canopy");
    let prompts = canopy.join("prompts");
    std::fs::create_dir_all(&prompts).unwrap();
    std::fs::write(
        canopy.join("config.toml"),
        format!("[graphs]\ncatalogue_url = \"{catalogue_url}\"\n"),
    )
    .unwrap();
    let guard = OverrideHome::set(home.path());
    (home, guard, prompts)
}

async fn install(
    handler: &TaskTriggerHandler,
    reference: &str,
    workdir: &str,
    name: Option<String>,
    overwrite_presets: Option<bool>,
) -> rmcp::model::CallToolResult {
    handler
        .graph_install(Parameters(GraphInstallParams {
            reference: reference.to_string(),
            workdir: workdir.to_string(),
            name,
            overwrite_presets,
        }))
        .await
        .unwrap()
}

/// `graph_install` follows the stub's redirect to the document, imports it
/// as a new graph, and reports the agent node still needing a harness with
/// the next command to assign it.
#[tokio::test]
async fn install_imports_through_the_catalogue_and_lists_nodes_without_harness() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let stub = StubCatalogue::start(
        [
            (
                "/i/graph/my-graph".to_string(),
                StubRoute::redirect_to("/doc"),
            ),
            (
                "/doc".to_string(),
                StubRoute::ok(&install_doc().to_string()),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let (_home, _guard, _prompts) = install_home(&stub.base_url());

    let installed = install(&handler, "my-graph", &workdir, None, None).await;
    assert!(!is_err(&installed), "{}", text(&installed));
    let report: serde_json::Value = serde_json::from_str(&raw_text(&installed)).unwrap();
    assert_eq!(report["name"], "shared");
    assert_eq!(report["workdir"], workdir);
    assert_eq!(report["ref"], "my-graph");
    assert_eq!(
        report["presets_written"],
        serde_json::json!([]),
        "no bundled presets, nothing written"
    );
    assert_eq!(
        report["nodes_missing_platform"],
        serde_json::json!(["start"])
    );
    let next = report["next"].as_str().expect("next step");
    assert!(next.contains("start"), "{next}");
    assert!(next.contains("graph_update_node"), "{next}");
    assert!(
        next.contains(report["graph_id"].as_str().unwrap()),
        "{next}"
    );

    let graph_id = report["graph_id"].as_str().unwrap().to_string();
    let stored = db.get_graph(&graph_id).unwrap().expect("graph stored");
    assert_eq!(stored.name, "shared");
    assert_eq!(stored.workdir, workdir);
}

/// A community ref with a version fetches the scoped install-counting path
/// with the version as the `ref` query parameter, then imports as usual.
#[tokio::test]
async fn install_supports_a_community_ref_with_a_version() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let stub = StubCatalogue::start(
        [(
            "/i/acme/widgets/graph/my-graph".to_string(),
            StubRoute::ok(&install_doc().to_string()),
        )]
        .into_iter()
        .collect(),
    );
    let (_home, _guard, _prompts) = install_home(&stub.base_url());

    let installed = install(&handler, "acme/widgets/my-graph@v1.2", &workdir, None, None).await;
    assert!(!is_err(&installed), "{}", text(&installed));
    let report: serde_json::Value = serde_json::from_str(&raw_text(&installed)).unwrap();
    assert_eq!(report["name"], "shared");
    assert_eq!(report["ref"], "acme/widgets/my-graph@v1.2");
    let hits = stub.hits();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].target, "/i/acme/widgets/graph/my-graph?ref=v1.2");
}

/// `--name` overrides the document's name, exactly as `graph_import`.
#[tokio::test]
async fn install_honors_the_name_override() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let stub = StubCatalogue::start(
        [(
            "/i/graph/my-graph".to_string(),
            StubRoute::ok(&install_doc().to_string()),
        )]
        .into_iter()
        .collect(),
    );
    let (_home, _guard, _prompts) = install_home(&stub.base_url());

    let installed = install(
        &handler,
        "my-graph",
        &workdir,
        Some("renamed".to_string()),
        None,
    )
    .await;
    assert!(!is_err(&installed), "{}", text(&installed));
    let report: serde_json::Value = serde_json::from_str(&raw_text(&installed)).unwrap();
    assert_eq!(report["name"], "renamed");
}

/// A preset conflict aborts the install: no graph is created and no preset
/// file is written.
#[tokio::test]
async fn install_preset_conflict_creates_no_graph_and_writes_nothing() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let mut doc = install_doc();
    doc["nodes"][0]["config"]["prompt_preset"] = serde_json::json!("custom");
    doc["prompt_presets"] = serde_json::json!([
        {"name": "aaa", "content": "aaa content"},
        {"name": "custom", "content": "new content"},
    ]);
    let stub = StubCatalogue::start(
        [(
            "/i/graph/my-graph".to_string(),
            StubRoute::ok(&doc.to_string()),
        )]
        .into_iter()
        .collect(),
    );
    let (_home, _guard, prompts) = install_home(&stub.base_url());
    std::fs::write(prompts.join("custom.md"), "old content").unwrap();

    let installed = install(&handler, "my-graph", &workdir, None, None).await;
    assert!(is_err(&installed), "{}", text(&installed));
    assert!(
        text(&installed).contains("different content"),
        "{}",
        text(&installed)
    );
    assert!(
        db.list_graphs(None, true).unwrap().is_empty(),
        "a conflict aborts before anything is written"
    );
    assert!(!prompts.join("aaa.md").exists());
    assert_eq!(
        std::fs::read_to_string(prompts.join("custom.md")).unwrap(),
        "old content"
    );
}

/// A relative workdir is refused before anything is fetched.
#[tokio::test]
async fn install_refuses_a_relative_workdir_without_fetching() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let stub = StubCatalogue::start(std::collections::HashMap::new());
    let (_home, _guard, _prompts) = install_home(&stub.base_url());

    let installed = install(&handler, "my-graph", "relative/path", None, None).await;
    assert!(is_err(&installed), "{}", text(&installed));
    assert!(
        text(&installed).contains("absolute"),
        "{}",
        text(&installed)
    );
    assert!(stub.hits().is_empty(), "refused before any fetch");
}

/// A catalogue 404 says the graph was not found in the catalogue.
#[tokio::test]
async fn install_reports_a_catalogue_404_as_not_found() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let stub = StubCatalogue::start(std::collections::HashMap::new());
    let (_home, _guard, _prompts) = install_home(&stub.base_url());

    let installed = install(&handler, "missing", &workdir, None, None).await;
    assert!(is_err(&installed), "{}", text(&installed));
    assert!(
        text(&installed).contains("not found in the catalogue"),
        "{}",
        text(&installed)
    );
}

/// An invalid ref is refused before any fetch, with the accepted forms in the
/// message.
#[tokio::test]
async fn install_refuses_an_invalid_ref_with_the_accepted_forms() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let stub = StubCatalogue::start(std::collections::HashMap::new());
    let (_home, _guard, _prompts) = install_home(&stub.base_url());

    let installed = install(&handler, "Not A Ref", &workdir, None, None).await;
    assert!(is_err(&installed), "{}", text(&installed));
    assert!(
        text(&installed).contains("Accepted forms"),
        "{}",
        text(&installed)
    );
    assert!(stub.hits().is_empty(), "refused before any fetch");
}

/// A fetched document that fails validation is refused with every problem
/// listed, and no graph is created.
#[tokio::test]
async fn install_refuses_an_invalid_document_and_creates_no_graph() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let mut doc = install_doc();
    doc["nodes"][0]["config"]["bogus"] = serde_json::json!(1);
    doc["nodes"][1]["config"]["bogus"] = serde_json::json!(2);
    let stub = StubCatalogue::start(
        [(
            "/i/graph/my-graph".to_string(),
            StubRoute::ok(&doc.to_string()),
        )]
        .into_iter()
        .collect(),
    );
    let (_home, _guard, _prompts) = install_home(&stub.base_url());

    let installed = install(&handler, "my-graph", &workdir, None, None).await;
    assert!(is_err(&installed), "{}", text(&installed));
    let message = text(&installed);
    assert!(message.contains("nodes[0].config"), "{message}");
    assert!(message.contains("nodes[1].config"), "{message}");
    assert!(
        db.list_graphs(None, true).unwrap().is_empty(),
        "an invalid document aborts before anything is written"
    );
}

/// `--overwrite-presets` replaces a differing local preset file and reports it
/// as written, then imports the graph.
#[tokio::test]
async fn install_overwrite_presets_replaces_a_conflicting_file() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let mut doc = install_doc();
    doc["nodes"][0]["config"]["prompt_preset"] = serde_json::json!("custom");
    doc["prompt_presets"] = serde_json::json!([
        {"name": "custom", "content": "new content"},
    ]);
    let stub = StubCatalogue::start(
        [(
            "/i/graph/my-graph".to_string(),
            StubRoute::ok(&doc.to_string()),
        )]
        .into_iter()
        .collect(),
    );
    let (_home, _guard, prompts) = install_home(&stub.base_url());
    std::fs::write(prompts.join("custom.md"), "old content").unwrap();

    let installed = install(&handler, "my-graph", &workdir, None, Some(true)).await;
    assert!(!is_err(&installed), "{}", text(&installed));
    let report: serde_json::Value = serde_json::from_str(&raw_text(&installed)).unwrap();
    assert_eq!(
        report["presets_written"],
        serde_json::json!(["custom"]),
        "the replaced preset is reported"
    );
    assert_eq!(
        std::fs::read_to_string(prompts.join("custom.md")).unwrap(),
        "new content"
    );
    let graph_id = report["graph_id"].as_str().unwrap();
    assert!(db.get_graph(graph_id).unwrap().is_some(), "graph imported");
}
