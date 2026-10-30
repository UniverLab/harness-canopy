//! Install a graph from the catalogue (`canopy graph install` / `graph_edit`).
//!
//! One command turns a published graph into a local graph: parse the ref,
//! GET the export document through the catalogue (so the catalogue counts
//! the install), validate it exactly as `canopy graph validate` would,
//! install its bundled presets, and hand the document to the same import
//! path `graph_edit` uses. The only network traffic is that one GET with
//! a `canopy/<version>` User-Agent — no telemetry beyond the catalogue's
//! own count.

mod fetch;
pub(crate) use fetch::fetch_document;
#[cfg(test)]
pub(crate) mod stub;

use std::path::Path;

use serde_json::Value;

use crate::domain::graph_transfer::{parse_export_document_str, GraphExportDocument};

/// A parsed catalogue ref: `<slug>` (official) or `<owner>/<repo>/<slug>`
/// (community), each with an optional `@<ref>` suffix selecting a version —
/// or `draft:<id>`, an unreviewed editor draft served directly by the
/// catalogue.
#[derive(Debug)]
pub(crate) enum GraphRef {
    /// A published ref: `(owner, repo)` scope for community, `None` for an
    /// official slug.
    Standard {
        /// `(owner, repo)` for a community ref; `None` for an official slug.
        scope: Option<(String, String)>,
        slug: String,
        /// The `@<ref>` suffix, sent as the `ref` query parameter.
        git_ref: Option<String>,
    },
    /// An editor draft: 26 lowercase base32 characters.
    Draft { id: String },
}

impl GraphRef {
    /// Render the ref back the way the user wrote it (for messages).
    fn display(&self) -> String {
        match self {
            GraphRef::Standard {
                scope,
                slug,
                git_ref,
            } => {
                let mut text = match scope {
                    None => slug.clone(),
                    Some((owner, repo)) => format!("{owner}/{repo}/{slug}"),
                };
                if let Some(git_ref) = git_ref {
                    text.push('@');
                    text.push_str(git_ref);
                }
                text
            }
            GraphRef::Draft { id } => format!("draft:{id}"),
        }
    }

    /// Whether this ref is an editor draft.
    pub(crate) fn is_draft(&self) -> bool {
        matches!(self, GraphRef::Draft { .. })
    }

    /// The draft id, when this ref is one.
    pub(crate) fn draft_id(&self) -> Option<&str> {
        match self {
            GraphRef::Draft { id } => Some(id),
            GraphRef::Standard { .. } => None,
        }
    }

    /// The stored source identity for an install from this ref: the slug for
    /// an official ref, `owner/repo/slug` for a community one — both without
    /// any `@<ref>` version suffix, so reinstalling another version updates
    /// in place — or `draft:<id>` for an editor draft.
    pub(crate) fn source_identity(&self) -> String {
        match self {
            GraphRef::Standard { scope, slug, .. } => match scope {
                None => slug.clone(),
                Some((owner, repo)) => format!("{owner}/{repo}/{slug}"),
            },
            GraphRef::Draft { id } => format!("draft:{id}"),
        }
    }

    /// The source label for the install review: `draft` for a draft, else
    /// the official/community label.
    pub(crate) fn source_label(&self) -> String {
        match self {
            GraphRef::Draft { .. } => "draft".to_string(),
            GraphRef::Standard { scope, git_ref, .. } => {
                crate::domain::nodes::review::source_label(scope, git_ref)
            }
        }
    }

    /// The slug (or draft id) for messages that name the document.
    fn slug_or_id(&self) -> &str {
        match self {
            GraphRef::Standard { slug, .. } => slug,
            GraphRef::Draft { id } => id,
        }
    }
}

/// Parse a graph catalogue ref: `<slug>` or `<owner>/<repo>/<slug>`, each
/// with an optional `@<ref>` suffix — or `draft:<id>` for an editor draft.
/// The slug is `[a-z0-9-]{1,48}`; owner and repo follow GitHub's rules; the
/// draft id is 26 lowercase base32 characters. Anything else is refused with
/// the accepted forms in the message.
pub(crate) fn parse_ref(raw: &str) -> Result<GraphRef, String> {
    parse_ref_for("graph", raw)
}

/// Parse a node catalogue ref — the same grammar as a graph ref, with the
/// refusal naming a node ref.
pub(crate) fn parse_ref_node(raw: &str) -> Result<GraphRef, String> {
    parse_ref_for("node", raw)
}

