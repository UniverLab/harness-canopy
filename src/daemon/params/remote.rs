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

/// `remote` `disconnect` action parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RemoteDisconnectParams {
    /// Device id to disconnect (first 16 hex chars of SHA-256 over its key).
    pub device_id: String,
}

/// `remote` `trust_add` action parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RemoteTrustAddParams {
    /// GitHub login to trust (case-insensitive).
    pub login: String,
    /// Grant every session (`true`) or only `sessions` (`false`).
    pub all_sessions: bool,
    /// Granted session names when `all_sessions` is `false`.
    #[serde(default)]
    pub sessions: Option<Vec<String>>,
}

/// `remote` `trust_remove` action parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RemoteTrustRemoveParams {
    /// GitHub login to forget (case-insensitive).
    pub login: String,
}
