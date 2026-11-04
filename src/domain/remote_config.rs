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
    /// Largest file a remote upload may send, in MiB. Defaults to 20, the
    /// same cap `session_send_file` applies. Clamped to at least 1.
    #[serde(default = "default_shelf_max_file_mb")]
    pub shelf_max_file_mb: u32,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            relay_url: default_relay_url(),
            github_client_id: default_github_client_id(),
            host_label: String::new(),
            shelf_max_file_mb: default_shelf_max_file_mb(),
        }
    }
}

fn default_shelf_max_file_mb() -> u32 {
    20
}

fn default_relay_url() -> String {
    "https://remote.univerlab.org".to_string()
}

fn default_github_client_id() -> String {
    "Ov23liVCfE58kUb3ssDy".to_string()
}