/// The draft id when `reference` parses as a `draft:<id>` ref — the one
/// check callers use instead of matching the `draft:` prefix themselves.
/// The grammar is the same for graph and node refs, so one helper serves
/// both install paths.
pub(crate) fn draft_id(reference: &str) -> Option<String> {
    parse_ref(reference).ok()?.draft_id().map(str::to_string)
}

fn parse_ref_for(kind: &str, raw: &str) -> Result<GraphRef, String> {
    let trimmed = raw.trim();
    if let Some(id) = trimmed.strip_prefix("draft:") {
        return if is_draft_id(id) {
            Ok(GraphRef::Draft { id: id.to_string() })
        } else {
            Err(invalid_ref_message(kind, raw))
        };
    }
    let (path, git_ref) = match trimmed.split_once('@') {
        None => (trimmed, None),
        Some((path, git_ref)) => {
            if path.is_empty() || !is_git_ref(git_ref) {
                return Err(invalid_ref_message(kind, raw));
            }
            (path, Some(git_ref.to_string()))
        }
    };
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        [slug] if is_slug(slug) => Ok(GraphRef::Standard {
            scope: None,
            slug: (*slug).to_string(),
            git_ref,
        }),
        [owner, repo, slug] if is_github_owner(owner) && is_github_repo(repo) && is_slug(slug) => {
            Ok(GraphRef::Standard {
                scope: Some(((*owner).to_string(), (*repo).to_string())),
                slug: (*slug).to_string(),
                git_ref,
            })
        }
        _ => Err(invalid_ref_message(kind, raw)),
    }
}

fn invalid_ref_message(kind: &str, raw: &str) -> String {
    format!(
        "Invalid {kind} ref '{raw}'. Accepted forms: <slug> or <owner>/<repo>/<slug>, \
         each with an optional @<ref> suffix (for example 'my-graph', \
         'acme/widgets/my-graph' or 'acme/widgets/my-graph@v1.2'), \
         or 'draft:<id>' for an editor draft (26 lowercase base32 characters). \
         The slug is 1-48 lowercase letters, digits, or '-'."
    )
}

/// A draft id: exactly 26 lowercase base32 characters (`[a-z2-7]{26}`).
fn is_draft_id(id: &str) -> bool {
    id.len() == 26
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || matches!(b, b'2'..=b'7'))
}

