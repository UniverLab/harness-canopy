//! The room's guest allowlist: trusted logins resolved to GitHub uids.
//!
//! The relay admits only hub-invited guests: on join and on every grant
//! change the host publishes `{"t":"guest_allow","uids":[...]}` to the room
//! (see [`crate::remote::trust::guest_allow_body`]) and the worker enforces
//! it. The trust store holds logins, so each login resolves through
//! `GET {api}/users/{login}` with the host's bearer token; resolved uids
//! stay in a memory-only cache for the life of the connection.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;

use crate::db::Database;

/// GitHub API base for login-to-uid resolution.
pub const GITHUB_API: &str = "https://api.github.com";

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
