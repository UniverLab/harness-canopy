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
