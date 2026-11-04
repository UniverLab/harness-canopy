//! The room's guest allowlist: trusted logins resolved to GitHub uids.
//!
//! The relay admits only hub-invited guests: on join and on every grant
//! change the host publishes `{"t":"guest_allow","uids":[...],"invites":[...]}`
//! to the room (see [`crate::remote::trust::guest_allow_body`]) and the
//! worker enforces it. The trust store holds logins, so each login resolves through
//! `GET {api}/users/{login}` with the host's bearer token; resolved uids
//! stay in a memory-only cache for the life of the connection.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;

use crate::db::Database;

/// GitHub API base for login-to-uid resolution.
pub const GITHUB_API: &str = "https://api.github.com";

/// Test hook: an environment variable naming a local stand-in for
/// [`GITHUB_API`], so a sandbox with a mock GitHub can resolve logins.
/// Honoured only for a loopback `http://` origin (see [`loopback_api`]);
/// any other value is ignored with a warning, so it can never send the
/// host's GitHub token anywhere but this machine.
pub const GITHUB_API_ENV: &str = "CANOPY_GITHUB_API";

/// The GitHub API base for login resolution: [`GITHUB_API`], or the
/// loopback stand-in [`GITHUB_API_ENV`] names (a test hook).
pub fn github_api() -> String {
    github_api_from(std::env::var(GITHUB_API_ENV).ok().as_deref())
}

/// [`github_api`] over an explicit variable value, for the tests.
fn github_api_from(value: Option<&str>) -> String {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return GITHUB_API.to_string();
    };
    match loopback_api(value) {
        Some(base) => {
            tracing::warn!("remote: resolving GitHub logins via the {GITHUB_API_ENV} test hook");
            base
        }
        None => {
            tracing::warn!(
                "remote: ignoring {GITHUB_API_ENV}: only a loopback http:// origin is honoured"
            );
            GITHUB_API.to_string()
        }
    }
}

/// `http://127.0.0.1[:port]` or `http://[::1][:port]` (an optional
/// trailing `/`), normalised without the slash; `None` for anything else:
/// another scheme or host, user info, a path, a query or a fragment.
fn loopback_api(value: &str) -> Option<String> {
    let authority = value.strip_prefix("http://")?;
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    let (host, port) = ["[::1]", "127.0.0.1"]
        .into_iter()
        .find_map(|host| authority.strip_prefix(host).map(|rest| (host, rest)))?;
    if !port.is_empty() {
        let digits = port.strip_prefix(':')?;
        let valid = !digits.is_empty()
            && digits.bytes().all(|byte| byte.is_ascii_digit())
            && digits.parse::<u16>().is_ok_and(|port| port != 0);
        if !valid {
            return None;
        }
    }
    Some(format!("http://{host}{port}"))
}

/// How long one uid lookup may take.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);

/// The `id` of one `GET /users/{login}` answer; the rest is ignored.
#[derive(Debug, Deserialize)]
struct GithubUser {
    /// The numeric uid, `0` when the answer carries none.
    #[serde(default)]
    id: u64,
}

/// Resolve every trusted login to its GitHub uid, in store order. Cached
/// logins never hit the network; a login that fails to resolve is skipped
/// with a warning instead of aborting the whole list — aborting would keep a
/// removed guest admitted, while skipping fails closed.
pub async fn resolve_guest_uids(
    db: &Database,
    cache: &Mutex<HashMap<String, u64>>,
    bearer: &str,
    api_base: &str,
) -> Vec<u64> {
    let mut uids = Vec::new();
    let mut missing = Vec::new();
    for login in trusted_logins(db) {
        match cached_uid(cache, &login) {
            Some(uid) => uids.push(uid),
            None => missing.push(login),
        }
    }
    if missing.is_empty() {
        return uids;
    }
    let Ok(client) = http_client() else {
        tracing::warn!("remote: guest allowlist: cannot build the GitHub HTTP client");
        return uids;
    };
    for login in missing {
        if let Some(uid) = fetch_uid(&client, api_base, bearer, &login).await {
            if let Ok(mut map) = cache.lock() {
                map.insert(login, uid);
            }
            uids.push(uid);
        }
    }
    uids
}

