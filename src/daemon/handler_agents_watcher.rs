//! Watcher-restart helper for agent updates, split out of
//! `daemon::handler_agents` to keep that file under the size gate.
//!
//! Behaviour unchanged: after an agent update, restart its watcher under the
//! (possibly new) id when the agent is a watch agent.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_helpers::watcher_restart_needed;
use crate::daemon::helpers::success_result;
use crate::daemon::params::TaskUpdateParams;
use crate::domain::models::Agent;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::ErrorData as McpError;

impl TaskTriggerHandler {
    pub(crate) async fn restart_updated_watcher(
        &self,
        params: &TaskUpdateParams,
        agent: &Agent,
    ) -> Result<Option<CallToolResult>, McpError> {
        // A rename always needs the watcher to end up running under the new
        // id, even if no other watch-related field changed.
        let renamed = agent.id != params.id;
        if !agent.is_watch() || (!watcher_restart_needed(params) && !renamed) {
            return Ok(None);
        }

        // The watcher for a renamed agent may already have been stopped
        // under the old id before the rename; stopping it again here is a
        // harmless no-op. Backing graphs are keyed by agent id.
        let _ = self.watcher_engine.stop_watcher(&params.id).await;
        let _ = self.watcher_engine.stop_graph_watcher(&agent.id).await;
        if !agent.enabled {
            return Ok(None);
        }

        let Some(backing) = self.db.get_graph(&agent.id).map_err(internal_error)? else {
            return Ok(Some(CallToolResult::success(vec![ContentBlock::text(format!(
                "Agent '{}' updated but watcher failed to restart: backing graph missing. It will be retried on daemon restart.",
                agent.id
            ))])));
        };
        let Err(e) = self.watcher_engine.start_graph_watcher(&backing).await else {
            return Ok(Some(success_result(&format!(
                "Agent '{}' updated successfully. Watcher restarted with new configuration.",
                agent.id
            ))));
        };

        Ok(Some(CallToolResult::success(vec![ContentBlock::text(format!(
            "Agent '{}' updated but watcher failed to restart: {}. It will be retried on daemon restart.",
            agent.id, e
        ))])))
    }
}
