//! Node authoring helpers and node-catalogue MCP tools.
//!
//! A node document is the shareable alternative to a full `config`: the
//! catalogue tools here install/list/remove them, and `graph_add_node` /
//! `graph_add_ensemble` instantiate one by name. The resolvers live here
//! rather than in `handler_nodes` so the tool module and the node tools stay
//! one cohesive surface.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::{build_json_result, internal_error};
use crate::daemon::handler_validate::{validate_node_document_agent_harness, validate_node_kind};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::blueprints::{merge_blueprint_config, Blueprint};
use crate::domain::graphs::GraphNodeKind;
use crate::domain::nodes::{self, RemoveOutcome};
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_node_lib, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "node_list",
        description = "List every node document available to graph_add_node and graph_add_ensemble — the ones installed under ~/.canopy/nodes (source 'installed') and the builtins embedded in the binary (source 'builtin': implementer, cargo-gates, reviewer-committer, commit-check, resilience, ensemble-proposers). An installed copy of a builtin name shadows it and is listed once, as 'installed'. Install more with node_install."
    )]
    pub(crate) async fn node_list(&self) -> Result<CallToolResult, McpError> {
        let dir = nodes::nodes_dir(&crate::domain::prompts::canopy_dir());
        match nodes::list(&dir) {
            Ok(entries) => Ok(build_json_result(&serde_json::json!({
                "nodes": node_rows(&entries),
            }))),
            Err(message) => Ok(error_result(&message)),
        }
    }

    #[tool(
        name = "node_install",
        description = "Install a shareable node document from the catalogue in one command: fetch the published document for `ref` — `<slug>` for an official node or `<owner>/<repo>/<slug>` for a community one, each with an optional `@<ref>` version suffix, or `draft:<id>` for an unreviewed editor draft (26 lowercase base32 characters, served directly, installable once within 30 minutes) — through `graphs.catalogue_url`, so the catalogue counts the install, and validate it exactly as `graph_validate` would (a graph document is refused). BEFORE ANYTHING IS WRITTEN the call returns {needs_review: true, review: {source, commands, prompts, attention}}: every shell command (config.command, labelled by node), every agent instruction (prompt_template, prompt_override, bundled or builtin preset content — first 3 lines with a total line count), and the attention marks for dangerous command shapes (pipe to shell, sudo, rm -rf, chmod 777, git push/--force, sensitive-path write, eval, base64 -d, pipe to python/node). AN AGENT MUST SHOW THIS REVIEW TO ITS USER before calling again with accept: true; without accept nothing is installed, and official documents are reviewed the same as community ones. With accept: true the bundled `preset` is installed into the prompts dir (a local file with different content aborts the install unless `overwrite_presets` is true), the document is written to ~/.canopy/nodes/<name>.json where it shadows a builtin of the same name, and the response carries the same review plus the name, kind, source URL, and presets written; add it to a graph with graph_add_node (node: <name>) or graph_add_ensemble (node: <name>)."
    )]
    pub(crate) async fn node_install(
        &self,
        Parameters(params): Parameters<NodeInstallParams>,
    ) -> Result<CallToolResult, McpError> {
        // The fetch is blocking, so it runs off the async runtime. It writes
        // nothing: the review comes first. A draft is consumed by the review
        // fetch, so the accept call reuses the parked body — with no parked
        // body it re-fetches and the 404 says the draft is gone.
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

        let review = crate::domain::nodes::review::review_node(&fetched.document, &fetched.source);
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
        let overwrite = params.overwrite_presets.unwrap_or(false);
        let document = fetched.document.clone();
        let written = tokio::task::spawn_blocking(move || {
            let canopy_dir = crate::domain::prompts::canopy_dir();
            crate::daemon::node_install::write_node_install(
                &document,
                overwrite,
                &crate::domain::nodes::nodes_dir(&canopy_dir),
                &crate::domain::prompts::prompts_dir(&canopy_dir),
            )
        })
        .await
        .map_err(|error| internal_error(error.to_string()))?;
        match written {
            Ok(presets_written) => Ok(build_json_result(&serde_json::json!({
                "name": fetched.document.name,
                "kind": fetched.document.kind,
                "ref": params.reference,
                "source_url": fetched.source_url,
                "source": fetched.source,
                "presets_written": presets_written,
                "review": review,
            }))),
            Err(message) => Ok(error_result(&message)),
        }
    }

    #[tool(
        name = "node_remove",
        description = "Remove an installed node document (~/.canopy/nodes/<name>.json). A builtin cannot be removed: with no installed copy of its name this refuses and says so, while an installed copy that shadows a builtin is removed like any other override, leaving the builtin resolvable again."
    )]
    pub(crate) async fn node_remove(
        &self,
        Parameters(params): Parameters<NodeRemoveParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name.trim();
        let dir = nodes::nodes_dir(&crate::domain::prompts::canopy_dir());
        match nodes::remove(&dir, name) {
            Ok(RemoveOutcome::Removed) => Ok(build_json_result(&serde_json::json!({
                "name": name,
                "removed": true,
            }))),
            Ok(RemoveOutcome::BuiltinProtected) => Ok(error_result(&format!(
                "Node '{name}' is a builtin and has no installed copy to remove, so it cannot be removed. Install your own copy with node_install to override it; that installed copy can then be removed."
            ))),
            Err(message) => Ok(error_result(&message)),
        }
    }
}

