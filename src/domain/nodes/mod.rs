//! Shareable node documents — a single Rust Designer, Python Validator, or
//! classic implementer as one installable file.
//!
//! A node document is `{format_version, name, kind, description?, config,
//! preset?}`; `canopy graph validate` is its schema check (see
//! `daemon::graph`), and `graph_node`/`graph_ensemble` can
//! instantiate one by name instead of asking the caller for a full `config`.
//! Installed copies live in `~/.canopy/nodes/<name>.json`; the six former
//! builtin blueprints are embedded in the binary with `include_str!` and are
//! always resolvable offline. Lookup order is installed copy first, then the
//! embedded builtin — so installing your own `implementer` overrides the
//! builtin one without losing the ability to remove the override again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod ensemble;
pub use ensemble::EnsembleNodeConfig;

pub(crate) mod attention;
pub(crate) mod review;

/// The optional prompt preset a node document bundles: installed into
/// `~/.canopy/prompts/<name>.md` with the graph installer's no-silent-
/// overwrite rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodePreset {
    pub name: String,
    pub content: String,
}

/// One shareable node: what it is (`kind`), what it does (its `config`), and
/// optionally the prompt preset its config references.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDocument {
    pub format_version: i64,
    pub name: String,
    /// `agent`, `check`, `gate`, `router`, or `ensemble`.
    pub kind: String,
    /// Documentation only — the rules never read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub config: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<NodePreset>,
}

/// Where a resolvable node document came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Installed,
    Builtin,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Installed => "installed",
            Source::Builtin => "builtin",
        }
    }
}

/// A resolved node document plus where it was found.
#[derive(Debug, Clone)]
pub struct NodeEntry {
    pub document: NodeDocument,
    pub source: Source,
}

/// The embedded builtins, sorted by name so every listing and error message
/// is deterministic.
const BUILTIN_DOCUMENTS: [(&str, &str); 6] = [
    ("cargo-gates", include_str!("builtin/cargo-gates.json")),
    ("commit-check", include_str!("builtin/commit-check.json")),
    (
        "ensemble-proposers",
        include_str!("builtin/ensemble-proposers.json"),
    ),
    ("implementer", include_str!("builtin/implementer.json")),
    ("resilience", include_str!("builtin/resilience.json")),
    (
        "reviewer-committer",
        include_str!("builtin/reviewer-committer.json"),
    ),
];

/// `~/.canopy/nodes` — where installed node documents live.
pub fn nodes_dir(canopy_dir: &Path) -> PathBuf {
    canopy_dir.join("nodes")
}

/// Parse one node document, refusing anything the shape does not define.
pub fn parse_document(raw: &str) -> Result<NodeDocument, String> {
    serde_json::from_str(raw).map_err(|error| format!("Invalid node document: {error}."))
}

/// A shareable name: `[a-z0-9-]{1,48}` — the same rule the catalogue's node
/// slugs use, and the only shape safe to join into `<dir>/<name>.json`.
pub fn is_slug(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Look up one embedded builtin. The embedded bytes are compile-time
/// constants covered by `builtins_parse_and_validate`, so a parse failure
/// here is a build defect, not a user error.
pub fn builtin(name: &str) -> Option<NodeDocument> {
    let (_, raw) = BUILTIN_DOCUMENTS
        .iter()
        .find(|(builtin, _)| *builtin == name)?;
    Some(parse_document(raw).expect("embedded builtin node document is valid"))
}

/// Read an installed copy from `dir`, if one exists. A file that exists but
/// does not parse is an error, never silently treated as absent: the user
/// asked for that copy.
pub fn read_installed(dir: &Path, name: &str) -> Result<Option<NodeDocument>, String> {
    if !is_slug(name) {
        return Err(invalid_name_message(name));
    }
    let path = dir.join(format!("{name}.json"));
    match std::fs::read_to_string(&path) {
        Ok(raw) => parse_document(&raw)
            .map(Some)
            .map_err(|error| format!("{}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Could not read {}: {error}.", path.display())),
    }
}

/// Resolve one name: an installed copy wins over the embedded builtin.
pub fn resolve(dir: &Path, name: &str) -> Result<Option<NodeEntry>, String> {
    if let Some(document) = read_installed(dir, name)? {
        return Ok(Some(NodeEntry {
            document,
            source: Source::Installed,
        }));
    }
    Ok(builtin(name).map(|document| NodeEntry {
        document,
        source: Source::Builtin,
    }))
}

/// Every resolvable node, installed copies first (they replace the builtin
/// of the same name), sorted by name.
pub fn list(dir: &Path) -> Result<Vec<NodeEntry>, String> {
    let mut entries: BTreeMap<String, NodeEntry> = BTreeMap::new();
    for (name, raw) in BUILTIN_DOCUMENTS {
        entries.insert(
            name.to_string(),
            NodeEntry {
                document: parse_document(raw).expect("embedded builtin node document is valid"),
                source: Source::Builtin,
            },
        );
    }
    for document in installed_documents(dir)? {
        entries.insert(
            document.name.clone(),
            NodeEntry {
                document,
                source: Source::Installed,
            },
        );
    }
    Ok(entries.into_values().collect())
}

/// Every parseable installed document in `dir` (a missing directory is an
/// empty store).
fn installed_documents(dir: &Path) -> Result<Vec<NodeDocument>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "Could not read the nodes dir {}: {error}.",
                dir.display()
            ))
        }
    };
    let mut documents = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("Could not read {}: {error}.", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|error| format!("Could not read {}: {error}.", path.display()))?;
        documents
            .push(parse_document(&raw).map_err(|error| format!("{}: {error}", path.display()))?);
    }
    Ok(documents)
}

