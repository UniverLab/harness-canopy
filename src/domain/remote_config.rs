//! The `[remote]` table of `config.toml`.

use serde::{Deserialize, Serialize};

use super::settings;

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
    /// STUN fallback server used when the relay returns no ICE servers.
    /// Empty means no fallback (the default on a fresh install); a change
    /// applies to the next connection.
    #[serde(default = "default_stun_fallback")]
    pub stun_fallback: String,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            relay_url: default_relay_url(),
            github_client_id: default_github_client_id(),
            host_label: String::new(),
            shelf_max_file_mb: default_shelf_max_file_mb(),
            stun_fallback: default_stun_fallback(),
        }
    }
}

fn default_stun_fallback() -> String {
    settings::default_string("remote.stun_fallback")
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

const RELAY_URL_ERROR: &str =
    "remote.relay_url must use https; plain http is allowed only for localhost";

/// Validate a `remote.relay_url`: `https://` and `wss://` are always
/// accepted; `http://` and `ws://` are accepted only for loopback hosts
/// (`localhost`, `127.0.0.0/8`, `::1`).
pub fn validate_relay_url(url: &str) -> Result<(), String> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(RELAY_URL_ERROR.to_string());
    };
    let scheme = scheme.to_ascii_lowercase();
    let authority = rest.split('/').next().unwrap_or("");
    let Some(raw_host) = extract_host(authority) else {
        return Err(RELAY_URL_ERROR.to_string());
    };
    let host = raw_host.to_ascii_lowercase();
    if host.is_empty() {
        return Err(RELAY_URL_ERROR.to_string());
    }
    match scheme.as_str() {
        "https" | "wss" => Ok(()),
        "http" | "ws" if is_loopback_host(&host) => Ok(()),
        _ => Err(RELAY_URL_ERROR.to_string()),
    }
}

fn extract_host(authority: &str) -> Option<&str> {
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (ipv6, after) = bracketed.split_once(']')?;
        if !after.is_empty() && !after.starts_with(':') {
            return None;
        }
        return Some(ipv6);
    }
    Some(authority.split_once(':').map_or(authority, |(h, _)| h))
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}
