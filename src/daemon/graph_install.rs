//! Install a graph from the catalogue (`canopy graph install` / `graph_install`).
//!
//! One command turns a published graph into a local graph: parse the ref,
//! GET the export document through the catalogue (so the catalogue counts
//! the install), validate it exactly as `canopy graph validate` would,
//! install its bundled presets, and hand the document to the same import
//! path `graph_import` uses. The only network traffic is that one GET with
//! a `canopy/<version>` User-Agent — no telemetry beyond the catalogue's
//! own count.

mod fetch;
#[cfg(test)]
pub(crate) mod stub;

use std::path::Path;

use crate::domain::graph_transfer::{parse_export_document_str, GraphExportDocument};

/// A parsed catalogue ref: `<slug>` (official) or `<owner>/<repo>/<slug>`
/// (community), each with an optional `@<ref>` suffix selecting a version.
#[derive(Debug)]
pub(crate) struct GraphRef {
    /// `(owner, repo)` for a community ref; `None` for an official slug.
    pub scope: Option<(String, String)>,
    pub slug: String,
    /// The `@<ref>` suffix, sent as the `ref` query parameter.
    pub git_ref: Option<String>,
}

impl GraphRef {
    /// Render the ref back the way the user wrote it (for messages).
    fn display(&self) -> String {
        let mut text = match &self.scope {
            None => self.slug.clone(),
            Some((owner, repo)) => format!("{owner}/{repo}/{}", self.slug),
        };
        if let Some(git_ref) = &self.git_ref {
            text.push('@');
            text.push_str(git_ref);
        }
        text
    }
}

/// Parse a catalogue ref: `<slug>` or `<owner>/<repo>/<slug>`, each with an
/// optional `@<ref>` suffix. The slug is `[a-z0-9-]{1,48}`; owner and repo
/// follow GitHub's rules. Anything else is refused with the accepted forms
/// in the message.
pub(crate) fn parse_ref(raw: &str) -> Result<GraphRef, String> {
    let trimmed = raw.trim();
    let (path, git_ref) = match trimmed.split_once('@') {
        None => (trimmed, None),
        Some((path, git_ref)) => {
            if path.is_empty() || !is_git_ref(git_ref) {
                return Err(invalid_ref_message(raw));
            }
            (path, Some(git_ref.to_string()))
        }
    };
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        [slug] if is_slug(slug) => Ok(GraphRef {
            scope: None,
            slug: (*slug).to_string(),
            git_ref,
        }),
        [owner, repo, slug] if is_github_owner(owner) && is_github_repo(repo) && is_slug(slug) => {
            Ok(GraphRef {
                scope: Some(((*owner).to_string(), (*repo).to_string())),
                slug: (*slug).to_string(),
                git_ref,
            })
        }
        _ => Err(invalid_ref_message(raw)),
    }
}