/// Resolve one login to its GitHub uid, for `canopy remote invite`: the
/// one-shot form of [`resolve_guest_uids`] with no cache. `None` (with a
/// warning) on any failure, like the allowlist path.
pub async fn resolve_one_login(login: &str, bearer: &str, api_base: &str) -> Option<u64> {
    let login = crate::remote::trust::normalize_login(login);
    if login.is_empty() {
        return None;
    }
    let client = http_client().ok()?;
    fetch_uid(&client, api_base, bearer, &login).await
}

/// The trusted logins in store order; an unreadable store resolves to
/// deny-all rather than blocking the publish.
fn trusted_logins(db: &Database) -> Vec<String> {
    match db.list_trust() {
        Ok(profiles) => profiles.into_iter().map(|profile| profile.login).collect(),
        Err(error) => {
            tracing::warn!("remote: guest allowlist: cannot list the trust store: {error:#}");
            Vec::new()
        }
    }
}

/// One cached uid; a poisoned cache reads as a miss and refetches.
fn cached_uid(cache: &Mutex<HashMap<String, u64>>, login: &str) -> Option<u64> {
    cache.lock().ok().and_then(|map| map.get(login).copied())
}

/// One GitHub client for the whole allowlist, identified as canopy: GitHub
/// answers requests without a `User-Agent` with a 403.
fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(RESOLVE_TIMEOUT)
        .user_agent(concat!("canopy/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Resolve one login through `GET {api_base}/users/{login}`; `None` (with a
/// warning) on any failure: transport, status, shape, or a missing id.
async fn fetch_uid(
    client: &reqwest::Client,
    api_base: &str,
    bearer: &str,
    login: &str,
) -> Option<u64> {
    let url = format!("{}/users/{login}", api_base.trim_end_matches('/'));
    let response = get_user(client, &url, bearer).await?;
    user_id(response, login).await
}

/// `GET` one user's profile; `None` (with a warning) unless HTTP 200.
async fn get_user(client: &reqwest::Client, url: &str, bearer: &str) -> Option<reqwest::Response> {
    let response = match client.get(url).bearer_auth(bearer).send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!("remote: guest allowlist: GET {url} failed: {error:#}");
            return None;
        }
    };
    if response.status() != reqwest::StatusCode::OK {
        tracing::warn!(
            "remote: guest allowlist: GET {url} returned HTTP {}",
            response.status()
        );
        return None;
    }
    Some(response)
}

/// The uid of one profile answer; `None` (with a warning) when the shape is
/// wrong or the id is missing.
async fn user_id(response: reqwest::Response, login: &str) -> Option<u64> {
    let user: GithubUser = match response.json().await {
        Ok(user) => user,
        Err(error) => {
            tracing::warn!("remote: guest allowlist: bad uid answer for {login}: {error:#}");
            return None;
        }
    };
    if user.id == 0 {
        tracing::warn!("remote: guest allowlist: no uid for {login}");
        return None;
    }
    Some(user.id)
}

#[cfg(test)]
mod tests {
    use super::{github_api_from, loopback_api, GITHUB_API};

    #[test]
    fn github_api_override_is_loopback_only() {
        // Unset or blank: the real API.
        assert_eq!(github_api_from(None), GITHUB_API);
        assert_eq!(github_api_from(Some("")), GITHUB_API);
        // A loopback http origin stands in, port and trailing slash allowed.
        for (value, base) in [
            ("http://127.0.0.1:9999", "http://127.0.0.1:9999"),
            ("http://127.0.0.1:9999/", "http://127.0.0.1:9999"),
            ("http://127.0.0.1", "http://127.0.0.1"),
            ("http://[::1]:9999", "http://[::1]:9999"),
        ] {
            assert_eq!(loopback_api(value).as_deref(), Some(base), "{value}");
            assert_eq!(github_api_from(Some(value)), base, "{value}");
        }
        // Anything else falls back to the real API: the host's token never
        // leaves this machine through the hook.
        for value in [
            "https://127.0.0.1:9999",
            "http://localhost:9999",
            "http://127.0.0.1.evil.example",
            "http://127.0.0.1@evil.example",
            "http://127.0.0.1:9999@evil.example",
            "http://evil.example/127.0.0.1",
            "http://127.0.0.1:9999/api",
            "http://127.0.0.1:9999?x=1",
            "http://127.0.0.1:0",
            "http://127.0.0.1:99999",
            "http://127.0.0.1:",
            "http://127.0.0.2:9999",
            "https://api.github.com.evil.example",
        ] {
            assert_eq!(loopback_api(value), None, "{value}");
            assert_eq!(github_api_from(Some(value)), GITHUB_API, "{value}");
        }
    }
}
