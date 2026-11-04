//! Install a shareable node document from the catalogue
//! (`canopy node install` / `node` `install`).
//!
//! The same one-GET path `graph install` uses — parse the ref, fetch the
//! document through the catalogue's install-counting endpoint, validate it
//! exactly as `canopy graph validate` would, then install its bundled preset
//! into the prompts dir (never silently over a preset owned by another
//! source) before writing `~/.canopy/nodes/<name>.json`. A document that
//! validates as anything other than a node is refused: installing a graph
//! document as a node would leave a file no authoring tool could use.
//!
//! A name already installed from a DIFFERENT source is refused — every graph
//! using the installed copy keeps running it — naming both sources and the
//! way out: `--as <local-name>` installs under another local name, or
//! `--replace` overwrites. Reinstalling from the SAME source updates in
//! place, and an installed copy still overrides a bare builtin.

use std::path::Path;

use crate::domain::graph_transfer::GraphExportPromptPreset;
use crate::domain::nodes::NodeDocument;

/// Options for [`write_node_install`]: the fetched ref's stored source
/// identity plus the collision flags (`--as`/`--replace`) and the preset
/// flag (`--overwrite-presets`).
#[derive(Debug, Clone)]
pub(crate) struct NodeInstallOptions {
    /// The fetched ref's stored source identity (`slug`,
    /// `owner/repo/slug`, or `draft:<id>`): stamped on the document and its
    /// bundled preset, and compared against the installed copy's.
    pub source_ref: String,
    /// `--as`: install under another local name instead of the document's
    /// own (see [`apply_as`]).
    pub local_name: Option<String>,
    /// `--replace`: overwrite an installed copy from another source (and a
    /// differing preset regardless of owner).
    pub replace: bool,
    /// `--overwrite-presets`: overwrite a differing preset file regardless
    /// of owner.
    pub overwrite_presets: bool,
}

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
    /// The stored source identity (see `GraphRef::source_identity`): what
    /// the install stamps on the document and its bundled preset.
    pub source_ref: String,
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
    let source_ref = parsed.source_identity();
    let report = crate::daemon::graph_validate::validate_document_str(body);
    reject_invalid_report(reference, &report)?;
    let document = crate::domain::nodes::parse_document(body)?;
    Ok(FetchedNode {
        document,
        source_url,
        source,
        source_ref,
        body: body.to_string(),
    })
}

/// Install one fetched node document: rename it under `--as` first, refuse
/// a name installed from another source (unless `--replace`), then install
/// its preset into `prompts_dir` with the ownership rule and write the
/// stamped document to `nodes_dir` — returning the preset names written
/// (new or overwritten; a file already present with identical content is
/// not listed). Every refusal lands before anything is written.
pub(crate) fn write_node_install(
    document: &NodeDocument,
    options: &NodeInstallOptions,
    nodes_dir: &Path,
    prompts_dir: &Path,
) -> Result<Vec<String>, String> {
    let mut document = match &options.local_name {
        Some(name) => apply_as(document, name)?,
        None => document.clone(),
    };
    check_collision(nodes_dir, &document, &options.source_ref, options.replace)?;
    let presets_written = presets_to_write(&document, prompts_dir);
    install_preset(&document, options, prompts_dir)?;
    // The catalogue never stamps the source — whatever the fetched body
    // claims is replaced with the ref it was fetched from.
    document.catalogue_source = Some(options.source_ref.clone());
    crate::domain::nodes::write(nodes_dir, &document)?;
    Ok(presets_written)
}

/// Refuse a `--as` local name that is not a slug — the check [`apply_as`]
/// runs, exposed so callers can validate before fetching: a one-use draft
/// must not be consumed by a request whose `--as` was never valid.
pub(crate) fn validate_as_name(local_name: &str) -> Result<(), String> {
    let trimmed = local_name.trim();
    if !crate::domain::nodes::is_slug(trimmed) {
        return Err(format!(
            "Invalid node name '{trimmed}': use 1-48 lowercase letters, digits, or '-'."
        ));
    }
    Ok(())
}

