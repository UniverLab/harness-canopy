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
pub(crate) use crate::daemon::handler_copy::{node_copy_note, plan_ensemble_copy, plan_node_copy};
#[cfg(test)]
pub(crate) use crate::daemon::handler_edges::resolve_graph_target;
#[cfg(test)]
pub(crate) use crate::daemon::handler_ensemble_core::{
    build_ensemble_unit, effective_member_prompt, member_node_config, validate_ensemble_members,
    BuiltEnsembleUnit, EnsembleUnitSpec,
};
#[cfg(test)]
pub(crate) use crate::daemon::handler_formatting::transport_details;
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
pub(crate) use crate::daemon::handler_node_lib::resolve_node_kind_and_config;
#[cfg(test)]
pub(crate) use crate::daemon::handler_nodes::unknown_config_keys;
#[cfg(test)]
pub(crate) use crate::daemon::handler_preflight_review::{
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
pub(crate) use crate::daemon::handler_specs_helpers::{
    spec_summary_json, validate_at_least_one_bool,
};
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

// Test seams below construct per-tool parameter newtypes; production code
// reaches them only through the `#[tool]` methods' own extractor.
#[cfg(test)]
use crate::daemon::handler_graph_tools::{
    GraphControlObject, GraphEdgeObject, GraphEditObject, GraphEnsembleObject, GraphNodeObject,
    GraphObject,
};
#[cfg(test)]
use crate::daemon::handler_resource_tools::{
    AgentEditObject, AgentObject, ConfigObject, CrewObject, HarnessObject, QueueObject,
    SpecEditObject, SpecObject,
};
#[cfg(test)]
use crate::daemon::handler_resource_tools_final::{
    CatalogueObject, IdentityObject, IntelligenceAdminObject, NodeObject, ProjectObject, RagObject,
    ScheduledPromptObject, SkillObject, SubagentObject,
};
#[cfg(test)]
use crate::daemon::handler_sandbox::SandboxObject;

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
/// `intelligence_graph_walk` and `spec list` read this constant; the next
/// surface that needs paging should reuse it instead of a new hardcoded
/// number.
pub(crate) const MCP_RESULT_BUDGET_BYTES: usize = 32_000;

/// Measured serialised size of one compact `spec list` row in bytes: 339
/// compact specs measured 55,605 characters on 2026-09-02, i.e. ~164
/// bytes/row. Budgeted at 200 bytes/row so the default page keeps headroom
/// against longer-than-average names.
const SPEC_LIST_COMPACT_ROW_BYTES: usize = 280;

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

/// Default `spec list` page derived from the budget, not a round number:
/// `MCP_RESULT_BUDGET_BYTES / SPEC_LIST_COMPACT_ROW_BYTES`, clamped to
/// `[1, 200]` like `graph` (the quotient is 160, so the upper
/// clamp is vacuous and the lower clamp is enforced by the const assert).
pub(crate) const SPEC_LIST_DEFAULT_LIMIT: u32 =
    MCP_RESULT_BUDGET_BYTES as u32 / SPEC_LIST_COMPACT_ROW_BYTES as u32;

const _: () = assert!(
    SPEC_LIST_DEFAULT_LIMIT >= 1 && SPEC_LIST_DEFAULT_LIMIT <= 200,
    "spec list default page must stay within [1, 200]"
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
    /// Shared RAG ingestion manager: rag_search must acquire its embedding
    /// client through this cache (B22) so queries reuse the loaded model and
    /// keep the persisted model-loaded status truthful.
    pub ingestion: Arc<crate::rag::ingestion::IngestionManager>,
    /// Rate limiters for `rag` `search` (10 calls/min). Keyed by agent_id.
    pub rag_limiters: Arc<tokio::sync::Mutex<std::collections::HashMap<String, RateLimiter>>>,
    /// Backing store for `skill` `list`/`get` (`~/.canopy/skills/`).
    /// Git operations block, so callers run it via `spawn_blocking`.
    pub dynamic_skills: Arc<crate::dynamic_skills::SkillStore>,
    /// The session shelf's bytes + rows owner (`session_send_file`).
    pub(crate) shelf: Arc<crate::sessions::shelf::ShelfStore>,
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
    pub ingestion: Arc<crate::rag::ingestion::IngestionManager>,
    pub dynamic_skills: Arc<crate::dynamic_skills::SkillStore>,
    pub shelf: Arc<crate::sessions::shelf::ShelfStore>,
    /// CB83: the daemon process start instant, captured once when the
    /// daemon starts (see `run_http_server` / `stdio_server_startup` in
    /// `server.rs`) and handed to every handler. `agent_status` `Uptime`
    /// must report the daemon's age, not the MCP session's.
    pub start_time: std::time::Instant,
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
            ingestion,
            dynamic_skills,
            shelf,
            start_time,
            port,
        } = deps;

        Self {
            db,
            executor,
            watcher_engine,
            scheduler_notify,
            graph_engine,
            notification_service,
            ingestion,
            rag_limiters: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            dynamic_skills,
            shelf,
            start_time,
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
        router.merge(TaskTriggerHandler::tool_router_sandbox());
        router.merge(TaskTriggerHandler::tool_router_shelf());
        router.merge(TaskTriggerHandler::tool_router_resource_tools());
        router.merge(TaskTriggerHandler::tool_router_resource_tools_final());
        router.merge(TaskTriggerHandler::tool_router_intel());
        router.merge(TaskTriggerHandler::tool_router_graphs());
        router.merge(TaskTriggerHandler::tool_router_graph_tools());
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
        router.merge(TaskTriggerHandler::tool_router_remote());
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
const HIDDEN_DEPRECATED_TOOLS: &[&str] = &["loop_complete_node", "loop_report_blocker"];

/// Fixed toolset handed to graph-node sessions so they stay far under the
/// 128-tool provider cap regardless of how many tools canopy adds later.
pub const NODE_TOOLS: &[&str] = &[
    "spec",
    "spec_edit",
    "graph_complete_node",
    "graph_report_blocker",
    "intelligence_get_context",
    "intelligence_search",
    "intelligence_upsert",
    "intelligence_admin",
    "rag",
    "skill",
    "identity",
    "get_tools",
];

fn node_tool_list() -> Vec<Tool> {
    visible_tool_list()
        .into_iter()
        .filter(|t| NODE_TOOLS.contains(&t.name.as_ref()))
        .collect()
}

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
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        // Header-then-env resolution, mirroring resolve_sync_agent_id: an HTTP
        // transport carries the profile in a header; the stdio path (including
        // the embedded server) carries it in the environment.
        let profile = (|| {
            if let Some(parts) = context.extensions.get::<axum::http::request::Parts>() {
                if let Some(value) = crate::shared::sync_identity::header_str(
                    parts,
                    crate::shared::sync_identity::CANOPY_MCP_PROFILE_HEADER,
                ) {
                    return Some(value.to_string());
                }
            }
            std::env::var(crate::shared::sync_identity::CANOPY_MCP_PROFILE_ENV)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })();
        let tools = if profile.as_deref() == Some("node") {
            node_tool_list()
        } else {
            visible_tool_list()
        };
        Ok(rmcp::model::ListToolsResult {
            tools,
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
                 Use agent_edit (add) to create scheduled agents, agent_edit (watch) for file watchers, \
                 agent_edit (run) to test immediately, and agent (status) for daemon health."
                    .to_string(),
            )
    }
}

#[cfg(test)]
mod readme_docs_tool_sync;

#[cfg(test)]
mod removed_names_tests;

/// Test-only dispatch seam: route a ported endpoint-test call through one of
/// the six graph resource tools exactly as an MCP client would — the typed
/// params struct is serialized, `action` is injected, and the call goes
/// through the tool's own per-tool parameter newtype validation before
/// reaching the
/// same `_impl` logic the old one-operation tools held.
#[cfg(test)]
pub(crate) async fn call_graph_action<P: serde::Serialize>(
    handler: &TaskTriggerHandler,
    tool: &str,
    action: &str,
    params: P,
) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
    let mut value = serde_json::to_value(&params).expect("params must serialize");
    if value.is_null() {
        value = serde_json::json!({});
    }
    // A typed `None` serializes as JSON `null`, which the wire cannot
    // distinguish from an explicit "clear it" null on double-option fields
    // — and every ported call site uses `None` for "omit". Explicit-null
    // tests bypass this helper and send the raw JSON object.
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, v| !v.is_null());
    }
    value["action"] = serde_json::Value::String(action.to_string());
    let object = value.as_object().cloned().expect("object params");
    let object = crate::daemon::actions::ActionObject(object);
    match tool {
        "graph" => {
            handler
                .graph(crate::daemon::params_extract::Parameters(GraphObject(
                    object,
                )))
                .await
        }
        "graph_edit" => {
            handler
                .graph_edit(crate::daemon::params_extract::Parameters(GraphEditObject(
                    object,
                )))
                .await
        }
        "graph_node" => {
            handler
                .graph_node(crate::daemon::params_extract::Parameters(GraphNodeObject(
                    object,
                )))
                .await
        }
        "graph_edge" => {
            handler
                .graph_edge(crate::daemon::params_extract::Parameters(GraphEdgeObject(
                    object,
                )))
                .await
        }
        "graph_ensemble" => {
            handler
                .graph_ensemble(crate::daemon::params_extract::Parameters(
                    GraphEnsembleObject(object),
                ))
                .await
        }
        "graph_control" => {
            handler
                .graph_control(crate::daemon::params_extract::Parameters(
                    GraphControlObject(object),
                ))
                .await
        }
        other => panic!("unknown graph resource tool '{other}'"),
    }
}

