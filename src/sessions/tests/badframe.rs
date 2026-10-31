//! Bad-frame tests: unknown `t` or missing fields answer
//! `error{code:"bad_request"}` naming the frame and the field; unknown
//! sessions answer `not_found`; writes to exited sessions are `forbidden`;
//! host metrics are remote-only on the local socket.

use super::{
    agent_spawn, connect_client, recv_json, send_json, serve_state, test_db, test_hub, KillGuard,
};
use crate::sessions::protocol::{parse_client_frame, ClientFrame};
use crate::sessions::ws::SessionsState;

fn parse_err(text: &str) -> String {
    match parse_client_frame(text) {
        Ok(frame) => panic!("expected a bad_request error, parsed {frame:?}"),
        Err(e) => {
            assert!(e.req.is_none(), "these frames carry no usable req");
            e.message
        }
    }
}

#[test]
fn unknown_frame_names_the_frame() {
    let message = parse_err(r#"{"t": "frobnicate"}"#);
    assert!(
        message.contains("frobnicate"),
        "must name the frame: {message}"
    );
}

#[test]
fn missing_field_names_frame_and_field() {
    let message = parse_err(r#"{"t": "attach"}"#);
    assert!(message.contains("attach"), "must name the frame: {message}");
    assert!(message.contains("id"), "must name the field: {message}");

    let message = parse_err(r#"{"t": "spawn", "req": "r1"}"#);
    assert!(message.contains("spawn"), "must name the frame: {message}");
    assert!(message.contains("kind"), "must name the field: {message}");

    let message = parse_err(r#"{"t": "list"}"#);
    assert!(message.contains("list"), "must name the frame: {message}");
    assert!(message.contains("req"), "must name the field: {message}");
}

#[test]
fn malformed_frames_are_bad_request() {
    assert!(parse_err("nope{{{").contains("JSON"));
    assert!(parse_err(r#"{"x": 1}"#).contains("'t'"));
    let wrong_type = parse_err(r#"{"t": "attach", "id": 42}"#);
    assert!(wrong_type.contains("attach") && wrong_type.contains("id"));
    let zero_size = parse_err(r#"{"t": "resize", "id": "x", "cols": 0, "rows": 24}"#);
    assert!(zero_size.contains("cols"));
    let bad_b64 = parse_err(r#"{"t": "input", "id": "x", "data": "!!!"}"#);
    assert!(bad_b64.contains("data"));
    let bad_yolo = parse_err(
        r#"{"t": "spawn", "req": "r", "kind": "agent", "cli": "c", "workdir": "/tmp", "yolo": "yes"}"#,
    );
    assert!(
        bad_yolo.contains("spawn") && bad_yolo.contains("yolo"),
        "must name the frame and the field: {bad_yolo}"
    );
}

#[test]
fn documented_shapes_parse() {
    assert!(matches!(
        parse_client_frame(r#"{"t": "list", "req": "r"}"#),
        Ok(ClientFrame::List { .. })
    ));
    assert!(matches!(
        parse_client_frame(r#"{"t": "attach", "id": "x"}"#),
        Ok(ClientFrame::Attach { .. })
    ));
    assert!(matches!(
        parse_client_frame(r#"{"t": "detach", "id": "x"}"#),
        Ok(ClientFrame::Detach { .. })
    ));
    assert!(matches!(
        parse_client_frame(r#"{"t": "input", "id": "x", "data": "aGk="}"#),
        Ok(ClientFrame::Input { .. })
    ));
    assert!(matches!(
        parse_client_frame(r#"{"t": "prompt", "req": "r", "id": "x", "text": "hi"}"#),
        Ok(ClientFrame::Prompt { .. })
    ));
    assert!(matches!(
        parse_client_frame(r#"{"t": "resize", "id": "x", "cols": 80, "rows": 24}"#),
        Ok(ClientFrame::Resize { .. })
    ));
    assert!(matches!(
        parse_client_frame(
            r#"{"t": "spawn", "req": "r", "kind": "agent", "cli": "c", "workdir": "/tmp"}"#
        ),
        Ok(ClientFrame::Spawn { .. })
    ));
    assert!(matches!(
        parse_client_frame(
            r#"{"t": "spawn", "req": "r", "kind": "terminal", "cli": "sh",
                "model": "m", "workdir": "/tmp", "name": "n", "prompt": "p"}"#
        ),
        Ok(ClientFrame::Spawn { .. })
    ));
    assert!(matches!(
        parse_client_frame(r#"{"t": "kill", "req": "r", "id": "x"}"#),
        Ok(ClientFrame::Kill { .. })
    ));
}

#[tokio::test]
async fn ws_answers_bad_request_for_bad_frames() {
    let (db, _db_dir) = test_db();
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(&mut client, &serde_json::json!({"t": "frobnicate"})).await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["t"], "error");
    assert_eq!(error["code"], "bad_request");
    assert!(error["message"]
        .as_str()
        .expect("message")
        .contains("frobnicate"));

    send_json(&mut client, &serde_json::json!({"t": "attach"})).await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["code"], "bad_request");
    let message = error["message"].as_str().expect("message");
    assert!(message.contains("attach") && message.contains("id"));

    // A bad frame does not break the connection: list still answers.
    send_json(&mut client, &serde_json::json!({"t": "list", "req": "l1"})).await;
    let listed = recv_json(&mut client, "sessions").await;
    assert_eq!(listed["t"], "sessions");
    assert_eq!(listed["req"], "l1");
    server.abort();
}

#[tokio::test]
async fn ws_answers_not_found_for_unknown_sessions() {
    let (db, _db_dir) = test_db();
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({"t": "attach", "id": "missing"}),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["code"], "not_found");
    assert!(error.get("req").is_none(), "attach carries no req to echo");

    send_json(
        &mut client,
        &serde_json::json!({"t": "kill", "req": "k9", "id": "missing"}),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["code"], "not_found");
    assert_eq!(error["req"], "k9", "kill echoes its req");
    server.abort();
}

#[tokio::test]
async fn writes_to_an_exited_session_are_forbidden() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) = KillGuard::spawn(
        &hub,
        &agent_spawn("true", &workdir.path().to_string_lossy()),
    );
    let id = session.id.clone();
    drop(session);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while hub
        .get(&id)
        .is_some_and(|s| s.status() == crate::sessions::SessionStatus::Running)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the child never exited"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let state = SessionsState::for_test(hub, "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({"t": "input", "id": id, "data": "aGk="}),
    )
    .await;
    assert_eq!(recv_json(&mut client, "error").await["code"], "forbidden");

    send_json(
        &mut client,
        &serde_json::json!({"t": "prompt", "req": "p1", "id": id, "text": "hi"}),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["code"], "forbidden");
    assert_eq!(error["req"], "p1");
    server.abort();
}

#[tokio::test]
async fn host_metrics_are_remote_only_on_the_local_socket() {
    let (db, _db_dir) = test_db();
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({"t": "host.metrics", "req": "m1"}),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["code"], "forbidden");
    assert_eq!(error["req"], "m1", "the one-shot echoes its req");

    send_json(
        &mut client,
        &serde_json::json!({"t": "host.metrics.watch", "interval_s": 5}),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["code"], "forbidden");
    assert!(error.get("req").is_none(), "watch carries no req to echo");
    server.abort();
}