/// A shareable slug: `[a-z0-9-]{1,48}` — the same rule node documents use.
fn is_slug(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A GitHub username: 1-39 alphanumerics or hyphens, never leading,
/// trailing, or doubled.
fn is_github_owner(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 39
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// A GitHub repository name: 1-100 letters, digits, `.`, `_` or `-`,
/// never `.` or `..`.
fn is_github_repo(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// A `@<ref>` suffix: non-empty, no whitespace or control characters.
fn is_git_ref(git_ref: &str) -> bool {
    !git_ref.is_empty()
        && !git_ref
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
}

/// A catalogue document fetched and validated — nothing written yet. Its
/// `source` is the review's source label (`official`, `owner/repo`, each
/// with `@<ref>`, or `draft`), so the CLI, MCP and docs name the origin the
/// same way.
#[derive(Debug)]
pub(crate) struct FetchedGraph {
    pub document: GraphExportDocument,
    /// The catalogue URL the document body was finally read from (after any
    /// redirects).
    pub source_url: String,
    /// The source label for the install review.
    pub source: String,
    /// The stored source identity ([`GraphRef::source_identity`]): what the
    /// install stamps on the bundled presets it writes.
    pub source_ref: String,
    /// The raw validated body — a draft review parks it for the accept call,
    /// since the draft is consumed by the first fetch.
    pub body: String,
}

/// Fetch the document for `reference` from the catalogue at `base` and
/// validate it exactly as `graph` would. Nothing is written: the
/// caller builds the review first, and only [`write_presets`] (after the
/// user accepted) touches disk.
pub(crate) fn fetch_graph(base: &str, reference: &str) -> Result<FetchedGraph, String> {
    let parsed = parse_ref(reference)?;
    let fetched = fetch::fetch_document(base, &parsed, "graph")?;
    assemble_graph(&parsed, reference, &fetched.body, fetched.source_url)
}

/// Rebuild the fetch result from a draft body the review call already
/// consumed: the same validation and import-path parsing as [`fetch_graph`],
/// without touching the network.
pub(crate) fn graph_from_body(
    reference: &str,
    body: &str,
    source_url: String,
) -> Result<FetchedGraph, String> {
    let parsed = parse_ref(reference)?;
    assemble_graph(&parsed, reference, body, source_url)
}

/// Validate `body` exactly as `graph` would and parse it for the
/// import path — the tail [`fetch_graph`] and [`graph_from_body`] share.
fn assemble_graph(
    parsed: &GraphRef,
    reference: &str,
    body: &str,
    source_url: String,
) -> Result<FetchedGraph, String> {
    let source = parsed.source_label();
    let source_ref = parsed.source_identity();
    let report = crate::daemon::graph_validate::validate_document_str(body);
    reject_invalid_report(reference, &report)?;
    let document = parse_export_document_str(body)?;
    Ok(FetchedGraph {
        document,
        source_url,
        source,
        source_ref,
        body: body.to_string(),
    })
}

/// Install the document's bundled presets into `prompts_dir` — refusing
/// (before anything is written) when a local preset file differs and belongs
/// to another source, unless `overwrite_presets` is set — and return the
/// preset names written (new or overwritten; files already present with
/// identical content are not listed). Every preset written is stamped with
/// `source_ref`, and a differing file owned by the same source is updated
/// in place.
pub(crate) fn write_presets(
    document: &GraphExportDocument,
    source_ref: &str,
    overwrite_presets: bool,
    prompts_dir: &Path,
) -> Result<Vec<String>, String> {
    let presets_written = presets_to_write(document, prompts_dir);
    crate::domain::graph_transfer::install_for_install(
        &document.prompt_presets,
        prompts_dir,
        source_ref,
        overwrite_presets,
    )?;
    Ok(presets_written)
}

/// Refuse a `--as` prefix that is not a slug — the check
/// [`rename_colliding_presets`] runs, exposed so callers can validate before
/// fetching: a one-use draft must not be consumed by a request whose `--as`
/// was never valid.
pub(crate) fn validate_as_prefix(prefix: &str) -> Result<(), String> {
    let trimmed = prefix.trim();
    if !is_slug(trimmed) {
        return Err(format!(
            "Invalid --as prefix '{trimmed}': use 1-48 lowercase letters, digits, or '-'."
        ));
    }
    Ok(())
}

/// Rename every bundled preset the install would refuse under the ownership
/// rule to `{prefix}-{preset}`, rewriting the agent node configs that
/// reference it — the `--as PREFIX` path of `canopy graph install`. The
/// caller runs this before building the review, so the review shows the
/// names that will be written. Entry-invalid presets are left alone (they
/// fail at write time instead); a renamed target that still collides
/// refuses. With `overwrite` nothing would refuse, so nothing is renamed.
pub(crate) fn rename_colliding_presets(
    document: &mut GraphExportDocument,
    prefix: &str,
    source_ref: &str,
    overwrite: bool,
    prompts_dir: &Path,
) -> Result<(), String> {
    validate_as_prefix(prefix)?;
    let prefix = prefix.trim();
    let mut renames: Vec<(String, String)> = Vec::new();
    for preset in &document.prompt_presets {
        let single = std::slice::from_ref(preset);
        if crate::domain::graph_transfer::validate_for_install(
            single,
            prompts_dir,
            source_ref,
            overwrite,
        )
        .is_ok()
        {
            continue;
        }
        let mut seen = std::collections::BTreeSet::new();
        if crate::domain::graph_transfer::validate_entry(preset, &mut seen).is_err() {
            continue;
        }
        renames.push((preset.name.clone(), format!("{prefix}-{}", preset.name)));
    }
    if renames.is_empty() {
        return Ok(());
    }
    for preset in &mut document.prompt_presets {
        if let Some((_, new)) = renames.iter().find(|(old, _)| *old == preset.name) {
            preset.name = new.clone();
        }
    }
    for node in &mut document.nodes {
        let Some(referenced) = node.config.get("prompt_preset").and_then(Value::as_str) else {
            continue;
        };
        if let Some((_, new)) = renames.iter().find(|(old, _)| *old == referenced) {
            node.config["prompt_preset"] = serde_json::json!(new);
        }
    }
    crate::domain::graph_transfer::validate_for_install(
        &document.prompt_presets,
        prompts_dir,
        source_ref,
        overwrite,
    )
}

/// Refuse a catalogue body that fails validation — listing every problem —
/// or that validates as something other than a graph document.
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
    if report.get("kind").and_then(serde_json::Value::as_str) != Some("graph") {
        let name = report
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        return Err(format!(
            "The catalogue returned a node document '{name}', not a graph document; \
             `graph_edit` only installs graph documents."
        ));
    }
    Ok(())
}

/// The bundled presets whose files the install will write: every entry that
/// is missing locally or differs. Entries already present with identical
/// content are no-ops and not listed.
fn presets_to_write(document: &GraphExportDocument, prompts_dir: &Path) -> Vec<String> {
    document
        .prompt_presets
        .iter()
        .filter(|preset| {
            let path = prompts_dir.join(format!("{}.md", preset.name));
            match std::fs::read_to_string(&path) {
                Ok(existing) => existing != preset.content,
                Err(_) => true,
            }
        })
        .map(|preset| preset.name.clone())
        .collect()
}

/// The catalogue base URL from `[graphs] catalogue_url`
/// (`https://graphs.univerlab.org` by default).
pub(crate) fn catalogue_url() -> String {
    let dir = crate::domain::prompts::canopy_dir();
    crate::domain::canopy_config::CanopyConfig::load(&dir)
        .graphs
        .catalogue_url
}

#[cfg(test)]
mod tests {
    use super::stub::{StubCatalogue, StubRoute};
    use super::*;
    use serde_json::json;

    fn graph_doc() -> serde_json::Value {
        json!({
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

    fn stub_serving(body: &str) -> StubCatalogue {
        StubCatalogue::start(
            [("/i/graph/my-graph".to_string(), StubRoute::ok(body))]
                .into_iter()
                .collect(),
        )
    }

    fn as_standard(reference: &GraphRef) -> (&Option<(String, String)>, &str, &Option<String>) {
        let GraphRef::Standard {
            scope,
            slug,
            git_ref,
        } = reference
        else {
            panic!("{reference:?} must parse as a standard ref");
        };
        (scope, slug, git_ref)
    }

    #[test]
    fn parse_ref_accepts_official_community_and_versioned_refs() {
        let official = parse_ref("my-graph").unwrap();
        assert!(!official.is_draft());
        let (scope, slug, git_ref) = as_standard(&official);
        assert!(scope.is_none());
        assert_eq!(slug, "my-graph");
        assert!(git_ref.is_none());

        let community = parse_ref("acme/widgets/my-graph").unwrap();
        let (scope, slug, _) = as_standard(&community);
        assert_eq!(*scope, Some(("acme".to_string(), "widgets".to_string())));
        assert_eq!(slug, "my-graph");

        let versioned = parse_ref("acme/widgets/my-graph@v1.2").unwrap();
        assert_eq!(as_standard(&versioned).2.as_deref(), Some("v1.2"));
        let versioned_official = parse_ref("my-graph@abc123").unwrap();
        assert_eq!(
            as_standard(&versioned_official).2.as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn install_action_declares_ref_workdir_name_and_overwrite() {
        let spec = crate::daemon::handler_graph_tools::graph_edit_action("install")
            .expect("graph_edit has an install action");
        let declared: Vec<&str> = spec
            .required
            .iter()
            .chain(spec.optional.iter())
            .copied()
            .collect();
        assert!(declared.contains(&"ref"), "{declared:?}");
        assert!(
            !declared.contains(&"reference"),
            "wire name is `ref`: {declared:?}"
        );
        assert!(declared.contains(&"workdir"), "{declared:?}");
        assert!(declared.contains(&"name"), "{declared:?}");
        assert!(declared.contains(&"overwrite_presets"), "{declared:?}");
        assert!(declared.contains(&"accept"), "{declared:?}");
        assert!(spec.required.contains(&"ref"), "{:?}", spec.required);
        assert!(spec.required.contains(&"workdir"), "{:?}", spec.required);
    }

    /// A valid draft id: 26 lowercase base32 characters.
    const DRAFT_ID: &str = "abcdefghijklmnopqrstuvwxyz";

    #[test]
    fn parse_ref_accepts_a_draft_id_for_graphs_and_nodes() {
        let reference = format!("draft:{DRAFT_ID}");
        for parsed in [parse_ref(&reference), parse_ref_node(&reference)] {
            let parsed = parsed.unwrap();
            assert!(parsed.is_draft());
            assert_eq!(parsed.draft_id(), Some(DRAFT_ID));
            assert_eq!(parsed.source_label(), "draft");
        }
        assert_eq!(draft_id(&reference).as_deref(), Some(DRAFT_ID));
        assert_eq!(draft_id("my-graph"), None);
    }

    #[test]
    fn parse_ref_refuses_anything_else_after_draft_with_the_accepted_forms() {
        let long = format!("draft:{DRAFT_ID}7");
        let suffixed = format!("draft:{DRAFT_ID}@v1");
        let cases = vec![
            "draft:",
            "draft:too-short",
            long.as_str(),
            "draft:ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            "draft:abcdefghijkmnpqrstuvwxyz01",
            "draft:abcdefghijkmnpqrstuvwxyz-9",
            suffixed.as_str(),
            "draft: abcdefghijklmnopqrstuvwxyz",
        ];
        for case in cases {
            for parsed in [parse_ref(case), parse_ref_node(case)] {
                let error = parsed.expect_err(&format!("{case:?} must be refused"));
                assert!(error.contains("Accepted forms"), "{case:?}: {error}");
                assert!(error.contains("draft:<id>"), "{case:?}: {error}");
            }
        }
        let error = parse_ref("My-Graph").expect_err("still refused");
        assert!(error.contains("draft:<id>"), "{error}");
    }

    #[test]
    fn fetch_graph_refuses_a_malformed_draft_id_before_any_request() {
        let stub = stub_serving(&graph_doc().to_string());
        let error = fetch_graph(&stub.base_url(), "draft:not-an-id")
            .expect_err("malformed draft id must fail");
        assert!(error.contains("draft:<id>"), "{error}");
        assert!(
            stub.hits().is_empty(),
            "a malformed id never reaches the network"
        );
    }

    #[test]
    fn fetch_graph_fetches_a_draft_and_reports_the_draft_source() {
        let stub = StubCatalogue::start(
            [(
                format!("/i/draft/{DRAFT_ID}"),
                StubRoute::ok(&graph_doc().to_string()),
            )]
            .into_iter()
            .collect(),
        );
        let fetched = fetch_graph(&stub.base_url(), &format!("draft:{DRAFT_ID}")).unwrap();
        assert_eq!(fetched.document.name, "shared");
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
    fn fetch_graph_refuses_a_draft_holding_a_node_document() {
        let node = json!({
            "format_version": 1,
            "name": "auditor",
            "kind": "agent",
            "config": {"prompt_template": "audit this"},
        });
        let stub = StubCatalogue::start(
            [(
                format!("/i/draft/{DRAFT_ID}"),
                StubRoute::ok(&node.to_string()),
            )]
            .into_iter()
            .collect(),
        );
        let error = fetch_graph(&stub.base_url(), &format!("draft:{DRAFT_ID}"))
            .expect_err("node doc must fail");
        assert!(error.contains("node document"), "{error}");
        assert!(error.contains("auditor"), "{error}");
    }

    #[test]
    fn fetch_graph_reports_a_missing_draft_with_its_lifetime() {
        let stub = StubCatalogue::start(std::collections::HashMap::new());
        let error =
            fetch_graph(&stub.base_url(), &format!("draft:{DRAFT_ID}")).expect_err("404 must fail");
        assert_eq!(
            error,
            "draft not found: drafts last 30 minutes and are deleted by their first install"
        );
    }

    #[test]
    fn parse_ref_refuses_anything_else_with_the_accepted_forms() {
        let long_slug = "a".repeat(49);
        let cases = vec![
            "",
            "a/b",
            "a/b/c/d",
            "My-Graph",
            "my_graph",
            "my graph",
            long_slug.as_str(),
            "-acme/widgets/g",
            "acme-/widgets/g",
            "ac--me/widgets/g",
            "acme/wid gets/g",
            "acme/../g",
            "acme//g",
            "my-graph@",
            "@v1",
            "my-graph@v 1",
        ];
        for case in cases {
            let error = parse_ref(case).expect_err(&format!("{case:?} must be refused"));
            assert!(error.contains("Accepted forms"), "{case:?}: {error}");
        }
    }

    #[test]
    fn fetch_graph_succeeds_and_reports_the_source_and_url() {
        let stub = stub_serving(&graph_doc().to_string());
        let fetched = fetch_graph(&stub.base_url(), "my-graph").unwrap();
        assert_eq!(fetched.document.name, "shared");
        assert_eq!(fetched.source, "official");
        assert_eq!(
            fetched.source_url,
            format!("{}/i/graph/my-graph", stub.base_url())
        );
        let prompts = tempfile::tempdir().unwrap();
        let written = write_presets(
            &fetched.document,
            &fetched.source_ref,
            false,
            prompts.path(),
        )
        .unwrap();
        assert!(written.is_empty());
    }

    #[test]
    fn fetch_graph_names_the_community_source_with_its_ref() {
        let stub = StubCatalogue::start(
            [(
                "/i/acme/widgets/graph/my-graph".to_string(),
                StubRoute::ok(&graph_doc().to_string()),
            )]
            .into_iter()
            .collect(),
        );
        let fetched = fetch_graph(&stub.base_url(), "acme/widgets/my-graph@v1.2").unwrap();
        assert_eq!(fetched.source, "acme/widgets@v1.2");
    }

    #[test]
    fn fetch_graph_lists_every_validation_problem() {
        let mut doc = graph_doc();
        doc["nodes"][0]["config"]["bogus_key"] = json!(1);
        doc["nodes"][1]["config"]["bogus_key"] = json!(2);
        let stub = stub_serving(&doc.to_string());
        let error = fetch_graph(&stub.base_url(), "my-graph").expect_err("invalid doc must fail");
        assert!(error.contains("nodes[0].config"), "{error}");
        assert!(error.contains("nodes[1].config"), "{error}");
    }

    #[test]
    fn fetch_graph_refuses_a_node_document() {
        let node = json!({
            "format_version": 1,
            "name": "auditor",
            "kind": "agent",
            "config": {"prompt_template": "audit this"},
        });
        let stub = stub_serving(&node.to_string());
        let error = fetch_graph(&stub.base_url(), "my-graph").expect_err("node doc must fail");
        assert!(error.contains("node document"), "{error}");
    }

    fn preset_doc() -> serde_json::Value {
        let mut doc = graph_doc();
        doc["nodes"][0]["config"]["prompt_preset"] = json!("custom");
        doc["prompt_presets"] = json!([
            {"name": "aaa", "content": "aaa content"},
            {"name": "custom", "content": "new content"},
            {"name": "same", "content": "same content"},
        ]);
        doc
    }

    #[test]
    fn write_presets_conflict_writes_nothing() {
        let stub = stub_serving(&preset_doc().to_string());
        let fetched = fetch_graph(&stub.base_url(), "my-graph").unwrap();
        let prompts = tempfile::tempdir().unwrap();
        std::fs::write(prompts.path().join("custom.md"), "old content").unwrap();
        std::fs::write(prompts.path().join("same.md"), "same content").unwrap();
        let error = write_presets(
            &fetched.document,
            &fetched.source_ref,
            false,
            prompts.path(),
        )
        .expect_err("conflict must fail");
        assert!(error.contains("custom"), "{error}");
        assert!(error.contains("different content"), "{error}");
        assert!(
            !prompts.path().join("aaa.md").exists(),
            "a conflict aborts before anything is written"
        );
        assert_eq!(
            std::fs::read_to_string(prompts.path().join("custom.md")).unwrap(),
            "old content"
        );
    }

    #[test]
    fn write_presets_overwrite_replaces_differing_presets() {
        let stub = stub_serving(&preset_doc().to_string());
        let fetched = fetch_graph(&stub.base_url(), "my-graph").unwrap();
        let prompts = tempfile::tempdir().unwrap();
        std::fs::write(prompts.path().join("custom.md"), "old content").unwrap();
        std::fs::write(prompts.path().join("same.md"), "same content").unwrap();
        let written =
            write_presets(&fetched.document, &fetched.source_ref, true, prompts.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(prompts.path().join("custom.md")).unwrap(),
            "new content"
        );
        assert_eq!(
            std::fs::read_to_string(prompts.path().join("aaa.md")).unwrap(),
            "aaa content"
        );
        assert_eq!(written, vec!["aaa", "custom"]);
    }

    #[test]
    fn source_identity_drops_the_version_but_keeps_the_scope() {
        assert_eq!(parse_ref("my-graph").unwrap().source_identity(), "my-graph");
        assert_eq!(
            parse_ref("my-graph@abc123").unwrap().source_identity(),
            "my-graph"
        );
        assert_eq!(
            parse_ref("acme/widgets/my-graph@v1.2")
                .unwrap()
                .source_identity(),
            "acme/widgets/my-graph"
        );
        let draft = format!("draft:{DRAFT_ID}");
        assert_eq!(parse_ref(&draft).unwrap().source_identity(), draft);
    }

    #[test]
    fn write_presets_updates_a_preset_owned_by_the_same_source() {
        let stub = stub_serving(&graph_doc().to_string());
        let fetched = fetch_graph(&stub.base_url(), "my-graph").unwrap();
        let prompts = tempfile::tempdir().unwrap();
        let first =
            crate::domain::graph_transfer::parse_export_document_str(&preset_doc().to_string())
                .unwrap();
        write_presets(&first, &fetched.source_ref, false, prompts.path()).unwrap();

        let mut second = preset_doc();
        second["prompt_presets"][1]["content"] = json!("newer content");
        let second =
            crate::domain::graph_transfer::parse_export_document_str(&second.to_string()).unwrap();
        let written = write_presets(&second, &fetched.source_ref, false, prompts.path())
            .expect("same-source reinstall updates without flags");
        assert_eq!(written, vec!["custom"]);
        assert_eq!(
            std::fs::read_to_string(prompts.path().join("custom.md")).unwrap(),
            "newer content"
        );
    }

    #[test]
    fn rename_colliding_presets_renames_only_what_would_refuse() {
        let stub = stub_serving(&preset_doc().to_string());
        let fetched = fetch_graph(&stub.base_url(), "my-graph").unwrap();
        let prompts = tempfile::tempdir().unwrap();
        std::fs::write(prompts.path().join("custom.md"), "old content").unwrap();
        std::fs::write(prompts.path().join("same.md"), "same content").unwrap();

        let mut document = fetched.document.clone();
        rename_colliding_presets(
            &mut document,
            "alice",
            &fetched.source_ref,
            false,
            prompts.path(),
        )
        .unwrap();
        let names: Vec<&str> = document
            .prompt_presets
            .iter()
            .map(|preset| preset.name.as_str())
            .collect();
        assert_eq!(names, vec!["aaa", "alice-custom", "same"]);
        assert_eq!(
            document.nodes[0].config["prompt_preset"], "alice-custom",
            "the node config follows the rename"
        );

        let written = write_presets(&document, &fetched.source_ref, false, prompts.path()).unwrap();
        assert_eq!(written, vec!["aaa", "alice-custom"]);
        assert_eq!(
            std::fs::read_to_string(prompts.path().join("custom.md")).unwrap(),
            "old content",
            "the colliding file is left alone"
        );
        assert_eq!(
            std::fs::read_to_string(prompts.path().join("alice-custom.md")).unwrap(),
            "new content"
        );
    }

    #[test]
    fn rename_colliding_presets_refuses_a_taken_target() {
        let stub = stub_serving(&preset_doc().to_string());
        let fetched = fetch_graph(&stub.base_url(), "my-graph").unwrap();
        let prompts = tempfile::tempdir().unwrap();
        std::fs::write(prompts.path().join("custom.md"), "old content").unwrap();
        std::fs::write(prompts.path().join("alice-custom.md"), "someone else").unwrap();

        let mut document = fetched.document.clone();
        let error = rename_colliding_presets(
            &mut document,
            "alice",
            &fetched.source_ref,
            false,
            prompts.path(),
        )
        .expect_err("a taken rename target must refuse");
        assert!(error.contains("alice-custom"), "{error}");
        assert!(error.contains("different content"), "{error}");
    }

    #[test]
    fn rename_colliding_presets_refuses_a_bad_prefix() {
        let stub = stub_serving(&preset_doc().to_string());
        let fetched = fetch_graph(&stub.base_url(), "my-graph").unwrap();
        let prompts = tempfile::tempdir().unwrap();
        let mut document = fetched.document.clone();
        let error = rename_colliding_presets(
            &mut document,
            "Not A Prefix",
            &fetched.source_ref,
            false,
            prompts.path(),
        )
        .expect_err("a non-slug prefix must fail");
        assert!(error.contains("Invalid --as prefix"), "{error}");
    }
}