/// Rename a fetched document under `--as` (`local_name`): the document
/// takes the new name, its bundled preset is renamed with it — a
/// `<doc>-…` preset keeps its suffix under the new name, any other preset
/// becomes `<as>-<preset>` — and a `prompt_preset` config pointing at the
/// old preset name is rewritten to the new one. A non-slug local name is
/// refused. The caller applies this to the fetched document for the review
/// as well, so the review shows the names that will be written.
pub(crate) fn apply_as(document: &NodeDocument, local_name: &str) -> Result<NodeDocument, String> {
    validate_as_name(local_name)?;
    let local_name = local_name.trim();
    let mut renamed = document.clone();
    let old_name = renamed.name.clone();
    renamed.name = local_name.to_string();
    let Some(preset) = renamed.preset.as_mut() else {
        return Ok(renamed);
    };
    let old_preset = preset.name.clone();
    let prefix = format!("{old_name}-");
    preset.name = match old_preset.strip_prefix(&prefix) {
        Some(rest) => format!("{local_name}-{rest}"),
        None => format!("{local_name}-{old_preset}"),
    };
    if renamed
        .config
        .get("prompt_preset")
        .and_then(serde_json::Value::as_str)
        == Some(old_preset.as_str())
    {
        renamed.config["prompt_preset"] = serde_json::json!(preset.name);
    }
    Ok(renamed)
}