/// Write an installed copy, creating the nodes dir when needed, and return
/// the path written.
pub fn write(dir: &Path, document: &NodeDocument) -> Result<PathBuf, String> {
    if !is_slug(&document.name) {
        return Err(invalid_name_message(&document.name));
    }
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("Could not create {}: {error}.", dir.display()))?;
    let path = dir.join(format!("{}.json", document.name));
    let mut body = serde_json::to_string_pretty(document)
        .map_err(|error| format!("Could not serialize node '{}': {error}.", document.name))?;
    body.push('\n');
    std::fs::write(&path, body)
        .map_err(|error| format!("Could not write {}: {error}.", path.display()))?;
    Ok(path)
}

/// What `remove` did: deleted an installed copy, or refused a builtin that
/// has no installed copy to delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveOutcome {
    Removed,
    BuiltinProtected,
}

/// Delete an installed copy. A builtin's installed override is deleted like
/// any other copy; a builtin with no override reports
/// [`RemoveOutcome::BuiltinProtected`] so the caller can say so.
pub fn remove(dir: &Path, name: &str) -> Result<RemoveOutcome, String> {
    if !is_slug(name) {
        return Err(invalid_name_message(name));
    }
    match std::fs::remove_file(dir.join(format!("{name}.json"))) {
        Ok(()) => Ok(RemoveOutcome::Removed),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if builtin(name).is_some() {
                Ok(RemoveOutcome::BuiltinProtected)
            } else {
                Err(unknown_node_message(dir, name)?)
            }
        }
        Err(error) => Err(format!("Could not remove node '{name}': {error}.")),
    }
}

/// The refusal for a name that does not exist, listing what does.
pub fn unknown_node_message(dir: &Path, name: &str) -> Result<String, String> {
    let names: Vec<String> = list(dir)?
        .into_iter()
        .map(|entry| format!("{} ({})", entry.document.name, entry.source.as_str()))
        .collect();
    Ok(format!(
        "Unknown node '{name}'. Available nodes: {}.",
        names.join(", ")
    ))
}

