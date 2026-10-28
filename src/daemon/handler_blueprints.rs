//! MCP handler tools split out of `daemon::handler` (blueprint tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::{build_id_result, internal_error};
use crate::daemon::handler_nodes::validate_node_config;
use crate::daemon::handler_validate::{validate_node_kind, validate_non_empty};
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::domain::blueprints::validate_blueprint_deletable;
use crate::domain::blueprints::Blueprint;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_blueprints, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "blueprint_list",
        description = "List every node blueprint (builtin and custom) — predesigned, reusable node templates for graph_node add."
    )]
    pub(crate) async fn blueprint_list(&self) -> Result<CallToolResult, McpError> {
        let blueprints = self.db.list_blueprints().map_err(internal_error)?;

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "blueprints": blueprints.iter().map(blueprint_json).collect::<Vec<_>>(),
            }))
            .unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "blueprint_create",
        description = "Create a custom node blueprint: a reusable {name, kind, config template} that graph_node add can reference by name."
    )]
    pub(crate) async fn blueprint_create(
        &self,
        Parameters(params): Parameters<BlueprintCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name.trim();
        if let Err(e) = validate_non_empty(name, "Blueprint name") {
            return Ok(error_result(&e));
        }
        if self
            .db
            .get_blueprint_by_name(name)
            .map_err(internal_error)?
            .is_some()
        {
            return Ok(error_result(&format!(
                "Blueprint '{name}' already exists; choose a different name."
            )));
        }

        let kind = match validate_node_kind(params.kind.trim()) {
            Ok(kind) => kind,
            Err(e) => return Ok(error_result(&e)),
        };
        let config = serde_json::Value::Object(params.config);
        if let Err(e) = validate_node_config(kind, &config) {
            return Ok(error_result(&e));
        }

        let blueprint = Blueprint {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            kind,
            config,
            builtin: false,
            created_at: chrono::Utc::now(),
        };
        self.db
            .insert_blueprint(&blueprint)
            .map_err(internal_error)?;

        Ok(build_id_result(&blueprint.id, "blueprint_id"))
    }

    #[tool(
        name = "blueprint_delete",
        description = "Delete a custom node blueprint. Refuses to delete a builtin."
    )]
    pub(crate) async fn blueprint_delete(
        &self,
        Parameters(params): Parameters<BlueprintDeleteParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name.trim();
        let Some(blueprint) = self
            .db
            .get_blueprint_by_name(name)
            .map_err(internal_error)?
        else {
            return Ok(error_result(&format!("Blueprint '{name}' not found.")));
        };
        if let Err(e) = validate_blueprint_deletable(&blueprint) {
            return Ok(error_result(&e));
        }

        self.db
            .delete_blueprint_by_name(name)
            .map_err(internal_error)?;

        Ok(success_result(&format!("Blueprint '{name}' deleted.")))
    }
}

pub(crate) fn blueprint_json(blueprint: &Blueprint) -> serde_json::Value {
    serde_json::json!({
        "id": blueprint.id,
        "name": blueprint.name,
        "kind": blueprint.kind.as_str(),
        "config": blueprint.config,
        "builtin": blueprint.builtin,
    })
}
