//! In-process tests for the device flow: a local axum mock stands in for
//! GitHub (base URLs injectable via [`RemoteEndpoints`]).

use super::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::routing::{get, post};
use axum::{Json, Router};

use crate::domain::canopy_config::RemoteConfig;

/// Spawn the mock GitHub: the device route answers fixed JSON (interval 0,
/// user code `ABCD-1234`), the token route pops one queued response per
/// poll, `/user` answers alice/42. Returns the base URL.
async fn spawn_mock(token_responses: Vec<Value>) -> String {
    let queue = Arc::new(Mutex::new(VecDeque::from(token_responses)));
    let app = Router::new()
        .route(
            "/login/device/code",
            post(|| async {
                Json(serde_json::json!({
                    "device_code": "mock-device-code",
                    "user_code": "ABCD-1234",
                    "verification_uri": "https://github.com/login/device",
                    "verification_uri_complete":
                        "https://github.com/login/device?user_code=ABCD-1234",
                    "expires_in": 900,
                    "interval": 0,
                }))
            }),
        )
        .route(
            "/login/oauth/access_token",
            post({
                let queue = Arc::clone(&queue);
                move || {
                    let queue = Arc::clone(&queue);
                    async move {
                        let next = queue
                            .lock()
                            .unwrap()
                            .pop_front()
                            .unwrap_or_else(|| serde_json::json!({"error": "expired_token"}));
                        Json(next)
                    }
                }
            }),
        )
        .route(
            "/user",
            get(|| async { Json(serde_json::json!({"login": "alice", "id": 42})) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn endpoints_for(base: &str) -> RemoteEndpoints {
    RemoteEndpoints {
        device_code_url: format!("{base}/login/device/code"),
        access_token_url: format!("{base}/login/oauth/access_token"),
        user_url: format!("{base}/user"),
    }
}

/// Captured `on_code` payload in tests: the user code plus the
/// verification URL.
type SeenCode = Arc<Mutex<Option<(String, Option<String>)>>>;

fn test_identity() -> GitHubIdentity {
    GitHubIdentity {
        token: "gho_test_secret".to_string(),
        login: "alice".to_string(),
        id: 42,
    }
}

#[tokio::test]
async fn login_pending_then_success_stores_identity() {
    let base = spawn_mock(vec![
        serde_json::json!({"error": "authorization_pending"}),
        serde_json::json!({"access_token": "gho_test_secret", "token_type": "bearer"}),
    ])
    .await;
    let dir = tempfile::TempDir::new().unwrap();
    let seen = SeenCode::default();
    let seen_clone = Arc::clone(&seen);
    let identity = login(
        "test-client-id",
        dir.path(),
        &endpoints_for(&base),
        |info| {
            *seen_clone.lock().unwrap() = Some((
                info.user_code.clone(),
                info.verification_uri_complete.clone(),
            ));
        },
    )
    .await
    .unwrap();
    assert_eq!(identity.login, "alice");
    assert_eq!(identity.id, 42);
    assert_eq!(identity.token, "gho_test_secret");

    let (user_code, complete) = seen.lock().unwrap().clone().unwrap();
    assert_eq!(user_code, "ABCD-1234");
    assert!(complete.unwrap().contains("ABCD-1234"));

    assert_eq!(load_identity(dir.path()).as_ref(), Some(&identity));
}

#[test]
fn slow_down_raises_the_interval_by_five_seconds() {
    assert!(matches!(
        poll_decision(&serde_json::json!({"error": "slow_down"}), 5),
        PollDecision::Retry(10)
    ));
    assert!(matches!(
        poll_decision(&serde_json::json!({"error": "authorization_pending"}), 5),
        PollDecision::Retry(5)
    ));
    assert!(matches!(
        poll_decision(&serde_json::json!({"access_token": "gho_x"}), 5),
        PollDecision::Success
    ));
}

#[tokio::test]
async fn expired_token_stops_login_without_storing() {
    let base = spawn_mock(vec![serde_json::json!({"error": "expired_token"})]).await;
    let dir = tempfile::TempDir::new().unwrap();
    let err = login("test-client-id", dir.path(), &endpoints_for(&base), |_| {})
        .await
        .unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");
    assert!(load_identity(dir.path()).is_none());
}

#[tokio::test]
async fn access_denied_stops_login_without_storing() {
    let base = spawn_mock(vec![serde_json::json!({"error": "access_denied"})]).await;
    let dir = tempfile::TempDir::new().unwrap();
    let err = login("test-client-id", dir.path(), &endpoints_for(&base), |_| {})
        .await
        .unwrap_err();
    assert!(err.to_string().contains("denied"), "{err}");
    assert!(load_identity(dir.path()).is_none());
}

#[test]
fn stored_identity_file_has_mode_0600_and_dir_0700() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::TempDir::new().unwrap();
    save_identity(dir.path(), &test_identity()).unwrap();
    let file_mode = std::fs::metadata(identity_path(dir.path()))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(file_mode, 0o600);
    let dir_mode = std::fs::metadata(dir.path().join("remote"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700);
}

#[test]
fn status_json_reports_signed_in_login_enabled_and_relay_url() {
    let config = CanopyConfig {
        remote: RemoteConfig {
            enabled: true,
            ..RemoteConfig::default()
        },
        ..CanopyConfig::default()
    };
    let status = build_remote_status(&config, Some(&test_identity()));
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "signed_in": true,
            "login": "alice",
            "enabled": true,
            "relay_url": "https://remote.univerlab.org",
        })
    );
    assert!(!serde_json::to_string(&status)
        .unwrap()
        .contains("gho_test_secret"));

    let signed_out = build_remote_status(&CanopyConfig::default(), None);
    assert!(!signed_out.signed_in);
    assert_eq!(signed_out.login, None);
}

#[test]
fn debug_and_errors_never_carry_the_token() {
    let rendered = format!("{:?}", test_identity());
    assert!(!rendered.contains("gho_test_secret"), "{rendered}");
    for body in [
        serde_json::json!({"error": "expired_token"}),
        serde_json::json!({"error": "access_denied"}),
        serde_json::json!({"unexpected": true}),
    ] {
        let PollDecision::Fail(message) = poll_decision(&body, 5) else {
            panic!("expected Fail for {body}");
        };
        assert!(!message.contains("gho_test_secret"), "{message}");
        assert!(!message.contains('\n'), "{message}");
    }
}

#[tokio::test]
async fn empty_client_id_fails_before_any_http() {
    let dir = tempfile::TempDir::new().unwrap();
    // Unroutable endpoints prove no HTTP happens: the client-id check fires first.
    let endpoints = RemoteEndpoints {
        device_code_url: "http://127.0.0.1:1/device".to_string(),
        access_token_url: "http://127.0.0.1:1/token".to_string(),
        user_url: "http://127.0.0.1:1/user".to_string(),
    };
    for client_id in ["", "   "] {
        let err = login(client_id, dir.path(), &endpoints, |_| {
            unreachable!("no code is issued without HTTP")
        })
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "remote.github_client_id is not set in ~/.canopy/config.toml"
        );
    }
}

#[test]
fn logout_is_idempotent() {
    let dir = tempfile::TempDir::new().unwrap();
    logout(dir.path()).unwrap();
    save_identity(dir.path(), &test_identity()).unwrap();
    assert!(load_identity(dir.path()).is_some());
    logout(dir.path()).unwrap();
    assert!(load_identity(dir.path()).is_none());
    logout(dir.path()).unwrap();
}
