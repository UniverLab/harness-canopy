//! Node authoring helpers and node-catalogue MCP tools.
//!
//! A node document is the shareable alternative to a full `config`: the
//! catalogue tools here install/list/remove them, and `graph_node` /
//! `graph_ensemble` instantiate one by name. The resolvers live here
//! rather than in `handler_nodes` so the tool module and the node tools stay
//! one cohesive surface.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::{build_json_result, internal_error};
use crate::daemon::handler_validate::{validate_node_document_agent_harness, validate_node_kind};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::domain::graphs::GraphNodeKind;
use crate::domain::nodes::{self, RemoveOutcome};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

impl TaskTriggerHandler {
    pub(crate) async fn node_list_impl(&self) -> Result<CallToolResult, McpError> {
        let dir = nodes::nodes_dir(&crate::domain::prompts::canopy_dir());
        match nodes::list(&dir) {
            Ok(entries) => Ok(build_json_result(&serde_json::json!({
                "nodes": node_rows(&entries),
            }))),
            Err(message) => Ok(error_result(&message)),
        }
    }

    /// The `node` `get` action. No standalone `node_get` tool ever existed;
    /// this is the one document — name, kind, source, and full config — an
    /// installed copy shadowing a builtin wins, exactly like `list`.
    pub(crate) async fn node_get_impl(
        &self,
        params: NodeGetParams,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name.trim();
        let dir = nodes::nodes_dir(&crate::domain::prompts::canopy_dir());
        let resolved = match nodes::resolve(&dir, name) {
            Ok(resolved) => resolved,
            Err(message) => return Ok(error_result(&message)),
        };
        match resolved {
            Some(entry) => Ok(build_json_result(&serde_json::json!({
                "name": entry.document.name,
                "kind": entry.document.kind,
                "description": entry.document.description,
                "source": entry.source.as_str(),
                "catalogue_source": entry.document.catalogue_source,
                "config": entry.document.config,
            }))),
            None => match nodes::unknown_node_message(&dir, name) {
                Ok(message) => Ok(error_result(&message)),
                Err(message) => Ok(error_result(&message)),
            },
        }
    }

    pub(crate) async fn node_install_impl(
        &self,
        params: NodeInstallParams,
    ) -> Result<CallToolResult, McpError> {
        // The fetch is blocking, so it runs off the async runtime. It writes
        // nothing: the review comes first. A draft is consumed by the review
        // fetch, so the accept call reuses the parked body — with no parked
        // body it re-fetches and the 404 says the draft is gone.
        if let Some(name) = &params.as_name {
            if let Err(message) = crate::daemon::node_install::validate_as_name(name) {
                return Ok(error_result(&message));
            }
        }
        let accepted = params.accept == Some(true);
        let reference = params.reference.clone();
        let cached = if accepted {
            crate::daemon::graph_install::draft_id(&reference)
                .and_then(|id| crate::daemon::draft_pending::take(&id))
        } else {
            None
        };
        let fetched = tokio::task::spawn_blocking(move || match cached {
            Some((body, source_url)) => {
                crate::daemon::node_install::node_from_body(&reference, &body, source_url)
            }
            None => crate::daemon::node_install::fetch_node_document(
                &crate::daemon::graph_install::catalogue_url(),
                &reference,
            ),
        })
        .await
        .map_err(|error| internal_error(error.to_string()))?;
        let fetched = match fetched {
            Ok(fetched) => fetched,
            Err(message) => return Ok(error_result(&message)),
        };

        // `--as` renames before the review on both calls, so the review
        // shows the names that will be written; the accept call renames
        // from the same fetched body again at write time.
        let document = match &params.as_name {
            Some(name) => match crate::daemon::node_install::apply_as(&fetched.document, name) {
                Ok(document) => document,
                Err(message) => return Ok(error_result(&message)),
            },
            None => fetched.document.clone(),
        };
        let review = crate::domain::nodes::review::review_node(&document, &fetched.source);
        if !accepted {
            if let Some(id) = crate::daemon::graph_install::draft_id(&params.reference) {
                crate::daemon::draft_pending::store(
                    &id,
                    fetched.body.clone(),
                    fetched.source_url.clone(),
                );
            }
            return Ok(build_json_result(&serde_json::json!({
                "needs_review": true,
                "review": review,
            })));
        }

        // Only after the caller accepted does anything touch disk.
        let options = crate::daemon::node_install::NodeInstallOptions {
            source_ref: fetched.source_ref.clone(),
            local_name: params.as_name.clone(),
            replace: params.replace.unwrap_or(false),
            overwrite_presets: params.overwrite_presets.unwrap_or(false),
        };
        let original = fetched.document.clone();
        let written = tokio::task::spawn_blocking(move || {
            let canopy_dir = crate::domain::prompts::canopy_dir();
            crate::daemon::node_install::write_node_install(
                &original,
                &options,
                &crate::domain::nodes::nodes_dir(&canopy_dir),
                &crate::domain::prompts::prompts_dir(&canopy_dir),
            )
        })
        .await
        .map_err(|error| internal_error(error.to_string()))?;
        match written {
            Ok(presets_written) => Ok(build_json_result(&serde_json::json!({
                "name": document.name,
                "kind": document.kind,
                "ref": params.reference,
                "source_url": fetched.source_url,
                "source": fetched.source,
                "presets_written": presets_written,
                "review": review,
            }))),
            Err(message) => Ok(error_result(&message)),
        }
    }

