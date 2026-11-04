//! MCP handler tools split out of `daemon::handler` (node tools and node-config validation).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_edges::resolve_graph_target;
use crate::daemon::handler_formatting::{
    build_id_result, build_node_update_response_with_changes, internal_error,
};
use crate::daemon::handler_nodes_sections::{
    audit_infra_override_nodes, audit_unknown_key_nodes, autowire_infra_edge, merge_node_config,
    next_node_position, parse_node_name_kind, record_node_config_change,
    resolve_add_node_kind_config, resolve_updatable_node, validate_effective_node_config,
};
use crate::daemon::handler_specs_helpers::validate_at_least_one_bool;
use crate::daemon::handler_templates::check_new_node;
use crate::daemon::handler_validate::{
    config_has_agent_harness, json_value_kind_name, parse_router_routes,
    reject_interactive_only_platform, validate_agent_crew_exclusive, validate_node_mutex_config,
    validate_node_position_conflict, validate_non_empty, validate_not_join_kind,
};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::domain::graphs::{validate_router_routes, GraphNode, GraphNodeKind};
use crate::or_early_return;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_nodes, vis = "pub(crate)")]
impl TaskTriggerHandler {
    pub(crate) async fn graph_add_node_impl(
        &self,
        params: GraphAddNodeParams,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name.trim();
        if let Err(e) = validate_non_empty(name, "Graph node name") {
            return Ok(error_result(&e));
        }
        let target = match resolve_graph_target(
            &self.db,
            params.spec_id.as_deref(),
            params.graph_id.as_deref(),
        ) {
            Ok(target) => target,
            Err(e) => return Ok(error_result(&e)),
        };

        let (kind, config) = match resolve_add_node_kind_config(
            params.node.as_deref(),
            params.kind.as_deref(),
            params.config,
            params.config_overrides,
        ) {
            Ok(result) => result,
            Err(e) => return Ok(e),
        };
        if let Err(e) = validate_not_join_kind(kind) {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_node_config(kind, &config) {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_node_mutex_config(name, &config) {
            return Ok(error_result(&e));
        }
        if let Some(msg) = reject_interactive_only_platform(&config) {
            return Ok(error_result(&msg));
        }
        if let Err(e) = check_new_node(&self.db, &target, kind, &config, name) {
            return Ok(error_result(&e));
        }

        let (spec_id, graph_id, next_position) = next_node_position(&self.db, &target)?;

        let node = GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id,
            graph_id: graph_id.clone(),
            name: name.to_string(),
            kind,
            config,
            position: next_position,
            created_at: chrono::Utc::now(),
        };
        self.db.insert_graph_node(&node).map_err(internal_error)?;

        autowire_infra_edge(&self.db, &node, &graph_id);

        Ok(build_id_result(&node.id, "node_id"))
    }

    pub(crate) async fn graph_update_node_impl(
        &self,
        params: GraphUpdateNodeParams,
    ) -> Result<CallToolResult, McpError> {
        let (node_id, node) = or_early_return!(resolve_updatable_node(&self.db, &params));
        let (name, kind) = match parse_node_name_kind(&params) {
            Ok(parsed) => parsed,
            Err(e) => return Ok(e),
        };

        if let Some(position) = params.position {
            if let Err(e) = validate_node_position_conflict(&self.db, &node, &node_id, position) {
                return Ok(error_result(&e));
            }
        }

        if let Err(e) = validate_at_least_one_bool(
            &[
                name.is_some(),
                kind.is_some(),
                params.config.is_some(),
                params.position.is_some(),
            ],
            "graph_node",
        ) {
            return Ok(error_result(&e));
        }

        let config_replace = params.config_replace.unwrap_or(false);
        let (effective_config, changed_keys) =
            merge_node_config(&node.config, params.config.as_ref(), config_replace);

        let () = or_early_return!(validate_effective_node_config(
            &self.db,
            &node,
            &node_id,
            kind,
            name,
            &effective_config,
            params.config.is_some()
        ));

        let config_to_store = if params.config.is_some() {
            Some(&effective_config)
        } else {
            None
        };

        self.db
            .update_graph_node_details(&node_id, name, kind, config_to_store, params.position)
            .map_err(internal_error)?;

        record_node_config_change(&self.db, &node, &node_id, name);

        Ok(build_node_update_response_with_changes(
            &node_id,
            &changed_keys,
        ))
    }

    pub(crate) async fn graph_audit_node_configs_impl(&self) -> Result<CallToolResult, McpError> {
        let nodes = self.db.list_all_graph_nodes().map_err(internal_error)?;

        let flagged = audit_unknown_key_nodes(&self.db, &nodes)?;

        let infra_overrides = audit_infra_override_nodes(&self.db, &nodes)?;

        let commit_rights_nodes: Vec<serde_json::Value> = nodes
            .iter()
            .filter(|n| n.config.get("commit_rights").and_then(|v| v.as_bool()) == Some(true))
            .map(|n| {
                serde_json::json!({
                    "node_id": n.id,
                    "name": n.name,
                    "spec_id": n.spec_id,
                    "graph_id": n.graph_id,
                })
            })
            .collect();
        let all_ensembles = self.db.list_all_ensembles().map_err(internal_error)?;
        let commit_rights_ensembles: Vec<serde_json::Value> = all_ensembles
            .iter()
            .filter(|e| e.ensemble.commit_rights)
            .map(|e| {
                serde_json::json!({
                    "ensemble_id": e.ensemble.id,
                    "name": e.ensemble.name,
                    "spec_id": e.ensemble.spec_id,
                    "graph_id": e.ensemble.graph_id,
                })
            })
            .collect();

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "flagged_nodes": flagged,
                "commit_rights_nodes": commit_rights_nodes,
                "commit_rights_ensembles": commit_rights_ensembles,
                "infra_config_overrides": infra_overrides,
            }))
            .unwrap_or_default(),
        )]))
    }
}

