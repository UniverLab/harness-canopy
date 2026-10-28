//! Install a shareable node document from the catalogue
//! (`canopy node install` / `node` `install`).
//!
//! The same one-GET path `graph install` uses — parse the ref, fetch the
//! document through the catalogue's install-counting endpoint, validate it
//! exactly as `canopy graph validate` would, then install its bundled preset
//! into the prompts dir (never silently over a differing file) before
//! writing `~/.canopy/nodes/<name>.json`. A document that validates as
//! anything other than a node is refused: installing a graph document as a
//! node would leave a file no authoring tool could use.

use std::path::Path;

use crate::domain::graph_transfer::GraphExportPromptPreset;
use crate::domain::nodes::NodeDocument;

/// A fetched and validated node document — nothing written yet. Its
/// `source` is the review's source label, matching the graph side.
#[derive(Debug)]
pub(crate) struct FetchedNode {
    pub document: NodeDocument,
    /// The catalogue URL the document body was finally read from (after any
    /// redirects).
    pub source_url: String,
    /// The source label for the install review.
    pub source: String,
    /// The raw validated body — a draft review parks it for the accept call,
    /// since the draft is consumed by the first fetch.
    pub body: String,
}

/// Fetch `reference` from the catalogue at `base` and validate it exactly as
/// `graph` would. Nothing is written: the caller builds the review
/// first, and only [`write_node_install`] (after the user accepted) touches
/// disk.
pub(crate) fn fetch_node_document(base: &str, reference: &str) -> Result<FetchedNode, String> {
    let parsed = crate::daemon::graph_install::parse_ref_node(reference)?;
    let fetched = crate::daemon::graph_install::fetch_document(base, &parsed, "node")?;
    assemble_node(&parsed, reference, &fetched.body, fetched.source_url)
}

/// Rebuild the fetch result from a draft body the review call already
/// consumed: the same validation and import-path parsing as
/// [`fetch_node_document`], without touching the network.
pub(crate) fn node_from_body(
    reference: &str,
    body: &str,
    source_url: String,
) -> Result<FetchedNode, String> {
    let parsed = crate::daemon::graph_install::parse_ref_node(reference)?;
    assemble_node(&parsed, reference, body, source_url)
}

/// Validate `body` exactly as `graph` would and parse it for the
/// import path — the tail [`fetch_node_document`] and [`node_from_body`]
/// share.
fn assemble_node(
    parsed: &crate::daemon::graph_install::GraphRef,
    reference: &str,
    body: &str,
    source_url: String,
) -> Result<FetchedNode, String> {
    let source = parsed.source_label();
    let report = crate::daemon::graph_validate::validate_document_str(body);
    reject_invalid_report(reference, &report)?;
    let document = crate::domain::nodes::parse_document(body)?;
    Ok(FetchedNode {
        document,
        source_url,
        source,
        body: body.to_string(),
    })
}

/// Install the document's preset into `prompts_dir` with the graph
/// installer's rules, then write the document to `nodes_dir` — and return
/// the preset names written (new or overwritten; a file already present
/// with identical content is not listed).
pub(crate) fn write_node_install(
    document: &NodeDocument,
    overwrite_presets: bool,
    nodes_dir: &Path,
    prompts_dir: &Path,
) -> Result<Vec<String>, String> {
    let presets_written = presets_to_write(document, prompts_dir);
    install_preset(document, overwrite_presets, prompts_dir)?;
    crate::domain::nodes::write(nodes_dir, document)?;
    Ok(presets_written)
}

/// Refuse a catalogue body that fails validation — listing every problem —
/// or that validates as something other than a node document.
fn reject_invalid_report(reference: &str, report: &serde_json::Value) -> Result<(), String> {
    if report.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let problems = report
            .get("problems")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut lines = vec![format!(
            "The catalogue document for '{reference}' failed validation ({} problem{}):",
            problems.len(),
            if problems.len() == 1 { "" } else { "s" },
        )];
        for problem in &problems {
            let path = problem.get("path").and_then(serde_json::Value::as_str);
            let message = problem.get("message").and_then(serde_json::Value::as_str);
            lines.push(format!(
                "{}: {}",
                path.unwrap_or("?"),
                message.unwrap_or("?")
            ));
        }
        return Err(lines.join("\n"));
    }
    if report.get("kind").and_then(serde_json::Value::as_str) != Some("node") {
        let name = report
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        return Err(format!(
            "The catalogue returned a graph document '{name}', not a node document; \
             `node` `install` only installs node documents."
        ));
    }
    Ok(())
}