    pub(crate) async fn node_remove_impl(
        &self,
        params: NodeRemoveParams,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name.trim();
        let dir = nodes::nodes_dir(&crate::domain::prompts::canopy_dir());
        match nodes::remove(&dir, name) {
            Ok(RemoveOutcome::Removed) => Ok(build_json_result(&serde_json::json!({
                "name": name,
                "removed": true,
            }))),
            Ok(RemoveOutcome::BuiltinProtected) => Ok(error_result(&format!(
                "Node '{name}' is a builtin and has no installed copy to remove, so it cannot be removed. Install your own copy with node install to override it; that installed copy can then be removed."
            ))),
            Err(message) => Ok(error_result(&message)),
        }
    }
}

/// The `node` `list` rows: name, kind, where the document came from, and
/// which catalogue entry installed it (`null` for builtins and local
/// documents).
fn node_rows(entries: &[nodes::NodeEntry]) -> Vec<serde_json::Value> {
    entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "name": entry.document.name,
                "kind": entry.document.kind,
                "source": entry.source.as_str(),
                "catalogue_source": entry.document.catalogue_source,
            })
        })
        .collect()
}

/// The nodes dir under the active canopy home.
pub(crate) fn nodes_dir() -> std::path::PathBuf {
    nodes::nodes_dir(&crate::domain::prompts::canopy_dir())
}

