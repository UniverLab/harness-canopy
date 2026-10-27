//! MCP tool for named mutexes: the read surface over `mutex_leases` and
//! `mutex_waiters`. One tool (`mutex`) with an `action` parameter — today
//! only `list` — so later actions (if any) extend the same surface.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_mutex, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "mutex",
        description = "Inspect named mutexes nodes hold while they run (`mutex: [\"cpu\"]` on a node or ensemble config). Action `list` returns every granted lease (resource, holder run/graph/node, acquired_at) and every queued waiter (run/graph/node, resources, requested_at) in FIFO order."
    )]
    pub(crate) async fn mutex(
        &self,
        Parameters(params): Parameters<MutexParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.action != "list" {
            return Ok(error_result(&format!(
                "Unknown mutex action '{}'. Supported actions: list.",
                params.action
            )));
        }
        let state = self.db.list_mutex_state().map_err(internal_error)?;
        Ok(success_result(
            &serde_json::to_string_pretty(&serde_json::json!({
                "leases": state.leases,
                "waiters": state.waiters,
            }))
            .unwrap_or_default(),
        ))
    }
}
