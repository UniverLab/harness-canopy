//! The `remote_*` MCP tools — the hub's Remote tab, served from the daemon.
//!
//! Every tool answers with the same [`crate::remote::panel`] JSON the CLI
//! prints: the status view (sign-in, the relay switch, the relay phase, the
//! live and paired devices), pairing-code creation, the on/off switch and
//! revokes. Relay phase, live peers and code creation live in this process,
//! so the TUI must ask here instead of reading files itself.

use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use serde_json::json;

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::build_json_result;
use crate::daemon::helpers::{data_dir, error_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::remote::{live, panel};

#[tool_router(router = tool_router_remote, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "remote_status",
        description = "Show the remote-control status for the hub's Remote tab: sign-in state, whether remote is enabled, the relay URL, the relay phase (connected, connecting, token rejected: sign in again, off), the live device ids and every paired device with its live flag."
    )]
    pub(crate) async fn remote_status(&self) -> Result<CallToolResult, McpError> {
        let dir = data_dir()?;
        let live = live::shared();
        match panel::status_view(&dir, &self.db, &live) {
            Ok(view) => to_json(&view),
            Err(error) => Ok(error_result(&format!("remote_status: {error:#}"))),
        }
    }

    #[tool(
        name = "remote_pair",
        description = "Create a one-time pairing code with its link, expiry and QR, replacing any live one — the same code `canopy remote pair` prints."
    )]
    pub(crate) async fn remote_pair(&self) -> Result<CallToolResult, McpError> {
        let dir = data_dir()?;
        match panel::create_pairing(&dir) {
            Ok(pair) => to_json(&pair),
            Err(error) => Ok(error_result(&format!("remote_pair: {error:#}"))),
        }
    }

    #[tool(
        name = "remote_set_enabled",
        description = "Turn the relay connection on or off (`[remote] enabled`); the daemon picks the change up without a restart."
    )]
    pub(crate) async fn remote_set_enabled(
        &self,
        Parameters(params): Parameters<RemoteSetEnabledParams>,
    ) -> Result<CallToolResult, McpError> {
        let dir = data_dir()?;
        match panel::set_enabled(&dir, params.enabled) {
            Ok(()) => to_json(&json!({ "enabled": params.enabled })),
            Err(error) => Ok(error_result(&format!("remote_set_enabled: {error:#}"))),
        }
    }

    #[tool(
        name = "remote_revoke",
        description = "Revoke a paired device by device id: delete the row and close its live connection at once."
    )]
    pub(crate) async fn remote_revoke(
        &self,
        Parameters(params): Parameters<RemoteRevokeParams>,
    ) -> Result<CallToolResult, McpError> {
        match panel::revoke(&self.db, params.device_id.trim()) {
            Ok(true) => to_json(&json!({ "revoked": params.device_id.trim() })),
            Ok(false) => Ok(error_result(&format!(
                "no paired device with id {}",
                params.device_id.trim()
            ))),
            Err(error) => Ok(error_result(&format!("remote_revoke: {error:#}"))),
        }
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let value = serde_json::to_value(value).map_err(|error| {
        McpError::internal_error(format!("remote: could not serialize: {error}"), None)
    })?;
    Ok(build_json_result(&value))
}
