//! `dir_list`: subdirectory listing for the remote New dialog's folder
//! picker — contents, ordering, symlinks, the 500 cap, `~` and relative
//! path rules, error codes, and the owner-only gate on the device bridge.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::*;
use crate::remote::bridge::RemoteBridge;
use crate::remote::trust::Grant;
use crate::sessions::dir_list::handle_dir_list;
use crate::sessions::protocol::{parse_client_frame, ClientFrame};
use crate::sessions::ws::SessionsState;

fn state() -> (Arc<SessionsState>, tempfile::TempDir) {
    let (db, dir) = test_db();
    (
        SessionsState::for_test(test_hub(&db), "token".to_string()),
        dir,
    )
}

/// Ask `handle_dir_list` for `path` and return the parsed answer frame.
async fn ask(state: &Arc<SessionsState>, req: &str, path: Option<&str>) -> Value {
    let (out, mut rx) = crate::sessions::outbound::channel();
    handle_dir_list(state, &out, req.to_string(), path.map(str::to_string));
    let reply = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("an answer arrives")
        .expect("the channel stays open");
    serde_json::from_str(&reply).expect("frames are JSON")
}

/// The `name` of every `dirs` entry, in answer order.
fn names(answer: &Value) -> Vec<String> {
    answer["dirs"]
        .as_array()
        .expect("dirs is an array")
        .iter()
        .map(|entry| entry["name"].as_str().expect("name string").to_string())
        .collect()
}

#[tokio::test]
async fn dir_list_answers_subdirectories_only() {
    let (state, _db_dir) = state();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("Bravo")).unwrap();
    std::fs::create_dir(dir.path().join("alpha")).unwrap();
    std::fs::create_dir(dir.path().join(".hidden")).unwrap();
    std::fs::write(dir.path().join("notes.txt"), "not a dir").unwrap();

    let answer = ask(&state, "d1", Some(&dir.path().to_string_lossy())).await;

    assert_eq!(answer["t"], "dir_list", "got {answer}");
    assert_eq!(answer["req"], "d1");
    assert_eq!(
        answer["path"],
        std::fs::canonicalize(dir.path())
            .unwrap()
            .to_string_lossy()
            .as_ref(),
        "the answer reports the canonical path"
    );
    assert_eq!(
        answer["parent"],
        std::fs::canonicalize(dir.path())
            .unwrap()
            .parent()
            .unwrap()
            .to_string_lossy()
            .as_ref(),
        "the parent is the canonical path's parent"
    );
    assert_eq!(answer["truncated"], false);
    assert_eq!(names(&answer), vec![".hidden", "alpha", "Bravo"]);
    assert!(
        !names(&answer).contains(&"notes.txt".to_string()),
        "files never appear: {answer}"
    );
}

#[tokio::test]
async fn dir_list_sorts_case_insensitively() {
    let (state, _db_dir) = state();
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["zeta", "Alpha", "beta"] {
        std::fs::create_dir(dir.path().join(name)).unwrap();
    }

    let answer = ask(&state, "d2", Some(&dir.path().to_string_lossy())).await;

    assert_eq!(names(&answer), vec!["Alpha", "beta", "zeta"]);
}

#[cfg(unix)]
#[tokio::test]
async fn dir_list_includes_a_symlink_to_a_directory() {
    let (state, _db_dir) = state();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("real")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();

    let answer = ask(&state, "d3", Some(&dir.path().to_string_lossy())).await;

    assert_eq!(names(&answer), vec!["link", "real"]);
}

#[cfg(unix)]
#[tokio::test]
async fn dir_list_skips_a_dangling_symlink() {
    let (state, _db_dir) = state();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("real")).unwrap();
    std::os::unix::fs::symlink(
        dir.path().join("missing-target"),
        dir.path().join("dangling"),
    )
    .unwrap();

    let answer = ask(&state, "d4", Some(&dir.path().to_string_lossy())).await;

    assert_eq!(names(&answer), vec!["real"]);
}

#[tokio::test]
async fn dir_list_caps_at_500_and_reports_truncated() {
    let (state, _db_dir) = state();
    let dir = tempfile::tempdir().expect("tempdir");
    for index in 0..505 {
        std::fs::create_dir(dir.path().join(format!("d{index:04}"))).unwrap();
    }

    let answer = ask(&state, "d5", Some(&dir.path().to_string_lossy())).await;

    assert_eq!(answer["truncated"], true, "got {answer}");
    assert_eq!(names(&answer).len(), 500);
}

#[tokio::test]
async fn dir_list_parent_is_null_at_the_root() {
    let (state, _db_dir) = state();

    let answer = ask(&state, "d6", Some("/")).await;

    assert_eq!(answer["t"], "dir_list", "got {answer}");
    assert_eq!(answer["path"], "/");
    assert!(answer["parent"].is_null(), "got {answer}");
}

