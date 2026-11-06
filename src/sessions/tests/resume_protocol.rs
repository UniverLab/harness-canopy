//! `resumable_list` and `session_resume`: the filters the picker applies,
//! the answers a live and an unknown id get, and a resume keeping the row's
//! own identity.

use std::time::Duration;

use serde_json::Value;

use super::*;
use crate::domain::canopy_config::CanopyConfig;
use crate::sessions::protocol::{
    parse_client_frame, resumable_frame, resumable_list_frame, session_resume_frame, ResumableItem,
    ERR_NOT_FOUND, OWNER_FEATURES,
};
use crate::sessions::protocol_resume::{
    handle_resumable_list, handle_session_resume, resumable_candidates,
};
use crate::sessions::resume::resume_recorded_session;
use crate::sessions::ws::SessionsState;

fn config_with(toml: &str) -> (tempfile::TempDir, CanopyConfig) {
    let dir = tempfile::tempdir().expect("config dir");
    std::fs::write(dir.path().join("config.toml"), toml).expect("write test config");
    let config = CanopyConfig::load(dir.path());
    (dir, config)
}

/// A config whose `cat` harness can resume a recorded conversation, so the
/// picker's harness filter keeps its rows.
fn resumable_cat() -> (tempfile::TempDir, CanopyConfig) {
    config_with("[[clis]]\nname = \"cat\"\nsession_resume_cmd = \"resume\"\n")
}

/// Insert one finished row the picker could offer.
fn insert_finished(db: &Database, id: &str, cli: &str, harness_id: Option<&str>) {
    db.insert_interactive_session(id, id, cli, "/tmp", None, None, "interactive", None)
        .unwrap();
    if let Some(harness_id) = harness_id {
        db.set_session_harness_conversation_id(id, harness_id)
            .unwrap();
    }
    db.mark_session_closed(id).unwrap();
}

fn ids(items: &[ResumableItem]) -> Vec<String> {
    items.iter().map(|item| item.id.clone()).collect()
}

#[test]
fn owner_devices_are_offered_session_resume() {
    assert!(
        OWNER_FEATURES.contains(&"session_resume"),
        "the owner's devices must be offered session_resume: {OWNER_FEATURES:?}"
    );
}