fn invalid_name_message(name: &str) -> String {
    format!("Invalid node name '{name}': use 1-48 lowercase letters, digits, or '-'.")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The six embedded builtins, sorted — the five former blueprints plus
    /// `ensemble-proposers`.
    const EXPECTED_BUILTINS: [&str; 6] = [
        "cargo-gates",
        "commit-check",
        "ensemble-proposers",
        "implementer",
        "resilience",
        "reviewer-committer",
    ];

    #[test]
    fn every_builtin_parses_and_validates_as_a_node_document() {
        for name in EXPECTED_BUILTINS {
            let document = builtin(name).unwrap_or_else(|| panic!("{name} resolves"));
            assert_eq!(document.name, name);
            assert_eq!(document.format_version, 1);
            let report = crate::daemon::graph_validate::validate_document_str(
                &serde_json::to_string(&document).unwrap(),
            );
            assert_eq!(
                report["ok"], true,
                "builtin '{name}' must validate: {report}"
            );
            assert_eq!(report["kind"], "node");
        }
    }

    #[test]
    fn the_five_former_blueprints_are_agent_and_check_nodes() {
        let mut kinds: Vec<(String, String)> = Vec::new();
        for name in ["implementer", "reviewer-committer", "resilience"] {
            let document = builtin(name).expect("agent builtin");
            assert_eq!(document.kind, "agent", "{name}");
            kinds.push((name.to_string(), document.kind));
            assert!(
                document.config.get("platform").is_none(),
                "a builtin node must not pin a harness"
            );
        }
        for name in ["cargo-gates", "commit-check"] {
            let document = builtin(name).expect("check builtin");
            assert_eq!(document.kind, "check", "{name}");
        }
        assert_eq!(kinds.len(), 3);
    }

    #[test]
    fn ensemble_proposers_is_an_ensemble_document_without_harnesses() {
        let document = builtin("ensemble-proposers").expect("ensemble builtin");
        assert_eq!(document.kind, "ensemble");
        let config = EnsembleNodeConfig::from_value("ensemble-proposers", &document.config)
            .expect("valid ensemble config");
        assert_eq!(config.members.len(), 3);
        assert!(config.prompt_template.contains("{{spec_content}}"));
        config.validate("ensemble-proposers").unwrap();
    }

    #[test]
    fn installed_copy_wins_over_the_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let nodes = nodes_dir(dir.path());

        let builtin_entry = resolve(&nodes, "implementer").unwrap().expect("builtin");
        assert_eq!(builtin_entry.source, Source::Builtin);

        let mut document = builtin("implementer").unwrap();
        document.config = serde_json::json!({"prompt_template": "my own implementer"});
        write(&nodes, &document).unwrap();

        let installed = resolve(&nodes, "implementer").unwrap().expect("installed");
        assert_eq!(installed.source, Source::Installed);
        assert_eq!(
            installed.document.config["prompt_template"],
            "my own implementer"
        );

        let listed = list(&nodes).unwrap();
        let implementer = listed
            .iter()
            .find(|entry| entry.document.name == "implementer")
            .expect("listed once");
        assert_eq!(implementer.source, Source::Installed);
        assert_eq!(
            listed
                .iter()
                .filter(|entry| entry.document.name == "implementer")
                .count(),
            1
        );
    }

    #[test]
    fn list_shows_builtins_offline() {
        let dir = tempfile::tempdir().unwrap();
        let listed = list(&nodes_dir(dir.path())).unwrap();
        assert_eq!(listed.len(), 6, "no installed copies, six builtins");
        assert!(listed.iter().all(|entry| entry.source == Source::Builtin));
    }

    #[test]
    fn remove_refuses_a_builtin_but_deletes_its_override() {
        let dir = tempfile::tempdir().unwrap();
        let nodes = nodes_dir(dir.path());
        assert_eq!(
            remove(&nodes, "implementer").unwrap(),
            RemoveOutcome::BuiltinProtected
        );

        write(&nodes, &builtin("implementer").unwrap()).unwrap();
        assert_eq!(
            remove(&nodes, "implementer").unwrap(),
            RemoveOutcome::Removed
        );
        assert_eq!(
            resolve(&nodes, "implementer")
                .unwrap()
                .expect("builtin remains")
                .source,
            Source::Builtin
        );
    }

    #[test]
    fn remove_refuses_an_unknown_name_with_the_available_list() {
        let dir = tempfile::tempdir().unwrap();
        let error = remove(&nodes_dir(dir.path()), "does-not-exist").unwrap_err();
        assert!(error.contains("does-not-exist"), "{error}");
        assert!(error.contains("implementer"), "{error}");
    }

    #[test]
    fn write_refuses_a_name_that_is_not_a_slug() {
        let dir = tempfile::tempdir().unwrap();
        let mut document = builtin("implementer").unwrap();
        document.name = "../escape".to_string();
        let error = write(&nodes_dir(dir.path()), &document).unwrap_err();
        assert!(error.contains("Invalid node name"), "{error}");
        assert!(!dir.path().join("escape.json").exists());
    }
}
