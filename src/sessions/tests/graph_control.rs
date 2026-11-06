//! `graph_control` tests: frame parsing, the unknown/archived-id refusal,
//! and the daemon-tool contract — pause and continue send exactly the FR2
//! arguments, a tool refusal reaches the client verbatim as `busy`, and an
//! unreachable daemon answers `internal`.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::Utc;
use serde_json::json;

use super::{connect_client, recv_json, send_json, serve_state, test_db, test_hub, try_recv_json};
use crate::domain::graphs::{Graph, GraphStatus};
use crate::sessions::graphs::run_graph_control;
use crate::sessions::protocol::{parse_client_frame, ClientFrame, GraphControlAction};
use crate::tui::mcp_client::test_support::{spawn_fake_daemon, unused_port};

// ── parsing ─────────────────────────────────────────────────────────────

#[test]
fn graph_control_frames_parse_pause_and_continue() {
    assert!(matches!(
        parse_client_frame(r#"{"t": "graph_control", "req": "c1", "id": "g1", "action": "pause"}"#),
        Ok(ClientFrame::GraphControl { req, id, action })
            if req == "c1" && id == "g1" && action == GraphControlAction::Pause
    ));
    assert!(matches!(
        parse_client_frame(
            r#"{"t": "graph_control", "req": "c2", "id": "g2", "action": "continue"}"#
        ),
        Ok(ClientFrame::GraphControl { req, id, action })
            if req == "c2" && id == "g2" && action == GraphControlAction::Continue
    ));
}

#[test]
fn graph_control_rejects_a_bad_action_naming_frame_field_and_req() {
    let error = parse_client_frame(
        r#"{"t": "graph_control", "req": "r1", "id": "g1", "action": "resume"}"#,
    )
    .unwrap_err();
    assert_eq!(error.req.as_deref(), Some("r1"), "must echo the req");
    assert!(
        error.message.contains("graph_control"),
        "must name the frame: {}",
        error.message
    );
    assert!(
        error.message.contains("action"),
        "must name the field: {}",
        error.message
    );
}

#[test]
fn graph_control_names_missing_fields() {
    let missing_req =
        parse_client_frame(r#"{"t": "graph_control", "id": "g1", "action": "pause"}"#).unwrap_err();
    assert!(
        missing_req.message.contains("req"),
        "missing req named: {}",
        missing_req.message
    );
    let missing_id =
        parse_client_frame(r#"{"t": "graph_control", "req": "r1", "action": "pause"}"#)
            .unwrap_err();
    assert!(
        missing_id.message.contains("id"),
        "missing id named: {}",
        missing_id.message
    );
    let missing_action =
        parse_client_frame(r#"{"t": "graph_control", "req": "r1", "id": "g1"}"#).unwrap_err();
    assert_eq!(
        missing_action.req.as_deref(),
        Some("r1"),
        "a missing action still echoes the req"
    );
    assert!(
        missing_action.message.contains("action"),
        "missing action named: {}",
        missing_action.message
    );
}

// ── the unknown/archived-id refusal over the socket ─────────────────────

fn graph_row(id: &str, archived: bool) -> Graph {
    Graph {
        id: id.to_string(),
        name: format!("graph-{id}"),
        description: None,
        workdir: "/srv/work/proj".to_string(),
        status: GraphStatus::Running,
        trigger: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: BTreeMap::new(),
        archived,
        paused_by_reconciliation: false,
        infra_node_id: None,
        allow_dirty_start: false,
        require_spec_type: false,
    }
}

#[tokio::test]
async fn graph_control_unknown_or_archived_id_answers_not_found() {
    let (db, _dir) = test_db();
    db.insert_graph(&graph_row("g-arch", true)).unwrap();
    let state = crate::sessions::ws::SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &json!({"t": "graph_control", "req": "c1", "id": "g-missing", "action": "pause"}),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["t"], "error");
    assert_eq!(error["code"], "not_found");
    assert_eq!(error["req"], "c1", "must echo the request: {error}");
    assert!(
        error["message"]
            .as_str()
            .expect("message")
            .contains("g-missing"),
        "must name the id: {error}"
    );

    send_json(
        &mut client,
        &json!({"t": "graph_control", "req": "c2", "id": "g-arch", "action": "continue"}),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["t"], "error");
    assert_eq!(
        error["code"], "not_found",
        "archived reads as not found: {error}"
    );
    assert_eq!(error["req"], "c2", "must echo the request: {error}");
    assert!(
        try_recv_json(&mut client, Duration::from_millis(500))
            .await
            .is_none(),
        "a refused control calls nothing and answers once"
    );
    server.abort();
}

// ── the daemon-tool contract ────────────────────────────────────────────

/// One `run_graph_control` answer.
async fn run_once(port: Option<u16>, action: GraphControlAction, req: &str) -> serde_json::Value {
    let (out_tx, mut rx) = crate::sessions::outbound::channel();
    run_graph_control(port, &out_tx, req.to_string(), "g1".to_string(), action).await;
    let reply = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("an answer arrives")
        .expect("the channel stays open");
    serde_json::from_str(&reply).expect("frames are JSON")
}

#[tokio::test]
async fn graph_control_pause_and_continue_call_the_daemon_tool_with_fr2_args() {
    let fake = spawn_fake_daemon(json!({
        "content": [{"type": "text", "text": "done"}],
        "isError": false,
    }));
    let port: u16 = fake.port.parse().expect("fake daemon port");

    let (out_tx, mut rx) = crate::sessions::outbound::channel();
    run_graph_control(
        Some(port),
        &out_tx,
        "p1".to_string(),
        "g1".to_string(),
        GraphControlAction::Pause,
    )
    .await;
    let paused: serde_json::Value = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("the pause answer arrives")
            .expect("the channel stays open"),
    )
    .expect("frames are JSON");
    assert_eq!(paused["t"], "ok", "pause succeeds: {paused}");
    assert_eq!(paused["req"], "p1", "must echo the request: {paused}");

    run_graph_control(
        Some(port),
        &out_tx,
        "c1".to_string(),
        "g1".to_string(),
        GraphControlAction::Continue,
    )
    .await;
    let continued: serde_json::Value = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("the continue answer arrives")
            .expect("the channel stays open"),
    )
    .expect("frames are JSON");
    assert_eq!(continued["t"], "ok", "continue succeeds: {continued}");
    assert_eq!(continued["req"], "c1", "must echo the request: {continued}");

    let calls = fake.recorded_calls();
    assert_eq!(calls.len(), 2, "one tool call per control: {calls:?}");
    assert_eq!(calls[0]["name"], "graph_control");
    assert_eq!(
        calls[0]["arguments"],
        json!({"action": "pause", "graph_id": "g1"})
    );
    assert_eq!(calls[1]["name"], "graph_control");
    assert_eq!(
        calls[1]["arguments"],
        json!({"action": "continue", "graph_id": "g1", "mode": "retry_current_node"})
    );
}

#[tokio::test]
async fn graph_control_tool_refusal_answers_busy_with_the_text_intact() {
    let fake = spawn_fake_daemon(json!({
        "content": [{"type": "text", "text": "Graph 'g1' is not running."}],
        "isError": true,
    }));
    let port: u16 = fake.port.parse().expect("fake daemon port");

    let answer = run_once(Some(port), GraphControlAction::Pause, "p1").await;
    assert_eq!(answer["t"], "error", "a refusal is an error: {answer}");
    assert_eq!(answer["code"], "busy", "a refusal is busy: {answer}");
    assert_eq!(answer["req"], "p1", "must echo the request: {answer}");
    assert_eq!(
        answer["message"], "Graph 'g1' is not running.",
        "the tool's text travels verbatim: {answer}"
    );
}

#[tokio::test]
async fn graph_control_unreachable_daemon_answers_internal() {
    // A freed port is racy under parallel tests — another test's fake
    // daemon can bind it between the probe and the call — so retry fresh
    // ports; a stolen port answers `ok` and the next attempt re-probes.
    for _ in 0..5 {
        let port: u16 = unused_port().parse().expect("unused port");
        let answer = run_once(Some(port), GraphControlAction::Pause, "p1").await;
        if answer["t"] == "error" && answer["code"] == "internal" {
            assert_eq!(answer["req"], "p1", "must echo the request: {answer}");
            return;
        }
    }
    panic!("an unreachable daemon must answer internal within five fresh ports");
}
