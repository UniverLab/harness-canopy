//! Bearer-token gate for the daemon's `/mcp` endpoint (audit finding F-01).
//!
//! Every request under `/mcp` must present `Authorization: Bearer <token>`
//! where the token is the content of `~/.canopy/daemon.token`. A request
//! with no header, a wrong token or a malformed header gets `401` with
//! `WWW-Authenticate: Bearer realm="canopy"` and a JSON error body —
//! without the request body ever being read or parsed. The token file is
//! re-read whenever its mtime changes, so a rotated token takes effect
//! without a restart. The token value never appears in logs or errors.
//!
//! [`authed_mcp_route`] wraps ONLY the `/mcp` routes (see
//! `build_mcp_router`): it must never be applied router-wide, which would
//! double-gate `/sessions/ws` (that endpoint enforces the same token
//! itself, with its own 401 shape).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::sessions::auth::{bearer_token, token_path, token_valid};

/// Exact body of the 401 the gate returns. JSON-RPC style; the token value
/// never appears in it.
pub const MISSING_TOKEN_BODY: &str = r#"{"error":"missing or invalid daemon token"}"#;
/// Challenge header of the 401 the gate returns.
pub const WWW_AUTHENTICATE_VALUE: &str = r#"Bearer realm="canopy""#;

/// Shared gate state: the data dir holding `daemon.token` plus the cached
/// token and the mtime it was read at. Re-read on mtime change only, so the
/// steady state does one `stat` per request and no file IO.
#[derive(Clone)]
struct McpAuthState {
    data_dir: PathBuf,
    cached: Arc<Mutex<CachedToken>>,
}

#[derive(Default)]
struct CachedToken {
    token: Option<String>,
    mtime: Option<SystemTime>,
}

impl McpAuthState {
    fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            cached: Arc::new(Mutex::new(CachedToken::default())),
        }
    }

    /// The current token, re-reading the file when its mtime moved (or when
    /// nothing is cached yet). `None` when the file cannot be read — the
    /// caller fails closed. Never logs the token.
    fn current_token(&self) -> Option<String> {
        let path = token_path(&self.data_dir);
        let mtime = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok();
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let (Some(token), Some(stored)) = (&cached.token, cached.mtime) {
            if Some(stored) == mtime {
                return Some(token.clone());
            }
        }
        let token = std::fs::read_to_string(&path)
            .ok()
            .map(|content| content.trim().to_string())
            .filter(|token| !token.is_empty())?;
        cached.token = Some(token.clone());
        cached.mtime = mtime;
        Some(token)
    }
}

/// Wrap the `/mcp` method router with the bearer-token gate. Takes and
/// returns a `MethodRouter` on purpose: `tower` is not a direct dependency
/// of this crate (project limits forbid adding one for this spec), so no
/// signature here can name the middleware layer type — applying it inside
/// this module keeps the concrete type local. Mount the result on the
/// `/mcp` routes only; never as a router-wide layer (that would
/// double-gate `/sessions/ws`).
pub fn authed_mcp_route<S>(
    route: axum::routing::MethodRouter<S>,
    data_dir: &std::path::Path,
) -> axum::routing::MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    route.layer(axum::middleware::from_fn_with_state(
        McpAuthState::new(data_dir.to_path_buf()),
        require_daemon_token,
    ))
}

/// Reject unauthenticated `/mcp` requests before the inner service (and its
/// body parsing) ever runs. Authentication precedes any Host or content-type
/// check the inner service performs, by construction.
async fn require_daemon_token(
    State(state): State<McpAuthState>,
    request: Request,
    next: Next,
) -> Response {
    let authorized = state.current_token().is_some_and(|expected| {
        request
            .headers()
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(bearer_token)
            .is_some_and(|provided| token_valid(provided, &expected))
    });
    if !authorized {
        return unauthorized();
    }
    next.run(request).await
}

