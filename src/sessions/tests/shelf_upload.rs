//! `shelf_put_*` protocol tests: the begin/part/end round-trip, its refusal
//! order, checksum verification, name sanitization, orphan temps and the
//! advertised feature/protocol version.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{test_db, test_hub};
use crate::remote::bridge::RemoteBridge;
use crate::sessions::outbound::{self, ConnRx};
use crate::sessions::protocol::{
    parse_client_frame, shelf_put_begin_frame, shelf_put_end_frame, shelf_put_part_frame,
    ClientFrame, ERR_CHECKSUM_MISMATCH, ERR_TOO_LARGE, PROTO_VERSION,
};
use crate::sessions::shelf::ShelfStore;
use crate::sessions::ws::SessionsState;
use crate::sessions::HubEvent;

fn state() -> (Arc<SessionsState>, tempfile::TempDir) {
    let (db, dir) = test_db();
    (
        SessionsState::for_test(test_hub(&db), "token".to_string()),
        dir,
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn json(frame: &str) -> Value {
    serde_json::from_str(frame).expect("frame is JSON")
}

/// Parse the frame exactly as a client would, then let the bridge handle it —
/// the pair the contract names.
async fn apply(bridge: &mut RemoteBridge, frame: &str, out: &outbound::ConnTx) {
    parse_client_frame(frame).expect("client frame parses");
    bridge.handle_text(frame, out).await;
}

async fn recv(rx: &mut ConnRx, what: &str) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .expect("channel open");
    json(&frame)
}

/// The begin/reply pair: returns the reserved file id and the parsed answer,
/// panicking on an error frame.
async fn begin_ok(
    bridge: &mut RemoteBridge,
    out: &outbound::ConnTx,
    rx: &mut ConnRx,
    id: Option<&str>,
    name: &str,
    bytes: &[u8],
) -> (String, Value) {
    let frame = shelf_put_begin_frame("b1", id, name, bytes.len() as u64, None, &sha256_hex(bytes));
    apply(bridge, &frame, out).await;
    let ready = recv(rx, "shelf_put_ready").await;
    assert_eq!(ready["t"], "shelf_put_ready", "got {ready}");
    assert_eq!(ready["req"], "b1");
    let file_id = ready["file_id"].as_str().expect("a file id").to_string();
    (file_id, ready)
}

async fn end(bridge: &mut RemoteBridge, out: &outbound::ConnTx, file_id: &str) {
    apply(bridge, &shelf_put_end_frame(file_id), out).await;
}

#[tokio::test]
async fn begin_part_end_round_trip_commits_bytes_mode_and_shelf_added() {
    let (state, _dir) = state();
    let (out, mut rx) = outbound::channel();
    let mut events = state.hub.subscribe_events();
    let mut bridge = RemoteBridge::new(Arc::clone(&state));
    let bytes = b"# hello shelf\n".to_vec();

    let (file_id, _) = begin_ok(&mut bridge, &out, &mut rx, None, "notes.md", &bytes).await;
    let part = shelf_put_part_frame(&file_id, 0, &bytes);
    apply(&mut bridge, &part, &out).await;
    assert!(
        rx.try_recv().is_err(),
        "a lone small part is not acknowledged"
    );
    end(&mut bridge, &out, &file_id).await;

    let added = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("an event arrives")
        .expect("bus open");
    match added {
        HubEvent::ShelfAdded(item) => {
            assert_eq!(item.file_id, file_id);
            assert_eq!(item.name, "notes.md");
            assert_eq!(item.size, bytes.len() as i64);
            assert_eq!(item.mime, "application/octet-stream");
            assert_eq!(item.from_label, "user");
        }
        other => panic!("expected shelf_added, got {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "end answers no frame");

    let row = state.shelf.get(&file_id).expect("row committed");
    assert_eq!(row.origin, "user");
    assert_eq!(state.shelf.read_file(&file_id).unwrap(), bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(state.shelf.path_for(&file_id).unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the committed file is 0600");
    }
}

#[tokio::test]
async fn every_eighth_part_is_acknowledged() {
    let (state, _dir) = state();
    let (out, mut rx) = outbound::channel();
    let mut bridge = RemoteBridge::new(Arc::clone(&state));
    let bytes = vec![7u8; 9];
    let (file_id, _) = begin_ok(&mut bridge, &out, &mut rx, None, "nine.bin", &bytes).await;

    for part in 0..9u64 {
        let frame = shelf_put_part_frame(&file_id, part, &[bytes[part as usize]]);
        apply(&mut bridge, &frame, &out).await;
        if part == 7 {
            let ack = recv(&mut rx, "shelf_put_ack").await;
            assert_eq!(ack["t"], "shelf_put_ack");
            assert_eq!(ack["file_id"], file_id);
            assert_eq!(ack["part"], 7);
        } else {
            assert!(rx.try_recv().is_err(), "part {part} is not acknowledged");
        }
    }
    end(&mut bridge, &out, &file_id).await;
    assert_eq!(state.shelf.read_file(&file_id).unwrap(), bytes);
}

#[tokio::test]
async fn too_large_is_refused_before_any_data() {
    let (state, _dir) = state();
    state.shelf.set_max_file_bytes(4);
    let (out, mut rx) = outbound::channel();
    let mut bridge = RemoteBridge::new(Arc::clone(&state));

    let frame = shelf_put_begin_frame("big", None, "big.bin", 5, None, &sha256_hex(b"12345"));
    apply(&mut bridge, &frame, &out).await;
    let error = recv(&mut rx, "too_large").await;
    assert_eq!(error["t"], "error");
    assert_eq!(error["req"], "big");
    assert_eq!(error["code"], ERR_TOO_LARGE);
    assert!(
        temp_uploads(&state.shelf).is_empty(),
        "nothing is reserved before the refusal"
    );
}

#[tokio::test]
async fn checksum_mismatch_deletes_the_temp() {
    let (state, _dir) = state();
    let (out, mut rx) = outbound::channel();
    let mut bridge = RemoteBridge::new(Arc::clone(&state));
    let declared = b"the expected bytes";
    let (file_id, _) = begin_ok(&mut bridge, &out, &mut rx, None, "x.bin", declared).await;

    apply(
        &mut bridge,
        &shelf_put_part_frame(&file_id, 0, b"the wrong bytes!!!"),
        &out,
    )
    .await;
    end(&mut bridge, &out, &file_id).await;
    let error = recv(&mut rx, "checksum_mismatch").await;
    assert_eq!(error["code"], ERR_CHECKSUM_MISMATCH);
    assert!(state.shelf.get(&file_id).is_none(), "no row is written");
    assert!(
        !state.shelf.path_for(&file_id).unwrap().exists(),
        "no final file"
    );
    assert!(temp_uploads(&state.shelf).is_empty(), "the temp is gone");
}

#[tokio::test]
async fn traversal_name_is_stripped_and_lands_under_the_file_id() {
    let (state, _dir) = state();
    let (out, mut rx) = outbound::channel();
    let mut bridge = RemoteBridge::new(Arc::clone(&state));
    let bytes = b"passwd-like content";
    let (file_id, _) = begin_ok(&mut bridge, &out, &mut rx, None, "../../etc/passwd", bytes).await;
    apply(&mut bridge, &shelf_put_part_frame(&file_id, 0, bytes), &out).await;
    end(&mut bridge, &out, &file_id).await;

    let row = state.shelf.get(&file_id).expect("row committed");
    assert_eq!(row.name, "passwd", "directories are stripped");
    let path = state.shelf.path_for(&file_id).unwrap();
    assert!(path.is_file(), "the bytes live under the shelf's file id");
    assert!(
        path.starts_with(
            state
                .shelf
                .path_for("ffffffffffffffff")
                .unwrap()
                .parent()
                .unwrap()
        ),
        "and never outside the shelf directory"
    );
}

#[tokio::test]
async fn orphan_upload_temps_are_swept_only_after_a_day() {
    let (state, _dir) = state();
    let dir = state.shelf.path_for("0000000000000000").unwrap();
    let dir = dir.parent().unwrap().to_path_buf();

    let old = dir.join(".aaaaaaaaaaaaaaaa.upload");
    std::fs::write(&old, b"old").unwrap();
    let file = std::fs::File::options().write(true).open(&old).unwrap();
    file.set_modified(std::time::SystemTime::now() - Duration::from_secs(25 * 3600))
        .unwrap();

    let fresh = dir.join(".bbbbbbbbbbbbbbbb.upload");
    std::fs::write(&fresh, b"fresh").unwrap();

    // An in-map upload survives even when its temp is older than a day —
    // the temp is aged so the test pins the in-flight exemption, not the
    // file's youth.
    let in_map = state
        .shelf
        .begin_upload(None, "live.bin", 1, None, &sha256_hex(b"x"))
        .unwrap();
    let in_map_temp = dir.join(format!(".{in_map}.upload"));
    let file = std::fs::File::options()
        .write(true)
        .open(&in_map_temp)
        .unwrap();
    file.set_modified(std::time::SystemTime::now() - Duration::from_secs(25 * 3600))
        .unwrap();

    state.shelf.maintenance();
    assert!(!old.exists(), "a day-old orphan is swept");
    assert!(fresh.exists(), "a fresh orphan is kept");
    assert!(
        in_map_temp.exists(),
        "an in-flight upload's temp survives even when old"
    );
}

#[test]
fn abort_with_a_crafted_id_deletes_nothing_outside_the_shelf() {
    let (state, _dir) = state();
    let dir = state.shelf.dir().to_path_buf();
    let parent = dir.parent().expect("the shelf has a parent").to_path_buf();
    let nonce = crate::sessions::shelf::new_file_id();
    let sentinel = parent.join(format!("sentinel-{nonce}.upload"));
    std::fs::write(&sentinel, b"keep me").unwrap();

    // Without the id gate this resolves to the sentinel and deletes it.
    state.shelf.abort_upload(&format!("../sentinel-{nonce}"));
    assert!(
        sentinel.exists(),
        "a crafted abort id must not escape the shelf"
    );
    let _ = std::fs::remove_file(&sentinel);

    // An unknown but well-formed id stays a silent no-op.
    state.shelf.abort_upload("ffffffffffffffff");
}

#[tokio::test]
async fn empty_string_id_lands_on_the_host_shelf() {
    let (state, _dir) = state();
    let (out, mut rx) = outbound::channel();
    let mut bridge = RemoteBridge::new(Arc::clone(&state));
    let bytes = b"host file";
    let (file_id, _) = begin_ok(&mut bridge, &out, &mut rx, Some(""), "h.txt", bytes).await;
    apply(&mut bridge, &shelf_put_part_frame(&file_id, 0, bytes), &out).await;
    end(&mut bridge, &out, &file_id).await;

    let row = state.shelf.get(&file_id).expect("row committed");
    assert_eq!(
        row.session_id, None,
        "an empty id means the host shelf, not a `\"\"` shelf"
    );
}

#[test]
fn empty_declared_mime_falls_back_to_octet_stream() {
    let (state, _dir) = state();
    let bytes = b"plain bytes, no magic";
    let file_id = state
        .shelf
        .begin_upload(
            None,
            "x.bin",
            bytes.len() as u64,
            Some(String::new()),
            &sha256_hex(bytes),
        )
        .unwrap();
    state.shelf.upload_part(&file_id, 0, bytes).unwrap();
    let item = state.shelf.upload_end(&file_id).unwrap();
    assert_eq!(item.mime, "application/octet-stream");
}

#[tokio::test]
async fn hello_advertises_shelf_upload_and_proto_is_two() {
    let hello = json(&crate::sessions::protocol::hello_frame("host", &[], false));
    assert_eq!(PROTO_VERSION, 2);
    assert_eq!(hello["proto"], 2);
    let features: Vec<&str> = hello["features"]
        .as_array()
        .expect("features")
        .iter()
        .map(|value| value.as_str().expect("a feature"))
        .collect();
    assert!(features.contains(&"shelf"), "{features:?}");
    assert!(features.contains(&"shelf_upload"), "{features:?}");
}

#[test]
fn put_frames_round_trip_through_the_client_parser() {
    let frame = shelf_put_begin_frame(
        "r1",
        None,
        "a.md",
        3,
        Some("text/markdown"),
        &"a".repeat(64),
    );
    match parse_client_frame(&frame).expect("parses") {
        ClientFrame::ShelfPutBegin {
            req,
            id,
            name,
            size,
            mime,
            sha256,
        } => {
            assert_eq!(req, "r1");
            assert_eq!(id, None);
            assert_eq!(name, "a.md");
            assert_eq!(size, 3);
            assert_eq!(mime.as_deref(), Some("text/markdown"));
            assert_eq!(sha256, "a".repeat(64));
        }
        other => panic!("expected ShelfPutBegin, got {other:?}"),
    }
    let data = b"abc";
    let part = shelf_put_part_frame("deadbeefdeadbeef", 2, data);
    match parse_client_frame(&part).expect("parses") {
        ClientFrame::ShelfPutPart {
            file_id,
            part,
            data: decoded,
        } => {
            assert_eq!(file_id, "deadbeefdeadbeef");
            assert_eq!(part, 2);
            assert_eq!(decoded, data);
        }
        other => panic!("expected ShelfPutPart, got {other:?}"),
    }
    assert!(matches!(
        parse_client_frame(&shelf_put_end_frame("deadbeefdeadbeef")).expect("parses"),
        ClientFrame::ShelfPutEnd { .. }
    ));
    assert!(matches!(
        parse_client_frame(&crate::sessions::protocol::shelf_put_abort_frame(
            "deadbeefdeadbeef"
        ))
        .expect("parses"),
        ClientFrame::ShelfPutAbort { .. }
    ));
}

/// A scripted protocol transcript — begin → parts → end → `shelf_added` —
/// against a scratch data dir, through `parse_client_frame` + the bridge.
/// Run with `--nocapture` to paste the transcript into a report.
#[tokio::test]
async fn scripted_protocol_transcript_begin_parts_end_shelf_added() {
    let (state, _dir) = state();
    let (out, mut rx) = outbound::channel();
    let mut events = state.hub.subscribe_events();
    let mut bridge = RemoteBridge::new(Arc::clone(&state));
    let bytes = b"transcript bytes";

    let begin = shelf_put_begin_frame(
        "t1",
        None,
        "note.txt",
        bytes.len() as u64,
        None,
        &sha256_hex(bytes),
    );
    println!("C→H {begin}");
    apply(&mut bridge, &begin, &out).await;
    let ready = recv(&mut rx, "shelf_put_ready").await;
    println!("H→C {ready}");
    let file_id = ready["file_id"].as_str().expect("a file id").to_string();

    for (part, chunk) in bytes.chunks(5).enumerate() {
        let frame = shelf_put_part_frame(&file_id, part as u64, chunk);
        println!("C→H {}", elide_data(&frame));
        apply(&mut bridge, &frame, &out).await;
    }
    let end_frame = shelf_put_end_frame(&file_id);
    println!("C→H {end_frame}");
    end(&mut bridge, &out, &file_id).await;

    let added = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("an event arrives")
        .expect("bus open");
    println!("H→* shelf_added {added:?}");
    assert!(matches!(added, HubEvent::ShelfAdded(_)));
    assert_eq!(state.shelf.read_file(&file_id).unwrap(), bytes);
}

/// The frame with its base64 `data` shortened, for transcript printing.
fn elide_data(frame: &str) -> Value {
    let mut value = json(frame);
    let elided = value
        .get("data")
        .and_then(Value::as_str)
        .map(|data| format!("{}…({} chars)", &data[..data.len().min(8)], data.len()));
    if let Some(elided) = elided {
        value["data"] = Value::String(elided);
    }
    value
}

/// Names of the `.upload` temp files currently in the shelf directory.
fn temp_uploads(shelf: &ShelfStore) -> Vec<String> {
    let dir = shelf.path_for("0000000000000000").unwrap();
    let dir = dir.parent().unwrap();
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".upload"))
        .collect()
}
