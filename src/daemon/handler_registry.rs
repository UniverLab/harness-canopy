//! MCP handler tools split out of `daemon::handler` (the `harness refresh_registry`
//! action). Lets an operator apply a merged registry fix immediately, without
//! the TUI's 24-hour background refresh — the same `refresh_registry_inner`
//! path the TUI startup uses, safe while graphs run because it only rewrites
//! `config.toml` and the baseline sidecar.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::{build_json_result, internal_error};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

impl TaskTriggerHandler {
    pub(crate) async fn harness_refresh_registry_impl(&self) -> Result<CallToolResult, McpError> {
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
