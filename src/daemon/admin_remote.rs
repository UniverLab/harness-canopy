//! The token-authenticated admin route for the four privileged `remote`
//! actions (audit finding F-01, second part).
//!
//! Agents calling the MCP `remote` tool can no longer pair a device, trust
//! a guest, revoke a device or switch remote access on: those actions moved
//! here, behind the same daemon-token gate as `/mcp` plus an explicit
//! `"confirm": true` in the body. The CLI and the TUI are the only callers,
//! and both ask a human first. The route unwraps the MCP envelope — callers
//! never see a `CallToolResult`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;
use serde_json::{json, Value};

use crate::daemon::handler::HandlerDeps;
use crate::daemon::handler_remote::{remote_pair_impl, remote_set_enabled_impl};
use crate::daemon::params::{RemoteRevokeParams, RemoteSetEnabledParams, RemoteTrustAddParams};
use crate::daemon::TaskTriggerHandler;

/// The `POST /admin/remote` method router, ungated: the caller wraps it
/// with `mcp_auth::authed_mcp_route` (the bearer-token gate, 401 shape
/// unchanged) and supplies `HandlerDeps` as state.
pub(crate) fn admin_remote_method_router() -> axum::routing::MethodRouter<HandlerDeps> {
    axum::routing::post(admin_remote_handler)
}

/// The token-gated `/admin/remote` router, ready to merge: the same
/// bearer-token gate as `/mcp` and nothing else; `/mcp` and
/// `/sessions/ws` are untouched. The handler builds per request from the
/// same deps the `/mcp` session factory closes over.
pub(crate) fn admin_remote_router<S>(
    data_dir: &std::path::Path,
    deps: &HandlerDeps,
) -> axum::Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let authed = super::mcp_auth::authed_mcp_route(admin_remote_method_router(), data_dir);
    axum::Router::new().route("/admin/remote", authed.with_state(deps.clone()))
}

/// One parsed `POST /admin/remote` call: the action plus its parameters.
/// `confirm` stays on the raw body — `set_enabled` with `enabled: false`
/// only reduces access, so it is the one call that needs no confirm.
struct AdminRemoteCall {
    action: String,
    enabled: Option<bool>,
    device_id: Option<String>,
    login: Option<String>,
    all_sessions: Option<bool>,
    sessions: Vec<String>,
    upload: bool,
}

/// The `400` answer for a request error: `{"error": <message>}`.
fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(json!({ "error": message.into() })),
    )
        .into_response()
}

/// The `422` answer for a tool-level refusal (unknown device id, a panel
/// failure): `{"error": "<its text>"}`.
fn unprocessable(text: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        axum::Json(json!({ "error": text })),
    )
        .into_response()
}

/// The `500` answer for an MCP error or an unparsable result envelope.
fn server_error(message: impl Into<String>) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({ "error": message.into() })),
    )
        .into_response()
}

/// Parse the request body into one call. Anything malformed — a body that
/// is not JSON, a missing or unknown action, missing or mistyped params —
/// is a `400`; the message names the problem. Params deserialize through
/// the same typed structs the MCP tool used, so the shapes stay identical.
fn parse_call(body: &[u8]) -> Result<(AdminRemoteCall, bool), String> {
    let value: Value = serde_json::from_slice(body).map_err(|_| "malformed body".to_string())?;
    let action = value
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing action".to_string())?;
    if !matches!(action, "pair" | "set_enabled" | "revoke" | "trust_add") {
        return Err(format!("unknown action: {action}"));
    }
    let mut call = AdminRemoteCall {
        action: action.to_string(),
        enabled: None,
        device_id: None,
        login: None,
        all_sessions: None,
        sessions: Vec::new(),
        upload: false,
    };
    match action {
        "set_enabled" => {
            let params: RemoteSetEnabledParams = typed_param(&value, action)?;
            call.enabled = Some(params.enabled);
        }
        "revoke" => {
            let params: RemoteRevokeParams = typed_param(&value, action)?;
            call.device_id = Some(params.device_id);
        }
        "trust_add" => {
            let params: RemoteTrustAddParams = typed_param(&value, action)?;
            call.login = Some(params.login);
            call.all_sessions = Some(params.all_sessions);
            call.sessions = params.sessions.unwrap_or_default();
            call.upload = params.upload.unwrap_or(false);
        }
        _ => {}
    }
    let confirm = value
        .get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok((call, confirm))
}

/// Deserialize the action's params through its typed struct (`action` and
/// `confirm` are ignored as unknown fields). A missing or mistyped param
/// is a `400` naming the action.
fn typed_param<T: serde::de::DeserializeOwned>(value: &Value, action: &str) -> Result<T, String> {
    serde_json::from_value(value.clone())
        .map_err(|error| format!("missing {action} param: {error}"))
}

