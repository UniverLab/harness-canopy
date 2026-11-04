//! CM50: the `crew assignments` action — "which graphs use this harness or
//! model?" answered from the same read path the CLI uses, with no prompt
//! text in the answer.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::db::graph_crew::GraphCrewDbParams;
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

impl TaskTriggerHandler {
    pub(crate) async fn crew_assignments_impl(
        &self,
        params: GraphCrewParams,
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
