//! Daemon bearer auth for `canopy bridge`'s outgoing daemon requests.
//!
//! The token is read once at startup from `daemon.token` (never logged) and
//! attached via the shared `daemon_auth_header` helper. A process-wide cell
//! is enough: the bridge is a single-purpose sidecar, and threading the token
//! through every forward would churn a dozen test call sites for no gain.

use reqwest::RequestBuilder;

/// The daemon token for this bridge process. First write wins; requests sent
/// without it get the daemon's 401.
static BRIDGE_TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Remember the daemon token for this process.
pub(crate) fn set_bridge_token(token: String) {
    let _ = BRIDGE_TOKEN.set(token);
}

/// The daemon token when startup managed to read it.
pub(crate) fn bridge_token() -> Option<String> {
    BRIDGE_TOKEN.get().cloned()
}

/// Attach the daemon bearer token to an outgoing request when one was read
/// at startup, via the shared helper.
pub(crate) fn with_daemon_auth(request: RequestBuilder, token: Option<String>) -> RequestBuilder {
    match token {
        Some(token) => {
            let (name, value) = crate::sessions::auth::daemon_auth_header(&token);
            request.header(name, value)
        }
        None => request,
    }
}

/// Best-effort read of the daemon token at startup. Without it the daemon
/// answers 401; the bridge still runs so the failure is visible, not silent.
pub(crate) fn init_bridge_token() {
    if let Ok(data_dir) = crate::ensure_data_dir() {
        if let Ok(token) = crate::sessions::client::read_token(&data_dir) {
            set_bridge_token(token);
        }
    }
}