#[test]
fn client_frames_parse_and_require_their_fields() {
    assert!(matches!(
        parse_client_frame(&resumable_list_frame("r1")),
        Ok(crate::sessions::protocol::ClientFrame::ResumableList { req }) if req == "r1"
    ));
    assert!(matches!(
        parse_client_frame(&session_resume_frame("r2", "s1")),
        Ok(crate::sessions::protocol::ClientFrame::SessionResume { req, id })
            if req == "r2" && id == "s1"
    ));

    let missing_req = parse_client_frame(r#"{"t": "resumable_list"}"#)
        .expect_err("a missing req must be refused");
    assert!(missing_req.message.contains("missing field 'req'"));
    let missing_resume_id = parse_client_frame(r#"{"t": "session_resume", "req": "r3"}"#)
        .expect_err("a missing id must be refused");
    assert!(missing_resume_id.message.contains("missing field 'id'"));
    let missing_resume_req = parse_client_frame(r#"{"t": "session_resume", "id": "s1"}"#)
        .expect_err("a missing req must be refused");
    assert!(missing_resume_req.message.contains("missing field 'req'"));
}

#[test]
fn resumable_frame_lists_the_rows_under_sessions() {
    let item = ResumableItem {
        id: "s1".to_string(),
        name: "one".to_string(),
        cli: "cat".to_string(),
        workdir: "/work".to_string(),
        started_at: "2026-11-06T00:00:00Z".to_string(),
        has_conversation: true,
    };
    let frame: Value =
        serde_json::from_str(&resumable_frame("r1", std::slice::from_ref(&item))).unwrap();
    assert_eq!(frame["t"], "resumable");
    assert_eq!(frame["req"], "r1");
    assert_eq!(frame["sessions"][0]["id"], "s1");
    assert_eq!(frame["sessions"][0]["workdir"], "/work");
    assert_eq!(frame["sessions"][0]["has_conversation"], true);
}

#[test]
fn resumable_candidates_drop_a_row_live_in_the_hub() {
    let (db, _dir) = test_db();
    let (_config_dir, config) = resumable_cat();
    let hub = test_hub(&db);
    let (live, _guard) = KillGuard::spawn(
        &hub,
        &SpawnRequest {
            id: Some("live-1".to_string()),
            ..agent_spawn("cat", "/tmp")
        },
    );
    assert_eq!(live.id, "live-1");
    // The row is finished in the database while its PTY is still live.
    db.mark_session_closed("live-1").unwrap();
    insert_finished(&db, "dead-1", "cat", None);

    let candidates = resumable_candidates(&hub, &config);

    assert_eq!(ids(&candidates), vec!["dead-1".to_string()]);
}

#[test]
fn resumable_candidates_drop_a_harness_without_resume_machinery() {
    let (db, _dir) = test_db();
    let (_config_dir, config) = config_with(
        "[[clis]]\nname = \"cat\"\nsession_resume_cmd = \"resume\"\n\n\
         [[clis]]\nname = \"freshonly\"\n",
    );
    let hub = test_hub(&db);
    insert_finished(&db, "no-resume", "freshonly", None);
    insert_finished(&db, "unknown", "not-configured", None);
    insert_finished(&db, "resumable", "cat", None);

    let candidates = resumable_candidates(&hub, &config);

    assert_eq!(ids(&candidates), vec!["resumable".to_string()]);
}

#[test]
fn resumable_candidates_keep_the_newest_row_per_conversation() {
    let (db, _dir) = test_db();
    let (_config_dir, config) = resumable_cat();
    let hub = test_hub(&db);
    insert_finished(&db, "older", "cat", Some("conv-shared"));
    std::thread::sleep(Duration::from_millis(5));
    insert_finished(&db, "newer", "cat", Some("conv-shared"));

    let candidates = resumable_candidates(&hub, &config);

    assert_eq!(ids(&candidates), vec!["newer".to_string()]);
    assert!(
        candidates[0].has_conversation,
        "a recorded conversation id is reported as such"
    );
}

#[test]
fn resumable_candidates_cap_the_list_at_fifty() {
    let (db, _dir) = test_db();
    let (_config_dir, config) = resumable_cat();
    let hub = test_hub(&db);
    for index in 0..60 {
        insert_finished(&db, &format!("s{index}"), "cat", None);
    }

    let candidates = resumable_candidates(&hub, &config);

    assert_eq!(candidates.len(), 50, "the answer is capped after filtering");
}

fn state_with(hub: SessionHub) -> Arc<SessionsState> {
    SessionsState::for_test(hub, "token".to_string())
}

async fn ask_resumable_list(state: &Arc<SessionsState>, req: &str) -> Value {
    let (out, mut rx) = crate::sessions::outbound::channel();
    handle_resumable_list(state, &out, req.to_string());
    let reply = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("an answer arrives")
        .expect("the channel stays open");
    serde_json::from_str(&reply).expect("frames are JSON")
}

#[tokio::test]
async fn resumable_list_answers_the_wire_frame() {
    let (db, _dir) = test_db();
    let state = state_with(test_hub(&db));

    let answer = ask_resumable_list(&state, "r0").await;

    assert_eq!(answer["t"], "resumable", "got {answer}");
    assert_eq!(answer["req"], "r0");
    assert_eq!(
        answer["sessions"],
        Value::Array(Vec::new()),
        "a fresh database has nothing to offer"
    );
}

async fn ask_resume(state: &Arc<SessionsState>, req: &str, id: &str) -> Value {
    let (out, mut rx) = crate::sessions::outbound::channel();
    handle_session_resume(state, &out, req.to_string(), id.to_string());
    let reply = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("an answer arrives")
        .expect("the channel stays open");
    serde_json::from_str(&reply).expect("frames are JSON")
}

#[tokio::test]
async fn session_resume_of_a_live_session_answers_spawned_and_relaunches_nothing() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let (_live, _guard) = KillGuard::spawn(
        &hub,
        &SpawnRequest {
            id: Some("live-1".to_string()),
            ..agent_spawn("cat", "/tmp")
        },
    );
    let state = state_with(hub.clone());
    assert_eq!(hub.list().len(), 1);

    let answer = ask_resume(&state, "r1", "live-1").await;

    assert_eq!(answer["t"], "spawned", "got {answer}");
    assert_eq!(answer["req"], "r1");
    assert_eq!(answer["id"], "live-1");
    assert_eq!(
        hub.list().len(),
        1,
        "the live session must not be relaunched"
    );
}

#[tokio::test]
async fn session_resume_of_an_unknown_id_answers_not_found() {
    let (db, _dir) = test_db();
    let state = state_with(test_hub(&db));

    let answer = ask_resume(&state, "r2", "no-such-session").await;

    assert_eq!(answer["t"], "error", "got {answer}");
    assert_eq!(answer["code"], ERR_NOT_FOUND, "got {answer}");
    assert_eq!(answer["req"], "r2");
}

#[test]
fn resume_recorded_session_keeps_the_row_id_and_leaves_it_active() {
    let (db, _dir) = test_db();
    let (_config_dir, config) = config_with("[[clis]]\nname = \"cat\"\n");
    let workdir = tempfile::tempdir().expect("workdir");
    let workdir = workdir.path().to_string_lossy().to_string();
    db.insert_interactive_session(
        "keep-me",
        "kept-name",
        "cat",
        &workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.mark_session_closed("keep-me").unwrap();
    let row = db
        .get_interactive_session("keep-me")
        .unwrap()
        .expect("the finished row exists");
    let hub = test_hub(&db);

    resume_recorded_session(&hub, &row, &config, None).expect("the resume starts a process");

    let resumed = hub.get("keep-me").expect("the session came back");
    assert_eq!(resumed.name, "kept-name", "the row keeps its name");
    assert_eq!(
        db.get_interactive_session_status("keep-me")
            .unwrap()
            .as_deref(),
        Some("active"),
        "the resumed row is active again"
    );
    assert_eq!(
        db.get_interactive_session_args("keep-me").unwrap(),
        None,
        "the recorded launch args are never rewritten with the resume command"
    );
    let _ = hub.kill("keep-me");
}