/// Identity wrapper keeping call-site parentheses balanced when a ported
/// test swaps `old_tool(Parameters(params))` for
/// `call_graph_action(&handler, tool, action, params_id(params))`.
#[cfg(test)]
pub(crate) fn params_id<T>(value: T) -> T {
    value
}

/// Explicit-null variant of [`call_graph_action`]: sends a raw JSON object
/// (nulls kept — a null is a "clear it" instruction on double-option
/// fields) through the tool's own dispatch.
#[cfg(test)]
pub(crate) async fn call_graph_action_value(
    handler: &TaskTriggerHandler,
    tool: &str,
    action: &str,
    mut params: serde_json::Value,
) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
    params["action"] = serde_json::Value::String(action.to_string());
    let object = params
        .as_object()
        .cloned()
        .expect("object params must serialize");
    let object = crate::daemon::actions::ActionObject(object);
    match tool {
        "graph" => {
            handler
                .graph(crate::daemon::params_extract::Parameters(GraphObject(
                    object,
                )))
                .await
        }
        "graph_edit" => {
            handler
                .graph_edit(crate::daemon::params_extract::Parameters(GraphEditObject(
                    object,
                )))
                .await
        }
        "graph_node" => {
            handler
                .graph_node(crate::daemon::params_extract::Parameters(GraphNodeObject(
                    object,
                )))
                .await
        }
        "graph_edge" => {
            handler
                .graph_edge(crate::daemon::params_extract::Parameters(GraphEdgeObject(
                    object,
                )))
                .await
        }
        "graph_ensemble" => {
            handler
                .graph_ensemble(crate::daemon::params_extract::Parameters(
                    GraphEnsembleObject(object),
                ))
                .await
        }
        "graph_control" => {
            handler
                .graph_control(crate::daemon::params_extract::Parameters(
                    GraphControlObject(object),
                ))
                .await
        }
        other => panic!("unknown graph resource tool '{other}'"),
    }
}