/// Dispatch one confirmed call through the four widened `remote_*`
/// implementations. The handler is built the way `build_mcp_router`'s
/// session factory builds it.
fn dispatch(deps: &HandlerDeps, call: &AdminRemoteCall) -> Result<CallToolResult, McpError> {
    match call.action.as_str() {
        "pair" => remote_pair_impl(),
        "set_enabled" => remote_set_enabled_impl(call.enabled.unwrap_or(false)),
        "revoke" => {
            let handler = TaskTriggerHandler::with_deps(deps.clone());
            handler.remote_revoke_impl(call.device_id.as_deref().unwrap_or_default().trim())
        }
        "trust_add" => {
            let handler = TaskTriggerHandler::with_deps(deps.clone());
            handler.remote_trust_add_impl(
                call.login.as_deref().unwrap_or_default().trim(),
                call.all_sessions.unwrap_or(false),
                &call.sessions,
                call.upload,
            )
        }
        _ => unreachable!("parse_call only admits the four admin actions"),
    }
}

/// The text of a tool result: the single text block, or `None` when the
/// envelope holds anything else.
fn result_text(result: &CallToolResult) -> Option<String> {
    if result.content.len() == 1 {
        result.content[0].as_text().map(|text| text.text.clone())
    } else {
        None
    }
}

/// Unwrap the MCP envelope into the HTTP contract: a clean result answers
/// `200` with the JSON parsed from its single text block; a tool-level
/// refusal answers `422`; an unparsable block is a `500`.
fn answer(result: Result<CallToolResult, McpError>) -> Response {
    match result {
        Err(error) => server_error(error.message.to_string()),
        Ok(result) if result.is_error == Some(true) => match result_text(&result) {
            Some(text) => unprocessable(&text),
            None => server_error("unreadable result"),
        },
        Ok(result) => match result_text(&result) {
            Some(text) => match serde_json::from_str::<Value>(&text) {
                Ok(body) => (StatusCode::OK, axum::Json(body)).into_response(),
                Err(_) => server_error("unreadable result"),
            },
            None => server_error("unreadable result"),
        },
    }
}