/// `graph_node`/`graph_node`'s interactive-only gate (CM35):
/// Every config key an `agent` node is actually read for — see
/// `execute_agent_node`/`resolve_node_prompt_template` in `graph_engine.rs`.
/// Notably absent: `prompt`. An agent's prompt is `prompt_template` (or
/// `prompt_preset`); `prompt` is a plain node never reads, and a node that
/// carries it silently runs on the bare fallback template instead — the
/// defect this allowlist exists to catch. `commit_rights` (B37) is accepted
/// on every kind since the engine checks it uniformly regardless of which
/// node in the graph actually moves HEAD. `require_report` (boolean, default
/// `false`) exists because a harness can exit 0 having done nothing — see
/// `agent_finished_execution` in `graph_engine.rs`. CM30:
/// `infra_retry_limit`/`infra_crash_max_seconds`/`infra_backoff_seconds`
/// override the infra-crash retry budget for this node alone (validated as
/// non-negative integers); absent, the platform's config.toml value applies,
/// then the engine default (2/60/30).
pub(crate) const AGENT_CONFIG_KEYS: &[&str] = &[
    "platform",
    "cli",
    "crew",
    "model",
    "effort",
    "timeout_minutes",
    "resume",
    "resume_prompt",
    "prompt_template",
    "prompt_preset",
    "require_report",
    "commit_rights",
    "skills",
    "infra_retry_limit",
    "infra_crash_max_seconds",
    "infra_backoff_seconds",
    "mutex",
];

/// Every config key a `check` node is read for — see `execute_check_node`.
pub(crate) const CHECK_CONFIG_KEYS: &[&str] = &[
    "command",
    "success_condition",
    "timeout_seconds",
    "commit_rights",
    "mutex",
];

/// Every config key a `gate` node is read for — see `execute_gate_node`.
pub(crate) const GATE_CONFIG_KEYS: &[&str] = &["evaluate", "value", "commit_rights", "mutex"];

