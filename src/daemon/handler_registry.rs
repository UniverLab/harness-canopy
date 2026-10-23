//! MCP handler tools split out of `daemon::handler` (the `registry_refresh`
//! tool). Lets an operator apply a merged registry fix immediately, without
//! the TUI's 24-hour background refresh — the same `refresh_registry_inner`
//! path the TUI startup uses, safe while graphs run because it only rewrites
//! `config.toml` and the baseline sidecar.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::{build_json_result, internal_error};
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_registry, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "registry_refresh",
        description = "Fetch the platform registry and apply it to config.toml now, ignoring the 24-hour interval."
    )]
    pub(crate) async fn registry_refresh(&self) -> Result<CallToolResult, McpError> {
        let report = tokio::task::spawn_blocking(move || {
            let home = dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
            crate::setup_module::registry_fetch::refresh_registry_inner(
                &home,
                crate::setup_module::registry_fetch::fetch_registry,
            )
        })
        .await
        .map_err(|e| internal_error(e.to_string()))?
        .map_err(|e| internal_error(e.to_string()))?;

        let value = serde_json::to_value(&report).map_err(|e| internal_error(e.to_string()))?;
        Ok(build_json_result(&value))
    }
}
