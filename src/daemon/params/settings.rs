use super::*;

/// CM51 `config_list` parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConfigListParams {
    /// Only list settings in this group (e.g. "TUI & appearance").
    pub group: Option<String>,
}

/// CM51 `config_get` parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConfigGetParams {
    /// The setting's dotted key (e.g. `clean.retention_days`).
    pub key: String,
}

/// CM51 `config_set` parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConfigSetParams {
    /// The setting's dotted key (e.g. `theme`).
    pub key: String,
    /// The new value, as TOML (`"modern"`, `12`, `["/a"]`) or as bare text
    /// for a string/enum setting (`modern`).
    pub value: String,
}

/// CM51 `config_reset` parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConfigResetParams {
    /// The setting's dotted key to reset to its default.
    pub key: String,
}