fn invalid_ref_message(raw: &str) -> String {
    format!(
        "Invalid graph ref '{raw}'. Accepted forms: <slug> or <owner>/<repo>/<slug>, \
         each with an optional @<ref> suffix (for example 'my-graph', \
         'acme/widgets/my-graph' or 'acme/widgets/my-graph@v1.2'). \
         The slug is 1-48 lowercase letters, digits, or '-'."
    )
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

/// A catalogue document fetched, validated, and with its presets installed —
/// ready for the import path.
#[derive(Debug)]
pub(crate) struct PreparedInstall {
    pub document: GraphExportDocument,
    /// Preset names whose files this install wrote (new or overwritten);
    /// files already present with identical content are not listed.
    pub presets_written: Vec<String>,
    /// The catalogue URL the document body was finally read from (after any
    /// redirects).
    pub source_url: String,
}

/// Fetch the document for `reference` from the catalogue at `base`,
/// validate it exactly as `graph_validate` would, and install its bundled
/// presets into `prompts_dir` — refusing (before anything is written) when
/// a local preset file differs, unless `overwrite_presets` is set.
pub(crate) fn prepare_install(
    base: &str,
    reference: &str,
    overwrite_presets: bool,
    prompts_dir: &Path,
) -> Result<PreparedInstall, String> {
    let parsed = parse_ref(reference)?;
    let fetched = fetch::fetch_document(base, &parsed)?;
    let report = crate::daemon::graph_validate::validate_document_str(&fetched.body);
    reject_invalid_report(reference, &report)?;
    let document = parse_export_document_str(&fetched.body)?;
    let presets_written = presets_to_write(&document, prompts_dir);
    if overwrite_presets {
        crate::domain::graph_transfer::install_overwriting(&document.prompt_presets, prompts_dir)?;
    } else {
        crate::domain::graph_transfer::validate(&document.prompt_presets, prompts_dir)?;
        crate::domain::graph_transfer::install(&document.prompt_presets, prompts_dir)?;
    }
    Ok(PreparedInstall {
        document,
        presets_written,
        source_url: fetched.source_url,
    })
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
             `graph_install` only installs graph documents."
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

    #[test]
    fn parse_ref_accepts_official_community_and_versioned_refs() {
        let official = parse_ref("my-graph").unwrap();
        assert!(official.scope.is_none());
        assert_eq!(official.slug, "my-graph");
        assert!(official.git_ref.is_none());

        let community = parse_ref("acme/widgets/my-graph").unwrap();
        assert_eq!(
            community.scope,
            Some(("acme".to_string(), "widgets".to_string()))
        );
        assert_eq!(community.slug, "my-graph");

        let versioned = parse_ref("acme/widgets/my-graph@v1.2").unwrap();
        assert_eq!(versioned.git_ref.as_deref(), Some("v1.2"));
        let versioned_official = parse_ref("my-graph@abc123").unwrap();
        assert_eq!(versioned_official.git_ref.as_deref(), Some("abc123"));
    }

    #[test]
    fn advertised_schema_exposes_ref_workdir_name_and_overwrite() {
        let tools = crate::daemon::handler::TaskTriggerHandler::tool_router().list_all();
        let tool = tools
            .iter()
            .find(|t| t.name.as_ref() == "graph_install")
            .expect("graph_install is registered");
        let schema = serde_json::to_value(&tool.input_schema).expect("schema serializes");
        let props = schema["properties"].as_object().expect("properties object");
        assert!(props.contains_key("ref"), "schema: {schema}");
        assert!(!props.contains_key("reference"), "schema: {schema}");
        assert!(props.contains_key("workdir"), "schema: {schema}");
        assert!(props.contains_key("name"), "schema: {schema}");
        assert!(props.contains_key("overwrite_presets"), "schema: {schema}");
        let required = schema["required"].as_array().expect("required array");
        assert!(required.contains(&json!("ref")), "schema: {schema}");
        assert!(required.contains(&json!("workdir")), "schema: {schema}");
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
    fn prepare_install_succeeds_and_reports_nothing_written_without_presets() {
        let stub = stub_serving(&graph_doc().to_string());
        let prompts = tempfile::tempdir().unwrap();
        let prepared =
            prepare_install(&stub.base_url(), "my-graph", false, prompts.path()).unwrap();
        assert_eq!(prepared.document.name, "shared");
        assert!(prepared.presets_written.is_empty());
        assert_eq!(
            prepared.source_url,
            format!("{}/i/graph/my-graph", stub.base_url())
        );
    }

    #[test]
    fn prepare_install_lists_every_validation_problem() {
        let mut doc = graph_doc();
        doc["nodes"][0]["config"]["bogus_key"] = json!(1);
        doc["nodes"][1]["config"]["bogus_key"] = json!(2);
        let stub = stub_serving(&doc.to_string());
        let prompts = tempfile::tempdir().unwrap();
        let error = prepare_install(&stub.base_url(), "my-graph", false, prompts.path())
            .expect_err("invalid doc must fail");
        assert!(error.contains("nodes[0].config"), "{error}");
        assert!(error.contains("nodes[1].config"), "{error}");
    }

    #[test]
    fn prepare_install_refuses_a_node_document() {
        let node = json!({
            "format_version": 1,
            "name": "auditor",
            "kind": "agent",
            "config": {"prompt_template": "audit this"},
        });
        let stub = stub_serving(&node.to_string());
        let prompts = tempfile::tempdir().unwrap();
        let error = prepare_install(&stub.base_url(), "my-graph", false, prompts.path())
            .expect_err("node doc must fail");
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
    fn prepare_install_preset_conflict_writes_nothing() {
        let stub = stub_serving(&preset_doc().to_string());
        let prompts = tempfile::tempdir().unwrap();
        std::fs::write(prompts.path().join("custom.md"), "old content").unwrap();
        std::fs::write(prompts.path().join("same.md"), "same content").unwrap();
        let error = prepare_install(&stub.base_url(), "my-graph", false, prompts.path())
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
    fn prepare_install_overwrite_replaces_differing_presets() {
        let stub = stub_serving(&preset_doc().to_string());
        let prompts = tempfile::tempdir().unwrap();
        std::fs::write(prompts.path().join("custom.md"), "old content").unwrap();
        std::fs::write(prompts.path().join("same.md"), "same content").unwrap();
        let prepared = prepare_install(&stub.base_url(), "my-graph", true, prompts.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(prompts.path().join("custom.md")).unwrap(),
            "new content"
        );
        assert_eq!(
            std::fs::read_to_string(prompts.path().join("aaa.md")).unwrap(),
            "aaa content"
        );
        assert_eq!(prepared.presets_written, vec!["aaa", "custom"]);
    }
}
