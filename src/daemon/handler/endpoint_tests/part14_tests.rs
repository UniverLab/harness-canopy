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
    accept: Option<bool>,
) -> rmcp::model::CallToolResult {
    handler
        .graph_install(Parameters(GraphInstallParams {
            reference: reference.to_string(),
            workdir: workdir.to_string(),
            name,
            overwrite_presets,
            accept,
        }))
        .await
        .unwrap()
}

/// An install that already carries the user's acceptance — what every
/// "actually install it" test passes.
async fn install_accepted(
    handler: &TaskTriggerHandler,
    reference: &str,
    workdir: &str,
    name: Option<String>,
    overwrite_presets: Option<bool>,
) -> rmcp::model::CallToolResult {
    install(
        handler,
        reference,
        workdir,
        name,
        overwrite_presets,
        Some(true),
    )
    .await
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

    let installed = install_accepted(&handler, "my-graph", &workdir, None, None).await;
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

    let installed =
        install_accepted(&handler, "acme/widgets/my-graph@v1.2", &workdir, None, None).await;
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

    let installed = install_accepted(
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

    let installed = install_accepted(&handler, "my-graph", &workdir, None, None).await;
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

    let installed = install_accepted(&handler, "my-graph", "relative/path", None, None).await;
    assert!(is_err(&installed), "{}", text(&installed));
    assert!(
        text(&installed).contains("absolute"),
        "{}",
        text(&installed)
    );
    assert!(stub.hits().is_empty(), "refused before any fetch");
}

/// A call without `accept: true` installs nothing: the review comes back,
/// the database stays empty, and the prompts dir is untouched.
#[tokio::test]
async fn install_without_accept_returns_the_review_and_writes_nothing() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let mut doc = install_doc();
    doc["nodes"][0]["config"]["prompt_preset"] = serde_json::json!("custom");
    doc["prompt_presets"] = serde_json::json!([
        {"name": "custom", "content": "bundled content"},
    ]);
    doc["hooks"] = serde_json::json!({
        "on_completed": [{"command": "sudo systemctl restart worker"}],
    });
    let stub = StubCatalogue::start(
        [(
            "/i/graph/my-graph".to_string(),
            StubRoute::ok(&doc.to_string()),
        )]
        .into_iter()
        .collect(),
    );
    let (_home, _guard, prompts) = install_home(&stub.base_url());

    let pending = install(&handler, "my-graph", &workdir, None, None, None).await;
    assert!(!is_err(&pending), "{}", text(&pending));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&pending)).unwrap();
    assert_eq!(body["needs_review"], true);
    let review = &body["review"];
    assert_eq!(review["source"], "official");
    let commands = review["commands"].as_array().expect("commands list");
    assert_eq!(commands.len(), 2, "{commands:?}");
    assert_eq!(commands[0]["label"], "node \"finish\"");
    assert_eq!(commands[1]["label"], "hook 'on_completed' #1");
    assert_eq!(commands[1]["command"], "sudo systemctl restart worker");
    let prompts_list = review["prompts"].as_array().expect("prompts list");
    assert!(
        prompts_list
            .iter()
            .any(|prompt| prompt["label"] == "preset \"custom\""),
        "{prompts_list:?}"
    );
    let attention = review["attention"].as_array().expect("attention list");
    assert!(
        attention.iter().any(|mark| mark["pattern"] == "sudo"),
        "{attention:?}"
    );

    assert!(
        db.list_graphs(None, true).unwrap().is_empty(),
        "a call without accept imports nothing"
    );
    assert!(
        !prompts.join("custom.md").exists(),
        "a call without accept writes no preset"
    );
    let hits = stub.hits();
    assert_eq!(hits.len(), 1, "one fetch builds the review");
}

/// With `accept: true` the same review comes back inside the install report.
#[tokio::test]
async fn install_with_accept_echoes_the_review_in_the_report() {
    let (dir, db, handler) = endpoint_test_handler();
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

    let installed = install_accepted(&handler, "my-graph", &workdir, None, None).await;
    assert!(!is_err(&installed), "{}", text(&installed));
    let report: serde_json::Value = serde_json::from_str(&raw_text(&installed)).unwrap();
    let review = &report["review"];
    assert_eq!(review["source"], "official");
    assert_eq!(review["name"], "shared");
    assert!(
        review["commands"]
            .as_array()
            .expect("commands")
            .iter()
            .any(|command| command["label"] == "node \"finish\""),
        "{review}"
    );
    assert!(!db.list_graphs(None, true).unwrap().is_empty());
}