/// Resolve a `graph_node` call's kind/config from an explicit
/// `kind`+`config` pair. (A `node:` document name never reaches here —
/// `resolve_add_node_kind_config` routes it to [`resolve_node_document`].)
pub(crate) fn resolve_node_kind_and_config(
    kind: Option<&str>,
    config: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<(GraphNodeKind, serde_json::Value), String> {
    let explicit_kind = kind.map(str::trim).filter(|s| !s.is_empty());
    let node_kind = validate_node_kind(
        explicit_kind
            .ok_or_else(|| "Provide 'kind' when not using a node document.".to_string())?,
    )?;
    let config = config
        .map(serde_json::Value::Object)
        .ok_or_else(|| "Provide either 'config' or 'node'.".to_string())?;
    Ok((node_kind, config))
}

/// Resolve a `node:` name into the kind/config `graph_node` stores: the
/// installed copy if there is one, else the embedded builtin, with
/// `config_overrides` shallow-merged on top. An ensemble node document is
/// refused here — it belongs to `graph_ensemble`.
pub(crate) fn resolve_node_document(
    name: &str,
    config_overrides: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<(GraphNodeKind, serde_json::Value), String> {
    let dir = nodes_dir();
    let Some(entry) = nodes::resolve(&dir, name)? else {
        return Err(nodes::unknown_node_message(&dir, name)?);
    };
    if entry.document.kind == "ensemble" {
        return Err(format!(
            "Node document '{name}' is an ensemble; add it with graph_ensemble add (node: \"{name}\") instead of graph_node add."
        ));
    }
    let kind = validate_node_kind(&entry.document.kind)?;
    let overrides = config_overrides.map(serde_json::Value::Object);
    let merged = nodes::merge_config(&entry.document.config, overrides.as_ref());
    if kind == GraphNodeKind::Agent {
        validate_node_document_agent_harness(name, &merged)?;
    }
    Ok((kind, merged))
}

/// Resolve an `ensemble` node document by name for `graph_ensemble`.
/// Returns the document's name (for error messages) and its parsed config.
/// A single-node document is refused here — it belongs to `graph_node`.
pub(crate) fn resolve_ensemble_node_document(
    name: &str,
) -> Result<(String, crate::domain::nodes::EnsembleNodeConfig), String> {
    let dir = nodes_dir();
    let Some(entry) = nodes::resolve(&dir, name)? else {
        return Err(nodes::unknown_node_message(&dir, name)?);
    };
    if entry.document.kind != "ensemble" {
        return Err(format!(
            "Node document '{name}' is a {} node, not an ensemble; add it with graph_node (node: \"{name}\") instead of graph_ensemble.",
            entry.document.kind
        ));
    }
    let config = crate::domain::nodes::EnsembleNodeConfig::from_value(
        &entry.document.name,
        &entry.document.config,
    )?;
    config.validate(&entry.document.name)?;
    Ok((entry.document.name, config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    #[test]
    fn a_builtin_node_resolves_offline_with_no_installed_copy() {
        let home = tempfile::tempdir().unwrap();
        let _guard = OverrideHome::set(home.path());
        let (kind, config) = resolve_node_document(
            "cargo-gates",
            Some(
                [
                    ("timeout_seconds".to_string(), json!(600)),
                    ("platform".to_string(), json!("ignored-for-check")),
                ]
                .into_iter()
                .collect(),
            ),
        )
        .unwrap();
        assert_eq!(kind, GraphNodeKind::Check);
        assert_eq!(config["timeout_seconds"], 600);
        assert!(
            config["command"].as_str().unwrap().contains("cargo fmt"),
            "{config}"
        );
        assert_eq!(
            nodes::resolve(&nodes_dir(), "cargo-gates")
                .unwrap()
                .expect("resolves")
                .source,
            nodes::Source::Builtin
        );
    }

    #[test]
    fn an_installed_copy_wins_over_the_builtin() {
        let home = tempfile::tempdir().unwrap();
        let _guard = OverrideHome::set(home.path());
        let dir = nodes_dir();
        let mut document = nodes::builtin("implementer").unwrap();
        document.config = json!({"prompt_template": "my implementer"});
        nodes::write(&dir, &document).unwrap();

        let (kind, config) = resolve_node_document(
            "implementer",
            Some(
                [("platform".to_string(), json!("claude"))]
                    .into_iter()
                    .collect(),
            ),
        )
        .unwrap();
        assert_eq!(kind, GraphNodeKind::Agent);
        assert_eq!(config["prompt_template"], "my implementer");
        assert_eq!(config["platform"], "claude");
        assert_eq!(
            nodes::resolve(&nodes_dir(), "implementer")
                .unwrap()
                .expect("resolves")
                .source,
            nodes::Source::Installed
        );
    }

    #[test]
    fn an_agent_node_without_a_harness_override_is_refused_naming_the_document() {
        let home = tempfile::tempdir().unwrap();
        let _guard = OverrideHome::set(home.path());
        let error = resolve_node_document("implementer", None).unwrap_err();
        assert!(error.contains("implementer"), "{error}");
        assert!(error.contains("platform"), "{error}");
    }

    #[test]
    fn an_ensemble_node_document_is_refused_by_graph_add_node() {
        let home = tempfile::tempdir().unwrap();
        let _guard = OverrideHome::set(home.path());
        let error = resolve_node_document("ensemble-proposers", None).unwrap_err();
        assert!(error.contains("ensemble-proposers"), "{error}");
        assert!(error.contains("graph_ensemble"), "{error}");
    }

    #[test]
    fn a_single_node_document_is_refused_by_graph_add_ensemble() {
        let home = tempfile::tempdir().unwrap();
        let _guard = OverrideHome::set(home.path());
        let error = resolve_ensemble_node_document("implementer").unwrap_err();
        assert!(error.contains("implementer"), "{error}");
        assert!(error.contains("graph_node"), "{error}");
    }

    #[test]
    fn an_unknown_node_lists_the_available_names() {
        let home = tempfile::tempdir().unwrap();
        let _guard = OverrideHome::set(home.path());
        let error = resolve_node_document("nope", None).unwrap_err();
        assert!(error.contains("nope"), "{error}");
        assert!(error.contains("implementer"), "{error}");
        assert!(error.contains("ensemble-proposers"), "{error}");
    }
}