#[tokio::test]
async fn dir_list_expands_tilde_and_empty_path_to_home() {
    let (state, _db_dir) = state();
    let home = dirs::home_dir().expect("home directory");
    let canonical_home = std::fs::canonicalize(&home).expect("canonical home");

    for path in ["~", "~/", ""] {
        let answer = ask(&state, "d7", Some(path)).await;
        assert_eq!(answer["t"], "dir_list", "path {path:?}: got {answer}");
        assert_eq!(
            answer["path"],
            canonical_home.to_string_lossy().as_ref(),
            "path {path:?} must resolve to the home directory"
        );
    }
    let absent = ask(&state, "d7", None).await;
    assert_eq!(
        absent["path"],
        canonical_home.to_string_lossy().as_ref(),
        "an absent path means the home directory"
    );
}

#[tokio::test]
async fn dir_list_refuses_a_relative_path() {
    let (state, _db_dir) = state();

    for path in ["some/relative", "~user"] {
        let answer = ask(&state, "d8", Some(path)).await;
        assert_eq!(answer["t"], "error", "path {path:?}: got {answer}");
        assert_eq!(answer["code"], "bad_request", "got {answer}");
        assert_eq!(answer["req"], "d8");
        assert!(
            answer["message"]
                .as_str()
                .expect("message string")
                .contains("must be absolute or ~-prefixed"),
            "got {answer}"
        );
    }
}

#[tokio::test]
async fn dir_list_missing_path_answers_not_found() {
    let (state, _db_dir) = state();
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("no-such-dir");

    let answer = ask(&state, "d9", Some(&missing.to_string_lossy())).await;

    assert_eq!(answer["t"], "error", "got {answer}");
    assert_eq!(answer["code"], "not_found", "got {answer}");
    assert_eq!(answer["req"], "d9");
}

#[tokio::test]
async fn dir_list_a_file_answers_not_found() {
    let (state, _db_dir) = state();
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("plain-file");
    std::fs::write(&file, "not a directory").unwrap();

    let answer = ask(&state, "d10", Some(&file.to_string_lossy())).await;

    assert_eq!(answer["t"], "error", "got {answer}");
    assert_eq!(answer["code"], "not_found", "got {answer}");
    assert_eq!(answer["req"], "d10");
}

#[test]
fn dir_list_frames_parse_their_fields() {
    assert!(matches!(
        parse_client_frame(r#"{"t": "dir_list", "req": "r1"}"#),
        Ok(ClientFrame::DirList { req, path }) if req == "r1" && path.is_none()
    ));
    assert!(matches!(
        parse_client_frame(r#"{"t": "dir_list", "req": "r1", "path": null}"#),
        Ok(ClientFrame::DirList { path, .. }) if path.is_none()
    ));
    assert!(matches!(
        parse_client_frame(r#"{"t": "dir_list", "req": "r1", "path": "/tmp"}"#),
        Ok(ClientFrame::DirList { path, .. }) if path.as_deref() == Some("/tmp")
    ));

    let missing_req = parse_client_frame(r#"{"t": "dir_list", "path": "/tmp"}"#)
        .expect_err("a missing req must be refused");
    assert!(missing_req.message.contains("missing field 'req'"));
    let bad_path = parse_client_frame(r#"{"t": "dir_list", "req": "r1", "path": 42}"#)
        .expect_err("a non-string path must be refused");
    assert!(bad_path.message.contains("field 'path' must be a string"));
}

#[tokio::test]
async fn dir_list_is_forbidden_to_a_non_owner_remote_peer() {
    let (state, _db_dir) = state();
    let (out, mut rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(
        state,
        Grant::Sessions(HashSet::from(["alpha".to_string()])),
        false,
    );

    bridge
        .handle_text(
            &json!({"t": "dir_list", "req": "d1", "path": "/tmp"}).to_string(),
            &out,
        )
        .await;
    let reply = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("an answer arrives")
        .expect("the channel stays open");
    let answer: Value = serde_json::from_str(&reply).expect("frames are JSON");

    assert_eq!(answer["t"], "error", "got {answer}");
    assert_eq!(answer["code"], "forbidden", "got {answer}");
    assert_eq!(answer["req"], "d1");
    assert_eq!(
        answer["message"],
        "dir_list is only available to the host owner's devices"
    );
}

#[tokio::test]
async fn dir_list_answers_the_owner_on_the_remote_bridge() {
    let (state, _db_dir) = state();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let (out, mut rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(state, Grant::All, false);
    bridge.set_owner(true);

    bridge
        .handle_text(
            &json!({
                "t": "dir_list", "req": "d2",
                "path": dir.path().to_string_lossy(),
            })
            .to_string(),
            &out,
        )
        .await;
    let reply = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("an answer arrives")
        .expect("the channel stays open");
    let answer: Value = serde_json::from_str(&reply).expect("frames are JSON");

    assert_eq!(answer["t"], "dir_list", "got {answer}");
    assert_eq!(answer["req"], "d2");
    assert_eq!(names(&answer), vec!["sub"]);
}