/// A 410 block report from the catalogue aborts with the reason and the
/// issue, and nothing is written.
#[tokio::test]
async fn install_reports_a_410_block_with_its_reason_and_writes_nothing() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let stub = StubCatalogue::start(
        [(
            "/i/graph/my-graph".to_string(),
            StubRoute {
                status: 410,
                headers: Vec::new(),
                body: br#"{"error":"blocked","reason":"exfiltrates tokens","issue":"CAT-42"}"#
                    .to_vec(),
            },
        )]
        .into_iter()
        .collect(),
    );
    let (_home, _guard, _prompts) = install_home(&stub.base_url());

    let refused = install_accepted(&handler, "my-graph", &workdir, None, None).await;
    assert!(is_err(&refused), "{}", text(&refused));
    let message = text(&refused);
    assert!(
        message.contains(
            "my-graph was removed from the catalogue after a community report: \
             exfiltrates tokens (CAT-42)"
        ),
        "{message}"
    );
    assert!(
        db.list_graphs(None, true).unwrap().is_empty(),
        "a blocked document is never imported"
    );
    assert!(!stub.hits().is_empty());
}

/// A catalogue 404 says the graph was not found in the catalogue.
#[tokio::test]
async fn install_reports_a_catalogue_404_as_not_found() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let stub = StubCatalogue::start(std::collections::HashMap::new());
    let (_home, _guard, _prompts) = install_home(&stub.base_url());

    let installed = install_accepted(&handler, "missing", &workdir, None, None).await;
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

    let installed = install_accepted(&handler, "Not A Ref", &workdir, None, None).await;
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

    let installed = install_accepted(&handler, "my-graph", &workdir, None, None).await;
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

    let installed = install_accepted(&handler, "my-graph", &workdir, None, Some(true)).await;
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

/// A draft installs across the two calls with a single fetch: the review
/// call consumes it and parks the body, the accept call reuses it. The
/// review and the report both name the source as `draft`.
#[tokio::test]
async fn install_draft_reviews_then_installs_with_one_fetch() {
    const DRAFT_ID: &str = "graphdraftabcdefghijklmnop";
    let reference = format!("draft:{DRAFT_ID}");
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let stub = StubCatalogue::start(
        [(
            format!("/i/draft/{DRAFT_ID}"),
            StubRoute::ok(&install_doc().to_string()),
        )]
        .into_iter()
        .collect(),
    );
    let (_home, _guard, _prompts) = install_home(&stub.base_url());

    let pending = install(&handler, &reference, &workdir, None, None, None).await;
    assert!(!is_err(&pending), "{}", text(&pending));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&pending)).unwrap();
    assert_eq!(body["needs_review"], true);
    assert_eq!(body["review"]["source"], "draft");

    let installed = install_accepted(&handler, &reference, &workdir, None, None).await;
    assert!(!is_err(&installed), "{}", text(&installed));
    let report: serde_json::Value = serde_json::from_str(&raw_text(&installed)).unwrap();
    assert_eq!(report["name"], "shared");
    assert_eq!(report["source"], "draft");
    let graph_id = report["graph_id"].as_str().unwrap();
    assert!(db.get_graph(graph_id).unwrap().is_some(), "graph imported");

    let hits = stub.hits();
    assert_eq!(hits.len(), 1, "the accept call reuses the parked body");
    assert_eq!(hits[0].target, format!("/i/draft/{DRAFT_ID}"));
}

/// Accepting a draft with no parked body re-fetches, and the catalogue's
/// 404 says the draft is gone.
#[tokio::test]
async fn install_draft_accept_without_a_parked_body_reports_it_gone() {
    const DRAFT_ID: &str = "graphdraftmissingdocabcdef";
    let reference = format!("draft:{DRAFT_ID}");
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let stub = StubCatalogue::start(std::collections::HashMap::new());
    let (_home, _guard, _prompts) = install_home(&stub.base_url());

    let installed = install_accepted(&handler, &reference, &workdir, None, None).await;
    assert!(is_err(&installed), "{}", text(&installed));
    assert!(
        text(&installed).contains(
            "draft not found: drafts last 30 minutes and are deleted by their first install"
        ),
        "{}",
        text(&installed)
    );
}