/// Every config key a `router` node is read for — see `parse_router_routes`
/// and `parse_router_decision`.
pub(crate) const ROUTER_CONFIG_KEYS: &[&str] = &[
    "routes",
    "fallback",
    "platform",
    "cli",
    "model",
    "timeout_minutes",
    "prompt_template",
    "decision",
    "min_confidence",
    "commit_rights",
];

/// The config keys a node's kind is actually read for, or `None` for `Join`
/// (engine-managed — see `validate_node_config`'s `Join` arm — so there is
/// no caller-supplied key to check against). Shared by
/// [`validate_known_config_keys`] (write-time rejection) and
/// [`unknown_config_keys`] (read-only detection of already-stored nodes via
/// the `graph` tool), so the two can never name a
/// different accepted set for the same kind.
pub(crate) fn allowed_config_keys(kind: GraphNodeKind) -> Option<&'static [&'static str]> {
    match kind {
        GraphNodeKind::Agent => Some(AGENT_CONFIG_KEYS),
        GraphNodeKind::Check => Some(CHECK_CONFIG_KEYS),
        GraphNodeKind::Gate => Some(GATE_CONFIG_KEYS),
        GraphNodeKind::Router => Some(ROUTER_CONFIG_KEYS),
        GraphNodeKind::Join => None,
    }
}

/// Every key in `map` that `kind` will never read, sorted. Empty for `Join`
/// (see [`allowed_config_keys`]) and for a config that only carries
/// recognized keys. Pure read-only classification — no error message, no
/// early return — so it doubles as both [`validate_known_config_keys`]'s
/// rejection check and `graph`'s detection query over
/// nodes that predate this validation.
pub(crate) fn unknown_config_keys(
    kind: GraphNodeKind,
    map: &serde_json::Map<String, serde_json::Value>,
) -> Vec<String> {
    let Some(allowed) = allowed_config_keys(kind) else {
        return Vec::new();
    };
    let mut unknown: Vec<String> = map
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect();
    unknown.sort_unstable();
    unknown
}

/// Reject any config key a node's kind will never read. This is what makes
/// `{ "platform": "claude", "prompt": "..." }` on an agent node fail loudly
/// at write time instead of being accepted, stored, and silently run on the
/// bare fallback template (`resolve_node_prompt_template`'s default) — the
/// exact incident this check exists to prevent. `prompt` on an agent node
/// gets its own message naming `prompt_template` as the field that actually
/// gets read; every other unrecognized key gets the generic message naming
/// what the kind does accept.
pub(crate) fn validate_known_config_keys(
    kind: GraphNodeKind,
    map: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let unknown = unknown_config_keys(kind, map);
    let Some(bad_key) = unknown.first() else {
        return Ok(());
    };
    // `allowed_config_keys` is `None` only for `Join`, for which
    // `unknown_config_keys` always returns empty — so reaching here means
    // it returned `Some`.
    let allowed = allowed_config_keys(kind).expect("non-empty unknown keys implies Some(allowed)");
    let mut accepted = allowed.to_vec();
    accepted.sort_unstable();

    if kind == GraphNodeKind::Agent && bad_key == "prompt" {
        return Err(format!(
            "Graph node config for kind 'agent' does not read a 'prompt' key; use 'prompt_template' (or 'prompt_preset') instead. Accepted keys: {}.",
            accepted.join(", ")
        ));
    }
    Err(format!(
        "Graph node config for kind '{}' has unrecognized key '{bad_key}'. Accepted keys: {}.",
        kind.as_str(),
        accepted.join(", ")
    ))
}

