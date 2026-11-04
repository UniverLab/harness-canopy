//! CM49: MCP tools for crews — named, ordered member lists that ensembles,
//! agent nodes and agent hooks bind to.
//!
//! Member order is meaningful: `cascade` tries members in order,
//! `round_robin` rotates through them, and a bound agent node or hook takes
//! the first member the roster reports available.
//!
//! These tools are the only write path (besides the CLI, which calls them) to
//! the `crews`/`crew_members` tables.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::domain::crews::CrewMember;
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

fn member_json(member: &CrewMember) -> serde_json::Value {
    serde_json::json!({
        "position": member.position,
        "platform": member.platform,
        "model": member.model,
        "timeout_minutes": member.timeout_minutes,
        "infra_retry_limit": member.infra_retry_limit,
        "infra_crash_max_seconds": member.infra_crash_max_seconds,
        "infra_backoff_seconds": member.infra_backoff_seconds,
    })
}

impl TaskTriggerHandler {
    pub(crate) async fn crew_list_impl(&self) -> Result<CallToolResult, McpError> {
        let crews = self.db.crew_list().map_err(internal_error)?;
        let counts = self.db.crew_binding_counts().map_err(internal_error)?;
        let body: Vec<serde_json::Value> = crews
            .iter()
            .map(|(crew, members)| {
                let count = counts.get(&crew.id).copied().unwrap_or_default();
                serde_json::json!({
                    "name": crew.name,
                    "description": crew.description,
                    "version": crew.version,
                    "members": members.iter().map(member_json).collect::<Vec<_>>(),
                    "bindings": {
                        "ensembles": count.ensembles,
                        "nodes": count.nodes,
                        "hooks": count.hooks,
                    },
                })
            })
            .collect();
        Ok(success_result(
            &serde_json::to_string_pretty(&body).unwrap_or_default(),
        ))
    }

    pub(crate) async fn crew_get_impl(
        &self,
        params: CrewGetParams,
    ) -> Result<CallToolResult, McpError> {
        let Some((crew, members)) = self.db.crew_get(&params.name).map_err(internal_error)? else {
            return Ok(error_result(&format!("crew '{}' not found.", params.name)));
        };
        let bindings = self
            .db
            .crew_bindings(&params.name)
            .map_err(internal_error)?;
        let binding_json: Vec<serde_json::Value> = bindings
            .iter()
            .map(|binding| {
                serde_json::json!({
                    "graph": binding.graph,
                    "kind": binding.kind.as_str(),
                    "name": binding.name,
                })
            })
            .collect();
        Ok(success_result(
            &serde_json::to_string_pretty(&serde_json::json!({
                "name": crew.name,
                "description": crew.description,
                "version": crew.version,
                "members": members.iter().map(member_json).collect::<Vec<_>>(),
                "bindings": binding_json,
            }))
            .unwrap_or_default(),
        ))
    }

    pub(crate) async fn crew_create_impl(
        &self,
        params: CrewCreateParams,
    ) -> Result<CallToolResult, McpError> {
        match (params.members.as_ref(), params.from_ensemble.as_ref()) {
            (Some(_), Some(_)) => Ok(error_result(
                "pass crew members or from_ensemble, never both.",
            )),
            (None, None) => Ok(error_result("pass either crew members or from_ensemble.")),
            (Some(members), None) => {
                let members = members_from_json(members.clone());
                match self
                    .db
                    .crew_create(&params.name, params.description.as_deref(), &members)
                {
                    Ok(crew) => Ok(success_result(&format!(
                        "Crew '{}' created with {} member(s), version {}.",
                        crew.name,
                        members.len(),
                        crew.version
                    ))),
                    Err(e) => Ok(error_result(&e.to_string())),
                }
            }
            (None, Some(ensemble_id)) => match self.db.crew_create_from_ensemble(
                &params.name,
                params.description.as_deref(),
                ensemble_id,
            ) {
                Ok(crew) => Ok(success_result(&format!(
                    "Crew '{}' created from ensemble '{}', version {}.",
                    crew.name, ensemble_id, crew.version
                ))),
                Err(e) => Ok(error_result(&e.to_string())),
            },
        }
    }

    pub(crate) async fn crew_update_impl(
        &self,
        params: CrewUpdateParams,
    ) -> Result<CallToolResult, McpError> {
        let members = params
            .members
            .as_ref()
            .map(|m| members_from_json(m.clone()));
        let crew = match self.db.crew_update(
            &params.name,
            members.as_deref(),
            params.description.as_deref(),
            params.rename.as_deref(),
        ) {
            Ok(crew) => crew,
            Err(e) => return Ok(error_result(&e.to_string())),
        };
        let mut sync = Vec::new();
        for (ensemble_id, status) in self
            .db
            .crew_bound_ensembles(&crew.name)
            .map_err(internal_error)?
        {
            // Never touch a bound ensemble whose graph is running or pausing —
            // replacing members under an in-flight dispatch is forbidden.
            if status == "running" || status == "pausing" {
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
        Ok(success_result(
            &serde_json::to_string_pretty(&serde_json::json!({
                "name": crew.name,
                "description": crew.description,
                "version": crew.version,
                "sync": sync,
            }))
            .unwrap_or_default(),
        ))
    }

    pub(crate) async fn crew_delete_impl(
        &self,
        params: CrewDeleteParams,
    ) -> Result<CallToolResult, McpError> {
        match self.db.crew_delete(&params.name) {
            Ok(()) => Ok(success_result(&format!("Crew '{}' deleted.", params.name))),
            Err(e) => Ok(error_result(&e.to_string())),
        }
    }
}