/// Test-only dispatch seam for the eighteen second- and final-wave resource
/// tools (`spec`, `spec_edit`, `queue`, `agent`, `agent_edit`, `harness`,
/// `config`, `crew`, `node`, `intelligence_admin`, `project`,
/// `scheduled_prompts`, `sandbox`, `skill`, `subagent`, `rag`, `identity`,
/// `catalogue`): the
/// typed params struct is serialized with nulls stripped, `action` is injected,
/// and the call goes through the tool's own per-tool parameter newtype
/// validation before reaching the same `_impl` logic the old one-operation
/// tools held. Tools that take a caller session run without one, like the
/// ported tests' old `OptionalExtension(None)`.
#[cfg(test)]
pub(crate) async fn call_resource_action<P: serde::Serialize>(
    handler: &TaskTriggerHandler,
    tool: &str,
    action: &str,
    params: P,
) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
    let mut value = serde_json::to_value(&params).expect("params must serialize");
    if value.is_null() {
        value = serde_json::json!({});
    }
    // Same null-stripping contract as [`call_graph_action`]: a typed `None`
    // means "omit", never an explicit "clear it" null.
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, v| !v.is_null());
    }
    call_resource_action_value(handler, tool, action, value).await
}

/// Explicit-null variant of [`call_resource_action`]: sends a raw JSON
/// object (nulls kept) through the tool's own dispatch.
#[cfg(test)]
pub(crate) async fn call_resource_action_value(
    handler: &TaskTriggerHandler,
    tool: &str,
    action: &str,
    mut params: serde_json::Value,
) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
    params["action"] = serde_json::Value::String(action.to_string());
    let object = params
        .as_object()
        .cloned()
        .expect("object params must serialize");
    let object = crate::daemon::actions::ActionObject(object);
    match tool {
        "spec" => {
            handler
                .spec(crate::daemon::params_extract::Parameters(SpecObject(
                    object,
                )))
                .await
        }
        "spec_edit" => {
            handler
                .spec_edit(crate::daemon::params_extract::Parameters(SpecEditObject(
                    object,
                )))
                .await
        }
        "queue" => {
            handler
                .queue(crate::daemon::params_extract::Parameters(QueueObject(
                    object,
                )))
                .await
        }
        "agent" => {
            handler
                .agent(crate::daemon::params_extract::Parameters(AgentObject(
                    object,
                )))
                .await
        }
        "agent_edit" => {
            handler
                .agent_edit(
                    crate::daemon::params_extract::Parameters(AgentEditObject(object)),
                    OptionalExtension(None),
                )
                .await
        }
        "harness" => {
            handler
                .harness(crate::daemon::params_extract::Parameters(HarnessObject(
                    object,
                )))
                .await
        }
        "config" => {
            handler
                .config(crate::daemon::params_extract::Parameters(ConfigObject(
                    object,
                )))
                .await
        }
        "crew" => {
            handler
                .crew(crate::daemon::params_extract::Parameters(CrewObject(
                    object,
                )))
                .await
        }
        "node" => {
            handler
                .node(crate::daemon::params_extract::Parameters(NodeObject(
                    object,
                )))
                .await
        }
        "intelligence_admin" => {
            handler
                .intelligence_admin(
                    crate::daemon::params_extract::Parameters(IntelligenceAdminObject(object)),
                    OptionalExtension(None),
                )
                .await
        }
        "project" => {
            handler
                .project(crate::daemon::params_extract::Parameters(ProjectObject(
                    object,
                )))
                .await
        }
        "scheduled_prompts" => {
            handler
                .scheduled_prompts(
                    crate::daemon::params_extract::Parameters(ScheduledPromptObject(object)),
                    OptionalExtension(None),
                )
                .await
        }
        "sandbox" => {
            handler
                .sandbox(crate::daemon::params_extract::Parameters(SandboxObject(
                    object,
                )))
                .await
        }
        "skill" => {
            handler
                .skill(crate::daemon::params_extract::Parameters(SkillObject(
                    object,
                )))
                .await
        }
        "subagent" => {
            handler
                .subagent(
                    crate::daemon::params_extract::Parameters(SubagentObject(object)),
                    OptionalExtension(None),
                )
                .await
        }
        "rag" => {
            handler
                .rag(
                    crate::daemon::params_extract::Parameters(RagObject(object)),
                    OptionalExtension(None),
                )
                .await
        }
        "identity" => {
            handler
                .identity(
                    crate::daemon::params_extract::Parameters(IdentityObject(object)),
                    OptionalExtension(None),
                )
                .await
        }
        "catalogue" => {
            handler
                .catalogue(crate::daemon::params_extract::Parameters(CatalogueObject(
                    object,
                )))
                .await
        }
        other => panic!("unknown resource tool '{other}'"),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod additional_tests;

// ── Additional coverage tests for handler.rs ─────────────────────
// Tests targeting uncovered functions: transport_details,
// append_temporal_agents_section, graph_run_blocker, graph_node_json,
// graph_edge_json, graph_run_json, ensemble_details_json,
// graph_completion_hook_json, graph_completion_hook_run_json,
// format_system_time, model_result_footer, build_ensemble_unit,
// and edge-case branches across validation functions.
#[cfg(test)]
mod coverage_tests;

// ── Direct #[tool] handler-method coverage ───────────────────────────
//
// The three test modules above almost exclusively exercise *pure* helper
// functions (validate_*, build_*, format_*). The `#[tool]` methods on
// `TaskTriggerHandler` itself — `agent_edit`, `intelligence_*`,
// `graph_*`, `spec_*`, etc. — are called
// directly here instead: construct `Parameters<...>`, call the method on a
// real handler wired to a real (tempdir) SQLite database, assert on the
// `CallToolResult` and on the resulting DB state. This is the surface a
// live MCP client actually calls, and where most of this file's uncovered
// lines live (error branches inside each handler in particular).
#[cfg(test)]
mod endpoint_tests;
