//! TURN credentials for one peer, fetched from the relay.
//!
//! [`fetch_ice_servers`] asks the relay for a short-lived TURN credential;
//! the transport in [`super::peer`] turns the answer into connection config.

use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use serde::Deserialize;

use super::peer::IceServer;

/// How long to wait for the relay's TURN credential response.
const RELAY_TIMEOUT: Duration = Duration::from_secs(10);

/// Compiled-in backup STUN server used when the relay's `/turn` fetch fails.
pub const FALLBACK_STUN_URL: &str = "stun:stun.l.google.com:19302";

/// How many times [`fetch_ice_servers_or_fallback`] tries the relay.
const FETCH_ATTEMPTS: u32 = 3;

/// Base backoff step between fetch attempts; attempt `n` waits `n` steps.
const RETRY_BACKOFF_STEP: Duration = Duration::from_millis(100);

/// Backup ICE servers: the single [`FALLBACK_STUN_URL`] server, no credentials.
pub fn fallback_ice_servers() -> Vec<IceServer> {
    vec![IceServer {
        urls: vec![FALLBACK_STUN_URL.to_string()],
        username: None,
        credential: None,
    }]
}

/// Tries [`fetch_ice_servers`] up to [`FETCH_ATTEMPTS`] times with a fixed
/// backoff, then falls back to [`fallback_ice_servers`] instead of leaving
/// the host with no ICE servers. A later call that succeeds returns the
/// provider servers; nothing sticks.
pub async fn fetch_ice_servers_or_fallback(relay_url: &str, token: &str) -> Vec<IceServer> {
    let mut last_error = String::new();
    for attempt in 1..=FETCH_ATTEMPTS {
        match fetch_ice_servers(relay_url, token).await {
            Ok(servers) => return servers,
            Err(error) => {
                last_error = format!("{error:#}");
                if attempt < FETCH_ATTEMPTS {
                    tokio::time::sleep(RETRY_BACKOFF_STEP * attempt).await;
                }
            }
        }
    }
    tracing::warn!(
        "remote: no ICE servers from the relay after {FETCH_ATTEMPTS} attempts ({last_error}); falling back to {FALLBACK_STUN_URL}"
    );
    fallback_ice_servers()
}

/// Asks the relay for ICE servers: `GET <relay_url>/turn` with the host token.
///
/// A non-200 response is an error rather than an empty list, because an empty
/// list would silently downgrade the session to host candidates and only fail
/// later, at connection time.
///
/// # Errors
///
/// Returns an error when the request fails, the status is not 200, or the body
/// is not the expected JSON shape.
pub async fn fetch_ice_servers(relay_url: &str, token: &str) -> Result<Vec<IceServer>> {
    let url = format!("{}/turn", relay_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(RELAY_TIMEOUT)
        .build()
        .context("build the relay HTTP client")?;
    let response = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(anyhow!("GET {url} returned HTTP {}", response.status()));
    }
    let body: TurnResponse = response
        .json()
        .await
        .with_context(|| format!("decode the ICE servers in the GET {url} response"))?;
    Ok(body
        .ice_servers
        .into_iter()
        .filter_map(|server| {
            let urls = server.urls.map(Urls::into_vec).unwrap_or_default();
            if urls.is_empty() {
                return None;
            }
            Some(IceServer {
                urls,
                username: server.username,
                credential: server.credential,
            })
        })
        .collect())
}

/// `{ "iceServers": [ { "urls": [...], "username": "...", "credential": "..." } ] }`
#[derive(Debug, Deserialize)]
struct TurnResponse {
    #[serde(rename = "iceServers")]
    ice_servers: Vec<IceServerBody>,
}

/// One entry of [`TurnResponse`]; `urls` is either a list or a single string.
#[derive(Debug, Deserialize)]
struct IceServerBody {
    #[serde(default)]
    urls: Option<Urls>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    credential: Option<String>,
}

