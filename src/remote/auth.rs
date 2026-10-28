//! GitHub device-flow sign-in for owner-only remote control.
//!
//! The relay admits only hosts signed in with the same GitHub account, so
//! canopy holds a GitHub identity token obtained with the OAuth device flow
//! (no browser on the host, no scopes, identity only). The same functions
//! serve `canopy remote login|logout|status` and the TUI — never shell out.
//!
//! The token is stored in `~/.canopy/remote/github.json` (mode 0600) and
//! never appears in logs, errors, or status output.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::canopy_config::CanopyConfig;

/// Default GitHub endpoints for the device flow; every URL is injectable
/// through [`RemoteEndpoints`] so tests run against a local mock server.
pub const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
/// Default token endpoint for the device flow.
pub const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
/// Default endpoint for resolving the signed-in user.
pub const GITHUB_USER_URL: &str = "https://api.github.com/user";

/// The three HTTP endpoints [`login`] talks to. Defaults to GitHub; tests
/// point these at a local mock server.
#[derive(Debug, Clone)]
pub struct RemoteEndpoints {
    /// Device-code endpoint (`POST`, returns the user code).
    pub device_code_url: String,
    /// Token endpoint (`POST`, polled until the user approves).
    pub access_token_url: String,
    /// User endpoint (`GET`, resolves login and id for the token).
    pub user_url: String,
}

impl Default for RemoteEndpoints {
    fn default() -> Self {
        Self {
            device_code_url: DEVICE_CODE_URL.to_string(),
            access_token_url: ACCESS_TOKEN_URL.to_string(),
            user_url: GITHUB_USER_URL.to_string(),
        }
    }
}

/// What the user must type at `https://github.com/login/device`.
#[derive(Debug, Clone)]
pub struct DeviceCodeInfo {
    /// The code shown to the user (e.g. `ABCD-1234`).
    pub user_code: String,
    /// One-click verification URL, when GitHub returns one.
    pub verification_uri_complete: Option<String>,
}

/// A signed-in GitHub identity. `Debug` redacts the token: it must never
/// appear in logs, errors, or status output.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubIdentity {
    /// OAuth access token (identity only, no scopes).
    pub token: String,
    /// GitHub username.
    pub login: String,
    /// GitHub user id.
    pub id: u64,
}

impl std::fmt::Debug for GitHubIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubIdentity")
            .field("token", &"<redacted>")
            .field("login", &self.login)
            .field("id", &self.id)
            .finish()
    }
}

/// The `canopy remote status` payload (human and `--json` share it).
#[derive(Debug, Clone, Serialize)]
pub struct RemoteStatus {
    /// Whether an identity is stored.
    pub signed_in: bool,
    /// GitHub login when signed in, `None` otherwise.
    pub login: Option<String>,
    /// Whether `[remote] enabled` is on.
    pub enabled: bool,
    /// The configured relay URL.
    pub relay_url: String,
}

/// Build the status payload from config plus the stored identity (if any).
/// The token is never part of the status.
pub fn build_remote_status(
    config: &CanopyConfig,
    identity: Option<&GitHubIdentity>,
) -> RemoteStatus {
    RemoteStatus {
        signed_in: identity.is_some(),
        login: identity.map(|identity| identity.login.clone()),
        enabled: config.remote.enabled,
        relay_url: config.remote.relay_url.clone(),
    }
}

/// Path of the stored identity: `<data_dir>/remote/github.json`.
pub fn identity_path(data_dir: &Path) -> PathBuf {
    data_dir.join("remote").join("github.json")
}