/// The bundled preset whose file the install will write: `None` when the
/// document carries no preset, or when the local file already matches.
fn presets_to_write(document: &NodeDocument, prompts_dir: &Path) -> Vec<String> {
    let Some(preset) = &document.preset else {
        return Vec::new();
    };
    let path = prompts_dir.join(format!("{}.md", preset.name));
    match std::fs::read_to_string(&path) {
        Ok(existing) if existing == preset.content => Vec::new(),
        _ => vec![preset.name.clone()],
    }
}

/// Install the document's bundled preset with the graph installer's rules:
/// an unsafe name, a builtin's name, or an existing file with different
/// content refuses the install unless `overwrite_presets` is set — a user's
/// edited preset is never clobbered silently.
fn install_preset(
    document: &NodeDocument,
    overwrite_presets: bool,
    prompts_dir: &Path,
) -> Result<(), String> {
    let Some(preset) = &document.preset else {
        return Ok(());
    };
    let presets = vec![GraphExportPromptPreset {
        name: preset.name.clone(),
        content: preset.content.clone(),
    }];
    if overwrite_presets {
        crate::domain::graph_transfer::install_overwriting(&presets, prompts_dir)
    } else {
        crate::domain::graph_transfer::validate(&presets, prompts_dir)?;
        crate::domain::graph_transfer::install(&presets, prompts_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::graph_install::stub::{StubCatalogue, StubRoute};

    /// A valid draft id: 26 lowercase base32 characters.
    const DRAFT_ID: &str = "abcdefghijklmnopqrstuvwxyz";

    fn draft_reference() -> String {
        format!("draft:{DRAFT_ID}")
    }

    fn node_doc() -> serde_json::Value {
        serde_json::json!({
            "format_version": 1,
            "name": "auditor",
            "kind": "agent",
            "config": {"prompt_template": "audit this", "timeout_minutes": 10},
        })
    }

    fn stub_serving(path: &str, body: &str) -> StubCatalogue {
        StubCatalogue::start(
            [(path.to_string(), StubRoute::ok(body))]
                .into_iter()
                .collect(),
        )
    }

    fn dirs() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let nodes = crate::domain::nodes::nodes_dir(home.path());
        let prompts = crate::domain::prompts::prompts_dir(home.path());
        (home, nodes, prompts)
    }

    #[test]
    fn fetch_node_document_writes_nothing_and_reports_the_source() {
        let stub = stub_serving("/i/node/auditor", &node_doc().to_string());
        let (_home, nodes, prompts) = dirs();
        let fetched = fetch_node_document(&stub.base_url(), "auditor").unwrap();
        assert_eq!(fetched.document.name, "auditor");
        assert_eq!(fetched.source, "official");
        assert_eq!(
            fetched.source_url,
            format!("{}/i/node/auditor", stub.base_url())
        );
        assert!(
            !nodes.join("auditor.json").exists(),
            "the fetch writes nothing before the review is accepted"
        );

        let written = write_node_install(&fetched.document, false, &nodes, &prompts).unwrap();
        assert!(written.is_empty());
        let stored = crate::domain::nodes::resolve(&nodes, "auditor")
            .unwrap()
            .expect("stored");
        assert_eq!(stored.source, crate::domain::nodes::Source::Installed);
        assert_eq!(stored.document.config["timeout_minutes"], 10);
    }

    #[test]
    fn fetch_node_document_requests_the_node_catalogue_path_and_names_the_source() {
        let stub = stub_serving("/i/acme/widgets/node/auditor", &node_doc().to_string());
        let fetched = fetch_node_document(&stub.base_url(), "acme/widgets/auditor@v1").unwrap();
        assert_eq!(fetched.source, "acme/widgets@v1");
        let hits = stub.hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target, "/i/acme/widgets/node/auditor?ref=v1");
    }

    #[test]
    fn write_node_install_installs_the_bundled_preset() {
        let mut doc = node_doc();
        doc["config"]["prompt_preset"] = serde_json::json!("auditor-preset");
        doc["preset"] = serde_json::json!({"name": "auditor-preset", "content": "audit well"});
        let stub = stub_serving("/i/node/auditor", &doc.to_string());
        let (_home, nodes, prompts) = dirs();
        let fetched = fetch_node_document(&stub.base_url(), "auditor").unwrap();
        let written = write_node_install(&fetched.document, false, &nodes, &prompts).unwrap();
        assert_eq!(written, vec!["auditor-preset"]);
        assert_eq!(
            std::fs::read_to_string(prompts.join("auditor-preset.md")).unwrap(),
            "audit well"
        );
    }

    #[test]
    fn write_node_install_preset_conflict_writes_neither_preset_nor_node() {
        let mut doc = node_doc();
        doc["preset"] = serde_json::json!({"name": "auditor-preset", "content": "new"});
        let stub = stub_serving("/i/node/auditor", &doc.to_string());
        let (_home, nodes, prompts) = dirs();
        std::fs::create_dir_all(&prompts).unwrap();
        std::fs::write(prompts.join("auditor-preset.md"), "old").unwrap();
        let fetched = fetch_node_document(&stub.base_url(), "auditor").unwrap();

        let error = write_node_install(&fetched.document, false, &nodes, &prompts)
            .expect_err("conflict must fail");
        assert!(error.contains("different content"), "{error}");
        assert_eq!(
            std::fs::read_to_string(prompts.join("auditor-preset.md")).unwrap(),
            "old"
        );
        assert!(
            !nodes.join("auditor.json").exists(),
            "a conflict aborts before the document is written"
        );
    }

    #[test]
    fn fetch_node_document_refuses_a_graph_document() {
        let graph = serde_json::json!({
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
        });
        let stub = stub_serving("/i/node/auditor", &graph.to_string());
        let (_home, nodes, _prompts) = dirs();
        let error =
            fetch_node_document(&stub.base_url(), "auditor").expect_err("graph doc must fail");
        assert!(error.contains("graph document"), "{error}");
        assert!(!nodes.join("auditor.json").exists());
    }

    #[test]
    fn fetch_node_document_lists_every_validation_problem() {
        let mut doc = node_doc();
        doc["name"] = serde_json::json!("Not A Slug");
        doc["config"]["bogus"] = serde_json::json!(1);
        let stub = stub_serving("/i/node/auditor", &doc.to_string());
        let error =
            fetch_node_document(&stub.base_url(), "auditor").expect_err("invalid doc must fail");
        assert!(error.contains("name"), "{error}");
        assert!(error.contains("config"), "{error}");
    }

    #[test]
    fn fetch_node_document_fetches_a_draft_and_reports_the_draft_source() {
        let stub = stub_serving(&format!("/i/draft/{DRAFT_ID}"), &node_doc().to_string());
        let fetched = fetch_node_document(&stub.base_url(), &draft_reference()).unwrap();
        assert_eq!(fetched.document.name, "auditor");
        assert_eq!(fetched.source, "draft");
        assert_eq!(
            fetched.source_url,
            format!("{}/i/draft/{DRAFT_ID}", stub.base_url())
        );
        let hits = stub.hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target, format!("/i/draft/{DRAFT_ID}"));
    }

    #[test]
    fn fetch_node_document_refuses_a_draft_holding_a_graph_document() {
        let graph = serde_json::json!({
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
        });
        let stub = stub_serving(&format!("/i/draft/{DRAFT_ID}"), &graph.to_string());
        let error = fetch_node_document(&stub.base_url(), &draft_reference())
            .expect_err("graph doc must fail");
        assert!(error.contains("graph document"), "{error}");
        assert!(error.contains("shared"), "{error}");
    }

    #[test]
    fn fetch_node_document_reports_a_missing_draft_with_its_lifetime() {
        let stub = StubCatalogue::start(std::collections::HashMap::new());
        let error =
            fetch_node_document(&stub.base_url(), &draft_reference()).expect_err("404 must fail");
        assert_eq!(
            error,
            "draft not found: drafts last 30 minutes and are deleted by their first install"
        );
    }

    #[test]
    fn write_node_install_writes_the_document() {
        let stub = stub_serving("/i/node/auditor", &node_doc().to_string());
        let (_home, nodes, prompts) = dirs();
        let fetched = fetch_node_document(&stub.base_url(), "auditor").unwrap();
        write_node_install(&fetched.document, false, &nodes, &prompts).unwrap();
        let stored = crate::domain::nodes::resolve(&nodes, "auditor")
            .unwrap()
            .expect("stored");
        assert_eq!(stored.document.name, "auditor");
    }
}
