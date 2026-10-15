//! CM50: the `graph_crew` MCP tool — "which graphs use this harness or
//! model?" answered from the same read path the CLI uses, with no prompt
//! text in the answer.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::graph_crew::GraphCrewDbParams;
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_graph_crew, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_crew",
        description = "List every graph's harness/model assignments: agent nodes outside ensembles, ensemble members, and agent-type hooks — each with platform, model, timeout and, when set, crew (CM49) and availability (CM48). Answers 'which graphs use this harness or model?' without reading SQLite. Filter with graph_id (one graph), platform (exact) or model (case-insensitive substring); include_archived widens past the archive. With group_by: \"pair\", returns one summary row per distinct platform/model pair with graph/ensemble/node/hook counts and the graph names. No prompt text is ever returned."
    )]
    pub(crate) async fn graph_crew(
        &self,
        Parameters(params): Parameters<GraphCrewParams>,
    ) -> Result<CallToolResult, McpError> {
        let db_params = GraphCrewDbParams {
            graph_id: params.graph_id.as_deref(),
            platform: params.platform.as_deref(),
            model: params.model.as_deref(),
            include_archived: params.include_archived.unwrap_or(false),
        };
        let json = match params.group_by.as_deref() {
            Some("pair") => serde_json::to_string_pretty(
                &self
                    .db
                    .graph_crew_summary(db_params)
                    .map_err(internal_error)?,
            ),
            Some(other) => {
                return Ok(error_result(&format!(
                    "group_by must be \"pair\"; got \"{other}\"."
                )));
            }
            None => serde_json::to_string_pretty(
                &self.db.graph_crew(db_params).map_err(internal_error)?,
            ),
        };
        Ok(success_result(&json.unwrap_or_default()))
    }
}