/// Load the stored identity, or `None` when missing or unreadable.
pub fn load_identity(data_dir: &Path) -> Option<GitHubIdentity> {
    let text = std::fs::read_to_string(identity_path(data_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Store the identity, creating `<data_dir>/remote` with mode 0700 and the
/// file with mode 0600.
pub fn save_identity(data_dir: &Path, identity: &GitHubIdentity) -> Result<()> {
    let dir = data_dir.join("remote");
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let text = serde_json::to_string_pretty(identity)?;
    let path = dir.join("github.json");
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?
        .write_all(text.as_bytes())?;
    // `mode` above only applies at creation; enforce 0600 on the file even
    // when it already existed with looser permissions.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Delete the stored identity. Idempotent: missing file is success.
pub fn logout(data_dir: &Path) -> Result<()> {
    match std::fs::remove_file(identity_path(data_dir)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Run the OAuth device flow: request a code, report it via `on_code`, poll
/// until the user approves, resolve the user, store the identity, return it.
pub async fn login(
    client_id: &str,
    data_dir: &Path,
    endpoints: &RemoteEndpoints,
    on_code: impl FnOnce(&DeviceCodeInfo),
) -> Result<GitHubIdentity> {
    if client_id.trim().is_empty() {
        bail!("remote.github_client_id is not set in ~/.canopy/config.toml");
    }
    let client = reqwest::Client::builder()
        .user_agent(concat!("canopy/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let (device_code, interval_secs, info) =
        request_device_code(&client, client_id, endpoints).await?;
    on_code(&info);
    let token = poll_for_token(&client, client_id, &device_code, interval_secs, endpoints).await?;
    let (login, id) = fetch_user(&client, &token, endpoints).await?;
    let identity = GitHubIdentity { token, login, id };
    save_identity(data_dir, &identity)?;
    Ok(identity)
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri_complete: Option<String>,
    #[serde(default = "default_poll_interval_secs")]
    interval: u64,
}

#[derive(Deserialize)]
struct GitHubUser {
    login: String,
    id: u64,
}

fn default_poll_interval_secs() -> u64 {
    5
}

async fn request_device_code(
    client: &reqwest::Client,
    client_id: &str,
    endpoints: &RemoteEndpoints,
) -> Result<(String, u64, DeviceCodeInfo)> {
    let response: DeviceCodeResponse = client
        .post(&endpoints.device_code_url)
        .header("Accept", "application/json")
        .form(&[("client_id", client_id), ("scope", "")])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("unexpected response from the GitHub device endpoint")?;
    let info = DeviceCodeInfo {
        user_code: response.user_code,
        verification_uri_complete: response.verification_uri_complete,
    };
    Ok((response.device_code, response.interval, info))
}

/// What one token-poll response means: wait longer, proceed, or stop.
pub(crate) enum PollDecision {
    /// Poll again after this many seconds.
    Retry(u64),
    /// The response carries an access token.
    Success,
    /// Stop with this one-line message.
    Fail(String),
}

/// Classify one token-endpoint response. Pure so tests pin the mapping
/// without sleeping: `authorization_pending` retries at the same interval,
/// `slow_down` adds 5 seconds, `expired_token`/`access_denied` stop.
pub(crate) fn poll_decision(body: &Value, interval_secs: u64) -> PollDecision {
    if body
        .get("access_token")
        .and_then(Value::as_str)
        .is_some_and(|token| !token.is_empty())
    {
        return PollDecision::Success;
    }
    match body.get("error").and_then(Value::as_str) {
        Some("authorization_pending") => PollDecision::Retry(interval_secs),
        Some("slow_down") => PollDecision::Retry(interval_secs + 5),
        Some("expired_token") => {
            PollDecision::Fail("device code expired; run `canopy remote login` again".to_string())
        }
        Some("access_denied") => PollDecision::Fail("authorization denied".to_string()),
        _ => PollDecision::Fail("unexpected response from the GitHub token endpoint".to_string()),
    }
}

async fn poll_for_token(
    client: &reqwest::Client,
    client_id: &str,
    device_code: &str,
    interval_secs: u64,
    endpoints: &RemoteEndpoints,
) -> Result<String> {
    let mut wait_secs = interval_secs;
    loop {
        tokio::time::sleep(Duration::from_secs(wait_secs)).await;
        let body: Value = client
            .post(&endpoints.access_token_url)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", client_id),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        match poll_decision(&body, wait_secs) {
            PollDecision::Retry(next) => wait_secs = next,
            PollDecision::Success => {
                return body
                    .get("access_token")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .context("GitHub granted access but returned no token");
            }
            PollDecision::Fail(message) => bail!("{message}"),
        }
    }
}

async fn fetch_user(
    client: &reqwest::Client,
    token: &str,
    endpoints: &RemoteEndpoints,
) -> Result<(String, u64)> {
    let user: GitHubUser = client
        .get(&endpoints.user_url)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("unexpected response from the GitHub user endpoint")?;
    Ok((user.login, user.id))
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