/// Both spellings the WebRTC specification allows for `urls`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Urls {
    /// `"urls": ["turn:...", "stun:..."]`
    Many(Vec<String>),
    /// `"urls": "turn:..."`
    One(String),
}

impl Urls {
    /// Flattens either spelling into a list.
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::Many(urls) => urls,
            Self::One(url) => vec![url],
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    use super::*;

    const PROVIDER_URL: &str = "turn:provider.example:3478";

    #[derive(Clone)]
    struct FlakyTurn {
        hits: Arc<AtomicU32>,
        fail_times: Arc<AtomicU32>,
    }

    async fn flaky_handler(State(state): State<FlakyTurn>) -> impl IntoResponse {
        let n = state.hits.fetch_add(1, Ordering::SeqCst) + 1;
        if n <= state.fail_times.load(Ordering::SeqCst) {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "boom"})),
            )
                .into_response()
        } else {
            (
                StatusCode::OK,
                Json(json!({"iceServers": [{"urls": [PROVIDER_URL], "username": "u", "credential": "p"}]})),
            )
                .into_response()
        }
    }

    async fn start_flaky(fail_times: u32) -> (String, Arc<AtomicU32>, Arc<AtomicU32>) {
        let hits = Arc::new(AtomicU32::new(0));
        let fail_times = Arc::new(AtomicU32::new(fail_times));
        let state = FlakyTurn {
            hits: Arc::clone(&hits),
            fail_times: Arc::clone(&fail_times),
        };
        let app = Router::new()
            .route("/turn", get(flaky_handler))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the fake relay");
        let addr = listener.local_addr().expect("fake relay addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), hits, fail_times)
    }

    fn has_fallback(servers: &[IceServer]) -> bool {
        servers
            .iter()
            .flat_map(|server| &server.urls)
            .any(|url| url == FALLBACK_STUN_URL)
    }

    #[tokio::test]
    async fn fail_twice_then_ok_recovers_without_fallback() {
        let (relay_url, hits, _) = start_flaky(2).await;
        let servers = fetch_ice_servers_or_fallback(&relay_url, "test-token").await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            3,
            "two failures then a success"
        );
        assert_eq!(servers.len(), 1, "the provider hands out one server");
        assert_eq!(servers[0].urls, vec![PROVIDER_URL.to_string()]);
        assert_eq!(servers[0].username.as_deref(), Some("u"));
        assert_eq!(servers[0].credential.as_deref(), Some("p"));
        assert!(!has_fallback(&servers), "no fallback after a success");
    }

    #[tokio::test]
    async fn fail_always_leaves_fallback_stun() {
        let (relay_url, hits, _) = start_flaky(u32::MAX).await;
        let servers = fetch_ice_servers_or_fallback(&relay_url, "test-token").await;
        assert_eq!(hits.load(Ordering::SeqCst), 3, "the retry is bounded");
        assert_eq!(
            servers.len(),
            1,
            "the fallback is one STUN server, not empty"
        );
        assert_eq!(servers[0].urls, vec![FALLBACK_STUN_URL.to_string()]);
        assert!(servers[0].username.is_none(), "STUN has no username");
        assert!(servers[0].credential.is_none(), "STUN has no credential");
        let expected = fallback_ice_servers();
        assert_eq!(servers[0].urls, expected[0].urls);
    }

    #[tokio::test]
    async fn later_success_clears_fallback() {
        let (relay_url, hits, fail_times) = start_flaky(u32::MAX).await;
        let first = fetch_ice_servers_or_fallback(&relay_url, "test-token").await;
        assert!(has_fallback(&first), "a failing relay yields the fallback");
        fail_times.store(0, Ordering::SeqCst);
        let second = fetch_ice_servers_or_fallback(&relay_url, "test-token").await;
        assert!(
            !has_fallback(&second),
            "a later success restores provider servers"
        );
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].urls, vec![PROVIDER_URL.to_string()]);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            4,
            "three failed attempts, then one success"
        );
    }
}
