use super::*;

/// `remote` `set_enabled` action parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RemoteSetEnabledParams {
    /// Turn the relay connection on (`true`) or off (`false`).
    pub enabled: bool,
}

/// `remote` `revoke` action parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RemoteRevokeParams {
    /// Device id to revoke (first 16 hex chars of SHA-256 over its key).
    pub device_id: String,
}
