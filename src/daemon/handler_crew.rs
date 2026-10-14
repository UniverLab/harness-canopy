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
use crate::daemon::params_extract::Parameters;
use crate::domain::crews::CrewMember;
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
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

#[tool_router(router = tool_router_crew, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "crew_list",
        description = "List every crew: its name, description, version, ordered members, and how many ensembles/nodes/hooks bind it. Member order is meaningful — cascade tries it in order, round_robin rotates through it, a bound agent node takes the first available member."
    )]
    pub(crate) async fn crew_list(
        &self,
        Parameters(_params): Parameters<CrewGetParams>,
    ) -> Result<CallToolResult, McpError> {
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

    #[tool(
        name = "crew_get",
        description = "Show one crew — its description, version, ordered members — and every binding of it as {graph, portion, name}."
    )]
    pub(crate) async fn crew_get(
        &self,
        Parameters(params): Parameters<CrewGetParams>,
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

    #[tool(
        name = "crew_create",
        description = "Create a crew from an ordered member list (`members`) or by seeding from an existing ensemble's current members (`from_ensemble`) — pass exactly one. `from_ensemble` refuses when any member has a per-member prompt_override. Member order is meaningful: cascade tries it in order, round_robin rotates through it, a bound agent node takes the first available member."
    )]
    pub(crate) async fn crew_create(
        &self,
        Parameters(params): Parameters<CrewCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        match (params.members.as_ref(), params.from_ensemble.as_ref()) {
            (Some(_), Some(_)) => {
                return Ok(error_result(
                    "pass crew members or from_ensemble, never both.",
                ));
            }
            (None, None) => {
                return Ok(error_result("pass either crew members or from_ensemble."));
            }
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

    #[tool(
        name = "crew_update",
        description = "Update a crew: replace the ordered member list (`members`), set `description`, and/or `rename`. A member-list write bumps the version; every bound ensemble picks it up at its next dispatch (graphs not currently running are synced eagerly)."
    )]
    pub(crate) async fn crew_update(
        &self,
        Parameters(params): Parameters<CrewUpdateParams>,
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

    #[tool(
        name = "crew_delete",
        description = "Delete a crew. Refused while any ensemble, agent node or agent hook still binds it; the error lists every binding."
    )]
    pub(crate) async fn crew_delete(
        &self,
        Parameters(params): Parameters<CrewDeleteParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.db.crew_delete(&params.name) {
            Ok(()) => Ok(success_result(&format!("Crew '{}' deleted.", params.name))),
            Err(e) => Ok(error_result(&e.to_string())),
        }
    }
}
