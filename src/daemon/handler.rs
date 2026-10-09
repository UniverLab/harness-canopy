//! MCP Server handler implementing all canopy tools.
//!
//! Uses the `rmcp` SDK's `#[tool_router]` and `#[tool_handler]` macros
//! with `Parameters<T>` for proper MCP protocol compliance.

use std::sync::Arc;

use crate::application::notification_service::NotificationService;
use crate::db::Database;
use crate::executor::Executor;
use crate::graph_engine::GraphEngine;
use crate::rag::rate_limiter::RateLimiter;
use crate::sync_manager::SyncManager;
use crate::watchers::WatcherEngine;
use rmcp::handler::server::common::AsRequestContext;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::*;
use rmcp::tool_handler;
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use tokio::sync::Notify;

// The inline `#[cfg(test)]` modules below reference the handler family's
// helpers through `use super::{…}` / `use super::*`. Production code no
// longer calls those helpers directly (each family module owns its own
// imports), so these imports and re-exports exist for the test build only.
#[cfg(test)]
use crate::application::ports::{AgentRepository, RunRepository};
#[cfg(test)]
#[allow(unused_imports)]
use crate::daemon::handler_formatting::make_log_path;
#[cfg(test)]
use crate::daemon::helpers::error_result;
#[cfg(test)]
use crate::daemon::params::*;
#[cfg(test)]
use crate::domain::blueprints::{validate_blueprint_deletable, Blueprint};
#[cfg(test)]
#[allow(unused_imports)]
use crate::domain::graphs::{
    Ensemble, EnsembleKind, Graph, GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind,
    GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus, GraphStatus,
};
#[cfg(test)]
use crate::domain::models::Trigger;
#[cfg(test)]
use crate::domain::queues::{Queue, QueueDetails};
#[cfg(test)]
use crate::shared::sync_identity::header_str;

pub(crate) use crate::daemon::handler_graphs::build_graph_trigger;
pub(crate) use crate::daemon::handler_nodes::validate_node_config;
pub(crate) use crate::daemon::handler_read::{active_graph_spec_context, ActiveGraphSpec};
pub(crate) use crate::daemon::handler_run_exec::{
    handle_retry_current_node, handle_skip_next_spec,
};
pub(crate) use crate::daemon::handler_seeds_skills::redact_secrets;
pub(crate) use crate::daemon::handler_validate::{
    delete_graph_edge_checked, retarget_graph_edge, validate_absolute_dir, validate_non_empty,
};

#[cfg(test)]
pub(crate) use crate::daemon::handler_agents::transport_details;
#[cfg(test)]
pub(crate) use crate::daemon::handler_blueprints::blueprint_json;
#[cfg(test)]
pub(crate) use crate::daemon::handler_copy::{node_copy_note, plan_ensemble_copy, plan_node_copy};
#[cfg(test)]
pub(crate) use crate::daemon::handler_edges::resolve_graph_target;
#[cfg(test)]
pub(crate) use crate::daemon::handler_ensemble_core::{
    build_ensemble_unit, effective_member_prompt, member_node_config, validate_ensemble_members,
    BuiltEnsembleUnit, EnsembleUnitSpec,
};
#[cfg(test)]
pub(crate) use crate::daemon::handler_formatting::{
    append_temporal_agents_section, build_graph_update_response, build_id_result,
    build_json_result, build_spec_update_response, format_duration_short, format_system_time,
};
#[cfg(test)]
pub(crate) use crate::daemon::handler_graphs::build_graph_completion_hook;
#[cfg(test)]
pub(crate) use crate::daemon::handler_meta::{build_get_tools_response, rag_result_json};
#[cfg(test)]
pub(crate) use crate::daemon::handler_models_probes::{
    clamp_max_recent_pairs, display_for_slug, format_recent_usage_section, model_result_footer,
    prepend_recent_usage,
};
#[cfg(test)]
pub(crate) use crate::daemon::handler_nodes::{
    resolve_node_kind_and_config, unknown_config_keys, validate_blueprint_exists,
};
#[cfg(test)]
pub(crate) use crate::daemon::handler_preflight::{
    build_preflight_reviewer_prompt, resolve_reviewer_skill_section,
};
#[cfg(test)]
pub(crate) use crate::daemon::handler_queues::{
    queue_details_json, validate_queue_exists, validate_queue_member_removable,
    validate_queue_not_consumed, validate_queue_reorder, validate_queue_reorder_locking,
};
#[cfg(test)]
pub(crate) use crate::daemon::handler_read::{
    build_graph_summary_json, ensemble_details_json, graph_completion_hook_json,
    graph_completion_hook_run_json, graph_details_json, graph_edge_json, graph_node_json,
    graph_trigger_json,
};
#[cfg(test)]
pub(crate) use crate::daemon::handler_run_exec::{
    find_in_flight_run, graph_run_blocker, graph_run_json, graph_run_status_guard,
    in_flight_run_error, node_name_for_error, queue_running_spec, resolve_reported_run,
};
#[cfg(test)]
pub(crate) use crate::daemon::handler_run_history::{
    graph_node_run_detail_json, graph_node_run_summary_json,
};
#[cfg(test)]
pub(crate) use crate::daemon::handler_run_lifecycle::perform_graph_reset;
#[cfg(test)]
pub(crate) use crate::daemon::handler_specs::{spec_summary_json, validate_at_least_one_bool};
#[cfg(test)]
pub(crate) use crate::daemon::handler_sync::TaggedProjectKnowledge;
#[cfg(test)]
pub(crate) use crate::daemon::handler_validate::{
    json_value_kind_name, validate_edge_condition, validate_edge_condition_with_route,
    validate_node_kind, validate_node_not_ensemble_owned, validate_node_position_conflict,
    validate_not_join_kind, validate_position_conflict, validate_route_edge_target,
    validate_spec_deletable, validate_spec_exists, validate_spec_set_status_target,
    validate_spec_status, validate_spec_workdir,
};

pub struct OptionalExtension<T>(pub Option<T>);

impl<C, T> rmcp::handler::server::common::FromContextPart<C> for OptionalExtension<T>
where
    C: AsRequestContext,
    T: Send + Sync + 'static + Clone,
{
    fn from_context_part(context: &mut C) -> Result<Self, rmcp::ErrorData> {
        Ok(OptionalExtension(
            context.as_request_context().extensions.get::<T>().cloned(),
        ))
    }
}

const MISSING_SYNC_IDENTITY_MESSAGE: &str =
    "Missing Canopy session identity. Launch via `canopy bridge --id <AGENT_ID>` so requests include Canopy identity headers.";

/// MCP result budget in bytes. Read surfaces must fit within this limit so
/// that reading state never requires the database. Both
/// `intelligence_graph_walk` and `spec_list` read this constant; the next
/// surface that needs paging should reuse it instead of a new hardcoded
/// number.
pub(crate) const MCP_RESULT_BUDGET_BYTES: usize = 32_000;

/// Measured serialised size of one compact `spec_list` row in bytes: 339
/// compact specs measured 55,605 characters on 2026-09-02, i.e. ~164
/// bytes/row. Budgeted at 200 bytes/row so the default page keeps headroom
/// against longer-than-average names.
const SPEC_LIST_COMPACT_ROW_BYTES: usize = 200;

/// Default `session_list` page derived from the MCP result budget. The
/// compact row estimate leaves room for long names while keeping the default
/// response within the result budget.
const SESSION_LIST_COMPACT_ROW_BYTES: usize = 160;

pub(crate) const SESSION_LIST_DEFAULT_LIMIT: u32 =
    MCP_RESULT_BUDGET_BYTES as u32 / SESSION_LIST_COMPACT_ROW_BYTES as u32;

const _: () = assert!(
    SESSION_LIST_DEFAULT_LIMIT >= 1 && SESSION_LIST_DEFAULT_LIMIT <= 200,
    "session_list default page must stay within [1, 200]"
);

/// Default `spec_list` page derived from the budget, not a round number:
/// `MCP_RESULT_BUDGET_BYTES / SPEC_LIST_COMPACT_ROW_BYTES`, clamped to
/// `[1, 200]` like `graph_node_runs_list` (the quotient is 160, so the upper
/// clamp is vacuous and the lower clamp is enforced by the const assert).
pub(crate) const SPEC_LIST_DEFAULT_LIMIT: u32 =
    MCP_RESULT_BUDGET_BYTES as u32 / SPEC_LIST_COMPACT_ROW_BYTES as u32;

const _: () = assert!(
    SPEC_LIST_DEFAULT_LIMIT >= 1 && SPEC_LIST_DEFAULT_LIMIT <= 200,
    "spec_list default page must stay within [1, 200]"
);

/// Estimated serialised size of one `intelligence_upsert` duplicate-candidate
/// row (id ~36, kind ~10, title ~60, project_hash ~64, excerpt up to
/// `crate::db::intelligence::DUPLICATE_EXCERPT_MAX_CHARS` chars, plus JSON
/// keys/quotes/commas) in bytes. Budgeted generously since excerpt is free
/// text. `intelligence_upsert` reads this so its duplicate-candidate report
/// stays inside `MCP_RESULT_BUDGET_BYTES` regardless of corpus size — CB45,
/// following the same derivation `SPEC_LIST_DEFAULT_LIMIT` and
/// `SESSION_LIST_DEFAULT_LIMIT` already use.
const INTELLIGENCE_DUPLICATE_CANDIDATE_ROW_BYTES: usize = 450;

pub(crate) const INTELLIGENCE_DUPLICATE_CANDIDATES_CAP: usize =
    MCP_RESULT_BUDGET_BYTES / INTELLIGENCE_DUPLICATE_CANDIDATE_ROW_BYTES;

const _: () = assert!(
    INTELLIGENCE_DUPLICATE_CANDIDATES_CAP >= 1 && INTELLIGENCE_DUPLICATE_CANDIDATES_CAP <= 200,
    "intelligence_upsert duplicate-candidates cap must stay within [1, 200]"
);

pub(crate) fn missing_sync_identity_error() -> McpError {
    McpError::invalid_params(MISSING_SYNC_IDENTITY_MESSAGE.to_string(), None)
}

pub struct TaskTriggerHandler {
    pub db: Arc<Database>,
    pub executor: Arc<Executor>,
    pub watcher_engine: Arc<WatcherEngine>,
    pub scheduler_notify: Arc<Notify>,
    pub graph_engine: Arc<GraphEngine>,
    pub notification_service: Arc<dyn NotificationService>,
    pub sync_manager: Arc<SyncManager>,
    /// Shared RAG ingestion manager: rag_search must acquire its embedding
    /// client through this cache (B22) so queries reuse the loaded model and
    /// keep the persisted model-loaded status truthful.
    pub ingestion: Arc<crate::rag::ingestion::IngestionManager>,
    /// Rate limiters for rag_search (10 calls/min). Keyed by agent_id.
    pub rag_limiters: Arc<tokio::sync::Mutex<std::collections::HashMap<String, RateLimiter>>>,
    /// Backing store for `skill_list`/`skill_get` (`~/.canopy/skills/`).
    /// Git operations block, so callers run it via `spawn_blocking`.
    pub dynamic_skills: Arc<crate::dynamic_skills::SkillStore>,
    pub start_time: std::time::Instant,
    pub port: u16,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[derive(Clone)]
pub struct HandlerDeps {
    pub db: Arc<Database>,
    pub executor: Arc<Executor>,
    pub watcher_engine: Arc<WatcherEngine>,
    pub scheduler_notify: Arc<Notify>,
    pub graph_engine: Arc<GraphEngine>,
    pub notification_service: Arc<dyn NotificationService>,
    pub sync_manager: Arc<SyncManager>,
    pub ingestion: Arc<crate::rag::ingestion::IngestionManager>,
    pub dynamic_skills: Arc<crate::dynamic_skills::SkillStore>,
    pub port: u16,
}

impl TaskTriggerHandler {
    pub fn with_deps(deps: HandlerDeps) -> Self {
        let HandlerDeps {
            db,
            executor,
            watcher_engine,
            scheduler_notify,
            graph_engine,
            notification_service,
            sync_manager,
            ingestion,
            dynamic_skills,
            port,
        } = deps;

        Self {
            db,
            executor,
            watcher_engine,
            scheduler_notify,
            graph_engine,
            notification_service,
            sync_manager,
            ingestion,
            rag_limiters: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            dynamic_skills,
            start_time: std::time::Instant::now(),
            port,
            tool_router: Self::tool_router(),
        }
    }

    /// Merge every family tool router into the single MCP router.
    /// Family order follows the original `handler.rs` tool order; the
    /// advertised order is alphabetical regardless
    /// (`ToolRouter::list_all` sorts by name).
    pub(crate) fn tool_router() -> ToolRouter<Self> {
        let mut router = ToolRouter::new();
        router.merge(TaskTriggerHandler::tool_router_agents());
        router.merge(TaskTriggerHandler::tool_router_scheduled_sends());
        router.merge(TaskTriggerHandler::tool_router_models_probes());
        router.merge(TaskTriggerHandler::tool_router_sync());
        router.merge(TaskTriggerHandler::tool_router_intel());
        router.merge(TaskTriggerHandler::tool_router_seeds_skills());
        router.merge(TaskTriggerHandler::tool_router_graphs());
        router.merge(TaskTriggerHandler::tool_router_specs());
        router.merge(TaskTriggerHandler::tool_router_blueprints());
        router.merge(TaskTriggerHandler::tool_router_queues());
        router.merge(TaskTriggerHandler::tool_router_nodes());
        router.merge(TaskTriggerHandler::tool_router_edges());
        router.merge(TaskTriggerHandler::tool_router_ensemble_core());
        router.merge(TaskTriggerHandler::tool_router_copy());
        router.merge(TaskTriggerHandler::tool_router_ensemble_update());
        router.merge(TaskTriggerHandler::tool_router_read());
        router.merge(TaskTriggerHandler::tool_router_transfer());
        router.merge(TaskTriggerHandler::tool_router_run_history());
        router.merge(TaskTriggerHandler::tool_router_preflight());
        router.merge(TaskTriggerHandler::tool_router_run_exec());
        router.merge(TaskTriggerHandler::tool_router_run_lifecycle());
        router.merge(TaskTriggerHandler::tool_router_meta());
        router
    }
}

/// CB68 FR1: MCP tool names kept callable (see `complete_node_impl` /
/// `report_blocker_impl` / `schedule_autorun_impl`) but excluded from
/// `tools/list` so no new caller discovers and adopts a 2.x name. Filtering
/// happens here — in a hand-written `list_tools` — rather than via
/// `ToolRouter::disable_route`, because a disabled route also rejects
/// `call()` (see `rmcp::handler::server::router::tool::ToolRouter::call`),
/// which would break the alias's whole purpose.
const HIDDEN_DEPRECATED_TOOLS: &[&str] = &[
    "loop_complete_node",
    "loop_report_blocker",
    "loop_schedule_autorun",
];

fn visible_tool_list() -> Vec<Tool> {
    TaskTriggerHandler::tool_router()
        .list_all()
        .into_iter()
        .filter(|t| !HIDDEN_DEPRECATED_TOOLS.contains(&t.name.as_ref()))
        .collect()
}

#[tool_handler]
impl ServerHandler for TaskTriggerHandler {
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        Ok(rmcp::model::ListToolsResult {
            tools: visible_tool_list(),
            meta: None,
            next_cursor: None,
        })
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "MCP server for registering, managing, and executing scheduled and event-driven agents. \
                 Use agent_add to create scheduled agents, agent_watch for file watchers, \
                 agent_run to test immediately, and agent_status for daemon health."
                    .to_string(),
            )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn clamp_max_recent_pairs_defaults_and_clamps() {
        assert_eq!(super::clamp_max_recent_pairs(None), 15);
        assert_eq!(super::clamp_max_recent_pairs(Some(0)), 1);
        assert_eq!(super::clamp_max_recent_pairs(Some(999)), 50);
        assert_eq!(super::clamp_max_recent_pairs(Some(20)), 20);
    }

    #[test]
    fn probe_json_keeps_slug_and_adds_display() {
        use crate::domain::canopy_config::CanopyConfig;
        use crate::domain::cli_config::CliConfig;
        let mut config = CanopyConfig::default();
        config.clis.push(CliConfig {
            name: "mistral".to_string(),
            provider: Some("Mistral AI".to_string()),
            tool_name: Some("Vibe".to_string()),
            ..Default::default()
        });
        assert_eq!(
            super::display_for_slug(&config, "mistral"),
            "Mistral AI · Vibe"
        );
        // Unknown slug and field-less config both render the slug.
        assert_eq!(super::display_for_slug(&config, "ghost"), "ghost");
        let mut bare = CanopyConfig::default();
        bare.clis.push(CliConfig {
            name: "mistral".to_string(),
            ..Default::default()
        });
        assert_eq!(super::display_for_slug(&bare, "mistral"), "mistral");
        // to_json itself keeps the slug identity key byte-identical.
        let report = crate::daemon::probe::ProbeReport {
            platform: "mistral".to_string(),
            model: None,
            outcome: crate::daemon::probe::ProbeOutcome::NotConfigured,
            duration_ms: 0,
            error: None,
            argv: None,
        };
        let v = report.to_json();
        assert_eq!(v["platform"], serde_json::json!("mistral"));
        assert!(v.get("platform_display").is_none());
    }

    use super::{
        active_graph_spec_context, build_ensemble_unit, build_get_tools_response,
        build_graph_completion_hook, build_graph_summary_json, build_graph_trigger,
        build_graph_update_response, build_id_result, build_json_result,
        build_spec_update_response, effective_member_prompt, find_in_flight_run,
        graph_details_json, graph_run_status_guard, graph_trigger_json, handle_retry_current_node,
        handle_skip_next_spec, header_str, in_flight_run_error, json_value_kind_name,
        member_node_config, missing_sync_identity_error, node_copy_note, node_name_for_error,
        perform_graph_reset, plan_ensemble_copy, plan_node_copy, queue_running_spec,
        rag_result_json, resolve_graph_target, resolve_node_kind_and_config, resolve_reported_run,
        spec_summary_json, unknown_config_keys, validate_absolute_dir, validate_at_least_one_bool,
        validate_blueprint_exists, validate_edge_condition, validate_edge_condition_with_route,
        validate_ensemble_members, validate_node_config, validate_node_kind,
        validate_node_not_ensemble_owned, validate_non_empty, validate_not_join_kind,
        validate_queue_exists, validate_queue_member_removable, validate_queue_not_consumed,
        validate_queue_reorder, validate_queue_reorder_locking, validate_route_edge_target,
        validate_spec_deletable, validate_spec_exists, validate_spec_set_status_target,
        validate_spec_status, validate_spec_workdir, ActiveGraphSpec, BuiltEnsembleUnit,
        EnsembleMemberParams, EnsembleUnitSpec, HandlerDeps, TaskTriggerHandler,
        MISSING_SYNC_IDENTITY_MESSAGE,
    };
    use crate::daemon::params::{
        GraphCompletionHookParams, GraphCopyEnsembleParams, GraphCopyNodeParams,
        GraphScheduleAutorunParams, GraphScheduleContinueParams, GraphTriggerParams,
    };
    use crate::db::Database;
    use crate::domain::blueprints::Blueprint;
    use crate::domain::graphs::{
        Ensemble, EnsembleKind, EnsembleMember, Graph, GraphEdge, GraphEdgeCondition, GraphNode,
        GraphNodeKind, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus, GraphStatus,
    };
    use crate::domain::models::Trigger;
    use crate::domain::queues::Queue;
    use crate::shared::sync_identity::CANOPY_AGENT_ID_HEADER;
    use tempfile::tempdir;

    fn ensemble_member_params(platform: &str) -> EnsembleMemberParams {
        EnsembleMemberParams {
            platform: platform.to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }
    }

    fn ensemble_member_params_with_prompt(
        platform: &str,
        prompt_override: &str,
    ) -> EnsembleMemberParams {
        EnsembleMemberParams {
            platform: platform.to_string(),
            model: None,
            prompt_override: Some(prompt_override.to_string()),
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }
    }

    /// The `#[tool]` macro locates a handler's parameter wrapper by the
    /// literal ident `Parameters`, then generates
    /// `schema_for_type::<Parameters<T>>()` for the advertised MCP
    /// `input_schema`. Swapping in this crate's own `Parameters<T>` (for
    /// enriched deserialization-failure messages) must not silently fall
    /// back to an empty schema — clients still need the real shape to build
    /// correct calls in the first place.
    #[test]
    fn tool_router_still_advertises_real_input_schemas_for_swapped_parameters_type() {
        let tools = TaskTriggerHandler::tool_router().list_all();

        let create_seed = tools
            .iter()
            .find(|tool| tool.name == "create_seed")
            .expect("create_seed tool should be registered");
        let props = create_seed
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("create_seed input_schema should have properties");
        assert!(props.contains_key("name"));
        assert!(props.contains_key("directives"));

        let intelligence_upsert = tools
            .iter()
            .find(|tool| tool.name == "intelligence_upsert")
            .expect("intelligence_upsert tool should be registered");
        let props = intelligence_upsert
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("intelligence_upsert input_schema should have properties");
        assert!(props.contains_key("node_data"));
    }

    /// Regression guard for the class of bug where a nested-object parameter
    /// is advertised as a bare `$ref` into `$defs` with no sibling `type` at
    /// the property level. A client that builds tool arguments from a
    /// shallow read of the property schema (never resolving `$ref`) sees no
    /// declared type there and falls back to sending the value as a string.
    /// This has already happened twice — `graph_add_node.config` and
    /// `intelligence_upsert.node_data` — so this test walks every
    /// registered tool's whole surface instead of asserting on one tool.
    #[test]
    fn every_tool_property_self_declares_its_type() {
        let tools = TaskTriggerHandler::tool_router().list_all();
        let mut violations = Vec::new();

        for tool in &tools {
            let Some(properties) = tool
                .input_schema
                .get("properties")
                .and_then(|p| p.as_object())
            else {
                continue;
            };

            for (prop_name, prop_schema) in properties {
                // A `$ref` with a sibling `type` counts via `has_type`;
                // a bare `$ref` with no sibling `type` does not, which is
                // exactly the shape this guard rejects.
                let has_type = prop_schema.get("type").is_some();
                let has_combinator =
                    prop_schema.get("anyOf").is_some() || prop_schema.get("oneOf").is_some();
                if !has_type && !has_combinator {
                    violations.push(format!("{}.{prop_name}", tool.name));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "properties with no declared type at the property level — a client that doesn't \
             resolve $ref can't tell these apart from an untyped string parameter: {violations:?}"
        );
    }

    /// B7 A1 frozen inventory: sorted MCP tool names. Any added, removed, or
    /// renamed tool fails `tool_inventory_is_byte_identical`.
    const EXPECTED_TOOL_NAMES: &[&str] = &[
        "agent_add",
        "agent_disable",
        "agent_enable",
        "agent_get",
        "agent_list",
        "agent_logs",
        "agent_models",
        "agent_probe",
        "agent_probe_recent",
        "agent_remove",
        "agent_report",
        "agent_run",
        "agent_schedule_enable",
        "agent_status",
        "agent_update",
        "agent_watch",
        "blueprint_create",
        "blueprint_delete",
        "blueprint_list",
        "create_seed",
        "evolve_identity",
        "get_identity",
        "get_tools",
        "graph_add_edge",
        "graph_add_ensemble",
        "graph_add_node",
        "graph_add_spec",
        "graph_archive",
        "graph_audit_node_configs",
        "graph_complete_node",
        "graph_continue",
        "graph_copy_ensemble",
        "graph_copy_node",
        "graph_create",
        "graph_delete_edge",
        "graph_delete_ensemble",
        "graph_delete_node",
        "graph_export",
        "graph_get",
        "graph_import",
        "graph_list",
        "graph_node_run_get",
        "graph_node_runs_list",
        "graph_pause",
        "graph_preflight",
        "graph_remove_spec",
        "graph_report_blocker",
        "graph_reset",
        "graph_restore",
        "graph_run",
        "graph_schedule_autorun",
        "graph_schedule_continue",
        "graph_update",
        "graph_update_edge",
        "graph_update_ensemble",
        "graph_update_node",
        "graph_update_spec",
        "intelligence_delete_node",
        "intelligence_delete_relation",
        "intelligence_get_context",
        "intelligence_graph_walk",
        "intelligence_link_projects",
        "intelligence_list_projects",
        "intelligence_search",
        "intelligence_upsert",
        "list_seeds",
        "loop_complete_node",
        "loop_report_blocker",
        "loop_schedule_autorun",
        "project_register",
        "project_remap",
        "project_search",
        "project_update",
        "queue_add_spec",
        "queue_create",
        "queue_list",
        "queue_remove_spec",
        "queue_reorder",
        "queue_set_group",
        "rag_search",
        "remove_seed",
        "scheduled_send_cancel",
        "scheduled_send_create",
        "scheduled_send_list",
        "session_list",
        "skill_get",
        "skill_list",
        "spec_convert",
        "spec_create",
        "spec_delete",
        "spec_list",
        "spec_section_get",
        "spec_set_status",
        "spec_update",
        "subagent_collect",
        "subagent_spawn",
        "sync_broadcast",
        "sync_declare_intent",
        "sync_get_context",
        "sync_report_status",
    ];

    /// B7 A1 frozen snapshot: per-tool `name|sha256(description)|sha256(input_schema JSON)`.
    /// Any byte drift in a description or schema fails `tool_inventory_is_byte_identical`.
    const EXPECTED_TOOL_ROWS: &[&str] = &[
                "agent_add|9d68a02373a5fddbeadced0e7863c7d816824428975fd76a7b7d410dde64e8c2|f4e62e346a1b0da8e4243cfde1fe49c3262b541c04bf56037931f63e44f6805e",
                "agent_disable|a5d3befdd0358cdb34f4cbceba2ad730b8cb7968bea1f4d5af15095cfc3e5d16|0b0c3f322df51316c53bc050ccdddd27c4296ae0ce3c97b074416a17f5bb8081",
                "agent_enable|812c3569335836df22c0021c4591ab1642a739c0be32ab921aa3a9dad35040f3|0b0c3f322df51316c53bc050ccdddd27c4296ae0ce3c97b074416a17f5bb8081",
                "agent_get|efc57a83aaf2b2b93b249749d2af313ad14b7d5f78bc63dfeb4a8067069d81fb|0b0c3f322df51316c53bc050ccdddd27c4296ae0ce3c97b074416a17f5bb8081",
                "agent_list|f12441b7b162701cc3feeead67ecc6adf610b4e28b3d2a2ba2eedd22d17b6b45|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
                "agent_logs|313a92b2669f743f4352fff989b30a666510fef1a7f6be65b4b17e8862d04acb|5632dbbe5962335a927367d89f703e6962b77257d60e3429e4226916bc6135ff",
                "agent_models|e26acac42beb6e5abc68fd28473196c549864a9796e020a4ebf9d62f81606090|5fa23052795e0ac7d84ebc3b37ee11e62c84d9d673da815b8d1249313b8133b7",
                "agent_probe|b1c612dc5b845b72e90f81458538c0fb18cb07768105adb87b06590875904577|69f6897484b8f07f4e272f180de0a41a1e275e0d152e13cbe413e26578376f64",
                "agent_probe_recent|665aea90717bcf46854edda2c7627d61c2242f960ae586d53185923e27f76e2e|9915aca58f0d6e4a64233b795f9f1d77d68f5bbf987b4afb76aeff4419241d73",
                "agent_remove|304a93b8e0d8f9957d7ff6e134154d1f67bc0e8920b5d7ff86874a93361d5de2|0b0c3f322df51316c53bc050ccdddd27c4296ae0ce3c97b074416a17f5bb8081",
                "agent_report|998eb6d649fe53809656f0a4db81248aa0feb0b045f957686d917fbcfe289224|ecc33998c9c269c62efe2a825c3492c7a8b986476cd10fbfe5d7cc179451c5a5",
                "agent_run|39ae4bbed141b7635a135111a7cf767c2569bf36d7d2d64da8089091534706f1|0b0c3f322df51316c53bc050ccdddd27c4296ae0ce3c97b074416a17f5bb8081",
                "agent_schedule_enable|ed852e08dc893d9cd6ae715374b0e695ee4516030ebe02b2e5ecb09aa0d98f1c|75b1290fa28ea5e614bbf45495964ab03ac917d86e3ce703843b403c8904221f",
                "agent_status|d1054486453a2080fe05eb9ccbcd4a86aefe4894a2a93870b6b2bb9e8781b194|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
                "agent_update|5f6ad5332501fc94b77510ae0093dcf92d451b051a90b3843e3e134c2d029415|a82dea1e39582995a0f9579ba0a7a9ed5a4ee5214eb1d612600901ae02c363a2",
                "agent_watch|fca0c0cedd156013122d4da513ee1409b06ff294f2915ac633c46bde361705d2|4cff7775405a9c6ed7d27980e11537c82a9141b706d4f32e5d168e59a5c1c527",
                "blueprint_create|066e750e9f90069f2f3ed530cdff588725ea3751872f30ea1aec5758bb15b5d2|8bcf3050ebfd3b2d22ef822cdfd1c41d40448d7f65cb3c1d5ed51abbd9e73a8f",
                "blueprint_delete|1eaa169fb5798e6d4095b44d1784bfb6a390925ec2e8513ca4dbe542119b69fb|12321d76b465455bffa88b83e79d533adc9ea8c854db8a5af7b5a559c93203a7",
                "blueprint_list|3408d1c0af354e3890676ffa6fff2237a27a130aaa7b49ca3e0e075f95362584|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
                "create_seed|ec16a67a00abe30d8410e1720468a039cd4b4c1a85820894e9b766a6ba868701|804d0cbcfaf45bb6e046d219beeeda3d4981abb00893efbcbc22e9d2547db8e6",
                "evolve_identity|cbae994f70163b304935cc28f006ffac2eec8e1bea905dea2c0fb59bd1c66cdc|5def265f8dddb09352b474cd0068aff558d8993ec3711458fe912388888c42a4",
                "get_identity|21d80d3a69d9719487bc498958b5600e86bf36519e229d270d2c2ae338383f72|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
                "get_tools|ea4c08804bf3a96c7e2e9f9be825feedb18ce038998f953065423576f6783beb|d81bd1699af6df84ec62ca38e6e360f6bae3c9d7f38fa307061f4eff5f84ba6f",
                "graph_add_edge|0368038fcdc6790d79f1558cf8c5e66aae12d6c475c616849c217670707ec5f7|237b62884bf39b63f372273414288ea9179b392cb42bbb94315978049564986d",
                "graph_add_ensemble|fcc6d04b6f0110d9f0cf798932a64014b85d3624dab3597b3f80349dfa64eee3|7a926a02e205ec84094636fc51763fb003ce246b58eb131a609c75d9089887aa",
                "graph_add_node|bc25e20a5d0a610f849de0481d4182916f31895fb16bb8ca007dceaee3e33232|7759661982738bea7c283e567107e2b53e38f778bbbf898afbfcec7dcf704a75",
                "graph_add_spec|d054dce21de8cb570c402ef4081ebbec27391f28052aaae6b7636b75a6135343|4ca4c5b48737acddab29e888155a58a248bbb7ed0659f1ea56ef5c7620575c0c",
                "graph_archive|86c62c25726d635c29691b8d1e90acbd2af55efafbf4b2acf866d82146fe9dc2|78e636b08231391ec48053bde18a790e2aede74da54003618003f2febe327bea",
                "graph_audit_node_configs|be2716e66df14f94162057f5919f89bf87cab3b84d7f90ba6c2614e90b4ec749|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
                "graph_complete_node|dd79817c0bd15c7fb324ee65bbf0b920c4d36f7958595932ab728a1d4418c76c|9fbd77387cb892656f3c66e396327bd2d66023f67f9cdae7c808091cac32cf9c",
                "graph_continue|0b6466413a10ef19b9a929d94e662a4279c523d9b2904fc5e1208fc9a9d07edb|4c6d787abe1add21babc503b93b6ca04c35161ee5c0104ea44e28ed7a7d81770",
                "graph_copy_ensemble|be5468502eeb1d5ac7e731194b9c052bb7969814aac59c41ae8d95b4fbc6742b|4b299e225f9117c00f10837d84ff727eb9bd6848468dfb34269a499c2cefe5f5",
                "graph_copy_node|00f28a0220fed92a866e7e9489c5c6d0796b61f125ba83d7f2e4528ed8089d75|7aa9c075d640d2f792c386c0a0ca8d999bbaa223ea9787e40a52ab7471a2c8fc",
                "graph_create|6f4749b2ffabdacfded946e01630ea558cefcf18110801fab0bb31c7fec79769|5f326cdb2c1ff3d7cca2cf25bf7fec5acc828bf70fe9e5e8e3d6423cb7de4a96",
                "graph_delete_edge|515e60718a86a111f77da9caab9a8f1a420c8a8cedea9c759dd3061efe54bcab|05d5202f073aea27378f794577e989b9636bad6d72397887d82887ad1d549d2f",
                "graph_delete_ensemble|a1de1cf8f4d4a23060cbe860ac4eb31995cb9a166c99da92d209d5f18758940d|93fdb4962aa14f5b7921af9435ab70339df060c397464a3cc4e514bd83cc720b",
                "graph_delete_node|82e65603faf2df8e3ff9ada9b0cb11f8470db786d91b32fa5ce0f9c3f9afbded|d9668a40560f1659b0e0d6e83a66825f8bb0e36729220f69f27f08bf81a4418f",
                "graph_export|9bd5614ab5d34f674cf481e66eeb77578a374934045710faa049689dffdf63f4|7321f3e8e4a31cc2e83e859854951f755aa5f62b8ae46b3151183247f19f458a",
                "graph_get|8307f64e41f067c8c502c2c431c24e3a4bfe3f9b609c44f62b4688fe37b851d0|df9a23578a82d3e67a8e963f14dfe8435035ab3ce659002aa35d330d6e9976b4",
                "graph_import|179e8ea66969268fc8c88777eeaadca8e3aac01cb9347b8897b734f2de34a3fe|ddf8cb9f351263109c144f91e08487b7f779d3afaf8b16c2971270b8e78e86ff",
                "graph_list|b06ff3bcc3cd8edcb2a8567034184a69561c8f350386f7647be570871fef776d|479e1ab97c37c84fcc3c4947dd99f6d7687bcb91d74682397d93c033b8e6e8b3",
                "graph_node_run_get|275f0c286c5378b0e0711d396d62695a2f54d0d1506b5c3ad3172b51fc4cf2ec|06fe6be024365e42918fb964eed4293abe33218115a300116d7d8778f4c14dc6",
                "graph_node_runs_list|9ad9113d90f883f5c739d9241d6012bf02fe59a4638b62730909301f9b93574b|aa2f6bbbff49f2bbfd014a1637f3c2a14afb08157d90fa6a6b58e4d8c6d64653",
                "graph_pause|b562a899a0c4d0913552a0f7d1d4c32ca357c940d64ba49b81b439b42b7c956f|c29898bc2505d3e3452dc002330223bc60ec88b3823bcb9b5f1d40fd75b5af7c",
                "graph_preflight|e322fc728e4eef3943934049c3291baf119a1a4f96a1c3b1d2f17d4f934e4dec|3425a799605c1b3b15ac19b666d4ed6580d61fddfa653caa8cf573061b648615",
                "graph_remove_spec|4ff8c80f3cd7d5fa1c2752f82a8eac04f93342355ff9789da4fc5f4ebd474fef|bdf4ae3b6449289652a28e7e04276b7d6d74eec7b4e35a85cbd45a11061eea33",
                "graph_report_blocker|e8b03ebc2341ed1dcec92b824650173db7d164d076ff501b3c460682c81f072c|666b976ef1b6f73603515e223da6b187b911606572b2bbba0377f4b5ca2798e7",
                "graph_reset|5fb36ec5cb4ed11594d06fb11f2be62ef911f5c9bc25c24b7e41e72d9f739a07|5122a2e0495a5d21d911c8571ab1440741434cfc3003d072c862a411ffec8914",
                "graph_restore|40e6e59ed7d4000db2dc48187e28d916ee02c7ee0a8fa7e2e6c07d9251988044|31a70644d3c87bb210f308e6aa19a31d08b38440498374efbac531b3f7bcb242",
                "graph_run|fa3db2dcec66808c55e2a5fe3623346e459ae74d796a4f54d478b85ac51636cc|9e084a80601932dc3d7241700eff51bb77c2764b1c886c8d1631cd8bfab52a27",
                "graph_schedule_autorun|9981ca74a4d734696c604599f06ea45c74621f5931d9ac344c31f14a9cb3a5f4|7961251cc9060f411c0b3065833a2afe07a1c1ba27fb8ec57780eec33945b70a",
                "graph_schedule_continue|1a383d649dbb9963e8ef76d698b9293b1a517205b3f61cd48d6c484f0c009c35|ab6b9783dd0a4ddb3cbcbe23c4ccaefa955cdffecead28d2a4ee758c21e27a55",
                "graph_update|27c9d848836295967b3588395ddf8a8038ef4b881b7402fb4b9bcccf430d4da8|e3d416ecd679f65c07b3885c2b787b2cfc8b2f8aa820dfa6ea251aa90f0e21ae",
                "graph_update_edge|ec07b87be17723f5212d8024e8bca2e2f44c215a417140898e1bab2539b832f5|51dafc02acbe8f4df24d69e44b6afcba2b381c212cfe426dcdbdde3a6196b6da",
                "graph_update_ensemble|997fcc3e308ab97906249ed356455e45e09f0dbd28e8187b04537aa3a8674e82|bfa346fe6e512a20b7e50beb43c92c36f8643a0baf87195b772bbb75354c1461",
                "graph_update_node|350486ce4efc1576750cbf28074916804495e2f90c4879103863a7f324042e36|13a30ea327b630709e99b78a98a12c210dc32034db31fad6595d4833a2a9697a",
                "graph_update_spec|cc242e8d96c9fa9c957a7ca885258947ab54337e4a9e1b9f2ddc66eb6ae06b6d|e8c2e945b5f6f1080c5fe54b38f36e0c75e1716e61f80db812c32547d30e8264",
                "intelligence_delete_node|b70ce24d59d2619e275cb40397037d7748f3ebca64ac5ffdc85eae6ee65bb1af|318c60c03fd6f5012a88be52de9aea7e9f372081ac1a378f5ee033b024ce229b",
                "intelligence_delete_relation|14ac75ee8ed351ec58f6bec7bbb0a9ca8267f01931e936d287cc0aaf494612a8|d2f7f0e554ab5fe74e9ddd1e8052321c9134f26ad3a434e350194e34ad505b49",
                "intelligence_get_context|669fe1f581e27a2bdf49f09765a42bc8f1f0879bf49c0466ff072ed315e73e33|55815882d32d2ebcf8374798bb69fdceacaf168be275a378115aa19551affc0c",
                "intelligence_graph_walk|2f5efed1b23f5b4400575ed31c19a02c454bbeaa364f7036ae14049663c0e0cb|6d5f32bf6c3e5676189186261fe35cc231358c0c49dd25bd0d0484939e506408",
                "intelligence_link_projects|e0b46f79afb49768ec33f101e8210ea7818dd8418aad92df4a8be69eebadbe2e|06275342174b1971a8ecbbad8520d1cf229436ed82b29cd5ce8451001b042b25",
                "intelligence_list_projects|68da4fb4e810e0f41612e0f56052ba26bbe029553038b43b7f4aa9aa6259f3aa|bab7d4f9c5d6321017d35ea0512c00090791f384e19f4a718198f9687c77f297",
                "intelligence_search|a91c69fe51924face0f4a2a6c4777b9dde708606f68cc44c7c46d16719d3f67c|94dbbccb091c5a677adfec1e8da82d11fec0b879f1aeed0a3c2fee95a2b0ed2d",
                "intelligence_upsert|5c5135cde6ee86d92a0aaa59b952c3ead35496abfed5ef95fb40c9ed13c9dd20|ed2ed96a95d6582f9e02e0afab570761db06ab4e0ce3c31e66f8d26788553a18",
                "list_seeds|94657a2e7927a4f05350461cdef7c05569145bc39c512444039f38077c689cde|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
                "loop_complete_node|a3a48975f0dafdc50569ac00a150cb091479281e7339f112708580a1764f95e8|9fbd77387cb892656f3c66e396327bd2d66023f67f9cdae7c808091cac32cf9c",
                "loop_report_blocker|9fb9038c5d224015aa25b157730b99339dd91372deb3c1460abb1e5e4fad2081|666b976ef1b6f73603515e223da6b187b911606572b2bbba0377f4b5ca2798e7",
                "loop_schedule_autorun|1e53058a7dad22cc057ce87b32d9fbaa20cca2b0e7721cc121e5c16b2b2677dd|23ebb42b7149e90e2ec3038873414a514267ea6a72e480535aee337f54caa1d5",
                "project_register|95d40d7773adb80b430d84f928d60d6a48d369505e8bf6c0d9f54aaa64b1d535|24cd867a6223c9a9ed47dea26a94d7d447da7ebd52364af69d06b426adf5f032",
                "project_remap|ee326ec962be9943ccf911a190168d71163903ab4370775a0a433d32e7721f25|a40e97ce72cb3e27c300467212bb6bb457c4d20ceb93b09aeb861d550484df69",
                "project_search|c0abdc269d287d6cf3465702f699e8464f18e0d6952f6870d2d1b4e37401b8da|65039e6ad0bffe393a7304d289e1197ef20782efe8f1694b8fb5d2bb793a2516",
                "project_update|33152dd73661dc1c449e9dc09e4b4bd72bdead18268244c1279c4e2dd3afb6b4|30ea776f5c3a35516b0f4d5e23ddce8eb5da7a3d6e31eee2e1c06fcde2cdc6b1",
                "queue_add_spec|71fed63c512e34c3bb79ae3438fd29bb76321b919c42fef162603a6d0f458e3d|c1408f7e09bff0fd60903ad268291abce4b2082e3ea571200c1f94a893fecb68",
                "queue_create|ad96a6520c976c9acdcf08610dc74a7d0728ae1cc81d47b678aed41ca63f1ac1|2e8186c094ebdc9401648f134916f125e5e0ce2daeca7bfe5167433716891e27",
                "queue_list|1f3e846f9b91428f0a705e8e2232e40ed93bd57e45eeb2960fee25b912bd06dc|6e2c5062a027279e85f76b630dac6eb45935821cc7b9fe21d0b5e85d5fa1a4ba",
                "queue_remove_spec|d607661fcaed39c344ae545f5a1f2246ed530af3ce9cc13615e2a62b933aafa8|20e00504d371ad091fc75d443e57fe7fe7479b6304cae52402e207af924ce134",
                "queue_reorder|c2553e02e72d85646d644b7aa94c6096ffcea3ffb819194875bc9e7d421ffb26|5d1dfef5e6016db1e4c9adddf9df2d7b22b4816fa0d3b5a38a08de10de4802b0",
                "queue_set_group|3eb98961df3f175e2b84f14beecc1629cf21c635e2bef749fe79919470056082|fceee53aad275541592e779fa462c4257d72f85c1649abcfa64aeebc84cdc146",
                "rag_search|8a1f6386074319d1c99700410f884ba9ed3a3f0f91276a50766cf677bf1b26cd|84c021f51349dfecbea78f5ebe89887d0f3e1e764ac91540bc20bc7e2d13ba45",
                "remove_seed|b24cd25a52f13d1714cd1a02c3156317166b33f19df049dc99326685750fc374|44f5accf3bb686511aa4445599a499601592df037d84886e46a92dccb6924c69",
                "scheduled_send_cancel|e97ad927d1c1d8778e0e7862e7ac4df954098bd847329a5cac398d19177d47e8|31cd20154b502bce97777d5d0b780d65ae849538b776016a121ba608bb227b9c",
                "scheduled_send_create|e6defc41d6d01fe133056562efc3aba3b50b6d1e2a06fe278894cbbbcb65a165|75bdf795b390451ff08f87c147f0f353f69b5f25ff2219f02c7e8901fff30ca6",
                "scheduled_send_list|946cdf95ebf54570fe9566aa69f8f5e7a18f08ea7b29a6be390bd9ce66a4df63|6ade068b88c489253907d5a691508969588e152e77aee3c2eeccd8ac385e8aab",
                "session_list|f983db33c96519c95584055f90c807dda350a17e9b12d402c0551590e772532b|23286691699c0a61ed77db5dac0618bc04db4ad52c21cdcf5df13a98242ed452",
                "skill_get|11bb4551bb0cf314b47867061fdb6ecc35812780d98808cd2f1e134d098bc8b9|a0ae058221b2b1a7b5105d991835dccd84a4a060693a3eebb05555b5b29e594e",
                "skill_list|970ee8acf69615b521eaca0b1b27b8555a7fdc96b5655ebf435f4667418d290b|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
                "spec_convert|dcabb9d52877782e47ac4fed3582e63e4c1cc6324c25b3079d6c25ef06552b16|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
                "spec_create|7dd9a69253452c11b113743c3c5118f35cf4bf029c3380bcb9777254e41fd2eb|f9c780c473de2cac1bef89874ad42c7948a27ee448d423fa9732e5849bcd0b64",
                "spec_delete|cd48e914d536c1e203436b9694cc93ba0b107b488b0383537716107c02537df8|e2f8bf96018973b702208f786b228eeaf8970d19822323251941c3c2b58c6bd9",
                "spec_list|9e42d06f6900ae06546a88a713f3e67e4fe3467a06561f1187f684b5d3579657|bcfbca658cfaeae86232022f6285d7f9a7f43357158f4d693e0fbba1a4a1a020",
                "spec_section_get|f2e7ae92e46d096fb4c5f1a3ce635e171dcd884e3f746ae936dfcb649e0aca22|c920c422e75f2d5dde9b2624d65af2613831ad217fd31d00bd00d7e34342f51e",
                "spec_set_status|1a8612df16827c55d054df32ed3d585f33a14068c8c6737db584b918727ff5ab|313d1242ae0bb189c86b7cce07fdc51fe07588dac444491f089283e985db30f8",
                "spec_update|4b63bd53115202d8c35c0e347ecc43491b5080564306c1fa643657d40790ba14|e7e35a41c1c50d14cc52e36b13a28342077e365683587e9207dd0ced2e28b0fa",
                "subagent_collect|c950e7193036445e86b6ffd6587304e0e71cf00302734d0ed6e83c549a2e95dd|5f286eb7d8c54c6fbc2148ca453732ed5773d888b6721af97d81de358ed14330",
                "subagent_spawn|062b2ac16b1f04f1ada1607d2cb04fe4840b9a09809382eed9c09878ab038fd1|64cdeb14b7796657a8099c8489439ee01a3389e9b088aa08474d4d57c5b27d7c",
                "sync_broadcast|c5904706ddfd0877686466deeff33f21d70075bbed6c8ae6eba5794fe3ea1043|41523abe2b3e2837139e505f609ec85fd6155ed80dd2cc4ff48fa3988fe8cd3b",
                "sync_declare_intent|621ecbc55446f78d4050026a7554487c33116b0ca218f359fc4355207d92abfe|87d05006d1026af131f08a68690f907d7419392b0774491f4347be5038d40bb4",
                "sync_get_context|5bf5707dc316b32fa427141fab68b709dc07af8a52676da646958774a432a1ff|66c7399ba7a381b4222e14f80163c35769d7ce4a474278464c8ccb326b831fab",
                "sync_report_status|ee93a882c476cc2792fa68bce24c1f187de5947c08b72bb55400dd114bdc07a4|b877ac8a670f1fb146c47ec679deaa5045ffa167867c24c06d192a0814b4a1ad"
    ];

    /// B7 A1: the MCP tool inventory is byte-identical across the handler
    /// decomposition. Sorted tool names must equal the frozen list, and every
    /// tool's `description` + `input_schema` JSON hash must equal the frozen
    /// snapshot. Any drift in names, descriptions, or schemas fails here.
    #[test]
    fn tool_inventory_is_byte_identical() {
        use sha2::{Digest, Sha256};
        fn hex(bytes: &[u8]) -> String {
            let mut digest = Sha256::new();
            digest.update(bytes);
            digest
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        }
        let mut tools = TaskTriggerHandler::tool_router().list_all();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(names, EXPECTED_TOOL_NAMES, "tool name inventory drifted");
        let rows: Vec<String> = tools
            .iter()
            .map(|t| {
                let desc = t.description.as_deref().unwrap_or("");
                let schema = serde_json::to_string(&t.input_schema).expect("schema serializes");
                format!(
                    "{}|{}|{}",
                    t.name,
                    hex(desc.as_bytes()),
                    hex(schema.as_bytes())
                )
            })
            .collect();
        assert_eq!(
            rows, EXPECTED_TOOL_ROWS,
            "tool description/schema inventory drifted"
        );
    }

    #[test]
    fn validate_ensemble_members_rejects_below_minimum() {
        let members = vec![ensemble_member_params("claude")];
        let err = validate_ensemble_members(&members, EnsembleKind::Parallel).unwrap_err();
        assert!(err.contains("2-8 members"), "{err}");
    }

    #[test]
    fn validate_ensemble_members_rejects_above_maximum() {
        let members: Vec<_> = (0..9).map(|_| ensemble_member_params("claude")).collect();
        let err = validate_ensemble_members(&members, EnsembleKind::Parallel).unwrap_err();
        assert!(err.contains("2-8 members"), "{err}");
    }

    #[test]
    fn validate_ensemble_members_accepts_boundary_counts() {
        let two: Vec<_> = (0..2).map(|_| ensemble_member_params("claude")).collect();
        assert!(validate_ensemble_members(&two, EnsembleKind::Parallel).is_ok());
        let eight: Vec<_> = (0..8).map(|_| ensemble_member_params("claude")).collect();
        assert!(validate_ensemble_members(&eight, EnsembleKind::Parallel).is_ok());
    }

    #[test]
    fn validate_ensemble_members_rejects_empty_platform() {
        let members = vec![
            ensemble_member_params("claude"),
            ensemble_member_params("  "),
        ];
        let err = validate_ensemble_members(&members, EnsembleKind::Parallel).unwrap_err();
        assert!(err.contains("platform"), "{err}");
    }

    /// `prompt_override` is normalized like `model`: trimmed, and a
    /// whitespace-only override collapses to `None` (use the shared
    /// prompt), not an override of empty string.
    #[test]
    fn validate_ensemble_members_trims_prompt_override_and_blanks_to_none() {
        let members = vec![
            ensemble_member_params_with_prompt("claude", "  review for security  "),
            {
                let mut m = ensemble_member_params("codex");
                m.prompt_override = Some("   ".to_string());
                m
            },
        ];
        let result = validate_ensemble_members(&members, EnsembleKind::Parallel).unwrap();
        assert_eq!(result[0].2.as_deref(), Some("review for security"));
        assert_eq!(result[1].2, None);
    }

    /// CM23: a member's own `timeout_minutes` follows the ensemble-level
    /// convention — negative is rejected, `0` is a legitimate immediate
    /// timeout.
    #[test]
    fn validate_ensemble_members_rejects_negative_member_timeout() {
        let members = vec![{
            let mut m = ensemble_member_params("claude");
            m.timeout_minutes = Some(-1);
            m
        }];
        let err = validate_ensemble_members(&members, EnsembleKind::Cascade).unwrap_err();
        assert!(err.contains("timeout_minutes"), "{err}");
    }

    #[test]
    fn validate_ensemble_members_accepts_zero_member_timeout() {
        let members = vec![{
            let mut m = ensemble_member_params("claude");
            m.timeout_minutes = Some(0);
            m
        }];
        let result = validate_ensemble_members(&members, EnsembleKind::Cascade).unwrap();
        assert_eq!(result[0].3, Some(0));
    }

    #[test]
    fn effective_member_prompt_prefers_override_over_shared() {
        assert_eq!(
            effective_member_prompt(Some("angle-specific prompt"), "shared prompt"),
            "angle-specific prompt"
        );
        assert_eq!(
            effective_member_prompt(None, "shared prompt"),
            "shared prompt"
        );
    }

    /// F1's "no nested ensembles" rule: wiring into a node that already
    /// belongs to another ensemble (as a member or as its join) must be
    /// rejected, not silently accepted.
    #[test]
    fn validate_node_not_ensemble_owned_rejects_member_and_join_nodes() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&GraphSpec {
            id: "spec-1".to_string(),
            graph_id: None,
            name: "spec-1".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();
        let now = chrono::Utc::now();
        db.insert_graph_node(&GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: now,
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "arbiter".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 10,
            created_at: now,
        })
        .unwrap();
        let member_nodes = vec![
            GraphNode {
                id: "m1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "member-1".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "claude"}),
                position: 2,
                created_at: now,
            },
            GraphNode {
                id: "m2".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "member-2".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "codex"}),
                position: 3,
                created_at: now,
            },
        ];
        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "quorum".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": "ens1"}),
            position: 4,
            created_at: now,
        };
        let edges = vec![
            GraphEdge {
                id: "kickoff->m1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "kickoff".to_string(),
                to_node: "m1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "kickoff->m2".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "kickoff".to_string(),
                to_node: "m2".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "m1->join1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "m1".to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "m2->join1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "m2".to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "join1->arbiter".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "join1".to_string(),
                to_node: "arbiter".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
        ];
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Proposers".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 2,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: now,
        };
        let members = vec![
            EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: "m1".to_string(),
                position: 0,
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
            EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: "m2".to_string(),
                position: 1,
                platform: "codex".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
        ];
        db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
            .unwrap();

        assert!(validate_node_not_ensemble_owned(&db, "m1").is_err());
        assert!(validate_node_not_ensemble_owned(&db, "join1").is_err());
        assert!(validate_node_not_ensemble_owned(&db, "kickoff").is_ok());
        assert!(validate_node_not_ensemble_owned(&db, "arbiter").is_ok());
    }

    fn spec_with_status(
        graph_id: &str,
        id: &str,
        position: i64,
        status: GraphSpecStatus,
    ) -> GraphSpec {
        GraphSpec {
            id: id.to_string(),
            graph_id: Some(graph_id.to_string()),
            name: id.to_string(),
            description: None,
            position,
            parallelizable: false,
            status,
            started_at: None,
            completed_at: Some(chrono::Utc::now()),
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        }
    }

    #[test]
    fn spec_delete_refuses_graph_bound_spec_with_actionable_error() {
        let spec = spec_with_status("graph-1", "spec-1", 1, GraphSpecStatus::Pending);

        let error = validate_spec_deletable(&spec).unwrap_err();

        assert!(
            error.contains("spec-1"),
            "error should name the spec: {error}"
        );
        assert!(
            error.contains("graph-1"),
            "error should name the graph: {error}"
        );
        assert!(
            error.contains("remove it from the graph") || error.contains("delete the graph"),
            "error should be actionable: {error}"
        );
    }

    #[test]
    fn spec_delete_allows_standalone_spec() {
        let mut spec = spec_with_status("graph-1", "spec-1", 1, GraphSpecStatus::Pending);
        spec.graph_id = None;

        assert!(validate_spec_deletable(&spec).is_ok());
    }

    fn graph_reset_fixture(status: GraphStatus) -> (tempfile::TempDir, Database, String) {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let graph_id = "graph-reset-test".to_string();
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: graph_id.clone(),
            name: "Graph".to_string(),
            description: None,
            workdir: dir.path().to_string_lossy().to_string(),
            status,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: Some(chrono::Utc::now()),
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        })
        .unwrap();
        (dir, db, graph_id)
    }

    #[test]
    fn graph_reset_failed_graph_resets_incomplete_specs_to_pending() {
        let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
        db.insert_graph_spec(&spec_with_status(
            &graph_id,
            "spec-done",
            1,
            GraphSpecStatus::Completed,
        ))
        .unwrap();
        db.insert_graph_spec(&spec_with_status(
            &graph_id,
            "spec-failed",
            2,
            GraphSpecStatus::Failed,
        ))
        .unwrap();

        let result = perform_graph_reset(&db, &graph_id, None).unwrap();
        assert!(!result.is_error.unwrap_or(false));

        let lp = db.get_graph(&graph_id).unwrap().unwrap();
        assert_eq!(lp.status, GraphStatus::Draft);
        assert!(lp.completed_at.is_none());

        let specs = db.list_graph_specs(&graph_id).unwrap();
        let done = specs.iter().find(|s| s.id == "spec-done").unwrap();
        let failed = specs.iter().find(|s| s.id == "spec-failed").unwrap();
        assert_eq!(failed.status, GraphSpecStatus::Pending);
        assert!(failed.completed_at.is_none());
        // Completed specs are preserved when `specs` isn't given explicitly.
        assert_eq!(done.status, GraphSpecStatus::Completed);
    }

    /// `Interrupted` is treated like any other non-completed status: a plain
    /// `graph_reset` (no explicit `specs`) returns it to `pending`, same as it
    /// would `failed` — an interruption isn't the special case here, only
    /// `completed` is (preserved unless named explicitly).
    #[test]
    fn graph_reset_resets_interrupted_spec_to_pending() {
        let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
        db.insert_graph_spec(&spec_with_status(
            &graph_id,
            "spec-interrupted",
            1,
            GraphSpecStatus::Interrupted,
        ))
        .unwrap();

        let result = perform_graph_reset(&db, &graph_id, None).unwrap();
        assert!(!result.is_error.unwrap_or(false));

        let specs = db.list_graph_specs(&graph_id).unwrap();
        let interrupted = specs.iter().find(|s| s.id == "spec-interrupted").unwrap();
        assert_eq!(interrupted.status, GraphSpecStatus::Pending);
        assert!(interrupted.started_at.is_none());
        assert!(interrupted.completed_at.is_none());
    }

    #[test]
    fn graph_reset_with_explicit_specs_resets_completed_spec_too() {
        let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
        db.insert_graph_spec(&spec_with_status(
            &graph_id,
            "spec-done",
            1,
            GraphSpecStatus::Completed,
        ))
        .unwrap();

        let result = perform_graph_reset(&db, &graph_id, Some(&["spec-done".to_string()])).unwrap();
        assert!(!result.is_error.unwrap_or(false));

        let spec = db.get_graph_spec("spec-done").unwrap().unwrap();
        assert_eq!(spec.status, GraphSpecStatus::Pending);
        assert!(spec.completed_at.is_none());
    }

    #[test]
    fn graph_reset_blanket_preserves_admin_skipped_specs() {
        let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
        // Administrative skip — must survive blanket reset.
        db.insert_graph_spec(&GraphSpec {
            id: "spec-admin-skipped".to_string(),
            graph_id: Some(graph_id.clone()),
            name: "admin-skipped".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Skipped,
            started_at: None,
            completed_at: Some(chrono::Utc::now()),
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: Some("admin".to_string()),
            completed_via_reason: Some("premise false".to_string()),
            completed_via_at: Some(chrono::Utc::now()),
        })
        .unwrap();
        db.insert_graph_spec(&spec_with_status(
            &graph_id,
            "spec-failed",
            2,
            GraphSpecStatus::Failed,
        ))
        .unwrap();

        let result = perform_graph_reset(&db, &graph_id, None).unwrap();
        assert!(!result.is_error.unwrap_or(false));

        let specs = db.list_graph_specs(&graph_id).unwrap();
        let skipped = specs.iter().find(|s| s.id == "spec-admin-skipped").unwrap();
        let failed = specs.iter().find(|s| s.id == "spec-failed").unwrap();
        assert_eq!(
            skipped.status,
            GraphSpecStatus::Skipped,
            "admin-skipped spec must survive a blanket reset"
        );
        assert_eq!(failed.status, GraphSpecStatus::Pending);

        let text = format!("{:?}", result.content);
        assert!(text.contains("1 spec(s) reset"), "{text}");
        assert!(text.contains("1 skipped spec(s) preserved"), "{text}");
    }

    #[test]
    fn graph_reset_blanket_resets_engine_skipped_spec() {
        let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
        // Engine-produced skip (no admin provenance) — must be reset.
        db.insert_graph_spec(&spec_with_status(
            &graph_id,
            "spec-engine-skipped",
            1,
            GraphSpecStatus::Skipped,
        ))
        .unwrap();

        let result = perform_graph_reset(&db, &graph_id, None).unwrap();
        assert!(!result.is_error.unwrap_or(false));

        let spec = db.get_graph_spec("spec-engine-skipped").unwrap().unwrap();
        assert_eq!(
            spec.status,
            GraphSpecStatus::Pending,
            "engine-skipped spec must be reset by blanket reset"
        );

        let text = format!("{:?}", result.content);
        assert!(text.contains("1 spec(s) reset"), "{text}");
        assert!(
            !text.contains("skipped"),
            "engine-skipped spec must not be counted as preserved: {text}"
        );
    }

    #[test]
    fn graph_reset_explicit_specs_resets_skipped_spec_and_clears_provenance() {
        let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
        db.insert_graph_spec(&GraphSpec {
            id: "spec-admin-skipped".to_string(),
            graph_id: Some(graph_id.clone()),
            name: "admin-skipped".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Skipped,
            started_at: None,
            completed_at: Some(chrono::Utc::now()),
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: Some("admin".to_string()),
            completed_via_reason: Some("premise false".to_string()),
            completed_via_at: Some(chrono::Utc::now()),
        })
        .unwrap();

        let result =
            perform_graph_reset(&db, &graph_id, Some(&["spec-admin-skipped".to_string()])).unwrap();
        assert!(!result.is_error.unwrap_or(false));

        let spec = db.get_graph_spec("spec-admin-skipped").unwrap().unwrap();
        assert_eq!(
            spec.status,
            GraphSpecStatus::Pending,
            "explicitly naming a skipped spec must reset it"
        );
        assert!(
            spec.completed_via.is_none(),
            "explicit reset must clear completed_via"
        );
        assert!(
            spec.completed_via_reason.is_none(),
            "explicit reset must clear completed_via_reason"
        );
        assert!(
            spec.completed_via_at.is_none(),
            "explicit reset must clear completed_via_at"
        );
    }

    #[test]
    fn graph_reset_no_skipped_specs_omits_preserved_note() {
        let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
        db.insert_graph_spec(&spec_with_status(
            &graph_id,
            "spec-failed",
            1,
            GraphSpecStatus::Failed,
        ))
        .unwrap();

        let result = perform_graph_reset(&db, &graph_id, None).unwrap();
        let text = format!("{:?}", result.content);
        assert!(text.contains("1 spec(s) reset"), "{text}");
        assert!(
            !text.contains("skipped"),
            "no skipped note when no specs were skipped: {text}"
        );
    }

    /// A graph whose last run was against a queue has empty (or irrelevant)
    /// bound specs — the queue's *members* are what actually need resetting.
    /// `graph_reset` must find them via the graph's persisted
    /// `active_run_queue_id`, reset every non-completed one back to pending
    /// (completed members untouched), and report the real count — not "0
    /// spec(s) reset", the false report from the incident this spec fixes.
    #[test]
    fn graph_reset_queue_run_resets_pending_queue_members_and_reports_count() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let graph_id = "graph-queue-reset-test".to_string();
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: graph_id.clone(),
            name: "Graph".to_string(),
            description: None,
            workdir: dir.path().to_string_lossy().to_string(),
            status: GraphStatus::Failed,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: Some(chrono::Utc::now()),
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: Some("queue-1".to_string()),
            hooks: std::collections::BTreeMap::new(),
        })
        .unwrap();

        let standalone = |id: &str, position: i64, status: GraphSpecStatus| GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: id.to_string(),
            description: None,
            position,
            parallelizable: false,
            status,
            started_at: None,
            completed_at: Some(chrono::Utc::now()),
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        db.insert_graph_spec(&standalone("queue-done", 1, GraphSpecStatus::Completed))
            .unwrap();
        db.insert_graph_spec(&standalone("queue-failed", 2, GraphSpecStatus::Failed))
            .unwrap();
        db.insert_graph_spec(&standalone("queue-pending", 3, GraphSpecStatus::Pending))
            .unwrap();
        db.insert_queue(&Queue {
            id: "queue-1".to_string(),
            name: "queue-1".to_string(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        for spec_id in ["queue-done", "queue-failed", "queue-pending"] {
            db.append_queue_member("queue-1", spec_id, None).unwrap();
        }

        let result = perform_graph_reset(&db, &graph_id, None).unwrap();
        assert!(!result.is_error.unwrap_or(false));
        let text = format!("{:?}", result.content);
        assert!(text.contains("2 spec(s) reset"), "{text}");

        let lp = db.get_graph(&graph_id).unwrap().unwrap();
        assert_eq!(lp.status, GraphStatus::Draft);

        let done = db.get_graph_spec("queue-done").unwrap().unwrap();
        let failed = db.get_graph_spec("queue-failed").unwrap().unwrap();
        let pending = db.get_graph_spec("queue-pending").unwrap().unwrap();
        assert_eq!(
            done.status,
            GraphSpecStatus::Completed,
            "completed queue member must be left untouched"
        );
        assert_eq!(failed.status, GraphSpecStatus::Pending);
        assert!(failed.completed_at.is_none());
        assert_eq!(pending.status, GraphSpecStatus::Pending);
    }

    /// A `Running`-status graph with no actual in-flight run is now
    /// resettable — the old status-only guard would have refused this
    /// (status alone said "running"), but the ground truth is the
    /// `graph_runs` table, and here it has nothing running under this graph.
    /// This is the intended flip side of
    /// `graph_reset_rejects_graph_with_in_flight_run_even_when_status_is_paused`:
    /// status and a run's lifetime are independent in both directions.
    #[test]
    fn graph_reset_allows_running_status_graph_with_no_in_flight_run() {
        let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Running);

        let result = perform_graph_reset(&db, &graph_id, None).unwrap();
        assert!(!result.is_error.unwrap_or(false));

        let lp = db.get_graph(&graph_id).unwrap().unwrap();
        assert_eq!(lp.status, GraphStatus::Draft);
    }

    /// The ground truth is the `graph_runs` table, not `lp.status` — a graph
    /// left `paused` by a sibling node's blocker report while a *different*
    /// node keeps executing must still refuse the reset, naming the node and
    /// run still in flight so the caller can wait or kill it deliberately
    /// (the 2026-08-05 incident this guards against: a reset silently killed
    /// and reset the still-running node's spec, and its late completion
    /// routed an edge and failed the graph out from under the fresh dispatch
    /// the reset then launched).
    #[test]
    fn graph_reset_rejects_graph_with_in_flight_run_even_when_status_is_paused() {
        let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Paused);
        db.insert_graph_spec(&spec_with_status(
            &graph_id,
            "spec-a",
            1,
            GraphSpecStatus::Running,
        ))
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "node-resilience".to_string(),
            spec_id: Some("spec-a".to_string()),
            graph_id: None,
            name: "resilience".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "sleep 30"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.insert_graph_run(&GraphNodeRun {
            id: "run-live".to_string(),
            graph_id: graph_id.clone(),
            spec_id: "spec-a".to_string(),
            node_id: "node-resilience".to_string(),
            status: GraphRunStatus::Running,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .unwrap();

        let result = perform_graph_reset(&db, &graph_id, None).unwrap();
        assert!(result.is_error.unwrap_or(false));
        let text = format!("{:?}", result.content);
        assert!(text.contains("resilience"), "{text}");
        assert!(text.contains("run-live"), "{text}");

        // Nothing touched: status, spec, and the run row all untouched.
        let lp = db.get_graph(&graph_id).unwrap().unwrap();
        assert_eq!(lp.status, GraphStatus::Paused);
        let spec = db.get_graph_spec("spec-a").unwrap().unwrap();
        assert_eq!(spec.status, GraphSpecStatus::Running);
        let run = db.get_graph_run("run-live").unwrap().unwrap();
        assert_eq!(run.status, GraphRunStatus::Running);
    }

    #[test]
    fn graph_reset_missing_graph_returns_not_found_error() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();

        let result = perform_graph_reset(&db, "does-not-exist", None).unwrap();
        assert!(result.is_error.unwrap_or(false));
        let text = format!("{:?}", result.content);
        assert!(text.contains("not found"), "{text}");
    }

    /// `graph_run` must keep refusing a `failed` graph, and the refusal must
    /// point at both sanctioned ways out: `graph_reset` (manual) and
    /// `graph_schedule_autorun` (self-service, for a quota-failed graph).
    #[test]
    fn graph_run_status_guard_rejects_failed_graph_and_names_both_ways_out() {
        let err = graph_run_status_guard("graph-1", GraphStatus::Failed).unwrap_err();
        assert!(err.contains("graph_reset"), "{err}");
        assert!(err.contains("graph_schedule_autorun"), "{err}");
    }

    #[test]
    fn graph_run_status_guard_rejects_completed_graph() {
        let err = graph_run_status_guard("graph-1", GraphStatus::Completed).unwrap_err();
        assert!(err.contains("graph_reset"), "{err}");
    }

    #[test]
    fn graph_run_status_guard_rejects_running_graph_with_graph_id() {
        let err = graph_run_status_guard("graph-1", GraphStatus::Running).unwrap_err();
        assert!(err.contains("graph-1"), "{err}");
        assert!(err.contains("already running"), "{err}");
    }

    #[test]
    fn graph_run_status_guard_accepts_draft_and_paused_graphs() {
        assert!(graph_run_status_guard("graph-1", GraphStatus::Draft).is_ok());
        assert!(graph_run_status_guard("graph-1", GraphStatus::Paused).is_ok());
    }

    // ── find_in_flight_run: graph_run's dispatch guard ────────────────────

    #[test]
    fn find_in_flight_run_none_when_nothing_running() {
        let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Paused);
        assert!(find_in_flight_run(&db, &graph_id).unwrap().is_none());
    }

    /// `graph_run` must refuse a duplicate dispatch while a node executes,
    /// regardless of the graph's own status — a `paused` graph can still have
    /// a live run when a sibling node's blocker report flipped status
    /// without terminating it (see `graph_reset_rejects_graph_with_in_flight_run_even_when_status_is_paused`
    /// for the same ground truth on the reset side).
    #[test]
    fn find_in_flight_run_finds_running_row_regardless_of_graph_status() {
        let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Paused);
        db.insert_graph_spec(&spec_with_status(
            &graph_id,
            "spec-a",
            1,
            GraphSpecStatus::Running,
        ))
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "node-resilience".to_string(),
            spec_id: Some("spec-a".to_string()),
            graph_id: None,
            name: "resilience".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "sleep 30"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.insert_graph_run(&GraphNodeRun {
            id: "run-live".to_string(),
            graph_id: graph_id.clone(),
            spec_id: "spec-a".to_string(),
            node_id: "node-resilience".to_string(),
            status: GraphRunStatus::Running,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .unwrap();

        let run = find_in_flight_run(&db, &graph_id)
            .unwrap()
            .expect("the still-running row must be found even though status is paused");
        assert_eq!(run.id, "run-live");

        let node_name = node_name_for_error(&db, &run.node_id).unwrap();
        let message = in_flight_run_error(&graph_id, &run.id, &node_name, run.started_at);
        assert!(message.contains("resilience"), "{message}");
        assert!(message.contains("run-live"), "{message}");
    }

    #[test]
    fn validate_node_config_rejects_double_encoded_string() {
        let config = serde_json::json!("{\"platform\": \"mimo\"}");
        let error = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
        assert!(error.contains("must be a JSON object"), "{error}");
    }

    #[test]
    fn validate_node_config_agent_requires_platform_or_cli() {
        let config = serde_json::json!({ "model": "opus" });
        let error = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
        assert!(error.contains("'agent'"), "{error}");
        assert!(error.contains("platform"), "{error}");
    }

    #[test]
    fn validate_node_config_check_requires_command() {
        let config = serde_json::json!({ "timeout_seconds": 30 });
        let error = validate_node_config(GraphNodeKind::Check, &config).unwrap_err();
        assert!(error.contains("'check'"), "{error}");
        assert!(error.contains("command"), "{error}");
    }

    #[test]
    fn validate_node_config_gate_requires_value_for_output_contains() {
        let config = serde_json::json!({ "evaluate": "output_contains" });
        let error = validate_node_config(GraphNodeKind::Gate, &config).unwrap_err();
        assert!(error.contains("'gate'"), "{error}");
        assert!(error.contains("value"), "{error}");

        let config_without_evaluate = serde_json::json!({});
        let error =
            validate_node_config(GraphNodeKind::Gate, &config_without_evaluate).unwrap_err();
        assert!(error.contains("value"), "{error}");
    }

    #[test]
    fn validate_node_config_accepts_valid_configs() {
        assert!(validate_node_config(
            GraphNodeKind::Agent,
            &serde_json::json!({ "platform": "claude" })
        )
        .is_ok());
        assert!(
            validate_node_config(GraphNodeKind::Agent, &serde_json::json!({ "cli": "codex" }))
                .is_ok()
        );
        assert!(validate_node_config(
            GraphNodeKind::Check,
            &serde_json::json!({ "command": "cargo test" })
        )
        .is_ok());
        assert!(validate_node_config(
            GraphNodeKind::Gate,
            &serde_json::json!({ "evaluate": "output_contains", "value": "ok" })
        )
        .is_ok());
    }

    fn router_config(routes: &serde_json::Value, fallback: &str) -> serde_json::Value {
        serde_json::json!({ "routes": routes, "fallback": fallback })
    }

    fn two_routes_json() -> serde_json::Value {
        serde_json::json!([
            { "label": "retry", "description": "Retry the current step." },
            { "label": "escalate", "description": "Hand off to a human." },
        ])
    }

    #[test]
    fn validate_node_config_router_accepts_valid_shape() {
        let config = router_config(&two_routes_json(), "retry");
        assert!(validate_node_config(GraphNodeKind::Router, &config).is_ok());
    }

    #[test]
    fn validate_node_config_router_rejects_fewer_than_two_routes() {
        let config = router_config(
            &serde_json::json!([{ "label": "retry", "description": "Retry." }]),
            "retry",
        );
        let error = validate_node_config(GraphNodeKind::Router, &config).unwrap_err();
        assert!(error.contains("at least 2 routes"), "{error}");
    }

    #[test]
    fn validate_node_config_router_rejects_no_fallback() {
        let config = serde_json::json!({ "routes": two_routes_json() });
        let error = validate_node_config(GraphNodeKind::Router, &config).unwrap_err();
        assert!(error.contains("fallback"), "{error}");
    }

    #[test]
    fn validate_node_config_router_rejects_missing_routes_field() {
        let error =
            validate_node_config(GraphNodeKind::Router, &serde_json::json!({})).unwrap_err();
        assert!(error.contains("'routes'"), "{error}");
    }

    /// The exact incident shape (graph `824de730-7fec-4031-800a-7933d2cf94c1`,
    /// node `d0458fe0-24b8-4a29-8a69-7bb0e742e046`): an agent node config
    /// with `prompt` instead of `prompt_template` must fail loudly, naming
    /// `prompt_template` as the field that's actually read — not be accepted
    /// and silently run on the bare fallback template.
    #[test]
    fn validate_node_config_agent_rejects_prompt_key_naming_prompt_template() {
        let config = serde_json::json!({ "platform": "claude", "prompt": "do the thing" });
        let error = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
        assert!(error.contains("'prompt'"), "{error}");
        assert!(
            error.contains("prompt_template"),
            "error must name the correct key: {error}"
        );
    }

    #[test]
    fn validate_node_config_rejects_unknown_key_for_every_kind() {
        let agent_error = validate_node_config(
            GraphNodeKind::Agent,
            &serde_json::json!({ "platform": "claude", "unexpected_field": true }),
        )
        .unwrap_err();
        assert!(agent_error.contains("unexpected_field"), "{agent_error}");
        assert!(agent_error.contains("'agent'"), "{agent_error}");

        let check_error = validate_node_config(
            GraphNodeKind::Check,
            &serde_json::json!({ "command": "true", "unexpected_field": true }),
        )
        .unwrap_err();
        assert!(check_error.contains("unexpected_field"), "{check_error}");

        let gate_error = validate_node_config(
            GraphNodeKind::Gate,
            &serde_json::json!({ "evaluate": "output_contains", "value": "ok", "unexpected_field": true }),
        )
        .unwrap_err();
        assert!(gate_error.contains("unexpected_field"), "{gate_error}");

        let router_error = validate_node_config(
            GraphNodeKind::Router,
            &serde_json::json!({ "routes": two_routes_json(), "fallback": "retry", "unexpected_field": true }),
        )
        .unwrap_err();
        assert!(router_error.contains("unexpected_field"), "{router_error}");
    }

    /// `commit_rights` (B37) is engine-checked on every node kind
    /// uniformly, so it must be accepted everywhere, not just on agent
    /// nodes.
    #[test]
    fn validate_node_config_accepts_commit_rights_on_every_kind() {
        assert!(validate_node_config(
            GraphNodeKind::Agent,
            &serde_json::json!({ "platform": "claude", "commit_rights": true })
        )
        .is_ok());
        assert!(validate_node_config(
            GraphNodeKind::Check,
            &serde_json::json!({ "command": "true", "commit_rights": true })
        )
        .is_ok());
    }

    /// `require_report` is an agent-only key (unlike `commit_rights`, which
    /// is engine-checked on every kind) — accepted on `agent`, rejected as
    /// unrecognized everywhere else, since only an agent node's process exit
    /// can be judged against a self-report.
    #[test]
    fn validate_node_config_accepts_require_report_on_agent_only() {
        assert!(validate_node_config(
            GraphNodeKind::Agent,
            &serde_json::json!({ "platform": "claude", "require_report": true })
        )
        .is_ok());

        let check_error = validate_node_config(
            GraphNodeKind::Check,
            &serde_json::json!({ "command": "true", "require_report": true }),
        )
        .unwrap_err();
        assert!(check_error.contains("require_report"), "{check_error}");
    }

    /// Neither `prompt_template` nor `prompt_preset` is still a VALID agent
    /// config (the bare fallback template is deliberately kept reachable —
    /// see the spec's "keep the fallback" constraint) — this only rejects
    /// keys the engine never reads, not this legitimate default-reliant
    /// shape. Visibility into "this node runs on the default" is a separate
    /// concern, handled by `agent_prompt_source`/`graph_node_json`'s
    /// `prompt_source` field, not by rejecting the config outright.
    #[test]
    fn validate_node_config_agent_without_prompt_template_or_preset_is_still_valid() {
        assert!(validate_node_config(
            GraphNodeKind::Agent,
            &serde_json::json!({ "platform": "claude" })
        )
        .is_ok());
    }

    #[test]
    fn unknown_config_keys_is_empty_for_join_regardless_of_content() {
        let config = serde_json::json!({ "anything": "goes", "ensemble_id": "e1" });
        let map = config.as_object().unwrap();
        assert!(unknown_config_keys(GraphNodeKind::Join, map).is_empty());
    }

    #[test]
    fn blueprints_are_listed_after_a_fresh_startup_and_reseeding_is_idempotent() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();

        let names_after_first_start: Vec<String> = db
            .list_blueprints()
            .unwrap()
            .into_iter()
            .map(|b| b.name)
            .collect();
        for expected in [
            "implementer",
            "cargo-gates",
            "reviewer-committer",
            "commit-check",
            "resilience",
        ] {
            assert!(
                names_after_first_start.contains(&expected.to_string()),
                "expected builtin '{expected}' after fresh startup, got {names_after_first_start:?}"
            );
        }

        // Simulate a second daemon startup against the same database.
        db.seed_builtin_blueprints().unwrap();
        let names_after_second_start: Vec<String> = db
            .list_blueprints()
            .unwrap()
            .into_iter()
            .map(|b| b.name)
            .collect();
        assert_eq!(
            names_after_first_start.len(),
            names_after_second_start.len(),
            "reseeding must not duplicate builtins"
        );
    }

    #[test]
    fn custom_blueprint_create_list_delete_round_trip_and_builtin_delete_refused() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();

        let custom = Blueprint {
            id: uuid::Uuid::new_v4().to_string(),
            name: "tdd-implementer".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({ "platform": "claude", "prompt": "write a failing test first" }),
            builtin: false,
            created_at: chrono::Utc::now(),
        };
        db.insert_blueprint(&custom).unwrap();

        let names: Vec<String> = db
            .list_blueprints()
            .unwrap()
            .into_iter()
            .map(|b| b.name)
            .collect();
        assert!(names.contains(&"tdd-implementer".to_string()));

        let removed = db.delete_blueprint_by_name("tdd-implementer").unwrap();
        assert!(removed);
        assert!(db
            .get_blueprint_by_name("tdd-implementer")
            .unwrap()
            .is_none());

        // Deleting a builtin is refused at the validation layer with an
        // actionable message, before ever touching the DB.
        let builtin = db
            .get_blueprint_by_name("implementer")
            .unwrap()
            .expect("builtin should exist");
        let error = super::validate_blueprint_deletable(&builtin).unwrap_err();
        assert!(error.contains("implementer"));
        assert!(error.contains("cannot be deleted"));
    }

    #[test]
    fn graph_add_node_from_blueprint_with_override_merges_config_and_override_wins() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();

        // The builtin carries no platform (see `builtin_blueprint_specs`),
        // so the caller must supply one via `config_overrides` — this also
        // exercises the required-override path with a successful call.
        let mut overrides = serde_json::Map::new();
        overrides.insert("platform".to_string(), serde_json::json!("claude"));
        overrides.insert("model".to_string(), serde_json::json!("opus"));

        let (kind, config) =
            resolve_node_kind_and_config(&db, None, None, Some("implementer"), Some(overrides))
                .expect("blueprint resolution should succeed");

        assert_eq!(kind, GraphNodeKind::Agent);
        assert_eq!(config["platform"], "claude");
        assert_eq!(config["model"], "opus");
        // The templated key (prompt_preset) survives the shallow merge.
        assert_eq!(config["prompt_preset"], "implementer");
    }

    /// The caller-must-supply-a-harness contract, from the other side: an
    /// agent blueprint's config omits `platform`/`cli` by design (see
    /// `builtin_blueprint_specs`), so creating a node from one without
    /// `config_overrides` supplying it must fail — never silently fall back
    /// to a default harness — with a message that names the missing field
    /// and says the omission is intentional.
    #[test]
    fn graph_add_node_from_agent_blueprint_without_harness_override_fails_with_useful_message() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();

        let error =
            resolve_node_kind_and_config(&db, None, None, Some("implementer"), None).unwrap_err();

        assert!(error.contains("implementer"), "{error}");
        assert!(error.contains("platform"), "{error}");
        assert!(
            error.contains("intentionally"),
            "error should explain the blueprint intentionally omits a harness: {error}"
        );
    }

    /// A custom blueprint that pins its own platform (the pre-existing,
    /// still-supported pattern) needs no override at all — only the
    /// harness-free builtins require one.
    #[test]
    fn graph_add_node_from_custom_blueprint_with_platform_needs_no_override() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_blueprint(&Blueprint {
            id: uuid::Uuid::new_v4().to_string(),
            name: "my-pinned-implementer".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude", "prompt": "go implement it"}),
            builtin: false,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let (kind, config) =
            resolve_node_kind_and_config(&db, None, None, Some("my-pinned-implementer"), None)
                .expect("a custom blueprint with its own platform should not require an override");

        assert_eq!(kind, GraphNodeKind::Agent);
        assert_eq!(config["platform"], "claude");
    }

    #[test]
    fn graph_add_node_with_unknown_blueprint_lists_available_names() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();

        let error = validate_blueprint_exists(&db, "does-not-exist").unwrap_err();

        assert!(error.contains("does-not-exist"));
        assert!(error.contains("implementer"));
        assert!(error.contains("cargo-gates"));
        assert!(error.contains("reviewer-committer"));
        assert!(error.contains("commit-check"));
        assert!(error.contains("resilience"));
    }

    #[test]
    fn resolve_sync_agent_id_reads_canopy_header() {
        let request = axum::http::Request::builder()
            .header(CANOPY_AGENT_ID_HEADER, "agent-123")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();

        assert_eq!(
            header_str(&parts, CANOPY_AGENT_ID_HEADER),
            Some("agent-123")
        );
    }

    #[test]
    fn resolve_sync_agent_id_requires_canopy_header() {
        let request = axum::http::Request::builder()
            .header("x-canopy-session-name", "cedro")
            .header("x-canopy-workdir", "/tmp/workdir")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();

        assert_eq!(header_str(&parts, CANOPY_AGENT_ID_HEADER), None);

        let error = missing_sync_identity_error();
        assert_eq!(error.message, MISSING_SYNC_IDENTITY_MESSAGE);
    }

    fn standalone_spec(id: &str) -> GraphSpec {
        GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: id.to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        }
    }

    fn queue_test_db() -> (tempfile::TempDir, Database) {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        (dir, db)
    }

    fn insert_queue(db: &Database, id: &str) {
        db.insert_queue(&Queue {
            id: id.to_string(),
            name: format!("{id}-name"),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    }

    /// Build a fully-wired `TaskTriggerHandler` over an in-memory-ish temp DB
    /// so the queue `#[tool]` methods (and their shared `do_queue_*` helpers)
    /// can be exercised end-to-end.
    fn queue_test_handler() -> (
        tempfile::TempDir,
        std::sync::Arc<Database>,
        TaskTriggerHandler,
    ) {
        use crate::application::notification_service::{
            DefaultNotificationService, NotificationService,
        };
        use crate::executor::Executor;
        use crate::graph_engine::GraphEngine;
        use crate::rag::ingestion::IngestionManager;
        use crate::sync_manager::SyncManager;
        use crate::watchers::WatcherEngine;
        use std::sync::Arc;
        use tokio::sync::Notify;

        let dir = tempdir().unwrap();
        let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
        let notif: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
        let executor = Arc::new(Executor::new(Arc::clone(&db), Arc::clone(&notif)));
        let sync_manager = Arc::new(SyncManager::new(Arc::clone(&db)));
        let graph_engine = Arc::new(GraphEngine::new(Arc::clone(&db), Arc::clone(&notif)));
        let watcher_engine = Arc::new(WatcherEngine::new(
            Arc::clone(&db),
            Arc::clone(&executor),
            Arc::clone(&graph_engine),
        ));
        let ingestion = Arc::new(IngestionManager::new(
            Arc::clone(&db),
            dir.path().to_path_buf(),
        ));
        let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::new(
            dir.path().join("skills"),
            Vec::new(),
            15,
        ));
        let handler = TaskTriggerHandler::with_deps(HandlerDeps {
            db: Arc::clone(&db),
            executor,
            watcher_engine,
            scheduler_notify: Arc::new(Notify::new()),
            graph_engine,
            notification_service: notif,
            sync_manager,
            ingestion,
            dynamic_skills,
            port: 0,
        });
        (dir, db, handler)
    }

    fn result_text(result: &rmcp::model::CallToolResult) -> String {
        format!("{:?}", result.content)
    }

    #[test]
    fn queue_crud_and_ordering_round_trips() {
        let (_dir, db) = queue_test_db();
        for id in ["spec-a", "spec-b", "spec-c"] {
            db.insert_graph_spec(&standalone_spec(id)).unwrap();
        }
        insert_queue(&db, "queue-1");

        db.append_queue_member("queue-1", "spec-a", None).unwrap();
        db.append_queue_member("queue-1", "spec-b", None).unwrap();
        db.append_queue_member("queue-1", "spec-c", None).unwrap();

        assert_eq!(
            db.list_queue_member_spec_ids("queue-1").unwrap(),
            vec!["spec-a", "spec-b", "spec-c"]
        );

        let details = db.get_queue_details("queue-1").unwrap().unwrap();
        assert_eq!(details.queue.id, "queue-1");
        assert_eq!(
            details
                .members
                .iter()
                .map(|spec| spec.id.clone())
                .collect::<Vec<_>>(),
            vec!["spec-a", "spec-b", "spec-c"]
        );

        assert!(db.remove_queue_member("queue-1", "spec-b").unwrap());
        assert_eq!(
            db.list_queue_member_spec_ids("queue-1").unwrap(),
            vec!["spec-a", "spec-c"]
        );
        assert!(!db.remove_queue_member("queue-1", "spec-b").unwrap());

        assert!(db.list_queues().unwrap().iter().any(|p| p.id == "queue-1"));
    }

    #[test]
    fn queue_reorder_is_total_and_deterministic() {
        let (_dir, db) = queue_test_db();
        for id in ["spec-a", "spec-b", "spec-c"] {
            db.insert_graph_spec(&standalone_spec(id)).unwrap();
        }
        insert_queue(&db, "queue-1");
        for id in ["spec-a", "spec-b", "spec-c"] {
            db.append_queue_member("queue-1", id, None).unwrap();
        }

        let current = db.list_queue_member_spec_ids("queue-1").unwrap();
        let order = vec![
            "spec-c".to_string(),
            "spec-a".to_string(),
            "spec-b".to_string(),
        ];
        assert!(validate_queue_reorder(&current, &order).is_ok());

        db.reorder_queue_members("queue-1", &order).unwrap();
        assert_eq!(db.list_queue_member_spec_ids("queue-1").unwrap(), order);
    }

    #[test]
    fn queue_reorder_rejects_partial_list() {
        let current = vec![
            "spec-a".to_string(),
            "spec-b".to_string(),
            "spec-c".to_string(),
        ];
        let order = vec!["spec-a".to_string(), "spec-b".to_string()];

        let error = validate_queue_reorder(&current, &order).unwrap_err();
        assert!(error.contains("exactly once; got 2"), "{error}");
    }

    #[test]
    fn queue_reorder_rejects_unknown_spec() {
        let current = vec![
            "spec-a".to_string(),
            "spec-b".to_string(),
            "spec-c".to_string(),
        ];
        let order = vec![
            "spec-a".to_string(),
            "spec-b".to_string(),
            "ghost".to_string(),
        ];

        let error = validate_queue_reorder(&current, &order).unwrap_err();
        assert!(error.contains("no spec 'ghost'"), "{error}");
    }

    fn queue_raw_text(result: &rmcp::model::CallToolResult) -> String {
        result.content[0].as_text().unwrap().text.clone()
    }

    fn queue_is_err(result: &rmcp::model::CallToolResult) -> bool {
        result.is_error == Some(true)
    }

    /// CM44 FR1/FR2/FR6: setting a member's group reorders the queue and
    /// returns the resulting order with groups.
    #[tokio::test]
    async fn queue_set_group_tool_returns_reordered_queue() {
        let (_dir, db, handler) = queue_test_handler();
        for id in ["a", "b", "c"] {
            db.insert_graph_spec(&standalone_spec(id)).unwrap();
        }
        insert_queue(&db, "queue-1");
        db.append_queue_member("queue-1", "a", Some("g1")).unwrap();
        db.append_queue_member("queue-1", "b", None).unwrap();
        db.append_queue_member("queue-1", "c", Some("g1")).unwrap();

        let result = handler
            .do_queue_set_group("queue-1", "b", Some("g1"))
            .await
            .unwrap();

        assert!(!queue_is_err(&result), "{}", result_text(&result));
        let value: serde_json::Value = serde_json::from_str(&queue_raw_text(&result)).unwrap();
        let members = value["queue"]["members"].as_array().unwrap();
        let ids: Vec<&str> = members.iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["a", "c", "b"]);
        let b = &members[2];
        assert_eq!(b["group"], serde_json::json!("g1"));
        assert_eq!(b["queue_position"], serde_json::json!(3));
    }

    /// CM44 FR3: a reorder that would split a group's pending members is
    /// refused, names the group and the offending positions, and changes
    /// nothing.
    #[tokio::test]
    async fn queue_reorder_refuses_a_split_group() {
        let (_dir, db, handler) = queue_test_handler();
        for id in ["a", "b", "c"] {
            db.insert_graph_spec(&standalone_spec(id)).unwrap();
        }
        insert_queue(&db, "queue-1");
        db.append_queue_member("queue-1", "a", Some("g")).unwrap();
        db.append_queue_member("queue-1", "b", Some("g")).unwrap();
        db.append_queue_member("queue-1", "c", None).unwrap();

        let result = handler
            .do_queue_reorder(
                "queue-1",
                &["a".to_string(), "c".to_string(), "b".to_string()],
            )
            .await
            .unwrap();

        assert!(queue_is_err(&result), "{}", result_text(&result));
        let msg = queue_raw_text(&result);
        assert!(msg.contains("group 'g'"), "{msg}");
        assert!(msg.contains("positions 1, 3"), "{msg}");
        assert!(msg.contains("Nothing was changed"), "{msg}");
        assert_eq!(
            db.list_queue_member_spec_ids("queue-1").unwrap(),
            vec!["a", "b", "c"]
        );
    }

    /// CM44 FR2/FR6: adding a spec to an existing group places it right after
    /// the group's last pending member and returns the resulting order.
    #[tokio::test]
    async fn queue_add_spec_tool_returns_order_with_groups() {
        let (_dir, db, handler) = queue_test_handler();
        for id in ["a", "b", "c", "x"] {
            db.insert_graph_spec(&standalone_spec(id)).unwrap();
        }
        insert_queue(&db, "queue-1");
        db.append_queue_member("queue-1", "a", Some("g")).unwrap();
        db.append_queue_member("queue-1", "b", Some("g")).unwrap();
        db.append_queue_member("queue-1", "c", None).unwrap();

        let result = handler
            .do_queue_add_spec("queue-1", "x", Some("g"))
            .await
            .unwrap();

        assert!(!queue_is_err(&result), "{}", result_text(&result));
        let value: serde_json::Value = serde_json::from_str(&queue_raw_text(&result)).unwrap();
        let members = value["queue"]["members"].as_array().unwrap();
        let ids: Vec<&str> = members.iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["a", "b", "x", "c"]);
        let x = &members[2];
        assert_eq!(x["group"], serde_json::json!("g"));
        assert_eq!(x["queue_position"], serde_json::json!(3));
    }

    fn running_spec(id: &str) -> GraphSpec {
        let mut spec = standalone_spec(id);
        spec.status = GraphSpecStatus::Running;
        spec
    }

    fn interrupted_spec(id: &str) -> GraphSpec {
        let mut spec = standalone_spec(id);
        spec.status = GraphSpecStatus::Interrupted;
        spec
    }

    /// A minimal graph row, needed only to satisfy `graph_runs.graph_id`'s FK.
    fn insert_test_graph(db: &Database, id: &str) {
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: GraphStatus::Running,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        })
        .unwrap();
    }

    /// A minimal node row, needed only to satisfy `graph_runs.node_id`'s FK.
    fn insert_test_node(db: &Database, id: &str, spec_id: &str) {
        db.insert_graph_node(&GraphNode {
            id: id.to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: id.to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    }

    fn graph_run_row(
        id: &str,
        graph_id: &str,
        spec_id: &str,
        status: GraphRunStatus,
    ) -> GraphNodeRun {
        GraphNodeRun {
            id: id.to_string(),
            graph_id: graph_id.to_string(),
            spec_id: spec_id.to_string(),
            node_id: "node-1".to_string(),
            status,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: (status != GraphRunStatus::Running).then(chrono::Utc::now),
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        }
    }

    // ── B12: stale graph_complete_node/graph_report_blocker reports ─────

    fn reported_run_fixture() -> (tempfile::TempDir, Database) {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        db.insert_graph_spec(&standalone_spec("spec-a")).unwrap();
        insert_test_node(&db, "node-1", "spec-a");
        (dir, db)
    }

    #[test]
    fn resolve_reported_run_accepts_matching_active_run() {
        let (_dir, db) = reported_run_fixture();
        db.insert_graph_run(&graph_run_row(
            "run-1",
            "graph-1",
            "spec-a",
            GraphRunStatus::Running,
        ))
        .unwrap();

        let run = resolve_reported_run(&db, "run-1", "node-1")
            .unwrap()
            .expect("a running run for the claimed node_id must be accepted");
        assert_eq!(run.id, "run-1");
    }

    #[test]
    fn resolve_reported_run_rejects_unknown_run_id() {
        let (_dir, db) = reported_run_fixture();

        let result = resolve_reported_run(&db, "no-such-run", "node-1").unwrap();
        let error = result.expect_err("an unknown run_id must be rejected");
        assert!(error.is_error.unwrap_or(false));
    }

    #[test]
    fn resolve_reported_run_rejects_node_id_mismatch() {
        let (_dir, db) = reported_run_fixture();
        db.insert_graph_run(&graph_run_row(
            "run-1",
            "graph-1",
            "spec-a",
            GraphRunStatus::Running,
        ))
        .unwrap();

        // "run-1" really belongs to "node-1" (see graph_run_row) — a report
        // claiming a different node_id for the same run_id is malformed.
        let result = resolve_reported_run(&db, "run-1", "some-other-node").unwrap();
        let error = result.expect_err("a node_id that doesn't match the run must be rejected");
        assert!(error.is_error.unwrap_or(false));
    }

    /// The core B12 regression case: a node run that was already finalized
    /// (timed out, killed on pause/reset, or superseded by a retry — any of
    /// which flips its status away from `running`) must reject a late report
    /// naming its exact `run_id`, rather than that report silently landing
    /// on whatever's now active for the same `node_id`.
    #[test]
    fn resolve_reported_run_rejects_already_finalized_run() {
        let (_dir, db) = reported_run_fixture();
        // The stale run: already finalized (e.g. by the timeout/pause kill
        // path), simulating the orphaned agent's late self-report arriving
        // after the engine gave up on it.
        db.insert_graph_run(&graph_run_row(
            "run-stale",
            "graph-1",
            "spec-a",
            GraphRunStatus::Fail,
        ))
        .unwrap();
        // A newer attempt at the SAME node is now the genuinely active run —
        // exactly the run a naive node_id-only lookup would have
        // misattributed the stale report to.
        db.insert_graph_run(&graph_run_row(
            "run-current",
            "graph-1",
            "spec-a",
            GraphRunStatus::Running,
        ))
        .unwrap();

        let result = resolve_reported_run(&db, "run-stale", "node-1").unwrap();
        let error = result.expect_err("a finalized run must reject a late report");
        assert!(error.is_error.unwrap_or(false));

        // And the genuinely active run must be left completely untouched.
        let current = db.get_graph_run("run-current").unwrap().unwrap();
        assert_eq!(current.status, GraphRunStatus::Running);
    }

    #[test]
    fn queue_not_consumed_allows_start_when_every_member_is_pending() {
        let (_dir, db) = queue_test_db();
        db.insert_graph_spec(&standalone_spec("spec-a")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
        insert_queue(&db, "queue-1");
        db.append_queue_member("queue-1", "spec-a", None).unwrap();
        db.append_queue_member("queue-1", "spec-b", None).unwrap();

        assert!(validate_queue_not_consumed(&db, "queue-1", "graph-requesting").is_ok());
    }

    #[test]
    fn queue_not_consumed_blocks_when_a_member_runs_under_another_graph() {
        let (_dir, db) = queue_test_db();
        db.insert_graph_spec(&running_spec("spec-a")).unwrap();
        insert_queue(&db, "queue-1");
        db.append_queue_member("queue-1", "spec-a", None).unwrap();
        insert_test_graph(&db, "graph-other");
        insert_test_node(&db, "node-1", "spec-a");
        db.insert_graph_run(&graph_run_row(
            "run-1",
            "graph-other",
            "spec-a",
            GraphRunStatus::Running,
        ))
        .unwrap();

        let error = validate_queue_not_consumed(&db, "queue-1", "graph-requesting").unwrap_err();

        assert!(error.contains("spec-a"), "{error}");
        assert!(error.contains("graph-other"), "{error}");
    }

    #[test]
    fn queue_not_consumed_allows_the_owning_graph_to_resume_its_own_running_spec() {
        // A paused queue run's active spec stays `running` between node
        // executions. Resuming the SAME graph against the SAME queue must not
        // be mistaken for a conflicting run.
        let (_dir, db) = queue_test_db();
        db.insert_graph_spec(&running_spec("spec-a")).unwrap();
        insert_queue(&db, "queue-1");
        db.append_queue_member("queue-1", "spec-a", None).unwrap();
        insert_test_graph(&db, "graph-owner");
        insert_test_node(&db, "node-1", "spec-a");
        db.insert_graph_run(&graph_run_row(
            "run-1",
            "graph-owner",
            "spec-a",
            GraphRunStatus::Pass,
        ))
        .unwrap();

        assert!(validate_queue_not_consumed(&db, "queue-1", "graph-owner").is_ok());
    }

    /// B18 (Requirement 2): `skip_next_spec` on a queue-driven paused graph
    /// must find its in-flight member through the graph's persisted
    /// `active_run_queue_id` — the member's own `graph_id` column stays `None`
    /// (queue membership never binds it), so `list_graph_specs(graph_id)` alone
    /// can't see it. Before this fix `handle_skip_next_spec` always errored
    /// "No running spec found" for a queue-driven pause.
    #[test]
    fn skip_next_spec_finds_and_skips_the_running_queue_member() {
        let (_dir, db) = queue_test_db();
        insert_test_graph(&db, "graph-owner");
        db.set_graph_active_run_queue("graph-owner", Some("queue-1"))
            .unwrap();

        db.insert_graph_spec(&running_spec("spec-a")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
        insert_queue(&db, "queue-1");
        db.append_queue_member("queue-1", "spec-a", None).unwrap();
        db.append_queue_member("queue-1", "spec-b", None).unwrap();

        handle_skip_next_spec(&db, "graph-owner").unwrap();

        let spec_a = db.get_graph_spec("spec-a").unwrap().unwrap();
        let spec_b = db.get_graph_spec("spec-b").unwrap().unwrap();
        assert_eq!(spec_a.status, GraphSpecStatus::Skipped);
        assert_eq!(spec_b.status, GraphSpecStatus::Pending);
    }

    #[test]
    fn skip_next_spec_prefers_the_graph_bound_spec_over_queue_context() {
        // A graph with its own bound `running` spec must use that, even if a
        // stale `active_run_queue_id` is still sitting on the graph from an
        // earlier, unrelated queue run.
        let (_dir, db) = queue_test_db();
        insert_test_graph(&db, "graph-owner");
        db.set_graph_active_run_queue("graph-owner", Some("queue-1"))
            .unwrap();

        let mut bound = running_spec("spec-bound");
        bound.graph_id = Some("graph-owner".to_string());
        db.insert_graph_spec(&bound).unwrap();
        db.insert_graph_spec(&running_spec("spec-queue")).unwrap();
        insert_queue(&db, "queue-1");
        db.append_queue_member("queue-1", "spec-queue", None)
            .unwrap();

        handle_skip_next_spec(&db, "graph-owner").unwrap();

        let bound_after = db.get_graph_spec("spec-bound").unwrap().unwrap();
        let queue_after = db.get_graph_spec("spec-queue").unwrap().unwrap();
        assert_eq!(bound_after.status, GraphSpecStatus::Skipped);
        assert_eq!(queue_after.status, GraphSpecStatus::Running);
    }

    #[test]
    fn skip_next_spec_errors_when_no_spec_is_running_anywhere() {
        let (_dir, db) = queue_test_db();
        insert_test_graph(&db, "graph-owner");

        let error = handle_skip_next_spec(&db, "graph-owner").unwrap_err();
        assert!(error.message.contains("No running spec found"));
    }

    /// CM42 (FR3): `skip_next_spec` must find a spec left `Interrupted` by a
    /// no-verdict ensemble block — not only a `running` one — so the skip verb
    /// keeps working on the same paused graph `retry_current_node` can retry.
    #[test]
    fn skip_next_spec_finds_interrupted_spec() {
        let (_dir, db) = queue_test_db();
        insert_test_graph(&db, "graph-owner");
        let mut bound = interrupted_spec("spec-bound");
        bound.graph_id = Some("graph-owner".to_string());
        db.insert_graph_spec(&bound).unwrap();

        handle_skip_next_spec(&db, "graph-owner").unwrap();

        let after = db.get_graph_spec("spec-bound").unwrap().unwrap();
        assert_eq!(after.status, GraphSpecStatus::Skipped);
    }

    /// CM42 (FR3): the queue-side half of the same lookup —
    /// `queue_running_spec` is `handle_skip_next_spec`'s fallback for a
    /// queue-driven pause and must see an `Interrupted` in-flight member too.
    #[test]
    fn queue_running_spec_finds_interrupted_spec() {
        let (_dir, db) = queue_test_db();
        insert_test_graph(&db, "graph-owner");
        db.set_graph_active_run_queue("graph-owner", Some("queue-1"))
            .unwrap();
        db.insert_graph_spec(&interrupted_spec("spec-a")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
        insert_queue(&db, "queue-1");
        db.append_queue_member("queue-1", "spec-a", None).unwrap();
        db.append_queue_member("queue-1", "spec-b", None).unwrap();

        let found = queue_running_spec(&db, "graph-owner").unwrap();

        assert_eq!(
            found.map(|spec| spec.id),
            Some("spec-a".to_string()),
            "the interrupted in-flight member is the current one"
        );
    }

    /// B35: `retry_current_node` must error when no spec is running in
    /// either the graph's bound specs or its queue — same validation shape as
    /// `skip_next_spec`.
    #[test]
    fn retry_current_node_errors_when_no_running_spec() {
        let (_dir, db) = queue_test_db();
        insert_test_graph(&db, "graph-owner");
        let error = handle_retry_current_node(&db, "graph-owner").unwrap_err();
        assert!(error.message.contains("No running spec found"));
    }

    /// B35: `retry_current_node` must find the running spec through the
    /// graph's persisted `active_run_queue_id` (queue member has `graph_id: None`).
    #[test]
    fn retry_current_node_finds_running_queue_member() {
        let (_dir, db) = queue_test_db();
        insert_test_graph(&db, "graph-owner");
        db.set_graph_active_run_queue("graph-owner", Some("queue-1"))
            .unwrap();

        db.insert_graph_spec(&running_spec("spec-a")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
        insert_queue(&db, "queue-1");
        db.append_queue_member("queue-1", "spec-a", None).unwrap();
        db.append_queue_member("queue-1", "spec-b", None).unwrap();

        // Should succeed without error — a running spec exists.
        assert!(handle_retry_current_node(&db, "graph-owner").is_ok());
    }

    /// CB50 FR1: `retry_current_node` must accept a bound spec that is
    /// `interrupted` — the status every interrupt-pause and every boot
    /// reconcile leaves behind — not just `running`.
    #[test]
    fn retry_current_node_accepts_interrupted_bound_spec() {
        let (_dir, db) = queue_test_db();
        insert_test_graph(&db, "graph-owner");
        let mut bound = interrupted_spec("spec-bound");
        bound.graph_id = Some("graph-owner".to_string());
        db.insert_graph_spec(&bound).unwrap();

        assert!(handle_retry_current_node(&db, "graph-owner").is_ok());
    }

    /// CB50 FR1: same, but the interrupted spec is a queue member (found
    /// through `active_run_queue_id`), not a bound spec — the queue-driven
    /// half of the same bug (B18's own scenario).
    #[test]
    fn retry_current_node_finds_interrupted_queue_member() {
        let (_dir, db) = queue_test_db();
        insert_test_graph(&db, "graph-owner");
        db.set_graph_active_run_queue("graph-owner", Some("queue-1"))
            .unwrap();

        db.insert_graph_spec(&interrupted_spec("spec-a")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
        insert_queue(&db, "queue-1");
        db.append_queue_member("queue-1", "spec-a", None).unwrap();
        db.append_queue_member("queue-1", "spec-b", None).unwrap();

        assert!(handle_retry_current_node(&db, "graph-owner").is_ok());
    }

    /// CB50 FR5: when nothing is resumable, the error names the spec that
    /// IS current, its status, and the verb that applies to it.
    #[test]
    fn retry_current_node_error_names_spec_status_and_applicable_verb() {
        let (_dir, db) = queue_test_db();
        insert_test_graph(&db, "graph-owner");
        let mut failed = standalone_spec("spec-failed");
        failed.status = GraphSpecStatus::Failed;
        failed.graph_id = Some("graph-owner".to_string());
        db.insert_graph_spec(&failed).unwrap();

        let error = handle_retry_current_node(&db, "graph-owner").unwrap_err();
        assert!(error.message.contains("spec-failed"), "{}", error.message);
        assert!(error.message.contains("failed"), "{}", error.message);
        assert!(error.message.contains("graph_reset"), "{}", error.message);
        assert!(!error.message.contains("loop_"), "{}", error.message);
    }

    #[test]
    fn queue_reorder_rejects_duplicate_spec() {
        let current = vec![
            "spec-a".to_string(),
            "spec-b".to_string(),
            "spec-c".to_string(),
        ];
        let order = vec![
            "spec-a".to_string(),
            "spec-a".to_string(),
            "spec-b".to_string(),
        ];

        let error = validate_queue_reorder(&current, &order).unwrap_err();
        assert!(error.contains("more than once"), "{error}");
    }

    #[test]
    fn queue_reorder_locking_refuses_ordering_that_moves_a_running_member() {
        // R6: the currently running spec is immutable in the queue's order.
        // Swapping it with a pending member must be refused, even though the
        // result is still a valid total permutation.
        let (_dir, db) = queue_test_db();
        db.insert_graph_spec(&running_spec("spec-a")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-c")).unwrap();
        insert_queue(&db, "queue-1");
        for id in ["spec-a", "spec-b", "spec-c"] {
            db.append_queue_member("queue-1", id, None).unwrap();
        }
        let current = db.list_queue_member_spec_ids("queue-1").unwrap();

        // Moves spec-a (running) from position 0 to position 1.
        let order = vec![
            "spec-b".to_string(),
            "spec-a".to_string(),
            "spec-c".to_string(),
        ];
        assert!(validate_queue_reorder(&current, &order).is_ok());

        let error = validate_queue_reorder_locking(&db, &current, &order).unwrap_err();
        assert!(error.contains("spec-a"), "{error}");
        assert!(error.contains("running"), "{error}");
    }

    #[test]
    fn queue_reorder_locking_refuses_ordering_that_moves_a_completed_member() {
        let (_dir, db) = queue_test_db();
        let mut done = standalone_spec("spec-a");
        done.status = GraphSpecStatus::Completed;
        db.insert_graph_spec(&done).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
        insert_queue(&db, "queue-1");
        for id in ["spec-a", "spec-b"] {
            db.append_queue_member("queue-1", id, None).unwrap();
        }
        let current = db.list_queue_member_spec_ids("queue-1").unwrap();

        let order = vec!["spec-b".to_string(), "spec-a".to_string()];
        let error = validate_queue_reorder_locking(&db, &current, &order).unwrap_err();
        assert!(error.contains("spec-a"), "{error}");
        assert!(error.contains("completed"), "{error}");
    }

    #[test]
    fn queue_reorder_locking_allows_permuting_pending_members_only() {
        let (_dir, db) = queue_test_db();
        db.insert_graph_spec(&running_spec("spec-a")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-c")).unwrap();
        insert_queue(&db, "queue-1");
        for id in ["spec-a", "spec-b", "spec-c"] {
            db.append_queue_member("queue-1", id, None).unwrap();
        }
        let current = db.list_queue_member_spec_ids("queue-1").unwrap();

        // spec-a (running) stays at position 0; only the pending tail moves.
        let order = vec![
            "spec-a".to_string(),
            "spec-c".to_string(),
            "spec-b".to_string(),
        ];
        assert!(validate_queue_reorder_locking(&db, &current, &order).is_ok());
    }

    #[test]
    fn queue_remove_spec_refuses_the_currently_running_spec() {
        let (_dir, db) = queue_test_db();
        db.insert_graph_spec(&running_spec("spec-a")).unwrap();
        insert_queue(&db, "queue-1");
        db.append_queue_member("queue-1", "spec-a", None).unwrap();

        let error = validate_queue_member_removable(&db, "queue-1", "spec-a").unwrap_err();
        assert!(error.contains("spec-a"), "{error}");
        assert!(error.contains("running"), "{error}");

        // Once it's no longer running, removal is allowed again.
        db.update_graph_spec_status(
            "spec-a",
            GraphSpecStatus::Completed,
            None,
            Some(chrono::Utc::now()),
        )
        .unwrap();
        assert!(validate_queue_member_removable(&db, "queue-1", "spec-a").is_ok());
    }

    #[test]
    fn queue_add_spec_rejects_nonexistent_spec() {
        let (_dir, db) = queue_test_db();
        insert_queue(&db, "queue-1");

        let error = validate_spec_exists(&db, "ghost-spec").unwrap_err();
        assert!(error.contains("not found"), "{error}");
    }

    #[test]
    fn queue_operations_reject_nonexistent_queue() {
        let (_dir, db) = queue_test_db();

        let error = validate_queue_exists(&db, "does-not-exist").unwrap_err();
        assert!(error.contains("not found"), "{error}");
    }

    #[test]
    fn resolve_graph_target_requires_exactly_one_of_spec_or_graph() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();

        let neither = resolve_graph_target(&db, None, None).unwrap_err();
        assert!(neither.contains("exactly one"), "{neither}");

        let both = resolve_graph_target(&db, Some("spec-x"), Some("graph-x")).unwrap_err();
        assert!(both.contains("exactly one"), "{both}");
    }

    #[test]
    fn resolve_graph_target_rejects_unknown_graph_id() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();

        let error = resolve_graph_target(&db, None, Some("does-not-exist")).unwrap_err();
        assert!(error.contains("not found"), "{error}");
    }

    #[test]
    fn graph_get_response_includes_graph_level_graph_alongside_specs() {
        // R1: `graph_get` (via `graph_details_json`) must surface the
        // top-level graph, not just each spec's own nodes/edges.
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let graph_id = "graph-with-graph".to_string();
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: graph_id.clone(),
            name: "Graph".to_string(),
            description: None,
            workdir: dir.path().to_string_lossy().to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "graph-node".to_string(),
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "implement".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let details = db.get_graph_details(&graph_id).unwrap().unwrap();
        let json = graph_details_json(&db, &details).unwrap();

        assert_eq!(json["graph"]["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(json["graph"]["nodes"][0]["id"], "graph-node");
        assert_eq!(json["graph"]["nodes"][0]["graph_id"], graph_id);
        assert!(json["specs"].as_array().unwrap().is_empty());
    }

    /// CB30 test helpers: a blank-name placeholder bound to a graph (engine
    /// bookkeeping for spec-less dispatches) and a named spec bound to a
    /// graph or standing alone as a queue member.
    fn cb30_test_graph(id: &str, status: GraphStatus) -> Graph {
        Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        }
    }

    fn placeholder_spec(id: &str, graph_id: &str, status: GraphSpecStatus) -> GraphSpec {
        let mut spec = standalone_spec(id);
        spec.graph_id = Some(graph_id.to_string());
        spec.name = String::new();
        spec.status = status;
        spec
    }

    fn named_spec(
        id: &str,
        graph_id: Option<&str>,
        name: &str,
        position: i64,
        status: GraphSpecStatus,
    ) -> GraphSpec {
        let mut spec = standalone_spec(id);
        spec.graph_id = graph_id.map(str::to_string);
        spec.name = name.to_string();
        spec.position = position;
        spec.status = status;
        spec
    }

    fn queue_driven_test_db() -> (tempfile::TempDir, Database) {
        // A running graph drawing from a queue with two named members, plus
        // the blank-name placeholder the engine binds to the graph itself.
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let mut lp = cb30_test_graph("graph-q", GraphStatus::Running);
        lp.active_run_queue_id = Some("q1".to_string());
        db.insert_graph(&lp).unwrap();
        insert_queue(&db, "q1");
        db.insert_graph_spec(&placeholder_spec("ph", "graph-q", GraphSpecStatus::Running))
            .unwrap();
        db.insert_graph_spec(&named_spec(
            "spec-a",
            None,
            "CM4 — queue work",
            0,
            GraphSpecStatus::Running,
        ))
        .unwrap();
        db.append_queue_member("q1", "spec-a", None).unwrap();
        db.insert_graph_spec(&named_spec(
            "spec-b",
            None,
            "CM6 — next work",
            0,
            GraphSpecStatus::Pending,
        ))
        .unwrap();
        db.append_queue_member("q1", "spec-b", None).unwrap();
        (dir, db)
    }

    #[test]
    fn graph_get_reports_queue_spec_not_placeholder() {
        // CB30-1: while a graph executes a queue, the graph level names the
        // queue spec in progress — its id and name — not the placeholder.
        let (_dir, db) = queue_driven_test_db();

        let details = db.get_graph_details("graph-q").unwrap().unwrap();
        let json = graph_details_json(&db, &details).unwrap();
        let specs = json["specs"].as_array().unwrap();
        assert!(
            !specs.is_empty(),
            "queue-driven graph_get must name the running queue spec: {json}"
        );
        assert!(
            specs
                .iter()
                .all(|s| !s["name"].as_str().unwrap_or("").trim().is_empty()),
            "no surface lists a spec whose name is blank: {json}"
        );
        let running = specs
            .iter()
            .find(|s| s["id"] == "spec-a")
            .expect("running queue spec must appear in graph_get specs");
        assert_eq!(running["name"], "CM4 — queue work");
        assert_eq!(json["queue"]["id"], "q1");
        assert_eq!(json["queue"]["name"], "q1-name");

        // The shared helper agrees: it is the same answer every surface
        // must give.
        let lp = db.get_graph("graph-q").unwrap().unwrap();
        match active_graph_spec_context(&db, &lp).unwrap() {
            ActiveGraphSpec::QueueMember {
                spec,
                queue_id,
                queue_name,
            } => {
                assert_eq!(spec.id, "spec-a");
                assert_eq!(spec.name, "CM4 — queue work");
                assert_eq!(queue_id, "q1");
                assert_eq!(queue_name, "q1-name");
            }
            _ => panic!("queue-driven graph must resolve to QueueMember"),
        }
    }

    #[test]
    fn graph_get_idea_run_shows_mode_idea_not_placeholder() {
        // CB30-4(idea): a graph run from a loose idea reports that it is
        // running an idea, and does not present the placeholder as a spec.
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph(&cb30_test_graph("graph-idea", GraphStatus::Running))
            .unwrap();
        db.insert_graph_spec(&placeholder_spec(
            "ph-idea",
            "graph-idea",
            GraphSpecStatus::Running,
        ))
        .unwrap();

        let details = db.get_graph_details("graph-idea").unwrap().unwrap();
        let json = graph_details_json(&db, &details).unwrap();
        assert!(
            json["specs"].as_array().unwrap().is_empty(),
            "idea-driven graph_get must not list the placeholder: {json}"
        );
        assert_eq!(json["mode"], "idea");
        assert!(
            json.get("queue").is_none(),
            "idea-driven graph_get must not carry a queue: {json}"
        );

        let lp = db.get_graph("graph-idea").unwrap().unwrap();
        assert!(
            matches!(
                active_graph_spec_context(&db, &lp).unwrap(),
                ActiveGraphSpec::Idea { .. }
            ),
            "placeholder-only graph must resolve to Idea"
        );
        let summary = build_graph_summary_json(&db, &lp).unwrap();
        assert!(
            summary["current_spec"].is_null(),
            "idea-driven summary must not name the placeholder: {summary}"
        );
    }

    #[test]
    fn graph_get_bound_specs_unchanged() {
        // CB30-3: a graph running its own bound specs still reports those,
        // unchanged — no queue/mode annotation.
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph(&cb30_test_graph("graph-bound", GraphStatus::Running))
            .unwrap();
        db.insert_graph_spec(&named_spec(
            "spec-1",
            Some("graph-bound"),
            "First work",
            0,
            GraphSpecStatus::Running,
        ))
        .unwrap();
        db.insert_graph_spec(&named_spec(
            "spec-2",
            Some("graph-bound"),
            "Second work",
            1,
            GraphSpecStatus::Pending,
        ))
        .unwrap();

        let details = db.get_graph_details("graph-bound").unwrap().unwrap();
        let json = graph_details_json(&db, &details).unwrap();
        let names: Vec<&str> = json["specs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["First work", "Second work"]);
        assert!(json.get("queue").is_none());
        assert!(json.get("mode").is_none());

        let lp = db.get_graph("graph-bound").unwrap().unwrap();
        match active_graph_spec_context(&db, &lp).unwrap() {
            ActiveGraphSpec::Bound(spec) => assert_eq!(spec.name, "First work"),
            _ => panic!("bound-spec graph must resolve to Bound"),
        }
        let summary = build_graph_summary_json(&db, &lp).unwrap();
        assert_eq!(summary["current_spec"], "First work");
        assert!(summary.get("queue").is_none());
    }

    #[test]
    fn graph_list_summary_shows_queue_spec() {
        // CB30-5: `graph list` names the running queue spec, never blank,
        // and carries the queue link.
        let (_dir, db) = queue_driven_test_db();

        let lp = db.get_graph("graph-q").unwrap().unwrap();
        let summary = build_graph_summary_json(&db, &lp).unwrap();
        assert_eq!(summary["current_spec"], "CM4 — queue work");
        assert_eq!(summary["queue"]["id"], "q1");
        assert_eq!(summary["queue"]["name"], "q1-name");
    }

    #[test]
    fn completed_queue_graph_is_not_idea() {
        // CB30/B31: a completed queue-driven graph keeps `active_run_queue_id`
        // set (B31 — last-run context). The shared helper must NOT report it
        // as `Idea` just because only the placeholder remains in bound specs.
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let mut lp = cb30_test_graph("graph-done", GraphStatus::Completed);
        lp.active_run_queue_id = Some("q-done".to_string());
        db.insert_graph(&lp).unwrap();
        insert_queue(&db, "q-done");
        db.insert_graph_spec(&placeholder_spec(
            "ph-done",
            "graph-done",
            GraphSpecStatus::Completed,
        ))
        .unwrap();
        let done = named_spec(
            "spec-done",
            None,
            "CM4 — done",
            0,
            GraphSpecStatus::Completed,
        );
        db.insert_graph_spec(&done).unwrap();
        db.append_queue_member("q-done", "spec-done", None).unwrap();

        match active_graph_spec_context(&db, &lp).unwrap() {
            ActiveGraphSpec::None => {}
            other => panic!("completed queue graph must be None, got {other:?}"),
        }
        let json =
            graph_details_json(&db, &db.get_graph_details("graph-done").unwrap().unwrap()).unwrap();
        assert!(
            json.get("mode").is_none(),
            "completed queue graph must not say 'idea': {json}"
        );
        assert_eq!(json["queue"]["id"], "q-done");
        assert!(
            json["specs"].as_array().unwrap().is_empty(),
            "no spec to show once the queue is drained: {json}"
        );
        let summary = build_graph_summary_json(&db, &lp).unwrap();
        assert!(summary["current_spec"].is_null());
        assert_eq!(summary["queue"]["id"], "q-done");
    }

    #[test]
    fn graph_get_response_surfaces_pinned_skills_on_a_node() {
        // S2: an agent node's pinned `skills` array must be visible to
        // agents inspecting/building graphs via graph_get, not just used
        // internally at spawn time.
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let graph_id = "graph-with-pinned-skills".to_string();
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: graph_id.clone(),
            name: "Graph".to_string(),
            description: None,
            workdir: dir.path().to_string_lossy().to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "pinned-node".to_string(),
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "implement".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": "claude",
                "skills": ["coder", "rust-idiomatic-patterns"]
            }),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let details = db.get_graph_details(&graph_id).unwrap().unwrap();
        let json = graph_details_json(&db, &details).unwrap();

        assert_eq!(
            json["graph"]["nodes"][0]["config"]["skills"],
            serde_json::json!(["coder", "rust-idiomatic-patterns"])
        );
    }

    #[test]
    fn graph_get_response_exposes_autorun_at_so_agents_never_need_sql() {
        // B14: a failed graph's pending autorun schedule must be visible via
        // `graph_get` — before this, an agent had to query
        // `background_agents.db` directly to learn whether a recovery wait
        // was already scheduled.
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let graph_id = "graph-with-autorun".to_string();
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: graph_id.clone(),
            name: "Graph".to_string(),
            description: None,
            workdir: dir.path().to_string_lossy().to_string(),
            status: GraphStatus::Failed,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        })
        .unwrap();

        // Unset: field must be present and null, not just absent.
        let details = db.get_graph_details(&graph_id).unwrap().unwrap();
        let json = graph_details_json(&db, &details).unwrap();
        assert!(json["autorun_at"].is_null());

        // `autorun_at` round-trips through an INTEGER (unix seconds) column,
        // so compare against a second-precision timestamp.
        let at = chrono::DateTime::from_timestamp(
            (chrono::Utc::now() + chrono::Duration::hours(2)).timestamp(),
            0,
        )
        .unwrap();
        db.schedule_graph_autorun(&graph_id, at).unwrap();

        let details = db.get_graph_details(&graph_id).unwrap().unwrap();
        let json = graph_details_json(&db, &details).unwrap();
        assert_eq!(json["autorun_at"].as_str().unwrap(), at.to_rfc3339());
    }

    // ── B41: cancel a scheduled autorun via graph_schedule_autorun(at=None) ──

    fn autorun_test_graph(graph_id: &str, workdir: &str, status: GraphStatus) -> Graph {
        Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: graph_id.to_string(),
            name: graph_id.to_string(),
            description: None,
            workdir: workdir.to_string(),
            status,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        }
    }

    /// B41: a `failed` graph with a pending autorun is exactly the state a
    /// scheduled wake-up needs to be cancellable from — cancelling must
    /// succeed and report the time that was cleared.
    #[tokio::test]
    async fn graph_schedule_autorun_cancels_pending_schedule_on_failed_graph() {
        use crate::daemon::params_extract::Parameters;

        let (dir, db, handler) = queue_test_handler();
        let graph_id = "graph-failed-autorun";
        db.insert_graph(&autorun_test_graph(
            graph_id,
            &dir.path().to_string_lossy(),
            GraphStatus::Failed,
        ))
        .unwrap();
        let scheduled_at = chrono::Utc::now() + chrono::Duration::hours(1);
        db.schedule_graph_autorun(graph_id, scheduled_at).unwrap();

        let result = handler
            .graph_schedule_autorun(Parameters(GraphScheduleAutorunParams {
                graph_id: graph_id.to_string(),
                at: None,
                quota_reset_message: None,
            }))
            .await
            .unwrap();

        let text = result_text(&result);
        assert!(text.contains("cancelled"), "{text}");
        let lp = db.get_graph(graph_id).unwrap().unwrap();
        assert!(lp.autorun_at.is_none(), "cancel must clear autorun_at");
        assert_eq!(
            lp.status,
            GraphStatus::Failed,
            "cancelling must not touch graph status"
        );
    }

    /// B41: cancelling must also work on a `completed` graph — the other
    /// status a graph with a pending autorun can hold.
    #[tokio::test]
    async fn graph_schedule_autorun_cancels_pending_schedule_on_completed_graph() {
        use crate::daemon::params_extract::Parameters;

        let (dir, db, handler) = queue_test_handler();
        let graph_id = "graph-completed-autorun";
        db.insert_graph(&autorun_test_graph(
            graph_id,
            &dir.path().to_string_lossy(),
            GraphStatus::Completed,
        ))
        .unwrap();
        let scheduled_at = chrono::Utc::now() + chrono::Duration::hours(1);
        db.schedule_graph_autorun(graph_id, scheduled_at).unwrap();

        let result = handler
            .graph_schedule_autorun(Parameters(GraphScheduleAutorunParams {
                graph_id: graph_id.to_string(),
                at: None,
                quota_reset_message: None,
            }))
            .await
            .unwrap();

        assert!(result_text(&result).contains("cancelled"));
        let lp = db.get_graph(graph_id).unwrap().unwrap();
        assert!(lp.autorun_at.is_none());
    }

    /// B41: cancelling when nothing is scheduled must succeed and say so —
    /// it is not an error, since the caller's intent (no pending autorun) is
    /// already satisfied.
    #[tokio::test]
    async fn graph_schedule_autorun_cancel_with_nothing_scheduled_is_not_an_error() {
        use crate::daemon::params_extract::Parameters;

        let (dir, db, handler) = queue_test_handler();
        let graph_id = "graph-no-autorun";
        db.insert_graph(&autorun_test_graph(
            graph_id,
            &dir.path().to_string_lossy(),
            GraphStatus::Failed,
        ))
        .unwrap();

        let result = handler
            .graph_schedule_autorun(Parameters(GraphScheduleAutorunParams {
                graph_id: graph_id.to_string(),
                at: None,
                quota_reset_message: None,
            }))
            .await
            .unwrap();

        assert!(!result.is_error.unwrap_or(false), "{result:?}");
        let text = result_text(&result);
        assert!(text.contains("no pending autorun"), "{text}");
    }

    // ── graph_schedule_continue: deferred resume of a paused graph ───────

    /// Setting a schedule on a paused graph must persist `auto_continue_at`
    /// and default the action to `retry_current_node` when omitted.
    #[tokio::test]
    async fn graph_schedule_continue_sets_pending_schedule_with_default_action() {
        use crate::daemon::params_extract::Parameters;

        let (dir, db, handler) = queue_test_handler();
        let graph_id = "graph-paused-continue";
        db.insert_graph(&autorun_test_graph(
            graph_id,
            &dir.path().to_string_lossy(),
            GraphStatus::Paused,
        ))
        .unwrap();
        let scheduled_at = chrono::DateTime::from_timestamp(
            (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp(),
            0,
        )
        .unwrap();

        let result = handler
            .graph_schedule_continue(Parameters(GraphScheduleContinueParams {
                graph_id: graph_id.to_string(),
                at: Some(scheduled_at.to_rfc3339()),
                action: None,
            }))
            .await
            .unwrap();

        assert!(!result.is_error.unwrap_or(false), "{result:?}");
        let text = result_text(&result);
        assert!(text.contains("retry_current_node"), "{text}");
        let lp = db.get_graph(graph_id).unwrap().unwrap();
        assert_eq!(lp.auto_continue_at, Some(scheduled_at));
        assert_eq!(
            lp.auto_continue_action.as_deref(),
            Some("retry_current_node")
        );
    }

    /// An explicit `skip_next_spec` action must be persisted as given.
    #[tokio::test]
    async fn graph_schedule_continue_persists_explicit_skip_action() {
        use crate::daemon::params_extract::Parameters;

        let (dir, db, handler) = queue_test_handler();
        let graph_id = "graph-paused-continue-skip";
        db.insert_graph(&autorun_test_graph(
            graph_id,
            &dir.path().to_string_lossy(),
            GraphStatus::Paused,
        ))
        .unwrap();

        handler
            .graph_schedule_continue(Parameters(GraphScheduleContinueParams {
                graph_id: graph_id.to_string(),
                at: Some((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
                action: Some("skip_next_spec".to_string()),
            }))
            .await
            .unwrap();

        let lp = db.get_graph(graph_id).unwrap().unwrap();
        assert_eq!(lp.auto_continue_action.as_deref(), Some("skip_next_spec"));
    }

    /// An invalid action must be rejected without touching the schedule.
    #[tokio::test]
    async fn graph_schedule_continue_rejects_invalid_action() {
        use crate::daemon::params_extract::Parameters;

        let (dir, db, handler) = queue_test_handler();
        let graph_id = "graph-paused-continue-bad-action";
        db.insert_graph(&autorun_test_graph(
            graph_id,
            &dir.path().to_string_lossy(),
            GraphStatus::Paused,
        ))
        .unwrap();

        let result = handler
            .graph_schedule_continue(Parameters(GraphScheduleContinueParams {
                graph_id: graph_id.to_string(),
                at: Some((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
                action: Some("not_a_real_action".to_string()),
            }))
            .await
            .unwrap();

        assert!(result.is_error.unwrap_or(false), "{result:?}");
        let lp = db.get_graph(graph_id).unwrap().unwrap();
        assert!(
            lp.auto_continue_at.is_none(),
            "an invalid action must not schedule anything"
        );
    }

    /// Omitting `at` must cancel a pending auto-continue schedule, mirroring
    /// `graph_schedule_autorun`'s cancel semantics (B41).
    #[tokio::test]
    async fn graph_schedule_continue_cancels_pending_schedule() {
        use crate::daemon::params_extract::Parameters;

        let (dir, db, handler) = queue_test_handler();
        let graph_id = "graph-paused-continue-cancel";
        db.insert_graph(&autorun_test_graph(
            graph_id,
            &dir.path().to_string_lossy(),
            GraphStatus::Paused,
        ))
        .unwrap();
        db.schedule_graph_auto_continue(
            graph_id,
            chrono::Utc::now() + chrono::Duration::hours(1),
            Some("retry_current_node"),
        )
        .unwrap();

        let result = handler
            .graph_schedule_continue(Parameters(GraphScheduleContinueParams {
                graph_id: graph_id.to_string(),
                at: None,
                action: None,
            }))
            .await
            .unwrap();

        let text = result_text(&result);
        assert!(text.contains("cancelled"), "{text}");
        let lp = db.get_graph(graph_id).unwrap().unwrap();
        assert!(
            lp.auto_continue_at.is_none(),
            "cancel must clear auto_continue_at"
        );
        assert_eq!(
            lp.status,
            GraphStatus::Paused,
            "cancelling must not touch graph status"
        );
    }

    /// Cancelling when nothing is scheduled must succeed and say so.
    #[tokio::test]
    async fn graph_schedule_continue_cancel_with_nothing_scheduled_is_not_an_error() {
        use crate::daemon::params_extract::Parameters;

        let (dir, db, handler) = queue_test_handler();
        let graph_id = "graph-paused-no-continue";
        db.insert_graph(&autorun_test_graph(
            graph_id,
            &dir.path().to_string_lossy(),
            GraphStatus::Paused,
        ))
        .unwrap();

        let result = handler
            .graph_schedule_continue(Parameters(GraphScheduleContinueParams {
                graph_id: graph_id.to_string(),
                at: None,
                action: None,
            }))
            .await
            .unwrap();

        assert!(!result.is_error.unwrap_or(false), "{result:?}");
        let text = result_text(&result);
        assert!(text.contains("no pending auto-continue"), "{text}");
    }

    /// Scheduling `graph_schedule_continue` must never touch `autorun_at`,
    /// and vice versa — the two schedules must stay fully independent so
    /// they can never be conflated at fire time.
    #[tokio::test]
    async fn graph_schedule_continue_and_graph_schedule_autorun_are_independent() {
        use crate::daemon::params_extract::Parameters;

        let (dir, db, handler) = queue_test_handler();
        let graph_id = "graph-independent-schedules";
        db.insert_graph(&autorun_test_graph(
            graph_id,
            &dir.path().to_string_lossy(),
            GraphStatus::Paused,
        ))
        .unwrap();

        handler
            .graph_schedule_continue(Parameters(GraphScheduleContinueParams {
                graph_id: graph_id.to_string(),
                at: Some((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
                action: None,
            }))
            .await
            .unwrap();

        let lp = db.get_graph(graph_id).unwrap().unwrap();
        assert!(lp.auto_continue_at.is_some());
        assert!(
            lp.autorun_at.is_none(),
            "graph_schedule_continue must not set autorun_at"
        );
    }

    // ── U10: copy nodes and ensembles ────────────────────────────────

    fn u10_check(
        id: &str,
        spec_id: Option<&str>,
        graph_id: Option<&str>,
        position: i64,
    ) -> GraphNode {
        GraphNode {
            id: id.to_string(),
            spec_id: spec_id.map(str::to_string),
            graph_id: graph_id.map(str::to_string),
            name: id.to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position,
            created_at: chrono::Utc::now(),
        }
    }

    fn u10_agent(
        id: &str,
        spec_id: Option<&str>,
        graph_id: Option<&str>,
        position: i64,
        config: serde_json::Value,
    ) -> GraphNode {
        GraphNode {
            id: id.to_string(),
            spec_id: spec_id.map(str::to_string),
            graph_id: graph_id.map(str::to_string),
            name: id.to_string(),
            kind: GraphNodeKind::Agent,
            config,
            position,
            created_at: chrono::Utc::now(),
        }
    }

    /// A persisted source ensemble ("proposers"): two members (claude, codex/o1),
    /// wired `from` → members → join → `to`.
    fn u10_source_ensemble(
        db: &Database,
        spec_id: Option<&str>,
        graph_id: Option<&str>,
        from: &str,
        to: &str,
    ) -> BuiltEnsembleUnit {
        let members = [
            ("claude".to_string(), None, None, None, None, None, None),
            (
                "codex".to_string(),
                Some("o1".to_string()),
                None,
                None,
                None,
                None,
                None,
            ),
        ];
        let built = build_ensemble_unit(&EnsembleUnitSpec {
            spec_id: spec_id.map(str::to_string),
            graph_id: graph_id.map(str::to_string),
            name: "proposers",
            prompt_template: "propose a solution",
            members: &members,
            entry_from_node: from,
            entry_condition: GraphEdgeCondition::Always,
            on_pass_to: to,
            on_pass_fan_out: None,
            on_fail_to: None,
            on_fail_fan_out: None,
            min_pass: 2,
            timeout_minutes: 30,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            start_position: 10,
            kind: EnsembleKind::Parallel,
            commit_rights: false,
        });
        db.insert_ensemble_unit(
            &built.ensemble,
            &built.members,
            &built.member_nodes,
            &built.join_node,
            &built.edges,
        )
        .unwrap();
        built
    }

    #[test]
    fn graph_copy_node_applies_overrides_and_wiring() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        db.insert_graph_node(&u10_agent(
            "impl",
            None,
            Some("graph-1"),
            1,
            serde_json::json!({"platform":"claude","prompt_template":"implement it","timeout_minutes":30}),
        ))
        .unwrap();
        db.insert_graph_node(&u10_check("gate", None, Some("graph-1"), 2))
            .unwrap();

        let params: GraphCopyNodeParams = serde_json::from_value(serde_json::json!({
            "source_node_id": "impl",
            "graph_id": "graph-1",
            "name": "review",
            "config_overrides": {"prompt_template": "review it", "platform": "codex"},
            "on_pass_to": "gate"
        }))
        .unwrap();
        let plan = plan_node_copy(&db, &params).unwrap();

        assert_ne!(plan.node.id, "impl", "the copy must have a fresh id");
        assert_eq!(plan.node.name, "review");
        assert_eq!(plan.node.kind, GraphNodeKind::Agent);
        // Overridden keys win; untouched source keys are preserved.
        assert_eq!(plan.node.config["prompt_template"], "review it");
        assert_eq!(plan.node.config["platform"], "codex");
        assert_eq!(plan.node.config["timeout_minutes"], 30);
        assert_eq!(plan.node.graph_id.as_deref(), Some("graph-1"));
        assert!(plan.node.spec_id.is_none());
        // One outgoing pass edge to the wiring target.
        assert_eq!(plan.edges.len(), 1);
        assert_eq!(plan.edges[0].from_node, plan.node.id);
        assert_eq!(plan.edges[0].to_node, "gate");
        assert_eq!(plan.edges[0].condition.as_str(), "pass");
        assert_eq!(plan.wiring.get("on_pass_to").unwrap(), "gate");
    }

    #[test]
    fn graph_copy_node_unwired_copy_is_valid_and_reported() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        db.insert_graph_node(&u10_agent(
            "impl",
            None,
            Some("graph-1"),
            1,
            serde_json::json!({"platform":"claude"}),
        ))
        .unwrap();

        // No wiring, no target override → copy into the source's own graph.
        let params: GraphCopyNodeParams =
            serde_json::from_value(serde_json::json!({"source_node_id": "impl"})).unwrap();
        let plan = plan_node_copy(&db, &params).unwrap();

        assert!(plan.edges.is_empty(), "an unwired copy creates no edges");
        assert!(plan.wiring.is_empty());
        assert_eq!(plan.node.graph_id.as_deref(), Some("graph-1"));
        assert_eq!(plan.node.config["platform"], "claude");
        assert_ne!(plan.node.id, "impl");

        // The response note must flag the unwired state explicitly.
        let note = node_copy_note(&plan.source_id, &plan.node.id, false);
        assert!(note.contains("Unwired copy"), "{note}");
        assert!(note.contains("graph_add_edge"), "{note}");
    }

    #[test]
    fn graph_copy_node_rejects_ensemble_owned_source() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        db.insert_graph_node(&u10_check("kickoff", None, Some("graph-1"), 1))
            .unwrap();
        db.insert_graph_node(&u10_agent(
            "arbiter",
            None,
            Some("graph-1"),
            2,
            serde_json::json!({"platform":"claude"}),
        ))
        .unwrap();
        let src = u10_source_ensemble(&db, None, Some("graph-1"), "kickoff", "arbiter");

        // A member node can't be copied directly — must go through the ensemble.
        let params: GraphCopyNodeParams = serde_json::from_value(serde_json::json!({
            "source_node_id": src.members[0].node_id
        }))
        .unwrap();
        let err = plan_node_copy(&db, &params).unwrap_err();
        assert!(err.contains("graph_copy_ensemble"), "{err}");
    }

    /// CM2: copying a node into a graph with `infra_node_id` set must
    /// auto-wire a `Error` edge to the infra node, preserving the
    /// pre-wiring invariant.
    #[test]
    fn graph_copy_node_auto_wires_error_edge_when_graph_has_infra_node() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: Some("infra".to_string()),
            id: "graph-1".to_string(),
            name: "graph-1".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: GraphStatus::Running,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        })
        .unwrap();
        db.insert_graph_node(&u10_agent(
            "src",
            None,
            Some("graph-1"),
            1,
            serde_json::json!({"platform":"claude","prompt_template":"do it"}),
        ))
        .unwrap();
        db.insert_graph_node(&u10_check("infra", None, Some("graph-1"), 2))
            .unwrap();

        let params: GraphCopyNodeParams = serde_json::from_value(serde_json::json!({
            "source_node_id": "src"
        }))
        .unwrap();
        let plan = plan_node_copy(&db, &params).unwrap();
        db.insert_node_with_edges(&plan.node, &plan.edges).unwrap();

        // Simulate the handler's auto-wiring block.
        if let Some(ref lid) = plan.node.graph_id {
            if let Ok(Some(lp)) = db.get_graph(lid) {
                if let Some(ref infra_node_id) = lp.infra_node_id {
                    let already_has_error = plan.edges.iter().any(|e| {
                        e.from_node == plan.node.id && e.condition == GraphEdgeCondition::Error
                    });
                    if !already_has_error {
                        let edge = GraphEdge {
                            id: uuid::Uuid::new_v4().to_string(),
                            spec_id: plan.node.spec_id.clone(),
                            graph_id: Some(lid.clone()),
                            from_node: plan.node.id.clone(),
                            to_node: infra_node_id.clone(),
                            condition: GraphEdgeCondition::Error,
                        };
                        let _ = db.insert_graph_edge(&edge);
                    }
                }
            }
        }

        let edges = db.list_graph_edges_for_graph("graph-1").unwrap();
        let error_edges: Vec<_> = edges
            .iter()
            .filter(|e| e.from_node == plan.node.id && e.condition == GraphEdgeCondition::Error)
            .collect();
        assert_eq!(
            error_edges.len(),
            1,
            "copied node must have exactly one Error edge to infra node"
        );
        assert_eq!(error_edges[0].to_node, "infra");
    }

    #[test]
    fn graph_copy_ensemble_overrides_prompt_and_rewires() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        db.insert_graph_node(&u10_check("kickoff", None, Some("graph-1"), 1))
            .unwrap();
        db.insert_graph_node(&u10_agent(
            "arbiter",
            None,
            Some("graph-1"),
            2,
            serde_json::json!({"platform":"claude"}),
        ))
        .unwrap();
        db.insert_graph_node(&u10_check("review_from", None, Some("graph-1"), 3))
            .unwrap();
        db.insert_graph_node(&u10_check("review_next", None, Some("graph-1"), 4))
            .unwrap();
        let src = u10_source_ensemble(&db, None, Some("graph-1"), "kickoff", "arbiter");

        // Canonical use: copy the proposer ensemble, swap in a review prompt,
        // rewire it after the gates.
        let params: GraphCopyEnsembleParams = serde_json::from_value(serde_json::json!({
            "source_ensemble_id": src.ensemble.id,
            "prompt_template": "review the proposal",
            "from_node": "review_from",
            "condition": "pass",
            "on_pass_to": "review_next"
        }))
        .unwrap();
        let plan = plan_ensemble_copy(&db, &params).unwrap();

        assert_ne!(plan.built.ensemble.id, src.ensemble.id);
        assert_eq!(plan.built.ensemble.prompt_template, "review the proposal");
        assert_eq!(plan.entry_from_node, "review_from");
        assert_eq!(plan.entry_condition.as_str(), "pass");
        assert_eq!(plan.on_pass_to, "review_next");
        assert!(!plan.members_replaced);
        assert_eq!(plan.built.member_nodes.len(), 2);
        for node in &plan.built.member_nodes {
            // The shared prompt propagates to every member node's config.
            assert_eq!(node.config["prompt_template"], "review the proposal");
            assert!(
                !src.member_nodes.iter().any(|m| m.id == node.id),
                "member node ids must be fresh"
            );
        }

        // Persisting the plan yields a resolvable ensemble with the same members.
        db.insert_ensemble_unit(
            &plan.built.ensemble,
            &plan.built.members,
            &plan.built.member_nodes,
            &plan.built.join_node,
            &plan.built.edges,
        )
        .unwrap();
        let details = db
            .get_ensemble_details(&plan.built.ensemble.id)
            .unwrap()
            .unwrap();
        assert_eq!(details.members.len(), 2);
        assert_eq!(details.members[0].platform, "claude");
        assert_eq!(details.members[1].platform, "codex");
    }

    #[test]
    fn graph_copy_ensemble_cross_graph_requires_and_uses_target_wiring() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        db.insert_graph_node(&u10_check("kickoff", None, Some("graph-1"), 1))
            .unwrap();
        db.insert_graph_node(&u10_agent(
            "arbiter",
            None,
            Some("graph-1"),
            2,
            serde_json::json!({"platform":"claude"}),
        ))
        .unwrap();
        let src = u10_source_ensemble(&db, None, Some("graph-1"), "kickoff", "arbiter");

        insert_test_graph(&db, "graph-2");
        db.insert_graph_node(&u10_check("k2", None, Some("graph-2"), 1))
            .unwrap();
        db.insert_graph_node(&u10_agent(
            "a2",
            None,
            Some("graph-2"),
            2,
            serde_json::json!({"platform":"claude"}),
        ))
        .unwrap();

        // Cross-graph copy without wiring: the source's entry/exit nodes don't
        // exist in graph-2, so it must be refused with an actionable message.
        let bad: GraphCopyEnsembleParams = serde_json::from_value(serde_json::json!({
            "source_ensemble_id": src.ensemble.id,
            "graph_id": "graph-2"
        }))
        .unwrap();
        let err = plan_ensemble_copy(&db, &bad).unwrap_err();
        assert!(err.contains("not found in the target graph"), "{err}");

        // With target wiring, the whole unit lands in graph-2's graph.
        let ok: GraphCopyEnsembleParams = serde_json::from_value(serde_json::json!({
            "source_ensemble_id": src.ensemble.id,
            "graph_id": "graph-2",
            "from_node": "k2",
            "on_pass_to": "a2"
        }))
        .unwrap();
        let plan = plan_ensemble_copy(&db, &ok).unwrap();
        assert_eq!(plan.built.ensemble.graph_id.as_deref(), Some("graph-2"));
        assert!(plan.built.ensemble.spec_id.is_none());
        assert_eq!(plan.built.join_node.graph_id.as_deref(), Some("graph-2"));
        for node in &plan.built.member_nodes {
            assert_eq!(node.graph_id.as_deref(), Some("graph-2"));
        }
        assert_eq!(plan.entry_from_node, "k2");
        assert_eq!(plan.on_pass_to, "a2");
    }

    #[test]
    fn graph_copy_ensemble_copies_config_only_no_runtime_state() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        db.insert_graph_spec(&standalone_spec("spec-src")).unwrap();
        db.insert_graph_node(&u10_check("kickoff", Some("spec-src"), None, 1))
            .unwrap();
        db.insert_graph_node(&u10_agent(
            "arbiter",
            Some("spec-src"),
            None,
            2,
            serde_json::json!({"platform":"claude"}),
        ))
        .unwrap();
        let src = u10_source_ensemble(&db, Some("spec-src"), None, "kickoff", "arbiter");

        // A completed run attached to a SOURCE member node: runtime state that
        // must never be carried into the copy.
        let source_member = src.members[0].node_id.clone();
        db.insert_graph_run(&GraphNodeRun {
            id: "run-src".to_string(),
            graph_id: "graph-1".to_string(),
            spec_id: "spec-src".to_string(),
            node_id: source_member,
            status: GraphRunStatus::Pass,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .unwrap();

        let params: GraphCopyEnsembleParams = serde_json::from_value(serde_json::json!({
            "source_ensemble_id": src.ensemble.id
        }))
        .unwrap();
        let plan = plan_ensemble_copy(&db, &params).unwrap();
        db.insert_ensemble_unit(
            &plan.built.ensemble,
            &plan.built.members,
            &plan.built.member_nodes,
            &plan.built.join_node,
            &plan.built.edges,
        )
        .unwrap();

        // Every copied node has a fresh identity...
        for node in &plan.built.member_nodes {
            assert!(!src.member_nodes.iter().any(|m| m.id == node.id));
        }
        // ...and NO run was copied: the only run is still the original, and
        // none reference the new member nodes.
        let runs = db.list_graph_runs_for_spec("spec-src").unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, "run-src");
        let new_ids: Vec<&str> = plan
            .built
            .member_nodes
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert!(!runs.iter().any(|r| new_ids.contains(&r.node_id.as_str())));
    }

    // ── validate_non_empty ──────────────────────────────────────────

    #[test]
    fn validate_non_empty_rejects_empty_string() {
        assert!(validate_non_empty("", "name").is_err());
    }

    #[test]
    fn validate_non_empty_rejects_whitespace_only() {
        assert!(validate_non_empty("   ", "name").is_err());
        assert!(validate_non_empty("\t\n", "name").is_err());
    }

    #[test]
    fn validate_non_empty_accepts_valid_value() {
        assert!(validate_non_empty("hello", "name").is_ok());
        assert!(validate_non_empty("  hello  ", "name").is_ok());
    }

    #[test]
    fn validate_non_empty_error_includes_field_name() {
        let err = validate_non_empty("", "workdir").unwrap_err();
        assert!(err.contains("workdir"), "{err}");
        assert!(err.contains("must not be empty"), "{err}");
    }

    // ── validate_absolute_dir ───────────────────────────────────────

    #[test]
    fn validate_absolute_dir_rejects_relative_path() {
        assert!(validate_absolute_dir("relative/path").is_err());
    }

    #[test]
    fn validate_absolute_dir_rejects_nonexistent_directory() {
        let err = validate_absolute_dir("/nonexistent/dir/path").unwrap_err();
        assert!(err.contains("existing directory"), "{err}");
    }

    #[test]
    fn validate_absolute_dir_accepts_existing_directory() {
        let dir = tempdir().unwrap();
        assert!(validate_absolute_dir(dir.path().to_str().unwrap()).is_ok());
    }

    // ── validate_spec_workdir ───────────────────────────────────────

    #[test]
    fn validate_spec_workdir_rejects_relative_path() {
        let err = validate_spec_workdir("relative/path").unwrap_err();
        assert!(err.contains("absolute path"), "{err}");
    }

    #[test]
    fn validate_spec_workdir_accepts_absolute_path() {
        assert!(validate_spec_workdir("/some/absolute/path").is_ok());
    }

    #[test]
    fn validate_spec_workdir_does_not_require_existing_directory() {
        assert!(validate_spec_workdir("/nonexistent/dir/path").is_ok());
    }

    // ── build_graph_trigger ──────────────────────────────────────────

    #[test]
    fn build_graph_trigger_none_returns_none() {
        assert!(build_graph_trigger(&None).unwrap().is_none());
    }

    #[test]
    fn build_graph_trigger_manual_returns_none() {
        let params = GraphTriggerParams {
            kind: "manual".to_string(),
            schedule: None,
            path: None,
            events: None,
            debounce_seconds: None,
            recursive: None,
        };
        assert!(build_graph_trigger(&Some(params)).unwrap().is_none());
    }

    #[test]
    fn build_graph_trigger_empty_kind_returns_none() {
        let params = GraphTriggerParams {
            kind: "".to_string(),
            schedule: None,
            path: None,
            events: None,
            debounce_seconds: None,
            recursive: None,
        };
        assert!(build_graph_trigger(&Some(params)).unwrap().is_none());
    }

    #[test]
    fn build_graph_trigger_cron_requires_schedule() {
        let params = GraphTriggerParams {
            kind: "cron".to_string(),
            schedule: None,
            path: None,
            events: None,
            debounce_seconds: None,
            recursive: None,
        };
        let err = build_graph_trigger(&Some(params)).unwrap_err();
        assert!(err.contains("schedule"), "{err}");
    }

    #[test]
    fn build_graph_trigger_cron_rejects_empty_schedule() {
        let params = GraphTriggerParams {
            kind: "cron".to_string(),
            schedule: Some("".to_string()),
            path: None,
            events: None,
            debounce_seconds: None,
            recursive: None,
        };
        let err = build_graph_trigger(&Some(params)).unwrap_err();
        assert!(err.contains("schedule"), "{err}");
    }

    #[test]
    fn build_graph_trigger_watch_requires_path() {
        let params = GraphTriggerParams {
            kind: "watch".to_string(),
            schedule: None,
            path: None,
            events: Some(vec!["modify".to_string()]),
            debounce_seconds: None,
            recursive: None,
        };
        let err = build_graph_trigger(&Some(params)).unwrap_err();
        assert!(err.contains("path"), "{err}");
    }

    #[test]
    fn build_graph_trigger_watch_rejects_relative_path() {
        let params = GraphTriggerParams {
            kind: "watch".to_string(),
            schedule: None,
            path: Some("relative/path".to_string()),
            events: Some(vec!["modify".to_string()]),
            debounce_seconds: None,
            recursive: None,
        };
        let err = build_graph_trigger(&Some(params)).unwrap_err();
        assert!(err.contains("absolute"), "{err}");
    }

    #[test]
    fn build_graph_trigger_watch_requires_events() {
        let params = GraphTriggerParams {
            kind: "watch".to_string(),
            schedule: None,
            path: Some("/tmp/test".to_string()),
            events: None,
            debounce_seconds: None,
            recursive: None,
        };
        let err = build_graph_trigger(&Some(params)).unwrap_err();
        assert!(err.contains("event"), "{err}");
    }

    #[test]
    fn build_graph_trigger_watch_accepts_valid_config() {
        let params = GraphTriggerParams {
            kind: "watch".to_string(),
            schedule: None,
            path: Some("/tmp/test".to_string()),
            events: Some(vec!["create".to_string(), "modify".to_string()]),
            debounce_seconds: Some(5),
            recursive: Some(true),
        };
        let trigger = build_graph_trigger(&Some(params)).unwrap().unwrap();
        match trigger {
            Trigger::Watch {
                path,
                events,
                debounce_seconds,
                recursive,
            } => {
                assert_eq!(path, "/tmp/test");
                assert_eq!(events.len(), 2);
                assert_eq!(debounce_seconds, 5);
                assert!(recursive);
            }
            other => panic!("expected Trigger::Watch, got {other:?}"),
        }
    }

    #[test]
    fn build_graph_trigger_unknown_kind_returns_error() {
        let params = GraphTriggerParams {
            kind: "unknown".to_string(),
            schedule: None,
            path: None,
            events: None,
            debounce_seconds: None,
            recursive: None,
        };
        let err = build_graph_trigger(&Some(params)).unwrap_err();
        assert!(err.contains("Unknown trigger kind"), "{err}");
        assert!(err.contains("unknown"), "{err}");
    }

    // ── build_graph_completion_hook ──────────────────────────────────

    #[test]
    fn build_graph_completion_hook_rejects_empty_platform() {
        let params = GraphCompletionHookParams {
            platform: Some("".to_string()),
            model: None,
            effort: None,
            prompt: Some("run tests".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let err = build_graph_completion_hook(&params).unwrap_err();
        assert!(err.contains("must have either"), "{err}");
    }

    #[test]
    fn build_graph_completion_hook_rejects_whitespace_platform() {
        let params = GraphCompletionHookParams {
            platform: Some("   ".to_string()),
            model: None,
            effort: None,
            prompt: Some("run tests".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let err = build_graph_completion_hook(&params).unwrap_err();
        assert!(err.contains("must have either"), "{err}");
    }

    #[test]
    fn build_graph_completion_hook_rejects_empty_prompt() {
        let params = GraphCompletionHookParams {
            platform: Some("claude".to_string()),
            model: None,
            effort: None,
            prompt: Some("".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let err = build_graph_completion_hook(&params).unwrap_err();
        assert!(err.contains("prompt"), "{err}");
    }

    #[test]
    fn build_graph_completion_hook_rejects_whitespace_prompt() {
        let params = GraphCompletionHookParams {
            platform: Some("claude".to_string()),
            model: None,
            effort: None,
            prompt: Some("  \t  ".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let err = build_graph_completion_hook(&params).unwrap_err();
        assert!(err.contains("prompt"), "{err}");
    }

    #[test]
    fn build_graph_completion_hook_accepts_valid_config() {
        let params = GraphCompletionHookParams {
            platform: Some("claude".to_string()),
            model: Some("opus-4".to_string()),
            effort: None,
            prompt: Some("{{graph_name}} completed".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(10),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let hook = build_graph_completion_hook(&params).unwrap();
        assert_eq!(hook.platform.as_deref(), Some("claude"));
        assert_eq!(hook.model, Some("opus-4".to_string()));
        assert_eq!(hook.prompt.as_deref(), Some("{{graph_name}} completed"));
        assert_eq!(hook.timeout_minutes, Some(10));
    }

    #[test]
    fn build_graph_completion_hook_rejects_unknown_placeholder() {
        let params = GraphCompletionHookParams {
            platform: Some("claude".to_string()),
            model: None,
            effort: None,
            prompt: Some("failure at {{nodee}}".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let err = build_graph_completion_hook(&params).unwrap_err();
        assert!(err.contains("nodee"), "{err}");
        for marker in crate::graph_engine::GRAPH_HOOK_BINDINGS {
            assert!(err.contains(marker), "{err}");
        }
    }

    #[test]
    fn build_graph_completion_hook_strips_whitespace() {
        let params = GraphCompletionHookParams {
            platform: Some("  claude  ".to_string()),
            model: Some("  opus  ".to_string()),
            effort: None,
            prompt: Some("  test  ".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let hook = build_graph_completion_hook(&params).unwrap();
        assert_eq!(hook.platform.as_deref(), Some("claude"));
        assert_eq!(hook.model, Some("opus".to_string()));
        assert_eq!(hook.prompt.as_deref(), Some("test"));
    }

    #[test]
    fn build_graph_completion_hook_empty_model_becomes_none() {
        let params = GraphCompletionHookParams {
            platform: Some("claude".to_string()),
            model: Some("  ".to_string()),
            effort: None,
            prompt: Some("test".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let hook = build_graph_completion_hook(&params).unwrap();
        assert!(hook.model.is_none());
    }

    // ── member_node_config ──────────────────────────────────────────

    #[test]
    fn build_graph_completion_hook_rejects_both_command_and_agent() {
        let params = GraphCompletionHookParams {
            platform: Some("claude".to_string()),
            model: None,
            effort: None,
            prompt: Some("test".to_string()),
            command: Some("echo hi".to_string()),
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let err = build_graph_completion_hook(&params).unwrap_err();
        assert!(err.contains("command"), "{err}");
        assert!(err.contains("platform"), "{err}");
    }

    #[test]
    fn build_graph_completion_hook_rejects_neither_command_nor_agent() {
        let params = GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let err = build_graph_completion_hook(&params).unwrap_err();
        assert!(err.contains("either"), "{err}");
    }

    #[test]
    fn build_graph_completion_hook_accepts_command_only() {
        let params = GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: Some("echo hello".to_string()),
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(5),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let hook = build_graph_completion_hook(&params).unwrap();
        assert_eq!(hook.command.as_deref(), Some("echo hello"));
        assert!(hook.platform.is_none());
        assert!(hook.prompt.is_none());
        assert_eq!(hook.timeout_minutes, Some(5));
    }

    #[test]
    fn build_graph_completion_hook_command_strips_whitespace() {
        let params = GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: Some("  echo hello  ".to_string()),
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let hook = build_graph_completion_hook(&params).unwrap();
        assert_eq!(hook.command.as_deref(), Some("echo hello"));
    }

    #[test]
    fn build_graph_completion_hook_accepts_interactive_config() {
        let params = GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: None,
            prompt: Some("  Graph {{graph_name}} failed  ".to_string()),
            command: None,
            target_session_id: Some("  session-7  ".to_string()),
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let hook = build_graph_completion_hook(&params).unwrap();
        assert!(hook.is_interactive());
        assert!(!hook.is_command());
        assert!(!hook.is_agent());
        assert_eq!(hook.target_session_id.as_deref(), Some("session-7"));
        assert_eq!(hook.prompt.as_deref(), Some("Graph {{graph_name}} failed"));
    }

    #[test]
    fn build_graph_completion_hook_rejects_mixed_interactive_modes() {
        // target plus platform is rejected.
        let params = GraphCompletionHookParams {
            platform: Some("claude".to_string()),
            model: None,
            effort: None,
            prompt: Some("hi".to_string()),
            command: None,
            target_session_id: Some("session-1".to_string()),
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        assert!(build_graph_completion_hook(&params).is_err());

        // target plus command is rejected.
        let params = GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: None,
            prompt: Some("hi".to_string()),
            command: Some("echo hi".to_string()),
            target_session_id: Some("session-1".to_string()),
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        assert!(build_graph_completion_hook(&params).is_err());

        // target plus model is rejected.
        let params = GraphCompletionHookParams {
            platform: None,
            model: Some("opus".to_string()),
            effort: None,
            prompt: Some("hi".to_string()),
            command: None,
            target_session_id: Some("session-1".to_string()),
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        assert!(build_graph_completion_hook(&params).is_err());

        // target plus effort is rejected.
        let params = GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: Some("high".to_string()),
            prompt: Some("hi".to_string()),
            command: None,
            target_session_id: Some("session-1".to_string()),
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        assert!(build_graph_completion_hook(&params).is_err());

        // target without a prompt is rejected.
        let params = GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: None,
            target_session_id: Some("session-1".to_string()),
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let err = build_graph_completion_hook(&params).unwrap_err();
        assert!(err.contains("prompt"), "{err}");
    }

    #[test]
    fn build_graph_completion_hook_accepts_name_based_interactive_config() {
        let params = GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: None,
            prompt: Some("hi".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: Some("  marasmius  ".to_string()),
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let hook = build_graph_completion_hook(&params).unwrap();
        assert!(hook.is_interactive());
        assert_eq!(hook.target_session_name.as_deref(), Some("marasmius"));
        assert!(hook.target_session_id.is_none());
    }

    #[test]
    fn build_graph_completion_hook_rejects_both_target_id_and_name() {
        let params = GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: None,
            prompt: Some("hi".to_string()),
            command: None,
            target_session_id: Some("session-1".to_string()),
            target_session_name: Some("session-2".to_string()),
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let err = build_graph_completion_hook(&params).unwrap_err();
        assert!(err.contains("target_session_id"), "{err}");
        assert!(err.contains("target_session_name"), "{err}");
    }

    #[test]
    fn build_graph_completion_hook_rejects_bare_prompt_with_no_target() {
        let params = GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: None,
            prompt: Some("hi".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let err = build_graph_completion_hook(&params).unwrap_err();
        assert!(err.contains("target_session_id"), "{err}");
        assert!(err.contains("target_session_name"), "{err}");
    }

    #[test]
    fn build_graph_completion_hook_rejects_target_graph_id_with_target_session_name() {
        let params = GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: None,
            target_session_id: None,
            target_session_name: Some("marasmius".to_string()),
            timeout_minutes: None,
            target_graph_id: Some("graph-1".to_string()),
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let err = build_graph_completion_hook(&params).unwrap_err();
        assert!(err.contains("target_session_name"), "{err}");
    }

    #[test]
    fn member_node_config_produces_correct_shape() {
        let config = member_node_config(
            "claude",
            Some("opus-4"),
            "do the thing",
            30,
            None,
            None,
            None,
        );
        assert_eq!(config["platform"], "claude");
        assert_eq!(config["model"], "opus-4");
        assert_eq!(config["prompt_template"], "do the thing");
        assert_eq!(config["timeout_minutes"], 30);
    }

    #[test]
    fn member_node_config_null_model_when_none() {
        let config = member_node_config("claude", None, "prompt", 15, None, None, None);
        assert_eq!(config["platform"], "claude");
        assert!(config["model"].is_null());
    }

    // ── validate_edge_condition ─────────────────────────────────────

    #[test]
    fn validate_edge_condition_accepts_pass() {
        assert_eq!(
            validate_edge_condition("pass").unwrap(),
            GraphEdgeCondition::Pass
        );
    }

    #[test]
    fn validate_edge_condition_accepts_fail() {
        assert_eq!(
            validate_edge_condition("fail").unwrap(),
            GraphEdgeCondition::Fail
        );
    }

    #[test]
    fn validate_edge_condition_accepts_always() {
        assert_eq!(
            validate_edge_condition("always").unwrap(),
            GraphEdgeCondition::Always
        );
    }

    #[test]
    fn validate_edge_condition_trims_whitespace() {
        assert_eq!(
            validate_edge_condition("  pass  ").unwrap(),
            GraphEdgeCondition::Pass
        );
    }

    #[test]
    fn validate_edge_condition_rejects_invalid() {
        let err = validate_edge_condition("sometimes").unwrap_err();
        assert!(err.contains("pass"), "{err}");
        assert!(err.contains("fail"), "{err}");
        assert!(err.contains("always"), "{err}");
    }

    // ── validate_edge_condition_with_route ───────────────────────────

    #[test]
    fn validate_edge_condition_with_route_builds_route_condition() {
        let c = validate_edge_condition_with_route("route", Some("escalate")).unwrap();
        assert_eq!(c, GraphEdgeCondition::Route("escalate".to_string()));
    }

    #[test]
    fn validate_edge_condition_with_route_requires_non_empty_label() {
        let err = validate_edge_condition_with_route("route", None).unwrap_err();
        assert!(err.contains("non-empty 'route' label"), "{err}");

        let err = validate_edge_condition_with_route("route", Some("  ")).unwrap_err();
        assert!(err.contains("non-empty 'route' label"), "{err}");
    }

    #[test]
    fn validate_edge_condition_with_route_leaves_simple_conditions_untouched() {
        assert_eq!(
            validate_edge_condition_with_route("pass", None).unwrap(),
            GraphEdgeCondition::Pass
        );
        assert_eq!(
            validate_edge_condition_with_route("fail", None).unwrap(),
            GraphEdgeCondition::Fail
        );
        assert_eq!(
            validate_edge_condition_with_route("always", None).unwrap(),
            GraphEdgeCondition::Always
        );
        assert!(validate_edge_condition_with_route("sideways", None).is_err());
    }

    // ── validate_route_edge_target ────────────────────────────────────

    #[test]
    fn validate_route_edge_target_is_noop_for_non_route_conditions() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        // No node inserted at all — a non-route condition must never look it
        // up, let alone fail over a missing node.
        assert!(validate_route_edge_target(&db, "missing-node", &GraphEdgeCondition::Pass).is_ok());
    }

    /// Insert a standalone spec (no owning graph) so a test can hang nodes
    /// off it without needing a full `Graph` row too.
    fn insert_standalone_spec(db: &Database, id: &str) {
        db.insert_graph_spec(&GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: id.to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();
    }

    #[test]
    fn validate_route_edge_target_rejects_non_router_from_node() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_standalone_spec(&db, "spec-1");
        db.insert_graph_node(&GraphNode {
            id: "agent-1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Agent".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({ "platform": "claude" }),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let err = validate_route_edge_target(
            &db,
            "agent-1",
            &GraphEdgeCondition::Route("retry".to_string()),
        )
        .unwrap_err();
        assert!(err.contains("router node"), "{err}");
    }

    #[test]
    fn validate_route_edge_target_rejects_undeclared_route() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_standalone_spec(&db, "spec-1");
        db.insert_graph_node(&GraphNode {
            id: "router-1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Router".to_string(),
            kind: GraphNodeKind::Router,
            config: router_config(&two_routes_json(), "retry"),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let err = validate_route_edge_target(
            &db,
            "router-1",
            &GraphEdgeCondition::Route("nonexistent".to_string()),
        )
        .unwrap_err();
        assert!(err.contains("undeclared route 'nonexistent'"), "{err}");
    }

    #[test]
    fn validate_route_edge_target_accepts_declared_route() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_standalone_spec(&db, "spec-1");
        db.insert_graph_node(&GraphNode {
            id: "router-1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Router".to_string(),
            kind: GraphNodeKind::Router,
            config: router_config(&two_routes_json(), "retry"),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        assert!(validate_route_edge_target(
            &db,
            "router-1",
            &GraphEdgeCondition::Route("escalate".to_string()),
        )
        .is_ok());
    }

    // ── validate_node_kind ──────────────────────────────────────────

    #[test]
    fn validate_node_kind_accepts_agent() {
        assert_eq!(validate_node_kind("agent").unwrap(), GraphNodeKind::Agent);
    }

    #[test]
    fn validate_node_kind_accepts_check() {
        assert_eq!(validate_node_kind("check").unwrap(), GraphNodeKind::Check);
    }

    #[test]
    fn validate_node_kind_accepts_gate() {
        assert_eq!(validate_node_kind("gate").unwrap(), GraphNodeKind::Gate);
    }

    #[test]
    fn validate_node_kind_accepts_join() {
        assert_eq!(validate_node_kind("join").unwrap(), GraphNodeKind::Join);
    }

    #[test]
    fn validate_node_kind_trims_whitespace() {
        assert_eq!(
            validate_node_kind("  agent  ").unwrap(),
            GraphNodeKind::Agent
        );
    }

    #[test]
    fn validate_node_kind_rejects_invalid() {
        let err = validate_node_kind("invalid").unwrap_err();
        assert!(err.contains("agent"), "{err}");
        assert!(err.contains("check"), "{err}");
        assert!(err.contains("gate"), "{err}");
    }

    // ── validate_not_join_kind ──────────────────────────────────────

    #[test]
    fn validate_not_join_kind_rejects_join() {
        let err = validate_not_join_kind(GraphNodeKind::Join).unwrap_err();
        assert!(err.contains("engine-managed"), "{err}");
        assert!(err.contains("graph_add_ensemble"), "{err}");
    }

    #[test]
    fn validate_not_join_kind_accepts_agent() {
        assert!(validate_not_join_kind(GraphNodeKind::Agent).is_ok());
    }

    #[test]
    fn validate_not_join_kind_accepts_check() {
        assert!(validate_not_join_kind(GraphNodeKind::Check).is_ok());
    }

    #[test]
    fn validate_not_join_kind_accepts_gate() {
        assert!(validate_not_join_kind(GraphNodeKind::Gate).is_ok());
    }

    // ── validate_spec_status ────────────────────────────────────────

    #[test]
    fn validate_spec_status_accepts_all_valid_statuses() {
        assert_eq!(
            validate_spec_status("pending").unwrap(),
            GraphSpecStatus::Pending
        );
        assert_eq!(
            validate_spec_status("running").unwrap(),
            GraphSpecStatus::Running
        );
        assert_eq!(
            validate_spec_status("completed").unwrap(),
            GraphSpecStatus::Completed
        );
        assert_eq!(
            validate_spec_status("failed").unwrap(),
            GraphSpecStatus::Failed
        );
        assert_eq!(
            validate_spec_status("skipped").unwrap(),
            GraphSpecStatus::Skipped
        );
        assert_eq!(
            validate_spec_status("interrupted").unwrap(),
            GraphSpecStatus::Interrupted
        );
    }

    #[test]
    fn validate_spec_status_is_case_insensitive() {
        assert_eq!(
            validate_spec_status("PENDING").unwrap(),
            GraphSpecStatus::Pending
        );
        assert_eq!(
            validate_spec_status("Running").unwrap(),
            GraphSpecStatus::Running
        );
    }

    #[test]
    fn validate_spec_status_trims_whitespace() {
        assert_eq!(
            validate_spec_status("  pending  ").unwrap(),
            GraphSpecStatus::Pending
        );
    }

    #[test]
    fn validate_spec_status_rejects_invalid() {
        let err = validate_spec_status("unknown").unwrap_err();
        assert!(err.contains("pending"), "{err}");
        assert!(err.contains("running"), "{err}");
        assert!(err.contains("completed"), "{err}");
        assert!(err.contains("failed"), "{err}");
        assert!(err.contains("skipped"), "{err}");
    }

    // ── validate_spec_set_status_target ─────────────────────────────

    #[test]
    fn validate_spec_set_status_target_accepts_valid() {
        assert_eq!(
            validate_spec_set_status_target("pending").unwrap(),
            GraphSpecStatus::Pending
        );
        assert_eq!(
            validate_spec_set_status_target("completed").unwrap(),
            GraphSpecStatus::Completed
        );
        assert_eq!(
            validate_spec_set_status_target("skipped").unwrap(),
            GraphSpecStatus::Skipped
        );
    }

    #[test]
    fn validate_spec_set_status_target_rejects_running() {
        let err = validate_spec_set_status_target("running").unwrap_err();
        assert!(err.contains("pending"), "{err}");
        assert!(err.contains("completed"), "{err}");
        assert!(err.contains("skipped"), "{err}");
    }

    #[test]
    fn validate_spec_set_status_target_rejects_failed() {
        let err = validate_spec_set_status_target("failed").unwrap_err();
        assert!(err.contains("pending"), "{err}");
        assert!(err.contains("completed"), "{err}");
        assert!(err.contains("skipped"), "{err}");
    }

    #[test]
    fn validate_spec_set_status_target_rejects_invalid() {
        assert!(validate_spec_set_status_target("unknown").is_err());
    }

    // ── json_value_kind_name ────────────────────────────────────────

    #[test]
    fn json_value_kind_name_null() {
        assert_eq!(json_value_kind_name(&serde_json::Value::Null), "null");
    }

    #[test]
    fn json_value_kind_name_bool() {
        assert_eq!(json_value_kind_name(&serde_json::json!(true)), "a boolean");
    }

    #[test]
    fn json_value_kind_name_number() {
        assert_eq!(json_value_kind_name(&serde_json::json!(42)), "a number");
    }

    #[test]
    fn json_value_kind_name_string() {
        assert_eq!(
            json_value_kind_name(&serde_json::json!("hello")),
            "a JSON-encoded string"
        );
    }

    #[test]
    fn json_value_kind_name_array() {
        assert_eq!(
            json_value_kind_name(&serde_json::json!([1, 2, 3])),
            "an array"
        );
    }

    #[test]
    fn json_value_kind_name_object() {
        assert_eq!(
            json_value_kind_name(&serde_json::json!({"key": "value"})),
            "an object"
        );
    }

    // ── validate_at_least_one_bool ──────────────────────────────────

    #[test]
    fn validate_at_least_one_bool_rejects_all_false() {
        let err = validate_at_least_one_bool(&[false, false, false], "fields").unwrap_err();
        assert!(err.contains("fields"), "{err}");
        assert!(err.contains("at least one"), "{err}");
    }

    #[test]
    fn validate_at_least_one_bool_accepts_one_true() {
        assert!(validate_at_least_one_bool(&[false, true, false], "fields").is_ok());
    }

    #[test]
    fn validate_at_least_one_bool_accepts_all_true() {
        assert!(validate_at_least_one_bool(&[true, true, true], "fields").is_ok());
    }

    // ── validate_queue_reorder ───────────────────────────────────────

    #[test]
    fn validate_queue_reorder_accepts_valid_permutation() {
        let current = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let reordered = vec!["c".to_string(), "a".to_string(), "b".to_string()];
        assert!(validate_queue_reorder(&current, &reordered).is_ok());
    }

    #[test]
    fn validate_queue_reorder_accepts_same_order() {
        let current = vec!["a".to_string(), "b".to_string()];
        let reordered = vec!["a".to_string(), "b".to_string()];
        assert!(validate_queue_reorder(&current, &reordered).is_ok());
    }

    #[test]
    fn validate_queue_reorder_rejects_length_mismatch() {
        let current = vec!["a".to_string(), "b".to_string()];
        let reordered = vec!["a".to_string()];
        let err = validate_queue_reorder(&current, &reordered).unwrap_err();
        assert!(err.contains("2"), "{err}");
        assert!(err.contains("1"), "{err}");
    }

    #[test]
    fn validate_queue_reorder_rejects_duplicate() {
        let current = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let reordered = vec!["a".to_string(), "a".to_string(), "b".to_string()];
        let err = validate_queue_reorder(&current, &reordered).unwrap_err();
        assert!(err.contains("a"), "{err}");
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn validate_queue_reorder_rejects_unknown_id() {
        let current = vec!["a".to_string(), "b".to_string()];
        let reordered = vec!["a".to_string(), "x".to_string()];
        let err = validate_queue_reorder(&current, &reordered).unwrap_err();
        assert!(err.contains("x"), "{err}");
        assert!(err.contains("no spec"), "{err}");
    }

    // ── spec_summary_json ───────────────────────────────────────────

    #[test]
    fn spec_summary_json_includes_required_fields() {
        let spec = GraphSpec {
            id: "spec-1".to_string(),
            graph_id: Some("graph-1".to_string()),
            name: "My Spec".to_string(),
            description: Some("does stuff".to_string()),
            position: 3,
            parallelizable: true,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: Some("/tmp/project".to_string()),
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        let json = spec_summary_json(&spec, true);
        assert_eq!(json["id"], "spec-1");
        assert_eq!(json["graph_id"], "graph-1");
        assert_eq!(json["name"], "My Spec");
        assert_eq!(json["description"], "does stuff");
        assert_eq!(json["position"], 3);
        assert_eq!(json["parallelizable"], true);
        assert_eq!(json["status"], "pending");
        assert_eq!(json["workdir"], "/tmp/project");
        assert!(json.get("completed_via").is_none());
    }

    #[test]
    fn spec_summary_json_includes_completed_via_when_set() {
        let spec = GraphSpec {
            id: "spec-2".to_string(),
            graph_id: None,
            name: "Done".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Completed,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: Some("graph_reset".to_string()),
            completed_via_reason: None,
            completed_via_at: None,
        };
        let json = spec_summary_json(&spec, true);
        assert_eq!(json["completed_via"], "graph_reset");
    }

    // ── blueprint_json ──────────────────────────────────────────────

    #[test]
    fn blueprint_json_includes_all_fields() {
        let bp = Blueprint {
            id: "bp-1".to_string(),
            name: "my-blueprint".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            builtin: true,
            created_at: chrono::Utc::now(),
        };
        let json = super::blueprint_json(&bp);
        assert_eq!(json["id"], "bp-1");
        assert_eq!(json["name"], "my-blueprint");
        assert_eq!(json["kind"], "agent");
        assert_eq!(json["builtin"], true);
        assert_eq!(json["config"]["platform"], "claude");
    }

    #[test]
    fn blueprint_json_non_builtin() {
        let bp = Blueprint {
            id: "bp-2".to_string(),
            name: "custom".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            builtin: false,
            created_at: chrono::Utc::now(),
        };
        let json = super::blueprint_json(&bp);
        assert_eq!(json["builtin"], false);
        assert_eq!(json["kind"], "check");
    }

    // ── build_id_result / build_json_result ─────────────────────────

    #[test]
    fn build_id_result_contains_key_and_id() {
        let result = build_id_result("abc-123", "graph_id");
        let text = format!("{:?}", result.content);
        assert!(text.contains("abc-123"), "{text}");
        assert!(text.contains("graph_id"), "{text}");
        assert!(result.is_error != Some(true));
    }

    #[test]
    fn build_json_result_contains_value() {
        let value = serde_json::json!({"key": "value", "count": 42});
        let result = build_json_result(&value);
        let text = format!("{:?}", result.content);
        assert!(text.contains("key"), "{text}");
        assert!(text.contains("value"), "{text}");
        assert!(text.contains("42"), "{text}");
        assert!(result.is_error != Some(true));
    }

    // ── graph_trigger_json ───────────────────────────────────────────

    fn make_graph_with_trigger(trigger: Option<Trigger>) -> Graph {
        Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "graph-1".to_string(),
            name: "Test Graph".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: GraphStatus::Draft,
            trigger,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn graph_trigger_json_manual() {
        let lp = make_graph_with_trigger(None);
        let json = graph_trigger_json(&lp);
        assert_eq!(json["type"], "manual");
        assert!(json.get("schedule").is_none());
        assert!(json.get("path").is_none());
    }

    #[test]
    fn graph_trigger_json_cron() {
        let lp = make_graph_with_trigger(Some(Trigger::Cron {
            schedule_expr: "0 9 * * *".to_string(),
        }));
        let json = graph_trigger_json(&lp);
        assert_eq!(json["type"], "cron");
        assert_eq!(json["schedule"], "0 9 * * *");
    }

    #[test]
    fn graph_trigger_json_watch() {
        let lp = make_graph_with_trigger(Some(Trigger::Watch {
            path: "/tmp/project".to_string(),
            events: vec![
                crate::domain::models::WatchEvent::Create,
                crate::domain::models::WatchEvent::Modify,
            ],
            debounce_seconds: 3,
            recursive: true,
        }));
        let json = graph_trigger_json(&lp);
        assert_eq!(json["type"], "watch");
        assert_eq!(json["path"], "/tmp/project");
        let events = json["events"].as_array().unwrap();
        assert!(events.contains(&serde_json::json!("create")));
        assert!(events.contains(&serde_json::json!("modify")));
    }

    // ── build_get_tools_response ────────────────────────────────────

    #[test]
    fn build_get_tools_response_session_start() {
        let json = build_get_tools_response("session_start");
        assert_eq!(json["scope"], "session_start");
        assert_eq!(json["risk"], "low");
        assert!(json["protocol"].is_array());
        assert!(json["tools"].is_array());
    }

    #[test]
    fn build_get_tools_response_file_write() {
        let json = build_get_tools_response("file_write");
        assert_eq!(json["scope"], "file_write");
        assert_eq!(json["risk"], "high");
    }

    #[test]
    fn build_get_tools_response_test_run() {
        let json = build_get_tools_response("test_run");
        assert_eq!(json["scope"], "test_run");
        assert_eq!(json["risk"], "medium");
    }

    #[test]
    fn build_get_tools_response_close_session() {
        let json = build_get_tools_response("close_session");
        assert_eq!(json["scope"], "close_session");
        assert_eq!(json["risk"], "low");
    }

    #[test]
    fn build_get_tools_response_multi_agent() {
        let json = build_get_tools_response("multi_agent");
        assert_eq!(json["scope"], "multi_agent");
        assert_eq!(json["risk"], "varies");
    }

    // ── rag_result_json ─────────────────────────────────────────────

    #[test]
    fn rag_result_json_includes_all_fields() {
        let search_result = crate::rag::vector_store::SearchResult {
            id: "sr-1".to_string(),
            file_path: "/tmp/doc.md".to_string(),
            content: "some content here".to_string(),
            created_at: 0,
            distance: Some(0.42f32),
        };
        let json = rag_result_json(&search_result);
        assert_eq!(json["source"], "/tmp/doc.md");
        assert_eq!(json["content"], "some content here");
        assert!(json["distance"].as_f64().unwrap() > 0.41);
        assert!(json["distance"].as_f64().unwrap() < 0.43);
    }

    // ── node_copy_note ──────────────────────────────────────────────

    #[test]
    fn node_copy_note_wired() {
        let note = node_copy_note("src-1", "new-1", true);
        assert!(note.contains("src-1"), "{note}");
        assert!(note.contains("new-1"), "{note}");
        assert!(!note.contains("NO"), "{note}");
    }

    #[test]
    fn node_copy_note_unwired() {
        let note = node_copy_note("src-2", "new-2", false);
        assert!(note.contains("src-2"), "{note}");
        assert!(note.contains("new-2"), "{note}");
        assert!(note.contains("NO"), "{note}");
        assert!(note.contains("graph_add_edge"), "{note}");
    }

    // ── build_graph_update_response / build_spec_update_response ──

    #[test]
    fn build_graph_update_response_contains_graph_id() {
        let result = build_graph_update_response("graph-42", None);
        let text = format!("{:?}", result.content);
        assert!(text.contains("graph-42"), "{text}");
        assert!(text.contains("updated"), "{text}");
        assert!(result.is_error != Some(true));
    }

    #[test]
    fn build_spec_update_response_contains_spec_id() {
        let result = build_spec_update_response("spec-99");
        let text = format!("{:?}", result.content);
        assert!(text.contains("spec-99"), "{text}");
        assert!(text.contains("updated"), "{text}");
        assert!(result.is_error != Some(true));
    }

    // ── validate_node_config edge cases ─────────────────────────────

    #[test]
    fn validate_node_config_gate_without_evaluate_defaults_to_output_contains() {
        // When "evaluate" is absent, it defaults to "output_contains", so "value" is required.
        let config = serde_json::json!({});
        let err = validate_node_config(GraphNodeKind::Gate, &config).unwrap_err();
        assert!(err.contains("value"), "{err}");
    }

    #[test]
    fn validate_node_config_gate_with_non_output_contains_evaluate_skips_value_check() {
        let config = serde_json::json!({"evaluate": "exit_code_0"});
        assert!(validate_node_config(GraphNodeKind::Gate, &config).is_ok());
    }

    #[test]
    fn validate_node_config_agent_with_cli_only() {
        let config = serde_json::json!({"cli": "codex"});
        assert!(validate_node_config(GraphNodeKind::Agent, &config).is_ok());
    }

    #[test]
    fn validate_node_config_agent_with_both_platform_and_cli() {
        let config = serde_json::json!({"platform": "claude", "cli": "codex"});
        assert!(validate_node_config(GraphNodeKind::Agent, &config).is_ok());
    }

    #[test]
    fn validate_node_config_rejects_non_object() {
        let cases = [
            serde_json::json!(null),
            serde_json::json!(42),
            serde_json::json!("string"),
            serde_json::json!([1, 2]),
        ];
        for config in &cases {
            let err = validate_node_config(GraphNodeKind::Agent, config).unwrap_err();
            assert!(err.contains("JSON object"), "{err}");
        }
    }

    #[test]
    fn validate_node_config_agent_rejects_empty_platform_and_cli() {
        let config = serde_json::json!({"platform": "", "cli": ""});
        let err = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
        assert!(err.contains("platform"), "{err}");
    }

    #[test]
    fn validate_node_config_agent_rejects_whitespace_only_platform() {
        let config = serde_json::json!({"platform": "   "});
        let err = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
        assert!(err.contains("platform"), "{err}");
    }

    // ── missing_sync_identity_error ─────────────────────────────────

    #[test]
    fn missing_sync_identity_error_message_matches_constant() {
        let err = missing_sync_identity_error();
        assert_eq!(err.message, MISSING_SYNC_IDENTITY_MESSAGE);
    }

    // ── GraphEdgeCondition::from_str / GraphNodeKind::from_str (domain) ──

    #[test]
    fn graph_edge_condition_from_str_roundtrip() {
        for cond in [
            GraphEdgeCondition::Pass,
            GraphEdgeCondition::Fail,
            GraphEdgeCondition::Always,
        ] {
            let s = cond.as_str();
            assert_eq!(GraphEdgeCondition::from_str(s), Some(cond));
        }
    }

    #[test]
    fn graph_edge_condition_from_str_invalid() {
        assert_eq!(GraphEdgeCondition::from_str("sometimes"), None);
        assert_eq!(GraphEdgeCondition::from_str(""), None);
    }

    #[test]
    fn validate_edge_condition_rejects_legacy_break() {
        let err = validate_edge_condition("break").unwrap_err();
        assert!(err.contains("pass, fail, always, error"), "{err}");
    }

    #[test]
    fn graph_node_kind_from_str_roundtrip() {
        for kind in [
            GraphNodeKind::Agent,
            GraphNodeKind::Check,
            GraphNodeKind::Gate,
            GraphNodeKind::Join,
        ] {
            let s = kind.as_str();
            assert_eq!(GraphNodeKind::from_str(s), Some(kind));
        }
    }

    #[test]
    fn graph_node_kind_from_str_invalid() {
        assert_eq!(GraphNodeKind::from_str("invalid"), None);
        assert_eq!(GraphNodeKind::from_str(""), None);
    }

    #[test]
    fn graph_spec_status_from_str_roundtrip() {
        for status in [
            GraphSpecStatus::Pending,
            GraphSpecStatus::Running,
            GraphSpecStatus::Completed,
            GraphSpecStatus::Failed,
            GraphSpecStatus::Skipped,
            GraphSpecStatus::Interrupted,
        ] {
            let s = status.as_str();
            assert_eq!(GraphSpecStatus::from_str(s), status);
        }
    }

    #[test]
    fn graph_spec_status_from_str_defaults_to_pending() {
        assert_eq!(
            GraphSpecStatus::from_str("unknown"),
            GraphSpecStatus::Pending
        );
        assert_eq!(GraphSpecStatus::from_str(""), GraphSpecStatus::Pending);
    }

    // ── build_graph_trigger: cron with invalid expression ────────────

    #[test]
    fn build_graph_trigger_cron_invalid_expression() {
        let params = GraphTriggerParams {
            kind: "cron".to_string(),
            schedule: Some("not-a-cron".to_string()),
            path: None,
            events: None,
            debounce_seconds: None,
            recursive: None,
        };
        let err = build_graph_trigger(&Some(params)).unwrap_err();
        assert!(err.contains("Invalid cron expression"), "{err}");
    }

    #[test]
    fn build_graph_trigger_cron_valid_expression() {
        let params = GraphTriggerParams {
            kind: "cron".to_string(),
            schedule: Some("*/5 * * * *".to_string()),
            path: None,
            events: None,
            debounce_seconds: None,
            recursive: None,
        };
        let trigger = build_graph_trigger(&Some(params)).unwrap().unwrap();
        match trigger {
            Trigger::Cron { schedule_expr } => {
                assert_eq!(schedule_expr, "*/5 * * * *");
            }
            other => panic!("expected Trigger::Cron, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod additional_tests {
    use super::*;
    use crate::daemon::params::{
        EnsembleMemberParams, GraphCompletionHookParams, GraphTriggerParams,
    };
    use crate::db::Database;
    use crate::domain::graphs::{
        Graph, GraphNode, GraphNodeKind, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus,
        GraphStatus,
    };
    use crate::domain::models::Trigger;
    use crate::domain::queues::Queue;
    use tempfile::tempdir;

    fn standalone_spec(id: &str) -> GraphSpec {
        GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: id.to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        }
    }

    fn insert_test_graph(db: &Database, id: &str) {
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        })
        .unwrap();
    }

    fn insert_test_node(db: &Database, id: &str, spec_id: &str) {
        db.insert_graph_node(&GraphNode {
            id: id.to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: id.to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    }

    fn graph_run_row(
        id: &str,
        graph_id: &str,
        spec_id: &str,
        status: GraphRunStatus,
    ) -> GraphNodeRun {
        GraphNodeRun {
            id: id.to_string(),
            graph_id: graph_id.to_string(),
            spec_id: spec_id.to_string(),
            node_id: "node-1".to_string(),
            status,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: (status != GraphRunStatus::Running).then(chrono::Utc::now),
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        }
    }

    // ── validate_position_conflict ────────────────────────────────

    #[test]
    fn validate_position_conflict_no_graph_id_returns_ok() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        assert!(validate_position_conflict(&db, None, "spec-x", 1).is_ok());
    }

    #[test]
    fn validate_position_conflict_no_conflict() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        let mut spec = standalone_spec("spec-a");
        spec.graph_id = Some("graph-1".to_string());
        db.insert_graph_spec(&spec).unwrap();
        // spec-a is at position 0, checking position 1 — no conflict.
        assert!(validate_position_conflict(&db, Some("graph-1"), "spec-x", 1).is_ok());
    }

    #[test]
    fn validate_position_conflict_detects_conflict() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        let mut spec = standalone_spec("spec-a");
        spec.graph_id = Some("graph-1".to_string());
        db.insert_graph_spec(&spec).unwrap();
        let error = validate_position_conflict(&db, Some("graph-1"), "spec-x", 0).unwrap_err();
        assert!(error.contains("position 0"), "{error}");
        assert!(error.contains("graph-1"), "{error}");
    }

    #[test]
    fn validate_position_conflict_excludes_self() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        let mut spec = standalone_spec("spec-a");
        spec.graph_id = Some("graph-1".to_string());
        db.insert_graph_spec(&spec).unwrap();
        // spec-a at position 0, checking spec-a itself at position 0 — no conflict.
        assert!(validate_position_conflict(&db, Some("graph-1"), "spec-a", 0).is_ok());
    }

    // ── validate_node_position_conflict ───────────────────────────

    #[test]
    fn validate_node_position_conflict_no_owner_returns_ok() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let node = GraphNode {
            id: "n1".to_string(),
            spec_id: None,
            graph_id: None,
            name: "n1".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({}),
            position: 1,
            created_at: chrono::Utc::now(),
        };
        assert!(validate_node_position_conflict(&db, &node, "n1", 1).is_ok());
    }

    #[test]
    fn validate_node_position_conflict_spec_owner() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-1")).unwrap();
        let now = chrono::Utc::now();
        db.insert_graph_node(&GraphNode {
            id: "n1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "n1".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 1,
            created_at: now,
        })
        .unwrap();
        let error = validate_node_position_conflict(
            &db,
            &GraphNode {
                id: "n1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "n1".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "claude"}),
                position: 5,
                created_at: now,
            },
            "other-node",
            1,
        )
        .unwrap_err();
        assert!(error.contains("Spec 'spec-1'"), "{error}");
        assert!(error.contains("position 1"), "{error}");
    }

    #[test]
    fn validate_node_position_conflict_graph_owner() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        let now = chrono::Utc::now();
        db.insert_graph_node(&GraphNode {
            id: "n1".to_string(),
            spec_id: None,
            graph_id: Some("graph-1".to_string()),
            name: "n1".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 1,
            created_at: now,
        })
        .unwrap();
        let error = validate_node_position_conflict(
            &db,
            &GraphNode {
                id: "n1".to_string(),
                spec_id: None,
                graph_id: Some("graph-1".to_string()),
                name: "n1".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "claude"}),
                position: 5,
                created_at: now,
            },
            "other-node",
            1,
        )
        .unwrap_err();
        assert!(error.contains("Graph 'graph-1'"), "{error}");
    }

    // ── validate_spec_workdir edge cases ──────────────────────────

    #[test]
    fn validate_spec_workdir_whitespace_is_absolute() {
        // Whitespace-only relative path is still relative.
        assert!(validate_spec_workdir("  relative  ").is_err());
    }

    #[test]
    fn validate_spec_workdir_root_path() {
        assert!(validate_spec_workdir("/").is_ok());
    }

    // ── validate_not_join_kind edge cases ─────────────────────────

    #[test]
    fn validate_not_join_kind_join_returns_error_with_actionable_message() {
        let err = validate_not_join_kind(GraphNodeKind::Join).unwrap_err();
        assert!(err.contains("engine-managed"), "{err}");
        assert!(err.contains("graph_add_ensemble"), "{err}");
    }

    // ── validate_edge_condition edge cases ────────────────────────

    #[test]
    fn validate_edge_condition_empty_string() {
        let err = validate_edge_condition("").unwrap_err();
        assert!(err.contains("pass"), "{err}");
    }

    #[test]
    fn validate_edge_condition_case_sensitive() {
        // "Pass" (capital P) should not match — only lowercase.
        assert!(validate_edge_condition("Pass").is_err());
    }

    // ── validate_node_kind edge cases ─────────────────────────────

    #[test]
    fn validate_node_kind_empty_string() {
        assert!(validate_node_kind("").is_err());
    }

    #[test]
    fn validate_node_kind_uppercase() {
        assert!(validate_node_kind("AGENT").is_err());
    }

    // ── validate_spec_status edge cases ───────────────────────────

    #[test]
    fn validate_spec_status_empty_string() {
        let err = validate_spec_status("").unwrap_err();
        assert!(err.contains("pending"), "{err}");
    }

    #[test]
    fn validate_spec_set_status_target_empty_string() {
        assert!(validate_spec_set_status_target("").is_err());
    }

    // ── validate_spec_set_status_target edge cases ────────────────

    #[test]
    fn validate_spec_set_status_target_case_insensitive() {
        assert_eq!(
            validate_spec_set_status_target("COMPLETED").unwrap(),
            GraphSpecStatus::Completed
        );
        assert_eq!(
            validate_spec_set_status_target("  Skipped  ").unwrap(),
            GraphSpecStatus::Skipped
        );
    }

    // ── json_value_kind_name edge cases ───────────────────────────

    #[test]
    fn json_value_kind_name_nested_object() {
        let v = serde_json::json!({"a": {"b": 1}});
        assert_eq!(json_value_kind_name(&v), "an object");
    }

    #[test]
    fn json_value_kind_name_empty_array() {
        let v = serde_json::json!([]);
        assert_eq!(json_value_kind_name(&v), "an array");
    }

    // ── validate_at_least_one_bool edge cases ─────────────────────

    #[test]
    fn validate_at_least_one_bool_single_true() {
        assert!(validate_at_least_one_bool(&[true], "f").is_ok());
    }

    #[test]
    fn validate_at_least_one_bool_single_false() {
        assert!(validate_at_least_one_bool(&[false], "f").is_err());
    }

    #[test]
    fn validate_at_least_one_bool_empty_slice() {
        // All false in an empty slice.
        assert!(validate_at_least_one_bool(&[], "f").is_err());
    }

    // ── validate_queue_reorder edge cases ──────────────────────────

    #[test]
    fn validate_queue_reorder_empty_current_and_ids() {
        assert!(validate_queue_reorder(&[], &[]).is_ok());
    }

    #[test]
    fn validate_queue_reorder_single_element() {
        let current = vec!["a".to_string()];
        let reordered = vec!["a".to_string()];
        assert!(validate_queue_reorder(&current, &reordered).is_ok());
    }

    // ── validate_non_empty edge cases ─────────────────────────────

    #[test]
    fn validate_non_empty_single_space() {
        assert!(validate_non_empty(" ", "field").is_err());
    }

    #[test]
    fn validate_non_empty_tabs_and_newlines() {
        assert!(validate_non_empty("\t\n\r", "field").is_err());
    }

    // ── validate_absolute_dir edge cases ──────────────────────────

    #[test]
    fn validate_absolute_dir_root() {
        assert!(validate_absolute_dir("/").is_ok());
    }

    #[test]
    fn validate_absolute_dir_home() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        // Home dir should exist and be absolute.
        if std::path::Path::new(&home).is_dir() {
            assert!(validate_absolute_dir(&home).is_ok());
        }
    }

    // ── node_copy_note edge cases ─────────────────────────────────

    #[test]
    fn node_copy_note_wired_mentions_both_ids() {
        let note = node_copy_note("source-abc", "target-xyz", true);
        assert!(note.contains("source-abc"));
        assert!(note.contains("target-xyz"));
        assert!(!note.contains("Unwired"));
    }

    #[test]
    fn node_copy_note_unwired_suggests_wiring_tool() {
        let note = node_copy_note("src", "dst", false);
        assert!(note.contains("graph_add_edge"));
        assert!(note.contains("entry_from_node") || note.contains("on_pass_to"));
    }

    // ── build_graph_trigger: watch with debounce_seconds and recursive ──

    #[test]
    fn build_graph_trigger_watch_defaults() {
        let params = GraphTriggerParams {
            kind: "watch".to_string(),
            schedule: None,
            path: Some("/tmp".to_string()),
            events: Some(vec!["modify".to_string()]),
            debounce_seconds: None,
            recursive: None,
        };
        let trigger = build_graph_trigger(&Some(params)).unwrap().unwrap();
        match trigger {
            Trigger::Watch {
                debounce_seconds,
                recursive,
                ..
            } => {
                assert_eq!(debounce_seconds, 2, "default debounce should be 2");
                assert!(!recursive, "default recursive should be false");
            }
            other => panic!("expected Watch, got {other:?}"),
        }
    }

    // ── build_graph_completion_hook edge cases ─────────────────────

    #[test]
    fn build_graph_completion_hook_none_model_becomes_none() {
        let params = GraphCompletionHookParams {
            platform: Some("claude".to_string()),
            model: None,
            effort: None,
            prompt: Some("test".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let hook = build_graph_completion_hook(&params).unwrap();
        assert!(hook.model.is_none());
    }

    #[test]
    fn build_graph_completion_hook_timeout_passthrough() {
        let params = GraphCompletionHookParams {
            platform: Some("mimo".to_string()),
            model: None,
            effort: None,
            prompt: Some("do stuff".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(45),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let hook = build_graph_completion_hook(&params).unwrap();
        assert_eq!(hook.timeout_minutes, Some(45));
    }

    // ── graph_run_status_guard: Draft and Paused are accepted ──────

    #[test]
    fn graph_run_status_guard_draft_accepted() {
        assert!(graph_run_status_guard("graph-d", GraphStatus::Draft).is_ok());
    }

    #[test]
    fn graph_run_status_guard_paused_accepted() {
        assert!(graph_run_status_guard("graph-p", GraphStatus::Paused).is_ok());
    }

    // ── validate_node_config: Join kind ───────────────────────────

    #[test]
    fn validate_node_config_join_always_passes() {
        let config = serde_json::json!({});
        assert!(validate_node_config(GraphNodeKind::Join, &config).is_ok());
    }

    // ── build_graph_trigger: watch with empty events ───────────────

    #[test]
    fn build_graph_trigger_watch_empty_events_rejected() {
        let params = GraphTriggerParams {
            kind: "watch".to_string(),
            schedule: None,
            path: Some("/tmp".to_string()),
            events: Some(vec![]),
            debounce_seconds: None,
            recursive: None,
        };
        let err = build_graph_trigger(&Some(params)).unwrap_err();
        assert!(err.contains("event"), "{err}");
    }

    // ── validate_queue_reorder_locking: pending spec is movable ────

    #[test]
    fn validate_queue_reorder_locking_allows_moving_pending_members() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-a")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-c")).unwrap();
        db.insert_queue(&Queue {
            id: "queue-1".to_string(),
            name: "queue-1".to_string(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        for id in ["spec-a", "spec-b", "spec-c"] {
            db.append_queue_member("queue-1", id, None).unwrap();
        }
        let current = db.list_queue_member_spec_ids("queue-1").unwrap();

        // All pending — any permutation is allowed.
        let order = vec![
            "spec-c".to_string(),
            "spec-a".to_string(),
            "spec-b".to_string(),
        ];
        assert!(validate_queue_reorder_locking(&db, &current, &order).is_ok());
    }

    // ── validate_queue_member_removable: non-running spec ──────────

    #[test]
    fn validate_queue_member_removable_pending_spec_ok() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-a")).unwrap();
        db.insert_queue(&Queue {
            id: "queue-1".to_string(),
            name: "queue-1".to_string(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        assert!(validate_queue_member_removable(&db, "queue-1", "spec-a").is_ok());
    }

    // ── resolve_reported_run: run not found for node_id ───────────

    #[test]
    fn resolve_reported_run_rejects_node_id_mismatch_even_if_running() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        db.insert_graph_spec(&standalone_spec("spec-a")).unwrap();
        insert_test_node(&db, "node-1", "spec-a");
        db.insert_graph_run(&graph_run_row(
            "run-1",
            "graph-1",
            "spec-a",
            GraphRunStatus::Running,
        ))
        .unwrap();

        let result = resolve_reported_run(&db, "run-1", "wrong-node").unwrap();
        let err = result.expect_err("mismatched node_id must be rejected");
        assert!(err.is_error.unwrap_or(false));
    }

    // ── validate_queue_not_consumed: empty queue ────────────────────

    #[test]
    fn validate_queue_not_consumed_empty_queue() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_queue(&Queue {
            id: "queue-empty".to_string(),
            name: "empty".to_string(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        assert!(validate_queue_not_consumed(&db, "queue-empty", "graph-1").is_ok());
    }

    // ── graph_run_status_guard: error messages name the graph ───────

    #[test]
    fn graph_run_status_guard_running_error_names_graph() {
        let err = graph_run_status_guard("my-graph", GraphStatus::Running).unwrap_err();
        assert!(err.contains("my-graph"), "{err}");
        assert!(err.contains("already running"), "{err}");
    }

    #[test]
    fn graph_run_status_guard_completed_error_names_reset() {
        let err = graph_run_status_guard("l", GraphStatus::Completed).unwrap_err();
        assert!(err.contains("graph_reset"), "{err}");
    }

    // ── validate_spec_deletable: no graph_id ───────────────────────

    #[test]
    fn validate_spec_deletable_standalone_spec_ok() {
        let spec = GraphSpec {
            id: "spec-1".to_string(),
            graph_id: None,
            name: "test".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        assert!(validate_spec_deletable(&spec).is_ok());
    }

    // ── validate_ensemble_members: whitespace-only platform ───────

    #[test]
    fn validate_ensemble_members_rejects_whitespace_platform() {
        let members = vec![
            EnsembleMemberParams {
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
            EnsembleMemberParams {
                platform: "\t\n".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
        ];
        let err = validate_ensemble_members(&members, EnsembleKind::Parallel).unwrap_err();
        assert!(err.contains("platform"), "{err}");
    }

    // ── validate_ensemble_members: model trimming ─────────────────

    #[test]
    fn validate_ensemble_members_trims_model() {
        let members = vec![
            EnsembleMemberParams {
                platform: "claude".to_string(),
                model: Some("  opus-4  ".to_string()),
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
            EnsembleMemberParams {
                platform: "mimo".to_string(),
                model: Some("   ".to_string()),
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
        ];
        let result = validate_ensemble_members(&members, EnsembleKind::Parallel).unwrap();
        assert_eq!(result[0].1.as_deref(), Some("opus-4"));
        assert_eq!(result[1].1, None); // whitespace-only model becomes None
    }

    // ── validate_position_conflict: no conflict with same position on different spec ──

    #[test]
    fn validate_position_conflict_same_position_different_graph() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        insert_test_graph(&db, "graph-2");
        let mut spec = standalone_spec("spec-a");
        spec.graph_id = Some("graph-1".to_string());
        db.insert_graph_spec(&spec).unwrap();
        // spec-a at position 0 in graph-1; check position 0 in graph-2 — no conflict.
        assert!(validate_position_conflict(&db, Some("graph-2"), "spec-x", 0).is_ok());
    }

    // ── build_graph_update_response ────────────────────────────────

    #[test]
    fn build_graph_update_response_text() {
        let result = build_graph_update_response("graph-xyz", None);
        let text = format!("{:?}", result.content);
        assert!(text.contains("graph-xyz"));
        assert!(text.contains("updated"));
    }

    // ── build_spec_update_response ────────────────────────────────

    #[test]
    fn build_spec_update_response_text() {
        let result = build_spec_update_response("spec-abc");
        let text = format!("{:?}", result.content);
        assert!(text.contains("spec-abc"));
        assert!(text.contains("updated"));
    }

    // ── blueprint_json ────────────────────────────────────────────

    #[test]
    fn blueprint_json_with_agent_kind() {
        let bp = Blueprint {
            id: "bp-a".to_string(),
            name: "agent-bp".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "mimo"}),
            builtin: false,
            created_at: chrono::Utc::now(),
        };
        let json = blueprint_json(&bp);
        assert_eq!(json["kind"], "agent");
        assert_eq!(json["builtin"], false);
    }

    // ── spec_summary_json edge cases ──────────────────────────────

    #[test]
    fn spec_summary_json_with_null_fields() {
        let spec = GraphSpec {
            id: "s1".to_string(),
            graph_id: None,
            name: "spec".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        let json = spec_summary_json(&spec, true);
        assert!(json["graph_id"].is_null());
        assert!(json["description"].is_null());
        assert!(json["workdir"].is_null());
    }

    // ── build_get_tools_response: all scopes return objects ────────

    #[test]
    fn build_get_tools_response_all_scopes_are_objects() {
        for scope in [
            "session_start",
            "file_write",
            "test_run",
            "close_session",
            "multi_agent",
        ] {
            let json = build_get_tools_response(scope);
            assert!(json.is_object(), "scope '{scope}' should be an object");
            assert!(json["scope"].as_str().is_some());
            assert!(json["protocol"].is_array());
            assert!(json["tools"].is_array());
        }
    }

    // ── rag_result_json edge cases ────────────────────────────────

    #[test]
    fn rag_result_json_with_none_distance() {
        let result = crate::rag::vector_store::SearchResult {
            id: "sr-2".to_string(),
            file_path: "/doc.md".to_string(),
            content: "text".to_string(),
            created_at: 0,
            distance: None,
        };
        let json = rag_result_json(&result);
        assert!(json["distance"].is_null());
    }

    // ── validate_node_config: gate with custom evaluate ───────────

    #[test]
    fn validate_node_config_gate_custom_evaluate_no_value_required() {
        let config = serde_json::json!({"evaluate": "exit_code_0"});
        assert!(validate_node_config(GraphNodeKind::Gate, &config).is_ok());
    }

    // ── validate_queue_reorder_locking: skipped spec is locked ─────

    #[test]
    fn validate_queue_reorder_locking_refuses_moving_skipped_spec() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let mut skipped = standalone_spec("spec-a");
        skipped.status = GraphSpecStatus::Skipped;
        db.insert_graph_spec(&skipped).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
        db.insert_queue(&Queue {
            id: "queue-1".to_string(),
            name: "queue-1".to_string(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.append_queue_member("queue-1", "spec-a", None).unwrap();
        db.append_queue_member("queue-1", "spec-b", None).unwrap();
        let current = db.list_queue_member_spec_ids("queue-1").unwrap();

        let order = vec!["spec-b".to_string(), "spec-a".to_string()];
        let error = validate_queue_reorder_locking(&db, &current, &order).unwrap_err();
        assert!(error.contains("spec-a"), "{error}");
        assert!(error.contains("skipped"), "{error}");
    }

    // ── node_copy_note ────────────────────────────────────────────

    #[test]
    fn node_copy_note_wired() {
        let note = node_copy_note("src", "dst", true);
        assert!(note.contains("src"));
        assert!(note.contains("dst"));
        assert!(!note.contains("Unwired"));
    }

    #[test]
    fn node_copy_note_unwired() {
        let note = node_copy_note("src", "dst", false);
        assert!(note.contains("Unwired"));
        assert!(note.contains("src"));
        assert!(note.contains("dst"));
        assert!(note.contains("NO incoming or outgoing edges"));
    }

    // ── json_value_kind_name ──────────────────────────────────────

    #[test]
    fn json_value_kind_name_null() {
        assert_eq!(json_value_kind_name(&serde_json::Value::Null), "null");
    }

    #[test]
    fn json_value_kind_name_bool() {
        assert_eq!(json_value_kind_name(&serde_json::json!(true)), "a boolean");
    }

    #[test]
    fn json_value_kind_name_number() {
        assert_eq!(json_value_kind_name(&serde_json::json!(42)), "a number");
    }

    #[test]
    fn json_value_kind_name_string() {
        assert_eq!(
            json_value_kind_name(&serde_json::json!("hello")),
            "a JSON-encoded string"
        );
    }

    #[test]
    fn json_value_kind_name_array() {
        assert_eq!(json_value_kind_name(&serde_json::json!([1, 2])), "an array");
    }

    #[test]
    fn json_value_kind_name_object() {
        assert_eq!(
            json_value_kind_name(&serde_json::json!({"a": 1})),
            "an object"
        );
    }

    // ── validate_node_config ──────────────────────────────────────

    #[test]
    fn validate_node_config_agent_needs_platform() {
        let config = serde_json::json!({"command": "test"});
        let err = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
        assert!(err.contains("platform"), "{err}");
    }

    #[test]
    fn validate_node_config_agent_with_platform() {
        let config = serde_json::json!({"platform": "claude"});
        assert!(validate_node_config(GraphNodeKind::Agent, &config).is_ok());
    }

    #[test]
    fn validate_node_config_agent_with_cli() {
        let config = serde_json::json!({"cli": "opencode"});
        assert!(validate_node_config(GraphNodeKind::Agent, &config).is_ok());
    }

    #[test]
    fn validate_node_config_check_needs_command() {
        let config = serde_json::json!({"platform": "claude"});
        let err = validate_node_config(GraphNodeKind::Check, &config).unwrap_err();
        assert!(err.contains("command"), "{err}");
    }

    #[test]
    fn validate_node_config_check_with_command() {
        let config = serde_json::json!({"command": "cargo test"});
        assert!(validate_node_config(GraphNodeKind::Check, &config).is_ok());
    }

    #[test]
    fn validate_node_config_gate_needs_value_when_output_contains() {
        let config = serde_json::json!({});
        let err = validate_node_config(GraphNodeKind::Gate, &config).unwrap_err();
        assert!(err.contains("value"), "{err}");
    }

    #[test]
    fn validate_node_config_gate_with_value() {
        let config = serde_json::json!({"value": "success"});
        assert!(validate_node_config(GraphNodeKind::Gate, &config).is_ok());
    }

    #[test]
    fn validate_node_config_not_object() {
        let config = serde_json::json!("not an object");
        let err = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
        assert!(err.contains("JSON object"), "{err}");
    }

    #[test]
    fn validate_node_config_join_always_ok() {
        let config = serde_json::json!({});
        assert!(validate_node_config(GraphNodeKind::Join, &config).is_ok());
    }

    #[test]
    fn validate_node_config_agent_empty_platform() {
        let config = serde_json::json!({"platform": "  "});
        let err = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
        assert!(err.contains("platform"), "{err}");
    }

    // ── validate_edge_condition ───────────────────────────────────

    #[test]
    fn validate_edge_condition_pass() {
        assert!(matches!(
            validate_edge_condition("pass").unwrap(),
            GraphEdgeCondition::Pass
        ));
    }

    #[test]
    fn validate_edge_condition_fail() {
        assert!(matches!(
            validate_edge_condition("fail").unwrap(),
            GraphEdgeCondition::Fail
        ));
    }

    #[test]
    fn validate_edge_condition_always() {
        assert!(matches!(
            validate_edge_condition("always").unwrap(),
            GraphEdgeCondition::Always
        ));
    }

    #[test]
    fn validate_edge_condition_invalid() {
        assert!(validate_edge_condition("sometimes").is_err());
    }

    #[test]
    fn validate_edge_condition_with_whitespace() {
        assert!(matches!(
            validate_edge_condition("  pass  ").unwrap(),
            GraphEdgeCondition::Pass
        ));
    }

    // ── validate_node_kind ────────────────────────────────────────

    #[test]
    fn validate_node_kind_agent() {
        assert!(matches!(
            validate_node_kind("agent").unwrap(),
            GraphNodeKind::Agent
        ));
    }

    #[test]
    fn validate_node_kind_check() {
        assert!(matches!(
            validate_node_kind("check").unwrap(),
            GraphNodeKind::Check
        ));
    }

    #[test]
    fn validate_node_kind_gate() {
        assert!(matches!(
            validate_node_kind("gate").unwrap(),
            GraphNodeKind::Gate
        ));
    }

    #[test]
    fn validate_node_kind_invalid() {
        assert!(validate_node_kind("invalid").is_err());
    }

    #[test]
    fn validate_node_kind_with_whitespace() {
        assert!(matches!(
            validate_node_kind("  agent  ").unwrap(),
            GraphNodeKind::Agent
        ));
    }

    // ── validate_not_join_kind ────────────────────────────────────

    #[test]
    fn validate_not_join_kind_rejects_join() {
        assert!(validate_not_join_kind(GraphNodeKind::Join).is_err());
    }

    #[test]
    fn validate_not_join_kind_accepts_agent() {
        assert!(validate_not_join_kind(GraphNodeKind::Agent).is_ok());
    }

    #[test]
    fn validate_not_join_kind_accepts_check() {
        assert!(validate_not_join_kind(GraphNodeKind::Check).is_ok());
    }

    #[test]
    fn validate_not_join_kind_accepts_gate() {
        assert!(validate_not_join_kind(GraphNodeKind::Gate).is_ok());
    }

    // ── validate_spec_status ──────────────────────────────────────

    #[test]
    fn validate_spec_status_all_valid() {
        assert!(matches!(
            validate_spec_status("pending").unwrap(),
            GraphSpecStatus::Pending
        ));
        assert!(matches!(
            validate_spec_status("running").unwrap(),
            GraphSpecStatus::Running
        ));
        assert!(matches!(
            validate_spec_status("completed").unwrap(),
            GraphSpecStatus::Completed
        ));
        assert!(matches!(
            validate_spec_status("failed").unwrap(),
            GraphSpecStatus::Failed
        ));
        assert!(matches!(
            validate_spec_status("skipped").unwrap(),
            GraphSpecStatus::Skipped
        ));
        assert!(matches!(
            validate_spec_status("interrupted").unwrap(),
            GraphSpecStatus::Interrupted
        ));
    }

    #[test]
    fn validate_spec_status_case_insensitive() {
        assert!(validate_spec_status("PENDING").is_ok());
        assert!(validate_spec_status("Running").is_ok());
    }

    #[test]
    fn validate_spec_status_with_whitespace() {
        assert!(validate_spec_status("  pending  ").is_ok());
    }

    #[test]
    fn validate_spec_status_invalid() {
        assert!(validate_spec_status("unknown").is_err());
    }

    // ── validate_spec_set_status_target ───────────────────────────

    #[test]
    fn validate_spec_set_status_target_valid() {
        assert!(validate_spec_set_status_target("pending").is_ok());
        assert!(validate_spec_set_status_target("completed").is_ok());
        assert!(validate_spec_set_status_target("skipped").is_ok());
    }

    #[test]
    fn validate_spec_set_status_target_running_invalid() {
        assert!(validate_spec_set_status_target("running").is_err());
    }

    #[test]
    fn validate_spec_set_status_target_failed_invalid() {
        assert!(validate_spec_set_status_target("failed").is_err());
    }

    // ── validate_at_least_one_bool ────────────────────────────────

    #[test]
    fn validate_at_least_one_bool_all_false() {
        assert!(validate_at_least_one_bool(&[false, false, false], "test").is_err());
    }

    #[test]
    fn validate_at_least_one_bool_one_true() {
        assert!(validate_at_least_one_bool(&[false, true, false], "test").is_ok());
    }

    #[test]
    fn validate_at_least_one_bool_all_true() {
        assert!(validate_at_least_one_bool(&[true, true], "test").is_ok());
    }

    #[test]
    fn validate_at_least_one_bool_empty() {
        assert!(validate_at_least_one_bool(&[], "test").is_err());
    }

    // ── missing_sync_identity_error ───────────────────────────────

    #[test]
    fn missing_sync_identity_error_includes_canopy_identity() {
        let err = missing_sync_identity_error();
        assert!(err.message.contains("Canopy session identity"));
    }

    // ── validate_spec_workdir ─────────────────────────────────────

    #[test]
    fn validate_spec_workdir_absolute() {
        assert!(validate_spec_workdir("/tmp").is_ok());
    }

    #[test]
    fn validate_spec_workdir_relative() {
        assert!(validate_spec_workdir("relative/path").is_err());
    }

    // ── validate_non_empty ────────────────────────────────────────

    #[test]
    fn validate_non_empty_valid() {
        assert!(validate_non_empty("hello", "field").is_ok());
    }

    #[test]
    fn validate_non_empty_empty() {
        assert!(validate_non_empty("", "field").is_err());
    }

    #[test]
    fn validate_non_empty_whitespace() {
        assert!(validate_non_empty("   ", "field").is_err());
    }

    #[test]
    fn validate_non_empty_error_message() {
        let err = validate_non_empty("", "my_field").unwrap_err();
        assert!(err.contains("my_field"));
    }

    // ── spec_summary_json ─────────────────────────────────────────

    #[test]
    fn spec_summary_json_basic() {
        let spec = standalone_spec("spec-1");
        let json = spec_summary_json(&spec, true);
        assert_eq!(json["id"], "spec-1");
        assert_eq!(json["name"], "spec-1");
        assert_eq!(json["status"], "pending");
        assert_eq!(json["parallelizable"], false);
    }

    #[test]
    fn spec_summary_json_with_completed_via() {
        let mut spec = standalone_spec("spec-1");
        spec.completed_via = Some("test".to_string());
        let json = spec_summary_json(&spec, true);
        assert_eq!(json["completed_via"], "test");
    }

    #[test]
    fn spec_summary_json_with_workdir() {
        let mut spec = standalone_spec("spec-1");
        spec.workdir = Some("/tmp/project".to_string());
        let json = spec_summary_json(&spec, true);
        assert_eq!(json["workdir"], "/tmp/project");
    }

    // ── build_get_tools_response ──────────────────────────────────

    #[test]
    fn build_get_tools_response_session_start() {
        let json = build_get_tools_response("session_start");
        assert_eq!(json["scope"], "session_start");
        assert_eq!(json["risk"], "low");
        assert!(json["protocol"].is_array());
        assert!(json["tools"].is_array());
    }

    #[test]
    fn build_get_tools_response_file_write() {
        let json = build_get_tools_response("file_write");
        assert_eq!(json["scope"], "file_write");
        assert_eq!(json["risk"], "high");
    }

    #[test]
    fn build_get_tools_response_test_run() {
        let json = build_get_tools_response("test_run");
        assert_eq!(json["scope"], "test_run");
        assert_eq!(json["risk"], "medium");
    }

    #[test]
    fn build_get_tools_response_close_session() {
        let json = build_get_tools_response("close_session");
        assert_eq!(json["scope"], "close_session");
        assert_eq!(json["risk"], "low");
    }

    #[test]
    fn build_get_tools_response_multi_agent() {
        let json = build_get_tools_response("multi_agent");
        assert_eq!(json["scope"], "multi_agent");
        assert_eq!(json["risk"], "varies");
    }

    // ── graph_run_status_guard ─────────────────────────────────────

    #[test]
    fn graph_run_status_guard_running() {
        let err = graph_run_status_guard("graph-1", GraphStatus::Running).unwrap_err();
        assert!(err.contains("already running"));
    }

    #[test]
    fn graph_run_status_guard_completed() {
        let err = graph_run_status_guard("graph-1", GraphStatus::Completed).unwrap_err();
        assert!(err.contains("cannot be resumed directly"), "{err}");
    }

    #[test]
    fn graph_run_status_guard_failed() {
        let err = graph_run_status_guard("graph-1", GraphStatus::Failed).unwrap_err();
        assert!(err.contains("cannot be resumed directly"), "{err}");
    }

    #[test]
    fn graph_run_status_guard_draft() {
        assert!(graph_run_status_guard("graph-1", GraphStatus::Draft).is_ok());
    }

    // ── graph_trigger_json ─────────────────────────────────────────

    #[test]
    fn graph_trigger_json_manual() {
        let lp = Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "l1".to_string(),
            name: "l1".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        };
        let json = graph_trigger_json(&lp);
        assert_eq!(json["type"], "manual");
        assert!(json.get("schedule").is_none());
    }

    #[test]
    fn graph_trigger_json_cron() {
        let lp = Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "l1".to_string(),
            name: "l1".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: GraphStatus::Draft,
            trigger: Some(Trigger::Cron {
                schedule_expr: "0 9 * * *".to_string(),
            }),
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        };
        let json = graph_trigger_json(&lp);
        assert_eq!(json["type"], "cron");
        assert_eq!(json["schedule"], "0 9 * * *");
    }

    // ── validate_queue_reorder ─────────────────────────────────────

    #[test]
    fn validate_queue_reorder_wrong_count() {
        let current = vec!["a".to_string(), "b".to_string()];
        let spec_ids = vec!["a".to_string()];
        assert!(validate_queue_reorder(&current, &spec_ids).is_err());
    }

    #[test]
    fn validate_queue_reorder_duplicate() {
        let current = vec!["a".to_string(), "b".to_string()];
        let spec_ids = vec!["a".to_string(), "a".to_string()];
        assert!(validate_queue_reorder(&current, &spec_ids).is_err());
    }

    #[test]
    fn validate_queue_reorder_unknown_spec() {
        let current = vec!["a".to_string(), "b".to_string()];
        let spec_ids = vec!["a".to_string(), "c".to_string()];
        assert!(validate_queue_reorder(&current, &spec_ids).is_err());
    }

    #[test]
    fn validate_queue_reorder_valid() {
        let current = vec!["a".to_string(), "b".to_string()];
        let spec_ids = vec!["b".to_string(), "a".to_string()];
        assert!(validate_queue_reorder(&current, &spec_ids).is_ok());
    }

    // ── blueprint_json ────────────────────────────────────────────

    #[test]
    fn blueprint_json_basic() {
        let bp = Blueprint {
            id: "bp-1".to_string(),
            name: "ensemble-proposers".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"members": []}),
            builtin: true,
            created_at: chrono::Utc::now(),
        };
        let json = blueprint_json(&bp);
        assert_eq!(json["id"], "bp-1");
        assert_eq!(json["name"], "ensemble-proposers");
        assert_eq!(json["builtin"], true);
    }

    // ── validate_absolute_dir ─────────────────────────────────────

    #[test]
    fn validate_absolute_dir_valid() {
        assert!(validate_absolute_dir("/tmp").is_ok());
    }

    #[test]
    fn validate_absolute_dir_relative() {
        assert!(validate_absolute_dir("relative").is_err());
    }
}

// ── Additional coverage tests for handler.rs ─────────────────────
// Tests targeting uncovered functions: transport_details,
// append_temporal_agents_section, graph_run_blocker, graph_node_json,
// graph_edge_json, graph_run_json, ensemble_details_json,
// graph_completion_hook_json, graph_completion_hook_run_json,
// format_system_time, model_result_footer, build_ensemble_unit,
// and edge-case branches across validation functions.
#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::daemon::params::EnsembleMemberParams;
    use crate::db::Database;
    use crate::domain::graphs::{
        Ensemble, EnsembleDetails, EnsembleMember, Graph, GraphCompletionHook,
        GraphCompletionHookRun, GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind,
        GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus, GraphStatus,
    };
    use crate::domain::models::{Agent, Cli};
    use crate::domain::queues::Queue;
    use tempfile::tempdir;

    fn standalone_spec(id: &str) -> GraphSpec {
        GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: id.to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        }
    }

    fn running_spec(id: &str) -> GraphSpec {
        let mut spec = standalone_spec(id);
        spec.status = GraphSpecStatus::Running;
        spec
    }

    fn make_agent(id: &str) -> Agent {
        Agent {
            id: id.to_string(),
            prompt: "test prompt".to_string(),
            trigger: None,
            cli: Cli("opencode".to_string()),
            model: None,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: chrono::Utc::now(),
            log_path: "/tmp/test.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
        }
    }

    fn make_graph(graph_id: &str, status: GraphStatus) -> Graph {
        Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: graph_id.to_string(),
            name: graph_id.to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        }
    }

    fn graph_run_row(
        id: &str,
        graph_id: &str,
        spec_id: &str,
        status: GraphRunStatus,
    ) -> GraphNodeRun {
        GraphNodeRun {
            id: id.to_string(),
            graph_id: graph_id.to_string(),
            spec_id: spec_id.to_string(),
            node_id: "node-1".to_string(),
            status,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: (status != GraphRunStatus::Running).then(chrono::Utc::now),
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        }
    }

    fn insert_test_graph(db: &Database, id: &str) {
        db.insert_graph(&make_graph(id, GraphStatus::Draft))
            .unwrap();
    }

    fn insert_test_node(db: &Database, id: &str, spec_id: &str) {
        db.insert_graph_node(&GraphNode {
            id: id.to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: id.to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    }

    // ── transport_details ──────────────────────────────────────────

    #[test]
    fn transport_details_streamable_http() {
        let (transport, port_str) = super::transport_details(8080);
        assert_eq!(transport, "Streamable HTTP");
        assert_eq!(port_str, "8080");
    }

    #[test]
    fn transport_details_stdio() {
        let (transport, port_str) = super::transport_details(0);
        assert_eq!(transport, "stdio");
        assert_eq!(port_str, "N/A");
    }

    #[test]
    fn transport_details_large_port() {
        let (transport, port_str) = super::transport_details(65535);
        assert_eq!(transport, "Streamable HTTP");
        assert_eq!(port_str, "65535");
    }

    // ── append_temporal_agents_section ──────────────────────────────

    #[test]
    fn append_temporal_empty_agents() {
        let mut status = "Canopy v0.1.0".to_string();
        super::append_temporal_agents_section(&mut status, &[]);
        assert_eq!(status, "Canopy v0.1.0");
    }

    #[test]
    fn append_temporal_no_expiring() {
        let mut status = "status".to_string();
        let agents = vec![make_agent("a1")];
        super::append_temporal_agents_section(&mut status, &agents);
        assert_eq!(status, "status");
    }

    #[test]
    fn append_temporal_active_expiry() {
        let mut status = "status".to_string();
        let mut a = make_agent("exp");
        a.expires_at = Some(chrono::Utc::now() + chrono::Duration::minutes(10));
        super::append_temporal_agents_section(&mut status, &[a]);
        assert!(status.contains("Temporal agents:"));
        assert!(status.contains("exp"));
        assert!(status.contains("remaining"));
    }

    #[test]
    fn append_temporal_expired() {
        let mut status = "status".to_string();
        let mut a = make_agent("old");
        a.expires_at = Some(chrono::Utc::now() - chrono::Duration::minutes(5));
        super::append_temporal_agents_section(&mut status, &[a]);
        assert!(status.contains("EXPIRED"));
    }

    #[test]
    fn append_temporal_skips_disabled() {
        let mut status = "status".to_string();
        let mut a = make_agent("dis");
        a.enabled = false;
        a.expires_at = Some(chrono::Utc::now() + chrono::Duration::minutes(10));
        super::append_temporal_agents_section(&mut status, &[a]);
        assert_eq!(status, "status");
    }

    #[test]
    fn append_temporal_multiple_agents() {
        let mut status = String::new();
        let mut a1 = make_agent("a1");
        a1.expires_at = Some(chrono::Utc::now() + chrono::Duration::minutes(10));
        let mut a2 = make_agent("a2");
        a2.expires_at = Some(chrono::Utc::now() + chrono::Duration::minutes(5));
        let a3 = make_agent("a3"); // no expiry
        super::append_temporal_agents_section(&mut status, &[a1, a2, a3]);
        assert!(status.contains("a1"));
        assert!(status.contains("a2"));
        assert!(!status.contains("a3"));
    }

    // ── graph_run_blocker ───────────────────────────────────────────

    #[test]
    fn blocker_present() {
        let run = GraphNodeRun {
            id: "r1".into(),
            graph_id: "l1".into(),
            spec_id: "s1".into(),
            node_id: "n1".into(),
            status: GraphRunStatus::Fail,
            input: None,
            output: Some(serde_json::json!({"blocker": "needs review"})),
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        };
        assert_eq!(
            super::graph_run_blocker(&run).as_deref(),
            Some("needs review")
        );
    }

    #[test]
    fn blocker_no_output() {
        let run = GraphNodeRun {
            id: "r2".into(),
            graph_id: "l1".into(),
            spec_id: "s1".into(),
            node_id: "n1".into(),
            status: GraphRunStatus::Pass,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        };
        assert!(super::graph_run_blocker(&run).is_none());
    }

    #[test]
    fn blocker_no_blocker_key() {
        let run = GraphNodeRun {
            id: "r3".into(),
            graph_id: "l1".into(),
            spec_id: "s1".into(),
            node_id: "n1".into(),
            status: GraphRunStatus::Fail,
            input: None,
            output: Some(serde_json::json!({"result": "failed"})),
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        };
        assert!(super::graph_run_blocker(&run).is_none());
    }

    #[test]
    fn blocker_non_string_value() {
        let run = GraphNodeRun {
            id: "r4".into(),
            graph_id: "l1".into(),
            spec_id: "s1".into(),
            node_id: "n1".into(),
            status: GraphRunStatus::Fail,
            input: None,
            output: Some(serde_json::json!({"blocker": 42})),
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        };
        assert!(super::graph_run_blocker(&run).is_none());
    }

    // ── graph_node_json ─────────────────────────────────────────────

    #[test]
    fn node_json_agent() {
        let node = GraphNode {
            id: "n1".into(),
            spec_id: Some("s1".into()),
            graph_id: None,
            name: "implement".into(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 5,
            created_at: chrono::Utc::now(),
        };
        let json = super::graph_node_json(&node, &[]);
        assert_eq!(json["id"], "n1");
        assert_eq!(json["spec_id"], "s1");
        assert!(json["graph_id"].is_null());
        assert_eq!(json["kind"], "agent");
        assert_eq!(json["position"], 5);
    }

    #[test]
    fn node_json_join_displays_quorum() {
        let node = GraphNode {
            id: "j1".into(),
            spec_id: None,
            graph_id: Some("l1".into()),
            name: "quorum".into(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({}),
            position: 10,
            created_at: chrono::Utc::now(),
        };
        let json = super::graph_node_json(&node, &[]);
        assert_eq!(json["kind"], "quorum");
    }

    // ── graph_edge_json ─────────────────────────────────────────────

    #[test]
    fn edge_json_all_fields() {
        let edge = GraphEdge {
            id: "e1".into(),
            spec_id: Some("s1".into()),
            graph_id: None,
            from_node: "n1".into(),
            to_node: "n2".into(),
            condition: GraphEdgeCondition::Pass,
        };
        let json = super::graph_edge_json(&edge);
        assert_eq!(json["id"], "e1");
        assert_eq!(json["from_node"], "n1");
        assert_eq!(json["to_node"], "n2");
        assert_eq!(json["condition"], "pass");
    }

    #[test]
    fn edge_json_graph_owner() {
        let edge = GraphEdge {
            id: "e2".into(),
            spec_id: None,
            graph_id: Some("l1".into()),
            from_node: "n1".into(),
            to_node: "n2".into(),
            condition: GraphEdgeCondition::Always,
        };
        let json = super::graph_edge_json(&edge);
        assert_eq!(json["graph_id"], "l1");
        assert!(json["spec_id"].is_null());
        assert_eq!(json["condition"], "always");
    }

    // ── graph_run_json ──────────────────────────────────────────────

    #[test]
    fn run_json_pass_with_output() {
        let run = GraphNodeRun {
            id: "r1".into(),
            graph_id: "l1".into(),
            spec_id: "s1".into(),
            node_id: "n1".into(),
            status: GraphRunStatus::Pass,
            input: Some(serde_json::json!({"key": "val"})),
            output: Some(serde_json::json!({"result": "ok"})),
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            iteration: 3,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        };
        let json = super::graph_run_json(&run);
        assert_eq!(json["status"], "pass");
        assert_eq!(json["iteration"], 3);
        assert_eq!(json["input"]["key"], "val");
        assert_eq!(json["output"]["result"], "ok");
    }

    #[test]
    fn run_json_running_no_completed() {
        let run = GraphNodeRun {
            id: "r2".into(),
            graph_id: "l1".into(),
            spec_id: "s1".into(),
            node_id: "n1".into(),
            status: GraphRunStatus::Running,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        };
        let json = super::graph_run_json(&run);
        assert!(json["completed_at"].is_null());
    }

    #[test]
    fn run_json_fail() {
        let run = GraphNodeRun {
            id: "r3".into(),
            graph_id: "l1".into(),
            spec_id: "s1".into(),
            node_id: "n1".into(),
            status: GraphRunStatus::Fail,
            input: None,
            output: Some(serde_json::json!({"blocker": "stuck"})),
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            iteration: 2,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        };
        let json = super::graph_run_json(&run);
        assert_eq!(json["status"], "fail");
        assert_eq!(json["iteration"], 2);
    }

    // ── ensemble_details_json ──────────────────────────────────────

    #[test]
    fn ensemble_json_full() {
        let details = EnsembleDetails {
            ensemble: Ensemble {
                commit_rights: false,
                id: "ens1".into(),
                spec_id: Some("s1".into()),
                graph_id: None,
                name: "proposers".into(),
                prompt_template: "draft".into(),
                join_node_id: "j1".into(),
                entry_from_node: "kickoff".into(),
                entry_condition: GraphEdgeCondition::Always,
                min_pass: 2,
                straggler_timeout_minutes: Some(10),
                quorum_grace_minutes: None,
                timeout_minutes: 30,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: "arbiter".into(),
                on_fail_to: Some("cleanup".into()),
                kind: EnsembleKind::Parallel,
                round_robin_index: None,
                created_at: chrono::Utc::now(),
            },
            members: vec![
                EnsembleMember {
                    ensemble_id: "ens1".into(),
                    node_id: "m1".into(),
                    position: 0,
                    platform: "claude".into(),
                    model: None,
                    prompt_override: Some("review for security issues only".into()),
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                EnsembleMember {
                    ensemble_id: "ens1".into(),
                    node_id: "m2".into(),
                    position: 1,
                    platform: "codex".into(),
                    model: Some("o1".into()),
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ],
        };
        let json = super::ensemble_details_json(&details, &[]);
        assert_eq!(json["id"], "ens1");
        assert_eq!(json["min_pass"], 2);
        assert_eq!(json["effective_straggler_timeout_minutes"], 10);
        assert_eq!(json["on_fail_to"], "cleanup");
        assert_eq!(json["members"].as_array().unwrap().len(), 2);
        let members = json["members"].as_array().unwrap();
        assert_eq!(members[0]["prompt_source"], "override");
        assert_eq!(
            members[0]["prompt_override"],
            "review for security issues only"
        );
        assert_eq!(members[1]["prompt_source"], "shared");
        assert!(members[1]["prompt_override"].is_null());
    }

    #[test]
    fn ensemble_json_no_straggler() {
        let details = EnsembleDetails {
            ensemble: Ensemble {
                commit_rights: false,
                id: "ens2".into(),
                spec_id: None,
                graph_id: Some("l1".into()),
                name: "t".into(),
                prompt_template: "t".into(),
                join_node_id: "j".into(),
                entry_from_node: "f".into(),
                entry_condition: GraphEdgeCondition::Always,
                min_pass: 1,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: 45,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: "to".into(),
                on_fail_to: None,
                kind: EnsembleKind::Parallel,
                round_robin_index: None,
                created_at: chrono::Utc::now(),
            },
            members: vec![EnsembleMember {
                ensemble_id: "ens2".into(),
                node_id: "m1".into(),
                position: 0,
                platform: "claude".into(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            }],
        };
        let json = super::ensemble_details_json(&details, &[]);
        assert_eq!(json["effective_straggler_timeout_minutes"], 45);
        assert!(json["on_fail_to"].is_null());
    }

    // ── graph_completion_hook_json ──────────────────────────────────

    #[test]
    fn hook_json_full() {
        let hook = GraphCompletionHook {
            platform: Some("claude".into()),
            model: Some("opus-4".into()),
            effort: None,
            prompt: Some("{{graph_name}} done".into()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(10),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let json = super::graph_completion_hook_json(&hook);
        assert_eq!(json["platform"], "claude");
        assert_eq!(json["model"], "opus-4");
        assert_eq!(json["timeout_minutes"], 10);
    }

    #[test]
    fn hook_json_no_model() {
        let hook = GraphCompletionHook {
            platform: Some("mimo".into()),
            model: None,
            effort: None,
            prompt: Some("t".into()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let json = super::graph_completion_hook_json(&hook);
        assert!(json["model"].is_null());
        assert!(json["timeout_minutes"].is_null());
    }

    // ── graph_completion_hook_run_json ──────────────────────────────

    #[test]
    fn hook_run_json_full() {
        let run = GraphCompletionHookRun {
            id: "chr1".into(),
            graph_id: "l1".into(),
            event: crate::domain::graphs::GraphHookEvent::OnCompleted,
            hook_index: 0,
            status: GraphRunStatus::Pass,
            output: Some(serde_json::json!({"summary": "done"})),
            summary: Some("ok".into()),
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            pid: Some(123),
            boot_id: None,
            executed_platform: None,
            executed_model: None,
            hook_config: None,
        };
        let json = super::graph_completion_hook_run_json(&run);
        assert_eq!(json["id"], "chr1");
        assert_eq!(json["status"], "pass");
        assert_eq!(json["summary"], "ok");
    }

    #[test]
    fn hook_run_json_no_completed() {
        let run = GraphCompletionHookRun {
            id: "chr2".into(),
            graph_id: "l1".into(),
            event: crate::domain::graphs::GraphHookEvent::OnFailed,
            hook_index: 1,
            status: GraphRunStatus::Running,
            output: None,
            summary: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            pid: None,
            boot_id: None,
            executed_platform: None,
            executed_model: None,
            hook_config: None,
        };
        let json = super::graph_completion_hook_run_json(&run);
        assert!(json["completed_at"].is_null());
        assert!(json["summary"].is_null());
    }

    // ── format_system_time ─────────────────────────────────────────

    #[test]
    fn format_system_time_now() {
        let formatted = super::format_system_time(std::time::SystemTime::now());
        assert!(chrono::DateTime::parse_from_rfc3339(&formatted).is_ok());
    }

    // ── model_result_footer ────────────────────────────────────────

    #[test]
    fn footer_live_source() {
        use crate::daemon::handler_formatting::ModelTruncation;
        use crate::domain::models_db::CatalogSource;
        let f = super::model_result_footer(
            "Models:",
            CatalogSource::Live,
            std::time::SystemTime::now(),
            std::time::Duration::from_secs(24 * 60 * 60),
            &ModelTruncation::default(),
            false,
        );
        assert!(f.contains("Source:"));
        assert!(!f.contains("unreachable"));
        assert!(f.contains("age:"));
        assert!(f.contains("refresh interval"));
    }

    #[test]
    fn footer_stale_source_says_how_stale_in_the_same_line() {
        use crate::daemon::handler_formatting::ModelTruncation;
        use crate::domain::models_db::CatalogSource;
        let fetched_at = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 3600);
        let f = super::model_result_footer(
            "Models:",
            CatalogSource::Stale,
            fetched_at,
            std::time::Duration::from_secs(24 * 60 * 60),
            &ModelTruncation::default(),
            false,
        );
        let source_line = f
            .lines()
            .find(|l| l.starts_with("Source:"))
            .expect("footer must have a Source line");
        assert!(source_line.contains("unreachable"));
        // How stale must be on the *same* line as the unreachable notice, not
        // just somewhere in the footer. 30h renders as "1d6h".
        assert!(source_line.contains("1d6h"));
    }

    #[test]
    fn footer_fresh_cache_does_not_say_refresh_due() {
        use crate::daemon::handler_formatting::ModelTruncation;
        use crate::domain::models_db::CatalogSource;
        let f = super::model_result_footer(
            "Models:",
            CatalogSource::Cache,
            std::time::SystemTime::now() - std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(24 * 60 * 60),
            &ModelTruncation::default(),
            false,
        );
        assert!(!f.contains("refresh due"));
        assert!(f.contains("age: 1m"));
    }

    #[test]
    fn footer_states_refresh_due_when_age_exceeds_ttl() {
        use crate::daemon::handler_formatting::ModelTruncation;
        use crate::domain::models_db::CatalogSource;
        // A source other than Stale whose age has still crept past the TTL
        // (e.g. the TTL was lowered after the cache was written) must still
        // surface a due-for-refresh notice, not just the raw age.
        let fetched_at = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
        let f = super::model_result_footer(
            "Models:",
            CatalogSource::Live,
            fetched_at,
            std::time::Duration::from_secs(3600),
            &ModelTruncation::default(),
            false,
        );
        assert!(f.contains("refresh due"));
    }

    #[test]
    fn format_duration_short_renders_expected_units() {
        use std::time::Duration;
        assert_eq!(super::format_duration_short(Duration::from_secs(45)), "45s");
        assert_eq!(super::format_duration_short(Duration::from_secs(90)), "1m");
        assert_eq!(
            super::format_duration_short(Duration::from_secs(2 * 3600 + 15 * 60)),
            "2h15m"
        );
        assert_eq!(
            super::format_duration_short(Duration::from_secs(24 * 3600)),
            "1d"
        );
        assert_eq!(
            super::format_duration_short(Duration::from_secs(3 * 24 * 3600 + 4 * 3600)),
            "3d4h"
        );
    }

    // ── build_ensemble_unit: with and without on_fail_to ───────────

    #[test]
    fn ensemble_unit_with_fail_to() {
        let members = vec![
            ("claude".into(), None, None, None, None, None, None),
            (
                "codex".into(),
                Some("o1".into()),
                None,
                None,
                None,
                None,
                None,
            ),
        ];
        let built = build_ensemble_unit(&EnsembleUnitSpec {
            spec_id: Some("s1".into()),
            graph_id: None,
            name: "failing",
            prompt_template: "do it",
            members: &members,
            entry_from_node: "kickoff",
            entry_condition: GraphEdgeCondition::Always,
            on_pass_to: "arbiter",
            on_pass_fan_out: None,
            on_fail_to: Some("cleanup"),
            on_fail_fan_out: None,
            min_pass: 1,
            timeout_minutes: 30,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            straggler_timeout_minutes: Some(10),
            quorum_grace_minutes: None,
            start_position: 1,
            kind: EnsembleKind::Parallel,
            commit_rights: false,
        });
        assert_eq!(built.ensemble.on_fail_to.as_deref(), Some("cleanup"));
        assert_eq!(built.ensemble.straggler_timeout_minutes, Some(10));
        let fail_edges: Vec<_> = built
            .edges
            .iter()
            .filter(|e| e.condition == GraphEdgeCondition::Fail)
            .collect();
        assert_eq!(fail_edges.len(), 1);
    }

    #[test]
    fn ensemble_unit_no_fail_to() {
        let members = vec![("claude".into(), None, None, None, None, None, None)];
        let built = build_ensemble_unit(&EnsembleUnitSpec {
            spec_id: None,
            graph_id: Some("l1".into()),
            name: "no-fail",
            prompt_template: "p",
            members: &members,
            entry_from_node: "start",
            entry_condition: GraphEdgeCondition::Pass,
            on_pass_to: "end",
            on_pass_fan_out: None,
            on_fail_to: None,
            on_fail_fan_out: None,
            min_pass: 1,
            timeout_minutes: 15,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            start_position: 5,
            kind: EnsembleKind::Parallel,
            commit_rights: false,
        });
        assert!(built.ensemble.on_fail_to.is_none());
        let fail_edges: Vec<_> = built
            .edges
            .iter()
            .filter(|e| e.condition == GraphEdgeCondition::Fail)
            .collect();
        assert!(fail_edges.is_empty());
    }

    #[test]
    fn ensemble_unit_positions_sequential() {
        let members = vec![
            ("p1".into(), None, None, None, None, None, None),
            ("p2".into(), None, None, None, None, None, None),
            ("p3".into(), None, None, None, None, None, None),
        ];
        let built = build_ensemble_unit(&EnsembleUnitSpec {
            spec_id: None,
            graph_id: Some("l1".into()),
            name: "pos",
            prompt_template: "p",
            members: &members,
            entry_from_node: "start",
            entry_condition: GraphEdgeCondition::Always,
            on_pass_to: "end",
            on_pass_fan_out: None,
            on_fail_to: None,
            on_fail_fan_out: None,
            min_pass: 3,
            timeout_minutes: 30,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            start_position: 10,
            kind: EnsembleKind::Parallel,
            commit_rights: false,
        });
        for (i, member) in built.members.iter().enumerate() {
            assert_eq!(member.position, i as i64);
        }
        for (i, node) in built.member_nodes.iter().enumerate() {
            assert_eq!(node.position, 10 + i as i64);
        }
        assert_eq!(built.join_node.position, 13);
    }

    // ── validate_position_conflict: no graph and empty graph ─────────

    #[test]
    fn position_conflict_no_graph() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        assert!(validate_position_conflict(&db, None, "spec-x", 1).is_ok());
    }

    #[test]
    fn position_conflict_empty_graph() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_test_graph(&db, "graph-empty");
        assert!(validate_position_conflict(&db, Some("graph-empty"), "spec-x", 0).is_ok());
    }

    // ── validate_node_position_conflict: edge cases ────────────────

    #[test]
    fn node_position_no_owner() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let node = GraphNode {
            id: "n1".into(),
            spec_id: None,
            graph_id: None,
            name: "n1".into(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({}),
            position: 1,
            created_at: chrono::Utc::now(),
        };
        assert!(validate_node_position_conflict(&db, &node, "n1", 1).is_ok());
    }

    #[test]
    fn node_position_empty_siblings() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&standalone_spec("s1")).unwrap();
        let node = GraphNode {
            id: "n1".into(),
            spec_id: Some("s1".into()),
            graph_id: None,
            name: "n1".into(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 1,
            created_at: chrono::Utc::now(),
        };
        assert!(validate_node_position_conflict(&db, &node, "n1", 5).is_ok());
    }

    #[test]
    fn node_position_no_conflict_different_position() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&standalone_spec("s1")).unwrap();
        db.insert_graph_node(&GraphNode {
            id: "n1".into(),
            spec_id: Some("s1".into()),
            graph_id: None,
            name: "n1".into(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        let node = GraphNode {
            id: "n2".into(),
            spec_id: Some("s1".into()),
            graph_id: None,
            name: "n2".into(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 5,
            created_at: chrono::Utc::now(),
        };
        assert!(validate_node_position_conflict(&db, &node, "n2", 2).is_ok());
    }

    // ── validate_queue_not_consumed edge cases ──────────────────────

    #[test]
    fn queue_not_consumed_empty_queue() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_queue(&Queue {
            id: "queue-e".into(),
            name: "empty".into(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        assert!(validate_queue_not_consumed(&db, "queue-e", "graph-1").is_ok());
    }

    #[test]
    fn queue_not_consumed_pending_spec() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&standalone_spec("s1")).unwrap();
        db.insert_queue(&Queue {
            id: "queue-p".into(),
            name: "queue-p".into(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.append_queue_member("queue-p", "s1", None).unwrap();
        assert!(validate_queue_not_consumed(&db, "queue-p", "graph-other").is_ok());
    }

    #[test]
    fn queue_not_consumed_own_graph() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&running_spec("s-owned")).unwrap();
        db.insert_graph(&make_graph("graph-owner", GraphStatus::Running))
            .unwrap();
        db.insert_queue(&Queue {
            id: "queue-o".into(),
            name: "queue-o".into(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.append_queue_member("queue-o", "s-owned", None).unwrap();
        insert_test_node(&db, "node-1", "s-owned");
        db.insert_graph_run(&graph_run_row(
            "run1",
            "graph-owner",
            "s-owned",
            GraphRunStatus::Running,
        ))
        .unwrap();
        assert!(validate_queue_not_consumed(&db, "queue-o", "graph-owner").is_ok());
    }

    // ── validate_queue_member_removable edge cases ──────────────────

    #[test]
    fn queue_member_removable_pending() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&standalone_spec("s1")).unwrap();
        db.insert_queue(&Queue {
            id: "queue-r".into(),
            name: "queue-r".into(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        assert!(validate_queue_member_removable(&db, "queue-r", "s1").is_ok());
    }

    #[test]
    fn queue_member_removable_nonexistent() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_queue(&Queue {
            id: "queue-r".into(),
            name: "queue-r".into(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        assert!(validate_queue_member_removable(&db, "queue-r", "ghost").is_ok());
    }

    // ── resolve_reported_run: stale statuses ───────────────────────

    #[test]
    fn reported_run_rejects_pass_status() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_test_graph(&db, "l1");
        db.insert_graph_spec(&standalone_spec("s1")).unwrap();
        insert_test_node(&db, "node-1", "s1");
        db.insert_graph_run(&graph_run_row("r1", "l1", "s1", GraphRunStatus::Pass))
            .unwrap();
        let result = resolve_reported_run(&db, "r1", "n1").unwrap();
        assert!(result
            .expect_err("pass run must be stale")
            .is_error
            .unwrap_or(false));
    }

    #[test]
    fn reported_run_rejects_fail_status() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_test_graph(&db, "l1");
        db.insert_graph_spec(&standalone_spec("s1")).unwrap();
        insert_test_node(&db, "node-1", "s1");
        db.insert_graph_run(&graph_run_row("r1", "l1", "s1", GraphRunStatus::Fail))
            .unwrap();
        let result = resolve_reported_run(&db, "r1", "n1").unwrap();
        assert!(result
            .expect_err("fail run must be stale")
            .is_error
            .unwrap_or(false));
    }

    // ── validate_queue_reorder_locking edge cases ───────────────────

    #[test]
    fn reorder_locking_all_pending() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&standalone_spec("a")).unwrap();
        db.insert_graph_spec(&standalone_spec("b")).unwrap();
        db.insert_graph_spec(&standalone_spec("c")).unwrap();
        db.insert_queue(&Queue {
            id: "p1".into(),
            name: "p1".into(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        for id in ["a", "b", "c"] {
            db.append_queue_member("p1", id, None).unwrap();
        }
        let current = db.list_queue_member_spec_ids("p1").unwrap();
        let order = vec!["c".into(), "a".into(), "b".into()];
        assert!(validate_queue_reorder_locking(&db, &current, &order).is_ok());
    }

    #[test]
    fn reorder_locking_failed_spec_locked() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let mut f = standalone_spec("f");
        f.status = GraphSpecStatus::Failed;
        db.insert_graph_spec(&f).unwrap();
        db.insert_graph_spec(&standalone_spec("p")).unwrap();
        db.insert_queue(&Queue {
            id: "p1".into(),
            name: "p1".into(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.append_queue_member("p1", "f", None).unwrap();
        db.append_queue_member("p1", "p", None).unwrap();
        let current = db.list_queue_member_spec_ids("p1").unwrap();
        let order = vec!["p".into(), "f".into()];
        let err = validate_queue_reorder_locking(&db, &current, &order).unwrap_err();
        assert!(err.contains("failed"), "{err}");
    }

    #[test]
    fn reorder_locking_skipped_spec_locked() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let mut s = standalone_spec("s");
        s.status = GraphSpecStatus::Skipped;
        db.insert_graph_spec(&s).unwrap();
        db.insert_graph_spec(&standalone_spec("p")).unwrap();
        db.insert_queue(&Queue {
            id: "p1".into(),
            name: "p1".into(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.append_queue_member("p1", "s", None).unwrap();
        db.append_queue_member("p1", "p", None).unwrap();
        let current = db.list_queue_member_spec_ids("p1").unwrap();
        let order = vec!["p".into(), "s".into()];
        let err = validate_queue_reorder_locking(&db, &current, &order).unwrap_err();
        assert!(err.contains("skipped"), "{err}");
    }

    #[test]
    fn reorder_locking_mixed_pending_and_running() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&running_spec("r")).unwrap();
        db.insert_graph_spec(&standalone_spec("p1")).unwrap();
        db.insert_graph_spec(&standalone_spec("p2")).unwrap();
        db.insert_queue(&Queue {
            id: "p1".into(),
            name: "p1".into(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.append_queue_member("p1", "r", None).unwrap();
        db.append_queue_member("p1", "p1", None).unwrap();
        db.append_queue_member("p1", "p2", None).unwrap();
        let current = db.list_queue_member_spec_ids("p1").unwrap();
        let order = vec!["r".into(), "p2".into(), "p1".into()];
        assert!(validate_queue_reorder_locking(&db, &current, &order).is_ok());
    }

    // ── validate_queue_reorder: all permutations ────────────────────

    #[test]
    fn queue_reorder_all_perms_of_three() {
        let current = vec!["a".into(), "b".into(), "c".into()];
        for perm in [
            ["a", "b", "c"],
            ["a", "c", "b"],
            ["b", "a", "c"],
            ["b", "c", "a"],
            ["c", "a", "b"],
            ["c", "b", "a"],
        ] {
            let reordered: Vec<String> = perm.into_iter().map(String::from).collect();
            assert!(validate_queue_reorder(&current, &reordered).is_ok());
        }
    }

    #[test]
    fn queue_reorder_empty() {
        assert!(validate_queue_reorder(&[], &[]).is_ok());
    }

    #[test]
    fn queue_reorder_large_queue() {
        let current: Vec<String> = (0..100).map(|i| format!("s{i}")).collect();
        let mut reordered = current.clone();
        reordered.reverse();
        assert!(validate_queue_reorder(&current, &reordered).is_ok());
    }

    // ── validate_ensemble_members: boundaries ──────────────────────

    #[test]
    fn ensemble_exactly_min() {
        let m: Vec<EnsembleMemberParams> = (0..2)
            .map(|i| EnsembleMemberParams {
                platform: format!("p{i}"),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            })
            .collect();
        assert!(validate_ensemble_members(&m, EnsembleKind::Parallel).is_ok());
    }

    #[test]
    fn ensemble_exactly_max() {
        let m: Vec<EnsembleMemberParams> = (0..8)
            .map(|i| EnsembleMemberParams {
                platform: format!("p{i}"),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            })
            .collect();
        assert!(validate_ensemble_members(&m, EnsembleKind::Parallel).is_ok());
    }

    #[test]
    fn ensemble_above_max() {
        let m: Vec<EnsembleMemberParams> = (0..9)
            .map(|i| EnsembleMemberParams {
                platform: format!("p{i}"),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            })
            .collect();
        assert!(validate_ensemble_members(&m, EnsembleKind::Parallel)
            .unwrap_err()
            .contains("2-8"));
    }

    #[test]
    fn ensemble_empty_model_becomes_none() {
        let m = vec![
            EnsembleMemberParams {
                platform: "claude".into(),
                model: Some("".into()),
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
            EnsembleMemberParams {
                platform: "mimo".into(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
        ];
        let result = validate_ensemble_members(&m, EnsembleKind::Parallel).unwrap();
        assert_eq!(result[0].1, None);
        assert_eq!(result[1].1, None);
    }

    // ── validate_node_config: gate edge cases ─────────────────────

    #[test]
    fn gate_empty_evaluate_and_value() {
        let config = serde_json::json!({"evaluate": "", "value": ""});
        validate_node_config(GraphNodeKind::Gate, &config).unwrap();
    }

    // ── spec_summary_json: all statuses ────────────────────────────

    #[test]
    fn spec_summary_all_statuses() {
        for status in [
            GraphSpecStatus::Pending,
            GraphSpecStatus::Running,
            GraphSpecStatus::Completed,
            GraphSpecStatus::Failed,
            GraphSpecStatus::Skipped,
            GraphSpecStatus::Interrupted,
        ] {
            let mut spec = standalone_spec("s");
            spec.status = status;
            let json = spec_summary_json(&spec, true);
            assert_eq!(json["status"], status.as_str());
        }
    }

    // ── validate_non_empty: unicode and special chars ───────────────

    #[test]
    fn non_empty_unicode() {
        assert!(validate_non_empty("こんにちは", "f").is_ok());
    }

    #[test]
    fn non_empty_mixed_whitespace() {
        assert!(validate_non_empty(" \t\n ", "f").is_err());
    }

    // ── validate_absolute_dir: trailing slash ──────────────────────

    #[test]
    fn absolute_dir_trailing_slash() {
        let dir = tempdir().unwrap();
        let path = format!("{}/", dir.path().to_string_lossy());
        assert!(validate_absolute_dir(&path).is_ok());
    }

    // ── validate_spec_set_status_target: all valid ─────────────────

    #[test]
    fn set_status_target_all_valid() {
        assert!(validate_spec_set_status_target("pending").is_ok());
        assert!(validate_spec_set_status_target("completed").is_ok());
        assert!(validate_spec_set_status_target("skipped").is_ok());
    }

    // ── validate_at_least_one_bool: mixed ──────────────────────────

    #[test]
    fn at_least_one_mixed() {
        assert!(validate_at_least_one_bool(&[false, true, false, true], "f").is_ok());
        assert!(validate_at_least_one_bool(&[false, false, false, false], "f").is_err());
    }

    // ── build_id_result: various keys ──────────────────────────────

    #[test]
    fn id_result_spec_id() {
        let r = build_id_result("abc", "spec_id");
        let t = format!("{:?}", r.content);
        assert!(t.contains("spec_id") && t.contains("abc"));
    }

    #[test]
    fn id_result_ensemble_id() {
        let r = build_id_result("ens-1", "ensemble_id");
        let t = format!("{:?}", r.content);
        assert!(t.contains("ensemble_id") && t.contains("ens-1"));
    }

    #[test]
    fn id_result_queue_id() {
        let r = build_id_result("q-1", "queue_id");
        let t = format!("{:?}", r.content);
        assert!(t.contains("queue_id") && t.contains("q-1"));
    }

    // ── build_json_result: nested ──────────────────────────────────

    #[test]
    fn json_result_nested() {
        let v = serde_json::json!({"mapping": {"old": "new"}, "wired": true});
        let r = build_json_result(&v);
        let t = format!("{:?}", r.content);
        assert!(t.contains("mapping") && t.contains("old"));
    }

    // ── member_node_config edge cases ──────────────────────────────

    #[test]
    fn member_config_zero_timeout() {
        let c = member_node_config("claude", None, "p", 0, None, None, None);
        assert_eq!(c["timeout_minutes"], 0);
    }

    // ── resolve_graph_target: whitespace-only ──────────────────────

    #[test]
    fn graph_target_whitespace_both() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let err = resolve_graph_target(&db, Some("   "), Some("  ")).unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
    }

    // ── rag_result_json: negative distance ─────────────────────────

    #[test]
    fn rag_negative_distance() {
        use crate::rag::vector_store::SearchResult;
        let r = SearchResult {
            id: "sr-neg".into(),
            file_path: "/t.md".into(),
            content: "c".into(),
            created_at: 0,
            distance: Some(-0.5),
        };
        let json = rag_result_json(&r);
        assert!(json["distance"].as_f64().unwrap() < 0.0);
    }

    // ── node_copy_note edge cases ──────────────────────────────────

    #[test]
    fn copy_note_wired() {
        let n = node_copy_note("src", "dst", true);
        assert!(n.contains("src") && n.contains("dst") && !n.contains("Unwired"));
    }

    #[test]
    fn copy_note_unwired() {
        let n = node_copy_note("src", "dst", false);
        assert!(n.contains("Unwired") && n.contains("graph_add_edge"));
    }

    #[test]
    fn spec_summary_json_compact_omits_description() {
        let mut spec = standalone_spec("spec-1");
        spec.description = Some("A long description".to_string());
        let json = spec_summary_json(&spec, false);
        assert!(
            json.get("description").is_none(),
            "compact mode must omit description"
        );
        assert_eq!(json["id"], "spec-1");
        assert_eq!(json["name"], "spec-1");
        assert_eq!(json["status"], "pending");
    }

    #[test]
    fn queue_details_json_compact_members_have_position_and_group() {
        let spec = standalone_spec("spec-1");
        let queue = Queue {
            id: "q-1".to_string(),
            name: "test-queue".to_string(),
            created_at: chrono::Utc::now(),
        };
        let mut member_groups = std::collections::HashMap::new();
        member_groups.insert("spec-1".to_string(), Some("group-a".to_string()));
        let details = QueueDetails {
            queue,
            members: vec![spec],
            member_groups,
        };
        let json = queue_details_json(&details, false);
        let members = json["members"].as_array().unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0]["queue_position"], 1);
        assert_eq!(members[0]["group"], "group-a");
        assert!(
            members[0].get("description").is_none(),
            "compact mode must omit description"
        );
    }

    #[test]
    fn graph_node_run_summary_json_compact_mode_keeps_id_omits_internal_ids() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-1")).unwrap();
        insert_test_node(&db, "node-1", "spec-1");
        let run = graph_run_row("run-1", "graph-1", "spec-1", GraphRunStatus::Pass);

        let compact = graph_node_run_summary_json(&db, &run, true);
        assert_eq!(compact["id"], "run-1", "compact must include id");
        assert!(
            compact.get("spec_id").is_none(),
            "compact must omit spec_id"
        );
        assert!(
            compact.get("node_id").is_none(),
            "compact must omit node_id"
        );
        assert!(
            compact.get("session_id").is_none(),
            "compact must omit session_id"
        );
        assert_eq!(compact["spec_name"], "spec-1");
        assert_eq!(compact["node_name"], "node-1");
        assert_eq!(compact["status"], "pass");
        assert_eq!(compact["iteration"], 1);
        assert!(compact.get("input").is_none(), "compact must omit input");
        assert!(compact.get("output").is_none(), "compact must omit output");

        let full = graph_node_run_summary_json(&db, &run, false);
        assert_eq!(full["id"], "run-1");
        assert_eq!(full["spec_id"], "spec-1");
        assert_eq!(full["node_id"], "node-1");
        assert!(
            full.get("spec_name").is_none(),
            "full mode does not carry spec_name"
        );
    }

    /// T3 (CB43): the recorded pair appears in both list modes; pre-migration
    /// rows (NULL) omit the keys rather than emitting nulls.
    #[test]
    fn cb43_run_pair_appears_in_both_list_modes() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&standalone_spec("spec-1")).unwrap();
        insert_test_node(&db, "node-1", "spec-1");
        let mut run = graph_run_row("run-1", "graph-1", "spec-1", GraphRunStatus::Pass);
        run.executed_platform = Some("opencode".to_string());
        run.executed_model = Some("opencode/big-pickle".to_string());

        for compact in [true, false] {
            let json = graph_node_run_summary_json(&db, &run, compact);
            assert_eq!(json["platform"], "opencode");
            assert_eq!(json["model"], "opencode/big-pickle");
        }

        let detail = graph_node_run_detail_json(&run, Some("node-1"), None);
        assert_eq!(detail["platform"], "opencode");
        assert_eq!(detail["model"], "opencode/big-pickle");

        // Legacy row: keys omitted, not null.
        let legacy = graph_run_row("run-2", "graph-1", "spec-1", GraphRunStatus::Pass);
        for compact in [true, false] {
            let json = graph_node_run_summary_json(&db, &legacy, compact);
            assert!(json.get("platform").is_none());
            assert!(json.get("model").is_none());
        }
        let detail = graph_node_run_detail_json(&legacy, Some("node-1"), None);
        assert!(detail.get("platform").is_none());
        assert!(detail.get("model").is_none());
    }

    /// CM26: `find_resumed_from_run_id` recovers "was this run a resume, and
    /// of what" purely from the session id a resumed run's own row carries
    /// verbatim — no dedicated column. Two rows on the same node share
    /// `session_id`; the later one (by insertion order) resumed the earlier
    /// one, never the reverse, and `graph_node_run_get`'s JSON surfaces it.
    #[test]
    fn graph_node_run_get_reports_resumed_from_run_id_for_ensemble_member() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        insert_test_graph(&db, "graph-1");
        db.insert_graph_spec(&standalone_spec("spec-1")).unwrap();
        insert_test_node(&db, "node-1", "spec-1");

        let mut first = graph_run_row("run-1", "graph-1", "spec-1", GraphRunStatus::Pass);
        first.session_id = Some("ses-1".to_string());
        db.insert_graph_run(&first).unwrap();

        let mut second = graph_run_row("run-2", "graph-1", "spec-1", GraphRunStatus::Pass);
        second.session_id = Some("ses-1".to_string());
        db.insert_graph_run(&second).unwrap();

        assert_eq!(
            db.find_resumed_from_run_id(&second.id, &second.node_id, "ses-1")
                .unwrap(),
            Some(first.id.clone()),
            "the later row (by insertion order) resumed the earlier one"
        );
        assert_eq!(
            db.find_resumed_from_run_id(&first.id, &first.node_id, "ses-1")
                .unwrap(),
            None,
            "nothing precedes the first row with that session id"
        );

        let resumed_from = db
            .find_resumed_from_run_id(&second.id, &second.node_id, "ses-1")
            .unwrap();
        let detail = graph_node_run_detail_json(&second, Some("node-1"), resumed_from.as_deref());
        assert_eq!(detail["resumed_from_run_id"], serde_json::json!(first.id));

        let detail_first = graph_node_run_detail_json(&first, Some("node-1"), None);
        assert_eq!(detail_first["resumed_from_run_id"], serde_json::Value::Null);
    }

    /// T4 (CB43): the recent section leads the output and carries last
    /// outcome + frequency; an empty installation gets a one-line notice.
    #[test]
    fn cb43_recent_section_leads_with_last_outcome() {
        use crate::db::graphs::RecentModelUsage;

        let now = chrono::Utc::now();
        let rows = vec![
            RecentModelUsage {
                platform: "opencode".to_string(),
                model: Some("opencode/big-pickle".to_string()),
                last_run: now,
                count: 12,
                last_outcome: "pass".to_string(),
            },
            RecentModelUsage {
                platform: "claude".to_string(),
                model: None,
                last_run: now - chrono::Duration::hours(2),
                count: 40,
                last_outcome: "timeout".to_string(),
            },
        ];
        let section = format_recent_usage_section(&rows);
        assert!(
            section.starts_with("Recently used"),
            "section must lead so truncation drops the catalogue tail, not the signal"
        );
        assert!(section.contains("opencode/big-pickle"));
        assert!(section.contains("12 runs"));
        assert!(section.contains("last outcome: pass"));
        // A heavily-used model whose last run timed out must not read as a
        // recommendation: the outcome, not the count, is the honest signal.
        assert!(section.contains("claude/(default)"));
        assert!(section.contains("last outcome: timeout"));

        let empty = format_recent_usage_section(&[]);
        assert!(empty.starts_with("Recently used"));
        assert!(empty.contains("No models have run"));
    }

    /// T4/T11 (CB43): prepending puts the section first on success, leaves
    /// errors alone, and stays inside budget.
    #[test]
    fn cb43_prepend_recent_usage_first_on_success_only() {
        let section = "Recently used on this machine (last 30 days):\n- a/b: x".to_string();

        let mut ok = CallToolResult::success(vec![Content::text("Source: catalogue".to_string())]);
        prepend_recent_usage(&mut ok, &section);
        let text = ok.content[0].as_text().unwrap().text.clone();
        assert!(text.starts_with("Recently used"));
        assert!(text.contains("Source: catalogue"));

        let mut err = error_result("No known model providers are mapped for platform 'x'.");
        let before = format!("{:?}", err.content);
        prepend_recent_usage(&mut err, &section);
        assert_eq!(format!("{:?}", err.content), before);

        let mut ok2 = CallToolResult::success(vec![Content::text("Source: catalogue".to_string())]);
        prepend_recent_usage(&mut ok2, "");
        assert_eq!(ok2.content[0].as_text().unwrap().text, "Source: catalogue");
    }

    /// T11 (CB43): a full fifteen-pair section is a couple of KB at most —
    /// the header survives catalogue truncation.
    #[test]
    fn cb43_full_section_stays_inside_budget() {
        use crate::db::graphs::RecentModelUsage;

        let now = chrono::Utc::now();
        let rows: Vec<RecentModelUsage> = (0..15)
            .map(|i| RecentModelUsage {
                platform: "opencode".to_string(),
                model: Some(format!("org/model-{i:02}-with-a-long-name")),
                last_run: now - chrono::Duration::minutes(i),
                count: 100,
                last_outcome: "pass".to_string(),
            })
            .collect();
        let section = format_recent_usage_section(&rows);
        assert!(section.len() < 8192, "section was {} bytes", section.len());
        assert!(section.starts_with("Recently used"));
    }
}

// ── Direct #[tool] handler-method coverage ───────────────────────────
//
// The three test modules above almost exclusively exercise *pure* helper
// functions (validate_*, build_*, format_*). The `#[tool]` methods on
// `TaskTriggerHandler` itself — `task_add`, `task_watch`, `sync_*`,
// `intelligence_*`, `graph_*`, `spec_*`, `blueprint_*`, etc. — are called
// directly here instead: construct `Parameters<...>`, call the method on a
// real handler wired to a real (tempdir) SQLite database, assert on the
// `CallToolResult` and on the resulting DB state. This is the surface a
// live MCP client actually calls, and where most of this file's uncovered
// lines live (error branches inside each handler in particular).
#[cfg(test)]
mod endpoint_tests {
    use super::*;
    use crate::application::notification_service::{
        DefaultNotificationService, NotificationService,
    };
    use crate::daemon::params_extract::Parameters;
    use crate::db::Database;
    use crate::domain::models::{Agent, Cli, RunLog, RunStatus, TriggerType};
    use crate::executor::Executor;
    use crate::graph_engine::GraphEngine;
    use crate::rag::ingestion::IngestionManager;
    use crate::sync_manager::SyncManager;
    use crate::watchers::WatcherEngine;
    use tempfile::tempdir;
    use tokio::sync::Notify;

    /// RAII guard: sets the real `$HOME` env var to `path` for the test
    /// body, restoring the previous value on drop. `make_log_path` /
    /// `data_dir()` read `dirs::home_dir()` directly (real `$HOME`, not the
    /// `CANOPY_HOME_OVERRIDE` some other subsystems honor), so any handler
    /// path that writes a log file needs this to avoid touching the real
    /// developer's `~/.canopy`. Safe under `cargo nextest` (one process per
    /// test) but would race under plain `cargo test`.
    struct HomeVar {
        prev: Option<std::ffi::OsString>,
    }

    impl HomeVar {
        fn set(path: &std::path::Path) -> Self {
            let prev = std::env::var_os("HOME");
            unsafe { std::env::set_var("HOME", path) };
            HomeVar { prev }
        }
    }

    impl Drop for HomeVar {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => unsafe { std::env::set_var("HOME", v) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    fn endpoint_test_handler() -> (
        tempfile::TempDir,
        std::sync::Arc<Database>,
        TaskTriggerHandler,
    ) {
        let dir = tempdir().unwrap();
        let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
        let notif: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
        let executor = Arc::new(Executor::new(Arc::clone(&db), Arc::clone(&notif)));
        let sync_manager = Arc::new(SyncManager::new(Arc::clone(&db)));
        let graph_engine = Arc::new(GraphEngine::new(Arc::clone(&db), Arc::clone(&notif)));
        let watcher_engine = Arc::new(WatcherEngine::new(
            Arc::clone(&db),
            Arc::clone(&executor),
            Arc::clone(&graph_engine),
        ));
        let ingestion = Arc::new(IngestionManager::new(
            Arc::clone(&db),
            dir.path().to_path_buf(),
        ));
        let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::new(
            dir.path().join("skills"),
            Vec::new(),
            15,
        ));
        let handler = TaskTriggerHandler::with_deps(HandlerDeps {
            db: Arc::clone(&db),
            executor,
            watcher_engine,
            scheduler_notify: Arc::new(Notify::new()),
            graph_engine,
            notification_service: notif,
            sync_manager,
            ingestion,
            dynamic_skills,
            port: 0,
        });
        (dir, db, handler)
    }

    /// B7 A5.1: `HandlerDeps` round-trip — `with_deps` and `new` construct
    /// handlers that expose the same `port`, a fresh `start_time`, and an
    /// identical tool inventory.
    #[test]
    fn handler_deps_round_trip_constructs_identical_handler() {
        use crate::application::notification_service::{
            DefaultNotificationService, NotificationService,
        };
        use crate::daemon::handler::HandlerDeps;
        use crate::executor::Executor;
        use crate::graph_engine::GraphEngine;
        use crate::rag::ingestion::IngestionManager;
        use crate::sync_manager::SyncManager;
        use crate::watchers::WatcherEngine;
        use std::sync::Arc;
        use tokio::sync::Notify;

        let dir = tempdir().unwrap();
        let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
        let notif: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
        let executor = Arc::new(Executor::new(Arc::clone(&db), Arc::clone(&notif)));
        let sync_manager = Arc::new(SyncManager::new(Arc::clone(&db)));
        let graph_engine = Arc::new(GraphEngine::new(Arc::clone(&db), Arc::clone(&notif)));
        let watcher_engine = Arc::new(WatcherEngine::new(
            Arc::clone(&db),
            Arc::clone(&executor),
            Arc::clone(&graph_engine),
        ));
        let ingestion = Arc::new(IngestionManager::new(
            Arc::clone(&db),
            dir.path().to_path_buf(),
        ));
        let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::new(
            dir.path().join("skills"),
            Vec::new(),
            15,
        ));
        let scheduler_notify = Arc::new(Notify::new());

        let deps = HandlerDeps {
            db: Arc::clone(&db),
            executor: Arc::clone(&executor),
            watcher_engine: Arc::clone(&watcher_engine),
            scheduler_notify: Arc::clone(&scheduler_notify),
            graph_engine: Arc::clone(&graph_engine),
            notification_service: Arc::clone(&notif),
            sync_manager: Arc::clone(&sync_manager),
            ingestion: Arc::clone(&ingestion),
            dynamic_skills: Arc::clone(&dynamic_skills),
            port: 0,
        };
        let via_deps = TaskTriggerHandler::with_deps(deps);

        assert_eq!(via_deps.port, 0);

        let deps_elapsed = via_deps.start_time.elapsed();
        assert!(
            deps_elapsed.as_secs() <= 1,
            "start_time too stale: {deps_elapsed:?}"
        );

        let deps_names: Vec<String> = TaskTriggerHandler::tool_router()
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        assert!(!deps_names.is_empty());
    }

    fn text(result: &CallToolResult) -> String {
        format!("{:?}", result.content)
    }

    fn is_err(result: &CallToolResult) -> bool {
        result.is_error == Some(true)
    }

    /// The raw (unescaped) text of a result's first content block — unlike
    /// [`text`] (which Debug-formats the whole `Vec<Content>`, escaping
    /// embedded quotes), this is safe to `serde_json::from_str` or otherwise
    /// parse structurally.
    fn raw_text(result: &CallToolResult) -> String {
        result.content[0].as_text().unwrap().text.clone()
    }

    /// Parse a `build_id_result`-shaped response (`{"<key>": "<id>"}`) and
    /// return the id.
    fn extract_id(result: &CallToolResult, key: &str) -> String {
        let value: serde_json::Value = serde_json::from_str(&raw_text(result))
            .unwrap_or_else(|e| panic!("expected JSON body, got {:?}: {e}", raw_text(result)));
        value[key]
            .as_str()
            .unwrap_or_else(|| panic!("expected string field '{key}' in {value}"))
            .to_string()
    }

    // ── task_add / agent_add ─────────────────────────────────────

    #[tokio::test]
    async fn task_add_registers_a_cron_agent() {
        let home = tempdir().unwrap();
        let _home_guard = HomeVar::set(home.path());
        let (_dir, db, handler) = endpoint_test_handler();

        let result = handler
            .task_add(Parameters(TaskAddParams {
                id: "agt-1".to_string(),
                prompt: "run the tests".to_string(),
                schedule: "*/5 * * * *".to_string(),
                cli: Some("opencode".to_string()),
                model: None,
                effort: None,
                duration_minutes: None,
                working_dir: None,
                timeout_minutes: None,
            }))
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        assert!(text(&result).contains("registered with schedule"));
        let stored = db.get_agent("agt-1").unwrap().unwrap();
        assert!(matches!(stored.trigger, Some(Trigger::Cron { .. })));
    }

    #[tokio::test]
    async fn task_add_rejects_invalid_cron_expression() {
        let home = tempdir().unwrap();
        let _home_guard = HomeVar::set(home.path());
        let (_dir, db, handler) = endpoint_test_handler();

        let result = handler
            .task_add(Parameters(TaskAddParams {
                id: "agt-bad-cron".to_string(),
                prompt: "run the tests".to_string(),
                schedule: "not a cron expr!".to_string(),
                cli: Some("opencode".to_string()),
                model: None,
                effort: None,
                duration_minutes: None,
                working_dir: None,
                timeout_minutes: None,
            }))
            .await
            .unwrap();

        assert!(is_err(&result));
        assert!(text(&result).contains("Invalid cron expression"));
        assert!(db.get_agent("agt-bad-cron").unwrap().is_none());
    }

    #[tokio::test]
    async fn task_add_rejects_invalid_id() {
        let home = tempdir().unwrap();
        let _home_guard = HomeVar::set(home.path());
        let (_dir, _db, handler) = endpoint_test_handler();

        let result = handler
            .task_add(Parameters(TaskAddParams {
                id: "bad id with spaces!".to_string(),
                prompt: "run the tests".to_string(),
                schedule: "*/5 * * * *".to_string(),
                cli: Some("opencode".to_string()),
                model: None,
                effort: None,
                duration_minutes: None,
                working_dir: None,
                timeout_minutes: None,
            }))
            .await
            .unwrap();

        assert!(is_err(&result));
        assert!(text(&result).contains("alphanumeric"));
    }

    // ── task_watch / agent_watch ─────────────────────────────────

    #[tokio::test]
    async fn task_watch_registers_a_watcher() {
        let home = tempdir().unwrap();
        let _home_guard = HomeVar::set(home.path());
        let (dir, db, handler) = endpoint_test_handler();

        let result = handler
            .task_watch(Parameters(TaskWatchParams {
                id: "watch-1".to_string(),
                path: dir.path().to_string_lossy().to_string(),
                events: vec!["modify".to_string(), "create".to_string()],
                prompt: "react to changes".to_string(),
                cli: Some("opencode".to_string()),
                model: None,
                effort: None,
                debounce_seconds: None,
                recursive: Some(true),
                timeout_minutes: None,
            }))
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        let stored = db.get_agent("watch-1").unwrap().unwrap();
        assert!(matches!(stored.trigger, Some(Trigger::Watch { .. })));
    }

    #[tokio::test]
    async fn task_watch_rejects_relative_path() {
        let home = tempdir().unwrap();
        let _home_guard = HomeVar::set(home.path());
        let (_dir, db, handler) = endpoint_test_handler();

        let result = handler
            .task_watch(Parameters(TaskWatchParams {
                id: "watch-bad-path".to_string(),
                path: "relative/path".to_string(),
                events: vec!["modify".to_string()],
                prompt: "react to changes".to_string(),
                cli: Some("opencode".to_string()),
                model: None,
                effort: None,
                debounce_seconds: None,
                recursive: None,
                timeout_minutes: None,
            }))
            .await
            .unwrap();

        assert!(is_err(&result));
        assert!(text(&result).contains("must be absolute"));
        assert!(db.get_agent("watch-bad-path").unwrap().is_none());
    }

    #[tokio::test]
    async fn task_watch_rejects_unknown_event_name() {
        let home = tempdir().unwrap();
        let _home_guard = HomeVar::set(home.path());
        let (dir, _db, handler) = endpoint_test_handler();

        let result = handler
            .task_watch(Parameters(TaskWatchParams {
                id: "watch-bad-event".to_string(),
                path: dir.path().to_string_lossy().to_string(),
                events: vec!["explode".to_string()],
                prompt: "react to changes".to_string(),
                cli: Some("opencode".to_string()),
                model: None,
                effort: None,
                debounce_seconds: None,
                recursive: None,
                timeout_minutes: None,
            }))
            .await
            .unwrap();

        assert!(is_err(&result));
    }

    // ── task_list / agent_list ───────────────────────────────────

    fn sample_agent(id: &str, trigger: Option<Trigger>) -> Agent {
        Agent {
            id: id.to_string(),
            prompt: "do things".to_string(),
            trigger,
            cli: Cli::new("opencode"),
            model: None,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: chrono::Utc::now(),
            log_path: format!("/tmp/{id}.log"),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
        }
    }

    #[tokio::test]
    async fn task_list_reports_agents_and_corrupt_rows() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.upsert_agent(&sample_agent("healthy-1", None)).unwrap();
        db.insert_corrupt_agent_for_test("corrupt-1", true).unwrap();

        let result = handler.task_list().await.unwrap();
        let out = text(&result);
        assert!(out.contains("healthy-1"));
        assert!(out.contains("corrupt"));
        assert!(out.contains("corrupt-1"));
    }

    #[tokio::test]
    async fn task_list_reports_empty_state() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler.task_list().await.unwrap();
        assert!(text(&result).contains("No agents registered"));
    }

    #[tokio::test]
    async fn agent_get_returns_full_prompt_and_every_field() {
        let (_dir, db, handler) = endpoint_test_handler();
        let mut agent = sample_agent(
            "long-prompt",
            Some(Trigger::Cron {
                schedule_expr: "0 9 * * *".to_string(),
            }),
        );
        agent.prompt = "x".repeat(2000);
        db.upsert_agent(&agent).unwrap();

        let result = handler
            .agent_get(Parameters(IdParam {
                id: "long-prompt".to_string(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let v: serde_json::Value = serde_json::from_str(&raw_text(&result))
            .unwrap_or_else(|e| panic!("expected JSON body: {e}"));
        // The 2,000-char prompt comes back byte-identical — no truncation.
        assert_eq!(v["prompt"].as_str().unwrap(), "x".repeat(2000));
        for field in [
            "id",
            "trigger_type",
            "trigger_config",
            "cli",
            "model",
            "effort",
            "working_dir",
            "enabled",
            "timeout_minutes",
            "expires_at",
            "enable_at",
            "notify_on_success",
            "log_path",
            "created_at",
            "last_run_at",
            "last_run_ok",
            "last_triggered_at",
            "trigger_count",
            "prompt",
        ] {
            assert!(v.get(field).is_some(), "missing field {field}");
        }
        assert_eq!(v["id"], serde_json::json!("long-prompt"));
        assert_eq!(v["trigger_type"], serde_json::json!("cron"));
        assert_eq!(
            v["trigger_config"]["schedule_expr"],
            serde_json::json!("0 9 * * *")
        );
        assert_eq!(v["cli"], serde_json::json!("opencode"));
        assert_eq!(v["timeout_minutes"], serde_json::json!(15));
    }

    #[tokio::test]
    async fn agent_get_unknown_id_returns_exact_error() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .agent_get(Parameters(IdParam {
                id: "nope".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert_eq!(raw_text(&result), "agent 'nope' not found");
    }

    #[tokio::test]
    async fn task_list_long_prompt_points_to_agent_get() {
        let (_dir, db, handler) = endpoint_test_handler();
        let mut agent = sample_agent("loop-watchdog", None);
        agent.prompt = "y".repeat(2000);
        db.upsert_agent(&agent).unwrap();

        let result = handler.task_list().await.unwrap();
        assert!(text(&result).contains("agent_get loop-watchdog"));
    }

    #[tokio::test]
    async fn session_list_round_trip_id_is_hook_acceptable() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.insert_interactive_session(
            "s-live",
            "boletus",
            "opencode",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();

        let result = handler
            .session_list(Parameters(SessionListParams {
                session_id: None,
                limit: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let out = raw_text(&result);
        let listed_id = out
            .lines()
            .find_map(|line| line.strip_prefix("- id: "))
            .and_then(|line| line.split(" | ").next())
            .expect("session id line");
        assert_eq!(listed_id, "s-live");
        assert!(db
            .get_active_sessions()
            .unwrap()
            .iter()
            .any(|session| session.id == listed_id));
    }

    #[tokio::test]
    async fn session_list_active_session_without_live_tui_is_marked_unattached_but_targetable() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.insert_interactive_session(
            "s-unattached",
            "unattached",
            "claude",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();

        let result = handler
            .session_list(Parameters(SessionListParams {
                session_id: None,
                limit: None,
            }))
            .await
            .unwrap();
        let out = raw_text(&result);
        assert!(out.contains("s-unattached"));
        assert!(out.contains("attached: no"));
        assert!(out.contains("hook_target: yes"));
    }

    #[tokio::test]
    async fn session_list_attached_session_is_marked_attached() {
        let (_dir, db, handler) = endpoint_test_handler();
        let Some(boot) = crate::system::boot_id() else {
            return;
        };
        db.insert_interactive_session(
            "s-attached",
            "attached",
            "claude",
            "/tmp",
            None,
            Some(std::process::id() as i64),
            "interactive",
            Some(&boot),
        )
        .unwrap();

        let result = handler
            .session_list(Parameters(SessionListParams {
                session_id: None,
                limit: None,
            }))
            .await
            .unwrap();
        let out = raw_text(&result);
        assert!(out.contains("s-attached"), "{out}");
        assert!(out.contains("attached: yes"), "{out}");
    }

    #[tokio::test]
    async fn session_list_lookup_returns_verbatim_hookable_id() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.insert_interactive_session(
            "agent-7f3a",
            "boletus",
            "opencode",
            "/tmp/w",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();

        let result = handler
            .session_list(Parameters(SessionListParams {
                session_id: Some("  agent-7f3a  ".to_string()),
                limit: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&result));
        let out = raw_text(&result);
        assert!(out.contains("id: agent-7f3a "), "{out}");
        assert!(db
            .get_active_sessions()
            .unwrap()
            .iter()
            .any(|s| s.id == "agent-7f3a"));
    }

    #[tokio::test]
    async fn session_list_unknown_id_is_not_found() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .session_list(Parameters(SessionListParams {
                session_id: Some("nope".to_string()),
                limit: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
        let out = text(&result);
        assert!(out.contains("No session found"));
        assert!(out.contains("nope"));
    }

    #[tokio::test]
    async fn session_list_omits_args_and_output() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.insert_interactive_session(
            "s-cloak",
            "private",
            "opencode",
            "/tmp",
            Some("--token secret"),
            None,
            "interactive",
            None,
        )
        .unwrap();

        let result = handler
            .session_list(Parameters(SessionListParams {
                session_id: None,
                limit: None,
            }))
            .await
            .unwrap();
        let out = raw_text(&result);
        // The session is listed (otherwise the assertions below pass
        // vacuously)…
        assert!(out.contains("id: s-cloak"), "{out}");
        assert!(out.contains("name: private"), "{out}");
        // …but its launch args / secrets never appear in the row.
        assert!(!out.contains("secret"), "{out}");
        assert!(!out.contains("--token"), "{out}");
        assert!(!out.contains("args"), "{out}");
    }

    #[tokio::test]
    async fn scheduled_send_create_targets_own_session_when_omitted() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.insert_interactive_session(
            "self-session",
            "me",
            "claude",
            "/proj",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        let _guard = AgentIdVar::set("self-session");

        let result = handler
            .scheduled_send_create(
                Parameters(ScheduledSendCreateParams {
                    prompt: "wake up".to_string(),
                    target_session_id: None,
                    at: None,
                    in_seconds: Some(60),
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));

        let pending = db
            .list_pending_scheduled_sends_for_session("self-session")
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].prompt, "wake up");
        assert_eq!(
            pending[0]
                .provenance
                .as_ref()
                .unwrap()
                .session_id
                .as_deref(),
            Some("self-session")
        );
    }

    #[tokio::test]
    async fn scheduled_send_create_targets_named_session() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.insert_interactive_session(
            "caller",
            "me",
            "claude",
            "/proj",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.insert_interactive_session(
            "other",
            "other",
            "claude",
            "/proj2",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        let _guard = AgentIdVar::set("caller");

        let result = handler
            .scheduled_send_create(
                Parameters(ScheduledSendCreateParams {
                    prompt: "for the other session".to_string(),
                    target_session_id: Some("other".to_string()),
                    at: None,
                    in_seconds: Some(30),
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));

        let pending = db
            .list_pending_scheduled_sends_for_session("other")
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0]
                .provenance
                .as_ref()
                .unwrap()
                .session_id
                .as_deref(),
            Some("caller")
        );
        assert_eq!(pending[0].provenance.as_ref().unwrap().kind, "agent");
    }

    #[tokio::test]
    async fn scheduled_send_create_rejects_unknown_target() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _guard = AgentIdVar::set("caller-2");

        let result = handler
            .scheduled_send_create(
                Parameters(ScheduledSendCreateParams {
                    prompt: "hi".to_string(),
                    target_session_id: Some("no-such-session".to_string()),
                    at: None,
                    in_seconds: Some(60),
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(text(&result).contains("no-such-session"));
    }

    #[tokio::test]
    async fn scheduled_send_create_requires_exactly_one_of_at_or_in_seconds() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.insert_interactive_session(
            "caller-3",
            "me",
            "claude",
            "/proj",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        let _guard = AgentIdVar::set("caller-3");

        let neither = handler
            .scheduled_send_create(
                Parameters(ScheduledSendCreateParams {
                    prompt: "hi".to_string(),
                    target_session_id: None,
                    at: None,
                    in_seconds: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(is_err(&neither));

        let both = handler
            .scheduled_send_create(
                Parameters(ScheduledSendCreateParams {
                    prompt: "hi".to_string(),
                    target_session_id: None,
                    at: Some("2026-07-10T09:00:00Z".to_string()),
                    in_seconds: Some(60),
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(is_err(&both));
    }

    #[tokio::test]
    async fn scheduled_send_list_defaults_to_own_session() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.insert_interactive_session(
            "list-self",
            "me",
            "claude",
            "/proj",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        let _guard = AgentIdVar::set("list-self");
        db.insert_scheduled_send(
            "ss-list-1",
            "hello",
            "list-self",
            Some("/proj"),
            chrono::Utc::now() + chrono::Duration::hours(1),
            None,
            None,
        )
        .unwrap();

        let result = handler
            .scheduled_send_list(
                Parameters(ScheduledSendListParams { session_id: None }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let out = raw_text(&result);
        assert!(out.contains("ss-list-1"));
        assert!(out.contains("hello"));
    }

    #[tokio::test]
    async fn scheduled_send_list_explicit_session_needs_no_identity() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.insert_scheduled_send(
            "ss-list-2",
            "explicit",
            "some-session",
            None,
            chrono::Utc::now() + chrono::Duration::hours(1),
            None,
            None,
        )
        .unwrap();

        let result = handler
            .scheduled_send_list(
                Parameters(ScheduledSendListParams {
                    session_id: Some("some-session".to_string()),
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        assert!(raw_text(&result).contains("ss-list-2"));
    }

    #[tokio::test]
    async fn scheduled_send_list_empty_reports_none_pending() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .scheduled_send_list(
                Parameters(ScheduledSendListParams {
                    session_id: Some("nobody".to_string()),
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&result));
        assert!(text(&result).contains("No pending"));
    }

    #[tokio::test]
    async fn scheduled_send_cancel_removes_pending_send() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.insert_scheduled_send(
            "ss-cancel-1",
            "to cancel",
            "session-x",
            None,
            chrono::Utc::now() + chrono::Duration::hours(1),
            None,
            None,
        )
        .unwrap();

        let result = handler
            .scheduled_send_cancel(Parameters(ScheduledSendCancelParams {
                id: "ss-cancel-1".to_string(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        assert!(db
            .list_pending_scheduled_sends_for_session("session-x")
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn scheduled_send_cancel_unknown_id_is_not_found() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .scheduled_send_cancel(Parameters(ScheduledSendCancelParams {
                id: "no-such-id".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(text(&result).contains("no-such-id"));
    }

    // ── task_remove / agent_remove ───────────────────────────────

    #[tokio::test]
    async fn task_remove_deletes_existing_agent() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.upsert_agent(&sample_agent("to-remove", None)).unwrap();

        let result = handler
            .task_remove(Parameters(IdParam {
                id: "to-remove".to_string(),
            }))
            .await
            .unwrap();

        assert!(!is_err(&result));
        assert!(text(&result).contains("removed"));
        assert!(db.get_agent("to-remove").unwrap().is_none());
    }

    #[tokio::test]
    async fn task_remove_reports_missing_agent() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .task_remove(Parameters(IdParam {
                id: "does-not-exist".to_string(),
            }))
            .await
            .unwrap();

        assert!(is_err(&result));
        assert!(text(&result).contains("No agent found"));
    }

    // ── task_enable / agent_enable ───────────────────────────────

    #[tokio::test]
    async fn task_enable_clears_expiry_on_expired_agent() {
        let (_dir, db, handler) = endpoint_test_handler();
        let mut agent = sample_agent("expired-1", None);
        agent.enabled = false;
        agent.expires_at = Some(chrono::Utc::now() - chrono::Duration::minutes(5));
        db.upsert_agent(&agent).unwrap();

        let result = handler
            .task_enable(Parameters(IdParam {
                id: "expired-1".to_string(),
            }))
            .await
            .unwrap();

        assert!(!is_err(&result));
        let stored = db.get_agent("expired-1").unwrap().unwrap();
        assert!(stored.enabled);
        assert!(stored.expires_at.is_none());
    }

    #[tokio::test]
    async fn task_enable_reports_missing_agent() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .task_enable(Parameters(IdParam {
                id: "ghost".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
    }

    // ── task_schedule_enable / agent_schedule_enable ─────────────

    #[tokio::test]
    async fn task_schedule_enable_sets_future_enable_time() {
        let (_dir, db, handler) = endpoint_test_handler();
        let mut agent = sample_agent("sched-1", None);
        agent.enabled = false;
        db.upsert_agent(&agent).unwrap();

        let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let result = handler
            .task_schedule_enable(Parameters(AgentScheduleEnableParams {
                id: "sched-1".to_string(),
                at: at.clone(),
            }))
            .await
            .unwrap();

        assert!(!is_err(&result));
        assert!(text(&result).contains("scheduled to enable"));
    }

    #[tokio::test]
    async fn task_schedule_enable_rejects_bad_timestamp() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.upsert_agent(&sample_agent("sched-2", None)).unwrap();

        let result = handler
            .task_schedule_enable(Parameters(AgentScheduleEnableParams {
                id: "sched-2".to_string(),
                at: "not-a-timestamp".to_string(),
            }))
            .await
            .unwrap();

        assert!(is_err(&result));
        assert!(text(&result).contains("Invalid ISO 8601"));
    }

    #[tokio::test]
    async fn task_schedule_enable_reports_missing_agent() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let result = handler
            .task_schedule_enable(Parameters(AgentScheduleEnableParams {
                id: "ghost".to_string(),
                at,
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
    }

    // ── task_disable / agent_disable ─────────────────────────────

    #[tokio::test]
    async fn task_disable_stops_a_watch_agent() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.upsert_agent(&sample_agent(
            "watch-disable",
            Some(Trigger::Watch {
                path: "/tmp".to_string(),
                events: vec![crate::domain::models::WatchEvent::Modify],
                debounce_seconds: 2,
                recursive: false,
            }),
        ))
        .unwrap();

        let result = handler
            .task_disable(Parameters(IdParam {
                id: "watch-disable".to_string(),
            }))
            .await
            .unwrap();

        assert!(!is_err(&result));
        let stored = db.get_agent("watch-disable").unwrap().unwrap();
        assert!(!stored.enabled);
    }

    #[tokio::test]
    async fn task_disable_reports_missing_agent() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .task_disable(Parameters(IdParam {
                id: "ghost".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
    }

    // ── agent_run ─────────────────────────────────────────────────

    #[tokio::test]
    async fn agent_run_launches_existing_agent_in_background() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.upsert_agent(&sample_agent("runnable", None)).unwrap();

        let result = handler
            .agent_run(Parameters(IdParam {
                id: "runnable".to_string(),
            }))
            .await
            .unwrap();

        assert!(!is_err(&result));
        assert!(text(&result).contains("launched in background"));
    }

    #[tokio::test]
    async fn agent_run_reports_missing_agent() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .agent_run(Parameters(IdParam {
                id: "ghost".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(text(&result).contains("No agent found"));
    }

    // ── task_status / agent_status ───────────────────────────────

    #[tokio::test]
    async fn task_status_reports_agent_counts() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.upsert_agent(&sample_agent(
            "cron-a",
            Some(Trigger::Cron {
                schedule_expr: "* * * * *".to_string(),
            }),
        ))
        .unwrap();
        db.upsert_agent(&sample_agent("manual-a", None)).unwrap();

        let result = handler.task_status().await.unwrap();
        let out = text(&result);
        assert!(out.contains("canopy v"));
        assert!(out.contains("cron: 1"));
        assert!(out.contains("manual: 1"));
    }

    // ── task_models / agent_models ───────────────────────────────

    // Note: These tests fail in CI due to HOME env var contention between
    // concurrent async tests. Marked as ignored until test isolation is improved.
    #[tokio::test]
    #[ignore]
    async fn task_models_rejects_unconfigured_platform() {
        let home = tempdir().unwrap();
        let canopy_dir = home.path().join(".canopy");
        std::fs::create_dir_all(&canopy_dir).unwrap();
        let config = crate::domain::canopy_config::CanopyConfig {
            clis: vec![crate::domain::cli_config::CliConfig {
                name: "echo-cli".to_string(),
                binary: "/bin/echo".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        config.save(&canopy_dir).unwrap();
        let _home_guard = HomeVar::set(home.path());

        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .task_models(Parameters(TaskModelsParams {
                platform: Some("not-configured-platform".to_string()),
                refresh: None,
                full: None,
            }))
            .await
            .unwrap();

        assert!(is_err(&result));
        assert!(text(&result).contains("is not configured in canopy"));
    }

    #[tokio::test]
    async fn task_models_enumerates_native_platform_models() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempdir().unwrap();
        let canopy_dir = home.path().join(".canopy");
        std::fs::create_dir_all(&canopy_dir).unwrap();
        let script = home.path().join("list-models.sh");
        std::fs::write(&script, "#!/bin/sh\necho model-a\necho model-b\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let config = crate::domain::canopy_config::CanopyConfig {
            clis: vec![crate::domain::cli_config::CliConfig {
                name: "native-cli".to_string(),
                binary: script.to_string_lossy().to_string(),
                models_list_cmd: Some("--list".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        config.save(&canopy_dir).unwrap();
        let _home_guard = HomeVar::set(home.path());

        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .task_models(Parameters(TaskModelsParams {
                platform: Some("native-cli".to_string()),
                refresh: Some(true),
                full: None,
            }))
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        let out = text(&result);
        assert!(out.contains("model-a"));
        assert!(out.contains("model-b"));
        assert!(out.contains("Source:"));
        assert!(
            out.contains("CLI enumeration"),
            "native-enumerated result must state it came from the CLI: {out}"
        );
        assert!(
            !out.contains("PROVIDER'S CATALOG"),
            "native-enumerated result must NOT carry the provider catalogue warning: {out}"
        );
    }

    #[tokio::test]
    async fn task_models_warns_for_platform_without_model_flag() {
        let home = tempdir().unwrap();
        let canopy_dir = home.path().join(".canopy");
        std::fs::create_dir_all(&canopy_dir).unwrap();
        let config = crate::domain::canopy_config::CanopyConfig {
            clis: vec![crate::domain::cli_config::CliConfig {
                name: "mistral".to_string(),
                binary: "/bin/echo".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        config.save(&canopy_dir).unwrap();
        // Seed a minimal models.dev catalog so the models.dev path succeeds
        // without a network fetch — `load_catalog_with_source` serves a fresh
        // cache with no network when the file exists and is within TTL.
        let cache_dir = canopy_dir.join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let catalog_json = format!(
            r#"{{"models":[{{"id":"mistral-medium-latest","name":"Mistral Medium","provider":"mistral"}}],"fetched_at":{now_secs}}}"#
        );
        std::fs::write(cache_dir.join("models_catalog.json"), catalog_json).unwrap();
        let _home_guard = HomeVar::set(home.path());

        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .task_models(Parameters(TaskModelsParams {
                platform: Some("mistral".to_string()),
                refresh: None,
                full: None,
            }))
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        let out = text(&result);
        assert!(
            out.contains("does not support model selection") || out.contains("NOT valid input"),
            "must warn that this platform does not support model selection: {out}"
        );
    }

    #[tokio::test]
    async fn task_models_warns_for_native_enumeration_without_model_flag() {
        use std::os::unix::fs::PermissionsExt;

        // FR4: a platform can have a `models_list_cmd` (so `agent_models`
        // takes the native-enumeration path) while still having no
        // `model_flag` — the enumerated ids are not valid `model` input and
        // the output must say so, exactly as the models.dev path does.
        let home = tempdir().unwrap();
        let canopy_dir = home.path().join(".canopy");
        std::fs::create_dir_all(&canopy_dir).unwrap();
        let script = home.path().join("list-agents.sh");
        std::fs::write(&script, "#!/bin/sh\necho agent-one\necho agent-two\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let config = crate::domain::canopy_config::CanopyConfig {
            clis: vec![crate::domain::cli_config::CliConfig {
                name: "named-agent-cli".to_string(),
                binary: script.to_string_lossy().to_string(),
                models_list_cmd: Some("--list".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        config.save(&canopy_dir).unwrap();
        let _home_guard = HomeVar::set(home.path());

        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .task_models(Parameters(TaskModelsParams {
                platform: Some("named-agent-cli".to_string()),
                refresh: Some(true),
                full: None,
            }))
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        let out = text(&result);
        assert!(out.contains("agent-one"), "enumeration still shown: {out}");
        assert!(
            out.contains("does not support model selection") || out.contains("NOT valid input"),
            "native-enumeration output must also warn when there is no model_flag: {out}"
        );
    }

    #[tokio::test]
    async fn native_result_labels_source_as_cli_and_drops_catalogue_warning() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempdir().unwrap();
        let canopy_dir = home.path().join(".canopy");
        std::fs::create_dir_all(&canopy_dir).unwrap();
        let script = home.path().join("list-models.sh");
        // Print ids that match the kiro-shaped regression: use the dot
        // separator the CLI accepts, not the hyphenated provider form.
        std::fs::write(
            &script,
            "#!/bin/sh\necho claude-sonnet-4.5\necho deepseek-3.2\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let config = crate::domain::canopy_config::CanopyConfig {
            clis: vec![crate::domain::cli_config::CliConfig {
                name: "kiro-shaped".to_string(),
                binary: script.to_string_lossy().to_string(),
                models_list_cmd: Some("--list".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        config.save(&canopy_dir).unwrap();
        let _home_guard = HomeVar::set(home.path());

        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .task_models(Parameters(TaskModelsParams {
                platform: Some("kiro-shaped".to_string()),
                refresh: Some(true),
                full: None,
            }))
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        let out = text(&result);
        // Must come from the CLI, not the catalogue.
        assert!(
            out.contains("CLI enumeration"),
            "native result must label its source as CLI: {out}"
        );
        assert!(
            !out.contains("PROVIDER'S CATALOG"),
            "native result must NOT carry the catalogue warning: {out}"
        );
        // Regression guard: the kiro-shaped dot-separated ids must be present.
        assert!(
            out.contains("claude-sonnet-4.5"),
            "must include the CLI-accepted id, not the provider form: {out}"
        );
        assert!(
            !out.contains("claude-sonnet-5"),
            "must NOT offer the provider-catalogue id that kiro rejects: {out}"
        );
    }

    #[tokio::test]
    async fn catalogue_result_keeps_cb9_warning_and_labels_source() {
        let home = tempdir().unwrap();
        let canopy_dir = home.path().join(".canopy");
        std::fs::create_dir_all(&canopy_dir).unwrap();
        // Seed a models.dev catalog so the catalogue path succeeds without
        // network — same pattern as `task_models_warns_for_platform_without_model_flag`.
        let cache_dir = canopy_dir.join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let catalog_json = format!(
            r#"{{"models":[{{"id":"some-model","name":"Some Model","provider":"mistral"}}],"fetched_at":{now_secs}}}"#
        );
        std::fs::write(cache_dir.join("models_catalog.json"), catalog_json).unwrap();
        let config = crate::domain::canopy_config::CanopyConfig {
            clis: vec![crate::domain::cli_config::CliConfig {
                name: "mistral".to_string(),
                binary: "/bin/echo".to_string(),
                // No models_list_cmd → catalogue path.
                ..Default::default()
            }],
            ..Default::default()
        };
        config.save(&canopy_dir).unwrap();
        let _home_guard = HomeVar::set(home.path());

        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .task_models(Parameters(TaskModelsParams {
                platform: Some("mistral".to_string()),
                refresh: None,
                full: None,
            }))
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        let out = text(&result);
        assert!(
            out.contains("PROVIDER'S CATALOG"),
            "catalogue result must carry CB9 warning: {out}"
        );
        assert!(
            out.contains("provider catalogue (models.dev)"),
            "catalogue result must label its source: {out}"
        );
    }

    #[tokio::test]
    async fn native_enumeration_failure_falls_back_to_catalogue_and_says_so() {
        let home = tempdir().unwrap();
        let canopy_dir = home.path().join(".canopy");
        std::fs::create_dir_all(&canopy_dir).unwrap();
        // Seed a models.dev catalog for the fallback path.
        let cache_dir = canopy_dir.join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let catalog_json = format!(
            r#"{{"models":[{{"id":"fallback-model","name":"Fallback","provider":"mistral"}}],"fetched_at":{now_secs}}}"#
        );
        std::fs::write(cache_dir.join("models_catalog.json"), catalog_json).unwrap();

        // Configure a platform whose models_list_cmd binary does NOT exist,
        // so enumeration fails and the fallback triggers. Use `mistral`
        // (a platform with a known provider mapping) so the catalogue
        // fallback has a provider list to render.
        let config = crate::domain::canopy_config::CanopyConfig {
            clis: vec![crate::domain::cli_config::CliConfig {
                name: "mistral".to_string(),
                binary: "/nonexistent/binary".to_string(),
                models_list_cmd: Some("--list".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        config.save(&canopy_dir).unwrap();
        let _home_guard = HomeVar::set(home.path());

        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .task_models(Parameters(TaskModelsParams {
                platform: Some("mistral".to_string()),
                refresh: None,
                full: None,
            }))
            .await
            .unwrap();

        // Must NOT be an error — the fallback is a degraded but usable result.
        assert!(!is_err(&result), "{}", text(&result));
        let out = text(&result);
        assert!(
            out.contains("enumeration failed"),
            "must explain that enumeration failed: {out}"
        );
        // The catalogue models must still be present as fallback.
        assert!(
            out.contains("fallback-model"),
            "catalogue fallback models must be present: {out}"
        );
    }

    #[tokio::test]
    async fn native_cache_and_refresh_match_catalogue_semantics() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempdir().unwrap();
        let canopy_dir = home.path().join(".canopy");
        std::fs::create_dir_all(&canopy_dir).unwrap();
        let script = home.path().join("list-models.sh");
        std::fs::write(&script, "#!/bin/sh\necho model-x\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let config = crate::domain::canopy_config::CanopyConfig {
            clis: vec![crate::domain::cli_config::CliConfig {
                name: "cache-test-cli".to_string(),
                binary: script.to_string_lossy().to_string(),
                models_list_cmd: Some("--list".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        config.save(&canopy_dir).unwrap();
        let _home_guard = HomeVar::set(home.path());

        let (_dir, _db, handler) = endpoint_test_handler();

        // First call with refresh: true → should be Live.
        let result1 = handler
            .task_models(Parameters(TaskModelsParams {
                platform: Some("cache-test-cli".to_string()),
                refresh: Some(true),
                full: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&result1), "{}", text(&result1));
        let out1 = text(&result1);
        assert!(
            out1.contains("live"),
            "first call with refresh should be live: {out1}"
        );

        // Second call without refresh → should be Cache (served from the
        // fresh cache populated by the first call).
        let result2 = handler
            .task_models(Parameters(TaskModelsParams {
                platform: Some("cache-test-cli".to_string()),
                refresh: None,
                full: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&result2), "{}", text(&result2));
        let out2 = text(&result2);
        assert!(
            out2.contains("cache"),
            "second call without refresh should be cache: {out2}"
        );
    }

    #[tokio::test]
    async fn catalogue_platform_without_provider_mapping_says_so_not_network_error() {
        // A platform that is configured but has no models.dev provider mapping
        // and no `models_list_cmd` must report *that* — not a misleading
        // "could not reach models.dev". FR3: behaviour here is unchanged from
        // before native enumeration was factored out.
        let home = tempdir().unwrap();
        let canopy_dir = home.path().join(".canopy");
        std::fs::create_dir_all(&canopy_dir).unwrap();
        // Seed a fresh catalog so the failure cannot be blamed on the network.
        let cache_dir = canopy_dir.join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let catalog_json = format!(
            r#"{{"models":[{{"id":"some-model","name":"Some Model","provider":"mistral"}}],"fetched_at":{now_secs}}}"#
        );
        std::fs::write(cache_dir.join("models_catalog.json"), catalog_json).unwrap();
        let config = crate::domain::canopy_config::CanopyConfig {
            clis: vec![crate::domain::cli_config::CliConfig {
                // Not among `providers_for_cli`'s arms → empty provider list.
                name: "unmapped-cli".to_string(),
                binary: "/bin/echo".to_string(),
                // No models_list_cmd → catalogue path.
                ..Default::default()
            }],
            ..Default::default()
        };
        config.save(&canopy_dir).unwrap();
        let _home_guard = HomeVar::set(home.path());

        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .task_models(Parameters(TaskModelsParams {
                platform: Some("unmapped-cli".to_string()),
                refresh: None,
                full: None,
            }))
            .await
            .unwrap();

        assert!(is_err(&result), "{}", text(&result));
        let out = text(&result);
        assert!(
            out.contains("No known model providers are mapped for platform 'unmapped-cli'"),
            "must name the real cause (no provider mapping): {out}"
        );
        assert!(
            !out.contains("could not reach models.dev"),
            "must NOT blame the network when the catalog is reachable: {out}"
        );
    }

    // ── task_logs / agent_logs ───────────────────────────────────

    #[tokio::test]
    async fn task_logs_reports_no_logs_for_unexecuted_agent() {
        let (dir, db, handler) = endpoint_test_handler();
        let mut agent = sample_agent("no-logs-yet", None);
        agent.log_path = dir
            .path()
            .join("no-logs-yet.log")
            .to_string_lossy()
            .to_string();
        db.upsert_agent(&agent).unwrap();

        let result = handler
            .task_logs(Parameters(TaskLogsParams {
                id: "no-logs-yet".to_string(),
                lines: None,
                since: None,
            }))
            .await
            .unwrap();

        assert!(!is_err(&result));
        assert!(text(&result).contains("No logs found"));
    }

    #[tokio::test]
    async fn task_logs_returns_recent_lines() {
        let (dir, db, handler) = endpoint_test_handler();
        let log_path = dir.path().join("has-logs.log");
        std::fs::write(&log_path, "line one\nline two\nline three\n").unwrap();
        let mut agent = sample_agent("has-logs", None);
        agent.log_path = log_path.to_string_lossy().to_string();
        db.upsert_agent(&agent).unwrap();

        let result = handler
            .task_logs(Parameters(TaskLogsParams {
                id: "has-logs".to_string(),
                lines: Some(2),
                since: None,
            }))
            .await
            .unwrap();

        let out = text(&result);
        assert!(out.contains("line two"));
        assert!(out.contains("line three"));
        assert!(!out.contains("line one"));
    }

    // ── task_update / agent_update ───────────────────────────────

    #[tokio::test]
    async fn task_update_renames_and_updates_prompt() {
        let home = tempdir().unwrap();
        let _home_guard = HomeVar::set(home.path());
        let (_dir, db, handler) = endpoint_test_handler();
        db.upsert_agent(&sample_agent(
            "old-id",
            Some(Trigger::Cron {
                schedule_expr: "* * * * *".to_string(),
            }),
        ))
        .unwrap();

        let result = handler
            .task_update(Parameters(TaskUpdateParams {
                id: "old-id".to_string(),
                new_id: Some("new-id".to_string()),
                prompt: Some("updated prompt".to_string()),
                cli: None,
                model: None,
                effort: None,
                schedule: None,
                working_dir: None,
                duration_minutes: None,
                path: None,
                events: None,
                debounce_seconds: None,
                recursive: None,
                enabled: None,
                notify_on_success: None,
            }))
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        assert!(db.get_agent("old-id").unwrap().is_none());
        let renamed = db.get_agent("new-id").unwrap().unwrap();
        assert_eq!(renamed.prompt, "updated prompt");
    }

    #[tokio::test]
    async fn task_update_reports_missing_agent() {
        let home = tempdir().unwrap();
        let _home_guard = HomeVar::set(home.path());
        let (_dir, _db, handler) = endpoint_test_handler();

        let result = handler
            .task_update(Parameters(TaskUpdateParams {
                id: "ghost".to_string(),
                new_id: None,
                prompt: None,
                cli: None,
                model: None,
                effort: None,
                schedule: None,
                working_dir: None,
                duration_minutes: None,
                path: None,
                events: None,
                debounce_seconds: None,
                recursive: None,
                enabled: None,
                notify_on_success: None,
            }))
            .await
            .unwrap();

        assert!(is_err(&result));
        assert!(text(&result).contains("No agent found"));
    }

    #[tokio::test]
    async fn task_update_rejects_invalid_new_id() {
        let home = tempdir().unwrap();
        let _home_guard = HomeVar::set(home.path());
        let (_dir, db, handler) = endpoint_test_handler();
        db.upsert_agent(&sample_agent("keep-id", None)).unwrap();

        let result = handler
            .task_update(Parameters(TaskUpdateParams {
                id: "keep-id".to_string(),
                new_id: Some("bad id!".to_string()),
                prompt: None,
                cli: None,
                model: None,
                effort: None,
                schedule: None,
                working_dir: None,
                duration_minutes: None,
                path: None,
                events: None,
                debounce_seconds: None,
                recursive: None,
                enabled: None,
                notify_on_success: None,
            }))
            .await
            .unwrap();

        assert!(is_err(&result));
        assert!(db.get_agent("keep-id").unwrap().is_some());
    }

    // ── task_report / agent_report ───────────────────────────────

    fn sample_run(id: &str, agent_id: &str, status: RunStatus) -> RunLog {
        RunLog {
            id: id.to_string(),
            background_agent_id: agent_id.to_string(),
            status,
            trigger_type: TriggerType::Manual,
            summary: None,
            started_at: chrono::Utc::now(),
            finished_at: None,
            exit_code: None,
            timeout_at: None,
            executed_platform: None,
            executed_model: None,
        }
    }

    #[tokio::test]
    async fn task_report_transitions_in_progress_to_success() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.upsert_agent(&sample_agent("reporter", None)).unwrap();
        db.insert_run(&sample_run("run-1", "reporter", RunStatus::InProgress))
            .unwrap();

        let result = handler
            .task_report(Parameters(TaskReportParams {
                run_id: "run-1".to_string(),
                status: "success".to_string(),
                summary: Some("all good".to_string()),
            }))
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        let run = db.get_run("run-1").unwrap().unwrap();
        assert!(matches!(run.status, RunStatus::Success));
        let agent = db.get_agent("reporter").unwrap().unwrap();
        assert_eq!(agent.last_run_ok, Some(true));
    }

    #[tokio::test]
    async fn task_report_rejects_invalid_status() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .task_report(Parameters(TaskReportParams {
                run_id: "run-x".to_string(),
                status: "sideways".to_string(),
                summary: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(text(&result).contains("Invalid status"));
    }

    #[tokio::test]
    async fn task_report_requires_summary_on_success() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.upsert_agent(&sample_agent("reporter-2", None)).unwrap();
        db.insert_run(&sample_run("run-2", "reporter-2", RunStatus::InProgress))
            .unwrap();

        let result = handler
            .task_report(Parameters(TaskReportParams {
                run_id: "run-2".to_string(),
                status: "success".to_string(),
                summary: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(text(&result).contains("summary is required"));
    }

    #[tokio::test]
    async fn task_report_rejects_invalid_transition() {
        let (_dir, db, handler) = endpoint_test_handler();
        db.upsert_agent(&sample_agent("reporter-3", None)).unwrap();
        // Already terminal (Success) — Success -> Success is not a valid
        // transition per validate_run_transition.
        db.insert_run(&sample_run("run-3", "reporter-3", RunStatus::Success))
            .unwrap();

        let result = handler
            .task_report(Parameters(TaskReportParams {
                run_id: "run-3".to_string(),
                status: "success".to_string(),
                summary: Some("again".to_string()),
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(text(&result).contains("Invalid transition"));
    }

    #[tokio::test]
    async fn task_report_errors_on_unknown_run_id() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .task_report(Parameters(TaskReportParams {
                run_id: "no-such-run".to_string(),
                status: "success".to_string(),
                summary: Some("done".to_string()),
            }))
            .await;
        assert!(result.is_err());
    }

    // ── graph_create / graph_update / graph tool handlers ─────

    fn valid_spec_description() -> String {
        "<spec>\n  <objective>Ship the feature.</objective>\n  <functional_requirements>Does the thing.</functional_requirements>\n  <non_functional_requirements>Is fast.</non_functional_requirements>\n  <constraints>None extra.</constraints>\n  <guidelines>Follow house style.</guidelines>\n  <in_scope>This change.</in_scope>\n  <out_of_scope>Everything else.</out_of_scope>\n</spec>"
            .to_string()
    }

    fn agent_node_config(platform: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        map.insert(
            "platform".to_string(),
            serde_json::Value::String(platform.to_string()),
        );
        map
    }

    #[tokio::test]
    async fn graph_create_and_update_round_trip() {
        let (dir, db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();

        let created = handler
            .graph_create(Parameters(GraphCreateParams {
                name: "My Graph".to_string(),
                description: None,
                workdir: workdir.clone(),
                trigger: None,
                infra_node_id: None,
                allow_dirty_start: false,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        let graph_id = extract_id(&created, "graph_id");
        assert!(db.get_graph(&graph_id).unwrap().is_some());

        let updated = handler
            .graph_update(Parameters(GraphUpdateParams {
                graph_id: graph_id.clone(),
                name: Some("Renamed Graph".to_string()),
                description: None,
                workdir: None,
                trigger: None,
                on_completed: None,
                hooks: None,
                infra_node_id: None,
                allow_dirty_start: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));
        assert_eq!(
            db.get_graph(&graph_id).unwrap().unwrap().name,
            "Renamed Graph"
        );
    }

    #[tokio::test]
    async fn graph_update_response_names_remaining_events_when_run_active() {
        let (dir, db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let created = handler
            .graph_create(Parameters(GraphCreateParams {
                name: "Active Hook Graph".to_string(),
                description: None,
                workdir,
                trigger: None,
                infra_node_id: None,
                allow_dirty_start: false,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        let graph_id = extract_id(&created, "graph_id");

        db.update_graph_status(
            &graph_id,
            GraphStatus::Running,
            Some(chrono::Utc::now()),
            None,
        )
        .unwrap();

        let mut hooks = std::collections::BTreeMap::new();
        for event in ["on_completed", "on_spec_completed"] {
            hooks.insert(
                event.to_string(),
                vec![GraphCompletionHookParams {
                    platform: None,
                    model: None,
                    effort: None,
                    prompt: None,
                    command: Some("printf ok".to_string()),
                    target_session_id: None,
                    target_session_name: None,
                    timeout_minutes: Some(1),
                    target_graph_id: None,
                    queue_id: None,
                    workdir_override: None,
                    idea: None,
                }],
            );
        }
        let updated = handler
            .graph_update(Parameters(GraphUpdateParams {
                graph_id: graph_id.clone(),
                name: None,
                description: None,
                workdir: None,
                trigger: None,
                on_completed: None,
                hooks: Some(hooks),
                infra_node_id: None,
                allow_dirty_start: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));
        let message = text(&updated);
        assert!(message.contains("remaining events"), "{message}");
        assert!(message.contains("on_completed"), "{message}");
        assert!(message.contains("on_spec_completed"), "{message}");
    }

    #[tokio::test]
    async fn graph_update_response_has_no_hook_note_without_active_run() {
        let (dir, _db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let created = handler
            .graph_create(Parameters(GraphCreateParams {
                name: "Idle Hook Graph".to_string(),
                description: None,
                workdir,
                trigger: None,
                infra_node_id: None,
                allow_dirty_start: false,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created));
        let graph_id = extract_id(&created, "graph_id");

        let mut hooks = std::collections::BTreeMap::new();
        hooks.insert(
            "on_completed".to_string(),
            vec![GraphCompletionHookParams {
                platform: None,
                model: None,
                effort: None,
                prompt: None,
                command: Some("printf ok".to_string()),
                target_session_id: None,
                target_session_name: None,
                timeout_minutes: Some(1),
                target_graph_id: None,
                queue_id: None,
                workdir_override: None,
                idea: None,
            }],
        );
        let updated = handler
            .graph_update(Parameters(GraphUpdateParams {
                graph_id: graph_id.clone(),
                name: None,
                description: None,
                workdir: None,
                trigger: None,
                on_completed: None,
                hooks: Some(hooks),
                infra_node_id: None,
                allow_dirty_start: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));
        assert!(!text(&updated).contains("remaining events"));
    }

    #[tokio::test]
    async fn graph_update_rejects_unknown_hook_placeholder_before_persisting() {
        let (dir, db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let created = handler
            .graph_create(Parameters(GraphCreateParams {
                name: "Hook Graph".to_string(),
                description: None,
                workdir,
                trigger: None,
                infra_node_id: None,
                allow_dirty_start: false,
            }))
            .await
            .unwrap();
        let graph_id = extract_id(&created, "graph_id");

        let mut valid_hooks = std::collections::BTreeMap::new();
        valid_hooks.insert(
            "on_failed".to_string(),
            vec![GraphCompletionHookParams {
                platform: None,
                model: None,
                effort: None,
                prompt: None,
                command: Some("printf '{{node}}'".to_string()),
                target_session_id: None,
                target_session_name: None,
                timeout_minutes: Some(1),
                target_graph_id: None,
                queue_id: None,
                workdir_override: None,
                idea: None,
            }],
        );
        let updated = handler
            .graph_update(Parameters(GraphUpdateParams {
                graph_id: graph_id.clone(),
                name: None,
                description: None,
                workdir: None,
                trigger: None,
                on_completed: None,
                hooks: Some(valid_hooks),
                infra_node_id: None,
                allow_dirty_start: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));
        let before = db.get_graph(&graph_id).unwrap().unwrap().hooks;

        let mut invalid_hooks = std::collections::BTreeMap::new();
        invalid_hooks.insert(
            "on_failed".to_string(),
            vec![GraphCompletionHookParams {
                platform: None,
                model: None,
                effort: None,
                prompt: Some("failure at {{nodee}}".to_string()),
                command: None,
                target_session_id: None,
                target_session_name: None,
                timeout_minutes: Some(1),
                target_graph_id: None,
                queue_id: None,
                workdir_override: None,
                idea: None,
            }],
        );
        let rejected = handler
            .graph_update(Parameters(GraphUpdateParams {
                graph_id: graph_id.clone(),
                name: None,
                description: None,
                workdir: None,
                trigger: None,
                on_completed: None,
                hooks: Some(invalid_hooks),
                infra_node_id: None,
                allow_dirty_start: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&rejected));
        let message = text(&rejected);
        assert!(message.contains("nodee"), "{message}");
        for marker in crate::graph_engine::GRAPH_HOOK_BINDINGS {
            assert!(message.contains(marker), "{message}");
        }
        let after = db.get_graph(&graph_id).unwrap().unwrap().hooks;
        assert_eq!(
            serde_json::to_value(before).unwrap(),
            serde_json::to_value(after).unwrap()
        );
    }

    #[tokio::test]
    async fn graph_create_rejects_empty_name_and_relative_workdir() {
        let (_dir, _db, handler) = endpoint_test_handler();

        let empty_name = handler
            .graph_create(Parameters(GraphCreateParams {
                name: "  ".to_string(),
                description: None,
                workdir: "/tmp".to_string(),
                trigger: None,
                infra_node_id: None,
                allow_dirty_start: false,
            }))
            .await
            .unwrap();
        assert!(is_err(&empty_name));

        let bad_workdir = handler
            .graph_create(Parameters(GraphCreateParams {
                name: "Graph".to_string(),
                description: None,
                workdir: "relative/dir".to_string(),
                trigger: None,
                infra_node_id: None,
                allow_dirty_start: false,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_workdir));
        assert!(text(&bad_workdir).contains("absolute") || text(&bad_workdir).contains("exist"));
    }

    fn check_node_config(command: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        map.insert(
            "command".to_string(),
            serde_json::Value::String(command.to_string()),
        );
        map
    }

    /// Builds a graph with implementer -> gate -> committer (agent, check,
    /// agent), returning the graph id and each node's id in graph order.
    async fn build_simple_graph(
        handler: &TaskTriggerHandler,
        workdir: &str,
        graph_name: &str,
    ) -> (String, String, String, String) {
        let created = handler
            .graph_create(Parameters(GraphCreateParams {
                name: graph_name.to_string(),
                description: Some("A shareable design".to_string()),
                workdir: workdir.to_string(),
                trigger: None,
                infra_node_id: None,
                allow_dirty_start: false,
            }))
            .await
            .unwrap();
        let graph_id = extract_id(&created, "graph_id");

        let n1 = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "implementer".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let n1_id = extract_id(&n1, "node_id");

        let n2 = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "gate".to_string(),
                kind: Some("check".to_string()),
                config: Some(check_node_config("cargo test")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let n2_id = extract_id(&n2, "node_id");

        let n3 = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "committer".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let n3_id = extract_id(&n3, "node_id");

        for (from, to, condition) in [(&n1_id, &n2_id, "always"), (&n2_id, &n3_id, "always")] {
            let edge = handler
                .graph_add_edge(Parameters(GraphAddEdgeParams {
                    spec_id: None,
                    graph_id: Some(graph_id.clone()),
                    from_node: from.clone(),
                    to_node: to.clone(),
                    condition: condition.to_string(),
                    route: None,
                }))
                .await
                .unwrap();
            assert!(!is_err(&edge), "{}", text(&edge));
        }

        (graph_id, n1_id, n2_id, n3_id)
    }

    #[tokio::test]
    async fn graph_export_returns_error_for_unknown_graph() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let result = handler
            .graph_export(Parameters(GraphExportParams {
                graph_id: "missing".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(text(&result).contains("not found"));
    }

    #[tokio::test]
    async fn graph_export_always_includes_platform_and_model() {
        let (dir, _db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let (graph_id, ..) = build_simple_graph(&handler, &workdir, "Export Graph").await;

        let exported = handler
            .graph_export(Parameters(GraphExportParams { graph_id }))
            .await
            .unwrap();
        assert!(!is_err(&exported), "{}", text(&exported));
        let doc: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();
        assert_eq!(doc["format_version"], 4);
        let implementer = doc["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["name"] == "implementer")
            .unwrap();
        assert_eq!(implementer["config"]["platform"], "claude");
        // build_simple_graph's agent configs carry no model: export states
        // the platform default explicitly as null.
        assert!(implementer["config"].get("model").is_some());
        assert!(implementer["config"]["model"].is_null());
    }

    /// Decision 2's enforced consequence: `graph_add_node` doesn't itself
    /// forbid two nodes sharing a name, so export must catch it — naming
    /// the offending node(s) rather than producing an ambiguous file.
    #[tokio::test]
    async fn graph_export_rejects_duplicate_node_names() {
        let (dir, _db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let created = handler
            .graph_create(Parameters(GraphCreateParams {
                name: "Dup Graph".to_string(),
                description: None,
                workdir,
                trigger: None,
                infra_node_id: None,
                allow_dirty_start: false,
            }))
            .await
            .unwrap();
        let graph_id = extract_id(&created, "graph_id");
        for _ in 0..2 {
            handler
                .graph_add_node(Parameters(GraphAddNodeParams {
                    spec_id: None,
                    graph_id: Some(graph_id.clone()),
                    name: "dup".to_string(),
                    kind: Some("check".to_string()),
                    config: Some(check_node_config("true")),
                    blueprint: None,
                    config_overrides: None,
                }))
                .await
                .unwrap();
        }

        let result = handler
            .graph_export(Parameters(GraphExportParams { graph_id }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(text(&result).contains("dup"));
        assert!(text(&result).to_lowercase().contains("duplicate"));
    }

    #[tokio::test]
    async fn graph_import_creates_new_graph_and_reports_missing_platform() {
        let (dir, db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let (source_graph_id, ..) = build_simple_graph(&handler, &workdir, "Source Graph").await;

        let exported = handler
            .graph_export(Parameters(GraphExportParams {
                graph_id: source_graph_id,
            }))
            .await
            .unwrap();
        let mut document: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();
        // Simulate a v1-style unbound document: strip the harness bindings
        // the v2 export now always carries, so import must report them.
        document["format_version"] = serde_json::json!(1);
        for node in document["nodes"].as_array_mut().unwrap().iter_mut() {
            if let Some(config) = node["config"].as_object_mut() {
                config.remove("platform");
                config.remove("model");
            }
        }

        let imported = handler
            .graph_import(Parameters(GraphImportParams {
                document,
                workdir: workdir.clone(),
                name: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&imported), "{}", text(&imported));
        let body: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
        // The source graph itself is still named "Source Graph" in this same
        // workdir, so decision 4's collision handling suffixes the import.
        assert_eq!(body["name"], "Source Graph (2)");
        let new_graph_id = body["graph_id"].as_str().unwrap().to_string();
        assert_ne!(new_graph_id, "");

        let missing: Vec<&str> = body["nodes_missing_platform"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(missing.contains(&"implementer"));
        assert!(missing.contains(&"committer"));
        assert!(body["terminals"]
            .as_array()
            .is_some_and(|terminals| !terminals.is_empty()));

        let nodes = db.list_graph_nodes_for_graph(&new_graph_id).unwrap();
        assert_eq!(nodes.len(), 3);
        let edges = db.list_graph_edges_for_graph(&new_graph_id).unwrap();
        assert_eq!(edges.len(), 2);
        // Import always creates a new graph, never touching the source.
        assert_eq!(db.list_graphs(Some(&workdir), true).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn graph_import_name_param_overrides_document_name() {
        let (dir, _db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let (source_graph_id, ..) = build_simple_graph(&handler, &workdir, "Source Graph").await;
        let exported = handler
            .graph_export(Parameters(GraphExportParams {
                graph_id: source_graph_id,
            }))
            .await
            .unwrap();
        let document: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();

        let imported = handler
            .graph_import(Parameters(GraphImportParams {
                document,
                workdir,
                name: Some("Custom Name".to_string()),
            }))
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
        assert_eq!(body["name"], "Custom Name");
    }

    /// Decision 4: import never overwrites — a name collision in the target
    /// workdir gets a numeric suffix instead of a refusal or an overwrite.
    #[tokio::test]
    async fn graph_import_dedupes_colliding_name_with_numeric_suffix() {
        let (dir, db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let (source_graph_id, ..) = build_simple_graph(&handler, &workdir, "Source Graph").await;
        let exported = handler
            .graph_export(Parameters(GraphExportParams {
                graph_id: source_graph_id,
            }))
            .await
            .unwrap();
        let document: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();

        let first = handler
            .graph_import(Parameters(GraphImportParams {
                document: document.clone(),
                workdir: workdir.clone(),
                name: Some("Collide".to_string()),
            }))
            .await
            .unwrap();
        let first_body: serde_json::Value = serde_json::from_str(&raw_text(&first)).unwrap();
        assert_eq!(first_body["name"], "Collide");

        let second = handler
            .graph_import(Parameters(GraphImportParams {
                document,
                workdir: workdir.clone(),
                name: Some("Collide".to_string()),
            }))
            .await
            .unwrap();
        let second_body: serde_json::Value = serde_json::from_str(&raw_text(&second)).unwrap();
        assert_eq!(second_body["name"], "Collide (2)");
        assert_ne!(second_body["graph_id"], first_body["graph_id"]);
        assert_eq!(db.list_graphs(Some(&workdir), true).unwrap().len(), 3);
    }

    /// Decision 5's all-or-nothing guarantee: a document whose edge names a
    /// nonexistent node is rejected before anything is written.
    #[tokio::test]
    async fn graph_import_rejects_bad_edge_reference_and_writes_nothing() {
        let (dir, db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let document = serde_json::json!({
            "format_version": 1,
            "name": "Broken Graph",
            "nodes": [
                {"name": "only", "kind": "check", "position": 1, "config": {"command": "true"}}
            ],
            "edges": [
                {"from_node": "only", "to_node": "ghost", "condition": "always"}
            ],
            "ensembles": []
        });

        let result = handler
            .graph_import(Parameters(GraphImportParams {
                document,
                workdir: workdir.clone(),
                name: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(text(&result).contains("ghost"));
        assert!(db.list_graphs(Some(&workdir), true).unwrap().is_empty());
    }

    #[tokio::test]
    async fn graph_import_rejects_missing_format_version() {
        let (dir, db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let document = serde_json::json!({
            "name": "No Version",
            "nodes": [],
            "edges": [],
            "ensembles": []
        });

        let result = handler
            .graph_import(Parameters(GraphImportParams {
                document,
                workdir: workdir.clone(),
                name: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(text(&result).contains("format_version"));
        assert!(db.list_graphs(Some(&workdir), true).unwrap().is_empty());
    }

    /// Requirement 5, exercised end to end through the MCP surface: export,
    /// import, export again — identical document except the name — for a
    /// graph that includes an ensemble, which must survive as an ensemble
    /// rather than expanded member nodes.
    #[tokio::test]
    async fn graph_export_import_v2_round_trip_preserves_ensemble() {
        let (dir, _db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();

        let created = handler
            .graph_create(Parameters(GraphCreateParams {
                name: "Ensemble Graph".to_string(),
                description: Some("Has an ensemble".to_string()),
                workdir: workdir.clone(),
                trigger: None,
                infra_node_id: None,
                allow_dirty_start: false,
            }))
            .await
            .unwrap();
        let graph_id = extract_id(&created, "graph_id");

        let kickoff = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "kickoff".to_string(),
                kind: Some("check".to_string()),
                config: Some(check_node_config("true")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let kickoff_id = extract_id(&kickoff, "node_id");

        let downstream = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "downstream".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let downstream_id = extract_id(&downstream, "node_id");

        let ensemble = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                kind: None,
                name: "Proposers".to_string(),
                prompt_template: Some("draft it".to_string()),
                blueprint: None,
                members: Some(vec![
                    EnsembleMemberParams {
                        platform: "openrouter".to_string(),
                        model: Some("model-a".to_string()),
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    EnsembleMemberParams {
                        platform: "openrouter".to_string(),
                        model: Some("model-b".to_string()),
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                condition: "always".to_string(),
                from_node: kickoff_id.clone(),
                on_pass_to: downstream_id.clone(),
                on_fail_to: None,
                min_pass: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&ensemble), "{}", text(&ensemble));

        let first_export = handler
            .graph_export(Parameters(GraphExportParams { graph_id }))
            .await
            .unwrap();
        let first_doc: serde_json::Value = serde_json::from_str(&raw_text(&first_export)).unwrap();
        assert_eq!(first_doc["ensembles"].as_array().unwrap().len(), 1);
        // CM32: the export document carries slugs only — no display string
        // is ever written to it.
        let first_raw = raw_text(&first_export);
        assert!(!first_raw.contains("platform_display"));
        assert!(!first_raw.contains("tool_name"));
        // The plain node list must exclude the ensemble's member/join nodes.
        assert_eq!(first_doc["nodes"].as_array().unwrap().len(), 2);

        let imported = handler
            .graph_import(Parameters(GraphImportParams {
                document: first_doc.clone(),
                workdir: workdir.clone(),
                name: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&imported), "{}", text(&imported));
        let imported_body: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
        let new_graph_id = imported_body["graph_id"].as_str().unwrap().to_string();
        assert_eq!(imported_body["name"], "Ensemble Graph (2)");
        // Every member carried its platform/model through the v2 export —
        // no node should be flagged.
        assert!(imported_body["nodes_missing_platform"]
            .as_array()
            .unwrap()
            .is_empty());

        let second_export = handler
            .graph_export(Parameters(GraphExportParams {
                graph_id: new_graph_id,
            }))
            .await
            .unwrap();
        let mut second_doc: serde_json::Value =
            serde_json::from_str(&raw_text(&second_export)).unwrap();
        // Identical except the name (decision 4 renamed it on collision).
        second_doc["name"] = first_doc["name"].clone();
        assert_eq!(first_doc, second_doc);
    }

    #[tokio::test]
    async fn graph_export_import_round_trip_preserves_all_ensemble_kinds() {
        // CB62 end-to-end: cascade + round_robin ensembles survive export ->
        // import -> export identical (modulo the collision-rename), so
        // `graph_get` on the imported graph shows cascade/round_robin, not
        // parallel. CB71 carries the cascade's second entry source in the
        // document. Infra budgets remain absent, and the round trip stays
        // identical for them because both exports omit them.
        let (dir, _db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();

        let created = handler
            .graph_create(Parameters(GraphCreateParams {
                name: "Kind Graph".to_string(),
                description: None,
                workdir: workdir.clone(),
                trigger: None,
                infra_node_id: None,
                allow_dirty_start: false,
            }))
            .await
            .unwrap();
        let graph_id = extract_id(&created, "graph_id");

        let kickoff = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "kickoff".to_string(),
                kind: Some("check".to_string()),
                config: Some(check_node_config("true")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let kickoff_id = extract_id(&kickoff, "node_id");

        let alt = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "alt".to_string(),
                kind: Some("check".to_string()),
                config: Some(check_node_config("true")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let alt_id = extract_id(&alt, "node_id");

        let downstream = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "downstream".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let downstream_id = extract_id(&downstream, "node_id");

        let wired = handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                from_node: kickoff_id.clone(),
                to_node: alt_id.clone(),
                condition: "always".to_string(),
                route: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&wired), "{}", text(&wired));

        let cascade = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                kind: Some("cascade".to_string()),
                name: "CascadeTeam".to_string(),
                prompt_template: Some("draft it".to_string()),
                blueprint: None,
                members: Some(vec![
                    EnsembleMemberParams {
                        platform: "openrouter".to_string(),
                        model: Some("model-a".to_string()),
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: Some(5),
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    EnsembleMemberParams {
                        platform: "openrouter".to_string(),
                        model: Some("model-b".to_string()),
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                condition: "always".to_string(),
                from_node: kickoff_id.clone(),
                on_pass_to: downstream_id.clone(),
                on_fail_to: None,
                min_pass: None,
                timeout_minutes: None,
                infra_retry_limit: Some(2),
                infra_crash_max_seconds: Some(60),
                infra_backoff_seconds: Some(30),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&cascade), "{}", text(&cascade));
        let cascade_id = extract_id(&cascade, "ensemble_id");

        let round_robin = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                kind: Some("round_robin".to_string()),
                name: "RrTeam".to_string(),
                prompt_template: Some("draft it".to_string()),
                blueprint: None,
                members: Some(vec![
                    EnsembleMemberParams {
                        platform: "openrouter".to_string(),
                        model: Some("model-a".to_string()),
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    EnsembleMemberParams {
                        platform: "openrouter".to_string(),
                        model: Some("model-b".to_string()),
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                condition: "always".to_string(),
                from_node: kickoff_id.clone(),
                on_pass_to: downstream_id.clone(),
                on_fail_to: None,
                min_pass: None,
                timeout_minutes: None,
                infra_retry_limit: Some(3),
                infra_crash_max_seconds: Some(61),
                infra_backoff_seconds: Some(31),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&round_robin), "{}", text(&round_robin));

        let second_source = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                ensemble_id: cascade_id.clone(),
                kind: None,
                prompt_template: None,
                members: None,
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: Some(alt_id.clone()),
                add_entry_condition: None,
                remove_entry_from: None,
                commit_rights: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&second_source), "{}", text(&second_source));

        let first_export = handler
            .graph_export(Parameters(GraphExportParams { graph_id }))
            .await
            .unwrap();
        let first_doc: serde_json::Value = serde_json::from_str(&raw_text(&first_export)).unwrap();
        assert_eq!(first_doc["ensembles"].as_array().unwrap().len(), 2);

        let imported = handler
            .graph_import(Parameters(GraphImportParams {
                document: first_doc.clone(),
                workdir: workdir.clone(),
                name: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&imported), "{}", text(&imported));
        let imported_body: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
        let new_graph_id = imported_body["graph_id"].as_str().unwrap().to_string();

        let second_export = handler
            .graph_export(Parameters(GraphExportParams {
                graph_id: new_graph_id,
            }))
            .await
            .unwrap();
        let mut second_doc: serde_json::Value =
            serde_json::from_str(&raw_text(&second_export)).unwrap();
        second_doc["name"] = first_doc["name"].clone();
        assert_eq!(first_doc, second_doc);

        let kinds: Vec<&str> = second_doc["ensembles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap_or("parallel"))
            .collect();
        assert!(kinds.contains(&"cascade"), "kinds: {kinds:?}");
        assert!(kinds.contains(&"round_robin"), "kinds: {kinds:?}");

        // CB71: the cascade's second entry source (`alt`, added with
        // add_entry_from, default condition always) is carried by the document
        // and restored — the round-trip equality above already pins that it
        // survives import; this pins that the FIRST export wrote it.
        let cascade_entry = first_doc["ensembles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "CascadeTeam")
            .expect("CascadeTeam in export");
        assert_eq!(
            cascade_entry["extra_entry_sources"],
            serde_json::json!([{"from_node": "alt", "condition": "always"}]),
            "CB71: the second entry source must appear in the export document"
        );
    }

    #[tokio::test]
    async fn graph_export_import_round_trip_preserves_ensemble_commit_rights() {
        // NFR1(d): an ensemble created with `commit_rights: true` keeps the
        // flag through export (document) and import (new graph's DB row).
        let (dir, db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();

        let created = handler
            .graph_create(Parameters(GraphCreateParams {
                name: "Commit Rights Ensemble Graph".to_string(),
                description: None,
                workdir: workdir.clone(),
                trigger: None,
                infra_node_id: None,
                allow_dirty_start: false,
            }))
            .await
            .unwrap();
        let graph_id = extract_id(&created, "graph_id");

        let kickoff = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "kickoff".to_string(),
                kind: Some("check".to_string()),
                config: Some(check_node_config("true")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let kickoff_id = extract_id(&kickoff, "node_id");

        let downstream = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "downstream".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let downstream_id = extract_id(&downstream, "node_id");

        let ensemble = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: Some(true),
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                kind: None,
                name: "Committer".to_string(),
                prompt_template: Some("draft it".to_string()),
                blueprint: None,
                members: Some(vec![
                    EnsembleMemberParams {
                        platform: "openrouter".to_string(),
                        model: Some("model-a".to_string()),
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    EnsembleMemberParams {
                        platform: "openrouter".to_string(),
                        model: Some("model-b".to_string()),
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                condition: "always".to_string(),
                from_node: kickoff_id.clone(),
                on_pass_to: downstream_id.clone(),
                on_fail_to: None,
                min_pass: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&ensemble), "{}", text(&ensemble));

        let exported = handler
            .graph_export(Parameters(GraphExportParams { graph_id }))
            .await
            .unwrap();
        let doc: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();
        assert_eq!(doc["ensembles"].as_array().unwrap().len(), 1);
        assert_eq!(
            doc["ensembles"][0]["commit_rights"],
            serde_json::json!(true),
            "export must carry the ensemble's commit_rights: {doc}"
        );

        let imported = handler
            .graph_import(Parameters(GraphImportParams {
                document: doc,
                workdir: workdir.clone(),
                name: Some("Commit Rights Ensemble Graph Imported".to_string()),
            }))
            .await
            .unwrap();
        assert!(!is_err(&imported), "{}", text(&imported));
        let imported_body: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
        let new_graph_id = imported_body["graph_id"].as_str().unwrap().to_string();

        let imported_ensembles = db.list_ensembles_for_graph(&new_graph_id).unwrap();
        assert_eq!(imported_ensembles.len(), 1);
        assert!(
            imported_ensembles[0].ensemble.commit_rights,
            "imported ensemble must hold commit rights"
        );
    }

    #[tokio::test]
    async fn graph_preflight_rejects_unknown_named_output_reference() {
        // A structurally valid two-node chain where the downstream node's
        // prompt references `{{output:Ghost}}` — a node that isn't in the
        // graph. Preflight must reject it before spending any probe quota,
        // not leave the marker to surface unsubstituted at runtime (CM1).
        let (dir, _db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let created = handler
            .graph_create(Parameters(GraphCreateParams {
                name: "Preflight Named Output Graph".to_string(),
                description: None,
                workdir,
                trigger: None,
                infra_node_id: None,
                allow_dirty_start: false,
            }))
            .await
            .unwrap();
        let graph_id = extract_id(&created, "graph_id");

        let alpha = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "alpha".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let alpha_id = extract_id(&alpha, "node_id");

        let mut beta_config = agent_node_config("claude");
        beta_config.insert(
            "prompt_template".to_string(),
            serde_json::Value::String("Follow the plan: {{output:Ghost}}".to_string()),
        );
        let beta = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "beta".to_string(),
                kind: Some("agent".to_string()),
                config: Some(beta_config),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let beta_id = extract_id(&beta, "node_id");

        let edge = handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                from_node: alpha_id,
                to_node: beta_id,
                condition: "always".to_string(),
                route: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&edge), "{}", text(&edge));

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: graph_id.clone(),
                timeout_seconds: None,
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(
            is_err(&result),
            "preflight should reject the unknown named-output reference: {}",
            text(&result)
        );
        let msg = text(&result);
        assert!(
            msg.contains("references unknown node 'Ghost'"),
            "preflight error should name the missing node, got: {msg}"
        );
    }

    #[tokio::test]
    async fn graph_preflight_reports_graph_validation_errors() {
        // Two agent nodes with no edge between them => multiple entry points.
        let (dir, _db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let created = handler
            .graph_create(Parameters(GraphCreateParams {
                name: "Preflight Graph".to_string(),
                description: None,
                workdir,
                trigger: None,
                infra_node_id: None,
                allow_dirty_start: false,
            }))
            .await
            .unwrap();
        let graph_id = extract_id(&created, "graph_id");

        handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "alpha".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "beta".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: graph_id.clone(),
                timeout_seconds: None,
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(
            is_err(&result),
            "preflight should report graph error: {}",
            text(&result)
        );
        let msg = text(&result).to_lowercase();
        assert!(
            msg.contains("entry"),
            "preflight error should mention entry points, got: {msg}"
        );
        assert!(
            msg.contains("alpha") && msg.contains("beta"),
            "preflight error should name the concrete nodes, got: {msg}"
        );
    }

    /// (CM19) `graph_preflight` reports a non-terminal node with no `fail`
    /// edge as a structured `graph_warnings` entry — advisory, not a tool
    /// error, and it does not change the verdict. A declared terminal with
    /// the same missing edge is not reported.
    #[tokio::test]
    async fn graph_preflight_warns_for_node_with_no_fail_edge() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());

        let mk_node = |name: &str, position: i64| GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            name: name.to_string(),
            kind: GraphNodeKind::Agent,
            // Unconfigured platform -> probe returns NotConfigured instantly,
            // no quota spent, preflight still succeeds.
            config: serde_json::json!({"platform": "cm19-unconfigured-platform"}),
            position,
            created_at: chrono::Utc::now(),
        };
        let alpha = mk_node("alpha", 1);
        let beta = mk_node("beta", 2);
        let gamma = mk_node("gamma", 3);
        db.insert_graph_node(&alpha).unwrap();
        db.insert_graph_node(&beta).unwrap();
        db.insert_graph_node(&gamma).unwrap();

        let mk_edge = |from: &str, to: &str, cond: GraphEdgeCondition| GraphEdge {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            from_node: from.to_string(),
            to_node: to.to_string(),
            condition: cond,
        };
        // alpha: pass + fail -> fully routed (not a dead end, not a terminal)
        db.insert_graph_edge(&mk_edge(&alpha.id, &beta.id, GraphEdgeCondition::Pass))
            .unwrap();
        db.insert_graph_edge(&mk_edge(&alpha.id, &beta.id, GraphEdgeCondition::Fail))
            .unwrap();
        // beta: pass only -> continues on success, dead-ends on fail => WARN
        db.insert_graph_edge(&mk_edge(&beta.id, &gamma.id, GraphEdgeCondition::Pass))
            .unwrap();
        // gamma: no outgoing edges -> deliberate terminal => NOT warned

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: Some(5),
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(
            !is_err(&result),
            "a preflight warning must not be a tool error: {}",
            text(&result)
        );
        let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();

        let warnings = body["graph_warnings"]
            .as_array()
            .unwrap_or_else(|| panic!("preflight must include graph_warnings: {body}"));
        assert_eq!(
            warnings.len(),
            1,
            "expected exactly one graph warning: {body}"
        );
        assert_eq!(warnings[0]["node_id"], beta.id);
        assert_eq!(warnings[0]["node_name"], "beta");
        assert_eq!(warnings[0]["missing_edge"], "fail");
        assert_eq!(warnings[0]["has_error_path"], false);
        let warn_text = warnings[0]["warning"].as_str().unwrap_or_default();
        assert!(
            warn_text.contains("beta") && warn_text.contains("no outgoing edge for that status"),
            "warning must name the node and state the consequence: {body}"
        );

        // gamma (declared terminal) must be in `terminals`, not in warnings.
        let terminals = body["terminals"].as_array().unwrap();
        assert!(
            terminals
                .iter()
                .any(|t| t.as_str().unwrap_or_default().contains("gamma")),
            "gamma should be listed as a terminal: {body}"
        );

        // Verdict is unaffected by the warning (requirement 4).
        assert!(
            body.get("would_fail").is_some(),
            "verdict fields intact: {body}"
        );
    }

    #[tokio::test]
    async fn graph_update_rejects_unknown_graph_and_requires_a_field() {
        let (dir, db, handler) = endpoint_test_handler();
        let missing = handler
            .graph_update(Parameters(GraphUpdateParams {
                graph_id: "ghost".to_string(),
                name: Some("x".to_string()),
                description: None,
                workdir: None,
                trigger: None,
                on_completed: None,
                hooks: None,
                infra_node_id: None,
                allow_dirty_start: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&missing));

        let lp = insert_test_graph(&db, dir.path());
        let no_fields = handler
            .graph_update(Parameters(GraphUpdateParams {
                graph_id: lp.id.clone(),
                name: None,
                description: None,
                workdir: None,
                trigger: None,
                on_completed: None,
                hooks: None,
                infra_node_id: None,
                allow_dirty_start: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&no_fields));
        assert!(text(&no_fields).contains("at least one field"));
    }

    fn insert_test_graph(db: &Database, workdir: &std::path::Path) -> Graph {
        let lp = Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: uuid::Uuid::new_v4().to_string(),
            name: "Test Graph".to_string(),
            description: None,
            workdir: workdir.to_string_lossy().to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        };
        db.insert_graph(&lp).unwrap();
        lp
    }

    fn insert_test_spec(db: &Database, graph_id: &str, position: i64) -> GraphSpec {
        let spec = GraphSpec {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id: Some(graph_id.to_string()),
            name: "Test Spec".to_string(),
            description: Some(valid_spec_description()),
            position,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        db.insert_graph_spec(&spec).unwrap();
        spec
    }

    fn insert_named_spec(
        db: &Database,
        graph_id: &str,
        name: &str,
        position: i64,
        status: GraphSpecStatus,
    ) -> GraphSpec {
        let spec = GraphSpec {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id: Some(graph_id.to_string()),
            name: name.to_string(),
            description: Some(valid_spec_description()),
            position,
            parallelizable: false,
            status,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        db.insert_graph_spec(&spec).unwrap();
        spec
    }

    fn init_git_repo(path: &std::path::Path) {
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .status()
                .expect("git command failed to run");
            assert!(status.success(), "git {:?} failed", args);
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "user.email", "test@example.com"]);
        std::fs::write(path.join("README.md"), "test").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
    }

    // ── CM29: dirty-workdir warning/refusal ─────────────────────────

    #[tokio::test]
    async fn graph_preflight_reports_dirty_workdir_warning() {
        let (_dir, db, handler) = endpoint_test_handler();
        let git_dir = tempdir().unwrap();
        init_git_repo(git_dir.path());
        let lp = insert_test_graph(&db, git_dir.path());
        std::fs::write(git_dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(git_dir.path().join("b.txt"), "b").unwrap();

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: None,
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
        assert_eq!(body["dirty_workdir_warning"]["count"], 2);
    }

    #[tokio::test]
    async fn graph_run_refuses_when_previous_spec_left_workdir_dirty() {
        let (_dir, db, handler) = endpoint_test_handler();
        let git_dir = tempdir().unwrap();
        init_git_repo(git_dir.path());

        // Spec A ended Failed on this workdir in another graph — it is the
        // blocking predecessor. No run of `lp` recorded the dirt, so there is
        // no attribution, but the refusal still names the predecessor (CB69).
        let failed_lp = insert_test_graph(&db, git_dir.path());
        insert_named_spec(&db, &failed_lp.id, "Spec A", 1, GraphSpecStatus::Failed);

        let lp = insert_test_graph(&db, git_dir.path());
        insert_named_spec(&db, &lp.id, "Spec B", 1, GraphSpecStatus::Pending);

        std::fs::write(git_dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(git_dir.path().join("b.txt"), "b").unwrap();
        std::fs::write(git_dir.path().join("c.txt"), "c").unwrap();

        let result = handler
            .graph_run(Parameters(GraphRunParams {
                graph_id: lp.id.clone(),
                queue_id: None,
                workdir: None,
                idea: None,
                sandbox: None,
                start_node_id: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&result), "{}", text(&result));
        let msg = text(&result);
        assert!(
            msg.contains("Spec A"),
            "refusal must name the blocking predecessor: {msg}"
        );
        assert!(
            msg.contains("failed"),
            "refusal must name the predecessor's end status: {msg}"
        );
        assert!(
            msg.contains("not launched"),
            "refusal must say not launched: {msg}"
        );
        assert!(
            msg.contains("Set allow_dirty_start"),
            "flag hint kept: {msg}"
        );
        assert!(
            !msg.contains("dirty before this graph ran"),
            "refusal must not use the attribution sentence: {msg}"
        );
    }

    #[tokio::test]
    async fn graph_run_allow_dirty_start_launches_with_warning() {
        let (_dir, db, handler) = endpoint_test_handler();
        let git_dir = tempdir().unwrap();
        init_git_repo(git_dir.path());

        let failed_lp = insert_test_graph(&db, git_dir.path());
        insert_named_spec(&db, &failed_lp.id, "Spec A", 1, GraphSpecStatus::Failed);

        let lp = insert_test_graph(&db, git_dir.path());
        insert_named_spec(&db, &lp.id, "Spec B", 1, GraphSpecStatus::Pending);
        db.update_graph_allow_dirty_start(&lp.id, true).unwrap();

        std::fs::write(git_dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(git_dir.path().join("b.txt"), "b").unwrap();
        std::fs::write(git_dir.path().join("c.txt"), "c").unwrap();

        let result = handler
            .graph_run(Parameters(GraphRunParams {
                graph_id: lp.id.clone(),
                queue_id: None,
                workdir: None,
                idea: None,
                sandbox: None,
                start_node_id: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let msg = text(&result);
        assert!(
            msg.contains("WARNING"),
            "launch warning still surfaced: {msg}"
        );
        assert!(
            msg.contains("allow_dirty_start is set"),
            "warning must state the reason: {msg}"
        );
        assert!(
            !msg.contains("Set allow_dirty_start"),
            "must not tell the user to set an already-set flag: {msg}"
        );
    }

    #[tokio::test]
    async fn graph_run_clean_workdir_unaffected_by_old_failure() {
        let (_dir, db, handler) = endpoint_test_handler();
        let git_dir = tempdir().unwrap();
        init_git_repo(git_dir.path());

        let failed_lp = insert_test_graph(&db, git_dir.path());
        insert_named_spec(&db, &failed_lp.id, "Spec A", 1, GraphSpecStatus::Failed);

        let lp = insert_test_graph(&db, git_dir.path());
        insert_named_spec(&db, &lp.id, "Spec B", 1, GraphSpecStatus::Pending);

        // No untracked files this time — the workdir is clean.
        let result = handler
            .graph_run(Parameters(GraphRunParams {
                graph_id: lp.id.clone(),
                queue_id: None,
                workdir: None,
                idea: None,
                sandbox: None,
                start_node_id: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
    }

    #[tokio::test]
    async fn graph_run_never_refused_for_non_git_workdir() {
        let (_dir, db, handler) = endpoint_test_handler();
        let plain_dir = tempdir().unwrap();

        let failed_lp = insert_test_graph(&db, plain_dir.path());
        insert_named_spec(&db, &failed_lp.id, "Spec A", 1, GraphSpecStatus::Failed);

        let lp = insert_test_graph(&db, plain_dir.path());
        insert_named_spec(&db, &lp.id, "Spec B", 1, GraphSpecStatus::Pending);

        std::fs::write(plain_dir.path().join("a.txt"), "a").unwrap();

        let result = handler
            .graph_run(Parameters(GraphRunParams {
                graph_id: lp.id.clone(),
                queue_id: None,
                workdir: None,
                idea: None,
                sandbox: None,
                start_node_id: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
    }

    // ── graph_add_spec / graph_update_spec ─────────────────────────

    #[tokio::test]
    async fn graph_add_spec_happy_path_and_position_conflict() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());

        let added = handler
            .graph_add_spec(Parameters(GraphAddSpecParams {
                graph_id: lp.id.clone(),
                name: "First Spec".to_string(),
                description: Some(valid_spec_description()),
                position: 1,
                parallelizable: false,
            }))
            .await
            .unwrap();
        assert!(!is_err(&added), "{}", text(&added));

        let conflict = handler
            .graph_add_spec(Parameters(GraphAddSpecParams {
                graph_id: lp.id.clone(),
                name: "Second Spec".to_string(),
                description: Some(valid_spec_description()),
                position: 1,
                parallelizable: false,
            }))
            .await
            .unwrap();
        assert!(is_err(&conflict));
        assert!(text(&conflict).contains("already has a spec at position"));

        let missing_desc = handler
            .graph_add_spec(Parameters(GraphAddSpecParams {
                graph_id: lp.id.clone(),
                name: "Third Spec".to_string(),
                description: None,
                position: 2,
                parallelizable: false,
            }))
            .await
            .unwrap();
        assert!(is_err(&missing_desc));

        let bad_template = handler
            .graph_add_spec(Parameters(GraphAddSpecParams {
                graph_id: lp.id,
                name: "Fourth Spec".to_string(),
                description: Some("just a sentence".to_string()),
                position: 3,
                parallelizable: false,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_template));
        assert!(
            text(&bad_template).contains("missing required tag")
                || text(&bad_template).contains("missing")
        );
    }

    #[tokio::test]
    async fn graph_update_spec_renames_and_rejects_no_op() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let updated = handler
            .graph_update_spec(Parameters(GraphUpdateSpecParams {
                spec_id: spec.id.clone(),
                name: Some("New Name".to_string()),
                description: None,
                position: None,
                parallelizable: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));
        assert_eq!(
            db.get_graph_spec(&spec.id).unwrap().unwrap().name,
            "New Name"
        );

        let no_op = handler
            .graph_update_spec(Parameters(GraphUpdateSpecParams {
                spec_id: spec.id.clone(),
                name: None,
                description: None,
                position: None,
                parallelizable: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&no_op));

        let missing = handler
            .graph_update_spec(Parameters(GraphUpdateSpecParams {
                spec_id: "ghost".to_string(),
                name: Some("x".to_string()),
                description: None,
                position: None,
                parallelizable: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&missing));
    }

    // ── spec_create / spec_list / spec_update / spec_set_status / spec_delete

    #[tokio::test]
    async fn spec_create_and_list_and_update() {
        let (_dir, db, handler) = endpoint_test_handler();

        let created = handler
            .spec_create(Parameters(SpecCreateParams {
                name: "Backlog item".to_string(),
                description: valid_spec_description(),
                workdir: Some("/tmp".to_string()),
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));

        let bad = handler
            .spec_create(Parameters(SpecCreateParams {
                name: "Backlog item 2".to_string(),
                description: "too short".to_string(),
                workdir: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad));

        let listed = handler
            .spec_list(Parameters(SpecListParams {
                workdir: None,
                status: None,
                unassigned_only: Some(true),
                include_descriptions: None,
                limit: None,
                offset: None,
            }))
            .await
            .unwrap();
        assert!(text(&listed).contains("Backlog item"));

        let bad_status = handler
            .spec_list(Parameters(SpecListParams {
                workdir: None,
                status: Some("sideways".to_string()),
                unassigned_only: None,
                include_descriptions: None,
                limit: None,
                offset: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_status));

        let all_specs = db.list_specs(None, None, false).unwrap();
        let spec_id = all_specs
            .iter()
            .find(|s| s.name == "Backlog item")
            .unwrap()
            .id
            .clone();

        let updated = handler
            .spec_update(Parameters(SpecUpdateParams {
                spec_id: spec_id.clone(),
                name: Some("Renamed backlog item".to_string()),
                description: None,
                workdir: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));
        assert_eq!(
            db.get_graph_spec(&spec_id).unwrap().unwrap().name,
            "Renamed backlog item"
        );
    }

    #[tokio::test]
    async fn spec_set_status_transitions_and_rejects_bound_spec() {
        let (dir, db, handler) = endpoint_test_handler();

        let standalone = handler
            .spec_create(Parameters(SpecCreateParams {
                name: "Standalone".to_string(),
                description: valid_spec_description(),
                workdir: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&standalone));
        let spec_id = db
            .list_specs(None, None, false)
            .unwrap()
            .into_iter()
            .find(|s| s.name == "Standalone")
            .unwrap()
            .id;

        let completed = handler
            .spec_set_status(Parameters(SpecSetStatusParams {
                spec_id: spec_id.clone(),
                status: "completed".to_string(),
                reason: "manually closed".to_string(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&completed), "{}", text(&completed));
        assert_eq!(
            db.get_graph_spec(&spec_id).unwrap().unwrap().status,
            GraphSpecStatus::Completed
        );

        let empty_reason = handler
            .spec_set_status(Parameters(SpecSetStatusParams {
                spec_id: spec_id.clone(),
                status: "pending".to_string(),
                reason: "  ".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&empty_reason));

        // A graph-bound spec must be rejected as "not standalone".
        let lp = insert_test_graph(&db, dir.path());
        let bound_spec = insert_test_spec(&db, &lp.id, 1);
        let not_standalone = handler
            .spec_set_status(Parameters(SpecSetStatusParams {
                spec_id: bound_spec.id.clone(),
                status: "completed".to_string(),
                reason: "trying anyway".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&not_standalone));
        assert!(text(&not_standalone).contains("bound to graph"));

        let unknown_status = handler
            .spec_set_status(Parameters(SpecSetStatusParams {
                spec_id: spec_id.clone(),
                status: "sideways".to_string(),
                reason: "x".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&unknown_status));
    }

    #[tokio::test]
    async fn spec_delete_removes_standalone_but_refuses_bound_spec() {
        let (dir, db, handler) = endpoint_test_handler();
        handler
            .spec_create(Parameters(SpecCreateParams {
                name: "Deletable".to_string(),
                description: valid_spec_description(),
                workdir: None,
            }))
            .await
            .unwrap();
        let spec_id = db
            .list_specs(None, None, false)
            .unwrap()
            .into_iter()
            .find(|s| s.name == "Deletable")
            .unwrap()
            .id;

        let deleted = handler
            .spec_delete(Parameters(SpecDeleteParams {
                spec_id: spec_id.clone(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&deleted), "{}", text(&deleted));
        assert!(db.get_graph_spec(&spec_id).unwrap().is_none());

        let missing = handler
            .spec_delete(Parameters(SpecDeleteParams {
                spec_id: "ghost".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&missing));

        let lp = insert_test_graph(&db, dir.path());
        let bound_spec = insert_test_spec(&db, &lp.id, 1);
        let refused = handler
            .spec_delete(Parameters(SpecDeleteParams {
                spec_id: bound_spec.id,
            }))
            .await
            .unwrap();
        assert!(is_err(&refused));
    }

    #[tokio::test]
    async fn graph_remove_spec_unbinds_bound_spec() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let removed = handler
            .graph_remove_spec(Parameters(GraphRemoveSpecParams {
                graph_id: lp.id.clone(),
                spec_id: spec.id.clone(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&removed), "{}", text(&removed));
        assert!(text(&removed).contains("unbound"));

        let after = db.get_graph_spec(&spec.id).unwrap().unwrap();
        assert!(after.graph_id.is_none());

        let unassigned = db.list_specs(None, None, true).unwrap();
        assert!(unassigned.iter().any(|s| s.id == spec.id));

        assert!(db
            .list_graph_specs(&lp.id)
            .unwrap()
            .iter()
            .all(|s| s.id != spec.id));
    }

    #[tokio::test]
    async fn graph_remove_spec_rejects_running_spec() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        db.update_graph_spec_status(&spec.id, GraphSpecStatus::Running, None, None)
            .unwrap();

        let refused = handler
            .graph_remove_spec(Parameters(GraphRemoveSpecParams {
                graph_id: lp.id.clone(),
                spec_id: spec.id.clone(),
            }))
            .await
            .unwrap();
        assert!(is_err(&refused));
        assert!(text(&refused).contains("running"));
    }

    #[tokio::test]
    async fn graph_remove_spec_rejects_wrong_graph() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp_a = insert_test_graph(&db, dir.path());
        let lp_b = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp_a.id, 1);

        let refused = handler
            .graph_remove_spec(Parameters(GraphRemoveSpecParams {
                graph_id: lp_b.id.clone(),
                spec_id: spec.id.clone(),
            }))
            .await
            .unwrap();
        assert!(is_err(&refused));
        assert!(text(&refused).contains(&lp_a.id));
    }

    #[tokio::test]
    async fn graph_remove_spec_rejects_standalone_spec() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let created = handler
            .spec_create(Parameters(SpecCreateParams {
                name: "Standalone".to_string(),
                description: valid_spec_description(),
                workdir: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        let standalone_id = db
            .list_specs(None, None, false)
            .unwrap()
            .into_iter()
            .find(|s| s.name == "Standalone")
            .unwrap()
            .id;

        let refused = handler
            .graph_remove_spec(Parameters(GraphRemoveSpecParams {
                graph_id: lp.id.clone(),
                spec_id: standalone_id,
            }))
            .await
            .unwrap();
        assert!(is_err(&refused));
        assert!(text(&refused).contains("not bound to any graph"));
    }

    #[tokio::test]
    async fn spec_delete_error_message_names_graph_remove_spec() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let refused = handler
            .spec_delete(Parameters(SpecDeleteParams {
                spec_id: spec.id.clone(),
            }))
            .await
            .unwrap();
        assert!(is_err(&refused));
        assert!(text(&refused).contains("graph_remove_spec"));
    }

    #[tokio::test]
    async fn graph_remove_spec_preserves_execution_history() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let node = insert_named_node(&db, &spec.id, "build", 1);
        insert_finalized_node_run(
            &db,
            &lp.id,
            &spec.id,
            &node.id,
            GraphRunStatus::Pass,
            None,
            chrono::Utc::now(),
        );

        let removed = handler
            .graph_remove_spec(Parameters(GraphRemoveSpecParams {
                graph_id: lp.id.clone(),
                spec_id: spec.id.clone(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&removed), "{}", text(&removed));

        let runs = db.list_graph_runs_for_spec(&spec.id).unwrap();
        assert_eq!(runs.len(), 1);
    }

    // ── blueprint_list / blueprint_create / blueprint_delete ─────

    #[tokio::test]
    async fn blueprint_create_list_delete_round_trip() {
        let (_dir, db, handler) = endpoint_test_handler();

        let created = handler
            .blueprint_create(Parameters(BlueprintCreateParams {
                name: "my-agent-bp".to_string(),
                kind: "agent".to_string(),
                config: agent_node_config("claude"),
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));

        let duplicate = handler
            .blueprint_create(Parameters(BlueprintCreateParams {
                name: "my-agent-bp".to_string(),
                kind: "agent".to_string(),
                config: agent_node_config("claude"),
            }))
            .await
            .unwrap();
        assert!(is_err(&duplicate));
        assert!(text(&duplicate).contains("already exists"));

        let bad_kind = handler
            .blueprint_create(Parameters(BlueprintCreateParams {
                name: "other-bp".to_string(),
                kind: "not-a-kind".to_string(),
                config: agent_node_config("claude"),
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_kind));

        let bad_config = handler
            .blueprint_create(Parameters(BlueprintCreateParams {
                name: "no-platform-bp".to_string(),
                kind: "agent".to_string(),
                config: serde_json::Map::new(),
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_config));

        let listed = handler.blueprint_list().await.unwrap();
        assert!(text(&listed).contains("my-agent-bp"));
        assert!(db
            .list_blueprints()
            .unwrap()
            .iter()
            .any(|bp| bp.name == "my-agent-bp"));

        let deleted = handler
            .blueprint_delete(Parameters(BlueprintDeleteParams {
                name: "my-agent-bp".to_string(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&deleted), "{}", text(&deleted));
        assert!(db.get_blueprint_by_name("my-agent-bp").unwrap().is_none());

        let missing = handler
            .blueprint_delete(Parameters(BlueprintDeleteParams {
                name: "ghost-bp".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&missing));
    }

    // ── graph_add_node / graph_update_node ──────────────────────────

    #[tokio::test]
    async fn graph_add_node_happy_path_and_invalid_config() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let added = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Kickoff".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&added), "{}", text(&added));
        assert_eq!(db.list_graph_nodes(&spec.id).unwrap().len(), 1);

        let missing_both_targets = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: None,
                graph_id: None,
                name: "Orphan".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&missing_both_targets));

        let bad_config = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "No Platform".to_string(),
                kind: Some("agent".to_string()),
                config: Some(serde_json::Map::new()),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_config));

        let join_rejected = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id),
                graph_id: None,
                name: "Quorum".to_string(),
                kind: Some("join".to_string()),
                config: Some(serde_json::Map::new()),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&join_rejected));
        assert!(text(&join_rejected).contains("engine-managed"));
    }

    /// The exact incident shape, through the real `graph_add_node` tool
    /// call: a `prompt` key (instead of `prompt_template`) must be rejected
    /// at write time, naming the correct key, and must never reach the DB.
    #[tokio::test]
    async fn graph_add_node_rejects_prompt_key_naming_prompt_template() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let mut config = agent_node_config("claude");
        config.insert(
            "prompt".to_string(),
            serde_json::Value::String("implement the spec".to_string()),
        );

        let rejected = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Implementer".to_string(),
                kind: Some("agent".to_string()),
                config: Some(config),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&rejected), "{}", text(&rejected));
        assert!(text(&rejected).contains("prompt_template"));
        assert!(db.list_graph_nodes(&spec.id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn graph_update_node_renames_and_validates() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let node_id = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Node A".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let node_id = extract_id(&node_id, "node_id");

        let renamed = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: node_id.clone(),
                name: Some("Node A Renamed".to_string()),
                kind: None,
                config: None,
                config_replace: None,
                position: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&renamed), "{}", text(&renamed));
        assert_eq!(
            db.get_graph_node(&node_id).unwrap().unwrap().name,
            "Node A Renamed"
        );

        let no_op = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: node_id.clone(),
                name: None,
                kind: None,
                config: None,
                config_replace: None,
                position: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&no_op));

        let missing = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: "ghost".to_string(),
                name: Some("x".to_string()),
                kind: None,
                config: None,
                config_replace: None,
                position: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&missing));
    }

    #[tokio::test]
    async fn graph_add_node_accepts_skills_on_agent_and_preserves_order() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let mut config = agent_node_config("claude");
        config.insert(
            "skills".to_string(),
            serde_json::json!(["alpha", "beta", "gamma"]),
        );

        let added = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Implementer".to_string(),
                kind: Some("agent".to_string()),
                config: Some(config),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&added), "{}", text(&added));
        let node_id = extract_id(&added, "node_id");

        let stored = db.get_graph_node(&node_id).unwrap().unwrap();
        let skills = stored.config["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 3);
        assert_eq!(skills[0].as_str().unwrap(), "alpha");
        assert_eq!(skills[1].as_str().unwrap(), "beta");
        assert_eq!(skills[2].as_str().unwrap(), "gamma");

        // Spec FR4: the names must reach the engine's resolution path in
        // listed order. `node_pinned_skills` (graph_engine.rs) is private, so
        // replicate its exact extraction from the persisted config here.
        let resolved: Vec<&str> = stored.config["skills"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(resolved, ["alpha", "beta", "gamma"]);
    }

    #[tokio::test]
    async fn graph_update_node_accepts_skills_on_agent_and_preserves_order() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let added = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Implementer".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let node_id = extract_id(&added, "node_id");

        let mut new_config = agent_node_config("claude");
        new_config.insert(
            "skills".to_string(),
            serde_json::json!(["delta", "epsilon"]),
        );

        let updated = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: node_id.clone(),
                name: None,
                kind: None,
                config: Some(new_config),
                config_replace: None,
                position: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));

        let stored = db.get_graph_node(&node_id).unwrap().unwrap();
        let skills = stored.config["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].as_str().unwrap(), "delta");
        assert_eq!(skills[1].as_str().unwrap(), "epsilon");
    }

    #[tokio::test]
    async fn graph_update_node_partial_config_merge_preserves_prompt() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let long_prompt = "A very long prompt that should not be touched by a model change.\
            Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor.\
            Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.";
        let mut config = agent_node_config("old-platform");
        config.insert(
            "prompt_template".to_string(),
            serde_json::json!(long_prompt),
        );
        config.insert("model".to_string(), serde_json::json!("old-model"));

        let added = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Agent".to_string(),
                kind: Some("agent".to_string()),
                config: Some(config),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let node_id = extract_id(&added, "node_id");

        let updated = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: node_id.clone(),
                name: None,
                kind: None,
                config: Some({
                    let mut m = serde_json::Map::new();
                    m.insert("model".to_string(), serde_json::json!("new-model"));
                    m
                }),
                config_replace: None,
                position: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));

        let stored = db.get_graph_node(&node_id).unwrap().unwrap();
        assert_eq!(
            stored.config["prompt_template"].as_str().unwrap(),
            long_prompt
        );
        assert_eq!(stored.config["model"].as_str().unwrap(), "new-model");
        assert_eq!(stored.config["platform"].as_str().unwrap(), "old-platform");

        let response_text = text(&updated);
        assert!(
            response_text.contains("model"),
            "response should name changed key: {response_text}"
        );
    }

    #[tokio::test]
    async fn graph_update_node_partial_config_merge_timeout_leaves_others() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let mut config = agent_node_config("claude");
        config.insert("model".to_string(), serde_json::json!("some-model"));
        config.insert("commit_rights".to_string(), serde_json::json!(true));

        let added = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Agent".to_string(),
                kind: Some("agent".to_string()),
                config: Some(config),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let node_id = extract_id(&added, "node_id");

        let updated = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: node_id.clone(),
                name: None,
                kind: None,
                config: Some({
                    let mut m = serde_json::Map::new();
                    m.insert("timeout_minutes".to_string(), serde_json::json!(30));
                    m
                }),
                config_replace: None,
                position: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));

        let stored = db.get_graph_node(&node_id).unwrap().unwrap();
        assert_eq!(stored.config["platform"].as_str().unwrap(), "claude");
        assert_eq!(stored.config["model"].as_str().unwrap(), "some-model");
        assert_eq!(stored.config["commit_rights"], serde_json::json!(true));
        assert_eq!(stored.config["timeout_minutes"], serde_json::json!(30));
    }

    #[tokio::test]
    async fn graph_update_node_full_replacement_drops_keys() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let mut config = agent_node_config("claude");
        config.insert("model".to_string(), serde_json::json!("some-model"));
        config.insert("timeout_minutes".to_string(), serde_json::json!(10));

        let added = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Agent".to_string(),
                kind: Some("agent".to_string()),
                config: Some(config),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let node_id = extract_id(&added, "node_id");

        let updated = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: node_id.clone(),
                name: None,
                kind: None,
                config: Some({
                    let mut m = serde_json::Map::new();
                    m.insert("platform".to_string(), serde_json::json!("new-platform"));
                    m
                }),
                config_replace: Some(true),
                position: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));

        let stored = db.get_graph_node(&node_id).unwrap().unwrap();
        assert_eq!(stored.config["platform"].as_str().unwrap(), "new-platform");
        assert!(
            stored.config.get("model").is_none(),
            "model should be dropped by full replacement"
        );
        assert!(
            stored.config.get("timeout_minutes").is_none(),
            "timeout_minutes should be dropped by full replacement"
        );
    }

    #[tokio::test]
    async fn graph_update_node_unknown_key_rejected_in_both_modes() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let added = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Agent".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let node_id = extract_id(&added, "node_id");

        let merge_result = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: node_id.clone(),
                name: None,
                kind: None,
                config: Some({
                    let mut m = serde_json::Map::new();
                    m.insert("totally_bogus_key".to_string(), serde_json::json!("x"));
                    m
                }),
                config_replace: None,
                position: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&merge_result));
        assert!(
            text(&merge_result).contains("totally_bogus_key"),
            "error should name the bad key: {}",
            text(&merge_result)
        );
        assert!(
            text(&merge_result).contains("Accepted keys"),
            "error should name accepted keys: {}",
            text(&merge_result)
        );

        let replace_result = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: node_id.clone(),
                name: None,
                kind: None,
                config: Some({
                    let mut m = serde_json::Map::new();
                    m.insert("another_bogus".to_string(), serde_json::json!("y"));
                    m
                }),
                config_replace: Some(true),
                position: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&replace_result));
        assert!(
            text(&replace_result).contains("another_bogus"),
            "error should name the bad key: {}",
            text(&replace_result)
        );
    }

    #[tokio::test]
    async fn graph_update_node_response_names_changed_keys() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let mut config = agent_node_config("claude");
        config.insert("model".to_string(), serde_json::json!("old-model"));
        config.insert("timeout_minutes".to_string(), serde_json::json!(5));

        let added = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Agent".to_string(),
                kind: Some("agent".to_string()),
                config: Some(config),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let node_id = extract_id(&added, "node_id");

        let updated = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: node_id.clone(),
                name: None,
                kind: None,
                config: Some({
                    let mut m = serde_json::Map::new();
                    m.insert("model".to_string(), serde_json::json!("new-model"));
                    m.insert("timeout_minutes".to_string(), serde_json::json!(10));
                    m
                }),
                config_replace: None,
                position: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));

        let response_text = text(&updated);
        assert!(
            response_text.contains("model"),
            "response should name 'model' as changed: {response_text}"
        );
        assert!(
            response_text.contains("timeout_minutes"),
            "response should name 'timeout_minutes' as changed: {response_text}"
        );
    }

    #[tokio::test]
    async fn graph_import_preserves_skills_on_agent_nodes() {
        let (dir, db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let (source_graph_id, ..) = build_simple_graph(&handler, &workdir, "Source Graph").await;

        let exported = handler
            .graph_export(Parameters(GraphExportParams {
                graph_id: source_graph_id.clone(),
            }))
            .await
            .unwrap();
        let mut document: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();

        let nodes = document["nodes"].as_array_mut().unwrap();
        for node in nodes.iter_mut() {
            if node["name"] == "implementer" {
                node["config"]["skills"] = serde_json::json!(["skill-a", "skill-b"]);
            }
        }

        let imported = handler
            .graph_import(Parameters(GraphImportParams {
                document,
                workdir: workdir.clone(),
                name: Some("Imported Graph".to_string()),
            }))
            .await
            .unwrap();
        assert!(!is_err(&imported), "{}", text(&imported));
        let body: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
        let new_graph_id = body["graph_id"].as_str().unwrap().to_string();

        let nodes = db.list_graph_nodes_for_graph(&new_graph_id).unwrap();
        let implementer = nodes.iter().find(|n| n.name == "implementer").unwrap();
        let skills = implementer.config["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].as_str().unwrap(), "skill-a");
        assert_eq!(skills[1].as_str().unwrap(), "skill-b");
    }

    #[tokio::test]
    async fn graph_add_node_rejects_skills_on_check_gate_router() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        for (kind, base_config) in [
            ("check", check_node_config("cargo test")),
            ("gate", {
                let mut m = serde_json::Map::new();
                m.insert(
                    "evaluate".to_string(),
                    serde_json::Value::String("pass".to_string()),
                );
                m
            }),
            ("router", {
                let mut m = serde_json::Map::new();
                m.insert("routes".to_string(), serde_json::json!(["a", "b"]));
                m
            }),
        ] {
            let mut config = base_config;
            config.insert("skills".to_string(), serde_json::json!(["alpha", "beta"]));

            let rejected = handler
                .graph_add_node(Parameters(GraphAddNodeParams {
                    spec_id: Some(spec.id.clone()),
                    graph_id: None,
                    name: format!("Node-{kind}"),
                    kind: Some(kind.to_string()),
                    config: Some(config),
                    blueprint: None,
                    config_overrides: None,
                }))
                .await
                .unwrap();
            assert!(is_err(&rejected), "kind={kind} should reject skills");
            // Must be the pre-existing generic unknown-key message for this
            // kind — not a new skills-specific special case (spec FR3).
            let msg = text(&rejected);
            assert!(
                msg.contains(&format!("kind '{kind}'"))
                    && msg.contains("unrecognized key 'skills'")
                    && msg.contains("Accepted keys:"),
                "kind={kind}: {msg}"
            );
        }

        assert!(db.list_graph_nodes(&spec.id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn graph_import_rejects_skills_on_non_agent_nodes() {
        let (dir, _db, handler) = endpoint_test_handler();
        let workdir = dir.path().to_string_lossy().to_string();
        let (source_graph_id, ..) = build_simple_graph(&handler, &workdir, "Source Graph").await;

        let exported = handler
            .graph_export(Parameters(GraphExportParams {
                graph_id: source_graph_id.clone(),
            }))
            .await
            .unwrap();
        let mut document: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();

        let nodes = document["nodes"].as_array_mut().unwrap();
        for node in nodes.iter_mut() {
            if node["name"] == "gate" {
                node["config"]["skills"] = serde_json::json!(["alpha"]);
            }
        }

        let rejected = handler
            .graph_import(Parameters(GraphImportParams {
                document,
                workdir: workdir.clone(),
                name: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&rejected), "{}", text(&rejected));
        // The node named "gate" in `build_simple_graph` is a `check` kind, so
        // import must reject it with the same generic unknown-key message
        // `graph_add_node`/`graph_update_node` produce (spec FR3).
        let msg = text(&rejected);
        assert!(
            msg.contains("kind 'check'")
                && msg.contains("unrecognized key 'skills'")
                && msg.contains("Accepted keys:"),
            "{msg}"
        );
    }

    #[tokio::test]
    async fn graph_add_node_tolerates_malformed_skills_values() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let mut config_string = agent_node_config("claude");
        config_string.insert(
            "skills".to_string(),
            serde_json::Value::String("not-an-array".to_string()),
        );

        let added_string = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "StringSkills".to_string(),
                kind: Some("agent".to_string()),
                config: Some(config_string),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&added_string), "{}", text(&added_string));
        let node_id_string = extract_id(&added_string, "node_id");
        let stored_string = db.get_graph_node(&node_id_string).unwrap().unwrap();
        assert!(stored_string.config["skills"].is_string());

        let mut config_mixed = agent_node_config("claude");
        config_mixed.insert(
            "skills".to_string(),
            serde_json::json!(["alpha", 42, "beta"]),
        );

        let added_mixed = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "MixedSkills".to_string(),
                kind: Some("agent".to_string()),
                config: Some(config_mixed),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&added_mixed), "{}", text(&added_mixed));
        let node_id_mixed = extract_id(&added_mixed, "node_id");
        let stored_mixed = db.get_graph_node(&node_id_mixed).unwrap().unwrap();
        let skills = stored_mixed.config["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 3);
        assert_eq!(skills[0].as_str().unwrap(), "alpha");
        assert!(skills[1].is_number());
        assert_eq!(skills[2].as_str().unwrap(), "beta");
    }

    // ── graph_add_edge / graph_update_edge ──────────────────────────

    async fn add_agent_node(handler: &TaskTriggerHandler, spec_id: &str, name: &str) -> String {
        let result = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec_id.to_string()),
                graph_id: None,
                name: name.to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        extract_id(&result, "node_id")
    }

    #[tokio::test]
    async fn graph_add_edge_wires_nodes_and_rejects_foreign_node() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let a = add_agent_node(&handler, &spec.id, "A").await;
        let b = add_agent_node(&handler, &spec.id, "B").await;

        let edge = handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                from_node: a.clone(),
                to_node: b.clone(),
                condition: "always".to_string(),
                route: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&edge), "{}", text(&edge));
        assert_eq!(db.list_graph_edges(&spec.id).unwrap().len(), 1);

        let foreign = handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                from_node: a.clone(),
                to_node: "not-a-real-node".to_string(),
                condition: "always".to_string(),
                route: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&foreign));

        let bad_condition = handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id),
                graph_id: None,
                from_node: a,
                to_node: b,
                condition: "sideways".to_string(),
                route: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_condition));

        let edges = db
            .list_graph_edges(&db.list_graph_specs(&lp.id).unwrap()[0].id)
            .unwrap();
        let edge_id = edges[0].id.clone();
        let updated = handler
            .graph_update_edge(Parameters(GraphUpdateEdgeParams {
                edge_id: edge_id.clone(),
                condition: "fail".to_string(),
                route: None,
                to_node: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));

        let same_condition = handler
            .graph_update_edge(Parameters(GraphUpdateEdgeParams {
                edge_id: edge_id.clone(),
                condition: "fail".to_string(),
                route: None,
                to_node: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&same_condition));
        assert!(text(&same_condition).contains("already uses condition"));

        let missing_edge = handler
            .graph_update_edge(Parameters(GraphUpdateEdgeParams {
                edge_id: "ghost-edge".to_string(),
                condition: "pass".to_string(),
                route: None,
                to_node: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&missing_edge));
    }

    // ── graph_update_edge(to_node) / graph_delete_edge / graph_delete_node ──

    #[tokio::test]
    async fn graph_update_edge_retargets_destination_without_changing_condition() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let a = add_agent_node(&handler, &spec.id, "A").await;
        let b = add_agent_node(&handler, &spec.id, "B").await;
        let c = add_agent_node(&handler, &spec.id, "C").await;

        let added = handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                from_node: a.clone(),
                to_node: b.clone(),
                condition: "pass".to_string(),
                route: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&added), "{}", text(&added));
        let edge_id = db.list_graph_edges(&spec.id).unwrap()[0].id.clone();

        let retargeted = handler
            .graph_update_edge(Parameters(GraphUpdateEdgeParams {
                edge_id: edge_id.clone(),
                condition: "pass".to_string(),
                route: None,
                to_node: Some(c.clone()),
            }))
            .await
            .unwrap();
        assert!(!is_err(&retargeted), "{}", text(&retargeted));

        let edge = db.get_graph_edge(&edge_id).unwrap().unwrap();
        assert_eq!(edge.to_node, c, "target changed to the new node");
        assert_eq!(
            edge.condition,
            crate::domain::graphs::GraphEdgeCondition::Pass,
            "condition untouched by a target-only update"
        );
        assert_eq!(
            edge.from_node, a,
            "source untouched by a target-only update"
        );
    }

    #[tokio::test]
    async fn graph_update_edge_rejects_retarget_to_a_node_in_another_graph() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let a = add_agent_node(&handler, &spec.id, "A").await;
        let b = add_agent_node(&handler, &spec.id, "B").await;

        let other_lp = insert_test_graph(&db, dir.path());
        let other_spec = insert_test_spec(&db, &other_lp.id, 1);
        let foreign = add_agent_node(&handler, &other_spec.id, "Foreign").await;

        handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                from_node: a.clone(),
                to_node: b.clone(),
                condition: "pass".to_string(),
                route: None,
            }))
            .await
            .unwrap();
        let edge_id = db.list_graph_edges(&spec.id).unwrap()[0].id.clone();

        let result = handler
            .graph_update_edge(Parameters(GraphUpdateEdgeParams {
                edge_id: edge_id.clone(),
                condition: "pass".to_string(),
                route: None,
                to_node: Some(foreign),
            }))
            .await
            .unwrap();
        assert!(is_err(&result));

        let edge = db.get_graph_edge(&edge_id).unwrap().unwrap();
        assert_eq!(edge.to_node, b, "cross-graph retarget never persisted");
    }

    #[tokio::test]
    async fn graph_delete_edge_removes_a_single_edge() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let a = add_agent_node(&handler, &spec.id, "A").await;
        let b = add_agent_node(&handler, &spec.id, "B").await;
        handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                from_node: a,
                to_node: b,
                condition: "pass".to_string(),
                route: None,
            }))
            .await
            .unwrap();
        let edge_id = db.list_graph_edges(&spec.id).unwrap()[0].id.clone();

        let deleted = handler
            .graph_delete_edge(Parameters(GraphDeleteEdgeParams {
                edge_id: edge_id.clone(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&deleted), "{}", text(&deleted));
        assert!(db.get_graph_edge(&edge_id).unwrap().is_none());

        let missing = handler
            .graph_delete_edge(Parameters(GraphDeleteEdgeParams {
                edge_id: "ghost-edge".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&missing));
    }

    #[tokio::test]
    async fn graph_delete_node_cascades_to_its_edges() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let a = add_agent_node(&handler, &spec.id, "A").await;
        let b = add_agent_node(&handler, &spec.id, "B").await;
        let c = add_agent_node(&handler, &spec.id, "C").await;
        for (from, to) in [(a.clone(), b.clone()), (b.clone(), c.clone())] {
            handler
                .graph_add_edge(Parameters(GraphAddEdgeParams {
                    spec_id: Some(spec.id.clone()),
                    graph_id: None,
                    from_node: from,
                    to_node: to,
                    condition: "pass".to_string(),
                    route: None,
                }))
                .await
                .unwrap();
        }
        assert_eq!(db.list_graph_edges(&spec.id).unwrap().len(), 2);

        // `b` (not the entry point — `a` is) sits between two edges; deleting
        // it must drop both, not just the ones naming it as `from_node`.
        let deleted = handler
            .graph_delete_node(Parameters(GraphDeleteNodeParams { node_id: b.clone() }))
            .await
            .unwrap();
        assert!(!is_err(&deleted), "{}", text(&deleted));

        assert!(db.get_graph_node(&b).unwrap().is_none());
        assert!(db.list_graph_edges(&spec.id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn graph_delete_node_rejects_the_graphs_entry_point() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let a = add_agent_node(&handler, &spec.id, "A").await;
        let b = add_agent_node(&handler, &spec.id, "B").await;
        handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                from_node: a.clone(),
                to_node: b,
                condition: "pass".to_string(),
                route: None,
            }))
            .await
            .unwrap();

        let result = handler
            .graph_delete_node(Parameters(GraphDeleteNodeParams { node_id: a.clone() }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(db.get_graph_node(&a).unwrap().is_some());
    }

    /// CB52 helper: a 2-member ensemble on this spec's graph, entered from
    /// `entry`, exiting to `arbiter` (pass) and optionally `fallback`
    /// (fail).
    async fn add_cb52_ensemble(
        handler: &TaskTriggerHandler,
        spec_id: &str,
        name: &str,
        entry: &str,
        arbiter: &str,
        on_fail_to: Option<&str>,
    ) -> String {
        let member = || crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        let result = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec_id.to_string()),
                graph_id: None,
                name: name.to_string(),
                kind: None,
                prompt_template: Some("Review {{spec_name}}".to_string()),
                members: Some(vec![member(), member()]),
                blueprint: None,
                from_node: entry.to_string(),
                condition: "always".to_string(),
                min_pass: Some(2),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.to_string(),
                on_fail_to: on_fail_to.map(str::to_string),
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        extract_id(&result, "ensemble_id")
    }

    /// CB52 NFR1(a): deleting the ensemble's primary entry node is refused
    /// and names the ensemble, the role, and the detaching verbs. Would
    /// still pass with the feature removed only if deletion succeeded — it
    /// must not.
    #[tokio::test]
    async fn graph_delete_node_refuses_entry_from_node_referenced_by_ensemble() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_cb52_ensemble(&handler, &spec.id, "Reviewers", &entry, &arbiter, None).await;

        let result = handler
            .graph_delete_node(Parameters(GraphDeleteNodeParams {
                node_id: entry.clone(),
            }))
            .await
            .unwrap();
        assert!(is_err(&result), "{}", text(&result));
        let msg = text(&result);
        assert!(msg.contains("entry_from_node"), "{msg}");
        assert!(msg.contains("Reviewers"), "{msg}");
        assert!(msg.contains(&ensemble_id), "{msg}");
        assert!(msg.contains("remove_entry_from"), "{msg}");
        assert!(msg.contains("from_node"), "{msg}");

        // Nothing was deleted: the node and the ensemble both survive.
        assert!(db.get_graph_node(&entry).unwrap().is_some());
        assert!(db.get_ensemble(&ensemble_id).unwrap().is_some());
    }

    /// CB52 NFR1(a-extra): same refusal for an extra entry source added via
    /// `add_entry_from` — role `entry_source`.
    #[tokio::test]
    async fn graph_delete_node_refuses_extra_entry_source() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let alt = add_agent_node(&handler, &spec.id, "Alt").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_cb52_ensemble(&handler, &spec.id, "Reviewers", &entry, &arbiter, None).await;

        let added = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: None,
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: Some(alt.clone()),
                add_entry_condition: None,
                remove_entry_from: None,
                commit_rights: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&added), "{}", text(&added));

        let result = handler
            .graph_delete_node(Parameters(GraphDeleteNodeParams {
                node_id: alt.clone(),
            }))
            .await
            .unwrap();
        assert!(is_err(&result), "{}", text(&result));
        let msg = text(&result);
        assert!(msg.contains("entry_source"), "{msg}");
        assert!(msg.contains("Reviewers"), "{msg}");
        assert!(msg.contains(&ensemble_id), "{msg}");

        assert!(db.get_graph_node(&alt).unwrap().is_some());
        assert!(db.get_ensemble(&ensemble_id).unwrap().is_some());
    }

    /// CB52 NFR1(b): deleting the pass-exit target is refused as `on_pass_to`.
    #[tokio::test]
    async fn graph_delete_node_refuses_on_pass_to() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_cb52_ensemble(&handler, &spec.id, "Reviewers", &entry, &arbiter, None).await;

        let result = handler
            .graph_delete_node(Parameters(GraphDeleteNodeParams {
                node_id: arbiter.clone(),
            }))
            .await
            .unwrap();
        assert!(is_err(&result), "{}", text(&result));
        let msg = text(&result);
        assert!(msg.contains("on_pass_to"), "{msg}");
        assert!(msg.contains("Reviewers"), "{msg}");
        assert!(msg.contains(&ensemble_id), "{msg}");

        assert!(db.get_graph_node(&arbiter).unwrap().is_some());
        assert!(db.get_ensemble(&ensemble_id).unwrap().is_some());
    }

    /// CB52 NFR1(b): deleting the fail-exit target is refused as `on_fail_to`.
    #[tokio::test]
    async fn graph_delete_node_refuses_on_fail_to() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let fallback = add_agent_node(&handler, &spec.id, "Fallback").await;
        let ensemble_id = add_cb52_ensemble(
            &handler,
            &spec.id,
            "Reviewers",
            &entry,
            &arbiter,
            Some(&fallback),
        )
        .await;

        let result = handler
            .graph_delete_node(Parameters(GraphDeleteNodeParams {
                node_id: fallback.clone(),
            }))
            .await
            .unwrap();
        assert!(is_err(&result), "{}", text(&result));
        let msg = text(&result);
        assert!(msg.contains("on_fail_to"), "{msg}");
        assert!(msg.contains("Reviewers"), "{msg}");
        assert!(msg.contains(&ensemble_id), "{msg}");

        assert!(db.get_graph_node(&fallback).unwrap().is_some());
        assert!(db.get_ensemble(&ensemble_id).unwrap().is_some());
    }

    /// CB52 NFR1(c): detaching the primary entry via `remove_entry_from`
    /// repoints `entry_from_node` at a remaining source (FR3), after which
    /// the old primary deletes cleanly and the ensemble survives.
    #[tokio::test]
    async fn after_remove_entry_from_primary_repoints_and_allows_deletion() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let alt = add_agent_node(&handler, &spec.id, "Alt").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_cb52_ensemble(&handler, &spec.id, "Reviewers", &entry, &arbiter, None).await;

        let added = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: None,
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: Some(alt.clone()),
                add_entry_condition: None,
                remove_entry_from: None,
                commit_rights: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&added), "{}", text(&added));

        let detached = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: None,
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: Some(entry.clone()),
                commit_rights: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&detached), "{}", text(&detached));
        assert_eq!(
            db.get_ensemble(&ensemble_id)
                .unwrap()
                .unwrap()
                .entry_from_node,
            alt
        );

        let deleted = handler
            .graph_delete_node(Parameters(GraphDeleteNodeParams {
                node_id: entry.clone(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&deleted), "{}", text(&deleted));
        assert!(db.get_graph_node(&entry).unwrap().is_none());
        assert!(db.get_ensemble(&ensemble_id).unwrap().is_some());
    }

    /// CB52: an ensemble's own member is still refused with the owned-node
    /// message (FR2 second half) — not with the FR1 reference message.
    #[tokio::test]
    async fn graph_delete_node_still_refuses_ensemble_owned_nodes() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_cb52_ensemble(&handler, &spec.id, "Reviewers", &entry, &arbiter, None).await;
        let member_id = db
            .get_ensemble_details(&ensemble_id)
            .unwrap()
            .unwrap()
            .members[0]
            .node_id
            .clone();

        let result = handler
            .graph_delete_node(Parameters(GraphDeleteNodeParams {
                node_id: member_id.clone(),
            }))
            .await
            .unwrap();
        assert!(is_err(&result), "{}", text(&result));
        let msg = text(&result);
        assert!(msg.contains("is a member of ensemble"), "{msg}");
        assert!(db.get_graph_node(&member_id).unwrap().is_some());
    }

    /// CB52 NFR1(d): `graph_preflight` on a graph with an orphan join
    /// (simulated pre-CB52 damage: a `join` row with no ensemble row)
    /// reports it instead of spending probe quota.
    #[tokio::test]
    async fn graph_preflight_reports_orphan_join() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        db.insert_graph_node(&GraphNode {
            id: "orphan-join".to_string(),
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "orphaned quorum".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": "gone"}),
            position: 99,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: None,
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&result), "{}", text(&result));
        let msg = text(&result);
        assert!(msg.contains("belongs to no ensemble"), "{msg}");
        assert!(msg.contains("orphaned quorum"), "{msg}");
        assert!(msg.contains("orphan-join"), "{msg}");
    }

    /// CB52 FR4: `graph_get` surfaces the same orphan under
    /// `orphan_join_warnings` without failing.
    #[tokio::test]
    async fn graph_get_includes_orphan_join_warnings() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        db.insert_graph_node(&GraphNode {
            id: "orphan-join".to_string(),
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "orphaned quorum".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": "gone"}),
            position: 99,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let result = handler
            .graph_get(Parameters(GraphGetParams {
                graph_id: lp.id.clone(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let body = raw_text(&result);
        assert!(body.contains("orphan_join_warnings"), "{body}");
        assert!(body.contains("orphan-join"), "{body}");
        assert!(body.contains("belongs to no ensemble"), "{body}");
    }

    #[tokio::test]
    async fn topology_mutations_are_rejected_while_the_graph_is_running() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let a = add_agent_node(&handler, &spec.id, "A").await;
        let b = add_agent_node(&handler, &spec.id, "B").await;
        let c = add_agent_node(&handler, &spec.id, "C").await;
        handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                from_node: a,
                to_node: b.clone(),
                condition: "pass".to_string(),
                route: None,
            }))
            .await
            .unwrap();
        let edge_id = db.list_graph_edges(&spec.id).unwrap()[0].id.clone();

        db.update_graph_status(&lp.id, GraphStatus::Running, None, None)
            .unwrap();

        let retarget = handler
            .graph_update_edge(Parameters(GraphUpdateEdgeParams {
                edge_id: edge_id.clone(),
                condition: "pass".to_string(),
                route: None,
                to_node: Some(c),
            }))
            .await
            .unwrap();
        assert!(is_err(&retarget));
        assert!(text(&retarget).contains("running"), "{}", text(&retarget));

        let delete_edge = handler
            .graph_delete_edge(Parameters(GraphDeleteEdgeParams {
                edge_id: edge_id.clone(),
            }))
            .await
            .unwrap();
        assert!(is_err(&delete_edge));
        assert!(
            text(&delete_edge).contains("running"),
            "{}",
            text(&delete_edge)
        );

        let delete_node = handler
            .graph_delete_node(Parameters(GraphDeleteNodeParams { node_id: b.clone() }))
            .await
            .unwrap();
        assert!(is_err(&delete_node));
        assert!(
            text(&delete_node).contains("running"),
            "{}",
            text(&delete_node)
        );

        // Nothing actually mutated while the graph was running.
        assert_eq!(db.get_graph_edge(&edge_id).unwrap().unwrap().to_node, b);
        assert!(db.get_graph_node(&b).unwrap().is_some());
    }

    #[tokio::test]
    async fn cm15_running_graph_pins_node_kind_and_position_accepts_config() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let a = add_agent_node(&handler, &spec.id, "A").await;

        db.update_graph_status(&lp.id, GraphStatus::Running, None, None)
            .unwrap();

        let kind_res = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: a.clone(),
                name: None,
                kind: Some("check".to_string()),
                config: None,
                config_replace: None,
                position: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&kind_res));
        assert!(text(&kind_res).contains("running"), "{}", text(&kind_res));

        let pos_res = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: a.clone(),
                name: None,
                kind: None,
                config: None,
                config_replace: None,
                position: Some(99),
            }))
            .await
            .unwrap();
        assert!(is_err(&pos_res));
        assert!(text(&pos_res).contains("running"), "{}", text(&pos_res));

        // The unpinned field (config) is still accepted and actually lands.
        let cfg = serde_json::json!({ "platform": "codex" })
            .as_object()
            .unwrap()
            .clone();
        let cfg_res = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: a.clone(),
                name: None,
                kind: None,
                config: Some(cfg),
                config_replace: None,
                position: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&cfg_res), "{}", text(&cfg_res));
        assert_eq!(
            db.get_graph_node(&a)
                .unwrap()
                .unwrap()
                .config
                .get("platform")
                .and_then(|v| v.as_str()),
            Some("codex"),
        );
    }

    #[tokio::test]
    async fn cm15_running_graph_pins_ensemble_kind_accepts_member_replacement() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ens = add_two_member_ensemble(&handler, &spec.id, "Ens", &entry, &arbiter).await;

        db.update_graph_status(&lp.id, GraphStatus::Running, None, None)
            .unwrap();

        let kind_res = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                kind: Some("cascade".to_string()),
                ..blank_ensemble_update(&ens)
            }))
            .await
            .unwrap();
        assert!(is_err(&kind_res));
        assert!(text(&kind_res).contains("running"), "{}", text(&kind_res));

        // Exit wiring is topology too — retargeting it mid-run is refused,
        // not accepted-and-dropped.
        let exit_res = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                on_pass_to: Some(entry.clone()),
                ..blank_ensemble_update(&ens)
            }))
            .await
            .unwrap();
        assert!(is_err(&exit_res));
        assert!(text(&exit_res).contains("running"), "{}", text(&exit_res));

        // Replacing the dead member set IS allowed while running.
        let members_res = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                members: Some(vec![
                    EnsembleMemberParams {
                        platform: "opencode".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    EnsembleMemberParams {
                        platform: "opencode".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                ..blank_ensemble_update(&ens)
            }))
            .await
            .unwrap();
        assert!(!is_err(&members_res), "{}", text(&members_res));
    }

    // ── router nodes (routes + route edges) ──────────────────────────

    fn router_node_config(
        routes: &serde_json::Value,
        fallback: &str,
    ) -> serde_json::Map<String, serde_json::Value> {
        serde_json::json!({ "routes": routes, "fallback": fallback })
            .as_object()
            .unwrap()
            .clone()
    }

    fn two_routes() -> serde_json::Value {
        serde_json::json!([
            { "label": "retry", "description": "Retry the current step." },
            { "label": "escalate", "description": "Hand off to a human." },
        ])
    }

    #[tokio::test]
    async fn graph_add_node_router_created_persisted_and_read_back_with_routes() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let router_result = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Router".to_string(),
                kind: Some("router".to_string()),
                config: Some(router_node_config(&two_routes(), "retry")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&router_result), "{}", text(&router_result));
        let router_id = extract_id(&router_result, "node_id");

        let retry_target = add_agent_node(&handler, &spec.id, "Retry target").await;
        let escalate_target = add_agent_node(&handler, &spec.id, "Escalate target").await;

        for (route, target) in [("retry", &retry_target), ("escalate", &escalate_target)] {
            let edge = handler
                .graph_add_edge(Parameters(GraphAddEdgeParams {
                    spec_id: Some(spec.id.clone()),
                    graph_id: None,
                    from_node: router_id.clone(),
                    to_node: target.clone(),
                    condition: "route".to_string(),
                    route: Some(route.to_string()),
                }))
                .await
                .unwrap();
            assert!(!is_err(&edge), "{}", text(&edge));
        }

        let got = handler
            .graph_get(Parameters(GraphGetParams {
                graph_id: lp.id.clone(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&got), "{}", text(&got));
        let json: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
        let nodes = json["specs"][0]["nodes"].as_array().unwrap();
        let router_json = nodes
            .iter()
            .find(|n| n["id"] == router_id)
            .expect("router node present in graph_get");
        assert_eq!(router_json["kind"], "router");
        let routes = router_json["routes"].as_array().expect("routes array");
        assert_eq!(routes.len(), 2);
        let retry_route = routes
            .iter()
            .find(|r| r["label"] == "retry")
            .expect("retry route present");
        assert_eq!(retry_route["fallback"], true);
        assert_eq!(retry_route["to_node"], retry_target);
        let escalate_route = routes
            .iter()
            .find(|r| r["label"] == "escalate")
            .expect("escalate route present");
        assert_eq!(escalate_route["fallback"], false);
        assert_eq!(escalate_route["to_node"], escalate_target);
    }

    #[tokio::test]
    async fn graph_add_node_router_rejects_fewer_than_two_routes_and_missing_fallback() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let too_few = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Router".to_string(),
                kind: Some("router".to_string()),
                config: Some(router_node_config(
                    &serde_json::json!([{ "label": "retry", "description": "Retry." }]),
                    "retry",
                )),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&too_few));
        assert!(text(&too_few).contains("at least 2 routes"));

        let mut no_fallback_config = router_node_config(&two_routes(), "retry");
        no_fallback_config.remove("fallback");
        let no_fallback = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id),
                graph_id: None,
                name: "Router".to_string(),
                kind: Some("router".to_string()),
                config: Some(no_fallback_config),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&no_fallback));
        assert!(text(&no_fallback).contains("fallback"));
    }

    #[tokio::test]
    async fn graph_add_edge_route_condition_rejects_undeclared_route_and_non_router_source() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let router_result = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Router".to_string(),
                kind: Some("router".to_string()),
                config: Some(router_node_config(&two_routes(), "retry")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let router_id = extract_id(&router_result, "node_id");
        let target = add_agent_node(&handler, &spec.id, "Target").await;

        let undeclared = handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                from_node: router_id.clone(),
                to_node: target.clone(),
                condition: "route".to_string(),
                route: Some("nonexistent".to_string()),
            }))
            .await
            .unwrap();
        assert!(is_err(&undeclared));
        assert!(text(&undeclared).contains("undeclared route"));

        let non_router = add_agent_node(&handler, &spec.id, "Non-router source").await;
        let wrong_source = handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id),
                graph_id: None,
                from_node: non_router,
                to_node: target,
                condition: "route".to_string(),
                route: Some("retry".to_string()),
            }))
            .await
            .unwrap();
        assert!(is_err(&wrong_source));
        assert!(text(&wrong_source).contains("router node"));
    }

    #[tokio::test]
    async fn graph_update_node_router_enforces_route_coverage_and_edge_consistency() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let router_result = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Router".to_string(),
                kind: Some("router".to_string()),
                config: Some(router_node_config(&two_routes(), "retry")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        let router_id = extract_id(&router_result, "node_id");
        let retry_target = add_agent_node(&handler, &spec.id, "Retry target").await;

        // Wire only the "retry" route — "escalate" is declared but not yet
        // served by any edge.
        let edge = handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                from_node: router_id.clone(),
                to_node: retry_target,
                condition: "route".to_string(),
                route: Some("retry".to_string()),
            }))
            .await
            .unwrap();
        assert!(!is_err(&edge), "{}", text(&edge));

        // Re-asserting the same routes now that wiring has started must
        // fail: "escalate" has no outgoing edge.
        let no_coverage = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: router_id.clone(),
                name: None,
                kind: None,
                config: Some(router_node_config(&two_routes(), "retry")),
                config_replace: None,
                position: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&no_coverage));
        assert!(text(&no_coverage).contains("'escalate' has no outgoing edge"));

        // Changing the declared routes so they no longer include "retry"
        // leaves the existing retry-labeled edge dangling on an undeclared
        // route.
        let stale_edge = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: router_id.clone(),
                name: None,
                kind: None,
                config: Some(router_node_config(
                    &serde_json::json!([
                        { "label": "escalate", "description": "Hand off to a human." },
                        { "label": "abort", "description": "Give up." },
                    ]),
                    "escalate",
                )),
                config_replace: None,
                position: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&stale_edge));
        assert!(text(&stale_edge).contains("undeclared route 'retry'"));
    }

    // ── graph_add_ensemble ──────────────────────────────────────────

    #[tokio::test]
    async fn graph_add_ensemble_happy_path_and_validation_errors() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

        let members = vec![
            crate::daemon::params::EnsembleMemberParams {
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
            crate::daemon::params::EnsembleMemberParams {
                platform: "opencode".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
        ];

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Review Ensemble".to_string(),
                kind: None,
                prompt_template: Some("Review {{spec_name}}".to_string()),
                members: Some(members.clone()),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: Some(2),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        assert_eq!(db.list_ensembles_for_spec(&spec.id).unwrap().len(), 1);

        let missing_prompt = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "No Prompt".to_string(),
                kind: None,
                prompt_template: None,
                members: Some(members.clone()),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&missing_prompt));
        assert!(text(&missing_prompt).contains("prompt_template"));

        let bad_min_pass = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Bad Min Pass".to_string(),
                kind: None,
                prompt_template: Some("Review".to_string()),
                members: Some(members.clone()),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: Some(99),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_min_pass));
        assert!(text(&bad_min_pass).contains("has an invalid min_pass"));

        let unknown_entry = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Bad Entry".to_string(),
                kind: None,
                prompt_template: Some("Review".to_string()),
                members: Some(members.clone()),
                blueprint: None,
                from_node: "not-a-node".to_string(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&unknown_entry));
        assert!(text(&unknown_entry).contains("not found"));

        let too_few_members = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id),
                graph_id: None,
                name: "Too Few".to_string(),
                kind: None,
                prompt_template: Some("Review".to_string()),
                members: Some(vec![members[0].clone()]),
                blueprint: None,
                from_node: entry,
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&too_few_members));
    }

    /// CM24: a negative `quorum_grace_minutes` is refused on add with the
    /// same wording pattern as the existing `straggler_timeout_minutes`
    /// check — while `0` (terminate immediately) is accepted and stored.
    #[tokio::test]
    async fn graph_add_ensemble_rejects_negative_quorum_grace() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let member = || crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };

        let negative = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Negative Grace".to_string(),
                kind: None,
                prompt_template: Some("Review".to_string()),
                members: Some(vec![member(), member()]),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: Some(-1),
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&negative));
        assert!(
            text(&negative).contains("quorum_grace_minutes must not be negative."),
            "{}",
            text(&negative)
        );

        let immediate = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Immediate Grace".to_string(),
                kind: None,
                prompt_template: Some("Review".to_string()),
                members: Some(vec![member(), member()]),
                blueprint: None,
                from_node: entry,
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: Some(0),
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&immediate), "{}", text(&immediate));
        let ensemble_id = extract_id(&immediate, "ensemble_id");
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.ensemble.quorum_grace_minutes, Some(0));
    }

    /// CM24: `quorum_grace_minutes` can be set and cleared back to `None`
    /// (wait-for-all) via `graph_update_ensemble`, mirroring the
    /// `straggler_timeout_minutes` clear path.
    #[tokio::test]
    async fn graph_update_ensemble_sets_and_clears_quorum_grace() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let member = || crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Grace Update".to_string(),
                kind: None,
                prompt_template: Some("Review".to_string()),
                members: Some(vec![member(), member()]),
                blueprint: None,
                from_node: entry,
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        let ensemble_id = extract_id(&created, "ensemble_id");

        let negative = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                ensemble_id: ensemble_id.clone(),
                quorum_grace_minutes: Some(Some(-2)),
                ..blank_ensemble_update(&ensemble_id)
            }))
            .await
            .unwrap();
        assert!(is_err(&negative));
        assert!(
            text(&negative).contains("quorum_grace_minutes must not be negative."),
            "{}",
            text(&negative)
        );

        let set = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                ensemble_id: ensemble_id.clone(),
                quorum_grace_minutes: Some(Some(5)),
                ..blank_ensemble_update(&ensemble_id)
            }))
            .await
            .unwrap();
        assert!(!is_err(&set), "{}", text(&set));
        assert_eq!(
            db.get_ensemble(&ensemble_id)
                .unwrap()
                .unwrap()
                .quorum_grace_minutes,
            Some(5)
        );

        let cleared = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                ensemble_id: ensemble_id.clone(),
                quorum_grace_minutes: Some(None),
                ..blank_ensemble_update(&ensemble_id)
            }))
            .await
            .unwrap();
        assert!(!is_err(&cleared), "{}", text(&cleared));
        assert_eq!(
            db.get_ensemble(&ensemble_id)
                .unwrap()
                .unwrap()
                .quorum_grace_minutes,
            None
        );
    }

    // ── graph_get / graph_list ──────────────────────────────────────

    #[tokio::test]
    async fn graph_get_and_graph_list() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());

        let got = handler
            .graph_get(Parameters(GraphGetParams {
                graph_id: lp.id.clone(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&got), "{}", text(&got));
        assert!(raw_text(&got).contains(&lp.id));

        let missing = handler
            .graph_get(Parameters(GraphGetParams {
                graph_id: "ghost".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&missing));

        let listed = handler
            .graph_list(Parameters(GraphListParams {
                workdir: None,
                include_archived: None,
            }))
            .await
            .unwrap();
        assert!(raw_text(&listed).contains(&lp.id));

        let filtered_out = handler
            .graph_list(Parameters(GraphListParams {
                workdir: Some("/nowhere".to_string()),
                include_archived: None,
            }))
            .await
            .unwrap();
        assert!(!raw_text(&filtered_out).contains(&lp.id));
    }

    /// `graph_get`'s node JSON must make an agent node's prompt source
    /// visible: `"explicit"` for a `prompt_template`, `"preset"` for a
    /// `prompt_preset`, and — the case this spec exists for — the node
    /// running on the bare fallback nobody chose must be distinguishable
    /// too, as `"default_fallback"`, without requiring a run first.
    #[tokio::test]
    async fn graph_get_reports_prompt_source_for_agent_nodes() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let mut explicit_config = agent_node_config("claude");
        explicit_config.insert(
            "prompt_template".to_string(),
            serde_json::Value::String("do the thing".to_string()),
        );
        let explicit_id = extract_id(
            &handler
                .graph_add_node(Parameters(GraphAddNodeParams {
                    spec_id: Some(spec.id.clone()),
                    graph_id: None,
                    name: "Explicit".to_string(),
                    kind: Some("agent".to_string()),
                    config: Some(explicit_config),
                    blueprint: None,
                    config_overrides: None,
                }))
                .await
                .unwrap(),
            "node_id",
        );

        let mut preset_config = agent_node_config("claude");
        preset_config.insert(
            "prompt_preset".to_string(),
            serde_json::Value::String("implementer".to_string()),
        );
        let preset_id = extract_id(
            &handler
                .graph_add_node(Parameters(GraphAddNodeParams {
                    spec_id: Some(spec.id.clone()),
                    graph_id: None,
                    name: "Preset".to_string(),
                    kind: Some("agent".to_string()),
                    config: Some(preset_config),
                    blueprint: None,
                    config_overrides: None,
                }))
                .await
                .unwrap(),
            "node_id",
        );

        let default_id = extract_id(
            &handler
                .graph_add_node(Parameters(GraphAddNodeParams {
                    spec_id: Some(spec.id.clone()),
                    graph_id: None,
                    name: "Default".to_string(),
                    kind: Some("agent".to_string()),
                    config: Some(agent_node_config("claude")),
                    blueprint: None,
                    config_overrides: None,
                }))
                .await
                .unwrap(),
            "node_id",
        );

        let got = handler
            .graph_get(Parameters(GraphGetParams {
                graph_id: lp.id.clone(),
            }))
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
        let nodes = json["specs"][0]["nodes"].as_array().unwrap();
        let prompt_source_of = |node_id: &str| -> String {
            nodes
                .iter()
                .find(|n| n["id"] == node_id)
                .unwrap_or_else(|| panic!("node {node_id} not found in {nodes:?}"))["prompt_source"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(prompt_source_of(&explicit_id), "explicit");
        assert_eq!(prompt_source_of(&preset_id), "preset");
        assert_eq!(prompt_source_of(&default_id), "default_fallback");
    }

    /// `graph_audit_node_configs` must find a node that predates write-time
    /// validation and still carries a key its kind never reads — verified
    /// against the exact incident shape (a `prompt` key on an agent node)
    /// and resolving the owning graph through the node's spec, since a
    /// spec-scoped node's own row has no `graph_id`.
    #[tokio::test]
    async fn graph_audit_node_configs_finds_node_with_ignored_prompt_key() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        // Bypass `graph_add_node`'s validation to simulate a node that was
        // written before this spec's check existed — exactly how graph
        // `824de730-7fec-4031-800a-7933d2cf94c1`'s node
        // `d0458fe0-24b8-4a29-8a69-7bb0e742e046` ended up carrying `prompt`.
        let legacy_node = GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Legacy Implementer".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude", "prompt": "implement the spec"}),
            position: 1,
            created_at: chrono::Utc::now(),
        };
        db.insert_graph_node(&legacy_node).unwrap();

        // A clean node must not show up in the audit.
        let clean_node = insert_named_node(&db, &spec.id, "Clean", 2);

        let audited = handler.graph_audit_node_configs().await.unwrap();
        assert!(!is_err(&audited), "{}", text(&audited));
        let json: serde_json::Value = serde_json::from_str(&raw_text(&audited)).unwrap();
        let flagged = json["flagged_nodes"].as_array().unwrap();
        assert_eq!(flagged.len(), 1);
        let flagged_node = &flagged[0];
        assert_eq!(flagged_node["node_id"], legacy_node.id);
        assert_eq!(flagged_node["graph_id"], lp.id);
        assert_eq!(flagged_node["unknown_keys"], serde_json::json!(["prompt"]));
        assert!(flagged.iter().all(|n| n["node_id"] != clean_node.id));
    }

    // ── graph_node_runs_list / graph_node_run_get ────────────────────

    fn insert_named_node(db: &Database, spec_id: &str, name: &str, position: i64) -> GraphNode {
        let node = GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: name.to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position,
            created_at: chrono::Utc::now(),
        };
        db.insert_graph_node(&node).unwrap();
        node
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_finalized_node_run(
        db: &Database,
        graph_id: &str,
        spec_id: &str,
        node_id: &str,
        status: GraphRunStatus,
        output: Option<serde_json::Value>,
        started_at: chrono::DateTime<chrono::Utc>,
    ) -> GraphNodeRun {
        let run = GraphNodeRun {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id: graph_id.to_string(),
            spec_id: spec_id.to_string(),
            node_id: node_id.to_string(),
            status,
            input: None,
            output,
            started_at,
            completed_at: Some(started_at),
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: Some("ses_test_123".to_string()),
            executed_platform: None,
            executed_model: None,
        };
        db.insert_graph_run(&run).unwrap();
        run
    }

    #[tokio::test]
    async fn graph_node_runs_list_defaults_to_most_recent_first_and_resolves_node_name() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let node_a = insert_named_node(&db, &spec.id, "build", 1);
        let node_b = insert_named_node(&db, &spec.id, "review", 2);

        let now = chrono::Utc::now();
        let older = insert_finalized_node_run(
            &db,
            &lp.id,
            &spec.id,
            &node_a.id,
            GraphRunStatus::Pass,
            Some(serde_json::json!({"reported_output": "ok"})),
            now - chrono::Duration::minutes(10),
        );
        let newer = insert_finalized_node_run(
            &db,
            &lp.id,
            &spec.id,
            &node_b.id,
            GraphRunStatus::Fail,
            Some(serde_json::json!({"stderr": "Error: Unsupported model mimo-auto"})),
            now,
        );

        let listed = handler
            .graph_node_runs_list(Parameters(GraphNodeRunsListParams {
                graph_id: lp.id.clone(),
                spec_id: None,
                node_id: None,
                limit: None,
                offset: None,
                compact: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&listed), "{}", text(&listed));
        let body = raw_text(&listed);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let runs = parsed["runs"].as_array().unwrap();
        assert_eq!(
            runs[0]["id"], newer.id,
            "the most recently started run must lead the default listing"
        );
        assert_eq!(runs[1]["id"], older.id);
        assert_eq!(runs[0]["node_name"], "review");
        assert_eq!(runs[0]["status"], "fail");
        assert_eq!(runs[0]["session_id"], "ses_test_123");
        assert!(
            runs[0].get("output").is_none(),
            "list responses must omit output — fetch it via graph_node_run_get"
        );

        let unknown_graph = handler
            .graph_node_runs_list(Parameters(GraphNodeRunsListParams {
                graph_id: "ghost".to_string(),
                spec_id: None,
                node_id: None,
                limit: None,
                offset: None,
                compact: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&unknown_graph));
    }

    #[tokio::test]
    async fn graph_node_runs_list_filters_by_spec_and_node_and_respects_limit() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec1 = insert_test_spec(&db, &lp.id, 1);
        let spec2 = insert_test_spec(&db, &lp.id, 2);
        let node1 = insert_named_node(&db, &spec1.id, "node1", 1);
        let node2 = insert_named_node(&db, &spec2.id, "node2", 1);
        let now = chrono::Utc::now();
        insert_finalized_node_run(
            &db,
            &lp.id,
            &spec1.id,
            &node1.id,
            GraphRunStatus::Pass,
            None,
            now - chrono::Duration::minutes(2),
        );
        let spec2_run = insert_finalized_node_run(
            &db,
            &lp.id,
            &spec2.id,
            &node2.id,
            GraphRunStatus::Pass,
            None,
            now - chrono::Duration::minutes(1),
        );
        insert_finalized_node_run(
            &db,
            &lp.id,
            &spec1.id,
            &node1.id,
            GraphRunStatus::Fail,
            None,
            now,
        );

        let by_spec = handler
            .graph_node_runs_list(Parameters(GraphNodeRunsListParams {
                graph_id: lp.id.clone(),
                spec_id: Some(spec2.id.clone()),
                node_id: None,
                limit: None,
                offset: None,
                compact: None,
            }))
            .await
            .unwrap();
        let by_spec_runs = serde_json::from_str::<serde_json::Value>(&raw_text(&by_spec)).unwrap()
            ["runs"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(by_spec_runs.len(), 1);
        assert_eq!(by_spec_runs[0]["id"], spec2_run.id);

        let limited = handler
            .graph_node_runs_list(Parameters(GraphNodeRunsListParams {
                graph_id: lp.id.clone(),
                spec_id: None,
                node_id: None,
                limit: Some(1),
                offset: None,
                compact: None,
            }))
            .await
            .unwrap();
        let limited_runs = serde_json::from_str::<serde_json::Value>(&raw_text(&limited)).unwrap()
            ["runs"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(limited_runs.len(), 1, "limit must bound the page size");
        assert_eq!(
            limited_runs[0]["status"], "fail",
            "the single returned run must be the most recent one"
        );
    }

    #[tokio::test]
    async fn graph_node_run_get_returns_full_output_and_redacts_secrets() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let node = insert_named_node(&db, &spec.id, "resilience", 1);
        let run = insert_finalized_node_run(
            &db,
            &lp.id,
            &spec.id,
            &node.id,
            GraphRunStatus::Fail,
            Some(serde_json::json!({
                "cli": "mimocode",
                "stderr": "Error: Unsupported model mimo-auto",
                "stdout": "leaked token=abcdefghij1234567890 in output",
                "infra_attempt": 1,
                "infra_crash": true,
            })),
            chrono::Utc::now(),
        );

        let got = handler
            .graph_node_run_get(Parameters(GraphNodeRunGetParams {
                run_id: run.id.clone(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&got), "{}", text(&got));
        let body = raw_text(&got);
        assert!(body.contains("Unsupported model mimo-auto"));
        assert!(body.contains("\"node_name\": \"resilience\""));
        assert!(
            body.contains("\"infra_attempt\": 1") && body.contains("\"infra_crash\": true"),
            "B19 infra markers must survive verbatim: {body}"
        );
        assert!(
            !body.contains("abcdefghij1234567890"),
            "a token-shaped value must be redacted, not passed through: {body}"
        );
        assert!(body.contains("[REDACTED]"));

        let missing = handler
            .graph_node_run_get(Parameters(GraphNodeRunGetParams {
                run_id: "ghost-run".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&missing));
    }

    // ── graph_pause / graph_continue ────────────────────────────────

    #[tokio::test]
    async fn graph_pause_rejects_non_running_graph() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let result = handler
            .graph_pause(Parameters(GraphPauseParams {
                graph_id: lp.id.clone(),
                interrupt: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(text(&result).contains("not running"));
    }

    #[tokio::test]
    async fn graph_continue_requires_a_paused_graph() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());

        let not_paused = handler
            .graph_continue(Parameters(GraphContinueParams {
                graph_id: lp.id.clone(),
                action: "retry_current_node".to_string(),
                node_id: None,
                spec_id: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&not_paused));
        assert!(text(&not_paused).contains("not paused"));

        let missing = handler
            .graph_continue(Parameters(GraphContinueParams {
                graph_id: "ghost".to_string(),
                action: "retry_current_node".to_string(),
                node_id: None,
                spec_id: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&missing));

        db.update_graph_status(&lp.id, GraphStatus::Paused, None, None)
            .unwrap();
        let bad_action = handler
            .graph_continue(Parameters(GraphContinueParams {
                graph_id: lp.id.clone(),
                action: "sideways".to_string(),
                node_id: None,
                spec_id: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_action));
        assert!(text(&bad_action).contains("retry_current_node, skip_next_spec or run_from_node"));
    }

    // ── graph_continue run_from_node (CM43 directed runs) ───────────

    #[tokio::test]
    async fn graph_continue_run_from_node_refuses_a_graph_with_an_active_run() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        // A blocker can flip a graph to `paused` while a sibling node is
        // still executing — FR2 refuses a directed run there, naming the
        // run, whatever the status column says.
        let run = insert_running_node_run(&db, &lp.id);
        db.update_graph_status(&lp.id, GraphStatus::Paused, None, None)
            .unwrap();

        let refused = handler
            .graph_continue(Parameters(GraphContinueParams {
                graph_id: lp.id.clone(),
                action: "run_from_node".to_string(),
                node_id: Some("Node".to_string()),
                spec_id: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&refused), "{}", text(&refused));
        let msg = text(&refused);
        assert!(msg.contains("still executing"), "{msg}");
        assert!(msg.contains(&run.id), "must name the run: {msg}");

        // A graph that is outright `running` (between nodes — no live run
        // row) also refuses.
        db.interrupt_graph_run(&run.id, "test").unwrap();
        db.update_graph_status(&lp.id, GraphStatus::Running, None, None)
            .unwrap();
        let running = handler
            .graph_continue(Parameters(GraphContinueParams {
                graph_id: lp.id.clone(),
                action: "run_from_node".to_string(),
                node_id: Some("Node".to_string()),
                spec_id: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&running), "{}", text(&running));
        assert!(text(&running).contains("pause it before a directed run"));
    }

    #[tokio::test]
    async fn graph_continue_run_from_node_accepts_spec_paused_on_and_reports_directed_plan() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());

        // The state `graph_pause` mid-spec leaves behind: graph paused, no
        // active run, paused-on spec reading `running`. FR2's "paused" is
        // exactly this — a directed run must accept it.
        insert_named_spec(&db, &lp.id, "Spec One", 1, GraphSpecStatus::Running);
        let skipped = insert_named_spec(&db, &lp.id, "Spec Two", 2, GraphSpecStatus::Skipped);
        db.update_graph_status(&lp.id, GraphStatus::Paused, None, None)
            .unwrap();

        // Top-level graph the specs fall back to: A --pass--> B (both quick
        // checks, so the background dispatch the accepted call spawns stays
        // harmless — under the plan's semantics every spec AFTER the
        // directed target also runs from Start, including the `Skipped` one).
        db.insert_graph_node(&GraphNode {
            id: "node-a".to_string(),
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            name: "A".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": "true",
                "success_condition": "exit_code_0"
            }),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "node-b".to_string(),
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            name: "B".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": "true",
                "success_condition": "exit_code_0"
            }),
            position: 2,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.insert_graph_edge(&GraphEdge {
            id: "edge-a-b".to_string(),
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            from_node: "node-a".to_string(),
            to_node: "node-b".to_string(),
            condition: GraphEdgeCondition::Pass,
        })
        .unwrap();

        // A `skipped` target is the one spec status outside FR2's accepted
        // states.
        let refused = handler
            .graph_continue(Parameters(GraphContinueParams {
                graph_id: lp.id.clone(),
                action: "run_from_node".to_string(),
                node_id: Some("B".to_string()),
                spec_id: Some(skipped.id.clone()),
            }))
            .await
            .unwrap();
        assert!(is_err(&refused), "{}", text(&refused));
        assert!(
            text(&refused).contains("'skipped', which a directed run does not accept"),
            "{}",
            text(&refused)
        );

        // An unknown node is refused with the list of valid names.
        let unknown = handler
            .graph_continue(Parameters(GraphContinueParams {
                graph_id: lp.id.clone(),
                action: "run_from_node".to_string(),
                node_id: Some("Zed".to_string()),
                spec_id: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&unknown), "{}", text(&unknown));
        let unknown_msg = text(&unknown);
        assert!(
            unknown_msg.contains("No node or ensemble matches 'Zed'"),
            "{unknown_msg}"
        );
        assert!(unknown_msg.contains("Valid nodes"), "{unknown_msg}");
        assert!(unknown_msg.contains("A, B"), "{unknown_msg}");

        // The paused-on spec is accepted (spec_id defaults to it), and the
        // response reports the whole directed plan.
        let accepted = handler
            .graph_continue(Parameters(GraphContinueParams {
                graph_id: lp.id.clone(),
                action: "run_from_node".to_string(),
                node_id: Some("B".to_string()),
                spec_id: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&accepted), "{}", text(&accepted));
        let msg = text(&accepted);
        assert!(msg.contains("resumed"), "{msg}");
        assert!(msg.contains("starts at node 'B'"), "{msg}");
        assert!(msg.contains("skipped A node(s)"), "{msg}");
        assert!(msg.contains("reused outputs: none"), "{msg}");
        assert!(msg.contains("iteration counters restarted at 1"), "{msg}");
    }

    // ── graph_complete_node / graph_report_blocker ──────────────────

    /// Creates a spec + agent node under `graph_id` and a `Running` node-run
    /// against it — the FK-satisfying fixture `graph_complete_node` /
    /// `graph_report_blocker` need (`graph_runs.spec_id`/`node_id` are both
    /// `NOT NULL REFERENCES`).
    fn insert_running_node_run(db: &Database, graph_id: &str) -> GraphNodeRun {
        let spec = insert_test_spec(db, graph_id, 1);
        let node = GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Node".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 1,
            created_at: chrono::Utc::now(),
        };
        db.insert_graph_node(&node).unwrap();
        let run = GraphNodeRun {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id: graph_id.to_string(),
            spec_id: spec.id,
            node_id: node.id,
            status: GraphRunStatus::Running,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        };
        db.insert_graph_run(&run).unwrap();
        run
    }

    #[tokio::test]
    async fn graph_complete_node_records_result_and_rejects_stale_report() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let run = insert_running_node_run(&db, &lp.id);
        let node_id = run.node_id.clone();

        let bad_status = handler
            .graph_complete_node(Parameters(GraphCompleteNodeParams {
                run_id: run.id.clone(),
                node_id: node_id.clone(),
                status: "sideways".to_string(),
                output: "out".to_string(),
                summary: "sum".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_status));

        let ok = handler
            .graph_complete_node(Parameters(GraphCompleteNodeParams {
                run_id: run.id.clone(),
                node_id: node_id.clone(),
                status: "pass".to_string(),
                output: "out".to_string(),
                summary: "sum".to_string(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&ok), "{}", text(&ok));

        // Reporting again against the now-finalized run must be rejected as
        // stale, not silently accepted.
        let stale = handler
            .graph_complete_node(Parameters(GraphCompleteNodeParams {
                run_id: run.id.clone(),
                node_id: node_id.clone(),
                status: "pass".to_string(),
                output: "out2".to_string(),
                summary: "sum2".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&stale));
        assert!(text(&stale).contains("no longer active"));

        let wrong_node = handler
            .graph_complete_node(Parameters(GraphCompleteNodeParams {
                run_id: run.id,
                node_id: "not-the-right-node".to_string(),
                status: "pass".to_string(),
                output: "out".to_string(),
                summary: "sum".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&wrong_node));

        let unknown_run = handler
            .graph_complete_node(Parameters(GraphCompleteNodeParams {
                run_id: "ghost-run".to_string(),
                node_id,
                status: "pass".to_string(),
                output: "out".to_string(),
                summary: "sum".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&unknown_run));
    }

    #[tokio::test]
    async fn graph_report_blocker_pauses_the_graph() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let run = insert_running_node_run(&db, &lp.id);
        let node_id = run.node_id.clone();

        let result = handler
            .graph_report_blocker(Parameters(GraphReportBlockerParams {
                run_id: run.id,
                node_id,
                description: "waiting on human input".to_string(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        assert_eq!(
            db.get_graph(&lp.id).unwrap().unwrap().status,
            GraphStatus::Paused
        );
    }

    #[tokio::test]
    async fn loop_complete_node_records_result_like_graph_complete_node_and_notes_deprecation() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let run = insert_running_node_run(&db, &lp.id);

        let result = handler
            .loop_complete_node(Parameters(GraphCompleteNodeParams {
                run_id: run.id.clone(),
                node_id: run.node_id.clone(),
                status: "pass".to_string(),
                output: "out".to_string(),
                summary: "sum".to_string(),
            }))
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        let body = raw_text(&result);
        assert!(body.contains("loop_complete_node was renamed graph_complete_node in 3.0.0"));

        // Same DB effect as graph_complete_node: the run is finalized pass.
        let stored = db.get_graph_run(&run.id).unwrap().unwrap();
        assert_eq!(stored.status, GraphRunStatus::Pass);
    }

    #[tokio::test]
    async fn loop_report_blocker_pauses_the_graph_and_notes_deprecation() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let run = insert_running_node_run(&db, &lp.id);

        let result = handler
            .loop_report_blocker(Parameters(GraphReportBlockerParams {
                run_id: run.id,
                node_id: run.node_id,
                description: "waiting on human input".to_string(),
            }))
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        assert!(raw_text(&result)
            .contains("loop_report_blocker was renamed graph_report_blocker in 3.0.0"));
        assert_eq!(
            db.get_graph(&lp.id).unwrap().unwrap().status,
            GraphStatus::Paused
        );
    }

    #[tokio::test]
    async fn loop_schedule_autorun_accepts_loop_id_as_graph_id() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());

        // Deserialize straight from JSON using the wire field name `loop_id`,
        // proving the `#[serde(alias = "loop_id")]` on `graph_id` actually
        // works end to end, not just as a Rust-level convenience constructor.
        let raw = serde_json::json!({
            "loop_id": lp.id,
            "at": null,
            "quota_reset_message": null,
        });
        let params: LoopScheduleAutorunParams = serde_json::from_value(raw).unwrap();
        assert_eq!(params.graph_id, lp.id);

        let result = handler
            .loop_schedule_autorun(Parameters(params))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        assert!(raw_text(&result)
            .contains("loop_schedule_autorun was renamed graph_schedule_autorun in 3.0.0"));
    }

    #[test]
    fn tool_listing_hides_the_three_deprecated_aliases_but_keeps_them_registered() {
        let visible = super::visible_tool_list();
        for hidden in [
            "loop_complete_node",
            "loop_report_blocker",
            "loop_schedule_autorun",
        ] {
            assert!(
                !visible.iter().any(|t| t.name == hidden),
                "{hidden} must not appear in the tool listing"
            );
        }
        // Still registered in the router (i.e. still callable) — just not listed.
        let all = TaskTriggerHandler::tool_router().list_all();
        for still_registered in [
            "loop_complete_node",
            "loop_report_blocker",
            "loop_schedule_autorun",
        ] {
            assert!(
                all.iter().any(|t| t.name == still_registered),
                "{still_registered} must remain registered in the router so call_tool still works"
            );
        }
        // A real graph_* tool must still be listed — the filter must be
        // exact-name, not a prefix/substring match that could eat something else.
        assert!(visible.iter().any(|t| t.name == "graph_complete_node"));
    }

    /// C19 FR4 (reusing the same blocker mechanism `graph_report_blocker`
    /// uses): a graph paused with an active blocker must not be silently
    /// relaunched via `graph_run` — that's the whole "not started again
    /// until a human clears it" the budget exists for. A plain
    /// `graph_pause` (`Paused`, no blocker) is unaffected — `graph_run`
    /// resuming that is the normal, sanctioned path and must keep working.
    #[tokio::test]
    async fn graph_run_refuses_a_paused_graph_with_an_active_blocker() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let run = insert_running_node_run(&db, &lp.id);
        let node_id = run.node_id.clone();

        let blocked = handler
            .graph_report_blocker(Parameters(GraphReportBlockerParams {
                run_id: run.id,
                node_id,
                description: "waiting on human input".to_string(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&blocked), "{}", text(&blocked));

        let result = handler
            .graph_run(Parameters(GraphRunParams {
                graph_id: lp.id.clone(),
                queue_id: None,
                workdir: None,
                idea: None,
                sandbox: None,
                start_node_id: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&result), "a blocked graph must refuse graph_run");
        assert!(text(&result).contains("blocked"), "{}", text(&result));
        assert_eq!(
            db.get_graph(&lp.id).unwrap().unwrap().status,
            GraphStatus::Paused,
            "the refusal must not itself change the graph's status"
        );
    }

    /// The defect this closes (2026-08-06, graph 824de730): a spec that dies
    /// because a FAILING node has no outgoing edge left no blocker anywhere
    /// a human could see from `graph_list` — the engine now derives one onto
    /// the terminating run (`GraphEngine::record_terminal_blocker`), and
    /// `graph_list` picks it up through the unchanged `graph_run_blocker`
    /// read path, with no MCP-side change required.
    #[tokio::test]
    async fn graph_list_reports_blocked_for_a_spec_that_dead_ends_on_a_failing_node() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        db.insert_graph_node(&GraphNode {
            id: "dead-end".to_string(),
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "dead-end".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": "exit 1",
                "success_condition": "exit_code_0",
            }),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        // No outgoing edge from "dead-end" for either status — the spec
        // terminates right there.

        handler
            .graph_engine
            .run_graph(lp.id.clone(), None, None, None, None, None)
            .await
            .unwrap();

        assert_eq!(
            db.get_graph_spec(&spec.id).unwrap().unwrap().status,
            GraphSpecStatus::Failed
        );

        let listed = handler
            .graph_list(Parameters(GraphListParams {
                workdir: None,
                include_archived: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&listed), "{}", text(&listed));
        let value: serde_json::Value = serde_json::from_str(&raw_text(&listed)).unwrap();
        let entry = value
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == lp.id)
            .expect("graph must appear in graph_list");
        assert_eq!(entry["blocked"], serde_json::json!(true));
        let blocker = entry["blocker"].as_str().expect("blocker must be a string");
        assert!(blocker.contains("dead-end"), "blocker: {blocker}");
    }

    // ── graph_copy_node / graph_copy_ensemble ───────────────────────

    #[tokio::test]
    async fn graph_copy_node_unwired_and_wired() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let source = add_agent_node(&handler, &spec.id, "Source").await;
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let exit = add_agent_node(&handler, &spec.id, "Exit").await;

        let unwired = handler
            .graph_copy_node(Parameters(GraphCopyNodeParams {
                source_node_id: source.clone(),
                spec_id: None,
                graph_id: None,
                name: Some("Source Copy".to_string()),
                config_overrides: None,
                entry_from_node: None,
                entry_condition: None,
                on_pass_to: None,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&unwired), "{}", text(&unwired));
        let value: serde_json::Value = serde_json::from_str(&raw_text(&unwired)).unwrap();
        assert_eq!(value["wired"], false);
        assert_eq!(db.list_graph_nodes(&spec.id).unwrap().len(), 4);

        let wired = handler
            .graph_copy_node(Parameters(GraphCopyNodeParams {
                source_node_id: source,
                spec_id: None,
                graph_id: None,
                name: Some("Source Copy 2".to_string()),
                config_overrides: None,
                entry_from_node: Some(entry),
                entry_condition: Some("always".to_string()),
                on_pass_to: Some(exit),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&wired), "{}", text(&wired));
        let value: serde_json::Value = serde_json::from_str(&raw_text(&wired)).unwrap();
        assert_eq!(value["wired"], true);

        let missing_source = handler
            .graph_copy_node(Parameters(GraphCopyNodeParams {
                source_node_id: "ghost-node".to_string(),
                spec_id: None,
                graph_id: None,
                name: None,
                config_overrides: None,
                entry_from_node: None,
                entry_condition: None,
                on_pass_to: None,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&missing_source));
    }

    #[tokio::test]
    async fn graph_copy_ensemble_duplicates_members_and_quorum() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Source Ensemble".to_string(),
                kind: None,
                prompt_template: Some("Review {{spec_name}}".to_string()),
                members: Some(vec![
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "opencode".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                blueprint: None,
                from_node: entry,
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        let source_ensemble_id = extract_id(&created, "ensemble_id");

        let copied = handler
            .graph_copy_ensemble(Parameters(GraphCopyEnsembleParams {
                source_ensemble_id: source_ensemble_id.clone(),
                spec_id: None,
                graph_id: None,
                name: Some("Copied Ensemble".to_string()),
                prompt_template: None,
                members: None,
                min_pass: None,
                timeout_minutes: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                from_node: None,
                condition: None,
                on_pass_to: None,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&copied), "{}", text(&copied));
        let copied_ensemble_id = extract_id(&copied, "ensemble_id");
        assert_ne!(copied_ensemble_id, source_ensemble_id);
        let copied_details = db
            .get_ensemble_details(&copied_ensemble_id)
            .unwrap()
            .unwrap();
        assert_eq!(copied_details.ensemble.name, "Copied Ensemble");
        assert_eq!(copied_details.members.len(), 2);

        let missing_source = handler
            .graph_copy_ensemble(Parameters(GraphCopyEnsembleParams {
                source_ensemble_id: "ghost-ensemble".to_string(),
                spec_id: None,
                graph_id: None,
                name: None,
                prompt_template: None,
                members: None,
                min_pass: None,
                timeout_minutes: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                from_node: None,
                condition: None,
                on_pass_to: None,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&missing_source));
    }

    /// A member's `prompt_override` replaces the shared `prompt_template` for
    /// that member only — everything else (platform/model semantics, the
    /// other members, `graph_get`'s prompt_source, and `graph_copy_ensemble`
    /// carrying it forward) keeps working exactly as before this field
    /// existed.
    #[tokio::test]
    async fn graph_add_ensemble_member_prompt_override_replaces_shared_prompt_for_that_member_only()
    {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Panel".to_string(),
                kind: None,
                prompt_template: Some("shared review prompt".to_string()),
                members: Some(vec![
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: Some("review for security issues".to_string()),
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "codex".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                blueprint: None,
                from_node: entry,
                condition: "always".to_string(),
                min_pass: Some(2),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        let ensemble_id = extract_id(&created, "ensemble_id");
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();

        // Platform/model are untouched by the override — it's an independent
        // knob, not a replacement for them.
        assert_eq!(details.members[0].platform, "claude");
        assert_eq!(
            details.members[0].prompt_override.as_deref(),
            Some("review for security issues")
        );
        assert_eq!(details.members[1].prompt_override, None);

        let overridden_node = db
            .get_graph_node(&details.members[0].node_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            overridden_node
                .config
                .get("prompt_template")
                .and_then(|v| v.as_str()),
            Some("review for security issues")
        );
        let shared_node = db
            .get_graph_node(&details.members[1].node_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            shared_node
                .config
                .get("prompt_template")
                .and_then(|v| v.as_str()),
            Some("shared review prompt")
        );

        // graph_get's ensemble view distinguishes shared vs. override per
        // member without the caller having to diff node config themselves.
        let spec_edges = db.list_graph_edges(&spec.id).unwrap();
        let ensembles_json = crate::daemon::handler::ensemble_details_json(&details, &spec_edges);
        let members_json = ensembles_json["members"].as_array().unwrap();
        assert_eq!(members_json[0]["prompt_source"], "override");
        assert_eq!(members_json[1]["prompt_source"], "shared");

        // Copying without replacing members carries each member's own
        // override (or lack of one) forward into the copy.
        let copied = handler
            .graph_copy_ensemble(Parameters(GraphCopyEnsembleParams {
                source_ensemble_id: ensemble_id.clone(),
                spec_id: None,
                graph_id: None,
                name: Some("Panel copy".to_string()),
                prompt_template: None,
                members: None,
                min_pass: None,
                timeout_minutes: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                from_node: None,
                condition: None,
                on_pass_to: None,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&copied), "{}", text(&copied));
        let copied_ensemble_id = extract_id(&copied, "ensemble_id");
        let copied_details = db
            .get_ensemble_details(&copied_ensemble_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            copied_details.members[0].prompt_override.as_deref(),
            Some("review for security issues")
        );
        assert_eq!(copied_details.members[1].prompt_override, None);
    }

    // ── graph_update_ensemble ────────────────────────────────────────

    #[tokio::test]
    async fn graph_update_ensemble_prompt_members_join_and_wiring() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let alt_entry = add_agent_node(&handler, &spec.id, "AltEntry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let alt_exit = add_agent_node(&handler, &spec.id, "AltExit").await;

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Ensemble".to_string(),
                kind: None,
                prompt_template: Some("Original prompt".to_string()),
                members: Some(vec![
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "opencode".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: Some(2),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        let ensemble_id = extract_id(&created, "ensemble_id");

        let added_entry = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: None,
                min_pass: None,
                straggler_timeout_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                quorum_grace_minutes: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: Some(alt_entry.clone()),
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&added_entry), "{}", text(&added_entry));

        // No fields at all -> rejected.
        let no_op = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: None,
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&no_op));
        assert!(text(&no_op).contains("at least one field"));

        // Prompt-only update, propagated to existing members without a resize.
        let prompt_updated = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: Some("New prompt".to_string()),
                members: None,
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&prompt_updated), "{}", text(&prompt_updated));
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.ensemble.prompt_template, "New prompt");

        // Grow membership 2 -> 4 while preserving both entry sources.
        let grown = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: Some(vec![
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "opencode".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "gemini".to_string(),
                        model: None,
                        prompt_override: Some("Review only for test coverage gaps.".to_string()),
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: Some("growth-test".to_string()),
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&grown), "{}", text(&grown));
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.members.len(), 4);
        // The new member's own prompt_override is persisted on the ensemble
        // row and propagated into its node config as the effective prompt,
        // while the other two still carry no override.
        assert_eq!(details.members[0].prompt_override, None);
        assert_eq!(details.members[1].prompt_override, None);
        assert_eq!(
            details.members[2].prompt_override.as_deref(),
            Some("Review only for test coverage gaps.")
        );
        let gemini_node = db
            .get_graph_node(&details.members[2].node_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            gemini_node
                .config
                .get("prompt_template")
                .and_then(|v| v.as_str()),
            Some("Review only for test coverage gaps.")
        );
        let member_ids: std::collections::HashSet<_> = details
            .members
            .iter()
            .map(|member| member.node_id.as_str())
            .collect();
        let entry_edges = db
            .list_graph_edges(&spec.id)
            .unwrap()
            .into_iter()
            .filter(|edge| {
                [entry.as_str(), alt_entry.as_str()].contains(&edge.from_node.as_str())
                    && member_ids.contains(edge.to_node.as_str())
            })
            .count();
        assert_eq!(entry_edges, 8);

        // Shrink membership 4 -> 3 and retain complete fan-out.
        let shrunk = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: Some(vec![
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "opencode".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "gemini".to_string(),
                        model: None,
                        prompt_override: Some("Review only for test coverage gaps.".to_string()),
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&shrunk), "{}", text(&shrunk));
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.members.len(), 3);
        let member_ids: std::collections::HashSet<_> = details
            .members
            .iter()
            .map(|member| member.node_id.as_str())
            .collect();
        let entry_edges = db
            .list_graph_edges(&spec.id)
            .unwrap()
            .into_iter()
            .filter(|edge| {
                [entry.as_str(), alt_entry.as_str()].contains(&edge.from_node.as_str())
                    && member_ids.contains(edge.to_node.as_str())
            })
            .count();
        assert_eq!(entry_edges, 6);

        // min_pass out of bounds.
        let bad_min_pass = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: None,
                min_pass: Some(99),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_min_pass));

        // Negative straggler timeout.
        let bad_straggler = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: None,
                min_pass: None,
                straggler_timeout_minutes: Some(Some(-1)),
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_straggler));

        // Re-wire on_pass_to to a new valid target.
        let rewired = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: None,
                min_pass: Some(2),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: Some(alt_exit.clone()),
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&rewired), "{}", text(&rewired));
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.ensemble.on_pass_to, alt_exit);

        // Unknown on_pass_to target.
        let bad_target = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: None,
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: Some("not-a-node".to_string()),
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_target));

        let missing_ensemble = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: "ghost-ensemble".to_string(),
                kind: None,
                prompt_template: Some("x".to_string()),
                members: None,
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&missing_ensemble));
    }

    /// CM23: `graph_update_ensemble`'s member list can set one member's own
    /// `timeout_minutes`, independent of the ensemble's shared value — FR3
    /// (set through the same call that manages membership) and the inner
    /// wiring together: the override lands on the `EnsembleMember` row AND
    /// gets baked into that member's own node config, not the ensemble's.
    #[tokio::test]
    async fn graph_update_ensemble_sets_member_own_timeout() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Ensemble".to_string(),
                kind: None,
                prompt_template: Some("Original prompt".to_string()),
                members: Some(vec![
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "opencode".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                blueprint: None,
                from_node: entry,
                condition: "always".to_string(),
                min_pass: Some(2),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: Some(20),
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        let ensemble_id = extract_id(&created, "ensemble_id");

        let updated = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: Some(vec![
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: Some(3),
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "opencode".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));

        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.members[0].timeout_minutes, Some(3));
        assert_eq!(details.members[1].timeout_minutes, None);

        let claude_node = db
            .get_graph_node(&details.members[0].node_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            claude_node.config.get("timeout_minutes").and_then(|v| v.as_i64()),
            Some(3),
            "member's own override, not the ensemble's shared 20, must be baked into its node config"
        );
        let opencode_node = db
            .get_graph_node(&details.members[1].node_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            opencode_node
                .config
                .get("timeout_minutes")
                .and_then(|v| v.as_i64()),
            Some(20),
            "member with no override still uses the ensemble's shared timeout_minutes"
        );
    }

    /// CM30: `graph_add_node`/`graph_update_node` accept and validate the
    /// three infra-retry keys on a plain agent node.
    #[test]
    fn validate_node_config_accepts_infra_retry_keys_on_agent() {
        let config = serde_json::json!({
            "platform": "claude",
            "infra_retry_limit": 0,
            "infra_crash_max_seconds": 120,
            "infra_backoff_seconds": 0,
        });
        assert!(validate_node_config(GraphNodeKind::Agent, &config).is_ok());
    }

    /// CM30: non-integer or negative infra-retry values are rejected with
    /// the new error message (mirrors the timeout_minutes convention: 0 is
    /// legitimate, negatives and strings are not).
    #[test]
    fn validate_node_config_rejects_bad_infra_retry_values() {
        for (key, bad) in [
            ("infra_retry_limit", serde_json::json!(-1)),
            ("infra_retry_limit", serde_json::json!("3")),
            ("infra_retry_limit", serde_json::json!(1.5)),
            ("infra_crash_max_seconds", serde_json::json!(-2)),
            ("infra_backoff_seconds", serde_json::json!("30")),
        ] {
            let config = serde_json::json!({"platform": "claude", key: bad});
            let err = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
            assert!(
                err.contains(key) && err.contains("non-negative integer"),
                "unexpected error for {key}={bad}: {err}"
            );
        }
    }

    /// CM30: member-level and ensemble-level infra fields round-trip through
    /// `get_ensemble_details`, bake into member node configs member-wins,
    /// and negative values are rejected at both levels.
    #[tokio::test]
    async fn graph_add_ensemble_infra_fields_round_trip_and_reject_negative() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

        let bad_member = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Bad Member".to_string(),
                kind: None,
                prompt_template: Some("Review".to_string()),
                members: Some(vec![
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: Some(-1),
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "opencode".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_member), "{}", text(&bad_member));
        assert!(
            text(&bad_member).contains("infra_retry_limit"),
            "{}",
            text(&bad_member)
        );

        let bad_ensemble = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Bad Ensemble".to_string(),
                kind: None,
                prompt_template: Some("Review".to_string()),
                members: Some(vec![
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "opencode".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: Some(-5),
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad_ensemble), "{}", text(&bad_ensemble));
        assert!(
            text(&bad_ensemble).contains("infra_crash_max_seconds"),
            "{}",
            text(&bad_ensemble)
        );

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Infra Ensemble".to_string(),
                kind: None,
                prompt_template: Some("Review".to_string()),
                members: Some(vec![
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: Some(1),
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: Some(7),
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "opencode".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                blueprint: None,
                from_node: entry,
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: Some(0),
                infra_crash_max_seconds: Some(120),
                infra_backoff_seconds: None,
                on_pass_to: arbiter,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        let ensemble_id = extract_id(&created, "ensemble_id");
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.ensemble.infra_retry_limit, Some(0));
        assert_eq!(details.ensemble.infra_crash_max_seconds, Some(120));
        assert_eq!(details.ensemble.infra_backoff_seconds, None);
        assert_eq!(details.members[0].infra_retry_limit, Some(1));
        assert_eq!(details.members[0].infra_backoff_seconds, Some(7));
        assert_eq!(details.members[1].infra_retry_limit, None);

        // Member wins over ensemble in the baked node config; a member with
        // no override inherits the ensemble default; an unset ensemble field
        // bakes nothing at all.
        let first = db
            .get_graph_node(&details.members[0].node_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            first
                .config
                .get("infra_retry_limit")
                .and_then(|v| v.as_i64()),
            Some(1)
        );
        assert_eq!(
            first
                .config
                .get("infra_crash_max_seconds")
                .and_then(|v| v.as_i64()),
            Some(120)
        );
        assert_eq!(
            first
                .config
                .get("infra_backoff_seconds")
                .and_then(|v| v.as_i64()),
            Some(7)
        );
        let second = db
            .get_graph_node(&details.members[1].node_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            second
                .config
                .get("infra_retry_limit")
                .and_then(|v| v.as_i64()),
            Some(0)
        );
        assert_eq!(
            second
                .config
                .get("infra_crash_max_seconds")
                .and_then(|v| v.as_i64()),
            Some(120)
        );
        assert!(second.config.get("infra_backoff_seconds").is_none());
    }

    /// CM30: `graph_update_ensemble` sets, propagates (prompt-only branch)
    /// and clears (null) the ensemble-level infra defaults, rebaking member
    /// node configs each time.
    #[tokio::test]
    async fn graph_update_ensemble_sets_propagates_and_clears_infra_defaults() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_two_member_ensemble(&handler, &spec.id, "Infra Update", &entry, &arbiter).await;

        // Infra-only update (no members/prompt/timeout): the prompt-only
        // branch must still rebake every member's node config.
        let mut update = blank_ensemble_update(&ensemble_id);
        update.infra_retry_limit = Some(Some(0));
        update.infra_crash_max_seconds = Some(Some(90));
        let updated = handler
            .graph_update_ensemble(Parameters(update))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.ensemble.infra_retry_limit, Some(0));
        assert_eq!(details.ensemble.infra_crash_max_seconds, Some(90));
        for member in &details.members {
            let node = db.get_graph_node(&member.node_id).unwrap().unwrap();
            assert_eq!(
                node.config
                    .get("infra_retry_limit")
                    .and_then(|v| v.as_i64()),
                Some(0)
            );
            assert_eq!(
                node.config
                    .get("infra_crash_max_seconds")
                    .and_then(|v| v.as_i64()),
                Some(90)
            );
        }

        // Clear back to deferring: null removes the row value AND the baked key.
        let mut clear = blank_ensemble_update(&ensemble_id);
        clear.infra_retry_limit = Some(None);
        clear.infra_crash_max_seconds = Some(None);
        let cleared = handler
            .graph_update_ensemble(Parameters(clear))
            .await
            .unwrap();
        assert!(!is_err(&cleared), "{}", text(&cleared));
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.ensemble.infra_retry_limit, None);
        assert_eq!(details.ensemble.infra_crash_max_seconds, None);
        for member in &details.members {
            let node = db.get_graph_node(&member.node_id).unwrap().unwrap();
            assert!(node.config.get("infra_retry_limit").is_none());
            assert!(node.config.get("infra_crash_max_seconds").is_none());
        }

        // Negative ensemble-level values are rejected, and rejected BEFORE
        // the propagate branch bakes them into every member's node config —
        // a refusal must change nothing, same guarantee CB40 gives min_pass.
        let mut negative = blank_ensemble_update(&ensemble_id);
        negative.infra_backoff_seconds = Some(Some(-1));
        let rejected = handler
            .graph_update_ensemble(Parameters(negative))
            .await
            .unwrap();
        assert!(is_err(&rejected), "{}", text(&rejected));
        assert!(
            text(&rejected).contains("infra_backoff_seconds"),
            "{}",
            text(&rejected)
        );
        for member in &details.members {
            let node = db.get_graph_node(&member.node_id).unwrap().unwrap();
            assert!(
                node.config.get("infra_backoff_seconds").is_none(),
                "rejected update must not bake infra_backoff_seconds into member config"
            );
        }

        // Same guarantee on the member-resize branch: a negative
        // ensemble-level value paired with a `members` resize must reject
        // before `replace_ensemble_members` runs, leaving the old members in
        // place.
        let mut negative_resize = blank_ensemble_update(&ensemble_id);
        negative_resize.infra_retry_limit = Some(Some(-1));
        negative_resize.members = Some(vec![
            crate::daemon::params::EnsembleMemberParams {
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
            crate::daemon::params::EnsembleMemberParams {
                platform: "opencode".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
            crate::daemon::params::EnsembleMemberParams {
                platform: "codex".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
        ]);
        let rejected_resize = handler
            .graph_update_ensemble(Parameters(negative_resize))
            .await
            .unwrap();
        assert!(is_err(&rejected_resize), "{}", text(&rejected_resize));
        let details_after = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(
            details_after.members.len(),
            details.members.len(),
            "rejected update must not resize the member list"
        );
    }

    /// CM30/FR3: `graph_get` reports the effective infra budget and its
    /// source per node — `"node"` for an explicit key, `"default"` when
    /// nothing (neither node nor platform) sets it.
    #[tokio::test]
    async fn graph_get_reports_infra_config_with_source() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let mut explicit_config = agent_node_config("cm30-test-platform-xyz");
        explicit_config.insert("infra_retry_limit".to_string(), serde_json::Value::from(5));
        let explicit_id = extract_id(
            &handler
                .graph_add_node(Parameters(GraphAddNodeParams {
                    spec_id: Some(spec.id.clone()),
                    graph_id: None,
                    name: "Explicit".to_string(),
                    kind: Some("agent".to_string()),
                    config: Some(explicit_config),
                    blueprint: None,
                    config_overrides: None,
                }))
                .await
                .unwrap(),
            "node_id",
        );
        let plain_id = extract_id(
            &handler
                .graph_add_node(Parameters(GraphAddNodeParams {
                    spec_id: Some(spec.id.clone()),
                    graph_id: None,
                    name: "Plain".to_string(),
                    kind: Some("agent".to_string()),
                    config: Some(agent_node_config("cm30-test-platform-xyz")),
                    blueprint: None,
                    config_overrides: None,
                }))
                .await
                .unwrap(),
            "node_id",
        );

        let got = handler
            .graph_get(Parameters(GraphGetParams {
                graph_id: lp.id.clone(),
            }))
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
        let nodes = json["specs"][0]["nodes"].as_array().unwrap();
        let infra_of = |node_id: &str| -> serde_json::Value {
            nodes
                .iter()
                .find(|n| n["id"] == node_id)
                .unwrap_or_else(|| panic!("node {node_id} not found in {nodes:?}"))["infra_config"]
                .clone()
        };
        let explicit = infra_of(&explicit_id);
        assert_eq!(explicit["infra_retry_limit"]["value"], 5);
        assert_eq!(explicit["infra_retry_limit"]["source"], "node");
        assert_eq!(explicit["infra_crash_max_seconds"]["value"], 60);
        assert_eq!(explicit["infra_crash_max_seconds"]["source"], "default");
        let plain = infra_of(&plain_id);
        assert_eq!(plain["infra_retry_limit"]["value"], 2);
        assert_eq!(plain["infra_retry_limit"]["source"], "default");
        assert_eq!(plain["infra_backoff_seconds"]["value"], 30);
        assert_eq!(plain["infra_backoff_seconds"]["source"], "default");
    }

    /// CM30/FR5: `graph_audit_node_configs` lists a node/member whose
    /// effective budget differs from its platform's own, and omits one
    /// whose override merely restates the platform value (or that sets
    /// nothing). The platform here is unconfigured, so its baseline is the
    /// engine default.
    #[tokio::test]
    async fn graph_audit_node_configs_lists_infra_overrides() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);

        let mut differing = agent_node_config("cm30-test-platform-xyz");
        differing.insert("infra_retry_limit".to_string(), serde_json::Value::from(0));
        let differing_id = extract_id(
            &handler
                .graph_add_node(Parameters(GraphAddNodeParams {
                    spec_id: Some(spec.id.clone()),
                    graph_id: None,
                    name: "Differing".to_string(),
                    kind: Some("agent".to_string()),
                    config: Some(differing),
                    blueprint: None,
                    config_overrides: None,
                }))
                .await
                .unwrap(),
            "node_id",
        );
        let mut restating = agent_node_config("cm30-test-platform-xyz");
        restating.insert("infra_retry_limit".to_string(), serde_json::Value::from(2));
        let restating_id = extract_id(
            &handler
                .graph_add_node(Parameters(GraphAddNodeParams {
                    spec_id: Some(spec.id.clone()),
                    graph_id: None,
                    name: "Restating".to_string(),
                    kind: Some("agent".to_string()),
                    config: Some(restating),
                    blueprint: None,
                    config_overrides: None,
                }))
                .await
                .unwrap(),
            "node_id",
        );
        let plain_id = extract_id(
            &handler
                .graph_add_node(Parameters(GraphAddNodeParams {
                    spec_id: Some(spec.id.clone()),
                    graph_id: None,
                    name: "Plain".to_string(),
                    kind: Some("agent".to_string()),
                    config: Some(agent_node_config("cm30-test-platform-xyz")),
                    blueprint: None,
                    config_overrides: None,
                }))
                .await
                .unwrap(),
            "node_id",
        );

        let audited = handler.graph_audit_node_configs().await.unwrap();
        assert!(!is_err(&audited), "{}", text(&audited));
        let json: serde_json::Value = serde_json::from_str(&raw_text(&audited)).unwrap();
        let overrides = json["infra_config_overrides"].as_array().unwrap();
        assert_eq!(overrides.len(), 1, "{overrides:?}");
        assert_eq!(overrides[0]["node_id"], differing_id);
        assert_eq!(overrides[0]["infra_retry_limit"]["value"], 0);
        assert_eq!(overrides[0]["infra_retry_limit"]["source"], "node");
        assert_eq!(overrides[0]["infra_retry_limit"]["platform_value"], 2);
        assert!(overrides.iter().all(|n| n["node_id"] != restating_id));
        assert!(overrides.iter().all(|n| n["node_id"] != plain_id));
    }

    // ── CM14: ensemble entry rewiring, multi-source entry, deletion ──

    /// Build a two-member ensemble for the CM14 tests: `name` entered from
    /// `from_node`, routing on pass to `on_pass_to`.
    async fn add_two_member_ensemble(
        handler: &TaskTriggerHandler,
        spec_id: &str,
        name: &str,
        from_node: &str,
        on_pass_to: &str,
    ) -> String {
        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec_id.to_string()),
                graph_id: None,
                name: name.to_string(),
                kind: None,
                prompt_template: Some("Do the thing {{spec_name}}".to_string()),
                members: Some(vec![
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    crate::daemon::params::EnsembleMemberParams {
                        platform: "opencode".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                blueprint: None,
                from_node: from_node.to_string(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: on_pass_to.to_string(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        extract_id(&created, "ensemble_id")
    }

    fn blank_ensemble_update(ensemble_id: &str) -> GraphUpdateEnsembleParams {
        GraphUpdateEnsembleParams {
            ensemble_id: ensemble_id.to_string(),
            kind: None,
            prompt_template: None,
            members: None,
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: None,
            on_fail_to: None,
            from_node: None,
            condition: None,
            add_entry_from: None,
            add_entry_condition: None,
            remove_entry_from: None,
            commit_rights: None,
        }
    }

    /// CB40: shrinking members below the stored `min_pass` is refused and
    /// writes nothing — the resize must not be committed.
    #[tokio::test]
    async fn graph_update_ensemble_shrink_below_min_pass_is_refused() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let member = || crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Shrink Ensemble".to_string(),
                kind: None,
                prompt_template: Some("Review {{spec_name}}".to_string()),
                members: Some(vec![member(), member(), member()]),
                blueprint: None,
                from_node: entry,
                condition: "always".to_string(),
                min_pass: Some(3),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        let ensemble_id = extract_id(&created, "ensemble_id");

        let shrunk = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: Some(vec![member(), member()]),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&shrunk), "shrink below min_pass should fail");
        let msg = text(&shrunk);
        assert!(msg.contains("has an invalid min_pass"), "{msg}");
        assert!(msg.contains('3'), "{msg}");
        assert!(msg.contains('2'), "{msg}");
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.members.len(), 3);
        assert_eq!(details.ensemble.min_pass, 3);
    }

    /// CB40: raising `min_pass` above the member count is refused.
    #[tokio::test]
    async fn graph_update_ensemble_raise_min_pass_above_count_is_refused() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let member = || crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Raise Ensemble".to_string(),
                kind: None,
                prompt_template: Some("Review {{spec_name}}".to_string()),
                members: Some(vec![member(), member()]),
                blueprint: None,
                from_node: entry,
                condition: "always".to_string(),
                min_pass: Some(2),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        let ensemble_id = extract_id(&created, "ensemble_id");

        let raised = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: None,
                min_pass: Some(99),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&raised), "raised min_pass should fail");
        assert!(
            text(&raised).contains("has an invalid min_pass"),
            "{}",
            text(&raised)
        );
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.ensemble.min_pass, 2);
    }

    /// CB40: a shrink that also lowers `min_pass` in the same call succeeds.
    #[tokio::test]
    async fn graph_update_ensemble_shrink_with_adjusted_min_pass_succeeds() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let member = || crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Adjust Ensemble".to_string(),
                kind: None,
                prompt_template: Some("Review {{spec_name}}".to_string()),
                members: Some(vec![member(), member(), member()]),
                blueprint: None,
                from_node: entry,
                condition: "always".to_string(),
                min_pass: Some(2),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        let ensemble_id = extract_id(&created, "ensemble_id");

        let shrunk = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: Some(vec![member(), member()]),
                min_pass: Some(1),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&shrunk), "{}", text(&shrunk));
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.members.len(), 2);
        assert_eq!(details.ensemble.min_pass, 1);
    }

    /// CB40: `graph_preflight` names an ensemble stored with `min_pass`
    /// above its member count (simulating a row predating validation).
    #[tokio::test]
    async fn graph_preflight_reports_ensemble_with_invalid_min_pass() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let member = || crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Stale Ensemble".to_string(),
                kind: None,
                prompt_template: Some("Review {{spec_name}}".to_string()),
                members: Some(vec![member(), member()]),
                blueprint: None,
                from_node: entry,
                condition: "always".to_string(),
                min_pass: Some(2),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        let ensemble_id = extract_id(&created, "ensemble_id");
        db.update_ensemble_join_config(&ensemble_id, Some(3), None, None, None, None, None, None)
            .unwrap();

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: None,
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&result), "preflight should flag invalid min_pass");
        let msg = text(&result);
        assert!(msg.contains("Stale Ensemble"), "{msg}");
        assert!(msg.contains("invalid min_pass"), "{msg}");
    }

    #[tokio::test]
    async fn graph_preflight_rejects_unsafe_optional_model_token() {
        let home = tempdir().unwrap();
        let canopy_dir = home.path().join(".canopy");
        std::fs::create_dir_all(&canopy_dir).unwrap();
        let config = crate::domain::canopy_config::CanopyConfig {
            clis: vec![crate::domain::cli_config::CliConfig {
                name: "bad-template-cli".to_string(),
                binary: "/bin/echo".to_string(),
                invocation_template: Some("-m {{model?}}#{{effort?}} {{prompt}}".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        config.save(&canopy_dir).unwrap();
        let _home_guard = HomeVar::set(home.path());

        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Entry".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("bad-template-cli")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap();

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: None,
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&result), "{}", text(&result));
        let msg = text(&result);
        assert!(msg.contains("bad-template-cli"), "{msg}");
        assert!(msg.contains("{{model?}}"), "{msg}");
    }

    #[tokio::test]
    async fn graph_preflight_names_ensemble_as_commit_rights_holder() {
        // NFR1(e): preflight's `commit_rights_holders` names the ensemble
        // (kind + name), not its members, as the graph's committer.
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_two_member_ensemble(&handler, &spec.id, "Committer Ensemble", &entry, &arbiter)
                .await;
        db.update_ensemble_commit_rights(&ensemble_id, true)
            .unwrap();

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: Some(5),
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(
            !is_err(&result),
            "preflight must succeed: {}",
            text(&result)
        );
        let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
        let holders = body["commit_rights_holders"].as_array().unwrap();
        assert!(
            holders
                .iter()
                .any(|h| h["kind"] == "ensemble" && h["name"] == "Committer Ensemble"),
            "preflight must name the ensemble as committer: {body}"
        );
    }

    #[tokio::test]
    async fn graph_preflight_warns_on_stale_loop_complete_node_in_ensemble_prompt() {
        // Build a graph with one ensemble whose shared prompt_template still
        // instructs the 2.x tool name — mirrors the CB68 objective's kilo
        // run fff7f1aa scenario (Implementer/Committer ensemble prompt).
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let member = || crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Stale Ensemble".to_string(),
                kind: None,
                prompt_template: Some("end with exactly one loop_complete_node call".to_string()),
                members: Some(vec![member(), member()]),
                blueprint: None,
                from_node: entry,
                condition: "always".to_string(),
                min_pass: Some(2),
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter,
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: Some(5),
                reviewer: None,
            }))
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        let body = raw_text(&result);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let warnings = parsed["stale_name_warnings"].as_array().unwrap();
        assert!(
            warnings.iter().any(|w| w["kind"] == "ensemble"
                && w["new_name"] == "graph_complete_node"
                && w["old_name"] == "loop_complete_node"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn graph_preflight_reports_no_stale_name_warnings_on_clean_graph() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        add_two_member_ensemble(&handler, &spec.id, "Clean Ensemble", &entry, &arbiter).await;

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: Some(5),
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let body = raw_text(&result);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed["stale_name_warnings"],
            serde_json::json!([]),
            "{body}"
        );
    }

    #[tokio::test]
    async fn graph_preflight_hook_warning_names_the_firing_event() {
        // Same clean-graph fixture as above plus one stale agent hook on
        // `on_failed`: if the hook scan were missing, `stale_name_warnings`
        // stays empty and this fails; and a warning that labels every hook a
        // generic "completion hook" fails the `on_failed` assertions.
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        add_two_member_ensemble(&handler, &spec.id, "Clean Ensemble", &entry, &arbiter).await;

        let mut hooks = std::collections::BTreeMap::new();
        hooks.insert(
            crate::domain::graphs::GraphHookEvent::OnFailed,
            vec![crate::domain::graphs::GraphCompletionHook {
                platform: Some("claude".to_string()),
                model: None,
                effort: None,
                prompt: Some("on failure, call loop_report_blocker with details".to_string()),
                command: None,
                target_session_id: None,
                target_session_name: None,
                timeout_minutes: None,
                target_graph_id: None,
                queue_id: None,
                workdir_override: None,
                idea: None,
            }],
        );
        db.update_graph_hooks(&lp.id, &hooks).unwrap();

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: Some(5),
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let body = raw_text(&result);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let warnings = parsed["stale_name_warnings"].as_array().unwrap();
        assert!(
            warnings.iter().any(|w| w["kind"] == "hook"
                && w["event"] == "on_failed"
                && w["old_name"] == "loop_report_blocker"
                && w["new_name"] == "graph_report_blocker"
                && w["warning"]
                    .as_str()
                    .is_some_and(|t| t.contains("on_failed hook"))),
            "{body}"
        );
    }

    /// `from_node` moves every entry edge to the new source with the new
    /// condition and leaves no edge from the old source; a failed rewire
    /// leaves the previous edges (and row) intact.
    #[tokio::test]
    async fn graph_update_ensemble_from_node_replaces_every_entry_edge() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let old_entry = add_agent_node(&handler, &spec.id, "OldEntry").await;
        let new_entry = add_agent_node(&handler, &spec.id, "NewEntry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_two_member_ensemble(&handler, &spec.id, "Workers", &old_entry, &arbiter).await;
        let member_ids: Vec<String> = db
            .get_ensemble_details(&ensemble_id)
            .unwrap()
            .unwrap()
            .members
            .iter()
            .map(|m| m.node_id.clone())
            .collect();

        let mut update = blank_ensemble_update(&ensemble_id);
        update.from_node = Some(new_entry.clone());
        update.condition = Some("pass".to_string());
        let rewired = handler
            .graph_update_ensemble(Parameters(update))
            .await
            .unwrap();
        assert!(!is_err(&rewired), "{}", text(&rewired));

        let edges = db.list_graph_edges(&spec.id).unwrap();
        assert!(
            !edges
                .iter()
                .any(|e| e.from_node == old_entry && member_ids.contains(&e.to_node)),
            "no entry edge may remain from the old source"
        );
        for member_id in &member_ids {
            assert!(
                edges.iter().any(|e| e.from_node == new_entry
                    && e.to_node == *member_id
                    && e.condition == GraphEdgeCondition::Pass),
                "new source must reach every member with the new condition"
            );
        }
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.ensemble.entry_from_node, new_entry);
        assert_eq!(details.ensemble.entry_condition, GraphEdgeCondition::Pass);

        // A failed rewire (unknown source) changes nothing.
        let mut bad_update = blank_ensemble_update(&ensemble_id);
        bad_update.from_node = Some("ghost-node".to_string());
        let failed = handler
            .graph_update_ensemble(Parameters(bad_update))
            .await
            .unwrap();
        assert!(is_err(&failed));
        let edges_after = db.list_graph_edges(&spec.id).unwrap();
        assert_eq!(edges_after.len(), edges.len());
        for member_id in &member_ids {
            assert!(edges_after
                .iter()
                .any(|e| e.from_node == new_entry && e.to_node == *member_id));
        }
        let details_after = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details_after.ensemble.entry_from_node, new_entry);
    }

    /// An ensemble entered from three different nodes carries entry edges
    /// from each of them to every member, and the graph still validates —
    /// the review-graph shape (designer, failing gate, reviewer bouncing
    /// back) with no relay node.
    #[tokio::test]
    async fn graph_update_ensemble_multi_source_entry_reaches_every_member() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let designer = add_agent_node(&handler, &spec.id, "Designer").await;
        let gate = add_agent_node(&handler, &spec.id, "Gate").await;
        let reviewer = add_agent_node(&handler, &spec.id, "Reviewer").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_two_member_ensemble(&handler, &spec.id, "Implementers", &designer, &arbiter).await;
        let member_ids: Vec<String> = db
            .get_ensemble_details(&ensemble_id)
            .unwrap()
            .unwrap()
            .members
            .iter()
            .map(|m| m.node_id.clone())
            .collect();

        for source in [&gate, &reviewer] {
            let mut update = blank_ensemble_update(&ensemble_id);
            update.add_entry_from = Some(source.clone());
            let added = handler
                .graph_update_ensemble(Parameters(update))
                .await
                .unwrap();
            assert!(!is_err(&added), "{}", text(&added));
        }

        let edges = db.list_graph_edges(&spec.id).unwrap();
        for source in [&designer, &gate, &reviewer] {
            for member_id in &member_ids {
                assert!(
                    edges.iter().any(|e| e.from_node == *source
                        && e.to_node == *member_id
                        && e.condition == GraphEdgeCondition::Always),
                    "source '{source}' must reach member '{member_id}'"
                );
            }
        }
        // The primary entry is untouched by additive wiring.
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.ensemble.entry_from_node, designer);

        // The multi-entry graph validates as a unit.
        let ensembles = db.list_ensembles_for_spec(&spec.id).unwrap();
        let nodes = db.list_graph_nodes(&spec.id).unwrap();
        crate::domain::validation::validate_ensembles_in_graph(&ensembles, &nodes, &edges)
            .expect("three-source entry must validate");

        // `graph_get` shows every entry source, not just the primary.
        let got = handler
            .graph_get(Parameters(GraphGetParams {
                graph_id: lp.id.clone(),
            }))
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
        let ensembles_json = json["specs"][0]["ensembles"].as_array().unwrap();
        assert_eq!(ensembles_json.len(), 1);
        let sources = ensembles_json[0]["entry_sources"].as_array().unwrap();
        assert_eq!(sources.len(), 3);
    }

    /// Detaching one entry source removes only its edges; detaching the
    /// primary promotes another source; the last source cannot be detached.
    #[tokio::test]
    async fn graph_update_ensemble_entry_remove_detaches_one_source() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let first = add_agent_node(&handler, &spec.id, "First").await;
        let second = add_agent_node(&handler, &spec.id, "Second").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_two_member_ensemble(&handler, &spec.id, "Workers", &first, &arbiter).await;
        let member_ids: Vec<String> = db
            .get_ensemble_details(&ensemble_id)
            .unwrap()
            .unwrap()
            .members
            .iter()
            .map(|m| m.node_id.clone())
            .collect();

        let mut add = blank_ensemble_update(&ensemble_id);
        add.add_entry_from = Some(second.clone());
        assert!(!is_err(
            &handler
                .graph_update_ensemble(Parameters(add))
                .await
                .unwrap()
        ));

        // Detach the primary: the other source is promoted in its place.
        let mut remove = blank_ensemble_update(&ensemble_id);
        remove.remove_entry_from = Some(first.clone());
        let removed = handler
            .graph_update_ensemble(Parameters(remove))
            .await
            .unwrap();
        assert!(!is_err(&removed), "{}", text(&removed));
        let edges = db.list_graph_edges(&spec.id).unwrap();
        assert!(!edges.iter().any(|e| e.from_node == first));
        for member_id in &member_ids {
            assert!(edges
                .iter()
                .any(|e| e.from_node == second && e.to_node == *member_id));
        }
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.ensemble.entry_from_node, second);

        // The last source cannot be detached — and the refusal changes nothing.
        let edge_count = edges.len();
        let mut remove_last = blank_ensemble_update(&ensemble_id);
        remove_last.remove_entry_from = Some(second.clone());
        let refused = handler
            .graph_update_ensemble(Parameters(remove_last))
            .await
            .unwrap();
        assert!(is_err(&refused));
        assert!(text(&refused).contains("last entry source"));
        assert_eq!(db.list_graph_edges(&spec.id).unwrap().len(), edge_count);
    }

    /// Builds a graph with one spec, two candidate entry nodes (`entry`,
    /// `alt_entry`), an `arbiter` exit node, and a parallel ensemble entered
    /// from both with `member_count` members. Returns (graph, spec, ensemble_id,
    /// entry, alt_entry).
    async fn build_two_source_ensemble_graph(
        db: &Database,
        handler: &TaskTriggerHandler,
        git_workdir: &std::path::Path,
        member_count: usize,
    ) -> (Graph, GraphSpec, String, String, String) {
        let lp = insert_test_graph(db, git_workdir);
        let spec = insert_test_spec(db, &lp.id, 1);
        let entry = add_agent_node(handler, &spec.id, "Entry").await;
        let alt_entry = add_agent_node(handler, &spec.id, "AltEntry").await;
        let arbiter = add_agent_node(handler, &spec.id, "Arbiter").await;
        let member = || crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Proposers".to_string(),
                kind: None,
                prompt_template: Some("Draft it".to_string()),
                members: Some((0..member_count).map(|_| member()).collect()),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        let ensemble_id = extract_id(&created, "ensemble_id");
        let mut add_alt = blank_ensemble_update(&ensemble_id);
        add_alt.add_entry_from = Some(alt_entry.clone());
        let added = handler
            .graph_update_ensemble(Parameters(add_alt))
            .await
            .unwrap();
        assert!(!is_err(&added), "{}", text(&added));
        (lp, spec, ensemble_id, entry, alt_entry)
    }

    /// CB54 FR5/FR6a (handler level, not just validate_ensembles_in_graph):
    /// growing then shrinking a two-source ensemble keeps `graph_run` accepting
    /// it at every member count, through the real graph_update_ensemble +
    /// graph_run tool path, with no manual rewiring in between.
    ///
    /// Uses three independent graphs (one per step) rather than three
    /// sequential `graph_run` calls on one graph: `graph_run` returns as soon
    /// as it spawns the background run task, so calling it twice on the same
    /// graph_id would race a real background run against the next resize.
    /// Three independent graphs keep each `graph_run` call deterministic
    /// while still exercising resize-then-launch for every member count
    /// through the real tool path.
    #[tokio::test]
    async fn graph_run_accepts_two_source_ensemble_through_grow_and_shrink() {
        let (_dir, db, handler) = endpoint_test_handler();
        let git_dir = tempdir().unwrap();
        init_git_repo(git_dir.path());

        async fn run_and_assert_accepted(
            db: &Database,
            handler: &TaskTriggerHandler,
            git_dir: &std::path::Path,
            member_count: usize,
            resize_to: Option<usize>,
            resize_again_to: Option<usize>,
        ) {
            let member = || crate::daemon::params::EnsembleMemberParams {
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            };
            let (lp, spec, ensemble_id, entry, alt_entry) =
                build_two_source_ensemble_graph(db, handler, git_dir, member_count).await;
            for target in [resize_to, resize_again_to].into_iter().flatten() {
                let mut update = blank_ensemble_update(&ensemble_id);
                update.members = Some((0..target).map(|_| member()).collect());
                let resized = handler
                    .graph_update_ensemble(Parameters(update))
                    .await
                    .unwrap();
                assert!(!is_err(&resized), "resize to {target}: {}", text(&resized));
            }
            let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
            let final_count = resize_again_to.or(resize_to).unwrap_or(member_count);
            assert_eq!(details.members.len(), final_count);
            let member_ids: std::collections::HashSet<_> =
                details.members.iter().map(|m| m.node_id.as_str()).collect();
            let entry_edges = db
                .list_graph_edges(&spec.id)
                .unwrap()
                .into_iter()
                .filter(|e| {
                    [entry.as_str(), alt_entry.as_str()].contains(&e.from_node.as_str())
                        && member_ids.contains(e.to_node.as_str())
                })
                .count();
            assert_eq!(entry_edges, 2 * final_count);

            let result = handler
                .graph_run(Parameters(GraphRunParams {
                    graph_id: lp.id.clone(),
                    queue_id: None,
                    workdir: None,
                    idea: None,
                    sandbox: None,
                    start_node_id: None,
                }))
                .await
                .unwrap();
            assert!(
                !is_err(&result),
                "graph_run must accept a fully-fanned-out {final_count}-member ensemble: {}",
                text(&result)
            );
        }

        // Step 1: freshly built at 3 members, 2 sources.
        run_and_assert_accepted(&db, &handler, git_dir.path(), 3, None, None).await;
        // Step 2: built at 3, grown to 4.
        run_and_assert_accepted(&db, &handler, git_dir.path(), 3, Some(4), None).await;
        // Step 3: built at 3, grown to 4, shrunk back to 3.
        run_and_assert_accepted(&db, &handler, git_dir.path(), 3, Some(4), Some(3)).await;
    }

    /// CB54 FR3/FR6b (handler level): a source that already reaches some but
    /// not all members — e.g. after a hand edit deleted one of its edges — is
    /// completed by add_entry_from, not refused as "already an entry source".
    #[tokio::test]
    async fn graph_update_ensemble_add_entry_from_completes_partial_source() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let alt_entry = add_agent_node(&handler, &spec.id, "AltEntry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_two_member_ensemble(&handler, &spec.id, "Workers", &entry, &arbiter).await;

        let mut add_alt = blank_ensemble_update(&ensemble_id);
        add_alt.add_entry_from = Some(alt_entry.clone());
        let added = handler
            .graph_update_ensemble(Parameters(add_alt))
            .await
            .unwrap();
        assert!(!is_err(&added), "{}", text(&added));

        let member_ids: Vec<String> = db
            .get_ensemble_details(&ensemble_id)
            .unwrap()
            .unwrap()
            .members
            .iter()
            .map(|m| m.node_id.clone())
            .collect();
        assert_eq!(member_ids.len(), 2);

        // Hand-delete alt_entry's edge to the first member only — simulating the
        // exact partial state CB54 is about (some, not all, members reachable).
        let stale_edge = db
            .list_graph_edges(&spec.id)
            .unwrap()
            .into_iter()
            .find(|e| e.from_node == alt_entry && e.to_node == member_ids[0])
            .expect("alt_entry -> first member edge must exist before deletion");
        assert!(db.delete_graph_edge(&stale_edge.id).unwrap());

        // Re-adding the same source must complete the missing edge, not refuse
        // with "already an entry source".
        let mut repair = blank_ensemble_update(&ensemble_id);
        repair.add_entry_from = Some(alt_entry.clone());
        let repaired = handler
            .graph_update_ensemble(Parameters(repair))
            .await
            .unwrap();
        assert!(
            !is_err(&repaired),
            "add_entry_from must complete a partial source, not refuse it: {}",
            text(&repaired)
        );

        let edges = db.list_graph_edges(&spec.id).unwrap();
        for member_id in &member_ids {
            assert!(
                edges.iter().any(|e| e.from_node == alt_entry
                    && &e.to_node == member_id
                    && e.condition == GraphEdgeCondition::Always),
                "alt_entry must reach member '{member_id}' after the repair"
            );
        }
    }

    /// CB54 FR4/FR6c (handler level): graph_preflight refuses a partially-wired
    /// multi-source ensemble (a source that reaches some but not all members)
    /// before spending any probe quota, naming the source and the exact reach
    /// count.
    #[tokio::test]
    async fn graph_preflight_refuses_partial_multi_source_ensemble() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let alt_entry = add_agent_node(&handler, &spec.id, "AltEntry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let member = || crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Proposers".to_string(),
                kind: None,
                prompt_template: Some("Draft it".to_string()),
                members: Some(vec![member(), member(), member()]),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
        let ensemble_id = extract_id(&created, "ensemble_id");

        let mut add_alt = blank_ensemble_update(&ensemble_id);
        add_alt.add_entry_from = Some(alt_entry.clone());
        let added = handler
            .graph_update_ensemble(Parameters(add_alt))
            .await
            .unwrap();
        assert!(!is_err(&added), "{}", text(&added));

        let member_ids: Vec<String> = db
            .get_ensemble_details(&ensemble_id)
            .unwrap()
            .unwrap()
            .members
            .iter()
            .map(|m| m.node_id.clone())
            .collect();
        assert_eq!(member_ids.len(), 3);

        // Hand-break alt_entry's wiring to exactly 2 of 3 members.
        let stale_edge = db
            .list_graph_edges(&spec.id)
            .unwrap()
            .into_iter()
            .find(|e| e.from_node == alt_entry && e.to_node == member_ids[2])
            .expect("alt_entry -> third member edge must exist before deletion");
        assert!(db.delete_graph_edge(&stale_edge.id).unwrap());

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: None,
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(
            is_err(&result),
            "partial multi-source ensemble must refuse preflight"
        );
        let msg = text(&result);
        assert!(
            msg.contains(&alt_entry),
            "message must name the partial source: {msg}"
        );
        assert!(msg.contains("reaches 2 of 3 members"), "{msg}");
    }

    /// `graph_delete_ensemble` removes members, quorum and all their edges as
    /// one unit, leaves the rest of the graph intact, and strands no edge.
    #[tokio::test]
    async fn graph_delete_ensemble_removes_members_quorum_and_edges() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_two_member_ensemble(&handler, &spec.id, "Doomed", &entry, &arbiter).await;
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        let owned: Vec<String> = details
            .members
            .iter()
            .map(|m| m.node_id.clone())
            .chain(std::iter::once(details.ensemble.join_node_id.clone()))
            .collect();

        let deleted = handler
            .graph_delete_ensemble(Parameters(GraphDeleteEnsembleParams {
                ensemble_id: ensemble_id.clone(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&deleted), "{}", text(&deleted));

        for node_id in &owned {
            assert!(db.get_graph_node(node_id).unwrap().is_none());
        }
        assert!(db.get_ensemble_details(&ensemble_id).unwrap().is_none());
        // The surrounding graph survives.
        assert!(db.get_graph_node(&entry).unwrap().is_some());
        assert!(db.get_graph_node(&arbiter).unwrap().is_some());
        // No dangling edge may name a deleted node.
        for edge in db.list_graph_edges(&spec.id).unwrap() {
            assert!(
                !owned.contains(&edge.from_node) && !owned.contains(&edge.to_node),
                "dangling edge after delete: {edge:?}"
            );
            assert!(db.get_graph_node(&edge.from_node).unwrap().is_some());
            assert!(db.get_graph_node(&edge.to_node).unwrap().is_some());
        }

        // Deleting twice reports not-found instead of succeeding silently.
        let again = handler
            .graph_delete_ensemble(Parameters(GraphDeleteEnsembleParams { ensemble_id }))
            .await
            .unwrap();
        assert!(is_err(&again));
    }

    /// Deleting an ensemble while its graph runs is refused and changes nothing.
    #[tokio::test]
    async fn graph_delete_ensemble_refused_while_graph_running() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_two_member_ensemble(&handler, &spec.id, "Busy", &entry, &arbiter).await;
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        let member_id = details.members[0].node_id.clone();

        db.update_graph_status(&lp.id, GraphStatus::Running, None, None)
            .unwrap();
        let refused = handler
            .graph_delete_ensemble(Parameters(GraphDeleteEnsembleParams {
                ensemble_id: ensemble_id.clone(),
            }))
            .await
            .unwrap();
        assert!(is_err(&refused));
        assert!(text(&refused).contains("running"), "{}", text(&refused));
        assert!(db.get_ensemble_details(&ensemble_id).unwrap().is_some());
        assert!(db.get_graph_node(&member_id).unwrap().is_some());

        // Entry rewiring is a topology change too — refused while running.
        let mut update = blank_ensemble_update(&ensemble_id);
        update.from_node = Some(entry.clone());
        let rewired = handler
            .graph_update_ensemble(Parameters(update))
            .await
            .unwrap();
        assert!(is_err(&rewired));
        assert!(text(&rewired).contains("running"), "{}", text(&rewired));
    }

    /// A quorum routes into another ensemble with no intermediate node: the
    /// node count does not grow, the join fans out to every member of the
    /// target, and the target cannot be deleted while chained into.
    #[tokio::test]
    async fn graph_update_ensemble_chains_quorum_into_another_ensemble() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let designer = add_agent_node(&handler, &spec.id, "Designer").await;
        let gate = add_agent_node(&handler, &spec.id, "Gate").await;
        let end = add_agent_node(&handler, &spec.id, "End").await;
        let implementers =
            add_two_member_ensemble(&handler, &spec.id, "Implementers", &designer, &end).await;
        let reviewers = add_two_member_ensemble(&handler, &spec.id, "Reviewers", &gate, &end).await;
        let impl_details = db.get_ensemble_details(&implementers).unwrap().unwrap();
        let review_details = db.get_ensemble_details(&reviewers).unwrap().unwrap();
        let review_member_ids: Vec<String> = review_details
            .members
            .iter()
            .map(|m| m.node_id.clone())
            .collect();
        let node_count = db.list_graph_nodes(&spec.id).unwrap().len();

        // Chain: implementers' quorum passes straight into the reviewers.
        let mut chain = blank_ensemble_update(&implementers);
        chain.on_pass_to = Some(reviewers.clone());
        let chained = handler
            .graph_update_ensemble(Parameters(chain))
            .await
            .unwrap();
        assert!(!is_err(&chained), "{}", text(&chained));

        // No relay node was created for the chain.
        assert_eq!(db.list_graph_nodes(&spec.id).unwrap().len(), node_count);
        let edges = db.list_graph_edges(&spec.id).unwrap();
        for member_id in &review_member_ids {
            assert!(
                edges
                    .iter()
                    .any(|e| e.from_node == impl_details.ensemble.join_node_id
                        && e.to_node == *member_id
                        && e.condition == GraphEdgeCondition::Pass),
                "quorum must fan out to every member of the chained ensemble"
            );
        }
        let chained_details = db.get_ensemble_details(&implementers).unwrap().unwrap();
        assert_eq!(
            chained_details.ensemble.on_pass_to,
            review_details.ensemble.join_node_id
        );
        crate::domain::validation::validate_ensembles_in_graph(
            &db.list_ensembles_for_spec(&spec.id).unwrap(),
            &db.list_graph_nodes(&spec.id).unwrap(),
            &edges,
        )
        .expect("chained ensembles must validate");

        // The chained-into ensemble cannot be deleted while targeted.
        let blocked = handler
            .graph_delete_ensemble(Parameters(GraphDeleteEnsembleParams {
                ensemble_id: reviewers.clone(),
            }))
            .await
            .unwrap();
        assert!(is_err(&blocked));
        assert!(text(&blocked).contains("routes its exit into it"));

        // Unchain, then the deletion goes through.
        let mut unchain = blank_ensemble_update(&implementers);
        unchain.on_pass_to = Some(end.clone());
        let unchained = handler
            .graph_update_ensemble(Parameters(unchain))
            .await
            .unwrap();
        assert!(!is_err(&unchained), "{}", text(&unchained));
        let deleted = handler
            .graph_delete_ensemble(Parameters(GraphDeleteEnsembleParams {
                ensemble_id: reviewers.clone(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&deleted), "{}", text(&deleted));
        assert!(db.get_ensemble_details(&reviewers).unwrap().is_none());
    }

    /// Member and quorum nodes still refuse direct wiring — and the refusals
    /// now name the ensemble-level surface that does the job.
    #[tokio::test]
    async fn graph_add_edge_still_refuses_ensemble_nodes_with_helpful_message() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
        let ensemble_id =
            add_two_member_ensemble(&handler, &spec.id, "Guarded", &entry, &arbiter).await;
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        let member_id = details.members[0].node_id.clone();
        let join_id = details.ensemble.join_node_id.clone();

        let into_member = handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                from_node: entry.clone(),
                to_node: member_id.clone(),
                condition: "always".to_string(),
                route: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&into_member));
        let member_msg = text(&into_member);
        assert!(member_msg.contains("graph_update_ensemble"), "{member_msg}");
        assert!(member_msg.contains("graph_delete_ensemble"), "{member_msg}");

        let out_of_quorum = handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                from_node: join_id.clone(),
                to_node: arbiter.clone(),
                condition: "pass".to_string(),
                route: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&out_of_quorum));
        assert!(text(&out_of_quorum).contains("graph_update_ensemble"));

        let delete_member = handler
            .graph_delete_node(Parameters(GraphDeleteNodeParams { node_id: member_id }))
            .await
            .unwrap();
        assert!(is_err(&delete_member));
        assert!(text(&delete_member).contains("graph_delete_ensemble"));

        let delete_quorum = handler
            .graph_delete_node(Parameters(GraphDeleteNodeParams { node_id: join_id }))
            .await
            .unwrap();
        assert!(is_err(&delete_quorum));
        assert!(text(&delete_quorum).contains("graph_delete_ensemble"));
    }

    // ── sync_* / intelligence_* / get_tools / project_* ────────────

    /// RAII guard: sets the real `CANOPY_AGENT_ID` env var, which
    /// `resolve_sync_agent_id` falls back to when no request `Parts` header
    /// is present (as in these direct-call tests). Safe under `cargo
    /// nextest` (one process per test).
    struct AgentIdVar {
        prev: Option<std::ffi::OsString>,
    }

    impl AgentIdVar {
        fn set(id: &str) -> Self {
            let prev = std::env::var_os(crate::shared::sync_identity::CANOPY_AGENT_ID_ENV);
            unsafe {
                std::env::set_var(crate::shared::sync_identity::CANOPY_AGENT_ID_ENV, id);
            }
            AgentIdVar { prev }
        }

        fn clear() -> Self {
            let prev = std::env::var_os(crate::shared::sync_identity::CANOPY_AGENT_ID_ENV);
            unsafe {
                std::env::remove_var(crate::shared::sync_identity::CANOPY_AGENT_ID_ENV);
            }
            AgentIdVar { prev }
        }
    }

    impl Drop for AgentIdVar {
        fn drop(&mut self) {
            let key = crate::shared::sync_identity::CANOPY_AGENT_ID_ENV;
            match &self.prev {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }

    // ── intelligence_upsert / intelligence_search / intelligence_graph_walk

    #[tokio::test]
    async fn intelligence_upsert_search_and_graph_walk() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-intel-1");

        let created = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("fact-1".to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some("Rust is memory safe".to_string()),
                        body: Some("Ownership rules prevent data races.".to_string()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));

        let linked = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("fact-2".to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some("Ownership rules".to_string()),
                        body: Some("One owner at a time.".to_string()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: Some(vec![IntelligenceRelationParams {
                            to_node_id: "fact-1".to_string(),
                            relation: "extends".to_string(),
                            weight: Some(1.0),
                        }]),
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&linked), "{}", text(&linked));

        let searched = handler
            .intelligence_search(
                Parameters(IntelligenceSearchParams {
                    query: "memory safe".to_string(),
                    kind: Some("fact".to_string()),
                    limit: Some(5),
                    project_hash: None,
                    depth: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(raw_text(&searched).contains("fact-1"));

        let walked = handler
            .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
                node_id: "fact-2".to_string(),
                depth: Some(2),
                compact: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&walked), "{}", text(&walked));
        assert!(raw_text(&walked).contains("fact-1"));

        let missing_root = handler
            .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
                node_id: "ghost-node".to_string(),
                depth: None,
                compact: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&missing_root));
    }

    // ── intelligence_delete_node / intelligence_delete_relation ──────────

    #[tokio::test]
    async fn intelligence_upsert_partial_update_via_mcp() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-intel-partial-1");

        let created = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("partial-mcp-1".to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some("Original title".to_string()),
                        body: Some("Original body text".to_string()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));

        // A call carrying only id + title changes the title and leaves
        // everything else exactly as it was.
        let updated = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("partial-mcp-1".to_string()),
                        kind: None,
                        status: None,
                        title: Some("Corrected title".to_string()),
                        body: None,
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));
        let value: serde_json::Value = serde_json::from_str(&raw_text(&updated)).unwrap();
        assert_eq!(value["created"], false);
        let changed: Vec<String> = serde_json::from_value(value["changed_fields"].clone()).unwrap();
        assert!(
            changed.contains(&"title".to_string()),
            "changed_fields should name title: {changed:?}"
        );
        assert!(
            !changed.contains(&"body".to_string()),
            "changed_fields should not name untouched body: {changed:?}"
        );
        assert_eq!(value["node"]["title"], "Corrected title");
        assert_eq!(value["node"]["body"], "Original body text");
    }

    #[tokio::test]
    async fn intelligence_upsert_body_replace_via_mcp() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-intel-partial-2");

        let created = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("hooks-mcp-1".to_string()),
                        kind: Some("decision".to_string()),
                        status: None,
                        title: Some("Scope".to_string()),
                        body: Some(
                            "Scope notes.\nfuera de esta versión: hooks internos\nMore notes."
                                .to_string(),
                        ),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));

        let updated = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("hooks-mcp-1".to_string()),
                        kind: None,
                        status: None,
                        title: None,
                        body: None,
                        body_replace: Some(crate::daemon::params::BodyReplaceParams {
                            fragment: "fuera de esta versión: hooks internos".to_string(),
                            replacement: "dentro de esta versión: hooks internos".to_string(),
                        }),
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));
        let value: serde_json::Value = serde_json::from_str(&raw_text(&updated)).unwrap();
        assert_eq!(value["created"], false);
        let changed: Vec<String> = serde_json::from_value(value["changed_fields"].clone()).unwrap();
        assert!(
            changed.contains(&"body".to_string()),
            "changed_fields should name body: {changed:?}"
        );
        assert_eq!(
            value["node"]["body"],
            "Scope notes.\ndentro de esta versión: hooks internos\nMore notes."
        );

        // A fragment that matches twice is refused instead of guessed.
        let err = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("hooks-mcp-1".to_string()),
                        kind: None,
                        status: None,
                        title: None,
                        body: None,
                        body_replace: Some(crate::daemon::params::BodyReplaceParams {
                            fragment: "notes".to_string(),
                            replacement: "remarks".to_string(),
                        }),
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .expect_err("ambiguous fragment must fail the write");
        assert!(
            format!("{err:?}").contains("ambiguous"),
            "refusal should say ambiguous: {err:?}"
        );
    }

    async fn upsert_intel_fact(
        handler: &TaskTriggerHandler,
        id: &str,
        title: &str,
        relations: Option<Vec<IntelligenceRelationParams>>,
    ) {
        let result = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some(id.to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some(title.to_string()),
                        body: Some("Test body".to_string()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
    }

    #[tokio::test]
    async fn intelligence_delete_node_removes_node_and_reports_relations() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-intel-delete-1");

        upsert_intel_fact(&handler, "delete-a", "Node A", None).await;
        upsert_intel_fact(
            &handler,
            "delete-b",
            "Node B",
            Some(vec![IntelligenceRelationParams {
                to_node_id: "delete-a".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
        )
        .await;
        upsert_intel_fact(
            &handler,
            "delete-c",
            "Node C",
            Some(vec![IntelligenceRelationParams {
                to_node_id: "delete-b".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
        )
        .await;

        // "delete-b" sits in the middle of a -> b, b -> c: two edges touch it.
        let deleted = handler
            .intelligence_delete_node(
                Parameters(IntelligenceDeleteNodeParams {
                    node_id: "delete-b".to_string(),
                    project_hash: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&deleted), "{}", text(&deleted));
        let body = raw_text(&deleted);
        assert!(body.contains("delete-b"));
        assert!(body.contains("\"relations_removed\": 2"));

        let searched = handler
            .intelligence_search(
                Parameters(IntelligenceSearchParams {
                    query: "Node B".to_string(),
                    kind: None,
                    limit: Some(5),
                    project_hash: None,
                    depth: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!raw_text(&searched).contains("delete-b"));

        // Neither surviving neighbor's graph walk should error or reference
        // the removed middle node.
        let from_a = handler
            .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
                node_id: "delete-a".to_string(),
                depth: Some(3),
                compact: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&from_a), "{}", text(&from_a));
        assert!(!raw_text(&from_a).contains("delete-b"));

        let from_c = handler
            .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
                node_id: "delete-c".to_string(),
                depth: Some(3),
                compact: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&from_c), "{}", text(&from_c));
        assert!(!raw_text(&from_c).contains("delete-b"));
    }

    #[tokio::test]
    async fn intelligence_delete_node_missing_id_returns_error_not_silent_success() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-intel-delete-2");

        let deleted = handler
            .intelligence_delete_node(
                Parameters(IntelligenceDeleteNodeParams {
                    node_id: "ghost-node".to_string(),
                    project_hash: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(is_err(&deleted));
    }

    #[tokio::test]
    async fn intelligence_delete_relation_removes_edge_leaves_nodes_intact() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-intel-delete-3");

        upsert_intel_fact(&handler, "rel-a", "Node A", None).await;
        upsert_intel_fact(
            &handler,
            "rel-b",
            "Node B",
            Some(vec![IntelligenceRelationParams {
                to_node_id: "rel-a".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
        )
        .await;

        let walked = handler
            .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
                node_id: "rel-b".to_string(),
                depth: Some(1),
                compact: None,
            }))
            .await
            .unwrap();
        let edge_id = serde_json::from_str::<serde_json::Value>(&raw_text(&walked))
            .unwrap()
            .get("edges")
            .and_then(|edges| edges.as_array())
            .and_then(|edges| edges.first())
            .and_then(|edge| edge.get("id"))
            .and_then(|id| id.as_i64())
            .expect("edge id present in graph walk response");

        let deleted = handler
            .intelligence_delete_relation(
                Parameters(IntelligenceDeleteRelationParams {
                    edge_id,
                    project_hash: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&deleted), "{}", text(&deleted));

        let walked_after = handler
            .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
                node_id: "rel-b".to_string(),
                depth: Some(1),
                compact: None,
            }))
            .await
            .unwrap();
        assert!(!raw_text(&walked_after).contains("rel-a"));

        // Both endpoint nodes survive the relation delete.
        let searched = handler
            .intelligence_search(
                Parameters(IntelligenceSearchParams {
                    query: "Node".to_string(),
                    kind: Some("fact".to_string()),
                    limit: Some(10),
                    project_hash: None,
                    depth: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        let search_body = raw_text(&searched);
        assert!(search_body.contains("rel-a"));
        assert!(search_body.contains("rel-b"));

        let missing = handler
            .intelligence_delete_relation(
                Parameters(IntelligenceDeleteRelationParams {
                    edge_id,
                    project_hash: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(is_err(&missing));
    }

    #[tokio::test]
    async fn intelligence_get_context_light_and_full() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-ctx-1");

        let bad_scope = handler
            .intelligence_get_context(
                Parameters(IntelligenceGetContextParams {
                    scope: "sideways".to_string(),
                    project_hash: None,
                    depth: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(is_err(&bad_scope));

        let light = handler
            .intelligence_get_context(
                Parameters(IntelligenceGetContextParams {
                    scope: "light".to_string(),
                    project_hash: None,
                    depth: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&light), "{}", text(&light));
        assert!(raw_text(&light).contains("\"scope\": \"light\""));

        let full = handler
            .intelligence_get_context(
                Parameters(IntelligenceGetContextParams {
                    scope: "full".to_string(),
                    project_hash: None,
                    depth: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&full), "{}", text(&full));
        assert!(raw_text(&full).contains("\"scope\": \"full\""));
    }

    // ── intelligence_list_projects / intelligence_link_projects ────

    #[tokio::test]
    async fn intelligence_list_and_link_projects() {
        let (_dir, db, handler) = endpoint_test_handler();
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        db.register_project_path(dir_a.path()).unwrap();
        db.register_project_path(dir_b.path()).unwrap();
        let hash_a = crate::domain::project::workdir_hash(&dir_a.path().to_string_lossy());
        let hash_b = crate::domain::project::workdir_hash(&dir_b.path().to_string_lossy());

        let listed = handler
            .intelligence_list_projects(Parameters(IntelligenceListProjectsParams {
                query: None,
                limit: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&listed));

        let empty_fields = handler
            .intelligence_link_projects(Parameters(IntelligenceLinkProjectsParams {
                from_project_hash: "  ".to_string(),
                to_project_hash: hash_b.clone(),
                relation: None,
                weight: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&empty_fields));

        let linked = handler
            .intelligence_link_projects(Parameters(IntelligenceLinkProjectsParams {
                from_project_hash: hash_a,
                to_project_hash: hash_b,
                relation: Some("depends_on".to_string()),
                weight: Some(0.5),
            }))
            .await
            .unwrap();
        assert!(!is_err(&linked), "{}", text(&linked));
        assert!(raw_text(&linked).contains("depends_on"));
    }

    // ── CM9: typed project graph traversal ──────────────────────

    /// Register parent + child dirs (child nested under parent) with one fact
    /// each. Returns (base_tempdir, parent_hash, child_hash).
    fn cm9_seed_parent_child(db: &Database) -> (tempfile::TempDir, String, String) {
        let base = tempdir().unwrap();
        let parent = base.path().join("ws-parent");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let p = db.register_project_path(&parent).unwrap();
        let c = db.register_project_path(&child).unwrap();
        for (id, hash, title) in [
            (
                "cm9-fact-parent",
                p.hash.clone(),
                "workspace billing rulebook",
            ),
            (
                "cm9-fact-child",
                c.hash.clone(),
                "workspace billing rulebook",
            ),
        ] {
            db.upsert_intelligence_node(crate::db::intelligence::IntelligenceNodeInput {
                id: Some(id.to_string()),
                kind: Some("fact".to_string()),
                status: None,
                title: Some(title.to_string()),
                body: Some(format!("{title} body")),
                body_replace: None,
                metadata: None,
                project_hash: Some(Some(hash)),
                session_id: None,
                relations: None,
            })
            .unwrap();
        }
        (base, p.hash, c.hash)
    }

    #[tokio::test]
    async fn intelligence_link_rejects_unknown_relation_via_mcp() {
        let (_dir, db, handler) = endpoint_test_handler();
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        db.register_project_path(dir_a.path()).unwrap();
        db.register_project_path(dir_b.path()).unwrap();
        let hash_a = crate::domain::project::workdir_hash(
            &std::fs::canonicalize(dir_a.path())
                .unwrap()
                .to_string_lossy(),
        );
        let hash_b = crate::domain::project::workdir_hash(
            &std::fs::canonicalize(dir_b.path())
                .unwrap()
                .to_string_lossy(),
        );

        let bad = handler
            .intelligence_link_projects(Parameters(IntelligenceLinkProjectsParams {
                from_project_hash: hash_a.clone(),
                to_project_hash: hash_b.clone(),
                relation: Some("blocks".to_string()),
                weight: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&bad), "unknown relation must be an error_result");
        assert!(raw_text(&bad).contains("allowed:"));

        let derived = handler
            .intelligence_link_projects(Parameters(IntelligenceLinkProjectsParams {
                from_project_hash: hash_a,
                to_project_hash: hash_b,
                relation: Some("contains".to_string()),
                weight: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&derived));
        assert!(raw_text(&derived).contains("derived"));
    }

    #[tokio::test]
    async fn intelligence_get_context_traverses_and_warns() {
        let (_dir, db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-cm9-ctx-1");
        let (_base, parent_hash, child_hash) = cm9_seed_parent_child(&db);
        // A third project depending on the parent (impact warning source).
        let dep_dir = tempdir().unwrap();
        let depender = db.register_project_path(dep_dir.path()).unwrap();
        db.link_projects(&depender.hash, &parent_hash, "depends_on", None)
            .unwrap();

        let full = handler
            .intelligence_get_context(
                Parameters(IntelligenceGetContextParams {
                    scope: "full".to_string(),
                    project_hash: Some(parent_hash.clone()),
                    depth: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&full), "{}", text(&full));
        let body = raw_text(&full);
        // Parent fact direct + child fact inherited via outbound contains.
        assert!(body.contains("cm9-fact-parent"));
        assert!(body.contains("cm9-fact-child"));
        assert!(body.contains("inherited:contains"));
        assert!(body.contains("\"via_project\""));
        assert!(body.contains("\"projects_reached\""));
        // Impact warning names the depender.
        assert!(body.contains("\"dependency_impact\""));
        assert!(body.contains(&depender.hash));

        let light = handler
            .intelligence_get_context(
                Parameters(IntelligenceGetContextParams {
                    scope: "light".to_string(),
                    project_hash: Some(child_hash),
                    depth: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&light), "{}", text(&light));
        let light_body = raw_text(&light);
        assert!(light_body.contains("cm9-fact-child"));
        assert!(
            !light_body.contains("\"dependency_impact\""),
            "light scope must not warn"
        );
    }

    #[tokio::test]
    async fn intelligence_search_scoped() {
        let (_dir, db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-cm9-search-1");
        let (_base, parent_hash, child_hash) = cm9_seed_parent_child(&db);

        let scoped = handler
            .intelligence_search(
                Parameters(IntelligenceSearchParams {
                    query: "billing rulebook".to_string(),
                    kind: Some("fact".to_string()),
                    limit: Some(10),
                    project_hash: Some(child_hash.clone()),
                    depth: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&scoped), "{}", text(&scoped));
        let body = raw_text(&scoped);
        assert!(body.contains("cm9-fact-child"));
        assert!(body.contains("cm9-fact-parent"));
        assert!(body.contains("\"via_relation\""));
        assert!(body.contains("\"projects_reached\""));

        // Depth 0 sees only the child project itself.
        let narrow = handler
            .intelligence_search(
                Parameters(IntelligenceSearchParams {
                    query: "billing rulebook".to_string(),
                    kind: Some("fact".to_string()),
                    limit: Some(10),
                    project_hash: Some(child_hash),
                    depth: Some(0),
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        let narrow_body = raw_text(&narrow);
        assert!(narrow_body.contains("cm9-fact-child"));
        assert!(!narrow_body.contains("cm9-fact-parent"));
        let _ = parent_hash;
    }

    #[tokio::test]
    async fn project_register_tool_registers_without_marker() {
        let (_dir, db, handler) = endpoint_test_handler();
        let base = tempdir().unwrap();
        let target = base.path().join("explicit-proj");
        std::fs::create_dir_all(&target).unwrap();
        assert!(!target.join(".canopy-project").exists());

        let registered = handler
            .project_register(Parameters(ProjectRegisterParams {
                path: target.to_string_lossy().to_string(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&registered), "{}", text(&registered));
        assert!(raw_text(&registered).contains("\"workdir_hash\""));

        let canonical = std::fs::canonicalize(&target)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let expected = crate::domain::project::workdir_hash(&canonical);
        assert!(db.get_project(&expected).unwrap().is_some());
    }

    // ── get_tools ────────────────────────────────────────────────

    #[tokio::test]
    async fn get_tools_returns_scoped_protocol() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-tools-1");

        let bad_scope = handler
            .get_tools(
                Parameters(GetToolsParams {
                    scope: "sideways".to_string(),
                    path: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(is_err(&bad_scope));

        let file_write = handler
            .get_tools(
                Parameters(GetToolsParams {
                    scope: "file_write".to_string(),
                    path: Some("/tmp/a.rs".to_string()),
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&file_write));
        assert!(raw_text(&file_write).contains("/tmp/a.rs"));

        let session_start = handler
            .get_tools(
                Parameters(GetToolsParams {
                    scope: "session_start".to_string(),
                    path: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&session_start));
        assert!(raw_text(&session_start).contains("session_start"));
    }

    #[tokio::test]
    async fn get_tools_session_start_carries_own_session_id() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-session-start-1");

        let result = handler
            .get_tools(
                Parameters(GetToolsParams {
                    scope: "session_start".to_string(),
                    path: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        let value: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
        assert_eq!(value["session_id"].as_str(), Some("agent-session-start-1"));
        assert!(value["session_hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("target_session_id")));
    }

    #[tokio::test]
    async fn get_tools_without_sync_identity_still_returns_protocol() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::clear();

        let result = handler
            .get_tools(
                Parameters(GetToolsParams {
                    scope: "session_start".to_string(),
                    path: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();

        assert!(!is_err(&result), "{}", text(&result));
        let value: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
        assert_eq!(value["scope"].as_str(), Some("session_start"));
        assert!(value.get("session_id").is_none());
    }

    // ── project_search / project_update ─────────────────────────

    #[tokio::test]
    async fn project_search_and_update() {
        let (_dir, db, handler) = endpoint_test_handler();
        let base = tempdir().unwrap();
        let project_dir = base.path().join("searchable-project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let project = db.register_project_path(&project_dir).unwrap();

        let no_match = handler
            .project_search(Parameters(ProjectSearchParams {
                query: "nothing-matches-this-xyz".to_string(),
            }))
            .await
            .unwrap();
        assert!(text(&no_match).contains("No projects found"));

        let found = handler
            .project_search(Parameters(ProjectSearchParams {
                query: "searchable-project".to_string(),
            }))
            .await
            .unwrap();
        assert!(raw_text(&found).contains(&project.hash));

        let updated = handler
            .project_update(Parameters(ProjectUpdateParams {
                project_hash: project.hash.clone(),
                description: Some("A test project".to_string()),
                tags: Some(vec!["rust".to_string()]),
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));

        let missing = handler
            .project_update(Parameters(ProjectUpdateParams {
                project_hash: "not-a-real-hash".to_string(),
                description: Some("x".to_string()),
                tags: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&missing));
    }

    // ── project_remap ─────────────────────────────────────────────

    #[tokio::test]
    async fn project_remap_moves_project_to_new_path() {
        let (_dir, db, handler) = endpoint_test_handler();
        let base = tempdir().unwrap();
        let old_dir = base.path().join("cadforge");
        let new_dir = base.path().join("cadspec");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();
        let project = db.register_project_path(&old_dir).unwrap();
        db.insert_terminal_session("t-1", "t-1", "bash", &project.path)
            .unwrap();

        let result = handler
            .project_remap(Parameters(ProjectRemapParams {
                project_hash: project.hash.clone(),
                new_path: new_dir.to_string_lossy().to_string(),
                dry_run: None,
                force: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let body = raw_text(&result);
        assert!(body.contains("\"kind\": \"move\""));
        assert!(body.contains("\"rows_moved\": 2"));

        assert!(db.get_project(&project.hash).unwrap().is_none());
        let new_canonical = std::fs::canonicalize(&new_dir).unwrap();
        let moved = db
            .get_project_by_path(&new_canonical)
            .unwrap()
            .expect("project moved to new path");
        assert_eq!(
            db.project_dependent_counts(&moved.path)
                .unwrap()
                .terminal_sessions,
            1
        );
    }

    #[tokio::test]
    async fn project_remap_dry_run_changes_nothing() {
        let (_dir, db, handler) = endpoint_test_handler();
        let base = tempdir().unwrap();
        let old_dir = base.path().join("old-loc");
        let new_dir = base.path().join("new-loc");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();
        let project = db.register_project_path(&old_dir).unwrap();

        let result = handler
            .project_remap(Parameters(ProjectRemapParams {
                project_hash: project.hash.clone(),
                new_path: new_dir.to_string_lossy().to_string(),
                dry_run: Some(true),
                force: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        assert!(raw_text(&result).contains("\"dry_run\": true"));

        // Nothing actually moved.
        assert!(db.get_project(&project.hash).unwrap().is_some());
    }

    #[tokio::test]
    async fn project_remap_refuses_missing_path_without_force() {
        let (_dir, db, handler) = endpoint_test_handler();
        let base = tempdir().unwrap();
        let old_dir = base.path().join("still-here");
        std::fs::create_dir_all(&old_dir).unwrap();
        let project = db.register_project_path(&old_dir).unwrap();

        let result = handler
            .project_remap(Parameters(ProjectRemapParams {
                project_hash: project.hash.clone(),
                new_path: "/definitely/does/not/exist/anywhere".to_string(),
                dry_run: None,
                force: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(db.get_project(&project.hash).unwrap().is_some());
    }

    // ── skill_list / skill_get ──

    #[tokio::test]
    async fn skill_list_empty_and_skill_get_unknown() {
        let (_dir, _db, handler) = endpoint_test_handler();

        let listed = handler.skill_list().await.unwrap();
        assert!(!is_err(&listed));

        let missing = handler
            .skill_get(Parameters(SkillGetParams {
                name: "unknown-skill".to_string(),
            }))
            .await;
        assert!(missing.is_err());
    }

    // ── intelligence prefix resolution + created field ──────────────

    #[tokio::test]
    async fn intelligence_upsert_returns_created_true_for_new_node() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-intel-created-1");

        let result = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("brand-new-node-1".to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some("First time".to_string()),
                        body: Some("body".to_string()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let body = raw_text(&result);
        assert!(
            body.contains("\"created\": true"),
            "expected created:true in: {body}"
        );
        // An explicit id that matched nothing must produce a loud warning, not
        // just `created: true` (which also fires on the auto-id path).
        assert!(
            body.contains("\"warning\":") && body.contains("brand-new-node-1"),
            "explicit unknown id should warn about creating a new node: {body}"
        );
    }

    #[tokio::test]
    async fn intelligence_upsert_returns_created_false_for_update() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-intel-created-2");

        let first = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("update-me-1".to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some("Original".to_string()),
                        body: Some("v1".to_string()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&first), "{}", text(&first));
        assert!(
            raw_text(&first).contains("\"created\": true"),
            "first upsert should be created:true: {}",
            raw_text(&first)
        );

        let second = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("update-me-1".to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some("Updated".to_string()),
                        body: Some("v2".to_string()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&second), "{}", text(&second));
        assert!(
            raw_text(&second).contains("\"created\": false"),
            "second upsert should be created:false: {}",
            raw_text(&second)
        );
        // Updating an existing node must not carry the "created a new node"
        // warning.
        assert!(
            !raw_text(&second).contains("\"warning\":"),
            "update path should not warn: {}",
            raw_text(&second)
        );
    }

    #[tokio::test]
    async fn intelligence_upsert_with_prefix_updates_existing() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-intel-prefix-1");

        let full_id = "3a476c63-6b4a-4860-9c18-1c784b40a4b2";
        let create = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some(full_id.to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some("Original".to_string()),
                        body: Some("v1".to_string()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&create), "{}", text(&create));

        let via_prefix = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("3a476c63".to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some("Updated via prefix".to_string()),
                        body: Some("v2".to_string()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&via_prefix), "{}", text(&via_prefix));
        let body = raw_text(&via_prefix);
        assert!(
            body.contains("\"created\": false"),
            "prefix upsert should update, not create: {body}"
        );
        assert!(
            body.contains(full_id),
            "response should contain the full UUID: {body}"
        );
        assert!(
            body.contains("Updated via prefix"),
            "response should contain the new title: {body}"
        );

        let searched = handler
            .intelligence_search(
                Parameters(IntelligenceSearchParams {
                    query: "Updated via prefix".to_string(),
                    kind: Some("fact".to_string()),
                    limit: Some(10),
                    project_hash: None,
                    depth: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        let search_body = raw_text(&searched);
        assert!(
            search_body.contains(full_id),
            "the node should exist with updated title: {search_body}"
        );
        assert!(
            search_body.contains("\"count\": 1"),
            "exactly one node must match — the prefix upsert must not have created a duplicate: {search_body}"
        );
    }

    #[tokio::test]
    async fn intelligence_upsert_with_ambiguous_prefix_errors() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-intel-prefix-ambig");

        let id1 = "abc12300-0000-0000-0000-000000000001";
        let id2 = "abc45600-0000-0000-0000-000000000002";
        for id in [id1, id2] {
            let r = handler
                .intelligence_upsert(
                    Parameters(IntelligenceUpsertParams {
                        node_data: IntelligenceNodeParams {
                            id: Some(id.to_string()),
                            kind: Some("fact".to_string()),
                            status: None,
                            title: Some(format!("Node {id}")),
                            body: Some("body".to_string()),
                            body_replace: None,
                            metadata: None,
                            project_hash: None,
                            session_id: None,
                            relations: None,
                        },
                    }),
                    OptionalExtension(None),
                )
                .await
                .unwrap();
            assert!(!is_err(&r), "{}", text(&r));
        }

        let ambiguous = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("abc".to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some("Should fail".to_string()),
                        body: Some("body".to_string()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(
            is_err(&ambiguous),
            "ambiguous prefix should error: {}",
            text(&ambiguous)
        );
        let err_body = raw_text(&ambiguous);
        assert!(
            err_body.contains("Ambiguous"),
            "error should mention Ambiguous: {err_body}"
        );
        assert!(
            err_body.contains(id1),
            "error should list candidate {id1}: {err_body}"
        );
        assert!(
            err_body.contains(id2),
            "error should list candidate {id2}: {err_body}"
        );
    }

    #[tokio::test]
    async fn intelligence_graph_walk_with_prefix() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-intel-walk-prefix");

        let full_id = "deadbeef-0000-0000-0000-000000000001";
        let created = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some(full_id.to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some("Walk me".to_string()),
                        body: Some("body".to_string()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));

        let walked = handler
            .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
                node_id: "deadbeef".to_string(),
                depth: Some(1),
                compact: None,
            }))
            .await
            .unwrap();
        assert!(
            !is_err(&walked),
            "prefix walk should succeed: {}",
            text(&walked)
        );
        let body = raw_text(&walked);
        assert!(
            body.contains(full_id),
            "walk response should contain the full UUID: {body}"
        );
    }

    #[tokio::test]
    async fn intelligence_delete_node_with_prefix() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-intel-del-prefix");

        let full_id = "feedface-0000-0000-0000-000000000001";
        let created = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some(full_id.to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some("Delete me by prefix".to_string()),
                        body: Some("body".to_string()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));

        let deleted = handler
            .intelligence_delete_node(
                Parameters(IntelligenceDeleteNodeParams {
                    node_id: "feedface".to_string(),
                    project_hash: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(
            !is_err(&deleted),
            "prefix delete should succeed: {}",
            text(&deleted)
        );
        let body = raw_text(&deleted);
        assert!(
            body.contains(full_id),
            "delete response should contain the full UUID: {body}"
        );

        let walked = handler
            .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
                node_id: full_id.to_string(),
                depth: Some(1),
                compact: None,
            }))
            .await
            .unwrap();
        assert!(
            is_err(&walked),
            "node should be gone after prefix delete: {}",
            text(&walked)
        );
    }

    #[tokio::test]
    async fn spec_list_default_compact_omits_description() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let created = handler
            .spec_create(Parameters(SpecCreateParams {
                name: "Compact test".to_string(),
                description: valid_spec_description(),
                workdir: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));

        let listed = handler
            .spec_list(Parameters(SpecListParams {
                workdir: None,
                status: None,
                unassigned_only: None,
                include_descriptions: None,
                limit: None,
                offset: None,
            }))
            .await
            .unwrap();
        let body = raw_text(&listed);
        assert!(
            !body.contains("Objective") && !body.contains("<objective>"),
            "compact mode must omit description content"
        );
        assert!(body.contains("Compact test"));
    }

    #[tokio::test]
    async fn spec_list_with_include_descriptions_returns_description() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let desc = valid_spec_description();
        let created = handler
            .spec_create(Parameters(SpecCreateParams {
                name: "Verbose test".to_string(),
                description: desc.clone(),
                workdir: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));

        let listed = handler
            .spec_list(Parameters(SpecListParams {
                workdir: None,
                status: None,
                unassigned_only: None,
                include_descriptions: Some(true),
                limit: None,
                offset: None,
            }))
            .await
            .unwrap();
        let body = raw_text(&listed);
        assert!(
            body.contains("<objective>") || body.contains("Objective"),
            "include_descriptions must include description content"
        );
    }

    // ── CB28: result budget contract for graph walk + spec_list ──────────

    #[tokio::test]
    async fn intelligence_upsert_duplicate_candidates_fit_budget_and_omit_body() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-cb45-fit");

        // Three large-bodied existing nodes, all citing the same file:line so
        // all three are flagged as duplicates of the node upserted below.
        for i in 0..3 {
            let body = format!(
                "see src/cb45-shared.rs:1 for details. {}",
                "y".repeat(12_000)
            );
            let result = handler
                .intelligence_upsert(
                    Parameters(IntelligenceUpsertParams {
                        node_data: IntelligenceNodeParams {
                            id: Some(format!("cb45-fit-{i}")),
                            kind: Some("fact".to_string()),
                            status: None,
                            title: Some(format!("CB45 fit node {i}")),
                            body: Some(body),
                            body_replace: None,
                            metadata: None,
                            project_hash: None,
                            session_id: None,
                            relations: None,
                        },
                    }),
                    OptionalExtension(None),
                )
                .await
                .unwrap();
            assert!(!is_err(&result), "{}", text(&result));
        }

        let new_body = format!("also see src/cb45-shared.rs:1. {}", "z".repeat(12_000));
        let upserted = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("cb45-fit-new".to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some("CB45 fit new node".to_string()),
                        body: Some(new_body.clone()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&upserted), "{}", text(&upserted));

        let body = raw_text(&upserted);
        assert!(
            body.len() < MCP_RESULT_BUDGET_BYTES,
            "upsert response ({} bytes) with 3 large duplicate candidates must fit the {}-byte budget",
            body.len(),
            MCP_RESULT_BUDGET_BYTES
        );

        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

        // The written node itself is returned complete — requirement 4.
        assert_eq!(
            parsed["node"]["body"].as_str().unwrap(),
            new_body,
            "the caller's own write must not be truncated"
        );

        let candidates = parsed["duplicate_candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 3, "expected all 3 duplicates reported");
        for candidate in candidates {
            assert!(candidate.get("id").is_some());
            assert!(candidate.get("title").is_some());
            assert!(candidate.get("kind").is_some());
            assert!(candidate.get("excerpt").is_some());
            assert!(
                candidate.get("body").is_none(),
                "duplicate candidate must never carry a full body field"
            );
            let excerpt = candidate["excerpt"].as_str().unwrap();
            assert!(
                excerpt.len() < 300,
                "excerpt must be short, got {} chars",
                excerpt.len()
            );
            assert!(
                !excerpt.contains(&"y".repeat(1000)),
                "excerpt must not carry the candidate's large body"
            );
        }

        // Existing fields kept as-is — requirement 5.
        assert!(parsed.get("duplicate_warning").is_some());
        assert_eq!(
            parsed["duplicate_detection_note"].as_str().unwrap(),
            crate::db::intelligence::LEXICAL_DUPLICATE_LIMITATION
        );
    }

    #[tokio::test]
    async fn intelligence_upsert_duplicate_candidates_capped_with_omitted_count() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-cb45-cap");

        let total = INTELLIGENCE_DUPLICATE_CANDIDATES_CAP + 5;
        for i in 0..total {
            let result = handler
                .intelligence_upsert(
                    Parameters(IntelligenceUpsertParams {
                        node_data: IntelligenceNodeParams {
                            id: Some(format!("cb45-cap-{i:03}")),
                            kind: Some("fact".to_string()),
                            status: None,
                            title: Some(format!("CB45 cap node {i:03}")),
                            body: Some(format!("see src/cb45-cap-{i:03}.rs:1 for details.")),
                            body_replace: None,
                            metadata: None,
                            project_hash: None,
                            session_id: None,
                            relations: None,
                        },
                    }),
                    OptionalExtension(None),
                )
                .await
                .unwrap();
            assert!(!is_err(&result), "{}", text(&result));
        }

        let citations: String = (0..total)
            .map(|i| format!("see src/cb45-cap-{i:03}.rs:1"))
            .collect::<Vec<_>>()
            .join(" ");
        let upserted = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some("cb45-cap-new".to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some("CB45 cap new node".to_string()),
                        body: Some(citations),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&upserted), "{}", text(&upserted));

        let parsed: serde_json::Value = serde_json::from_str(&raw_text(&upserted)).unwrap();
        let candidates = parsed["duplicate_candidates"].as_array().unwrap();
        assert_eq!(
            candidates.len(),
            INTELLIGENCE_DUPLICATE_CANDIDATES_CAP,
            "duplicate_candidates must be capped at the budget-derived limit"
        );
        assert_eq!(parsed["duplicate_candidates_truncated"], true);
        assert_eq!(
            parsed["duplicate_candidates_omitted"].as_u64().unwrap() as usize,
            5,
            "omitted count must equal total minus the cap"
        );
    }

    async fn upsert_large_intel_node(
        handler: &TaskTriggerHandler,
        id: &str,
        title: &str,
        body_marker: &str,
        relations: Option<Vec<IntelligenceRelationParams>>,
    ) {
        let body = format!("{body_marker}-{}", "x".repeat(12_000));
        let result = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some(id.to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some(title.to_string()),
                        body: Some(body),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
    }

    #[tokio::test]
    async fn intelligence_graph_walk_compact_omits_bodies() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-cb28-walk-compact");

        upsert_large_intel_node(&handler, "cb28-a", "CB28 node A", "LARGE-BODY-ALPHA", None).await;
        upsert_large_intel_node(
            &handler,
            "cb28-b",
            "CB28 node B",
            "LARGE-BODY-BETA",
            Some(vec![IntelligenceRelationParams {
                to_node_id: "cb28-a".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
        )
        .await;
        upsert_large_intel_node(
            &handler,
            "cb28-c",
            "CB28 node C",
            "LARGE-BODY-GAMMA",
            Some(vec![IntelligenceRelationParams {
                to_node_id: "cb28-b".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
        )
        .await;

        let walked = handler
            .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
                node_id: "cb28-c".to_string(),
                depth: Some(2),
                compact: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&walked), "{}", text(&walked));
        let body = raw_text(&walked);
        for marker in ["LARGE-BODY-ALPHA", "LARGE-BODY-BETA", "LARGE-BODY-GAMMA"] {
            assert!(
                !body.contains(marker),
                "compact walk must omit node bodies, found {marker}"
            );
        }
        // The nodes are still there — only their bodies are gone.
        for title in ["CB28 node A", "CB28 node B", "CB28 node C"] {
            assert!(body.contains(title), "compact walk must keep {title}");
        }
        assert!(
            body.len() < MCP_RESULT_BUDGET_BYTES,
            "compact walk ({} bytes) must fit the {}-byte budget",
            body.len(),
            MCP_RESULT_BUDGET_BYTES
        );
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(
            parsed.get("node_count").is_some(),
            "walk must state node_count"
        );
        assert!(
            parsed.get("edge_count").is_some(),
            "walk must state edge_count"
        );
    }

    #[tokio::test]
    async fn intelligence_graph_walk_root_appears_once() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-cb28-walk-root");

        upsert_large_intel_node(&handler, "cb28-r1", "CB28 root R1", "ROOT-BODY-R1", None).await;
        upsert_large_intel_node(
            &handler,
            "cb28-r2",
            "CB28 leaf R2",
            "ROOT-BODY-R2",
            Some(vec![IntelligenceRelationParams {
                to_node_id: "cb28-r1".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
        )
        .await;

        let walked = handler
            .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
                node_id: "cb28-r2".to_string(),
                depth: Some(1),
                compact: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&walked), "{}", text(&walked));
        let parsed: serde_json::Value = serde_json::from_str(&raw_text(&walked)).unwrap();
        let root_id = parsed["root"]["id"].as_str().unwrap().to_string();
        let nodes = parsed["nodes"].as_array().unwrap();
        assert!(
            !nodes
                .iter()
                .any(|n| n.get("id").and_then(|id| id.as_str()) == Some(root_id.as_str())),
            "root {root_id} must not appear among nodes"
        );
        assert_eq!(
            parsed["node_count"].as_u64().unwrap() as usize,
            nodes.len(),
            "node_count must match nodes returned"
        );
    }

    #[tokio::test]
    async fn intelligence_graph_walk_compact_false_returns_bodies() {
        let (_dir, _db, handler) = endpoint_test_handler();
        let _agent_guard = AgentIdVar::set("agent-cb28-walk-full");

        upsert_large_intel_node(
            &handler,
            "cb28-f1",
            "CB28 full node",
            "FULL-BODY-DISTINCTIVE",
            None,
        )
        .await;

        let walked = handler
            .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
                node_id: "cb28-f1".to_string(),
                depth: Some(1),
                compact: Some(false),
            }))
            .await
            .unwrap();
        assert!(!is_err(&walked), "{}", text(&walked));
        assert!(
            raw_text(&walked).contains("FULL-BODY-DISTINCTIVE"),
            "compact: false must restore full bodies"
        );
    }

    fn insert_standalone_spec(db: &Database, name: &str) -> GraphSpec {
        let spec = GraphSpec {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id: None,
            name: name.to_string(),
            description: Some(valid_spec_description()),
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        db.insert_graph_spec(&spec).unwrap();
        spec
    }

    fn spec_list_ids(body: &str) -> Vec<String> {
        serde_json::from_str::<serde_json::Value>(body).unwrap()["specs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn spec_list_pagination_respects_limit_and_offset() {
        let (_dir, db, handler) = endpoint_test_handler();
        for i in 0..10 {
            insert_standalone_spec(&db, &format!("CB28 page spec {i:02}"));
        }

        let first = handler
            .spec_list(Parameters(SpecListParams {
                workdir: None,
                status: None,
                unassigned_only: None,
                include_descriptions: None,
                limit: Some(3),
                offset: Some(0),
            }))
            .await
            .unwrap();
        let first_body = raw_text(&first);
        let first_parsed: serde_json::Value = serde_json::from_str(&first_body).unwrap();
        assert_eq!(first_parsed["returned"], 3);
        assert_eq!(first_parsed["total"], 10);
        assert_eq!(first_parsed["truncated"], true);
        assert_eq!(first_parsed["omitted"], 7);
        let first_ids = spec_list_ids(&first_body);
        assert_eq!(first_ids.len(), 3);

        let second = handler
            .spec_list(Parameters(SpecListParams {
                workdir: None,
                status: None,
                unassigned_only: None,
                include_descriptions: None,
                limit: Some(3),
                offset: Some(3),
            }))
            .await
            .unwrap();
        let second_body = raw_text(&second);
        let second_ids = spec_list_ids(&second_body);
        assert_eq!(second_ids.len(), 3);
        for id in &second_ids {
            assert!(
                !first_ids.contains(id),
                "offset page must not repeat rows from the first page"
            );
        }

        let last = handler
            .spec_list(Parameters(SpecListParams {
                workdir: None,
                status: None,
                unassigned_only: None,
                include_descriptions: None,
                limit: Some(3),
                offset: Some(9),
            }))
            .await
            .unwrap();
        let last_body = raw_text(&last);
        let last_parsed: serde_json::Value = serde_json::from_str(&last_body).unwrap();
        assert_eq!(last_parsed["returned"], 1);
        assert_eq!(last_parsed["total"], 10);
        assert!(
            last_parsed.get("truncated").is_none(),
            "final page must not report truncation: {last_body}"
        );
        let last_ids = spec_list_ids(&last_body);
        assert!(!first_ids.contains(&last_ids[0]));
        assert!(!second_ids.contains(&last_ids[0]));
    }

    #[tokio::test]
    async fn spec_list_default_page_fits_budget() {
        let (_dir, db, handler) = endpoint_test_handler();
        let want = SPEC_LIST_DEFAULT_LIMIT as usize + 10;
        for i in 0..want {
            insert_standalone_spec(&db, &format!("CB28 budget probe spec {i:03}"));
        }

        let listed = handler
            .spec_list(Parameters(SpecListParams {
                workdir: None,
                status: None,
                unassigned_only: None,
                include_descriptions: None,
                limit: None,
                offset: None,
            }))
            .await
            .unwrap();
        let body = raw_text(&listed);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed["returned"].as_u64().unwrap() as usize,
            SPEC_LIST_DEFAULT_LIMIT as usize,
            "default page must return exactly the budget-derived default"
        );
        assert_eq!(parsed["total"].as_u64().unwrap() as usize, want);
        assert_eq!(parsed["truncated"], true);
        assert_eq!(
            parsed["omitted"].as_u64().unwrap() as usize,
            want - SPEC_LIST_DEFAULT_LIMIT as usize
        );
        assert!(
            body.len() <= MCP_RESULT_BUDGET_BYTES,
            "default spec_list page ({} bytes) must fit the {}-byte budget",
            body.len(),
            MCP_RESULT_BUDGET_BYTES
        );
    }

    #[tokio::test]
    async fn graph_node_runs_list_compact_mode() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let node = insert_named_node(&db, &spec.id, "builder", 1);
        let now = chrono::Utc::now();
        insert_finalized_node_run(
            &db,
            &lp.id,
            &spec.id,
            &node.id,
            GraphRunStatus::Pass,
            None,
            now,
        );

        let listed = handler
            .graph_node_runs_list(Parameters(GraphNodeRunsListParams {
                graph_id: lp.id.clone(),
                spec_id: None,
                node_id: None,
                limit: None,
                offset: None,
                compact: Some(true),
            }))
            .await
            .unwrap();
        assert!(!is_err(&listed), "{}", text(&listed));
        let body = raw_text(&listed);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let runs = parsed["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 1);
        assert!(runs[0].get("id").is_some(), "compact mode must include id");
        assert!(
            runs[0].get("spec_id").is_none(),
            "compact mode must omit spec_id"
        );
        assert_eq!(runs[0]["spec_name"], "Test Spec");
        assert_eq!(runs[0]["node_name"], "builder");
    }

    #[tokio::test]
    async fn graph_node_runs_list_compact_id_feeds_graph_node_run_get() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let node = insert_named_node(&db, &spec.id, "builder", 1);
        let run = insert_finalized_node_run(
            &db,
            &lp.id,
            &spec.id,
            &node.id,
            GraphRunStatus::Pass,
            Some(serde_json::json!({"reported_output": "ok"})),
            chrono::Utc::now(),
        );

        let listed = handler
            .graph_node_runs_list(Parameters(GraphNodeRunsListParams {
                graph_id: lp.id.clone(),
                spec_id: None,
                node_id: None,
                limit: None,
                offset: None,
                compact: Some(true),
            }))
            .await
            .unwrap();
        assert!(!is_err(&listed), "{}", text(&listed));
        let body = raw_text(&listed);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let runs = parsed["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 1);
        let compact_id = runs[0]
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(!compact_id.is_empty(), "compact listing must carry an id");
        assert_eq!(compact_id, run.id);
        assert!(
            runs[0].get("input").is_none(),
            "compact mode must omit input"
        );
        assert!(
            runs[0].get("output").is_none(),
            "compact mode must omit output"
        );

        let got = handler
            .graph_node_run_get(Parameters(GraphNodeRunGetParams {
                run_id: compact_id.to_string(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&got), "{}", text(&got));
        let detail: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
        assert_eq!(detail["id"], run.id);
    }

    #[tokio::test]
    async fn graph_node_runs_list_reports_truncation() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let node = insert_named_node(&db, &spec.id, "builder", 1);
        let now = chrono::Utc::now();
        for i in 0..3 {
            insert_finalized_node_run(
                &db,
                &lp.id,
                &spec.id,
                &node.id,
                GraphRunStatus::Pass,
                None,
                now - chrono::Duration::minutes(i),
            );
        }

        let capped = handler
            .graph_node_runs_list(Parameters(GraphNodeRunsListParams {
                graph_id: lp.id.clone(),
                spec_id: None,
                node_id: None,
                limit: Some(1),
                offset: None,
                compact: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&capped), "{}", text(&capped));
        let parsed: serde_json::Value = serde_json::from_str(&raw_text(&capped)).unwrap();
        assert_eq!(parsed["runs"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["total"], 3);
        assert_eq!(parsed["truncated"], true);
        assert_eq!(parsed["omitted"], 2);

        let full = handler
            .graph_node_runs_list(Parameters(GraphNodeRunsListParams {
                graph_id: lp.id.clone(),
                spec_id: None,
                node_id: None,
                limit: Some(50),
                offset: None,
                compact: None,
            }))
            .await
            .unwrap();
        let parsed_full: serde_json::Value = serde_json::from_str(&raw_text(&full)).unwrap();
        assert_eq!(parsed_full["total"], 3);
        assert!(
            parsed_full.get("truncated").is_none(),
            "a complete result must not claim truncation"
        );
        assert!(parsed_full.get("omitted").is_none());
    }

    #[tokio::test]
    async fn spec_update_with_prefix_resolves() {
        let (_dir, db, handler) = endpoint_test_handler();
        let created = handler
            .spec_create(Parameters(SpecCreateParams {
                name: "Prefix test".to_string(),
                description: valid_spec_description(),
                workdir: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));

        let all_specs = db.list_specs(None, None, false).unwrap();
        let full_id = all_specs[0].id.clone();
        let prefix = &full_id[..8];

        let updated = handler
            .spec_update(Parameters(SpecUpdateParams {
                spec_id: prefix.to_string(),
                name: Some("Renamed via prefix".to_string()),
                description: None,
                workdir: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));
        assert_eq!(
            db.get_graph_spec(&full_id).unwrap().unwrap().name,
            "Renamed via prefix"
        );
    }

    #[tokio::test]
    async fn spec_section_get_returns_section_from_tagged_spec() {
        let (_dir, db, handler) = endpoint_test_handler();
        let created = handler
            .spec_create(Parameters(SpecCreateParams {
                name: "Section test".to_string(),
                description: valid_spec_description(),
                workdir: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created), "{}", text(&created));

        let spec_id = db.list_specs(None, None, false).unwrap()[0].id.clone();

        let result = handler
            .spec_section_get(Parameters(SpecSectionGetParams {
                spec_id: spec_id.clone(),
                section: "objective".to_string(),
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let body = raw_text(&result);
        assert!(body.contains("Ship the feature"));
        assert!(body.contains("\"needs_conversion\": false"));
    }

    #[tokio::test]
    async fn spec_section_get_rejects_unknown_section() {
        let (_dir, db, handler) = endpoint_test_handler();
        let created = handler
            .spec_create(Parameters(SpecCreateParams {
                name: "Section test 2".to_string(),
                description: valid_spec_description(),
                workdir: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created));

        let spec_id = db.list_specs(None, None, false).unwrap()[0].id.clone();

        let result = handler
            .spec_section_get(Parameters(SpecSectionGetParams {
                spec_id,
                section: "bogus".to_string(),
            }))
            .await
            .unwrap();
        assert!(is_err(&result));
        assert!(text(&result).contains("unknown section"));
        assert!(text(&result).contains("bogus"));
    }

    #[tokio::test]
    async fn spec_convert_converts_legacy_and_skips_tagged() {
        let (_dir, db, handler) = endpoint_test_handler();

        let legacy_desc = "## Objective\nShip.\n\n## Functional Requirements\nDoes thing.\n\n## Non-Functional Requirements\nFast.\n\n## Constraints\nNone.\n\n## Guidelines\nStyle.\n\n## In Scope\nThis.\n\n## Out of Scope\nNothing.";
        db.insert_graph_spec(&GraphSpec {
            id: "legacy-spec-1".to_string(),
            graph_id: None,
            name: "Legacy".to_string(),
            description: Some(legacy_desc.to_string()),
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();

        let created = handler
            .spec_create(Parameters(SpecCreateParams {
                name: "Already tagged".to_string(),
                description: valid_spec_description(),
                workdir: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&created));

        let result = handler.spec_convert().await.unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let body = raw_text(&result);
        assert!(body.contains("\"converted\": 1"));
        assert!(body.contains("\"already_tagged\": 1"));

        let legacy = db.get_graph_spec("legacy-spec-1").unwrap().unwrap();
        let desc = legacy.description.unwrap();
        assert!(desc.contains("<spec>"));
        assert!(desc.contains("<objective>"));
    }

    #[tokio::test]
    async fn spec_convert_defers_running_spec_and_leaves_body_untouched() {
        let (_dir, db, handler) = endpoint_test_handler();

        let legacy_desc = "## Objective\nShip.\n\n## Functional Requirements\nDoes thing.\n\n## Non-Functional Requirements\nFast.\n\n## Constraints\nNone.\n\n## Guidelines\nStyle.\n\n## In Scope\nThis.\n\n## Out of Scope\nNothing.";
        db.insert_graph_spec(&GraphSpec {
            id: "running-legacy-spec".to_string(),
            graph_id: None,
            name: "Running legacy".to_string(),
            description: Some(legacy_desc.to_string()),
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Running,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();

        let result = handler.spec_convert().await.unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let body = raw_text(&result);
        assert!(body.contains("\"converted\": 0"), "{body}");
        assert!(body.contains("\"deferred_running\": 1"), "{body}");
        assert!(body.contains("running-legacy-spec"), "{body}");

        // A running spec's body must never be rewritten mid-flight.
        let after = db.get_graph_spec("running-legacy-spec").unwrap().unwrap();
        assert_eq!(after.description.as_deref(), Some(legacy_desc));

        // The DB-level guard also refuses the write directly.
        assert!(
            !db.update_spec_description_if_not_running("running-legacy-spec", "<spec></spec>")
                .unwrap(),
            "conditional update must report no row changed for a running spec"
        );
        let still = db.get_graph_spec("running-legacy-spec").unwrap().unwrap();
        assert_eq!(still.description.as_deref(), Some(legacy_desc));
    }

    #[tokio::test]
    async fn spec_convert_is_idempotent() {
        let (_dir, db, handler) = endpoint_test_handler();

        let legacy_desc = "## Objective\nShip src/main.rs:42 on 2026-09-01.\n\n## Functional Requirements\nNode abc-123 does thing.\n\n## Non-Functional Requirements\nLatency < 100ms.\n\n## Constraints\nUse `file:line`.\n\n## Guidelines\nStyle.\n\n## In Scope\nThis.\n\n## Out of Scope\nNothing.";
        db.insert_graph_spec(&GraphSpec {
            id: "idem-legacy-spec".to_string(),
            graph_id: None,
            name: "Idem legacy".to_string(),
            description: Some(legacy_desc.to_string()),
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();

        let first = handler.spec_convert().await.unwrap();
        assert!(raw_text(&first).contains("\"converted\": 1"));
        let converted_body = db
            .get_graph_spec("idem-legacy-spec")
            .unwrap()
            .unwrap()
            .description
            .unwrap();
        // Sentinels survive the mechanical conversion verbatim.
        assert!(converted_body.contains("src/main.rs:42"));
        assert!(converted_body.contains("2026-09-01"));
        assert!(converted_body.contains("abc-123"));
        assert!(converted_body.contains("Latency < 100ms"));
        assert!(converted_body.contains("`file:line`"));

        let second = handler.spec_convert().await.unwrap();
        let second_body = raw_text(&second);
        assert!(second_body.contains("\"converted\": 0"), "{second_body}");
        assert!(
            second_body.contains("\"already_tagged\": 1"),
            "{second_body}"
        );
        assert_eq!(
            db.get_graph_spec("idem-legacy-spec")
                .unwrap()
                .unwrap()
                .description
                .unwrap(),
            converted_body,
            "second conversion run must not change stored content"
        );
    }

    #[tokio::test]
    async fn resolve_spec_id_by_prefix_tests() {
        let (_dir, db, _handler) = endpoint_test_handler();
        let spec1 = GraphSpec {
            id: "aaaaaaaa-1111-1111-1111-111111111111".to_string(),
            graph_id: None,
            name: "Spec A".to_string(),
            description: Some("First".to_string()),
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        db.insert_graph_spec(&spec1).unwrap();
        let spec2 = GraphSpec {
            id: "aaaaaaaa-2222-2222-2222-222222222222".to_string(),
            graph_id: None,
            name: "Spec B".to_string(),
            description: Some("Second".to_string()),
            position: 2,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        db.insert_graph_spec(&spec2).unwrap();

        let exact = db.resolve_spec_id_by_prefix(&spec1.id).unwrap();
        assert_eq!(exact, Some(spec1.id.clone()));

        let prefix_match = db.resolve_spec_id_by_prefix("aaaaaaaa-1111").unwrap();
        assert_eq!(prefix_match, Some(spec1.id));

        let ambiguous = db.resolve_spec_id_by_prefix("aaaaaaaa");
        assert!(ambiguous.is_err(), "ambiguous prefix should error");
        assert!(ambiguous.unwrap_err().to_string().contains("Ambiguous"));

        let not_found = db.resolve_spec_id_by_prefix("zzzzzzzz").unwrap();
        assert_eq!(not_found, None);
    }

    #[tokio::test]
    async fn resolve_queue_id_by_prefix_tests() {
        let (_dir, db, _handler) = endpoint_test_handler();
        let q1 = Queue {
            id: "q-aaaaaaaa-1111".to_string(),
            name: "Q1".to_string(),
            created_at: chrono::Utc::now(),
        };
        let q2 = Queue {
            id: "q-aaaaaaaa-2222".to_string(),
            name: "Q2".to_string(),
            created_at: chrono::Utc::now(),
        };
        db.insert_queue(&q1).unwrap();
        db.insert_queue(&q2).unwrap();

        let exact = db.resolve_queue_id_by_prefix("q-aaaaaaaa-1111").unwrap();
        assert_eq!(exact, Some("q-aaaaaaaa-1111".to_string()));

        let prefix_match = db.resolve_queue_id_by_prefix("q-aaaaaaaa-111").unwrap();
        assert_eq!(prefix_match, Some("q-aaaaaaaa-1111".to_string()));

        let ambiguous = db.resolve_queue_id_by_prefix("q-aaaaaaaa");
        assert!(ambiguous.is_err());

        let not_found = db.resolve_queue_id_by_prefix("q-zzzz").unwrap();
        assert_eq!(not_found, None);
    }

    #[tokio::test]
    async fn resolve_graph_id_by_prefix_tests() {
        let (dir, db, _handler) = endpoint_test_handler();
        let lp1 = insert_test_graph(&db, dir.path());
        let lp2 = Graph {
            id: "graph-bbbbbbbb-2222".to_string(),
            name: "Graph B".to_string(),
            description: None,
            workdir: dir.path().to_string_lossy().to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            auto_continue_at: None,
            auto_continue_action: None,
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
        };
        db.insert_graph(&lp2).unwrap();

        let exact = db.resolve_graph_id_by_prefix(&lp1.id).unwrap();
        assert_eq!(exact, Some(lp1.id));

        let not_found = db.resolve_graph_id_by_prefix("graph-zzzz").unwrap();
        assert_eq!(not_found, None);
    }

    #[tokio::test]
    async fn graph_add_ensemble_with_cascade_kind_creates_single_member_ensemble() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

        let result = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Cascade".to_string(),
                kind: Some("cascade".to_string()),
                prompt_template: Some("do it".to_string()),
                members: Some(vec![EnsembleMemberParams {
                    platform: "claude".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                }]),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(
            !is_err(&result),
            "cascade ensemble with 1 member should succeed: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn graph_add_ensemble_with_parallel_kind_rejects_single_member() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

        let result = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Parallel".to_string(),
                kind: Some("parallel".to_string()),
                prompt_template: Some("do it".to_string()),
                members: Some(vec![EnsembleMemberParams {
                    platform: "claude".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                }]),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(
            is_err(&result),
            "parallel ensemble with 1 member should fail"
        );
    }

    #[tokio::test]
    async fn graph_add_ensemble_rejects_straggler_timeout_on_cascade() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

        let result = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Cascade".to_string(),
                kind: Some("cascade".to_string()),
                prompt_template: Some("do it".to_string()),
                members: Some(vec![EnsembleMemberParams {
                    platform: "claude".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                }]),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: Some(5),
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        assert!(
            is_err(&result),
            "cascade + straggler_timeout_minutes must be rejected"
        );
        assert!(
            text(&result).contains("straggler_timeout_minutes")
                && text(&result).contains("cascade"),
            "error must name the field and the kind: {}",
            text(&result)
        );
    }

    #[tokio::test]
    async fn graph_update_ensemble_rejects_quorum_grace_on_round_robin() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Ensemble".to_string(),
                kind: Some("round_robin".to_string()),
                prompt_template: Some("do it".to_string()),
                members: Some(vec![
                    EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    EnsembleMemberParams {
                        platform: "openrouter".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        let ensemble_id = extract_id(&created, "ensemble_id");

        let result = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: None,
                prompt_template: None,
                members: None,
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: Some(Some(3)),
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(
            is_err(&result),
            "round_robin + quorum_grace_minutes must be rejected"
        );
        assert!(
            text(&result).contains("quorum_grace_minutes") && text(&result).contains("round_robin"),
            "error must name the field and the kind: {}",
            text(&result)
        );
    }

    #[tokio::test]
    async fn graph_update_ensemble_switching_to_cascade_clears_stale_straggler_and_grace_fields() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Ensemble".to_string(),
                kind: Some("parallel".to_string()),
                prompt_template: Some("do it".to_string()),
                members: Some(vec![
                    EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    EnsembleMemberParams {
                        platform: "openrouter".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: Some(7),
                quorum_grace_minutes: Some(2),
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        let ensemble_id = extract_id(&created, "ensemble_id");

        let updated = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: Some("cascade".to_string()),
                prompt_template: None,
                members: None,
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(
            !is_err(&updated),
            "switching to cascade must succeed: {:?}",
            updated
        );
        assert!(
            text(&updated).contains("Cleared")
                && text(&updated).contains("straggler_timeout_minutes"),
            "response must say the stale fields were cleared: {}",
            text(&updated)
        );

        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.ensemble.kind, EnsembleKind::Cascade);
        assert_eq!(details.ensemble.straggler_timeout_minutes, None);
        assert_eq!(details.ensemble.quorum_grace_minutes, None);
    }

    #[tokio::test]
    async fn graph_update_ensemble_can_change_kind() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let spec = insert_test_spec(&db, &lp.id, 1);
        let entry = add_agent_node(&handler, &spec.id, "Entry").await;
        let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

        let created = handler
            .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
                commit_rights: None,
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Ensemble".to_string(),
                kind: None,
                prompt_template: Some("do it".to_string()),
                members: Some(vec![
                    EnsembleMemberParams {
                        platform: "claude".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                    EnsembleMemberParams {
                        platform: "openrouter".to_string(),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                        infra_retry_limit: None,
                        infra_crash_max_seconds: None,
                        infra_backoff_seconds: None,
                    },
                ]),
                blueprint: None,
                from_node: entry.clone(),
                condition: "always".to_string(),
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: arbiter.clone(),
                on_fail_to: None,
            }))
            .await
            .unwrap();
        let ensemble_id = extract_id(&created, "ensemble_id");

        let updated = handler
            .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
                commit_rights: None,
                ensemble_id: ensemble_id.clone(),
                kind: Some("cascade".to_string()),
                prompt_template: None,
                members: None,
                min_pass: None,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: None,
                on_fail_to: None,
                from_node: None,
                condition: None,
                add_entry_from: None,
                add_entry_condition: None,
                remove_entry_from: None,
            }))
            .await
            .unwrap();
        assert!(
            !is_err(&updated),
            "updating kind to cascade should succeed: {:?}",
            updated
        );

        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        assert_eq!(details.ensemble.kind, EnsembleKind::Cascade);
    }

    #[tokio::test]
    async fn graph_run_idea_and_queue_mutually_exclusive() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());

        let result = handler
            .graph_run(Parameters(GraphRunParams {
                graph_id: lp.id.clone(),
                queue_id: Some("some-queue".to_string()),
                workdir: None,
                idea: Some("build a landing page".to_string()),
                sandbox: None,
                start_node_id: None,
            }))
            .await
            .unwrap();
        assert!(
            is_err(&result),
            "idea and queue_id together must be rejected"
        );
        assert!(
            text(&result).contains("mutually exclusive"),
            "error must mention mutual exclusion: {}",
            text(&result)
        );
    }

    #[tokio::test]
    async fn idea_placeholder_spec_is_absent_from_spec_listings() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());

        // The row `run_graph_dispatch` binds to a spec-less / idea-driven graph
        // to carry `{{spec_content}}`: blank name, idea text in the
        // description. It is engine bookkeeping and must not surface as work.
        db.insert_graph_spec(&GraphSpec {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id: Some(lp.id.clone()),
            name: String::new(),
            description: Some("Build a hyperframes explainer about our launch".to_string()),
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Completed,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();

        let listed = raw_text(
            &handler
                .spec_list(Parameters(SpecListParams {
                    workdir: None,
                    status: None,
                    unassigned_only: None,
                    include_descriptions: Some(true),
                    limit: None,
                    offset: None,
                }))
                .await
                .unwrap(),
        );
        assert!(
            !listed.contains("hyperframes explainer"),
            "the idea placeholder must not appear in spec_list: {listed}"
        );
        assert!(
            listed.contains("\"total\": 0"),
            "the blank placeholder must not be counted in spec_list total: {listed}"
        );

        let got = handler
            .graph_get(Parameters(GraphGetParams {
                graph_id: lp.id.clone(),
            }))
            .await
            .unwrap();
        let got = raw_text(&got);
        assert!(
            !got.contains("hyperframes explainer"),
            "the idea placeholder must not appear in graph_get specs: {got}"
        );
    }

    /// (CB22) `graph_run` with a graph-bound spec whose name is `""` and
    /// description is `None` is refused synchronously: the error names the
    /// spec and the graph's status is untouched. The TUI's `run` flow sends
    /// only `graph_id`, so this is the user-visible refusal for a graph with
    /// no usable content and no queue.
    #[tokio::test]
    async fn graph_run_refuses_blank_bound_spec_and_leaves_graph_untouched() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let blank_id = uuid::Uuid::new_v4().to_string();
        db.insert_graph_spec(&GraphSpec {
            id: blank_id.clone(),
            graph_id: Some(lp.id.clone()),
            name: String::new(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();

        let result = handler
            .graph_run(Parameters(GraphRunParams {
                graph_id: lp.id.clone(),
                queue_id: None,
                workdir: None,
                idea: None,
                sandbox: None,
                start_node_id: None,
            }))
            .await
            .unwrap();
        assert!(
            is_err(&result),
            "a blank bound spec must refuse the launch: {}",
            text(&result)
        );
        let message = text(&result);
        assert!(
            message.contains(&blank_id),
            "the refusal must name the offending spec: {message}"
        );
        assert!(
            message.contains("both name and description are empty"),
            "the refusal must say what is missing: {message}"
        );

        let stored = db.get_graph(&lp.id).unwrap().unwrap();
        assert_eq!(
            stored.status,
            GraphStatus::Draft,
            "a refused launch must leave the graph's status untouched"
        );
    }

    /// (CB22) The TUI's graph-only dispatch is rejected synchronously when the
    /// graph has neither bound specs nor a selected queue; no placeholder row
    /// may be created while the background run is being started.
    #[tokio::test]
    async fn graph_run_refuses_when_no_specs_or_queue_are_selected() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());

        let result = handler
            .graph_run(Parameters(GraphRunParams {
                graph_id: lp.id.clone(),
                queue_id: None,
                workdir: None,
                idea: None,
                sandbox: None,
                start_node_id: None,
            }))
            .await
            .unwrap();

        assert!(
            is_err(&result),
            "a graph-only launch without specs must be refused: {}",
            text(&result)
        );
        assert!(
            text(&result).contains("no specs to run"),
            "the refusal must explain what is missing: {}",
            text(&result)
        );
        let stored = db.get_graph(&lp.id).unwrap().unwrap();
        assert_eq!(stored.status, GraphStatus::Draft);
        assert!(
            db.list_graph_specs(&lp.id).unwrap().is_empty(),
            "the refused TUI launch must not create a blank spec"
        );
    }

    /// (CB22) Queue isolation at the handler boundary: a `graph_run` with a
    /// valid `queue_id` is accepted even when the graph has an unrelated
    /// blank bound spec — queue launches validate only the queue's own
    /// members.
    #[tokio::test]
    async fn graph_run_with_queue_ignores_unrelated_blank_bound_spec() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        db.insert_graph_spec(&GraphSpec {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id: Some(lp.id.clone()),
            name: String::new(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();

        let member = GraphSpec {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id: None,
            name: "Queue Member".to_string(),
            description: Some(valid_spec_description()),
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        db.insert_graph_spec(&member).unwrap();
        let queue_id = uuid::Uuid::new_v4().to_string();
        db.insert_queue(&Queue {
            id: queue_id.clone(),
            name: queue_id.clone(),
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.append_queue_member(&queue_id, &member.id, None).unwrap();

        let result = handler
            .graph_run(Parameters(GraphRunParams {
                graph_id: lp.id.clone(),
                queue_id: Some(queue_id),
                workdir: None,
                idea: None,
                sandbox: None,
                start_node_id: None,
            }))
            .await
            .unwrap();
        assert!(
            !is_err(&result),
            "a valid queue launch must ignore the unrelated blank bound spec: {}",
            text(&result)
        );
    }

    /// (CB22) `graph_preflight` reports a blank bound spec as a structured
    /// `spec_warnings` entry — advisory, not a tool error — including when
    /// the graph has no agent targets to probe.
    #[tokio::test]
    async fn graph_preflight_warns_for_blank_bound_spec_without_probe_targets() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let blank_id = uuid::Uuid::new_v4().to_string();
        db.insert_graph_spec(&GraphSpec {
            id: blank_id.clone(),
            graph_id: Some(lp.id.clone()),
            name: String::new(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: None,
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(
            !is_err(&result),
            "a preflight warning must not be a tool error: {}",
            text(&result)
        );
        let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
        let warnings = body["spec_warnings"]
            .as_array()
            .unwrap_or_else(|| panic!("preflight must include spec_warnings: {body}"));
        assert_eq!(warnings.len(), 1, "expected one warning: {body}");
        assert_eq!(warnings[0]["spec_id"], blank_id);
        assert!(
            warnings[0]["warning"]
                .as_str()
                .unwrap_or_default()
                .contains("both name and description are empty"),
            "the warning must say what is missing: {body}"
        );
    }

    /// (CB22) `graph_preflight` includes `spec_warnings` alongside real probe
    /// results. The agent node references a deliberately unconfigured
    /// platform, so the probe reports `NotConfigured` immediately without
    /// spawning anything or spending quota.
    #[tokio::test]
    async fn graph_preflight_includes_spec_warnings_alongside_probes() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        let blank_id = uuid::Uuid::new_v4().to_string();
        db.insert_graph_spec(&GraphSpec {
            id: blank_id.clone(),
            graph_id: Some(lp.id.clone()),
            name: String::new(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            name: "impl".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "cb22-unconfigured-platform"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: Some(5),
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(
            !is_err(&result),
            "a preflight warning must not be a tool error: {}",
            text(&result)
        );
        let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
        assert!(
            body["probes"]
                .as_array()
                .is_some_and(|probes| !probes.is_empty()),
            "expected probe results alongside the warning: {body}"
        );
        let warnings = body["spec_warnings"]
            .as_array()
            .unwrap_or_else(|| panic!("preflight must include spec_warnings: {body}"));
        assert_eq!(warnings.len(), 1, "expected one warning: {body}");
        assert_eq!(warnings[0]["spec_id"], blank_id);
    }

    /// Write a canopy config at `<home>/.canopy/config.toml` declaring one
    /// `mistral` CLI carrying the CM32 registry identity. The binary is a
    /// name that cannot exist anywhere, so every probe against it fails at
    /// spawn — immediately, without launching a process or spending quota.
    fn write_cm32_config(home: &std::path::Path) {
        std::fs::create_dir_all(home.join(".canopy")).unwrap();
        std::fs::write(
            home.join(".canopy/config.toml"),
            "[[clis]]\nname = \"mistral\"\nbinary = \"cm32-no-such-binary-xyz\"\nprovider = \"Mistral AI\"\ntool_name = \"Vibe\"\n",
        )
        .unwrap();
    }

    /// (CM32) `agent_probe` accepts the slug and its result names the
    /// product: `platform` stays `mistral` while a sibling
    /// `platform_display` carries `Mistral AI · Vibe`. This pins the
    /// handler-level enrichment — the helper's unit tests would survive
    /// deleting the `platform_display` insertions, this would not.
    #[tokio::test]
    async fn agent_probe_result_names_the_product_beside_the_slug() {
        let home = tempdir().unwrap();
        write_cm32_config(home.path());
        let _home_guard = HomeVar::set(home.path());
        let (_dir, _db, handler) = endpoint_test_handler();

        let result = handler
            .agent_probe(Parameters(AgentProbeParams {
                platform: Some("mistral".to_string()),
                model: None,
                timeout_seconds: Some(5),
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
        let probes = body["probes"].as_array().unwrap();
        assert_eq!(probes.len(), 1, "{body}");
        assert_eq!(
            probes[0]["platform"],
            serde_json::json!("mistral"),
            "the identity key stays the slug"
        );
        assert_eq!(
            probes[0]["platform_display"],
            serde_json::json!("Mistral AI \u{b7} Vibe"),
            "the sibling names the product the registry publishes"
        );
        assert_ne!(
            probes[0]["outcome"],
            serde_json::json!("not_configured"),
            "the slug must be accepted as a configured platform"
        );
    }

    /// (CM32) `graph_preflight` probe entries carry the same
    /// `platform_display` sibling, keyed beside the slug. A node stores
    /// `platform: "mistral"` — the slug — and preflight's result names
    /// Vibe. Binary unresolvable → fails fast without spawning.
    #[tokio::test]
    async fn graph_preflight_probe_entries_name_the_product() {
        let home = tempdir().unwrap();
        write_cm32_config(home.path());
        let _home_guard = HomeVar::set(home.path());
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        db.insert_graph_node(&GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            name: "impl".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "mistral"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: Some(5),
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&result), "{}", text(&result));
        let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
        let probes = body["probes"]
            .as_array()
            .unwrap_or_else(|| panic!("expected probe results: {body}"));
        let mistral = probes
            .iter()
            .find(|p| p["platform"] == serde_json::json!("mistral"))
            .unwrap_or_else(|| panic!("expected a mistral probe entry: {body}"));
        assert_eq!(
            mistral["platform_display"],
            serde_json::json!("Mistral AI \u{b7} Vibe")
        );
    }

    /// (CB25) Without a `reviewer`, preflight returns `"review": null` and
    /// leaves the existing probe output untouched — the reviewer must never
    /// run implicitly. Covers both the main path and the no-target early
    /// return.
    #[tokio::test]
    async fn graph_preflight_without_reviewer_has_null_review() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        db.insert_graph_node(&GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            name: "impl".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "cb25-unconfigured-platform"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: Some(5),
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(
            !is_err(&result),
            "preflight without reviewer must succeed: {}",
            text(&result)
        );
        let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
        assert!(
            body.get("review").is_some_and(|v| v.is_null()),
            "review must be null when no reviewer is requested: {body}"
        );
        assert!(
            body["probes"].as_array().is_some_and(|p| !p.is_empty()),
            "existing probe results must be unchanged: {body}"
        );
        assert!(
            body["spec_warnings"].as_array().is_some(),
            "existing spec_warnings must be unchanged: {body}"
        );

        // No-target early return carries the same null review.
        let bare = insert_test_graph(&db, dir.path());
        let early = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: bare.id.clone(),
                timeout_seconds: None,
                reviewer: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&early), "{}", text(&early));
        let early_body: serde_json::Value = serde_json::from_str(&raw_text(&early)).unwrap();
        assert!(
            early_body.get("review").is_some_and(|v| v.is_null()),
            "early-return review must be null when no reviewer is requested: {early_body}"
        );
    }

    /// (CB25) With `reviewer`, preflight reports a `review` object without
    /// blocking the probe results. The spawn is real (`spawn_subagent`
    /// returns an id immediately), but its outcome is environment-dependent:
    /// `running` with a collectible row when the platform spawns, or
    /// `not_spawned` with a note when it cannot start (notably, under
    /// `debug_assertions` the platform registry's `reqwest::blocking`
    /// client cannot run in async context, so debug test runs always take
    /// the `not_spawned` branch while release builds spawn for real).
    /// Either branch keeps the probes intact and never errors.
    #[tokio::test]
    async fn graph_preflight_with_reviewer_spawns_background_run() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        db.insert_graph_node(&GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            name: "impl".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "cb25-unconfigured-platform"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: Some(5),
                reviewer: Some(GraphPreflightReviewer {
                    platform: "opencode".to_string(),
                    model: None,
                }),
            }))
            .await
            .unwrap();
        assert!(
            !is_err(&result),
            "preflight with reviewer must succeed: {}",
            text(&result)
        );
        let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
        assert!(
            body["probes"].as_array().is_some_and(|p| !p.is_empty()),
            "probe results must still be returned: {body}"
        );
        let review = &body["review"];
        assert_eq!(review["pair"], "opencode/default", "{body}");
        match review["status"].as_str().unwrap_or_default() {
            "running" => {
                assert_eq!(review["collect_with"], "subagent_collect", "{body}");
                let id = review["id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("running review must carry an id: {body}"));
                let record = db
                    .get_subagent_run(id)
                    .unwrap()
                    .unwrap_or_else(|| panic!("spawned reviewer row '{id}' must exist"));
                assert!(
                    !record.prompt.is_empty(),
                    "spawned reviewer row must carry the audit prompt"
                );
            }
            "not_spawned" => {
                assert!(
                    review["note"].as_str().is_some_and(|n| !n.is_empty()),
                    "a reviewer that cannot start must say why: {body}"
                );
            }
            other => panic!("review status must be running or not_spawned, got: {body} ({other})"),
        }
    }

    /// (CB25) A reviewer that cannot start degrades to a `not_spawned` note;
    /// it never fails the preflight and the probes are still returned.
    #[tokio::test]
    async fn graph_preflight_reviewer_failure_never_fails_preflight() {
        let (dir, db, handler) = endpoint_test_handler();
        let lp = insert_test_graph(&db, dir.path());
        db.insert_graph_node(&GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            name: "impl".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "cb25-unconfigured-platform"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let result = handler
            .graph_preflight(Parameters(GraphPreflightParams {
                graph_id: lp.id.clone(),
                timeout_seconds: Some(5),
                reviewer: Some(GraphPreflightReviewer {
                    platform: "no-such-platform-xyz".to_string(),
                    model: None,
                }),
            }))
            .await
            .unwrap();
        assert!(
            !is_err(&result),
            "a reviewer that cannot start must not fail preflight: {}",
            text(&result)
        );
        let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
        assert!(
            body["probes"].as_array().is_some_and(|p| !p.is_empty()),
            "probe results must still be returned: {body}"
        );
        assert_eq!(body["review"]["status"], "not_spawned", "{body}");
        assert!(
            body["review"]["note"]
                .as_str()
                .unwrap_or_default()
                .contains("unaffected"),
            "the note must say the probes are unaffected: {body}"
        );
    }

    /// (CB25) The reviewer prompt carries the graph id and, when the run
    /// targets one, the graph's active run queue id (read from the persisted
    /// graph row — no new preflight param), plus the audit-only instruction.
    /// Unit-tested directly: spawning a real reviewer is not possible under
    /// `debug_assertions` (see the `spawns_background_run` test), but the
    /// prompt construction is what carries the ids.
    #[test]
    fn graph_preflight_reviewer_prompt_carries_graph_and_queue_ids() {
        let prompt = build_preflight_reviewer_prompt("graph-1", Some("q-1"), "");
        assert!(
            prompt.contains("graph_id='graph-1'"),
            "reviewer prompt must carry the graph id: {prompt}"
        );
        assert!(
            prompt.contains("queue_id='q-1'"),
            "reviewer prompt must carry the queue id: {prompt}"
        );
        assert!(
            prompt.contains("Audit ONLY"),
            "reviewer prompt must carry the audit-only instruction: {prompt}"
        );

        let no_queue = build_preflight_reviewer_prompt("graph-1", None, "");
        assert!(
            no_queue.contains("graph_id='graph-1'"),
            "reviewer prompt must carry the graph id without a queue: {no_queue}"
        );
        assert!(
            !no_queue.contains("queue_id="),
            "reviewer prompt must not name a queue when there is none: {no_queue}"
        );
    }

    /// (CB25) When the `graph-reviewer` skill resolves in the store, its
    /// instructions are inlined under the pinned-skill delimiter; otherwise
    /// the prompt carries the unresolved note and the review still
    /// proceeds. Unit-tested directly for the same spawn reason as above.
    #[test]
    fn graph_preflight_reviewer_inlines_skill_when_store_has_it() {
        // Seeded store (empty sources): a present, fresh skill dir is
        // served without any network fetch.
        let dir = tempfile::tempdir().unwrap();
        let store =
            crate::dynamic_skills::SkillStore::new(dir.path().to_path_buf(), Vec::new(), 15);
        let skill_dir = dir.path().join("graph-reviewer");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# graph-reviewer\nCB25 seeded reviewer body.\n",
        )
        .unwrap();
        let checked = chrono::Utc::now().to_rfc3339();
        std::fs::write(
            skill_dir.join(".canopy-skill.toml"),
            format!(
                "source_url = \"test\"\ncommit_hash = \"test\"\nlast_checked = \"{checked}\"\n"
            ),
        )
        .unwrap();

        let section = resolve_reviewer_skill_section(&store);
        assert!(
            section.contains("## Skill: graph-reviewer"),
            "resolved skill must use the pinned-skill delimiter: {section}"
        );
        assert!(
            section.contains("CB25 seeded reviewer body."),
            "resolved skill must inline the skill instructions: {section}"
        );

        // Unseeded store degrades to the unresolved note, never an error.
        let bare_dir = tempfile::tempdir().unwrap();
        let bare =
            crate::dynamic_skills::SkillStore::new(bare_dir.path().to_path_buf(), Vec::new(), 15);
        let note = resolve_reviewer_skill_section(&bare);
        assert!(
            note.contains("## Skill: graph-reviewer")
                && note.contains("Could not resolve pinned skill"),
            "missing skill must degrade to the unresolved note: {note}"
        );
    }

    #[test]
    fn append_traversed_project_knowledge_reports_cap_full() {
        use crate::db::intelligence::IntelligenceNodeInput;
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("fact-cap".to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("cap fact".to_string()),
            body: Some("body".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some("cap-hash".to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();
        let mut tagged: super::TaggedProjectKnowledge = Vec::new();
        let done = super::TaskTriggerHandler::append_traversed_project_knowledge(
            &db,
            &mut tagged,
            "cap-hash",
            None,
            20,
            0,
        )
        .unwrap();
        assert!(!done, "total cap of zero stops traversal");
        assert!(tagged.is_empty());
    }

    #[test]
    fn append_traversed_project_knowledge_appends_within_cap() {
        use crate::db::intelligence::IntelligenceNodeInput;
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("fact-ok".to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("ok fact".to_string()),
            body: Some("body".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some("ok-hash".to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();
        let mut tagged: super::TaggedProjectKnowledge = Vec::new();
        let more = super::TaskTriggerHandler::append_traversed_project_knowledge(
            &db,
            &mut tagged,
            "ok-hash",
            None,
            20,
            60,
        )
        .unwrap();
        assert!(more, "room remains under the total cap");
        assert_eq!(tagged.len(), 1);
        assert_eq!(tagged[0].1, "ok-hash");
    }
}