/// `POST /admin/remote` with `{"action": ..., <params>, "confirm": true}`.
/// Confirm is required for every action except `set_enabled` with
/// `enabled: false` (checked before anything runs); anything else wrong
/// with the request is a `400`. Any other method answers `405` — the router
/// only mounts `POST`.
async fn admin_remote_handler(
    State(deps): State<HandlerDeps>,
    body: axum::body::Bytes,
) -> Response {
    let (call, confirm) = match parse_call(&body) {
        Ok(parsed) => parsed,
        Err(message) => return bad_request(message),
    };
    let needs_confirm = !(call.action == "set_enabled" && call.enabled == Some(false));
    if needs_confirm && !confirm {
        return bad_request("confirm required");
    }
    answer(dispatch(&deps, &call))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Serializes the tests that point `$HOME` at a temp dir (pair and
    /// set_enabled resolve their data dir from home, exactly like the
    /// daemon's own impls do). Async-aware, so holding it across the
    /// test's awaits is sound.
    static HOME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    const TEST_TOKEN: &str = "test-admin-token";

    /// One live `/admin/remote` server behind the real token gate, with
    /// `$HOME` pointed at a temp dir for the whole test. The guard and the
    /// temp dirs stay alive with the server.
    struct AdminServer {
        url: String,
        _home_guard: tokio::sync::MutexGuard<'static, ()>,
        home: tempfile::TempDir,
        _gate: tempfile::TempDir,
        _scratch: tempfile::TempDir,
        db: Arc<crate::db::Database>,
        prev_home: Option<std::ffi::OsString>,
    }

    impl Drop for AdminServer {
        fn drop(&mut self) {
            if let Some(prev) = self.prev_home.take() {
                std::env::set_var("HOME", prev);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    fn test_deps(db: &Arc<crate::db::Database>, scratch: &std::path::Path) -> HandlerDeps {
        use crate::application::notification_service::{
            DefaultNotificationService, NotificationService,
        };
        let notif: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
        let executor = Arc::new(crate::executor::Executor::new(
            Arc::clone(db),
            Arc::clone(&notif),
        ));
        let graph_engine = Arc::new(crate::graph_engine::GraphEngine::new(
            Arc::clone(db),
            Arc::clone(&notif),
        ));
        let watcher_engine = Arc::new(crate::watchers::WatcherEngine::new(
            Arc::clone(db),
            Arc::clone(&executor),
            Arc::clone(&graph_engine),
        ));
        let ingestion = Arc::new(crate::rag::ingestion::IngestionManager::new(
            Arc::clone(db),
            scratch.to_path_buf(),
        ));
        let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::new(
            scratch.join("skills"),
            Vec::new(),
            15,
        ));
        HandlerDeps {
            db: Arc::clone(db),
            executor,
            watcher_engine,
            scheduler_notify: Arc::new(tokio::sync::Notify::new()),
            scheduled_prompt_notify: Arc::new(tokio::sync::Notify::new()),
            graph_engine,
            notification_service: notif,
            ingestion,
            dynamic_skills,
            shelf: crate::sessions::shelf::ShelfStore::for_test(db),
            start_time: std::time::Instant::now(),
            port: 0,
        }
    }

    async fn spawn_admin() -> AdminServer {
        let home_guard = HOME_LOCK.lock().await;
        let home = tempfile::tempdir().expect("home dir");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        let gate = tempfile::tempdir().expect("gate dir");
        std::fs::write(
            gate.path().join(crate::sessions::auth::TOKEN_FILE_NAME),
            TEST_TOKEN,
        )
        .expect("token file");
        let scratch = tempfile::tempdir().expect("db dir");
        let db =
            Arc::new(crate::db::Database::new(&scratch.path().join("test.db")).expect("test db"));
        let deps = test_deps(&db, scratch.path());
        let authed =
            crate::daemon::mcp_auth::authed_mcp_route(admin_remote_method_router(), gate.path());
        let router = axum::Router::new().route("/admin/remote", authed.with_state(deps));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!(
            "http://127.0.0.1:{}/admin/remote",
            listener.local_addr().expect("port").port()
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        AdminServer {
            url,
            _home_guard: home_guard,
            home,
            _gate: gate,
            _scratch: scratch,
            db,
            prev_home,
        }
    }

    async fn post(
        server: &AdminServer,
        token: Option<&str>,
        body: &serde_json::Value,
    ) -> reqwest::Response {
        let mut request = reqwest::Client::new().post(&server.url).json(body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().await.expect("post admin")
    }

    async fn error_body(response: reqwest::Response) -> (u16, serde_json::Value) {
        let status = response.status().as_u16();
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("json body");
        (status, body)
    }

    fn pair_device(db: &Arc<crate::db::Database>, label: &str) -> String {
        crate::remote::devices::RemoteDevices::new(db.as_ref().clone())
            .pair(
                format!("test spki for {label}").into_bytes(),
                label,
                "octocat",
                "2026-01-01T00:00:00Z",
            )
            .expect("pair the device")
            .id
    }

    #[tokio::test]
    async fn no_token_answers_401() {
        let server = spawn_admin().await;
        let response = post(
            &server,
            None,
            &serde_json::json!({ "action": "pair", "confirm": true }),
        )
        .await;
        assert_eq!(response.status().as_u16(), 401);
    }

    #[tokio::test]
    async fn wrong_token_answers_401() {
        let server = spawn_admin().await;
        let response = post(
            &server,
            Some("wrong-token"),
            &serde_json::json!({ "action": "pair", "confirm": true }),
        )
        .await;
        assert_eq!(response.status().as_u16(), 401);
    }

    #[tokio::test]
    async fn every_privileged_action_needs_confirm() {
        let server = spawn_admin().await;
        for body in [
            serde_json::json!({ "action": "pair" }),
            serde_json::json!({ "action": "set_enabled", "enabled": true }),
            serde_json::json!({ "action": "revoke", "device_id": "bfcba2b742a807bb" }),
            serde_json::json!({ "action": "trust_add", "login": "octocat", "all_sessions": true }),
        ] {
            let (status, body) = error_body(post(&server, Some(TEST_TOKEN), &body).await).await;
            assert_eq!(status, 400, "{body}");
            assert_eq!(
                body.get("error").and_then(Value::as_str),
                Some("confirm required")
            );
        }
    }

    #[tokio::test]
    async fn set_enabled_false_needs_no_confirm() {
        let server = spawn_admin().await;
        let response = post(
            &server,
            Some(TEST_TOKEN),
            &serde_json::json!({ "action": "set_enabled", "enabled": false }),
        )
        .await;
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response.json::<serde_json::Value>().await.expect("json"),
            serde_json::json!({ "enabled": false, "closed": 0 })
        );
    }

    #[tokio::test]
    async fn unknown_action_malformed_body_and_missing_params_are_400() {
        let server = spawn_admin().await;
        let (status, body) = error_body(
            post(
                &server,
                Some(TEST_TOKEN),
                &serde_json::json!({ "action": "status", "confirm": true }),
            )
            .await,
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert!(body
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| error.contains("unknown action")));

        let raw = reqwest::Client::new()
            .post(&server.url)
            .bearer_auth(TEST_TOKEN)
            .body("not json")
            .send()
            .await
            .expect("post raw");
        assert_eq!(raw.status().as_u16(), 400);

        let (status, body) = error_body(
            post(
                &server,
                Some(TEST_TOKEN),
                &serde_json::json!({ "action": "revoke", "confirm": true }),
            )
            .await,
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert!(
            body.get("error")
                .and_then(Value::as_str)
                .is_some_and(|error| error.contains("revoke") && error.contains("device_id")),
            "{body}"
        );
    }

    #[tokio::test]
    async fn other_methods_answer_405() {
        let server = spawn_admin().await;
        let response = reqwest::Client::new()
            .get(&server.url)
            .bearer_auth(TEST_TOKEN)
            .send()
            .await
            .expect("get admin");
        assert_eq!(response.status().as_u16(), 405);
    }

    #[tokio::test]
    async fn pair_returns_the_pairing_view() {
        let server = spawn_admin().await;
        let response = post(
            &server,
            Some(TEST_TOKEN),
            &serde_json::json!({ "action": "pair", "confirm": true }),
        )
        .await;
        assert_eq!(response.status().as_u16(), 200);
        let body = response.json::<serde_json::Value>().await.expect("json");
        for key in ["code", "link", "expires_at", "qr"] {
            assert!(body.get(key).is_some(), "pairing has {key}: {body}");
        }
        assert!(
            server
                .home
                .path()
                .join(".canopy/remote/pairing.json")
                .exists(),
            "the code is written under the temp HOME, never the real one"
        );
    }

    #[tokio::test]
    async fn set_enabled_true_returns_enabled() {
        let server = spawn_admin().await;
        let response = post(
            &server,
            Some(TEST_TOKEN),
            &serde_json::json!({ "action": "set_enabled", "enabled": true, "confirm": true }),
        )
        .await;
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response.json::<serde_json::Value>().await.expect("json"),
            serde_json::json!({ "enabled": true })
        );
    }

    #[tokio::test]
    async fn revoke_round_trip_and_unknown_id_is_422() {
        let server = spawn_admin().await;
        let id = pair_device(&server.db, "laptop");
        let response = post(
            &server,
            Some(TEST_TOKEN),
            &serde_json::json!({ "action": "revoke", "device_id": id, "confirm": true }),
        )
        .await;
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response.json::<serde_json::Value>().await.expect("json"),
            serde_json::json!({ "revoked": id, "closed": 0 })
        );

        // A live peer reports `closed: 1` and is cancelled at once. This
        // test holds `HOME_LOCK` for its whole life, so no other admin test
        // can close the shared peers in between.
        let live_id = pair_device(&server.db, "live laptop");
        let live = crate::remote::live::shared();
        let token = std::sync::Arc::new(tokio_util::sync::CancellationToken::new());
        let (control, _inbox) = tokio::sync::mpsc::unbounded_channel();
        assert!(live
            .peers()
            .register(&live_id, "d:live", token.clone(), control));
        let response = post(
            &server,
            Some(TEST_TOKEN),
            &serde_json::json!({ "action": "revoke", "device_id": live_id, "confirm": true }),
        )
        .await;
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response.json::<serde_json::Value>().await.expect("json"),
            serde_json::json!({ "revoked": live_id, "closed": 1 })
        );
        assert!(token.is_cancelled(), "revoke closes the live peer at once");
        live.peers().close(&live_id);

        let (status, body) = error_body(
            post(
                &server,
                Some(TEST_TOKEN),
                &serde_json::json!({
                    "action": "revoke",
                    "device_id": "missingdevice00",
                    "confirm": true,
                }),
            )
            .await,
        )
        .await;
        assert_eq!(status, 422, "{body}");
        assert!(
            body.get("error")
                .and_then(Value::as_str)
                .is_some_and(|error| error.contains("no paired device")),
            "{body}"
        );
    }

    #[tokio::test]
    async fn trust_add_returns_the_profile() {
        let server = spawn_admin().await;
        let response = post(
            &server,
            Some(TEST_TOKEN),
            &serde_json::json!({
                "action": "trust_add",
                "login": "octocat",
                "all_sessions": true,
                "confirm": true,
            }),
        )
        .await;
        assert_eq!(response.status().as_u16(), 200);
        let body = response.json::<serde_json::Value>().await.expect("json");
        assert_eq!(body.get("login").and_then(Value::as_str), Some("octocat"));
    }
}
