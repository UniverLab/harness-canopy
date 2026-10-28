//! The `remote` MCP tool — the hub's Remote tab, served from the daemon.
//!
//! Every action answers with the same [`crate::remote::panel`] JSON the CLI
//! prints: the status view (sign-in, the relay switch, the relay phase, the
//! live and paired devices), pairing-code creation, the on/off switch and
//! revokes. Relay phase, live peers and code creation live in this process,
//! so the TUI must ask here instead of reading files itself.

use crate::daemon::actions::{action_params, ActionObject, ActionSpec, ToolSpec};
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

static REMOTE: ToolSpec = ToolSpec {
    name: "remote",
    actions: &[
        ActionSpec {
            name: "status",
            required: &[],
            optional: &[],
        },
        ActionSpec {
            name: "pair",
            required: &[],
            optional: &[],
        },
        ActionSpec {
            name: "set_enabled",
            required: &["enabled"],
            optional: &[],
        },
        ActionSpec {
            name: "revoke",
            required: &["device_id"],
            optional: &[],
        },
    ],
};

#[tool_router(router = tool_router_remote, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "remote",
        description = "Remote control of this hub from the browser. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `status` (required: none; optional: none) — sign-in state, whether remote is enabled, the relay URL, the relay phase (connected, connecting, token rejected: sign in again, off), the live device ids and every paired device with its live flag. \
        `pair` (required: none; optional: none) — create a one-time pairing code with its link, expiry and QR, replacing any live one; the same code `canopy remote pair` prints. \
        `set_enabled` (required: enabled; optional: none) — turn the relay connection on or off (`[remote] enabled`); the daemon picks the change up without a restart. \
        `revoke` (required: device_id; optional: none) — revoke a paired device: delete the row and close its live connection at once."
    )]
    pub(crate) async fn remote(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = REMOTE.validate(&params)?;
        match action {
            "status" => self.remote_status_impl(),
            "pair" => remote_pair_impl(),
            "set_enabled" => {
                let params: RemoteSetEnabledParams = action_params(&params)?;
                remote_set_enabled_impl(params.enabled)
            }
            "revoke" => {
                let params: RemoteRevokeParams = action_params(&params)?;
                self.remote_revoke_impl(params.device_id.trim())
            }
            _ => unreachable!("validated action"),
        }
    }
}

impl TaskTriggerHandler {
    fn remote_status_impl(&self) -> Result<CallToolResult, McpError> {
        let dir = data_dir()?;
        let live = live::shared();
        match panel::status_view(&dir, &self.db, &live) {
            Ok(view) => to_json(&view),
            Err(error) => Ok(error_result(&format!("remote status: {error:#}"))),
        }
    }

    fn remote_revoke_impl(&self, device_id: &str) -> Result<CallToolResult, McpError> {
        match panel::revoke(&self.db, device_id) {
            Ok(true) => to_json(&json!({ "revoked": device_id })),
            Ok(false) => Ok(error_result(&format!(
                "no paired device with id {device_id}"
            ))),
            Err(error) => Ok(error_result(&format!("remote revoke: {error:#}"))),
        }
    }
}

fn remote_pair_impl() -> Result<CallToolResult, McpError> {
    let dir = data_dir()?;
    match panel::create_pairing(&dir) {
        Ok(pair) => to_json(&pair),
        Err(error) => Ok(error_result(&format!("remote pair: {error:#}"))),
    }
}

fn remote_set_enabled_impl(enabled: bool) -> Result<CallToolResult, McpError> {
    let dir = data_dir()?;
    match panel::set_enabled(&dir, enabled) {
        Ok(()) => to_json(&json!({ "enabled": enabled })),
        Err(error) => Ok(error_result(&format!("remote set_enabled: {error:#}"))),
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let value = serde_json::to_value(value).map_err(|error| {
        McpError::internal_error(format!("remote: could not serialize: {error}"), None)
    })?;
    Ok(build_json_result(&value))
}