/// Refuse a document whose name is already installed from another source —
/// naming the installed source (or that the copy is local) and the incoming
/// one, with the way out — unless `replace` overwrites. No installed copy
/// installs (over a bare builtin, the override rule, unchanged), and a copy
/// from the SAME source updates in place. `None` (a hand-written or
/// pre-source copy) is local and never silently taken.
fn check_collision(
    nodes_dir: &Path,
    document: &NodeDocument,
    source_ref: &str,
    replace: bool,
) -> Result<(), String> {
    let Some(existing) = crate::domain::nodes::read_installed(nodes_dir, &document.name)? else {
        return Ok(());
    };
    if existing.catalogue_source.as_deref() == Some(source_ref) {
        return Ok(());
    }
    if replace {
        return Ok(());
    }
    Err(match existing.catalogue_source {
        Some(owner) => format!(
            "Node '{}' is already installed from '{owner}'; '{source_ref}' ships the same name. \
             Install it under another local name with --as <local-name>, or pass --replace to overwrite.",
            document.name
        ),
        None => format!(
            "Node '{}' already exists locally (not installed from the catalogue); '{source_ref}' ships the same name. \
             Install it under another local name with --as <local-name>, or pass --replace to overwrite.",
            document.name
        ),
    })
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

/// Install the document's bundled preset with the ownership rule: an
/// unsafe name, a builtin's name, or a differing file owned by another
/// source refuses the install — a preset name owned by another source is
/// never overwritten silently — while a differing file owned by the SAME
/// source is updated in place. `--replace` and `--overwrite-presets` both
/// overwrite regardless of owner.
fn install_preset(
    document: &NodeDocument,
    options: &NodeInstallOptions,
    prompts_dir: &Path,
) -> Result<(), String> {
    let Some(preset) = &document.preset else {
        return Ok(());
    };
    let presets = vec![GraphExportPromptPreset {
        name: preset.name.clone(),
        content: preset.content.clone(),
    }];
    crate::domain::graph_transfer::install_for_install(
        &presets,
        prompts_dir,
        &options.source_ref,
        options.overwrite_presets || options.replace,
    )
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

    fn options(source_ref: &str) -> NodeInstallOptions {
        NodeInstallOptions {
            source_ref: source_ref.to_string(),
            local_name: None,
            replace: false,
            overwrite_presets: false,
        }
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

        let written = write_node_install(
            &fetched.document,
            &options(&fetched.source_ref),
            &nodes,
            &prompts,
        )
        .unwrap();
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
        let written = write_node_install(
            &fetched.document,
            &options(&fetched.source_ref),
            &nodes,
            &prompts,
        )
        .unwrap();
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

        let error = write_node_install(
            &fetched.document,
            &options(&fetched.source_ref),
            &nodes,
            &prompts,
        )
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
        write_node_install(
            &fetched.document,
            &options(&fetched.source_ref),
            &nodes,
            &prompts,
        )
        .unwrap();
        let stored = crate::domain::nodes::resolve(&nodes, "auditor")
            .unwrap()
            .expect("stored");
        assert_eq!(stored.document.name, "auditor");
    }

    fn official_and_community_docs() -> (serde_json::Value, serde_json::Value) {
        let mut official = node_doc();
        official["preset"] =
            serde_json::json!({"name": "auditor-preset", "content": "official text"});
        let mut community = node_doc();
        community["config"]["prompt_template"] = serde_json::json!("alice audits");
        community["preset"] =
            serde_json::json!({"name": "auditor-preset", "content": "alice text"});
        (official, community)
    }

    fn stub_serving_both(official: &str, community: &str) -> StubCatalogue {
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

    #[test]
    fn install_from_another_source_refuses_and_writes_nothing() {
        let (official, community) = official_and_community_docs();
        let stub = stub_serving_both(&official.to_string(), &community.to_string());
        let (_home, nodes, prompts) = dirs();
        let first = fetch_node_document(&stub.base_url(), "auditor").unwrap();
        assert_eq!(first.source_ref, "auditor");
        write_node_install(
            &first.document,
            &options(&first.source_ref),
            &nodes,
            &prompts,
        )
        .unwrap();

        let second = fetch_node_document(&stub.base_url(), "alice/lab/auditor").unwrap();
        assert_eq!(second.source_ref, "alice/lab/auditor");
        let error = write_node_install(
            &second.document,
            &options(&second.source_ref),
            &nodes,
            &prompts,
        )
        .expect_err("another source must refuse");
        assert!(error.contains("'auditor'"), "{error}");
        assert!(error.contains("'alice/lab/auditor'"), "{error}");
        assert!(error.contains("--as"), "{error}");
        assert!(error.contains("--replace"), "{error}");

        let stored = crate::domain::nodes::resolve(&nodes, "auditor")
            .unwrap()
            .expect("stored");
        assert_eq!(stored.document.config["prompt_template"], "audit this");
        assert_eq!(stored.document.catalogue_source.as_deref(), Some("auditor"));
        assert_eq!(
            std::fs::read_to_string(prompts.join("auditor-preset.md")).unwrap(),
            "official text"
        );
    }

    #[test]
    fn reinstall_from_the_same_source_updates_in_place() {
        let mut first = node_doc();
        first["preset"] = serde_json::json!({"name": "auditor-preset", "content": "v1"});
        let stub = stub_serving("/i/node/auditor", &first.to_string());
        let (_home, nodes, prompts) = dirs();
        let fetched = fetch_node_document(&stub.base_url(), "auditor").unwrap();
        write_node_install(
            &fetched.document,
            &options(&fetched.source_ref),
            &nodes,
            &prompts,
        )
        .unwrap();

        let mut second = node_doc();
        second["config"]["prompt_template"] = serde_json::json!("audit harder");
        second["preset"] = serde_json::json!({"name": "auditor-preset", "content": "v2"});
        let updated = crate::domain::nodes::parse_document(&second.to_string()).unwrap();
        let written = write_node_install(&updated, &options("auditor"), &nodes, &prompts)
            .expect("same-source reinstall updates without flags");
        assert_eq!(written, vec!["auditor-preset"]);

        let stored = crate::domain::nodes::resolve(&nodes, "auditor")
            .unwrap()
            .expect("stored");
        assert_eq!(stored.document.config["prompt_template"], "audit harder");
        assert_eq!(stored.document.catalogue_source.as_deref(), Some("auditor"));
        assert_eq!(
            std::fs::read_to_string(prompts.join("auditor-preset.md")).unwrap(),
            "v2"
        );
    }

    #[test]
    fn replace_overwrites_an_installed_copy_from_another_source() {
        let (official, community) = official_and_community_docs();
        let stub = stub_serving_both(&official.to_string(), &community.to_string());
        let (_home, nodes, prompts) = dirs();
        let first = fetch_node_document(&stub.base_url(), "auditor").unwrap();
        write_node_install(
            &first.document,
            &options(&first.source_ref),
            &nodes,
            &prompts,
        )
        .unwrap();

        let second = fetch_node_document(&stub.base_url(), "alice/lab/auditor").unwrap();
        let mut replace = options(&second.source_ref);
        replace.replace = true;
        write_node_install(&second.document, &replace, &nodes, &prompts)
            .expect("--replace overwrites");

        let stored = crate::domain::nodes::resolve(&nodes, "auditor")
            .unwrap()
            .expect("stored");
        assert_eq!(stored.document.config["prompt_template"], "alice audits");
        assert_eq!(
            stored.document.catalogue_source.as_deref(),
            Some("alice/lab/auditor")
        );
        assert_eq!(
            std::fs::read_to_string(prompts.join("auditor-preset.md")).unwrap(),
            "alice text"
        );
    }

    #[test]
    fn install_restamps_a_source_the_catalogue_body_claims() {
        let mut doc = node_doc();
        doc["catalogue_source"] = serde_json::json!("mallory/evil/auditor");
        let stub = stub_serving("/i/node/auditor", &doc.to_string());
        let (_home, nodes, prompts) = dirs();
        let fetched = fetch_node_document(&stub.base_url(), "auditor").unwrap();
        write_node_install(
            &fetched.document,
            &options(&fetched.source_ref),
            &nodes,
            &prompts,
        )
        .unwrap();
        let stored = crate::domain::nodes::resolve(&nodes, "auditor")
            .unwrap()
            .expect("stored");
        assert_eq!(stored.document.catalogue_source.as_deref(), Some("auditor"));
    }

    #[test]
    fn apply_as_renames_the_document_preset_and_config_reference() {
        let mut doc = node_doc();
        doc["config"]["prompt_preset"] = serde_json::json!("auditor-preset");
        doc["preset"] = serde_json::json!({"name": "auditor-preset", "content": "audit well"});
        let document = crate::domain::nodes::parse_document(&doc.to_string()).unwrap();

        let renamed = apply_as(&document, "alice-auditor").unwrap();
        assert_eq!(renamed.name, "alice-auditor");
        let preset = renamed.preset.expect("preset kept");
        assert_eq!(preset.name, "alice-auditor-preset");
        assert_eq!(preset.content, "audit well");
        assert_eq!(renamed.config["prompt_preset"], "alice-auditor-preset");
        // The original is untouched.
        assert_eq!(document.name, "auditor");
    }

    #[test]
    fn apply_as_prefixes_a_preset_that_does_not_share_the_document_name() {
        let mut doc = node_doc();
        doc["config"]["prompt_preset"] = serde_json::json!("review");
        doc["preset"] = serde_json::json!({"name": "review", "content": "review well"});
        let document = crate::domain::nodes::parse_document(&doc.to_string()).unwrap();

        let renamed = apply_as(&document, "alice-auditor").unwrap();
        assert_eq!(
            renamed.preset.expect("preset kept").name,
            "alice-auditor-review"
        );
        assert_eq!(renamed.config["prompt_preset"], "alice-auditor-review");
    }

    #[test]
    fn apply_as_renames_a_preset_without_touching_an_unrelated_config() {
        let mut doc = node_doc();
        doc["config"]["prompt_preset"] = serde_json::json!("reviewer");
        doc["preset"] = serde_json::json!({"name": "auditor-preset", "content": "audit well"});
        let document = crate::domain::nodes::parse_document(&doc.to_string()).unwrap();

        let renamed = apply_as(&document, "alice-auditor").unwrap();
        assert_eq!(
            renamed.preset.expect("preset kept").name,
            "alice-auditor-preset"
        );
        assert_eq!(
            renamed.config["prompt_preset"], "reviewer",
            "a reference that is not the old preset name is left alone"
        );

        let plain = crate::domain::nodes::parse_document(&node_doc().to_string()).unwrap();
        let renamed = apply_as(&plain, "alice-auditor").unwrap();
        assert_eq!(renamed.name, "alice-auditor");
        assert!(renamed.preset.is_none());

        let error = apply_as(&plain, "Not A Slug").expect_err("non-slug --as must fail");
        assert!(error.contains("Invalid node name"), "{error}");
    }

    #[test]
    fn as_target_installed_from_another_source_refuses() {
        let (official, community) = official_and_community_docs();
        let stub = stub_serving_both(&official.to_string(), &community.to_string());
        let (_home, nodes, prompts) = dirs();
        let community = fetch_node_document(&stub.base_url(), "alice/lab/auditor").unwrap();
        let mut under_as = options(&community.source_ref);
        under_as.local_name = Some("alice-auditor".to_string());
        write_node_install(&community.document, &under_as, &nodes, &prompts).unwrap();

        let official = fetch_node_document(&stub.base_url(), "auditor").unwrap();
        let mut onto_taken = options(&official.source_ref);
        onto_taken.local_name = Some("alice-auditor".to_string());
        let error = write_node_install(&official.document, &onto_taken, &nodes, &prompts)
            .expect_err("--as onto a foreign name must refuse");
        assert!(error.contains("'alice-auditor'"), "{error}");
        assert!(error.contains("'alice/lab/auditor'"), "{error}");
        assert!(error.contains("'auditor'"), "{error}");

        let stored = crate::domain::nodes::resolve(&nodes, "alice-auditor")
            .unwrap()
            .expect("stored");
        assert_eq!(stored.document.config["prompt_template"], "alice audits");
    }

    fn reviewer_doc(name: &str, preset_content: &str) -> serde_json::Value {
        serde_json::json!({
            "format_version": 1,
            "name": name,
            "kind": "agent",
            "config": {"prompt_template": "review this", "timeout_minutes": 10},
            "preset": {"name": "shared-preset", "content": preset_content},
        })
    }

    /// Two different node names sharing one preset name: the second source
    /// refuses on the preset — never silently taking it — naming both
    /// sources, and writes neither its preset nor its node.
    #[test]
    fn preset_owned_by_another_source_refuses_when_node_names_differ() {
        let official = reviewer_doc("reviewer", "official text");
        let mut community = reviewer_doc("reviewer", "alice text");
        community["name"] = serde_json::json!("alice-reviewer");
        let stub = StubCatalogue::start(
            [
                (
                    "/i/node/reviewer".to_string(),
                    StubRoute::ok(&official.to_string()),
                ),
                (
                    "/i/alice/lab/node/alice-reviewer".to_string(),
                    StubRoute::ok(&community.to_string()),
                ),
            ]
            .into_iter()
            .collect(),
        );
        let (_home, nodes, prompts) = dirs();
        let first = fetch_node_document(&stub.base_url(), "reviewer").unwrap();
        write_node_install(
            &first.document,
            &options(&first.source_ref),
            &nodes,
            &prompts,
        )
        .unwrap();

        let second = fetch_node_document(&stub.base_url(), "alice/lab/alice-reviewer").unwrap();
        let error = write_node_install(
            &second.document,
            &options(&second.source_ref),
            &nodes,
            &prompts,
        )
        .expect_err("a preset owned by another source must refuse");
        assert!(error.contains("'shared-preset'"), "{error}");
        assert!(error.contains("'reviewer'"), "{error}");
        assert!(error.contains("'alice/lab/alice-reviewer'"), "{error}");
        assert!(error.contains("--as"), "{error}");
        assert!(
            !nodes.join("alice-reviewer.json").exists(),
            "a preset collision writes no node document"
        );
        assert_eq!(
            std::fs::read_to_string(prompts.join("shared-preset.md")).unwrap(),
            "official text"
        );
    }

    /// The preset-only collision's ways out: `--overwrite-presets` replaces
    /// the preset, `--replace` replaces it too, and `--as` renames the
    /// preset with the node so both copies coexist.
    #[test]
    fn preset_owned_by_another_source_has_three_ways_out() {
        let official = reviewer_doc("reviewer", "official text");
        let mut community = reviewer_doc("reviewer", "alice text");
        community["name"] = serde_json::json!("alice-reviewer");

        // `--overwrite-presets` replaces the preset and keeps the node name.
        let stub = StubCatalogue::start(
            [
                (
                    "/i/node/reviewer".to_string(),
                    StubRoute::ok(&official.to_string()),
                ),
                (
                    "/i/alice/lab/node/alice-reviewer".to_string(),
                    StubRoute::ok(&community.to_string()),
                ),
            ]
            .into_iter()
            .collect(),
        );
        let (_home, nodes, prompts) = dirs();
        let first = fetch_node_document(&stub.base_url(), "reviewer").unwrap();
        write_node_install(
            &first.document,
            &options(&first.source_ref),
            &nodes,
            &prompts,
        )
        .unwrap();
        let second = fetch_node_document(&stub.base_url(), "alice/lab/alice-reviewer").unwrap();
        let mut overwrite = options(&second.source_ref);
        overwrite.overwrite_presets = true;
        write_node_install(&second.document, &overwrite, &nodes, &prompts)
            .expect("--overwrite-presets replaces the preset");
        assert_eq!(
            std::fs::read_to_string(prompts.join("shared-preset.md")).unwrap(),
            "alice text"
        );
        assert!(nodes.join("alice-reviewer.json").exists());

        // `--replace` replaces the preset too.
        let (_home, nodes, prompts) = dirs();
        let first = fetch_node_document(&stub.base_url(), "reviewer").unwrap();
        write_node_install(
            &first.document,
            &options(&first.source_ref),
            &nodes,
            &prompts,
        )
        .unwrap();
        let second = fetch_node_document(&stub.base_url(), "alice/lab/alice-reviewer").unwrap();
        let mut replace = options(&second.source_ref);
        replace.replace = true;
        write_node_install(&second.document, &replace, &nodes, &prompts)
            .expect("--replace replaces the preset");
        assert_eq!(
            std::fs::read_to_string(prompts.join("shared-preset.md")).unwrap(),
            "alice text"
        );

        // `--as` renames the preset with the node: both files coexist.
        let (_home, nodes, prompts) = dirs();
        let first = fetch_node_document(&stub.base_url(), "reviewer").unwrap();
        write_node_install(
            &first.document,
            &options(&first.source_ref),
            &nodes,
            &prompts,
        )
        .unwrap();
        let second = fetch_node_document(&stub.base_url(), "alice/lab/alice-reviewer").unwrap();
        let mut renamed = options(&second.source_ref);
        renamed.local_name = Some("alice-reviewer-2".to_string());
        write_node_install(&second.document, &renamed, &nodes, &prompts)
            .expect("--as renames the preset with the node");
        assert_eq!(
            std::fs::read_to_string(prompts.join("shared-preset.md")).unwrap(),
            "official text",
            "the owned preset is untouched"
        );
        assert_eq!(
            std::fs::read_to_string(prompts.join("alice-reviewer-2-shared-preset.md")).unwrap(),
            "alice text"
        );
    }
}