/// The `node_list` rows: name, kind, and where the document came from.
fn node_rows(entries: &[nodes::NodeEntry]) -> Vec<serde_json::Value> {
    entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "name": entry.document.name,
                "kind": entry.document.kind,
                "source": entry.source.as_str(),
            })
        })
        .collect()
}

/// The nodes dir under the active canopy home.
pub(crate) fn nodes_dir() -> std::path::PathBuf {
    nodes::nodes_dir(&crate::domain::prompts::canopy_dir())
}

/// Look up a blueprint by name, or an actionable error listing every
/// available blueprint name (builtin and custom) so the caller can pick a
/// valid one without a separate `blueprint_list` round trip.
pub(crate) fn validate_blueprint_exists(db: &Database, name: &str) -> Result<Blueprint, String> {
    match db.get_blueprint_by_name(name).map_err(|e| e.to_string())? {
        Some(blueprint) => Ok(blueprint),
        None => {
            let available = db
                .list_blueprints()
                .map_err(|e| e.to_string())?
                .iter()
                .map(|b| b.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "Unknown blueprint '{name}'. Available blueprints: {available}."
            ))
        }
    }
}

/// Resolve a `graph_add_node` call's kind/config, either from an explicit
/// `kind`+`config` pair or from a named blueprint (optionally shallow-merged
/// with `config_overrides`). Exactly one of `config`/`blueprint`/`node` must
/// be usable — this is the "template as an alternative to a full config"
/// surface.
pub(crate) fn resolve_node_kind_and_config(
    db: &Database,
    kind: Option<&str>,
    config: Option<serde_json::Map<String, serde_json::Value>>,
    blueprint: Option<&str>,
    config_overrides: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<(GraphNodeKind, serde_json::Value), String> {
    let blueprint_name = blueprint.map(str::trim).filter(|s| !s.is_empty());
    let explicit_kind = kind.map(str::trim).filter(|s| !s.is_empty());

    match blueprint_name {
        Some(blueprint_name) => {
            let bp = validate_blueprint_exists(db, blueprint_name)?;
            let node_kind = match explicit_kind {
                Some(explicit) => validate_node_kind(explicit)?,
                None => bp.kind,
            };
            let overrides = config_overrides.map(serde_json::Value::Object);
            let merged = merge_blueprint_config(&bp.config, overrides.as_ref());
            // Blueprints intentionally carry no platform/cli/crew (see
            // `builtin_blueprint_specs`) — which harness runs a node is the
            // caller's decision, supplied here via `config_overrides`, never
            // a default this resolver falls back to. Catching the missing
            // case here (rather than only via `validate_node_config`'s
            // generic message) lets the error name the blueprint and say
            // *why* the field is missing instead of just that it is.
            if node_kind == GraphNodeKind::Agent {
                crate::daemon::handler_validate::validate_blueprint_agent_harness(
                    blueprint_name,
                    &merged,
                )?;
            }
            Ok((node_kind, merged))
        }
        None => {
            let node_kind = validate_node_kind(explicit_kind.ok_or_else(|| {
                "Provide 'kind' when not using a blueprint or node document.".to_string()
            })?)?;
            let config = config
                .map(serde_json::Value::Object)
                .ok_or_else(|| "Provide either 'config', 'blueprint', or 'node'.".to_string())?;
            Ok((node_kind, config))
        }
    }
}

/// Resolve a `node:` name into the kind/config `graph_add_node` stores: the
/// installed copy if there is one, else the embedded builtin, with
/// `config_overrides` shallow-merged on top. An ensemble node document is
/// refused here — it belongs to `graph_add_ensemble`.
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
            "Node document '{name}' is an ensemble; add it with graph_add_ensemble (node: \"{name}\") instead of graph_add_node."
        ));
    }
    let kind = validate_node_kind(&entry.document.kind)?;
    let overrides = config_overrides.map(serde_json::Value::Object);
    let merged = merge_blueprint_config(&entry.document.config, overrides.as_ref());
    if kind == GraphNodeKind::Agent {
        validate_node_document_agent_harness(name, &merged)?;
    }
    Ok((kind, merged))
}

/// Resolve an `ensemble` node document by name for `graph_add_ensemble`.
/// Returns the document's name (for error messages) and its parsed config.
/// A single-node document is refused here — it belongs to `graph_add_node`.
pub(crate) fn resolve_ensemble_node_document(
    name: &str,
) -> Result<(String, crate::domain::nodes::EnsembleNodeConfig), String> {
    let dir = nodes_dir();
    let Some(entry) = nodes::resolve(&dir, name)? else {
        return Err(nodes::unknown_node_message(&dir, name)?);
    };
    if entry.document.kind != "ensemble" {
        return Err(format!(
            "Node document '{name}' is a {} node, not an ensemble; add it with graph_add_node (node: \"{name}\") instead of graph_add_ensemble.",
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
        assert!(error.contains("graph_add_ensemble"), "{error}");
    }

    #[test]
    fn a_single_node_document_is_refused_by_graph_add_ensemble() {
        let home = tempfile::tempdir().unwrap();
        let _guard = OverrideHome::set(home.path());
        let error = resolve_ensemble_node_document("implementer").unwrap_err();
        assert!(error.contains("implementer"), "{error}");
        assert!(error.contains("graph_add_node"), "{error}");
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
