//! Auth tests: token file shape and the 401-before-upgrade gate.

use super::{connect_client, connect_raw, recv_json, send_json, serve_state, test_db, test_hub};
use crate::sessions::auth::{ensure_token, token_path, token_valid};
use crate::sessions::ws::SessionsState;

#[test]
fn token_file_is_hex_mode_0600_and_stable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token = ensure_token(dir.path()).expect("create token");
    assert_eq!(token.len(), 64, "32 bytes as hex");
    assert!(
        token.chars().all(|c| c.is_ascii_hexdigit()),
        "token must be hex: {token}"
    );

    let second = ensure_token(dir.path()).expect("read token");
    assert_eq!(second, token, "an existing token is reused, not rotated");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(token_path(dir.path()))
            .expect("token file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "token file must be owner-only");
    }
}

#[test]
fn blank_token_file_is_replaced() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(token_path(dir.path()), "  \n").expect("blank token");
    let token = ensure_token(dir.path()).expect("replace token");
    assert_eq!(token.len(), 64);
}

#[test]
fn token_comparison_is_exact() {
    assert!(token_valid("abc123", "abc123"));
    assert!(!token_valid("abc123", "abc124"));
    assert!(!token_valid("abc12", "abc123"));
    assert!(!token_valid("", "abc123"));
    assert!(!token_valid("abc123", ""));
}

#[tokio::test]
async fn ws_rejects_missing_token_with_401() {
    let (db, _dir) = test_db();
    let state = SessionsState::for_test(test_hub(&db), "test-token".to_string());
    let (addr, server) = serve_state(state).await;

    let err = connect_raw(addr, None).await.expect_err("must refuse");
    match *err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 401);
            let body = response.into_body().unwrap_or_default();
            assert!(
                !String::from_utf8_lossy(&body).contains("test-token"),
                "the 401 response must never contain the token"
            );
        }
        other => panic!("expected HTTP 401, got {other:?}"),
    }
    server.abort();
}

#[tokio::test]
async fn ws_rejects_wrong_token_with_401() {
    let (db, _dir) = test_db();
    let state = SessionsState::for_test(test_hub(&db), "correct-token".to_string());
    let (addr, server) = serve_state(state).await;

    let err = connect_raw(addr, Some("wrong-token"))
        .await
        .expect_err("must refuse");
    match *err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 401);
        }
        other => panic!("expected HTTP 401, got {other:?}"),
    }
    server.abort();
}

#[tokio::test]
async fn ws_accepts_correct_token_and_sends_hello() {
    let (db, _dir) = test_db();
    let state = SessionsState::for_test(test_hub(&db), "good-token".to_string());
    let (addr, server) = serve_state(state).await;

    let mut client = connect_client(addr, Some("good-token")).await;
    let hello = recv_json(&mut client, "hello").await;
    assert_eq!(hello["t"], "hello");
    assert_eq!(hello["proto"], 1);
    assert_eq!(hello["host_label"], "test-host");
    server.abort();
}

#[tokio::test]
async fn protocol_errors_never_contain_the_token() {
    let (db, _dir) = test_db();
    let token = "super-secret-token-value";
    let state = SessionsState::for_test(test_hub(&db), token.to_string());
    let (addr, server) = serve_state(state).await;

    let mut client = connect_client(addr, Some(token)).await;
    let _hello = recv_json(&mut client, "hello").await;
    send_json(
        &mut client,
        &serde_json::json!({"t": "attach", "id": "nope"}),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["code"], "not_found");
    assert!(
        !error.to_string().contains(token),
        "error frames must never leak the token"
    );
    server.abort();
}