/// Validate that a graph node's config is a JSON object with the fields its
/// kind needs at execution time. This exists because a double-encoded config
/// (e.g. `"{\"platform\": \"mimo\"}"` instead of `{"platform": "mimo"}`) used
/// to be accepted at creation time and only surfaced as an engine crash
/// ("Agent node ... is missing a platform/cli") mid-run, long after the node
/// was saved. It also rejects any key the node's kind will never read (see
/// [`validate_known_config_keys`]) — the same "surfaced at write time, not
/// run time" guarantee, for the case where the config shape is otherwise
/// valid but carries a key like `prompt` that a typo or a wrong mental model
/// (confusing it with the graph completion hook's own `prompt` field) put
/// there instead of `prompt_template`.
pub(crate) fn validate_node_config(
    kind: GraphNodeKind,
    config: &serde_json::Value,
) -> Result<(), String> {
    let Some(map) = config.as_object() else {
        return Err(format!(
            "Graph node config must be a JSON object, not {}. Pass an object (e.g. {{\"platform\": \"claude\"}}) rather than a JSON-encoded string.",
            json_value_kind_name(config)
        ));
    };

    validate_known_config_keys(kind, map)?;

    let has_non_empty_str = |field: &str| {
        map.get(field)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };

    match kind {
        GraphNodeKind::Agent => {
            if !config_has_agent_harness(map) {
                return Err(
                    "Graph node config for kind 'agent' must include a non-empty 'platform' (or 'cli' or 'crew') field.".to_string(),
                );
            }
            // CM49: `crew` names the harness the same way `platform` does —
            // pass one or the other, never both.
            validate_agent_crew_exclusive(map)?;
            for key in [
                "infra_retry_limit",
                "infra_crash_max_seconds",
                "infra_backoff_seconds",
            ] {
                if let Some(value) = map.get(key) {
                    if value.as_u64().is_none() {
                        return Err(format!(
                            "Graph node config field '{key}' must be a non-negative integer."
                        ));
                    }
                }
            }
        }
        GraphNodeKind::Check => {
            if !has_non_empty_str("command") {
                return Err(
                    "Graph node config for kind 'check' must include a non-empty 'command' field."
                        .to_string(),
                );
            }
        }
        GraphNodeKind::Gate => {
            let evaluate = map
                .get("evaluate")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("output_contains");
            if evaluate == "output_contains" && !has_non_empty_str("value") {
                return Err(
                    "Graph node config for kind 'gate' must include a non-empty 'value' field when 'evaluate' is 'output_contains'.".to_string(),
                );
            }
        }
        // A join node's config is engine-managed (see `graph_ensemble`) —
        // there is nothing for a caller to validate, and callers can never
        // reach this arm anyway since `graph_node`/`graph_node`
        // refuse `kind: "join"` outright.
        GraphNodeKind::Join => {}
        GraphNodeKind::Router => {
            let (routes, fallback) = parse_router_routes(config)?;
            validate_router_routes(&routes, &fallback)?;
            if let Some(value) = map.get("decision") {
                if value.as_bool().is_none() {
                    return Err("Graph node config field 'decision' must be a boolean.".to_string());
                }
            }
            if let Some(value) = map.get("min_confidence") {
                let n = value.as_f64().ok_or_else(|| {
                    "Graph node config field 'min_confidence' must be a number from 0 to 1."
                        .to_string()
                })?;
                if !(0.0..=1.0).contains(&n) || n.is_nan() {
                    return Err(
                        "Graph node config field 'min_confidence' must be a number from 0 to 1."
                            .to_string(),
                    );
                }
            }
            validate_router_decision_config(map)?;
        }
    }

    Ok(())
}

/// CM49: when a router node enables decision mode (`decision: true`), verify
/// that the named platform/cli entry in the local registry actually declares
/// a decision-model invocation — otherwise the router would silently fall
/// through to a non-decision spawn path mid-run.
fn validate_router_decision_config(
    map: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if map.get("decision").and_then(serde_json::Value::as_bool) != Some(true) {
        return Ok(());
    }
    let Some(platform) = map
        .get("platform")
        .or_else(|| map.get("cli"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(());
    };
    let Some(home) = std::env::var_os("CANOPY_HOME_OVERRIDE")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir)
    else {
        return Ok(());
    };
    let canopy_config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
    if let Some(cli_config) = canopy_config.get_cli(platform) {
        if cli_config.decision_args.is_none() {
            return Err(format!(
                "Router node uses decision mode but platform '{}' has no decision-model invocation in the registry.",
                platform
            ));
        }
    }
    Ok(())
}
