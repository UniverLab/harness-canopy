//! MCP tools for named model entries (`crew model ...`).
//!
//! A model is named once and crews reference it; `model_set` rewrites the
//! materialized pair on every tracking member and bumps each touched crew
//! exactly once, then eagerly syncs the bound ensembles of idle graphs the
//! same way `crew update` does.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

impl TaskTriggerHandler {
    /// Eagerly sync every ensemble bound to `crew_name`, with the exact rule
    /// `crew_update_impl` has always used: never touch a bound ensemble
    /// whose graph is running or pausing — say so in the reply (`sync`
    /// entry `DeferredRunningGraph`, `applies at next dispatch`) so the
    /// ensemble lags and applies the newer version at its next dispatch.
    pub(crate) async fn sync_crew_bound_ensembles(
        &self,
        crew_name: &str,
    ) -> Result<Vec<serde_json::Value>, McpError> {
        let mut sync = Vec::new();
        for (ensemble_id, status) in self
            .db
            .crew_bound_ensembles(crew_name)
            .map_err(internal_error)?
        {
            if status == "running" || status == "pausing" {
                sync.push(serde_json::json!({
                    "ensemble_id": ensemble_id,
                    "outcome": "DeferredRunningGraph",
                    "note": "applies at next dispatch",
                }));
                continue;
            }
            let outcome = self
                .db
                .sync_ensemble_with_crew(&ensemble_id)
                .map_err(internal_error)?;
            sync.push(serde_json::json!({
                "ensemble_id": ensemble_id,
                "outcome": format!("{outcome:?}"),
            }));
        }
        Ok(sync)
    }

    pub(crate) async fn crew_model_set_impl(
        &self,
        params: CrewModelSetParams,
    ) -> Result<CallToolResult, McpError> {
        let outcome =
            match self
                .db
                .crew_model_apply(&params.name, &params.platform, params.model.as_deref())
            {
                Ok(outcome) => outcome,
                Err(e) => return Ok(error_result(&e.to_string())),
            };
        if !outcome.changed {
            return Ok(success_result(
                &serde_json::to_string_pretty(&serde_json::json!({
                    "name": outcome.model.name,
                    "platform": outcome.model.platform,
                    "model": outcome.model.model,
                    "crews_updated": [],
                    "sync": [],
                }))
                .unwrap_or_default(),
            ));
        }
        let mut sync = Vec::new();
        for crew_name in &outcome.crews_updated {
            sync.extend(self.sync_crew_bound_ensembles(crew_name).await?);
        }
        Ok(success_result(
            &serde_json::to_string_pretty(&serde_json::json!({
                "name": outcome.model.name,
                "platform": outcome.model.platform,
                "model": outcome.model.model,
                "crews_updated": outcome.crews_updated,
                "sync": sync,
            }))
            .unwrap_or_default(),
        ))
    }

    pub(crate) async fn crew_model_list_impl(&self) -> Result<CallToolResult, McpError> {
        let entries = self.db.crew_model_list().map_err(internal_error)?;
        let body: Vec<serde_json::Value> = entries
            .iter()
            .map(|(model, crews)| {
                serde_json::json!({
                    "name": model.name,
                    "platform": model.platform,
                    "model": model.model,
                    "crews": crews,
                })
            })
            .collect();
        Ok(success_result(
            &serde_json::to_string_pretty(&body).unwrap_or_default(),
        ))
    }

    pub(crate) async fn crew_model_delete_impl(
        &self,
        params: CrewModelDeleteParams,
    ) -> Result<CallToolResult, McpError> {
        match self.db.crew_model_delete(&params.name) {
            Ok(()) => Ok(success_result(&format!(
                "Model '{}' deleted.",
                params.name.trim()
            ))),
            Err(e) => Ok(error_result(&e.to_string())),
        }
    }
}