/// The 401 for a missing, wrong or malformed daemon token: status, challenge
/// header and JSON body. The request body is untouched — this runs before
/// any parsing.
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            (WWW_AUTHENTICATE, WWW_AUTHENTICATE_VALUE),
            (CONTENT_TYPE, "application/json"),
        ],
        MISSING_TOKEN_BODY,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A router in the exact production shape (`build_mcp_router`): the
    /// `/mcp` routes behind [`authed_mcp_route`], `/sessions/ws` outside
    /// it. The stub answers 200 to everything (like the rmcp service, it
    /// ignores the path), so any 401 observed is the gate's own.
    async fn spawn_gated_server(initial_token: &str) -> (String, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp data dir");
        std::fs::write(token_path(dir.path()), initial_token).expect("write daemon token");
        let stub = axum::routing::MethodRouter::new().fallback(|| async { "inner-ok" });
        let authed = authed_mcp_route(axum::routing::any_service(stub), dir.path());
        let app = axum::Router::new()
            .route("/mcp", authed.clone())
            .route("/mcp/", authed.clone())
            .route("/mcp/{*rest}", authed)
            .route("/sessions/ws", axum::routing::get(|| async { "ws-ok" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind gated test server");
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (base, dir)
    }

    async fn post_mcp(base: &str, authorization: Option<&str>) -> reqwest::Response {
        let mut request = reqwest::Client::new()
            .post(format!("{base}/mcp"))
            .header("content-type", "application/json");
        if let Some(header) = authorization {
            request = request.header("authorization", header);
        }
        request
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .send()
            .await
            .expect("POST /mcp against the gated test server")
    }

    async fn assert_gate_rejection(response: reqwest::Response) {
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        let challenge = response
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        assert_eq!(challenge.as_deref(), Some(WWW_AUTHENTICATE_VALUE));
        assert_eq!(
            response.text().await.expect("read 401 body"),
            MISSING_TOKEN_BODY
        );
    }

    #[tokio::test]
    async fn mcp_rejects_request_without_token() {
        let (base, _dir) = spawn_gated_server("correct-token").await;
        assert_gate_rejection(post_mcp(&base, None).await).await;
    }

    #[tokio::test]
    async fn mcp_rejects_wrong_and_malformed_tokens() {
        let (base, _dir) = spawn_gated_server("correct-token").await;
        for authorization in [
            "Bearer wrong-token",
            "Bearer ",
            "Bearer",
            "Token correct-token",
            "bearer correct-token",
            "Basic YWxhZGRpbjpvcGVuc2VzYW1l",
        ] {
            assert_gate_rejection(post_mcp(&base, Some(authorization)).await).await;
        }
    }

    #[tokio::test]
    async fn mcp_accepts_correct_token() {
        let (base, _dir) = spawn_gated_server("correct-token").await;
        let response = post_mcp(&base, Some("Bearer correct-token")).await;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.text().await.expect("read 200 body"), "inner-ok");
    }

    #[tokio::test]
    async fn rotated_token_takes_effect_without_restart() {
        let (base, dir) = spawn_gated_server("token-a").await;
        let primed = post_mcp(&base, Some("Bearer token-a")).await;
        assert_eq!(primed.status(), reqwest::StatusCode::OK);

        let path = token_path(dir.path());
        std::fs::write(&path, "token-b").expect("rotate daemon token");
        // Force a newer mtime so the gate must re-read even on filesystems
        // whose timestamp granularity could otherwise collide the two writes.
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open rotated token")
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .expect("bump rotated token mtime");

        assert_gate_rejection(post_mcp(&base, Some("Bearer token-a")).await).await;
        let response = post_mcp(&base, Some("Bearer token-b")).await;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    #[tokio::test]
    async fn non_mcp_routes_have_no_gate() {
        // `/sessions/ws` (stubbed here; the real handler enforces the token
        // itself) must stay reachable without the gate's 401 — a router-wide
        // layer would reject it with the JSON body instead.
        let (base, _dir) = spawn_gated_server("correct-token").await;
        let response = reqwest::Client::new()
            .get(format!("{base}/sessions/ws"))
            .send()
            .await
            .expect("GET /sessions/ws against the gated test server");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.text().await.expect("read ws body"), "ws-ok");
    }

    #[tokio::test]
    async fn mcp_trailing_slash_stays_behind_gate() {
        // The previous `nest_service("/mcp", …)` covered `/mcp/` too — the
        // explicit routes must preserve that coverage, including the gate.
        let (base, _dir) = spawn_gated_server("correct-token").await;
        let client = reqwest::Client::new();
        let post_slash = |authorization: Option<&str>| {
            let mut request = client
                .post(format!("{base}/mcp/"))
                .header("content-type", "application/json");
            if let Some(header) = authorization {
                request = request.header("authorization", header);
            }
            request.body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        };
        assert_gate_rejection(
            post_slash(None)
                .send()
                .await
                .expect("POST /mcp/ against the gated test server"),
        )
        .await;
        let response = post_slash(Some("Bearer correct-token"))
            .send()
            .await
            .expect("POST /mcp/ against the gated test server");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.text().await.expect("read 200 body"), "inner-ok");
    }

    /// The gate only matters if the production router mounts it: this pins
    /// `build_mcp_router` (`src/daemon/server.rs`) to [`authed_mcp_route`].
    /// Reverting the router to a bare `nest_service("/mcp", …)` keeps every
    /// other test in this module green, so without this assertion the
    /// production wiring could silently lose the gate.
    #[test]
    fn production_router_wires_daemon_gate() {
        let server = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("daemon")
            .join("server.rs");
        let content = std::fs::read_to_string(&server).expect("read server.rs");
        assert!(
            content.contains("authed_mcp_route"),
            "build_mcp_router must mount /mcp behind mcp_auth::authed_mcp_route"
        );
    }

    /// Regression guard for req 5: every in-repo HTTP caller of `/mcp` must
    /// build its `Authorization` header through `daemon_auth_header`, so a
    /// new caller that hand-rolls (or forgets) the header fails here instead
    /// of at runtime with a 401. `src/daemon/bridge.rs` goes through the
    /// `bridge_auth` wrapper (which itself calls `daemon_auth_header`), so a
    /// `bridge_auth` reference counts as compliant. `src/daemon/server.rs`
    /// is allowlisted: it _serves_ `/mcp` behind this very layer, and its
    /// bare requests are test-only probes asserting the 401 above. This file
    /// is allowlisted too: its own `/mcp` + `reqwest` hits are the
    /// gated-server test probes below (no-header / wrong-token / right-token
    /// / rotation), not production callers. `src/daemon/admin_remote.rs` is
    /// allowlisted for the same reason: it serves `/admin/remote` behind
    /// this layer, and its `reqwest` hits are test-only probes of that
    /// route — its production callers (the CLI and the TUI) build their
    /// header through `daemon_auth_header` in `remote::admin_client`.
    #[test]
    fn every_mcp_http_caller_uses_daemon_auth_header() {
        const ALLOWLISTED: &[&str] = &[
            "src/daemon/server.rs",
            "src/daemon/mcp_auth.rs",
            "src/daemon/admin_remote.rs",
        ];
        let mut offenders = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = stack.pop() {
            let entries = std::fs::read_dir(&dir).expect("read src dir");
            for entry in entries {
                let path = entry.expect("src dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                    continue;
                }
                let content = std::fs::read_to_string(&path).expect("read src file");
                let mentions_mcp = content.contains("/mcp");
                let builds_http = content.contains("reqwest")
                    || content.contains("TcpStream")
                    || content.contains("POST /mcp");
                let allowlisted = ALLOWLISTED
                    .iter()
                    .any(|suffix| path.to_string_lossy().ends_with(suffix));
                if mentions_mcp
                    && builds_http
                    && !content.contains("daemon_auth_header")
                    && !content.contains("bridge_auth")
                    && !allowlisted
                {
                    offenders.push(path.to_string_lossy().into_owned());
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these files call /mcp over HTTP without daemon_auth_header: {offenders:?}"
        );
    }
}
