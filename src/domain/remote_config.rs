//! The `[remote]` table of `config.toml`.

use serde::{Deserialize, Serialize};

/// Settings for owner-only remote control. Read from the `[remote]` table
/// in `config.toml`. The relay admits only hosts signed in with the same
/// GitHub account; `github_client_id` is the public Client ID of
/// UniverLab's "Canopy Remote" OAuth App (device flow, identity only — the
/// client secret never appears in canopy).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_relay_url")]
    pub relay_url: String,
    #[serde(default = "default_github_client_id")]
    pub github_client_id: String,
    /// Label shown for this host. Empty means the hostname.
    #[serde(default)]
    pub host_label: String,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            relay_url: default_relay_url(),
            github_client_id: default_github_client_id(),
            host_label: String::new(),
        }
    }
}

fn default_relay_url() -> String {
    "https://remote.univerlab.org".to_string()
}

fn default_github_client_id() -> String {
    "Ov23liVCfE58kUb3ssDy".to_string()
}
