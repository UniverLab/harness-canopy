//! Session shelf tests: content sniffing, the 20 MiB limit, the atomic
//! write + orphan sweep, the 7-day retention, the 1 GiB cap's pruning
//! order, and the shelf protocol frames (`shelf_list` / `shelf_get` parts /
//! `shelf_delete` + the `shelf_added`/`shelf_removed` broadcasts).

use std::sync::Arc;

use chrono::{Duration, Utc};

use super::{connect_client, decode_b64, recv_json, send_json, serve_state, test_db, test_hub};
use crate::sessions::shelf::{
    is_file_id, shelf_file_path, ShelfError, ShelfStore, ERR_NOT_A_FILE, ERR_NOT_FOUND,
    ERR_TOO_LARGE, ERR_UNSUPPORTED_TYPE, MAX_FILE_BYTES, READ_CHUNK_BYTES,
};
use crate::sessions::ws::SessionsState;

// ── fixtures ──────────────────────────────────────────────────────────

/// A file that sniffs as PNG: the 8-byte magic plus arbitrary tail (the
/// sniffer only reads the head).
fn png_bytes(len: usize) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.resize(len.max(8), 0x42);
    bytes
}

fn scratch_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("scratch dir")
}

/// Write `bytes` to `dir/name` and return the path.
fn write_src(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).expect("write source");
    path
}

fn store(db: &Arc<crate::db::Database>) -> Arc<ShelfStore> {
    ShelfStore::for_test(db)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn code(err: &ShelfError) -> &'static str {
    err.code
}

// ── sniffing (FR8) ────────────────────────────────────────────────────

#[test]
fn sniff_accepts_each_allowed_type() {
    use crate::sessions::shelf::sniff_mime;
    let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    let gif = b"GIF89a\x01\x00\x01\x00\x00".to_vec();
    let mut webp = b"RIFF\x24\x00\x00\x00WEBPVP8 ".to_vec();
    webp.resize(24, 0);
    let svg = b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";

    for (bytes, want) in [
        (png_bytes(64), "image/png"),
        (jpeg.to_vec(), "image/jpeg"),
        (gif, "image/gif"),
        (webp, "image/webp"),
        (svg.to_vec(), "image/svg+xml"),
    ] {
        let got = sniff_mime(&bytes, &bytes).expect("allowed type sniffed");
        assert_eq!(got, want);
    }
}

#[test]
fn png_renamed_txt_is_accepted_as_png() {
    let (db, _dir) = test_db();
    let store = store(&db);
    let src = scratch_dir();
    let path = write_src(&src, "screenshot.txt", &png_bytes(64));
    let item = store
        .insert(&path, None, None, "host")
        .expect("extension must not matter");
    assert_eq!(item.mime, "image/png");
    assert_eq!(item.name, "screenshot.txt");
}

#[test]
fn text_file_renamed_png_is_rejected() {
    let (db, _dir) = test_db();
    let store = store(&db);
    let src = scratch_dir();
    let path = write_src(&src, "fake.png", b"just some text, not an image");
    let err = store.insert(&path, None, None, "host").unwrap_err();
    assert_eq!(code(&err), ERR_UNSUPPORTED_TYPE);
    assert!(
        store.list(None).is_empty(),
        "a rejected file must leave no row"
    );
}

#[test]
fn svg_with_xml_prolog_is_accepted_and_html_is_rejected() {
    let (db, _dir) = test_db();
    let store = store(&db);
    let src = scratch_dir();

    let svg = write_src(
        &src,
        "diagram.svg",
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- drawn by an agent -->\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\"></svg>",
    );
    let item = store.insert(&svg, None, None, "host").expect("prolog svg");
    assert_eq!(item.mime, "image/svg+xml");

    // HTML wearing an .svg extension: root element decides, not the name.
    let html = write_src(
        &src,
        "page.svg",
        b"<!DOCTYPE html>\n<html><body><svg width=\"1\"></svg></body></html>",
    );
    let err = store.insert(&html, None, None, "host").unwrap_err();
    assert_eq!(code(&err), ERR_UNSUPPORTED_TYPE);
}

// ── limits and error codes (FR2) ──────────────────────────────────────

#[test]
fn twenty_mib_boundary_is_inclusive() {
    let (db, _dir) = test_db();
    let store = store(&db);
    let src = scratch_dir();

    let exact = write_src(&src, "exact.png", &png_bytes(MAX_FILE_BYTES as usize));
    store
        .insert(&exact, None, None, "host")
        .expect("exactly 20 MiB is accepted");

    let over = write_src(&src, "over.png", &png_bytes(MAX_FILE_BYTES as usize + 1));
    let err = store.insert(&over, None, None, "host").unwrap_err();
    assert_eq!(code(&err), ERR_TOO_LARGE);
    assert_eq!(store.list(None).len(), 1, "only the exact file");
}

#[test]
fn missing_and_non_file_paths_report_their_codes() {
    let (db, _dir) = test_db();
    let store = store(&db);
    let src = scratch_dir();

    let missing = src.path().join("nope.png");
    let err = store.insert(&missing, None, None, "host").unwrap_err();
    assert_eq!(code(&err), ERR_NOT_FOUND);

    let err = store.insert(src.path(), None, None, "host").unwrap_err();
    assert_eq!(code(&err), ERR_NOT_A_FILE);
}

#[test]
fn shelf_ids_never_leave_the_shelf_directory() {
    assert!(!is_file_id("../../etc/passwd"));
    assert!(!is_file_id("DEADBEEFDEADBEEF"));
    assert!(shelf_file_path("../../etc/passwd").is_none());
    assert!(is_file_id("deadbeefdeadbeef"));
    let path = shelf_file_path("deadbeefdeadbeef").expect("valid id");
    assert!(path.ends_with("shelf/deadbeefdeadbeef"));
}

// ── atomic write + orphan cleanup (FR1, FR8) ──────────────────────────

#[test]
fn insert_leaves_row_and_file_and_no_temp() {
    let (db, _dir) = test_db();
    let store = store(&db);
    let src = scratch_dir();
    let path = write_src(&src, "shot.png", &png_bytes(2048));

    let item = store
        .insert(&path, None, Some("a caption"), "host")
        .unwrap();

    let row = store.get(&item.file_id).expect("row inserted");
    assert_eq!(row.caption.as_deref(), Some("a caption"));
    assert_eq!(row.size, 2048);
    assert_eq!(row.sha256, sha256_hex(&png_bytes(2048)));
    let file = store.dir().join(&item.file_id);
    assert!(file.is_file(), "bytes must be renamed into place");
    let entries: Vec<String> = std::fs::read_dir(store.dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, [item.file_id], "no temp file remains");
}

#[test]
fn startup_maintenance_sweeps_orphans_both_ways() {
    let (db, _dir) = test_db();
    let store = store(&db);
    let src = scratch_dir();
    let path = write_src(&src, "shot.png", &png_bytes(64));
    let item = store.insert(&path, None, None, "host").unwrap();

    // Crash window 1: row without file (died between insert and rename).
    std::fs::remove_file(store.dir().join(&item.file_id)).unwrap();
    store.maintenance();
    assert!(store.get(&item.file_id).is_none(), "fileless row swept");

    // Crash window 2: file without row (died after write, before insert),
    // plus a stale temp name.
    let stray = store.dir().join("0123456789abcdef");
    std::fs::write(&stray, b"orphan bytes").unwrap();
    let stale_tmp = store.dir().join(".0123456789abcdef.tmp");
    std::fs::write(&stale_tmp, b"half write").unwrap();
    store.maintenance();
    assert!(!stray.exists(), "rowless file swept");
    assert!(!stale_tmp.exists(), "stale temp swept");
}

#[test]
fn retention_keeps_files_for_seven_days_after_the_session_is_gone() {
    let (db, _dir) = test_db();
    let store = store(&db);
    let src = scratch_dir();
    let aged = |days: i64| (Utc::now() - Duration::days(days)).to_rfc3339();

    // A live session's files are kept no matter how old.
    db.insert_interactive_session(
        "sess-live",
        "live",
        "bash",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    let live = store
        .insert(
            &write_src(&src, "live.png", &png_bytes(32)),
            Some("sess-live"),
            None,
            "live",
        )
        .unwrap();
    db.set_shelf_created_at(&live.file_id, &aged(30)).unwrap();

    // A removed session's files stay until the 7-day mark…
    let young = store
        .insert(
            &write_src(&src, "young.png", &png_bytes(32)),
            Some("sess-gone"),
            None,
            "gone",
        )
        .unwrap();
    db.set_shelf_created_at(&young.file_id, &aged(6)).unwrap();

    // …and are reaped after it.
    let old = store
        .insert(
            &write_src(&src, "old.png", &png_bytes(32)),
            Some("sess-gone"),
            None,
            "gone",
        )
        .unwrap();
    db.set_shelf_created_at(&old.file_id, &aged(8)).unwrap();

    store.maintenance();

    assert!(
        store.get(&live.file_id).is_some(),
        "live session keeps its files"
    );
    assert!(store.get(&young.file_id).is_some(), "under 7 days is kept");
    assert!(store.get(&old.file_id).is_none(), "over 7 days is reaped");
    assert!(
        !store.dir().join(&old.file_id).exists(),
        "the reaped bytes go too"
    );
}

// ── the 1 GiB cap, injected small (FR4/FR8) ───────────────────────────

#[test]
fn cap_prunes_oldest_files_first() {
    let (db, _dir) = test_db();
    let store = store(&db).with_cap_bytes(1_000);
    let src = scratch_dir();

    let first = store
        .insert(
            &write_src(&src, "a.png", &png_bytes(300)),
            None,
            None,
            "host",
        )
        .unwrap();
    let second = store
        .insert(
            &write_src(&src, "b.png", &png_bytes(400)),
            None,
            None,
            "host",
        )
        .unwrap();
    // 1100 bytes total > 1000 cap → the oldest (first) is pruned on insert.
    let third = store
        .insert(
            &write_src(&src, "c.png", &png_bytes(400)),
            None,
            None,
            "host",
        )
        .unwrap();

    let items = store.list(None);
    let ids: Vec<&str> = items.iter().map(|i| i.file_id.as_str()).collect();
    assert_eq!(ids, vec![third.file_id.as_str(), second.file_id.as_str()]);
    assert!(store.get(&first.file_id).is_none(), "oldest pruned first");
    assert!(!store.dir().join(&first.file_id).exists());
    let total: i64 = items.iter().map(|i| i.size).sum();
    assert!(total <= 1_000, "shelf fits the cap: {total}");
}

// ── protocol frames (FR3/FR8) ─────────────────────────────────────────

/// Seed `count` PNG files on the store's host shelf and return their ids,
/// newest first.
fn seed(store: &ShelfStore, count: usize) -> Vec<String> {
    let src = scratch_dir();
    (0..count)
        .map(|i| {
            let path = write_src(&src, &format!("f{i}.png"), &png_bytes(100 + i));
            store.insert(&path, None, None, "host").unwrap().file_id
        })
        .collect()
}

#[tokio::test]
async fn shelf_list_answers_newest_first_and_hello_advertises_shelf() {
    let (db, _dir) = test_db();
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let ids = seed(&state.shelf, 3);
    let (addr, server) = serve_state(state.clone()).await;
    let mut client = connect_client(addr, Some("token")).await;

    let hello = recv_json(&mut client, "hello").await;
    assert_eq!(hello["t"], "hello");
    assert!(
        hello["features"]
            .as_array()
            .is_some_and(|f| f.iter().any(|v| v == "shelf")),
        "hello must advertise the shelf feature: {hello}"
    );

    send_json(
        &mut client,
        &serde_json::json!({"t": "shelf_list", "req": "l1"}),
    )
    .await;
    let shelf = recv_json(&mut client, "shelf").await;
    assert_eq!(shelf["t"], "shelf");
    assert_eq!(shelf["req"], "l1");
    assert!(shelf["id"].is_null(), "absent id = host shelf");
    let listed: Vec<&str> = shelf["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["file_id"].as_str().expect("file_id"))
        .collect();
    // `ids` is insertion order; the listing is newest first.
    assert_eq!(
        listed,
        vec![ids[2].as_str(), ids[1].as_str(), ids[0].as_str()]
    );
    let first = &shelf["items"][0];
    assert_eq!(first["from"], "host");
    assert!(first["size"].is_i64());
    server.abort();
}

#[tokio::test]
async fn shelf_get_streams_12kib_parts_that_reassemble_to_the_same_sha256() {
    let (db, _dir) = test_db();
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let src = scratch_dir();
    let bytes = png_bytes(40_000);
    let item = state
        .shelf
        .insert(&write_src(&src, "big.png", &bytes), None, None, "host")
        .unwrap();
    let (addr, server) = serve_state(state.clone()).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({"t": "shelf_get", "req": "g1", "file_id": item.file_id}),
    )
    .await;

    let mut reassembled = Vec::new();
    let mut parts = 0u32;
    loop {
        let frame = recv_json(&mut client, "file part").await;
        assert_eq!(frame["t"], "file");
        assert_eq!(frame["req"], "g1");
        assert_eq!(frame["file_id"], item.file_id);
        assert_eq!(
            frame["part"].as_u64().expect("part index"),
            u64::from(parts)
        );
        let chunk = decode_b64(frame["data"].as_str().expect("b64 data"));
        assert!(
            chunk.len() <= READ_CHUNK_BYTES,
            "part {} is {} bytes (> {READ_CHUNK_BYTES} raw)",
            parts,
            chunk.len()
        );
        reassembled.extend(chunk);
        parts += 1;
        if frame["last"].as_bool().expect("last flag") {
            break;
        }
    }
    assert_eq!(reassembled, bytes, "parts reassemble byte-for-byte");
    let row = db.get_shelf_file(&item.file_id).unwrap().expect("row");
    assert_eq!(sha256_hex(&reassembled), row.sha256);
    assert!(
        parts >= 4,
        "40000 bytes must split into several parts: {parts}"
    );
    server.abort();
}

#[tokio::test]
async fn shelf_get_unknown_id_answers_not_found() {
    let (db, _dir) = test_db();
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({"t": "shelf_get", "req": "g2", "file_id": "ffffffffffffffff"}),
    )
    .await;
    let frame = recv_json(&mut client, "error").await;
    assert_eq!(frame["t"], "error");
    assert_eq!(frame["req"], "g2");
    assert_eq!(frame["code"], "not_found");
    server.abort();
}

#[tokio::test]
async fn insert_and_delete_broadcast_added_and_removed_to_a_subscriber() {
    let (db, _dir) = test_db();
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state.clone()).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    let src = scratch_dir();
    let item = state
        .shelf
        .insert(
            &write_src(&src, "toast.png", &png_bytes(64)),
            None,
            None,
            "host",
        )
        .unwrap();

    let added = recv_json(&mut client, "shelf_added").await;
    assert_eq!(added["t"], "shelf_added");
    assert_eq!(added["item"]["file_id"], item.file_id);
    assert_eq!(added["item"]["name"], "toast.png");

    // A client-initiated delete answers `ok` and broadcasts `shelf_removed`
    // (the two frames race; collect both like the kill roundtrip does).
    send_json(
        &mut client,
        &serde_json::json!({"t": "shelf_delete", "req": "d1", "file_id": item.file_id}),
    )
    .await;
    let mut ok = false;
    let mut removed = false;
    while !(ok && removed) {
        let frame = recv_json(&mut client, "delete frames").await;
        match frame["t"].as_str() {
            Some("ok") => {
                assert_eq!(frame["req"], "d1");
                ok = true;
            }
            Some("shelf_removed") => {
                assert_eq!(frame["file_id"], item.file_id);
                removed = true;
            }
            other => panic!("unexpected frame during shelf_delete: {other:?}"),
        }
    }
    assert!(state.shelf.get(&item.file_id).is_none(), "file is gone");
    server.abort();
}

#[test]
fn unknown_host_frames_parse_as_ignored_not_errors() {
    use crate::sessions::protocol::{parse_host_frame, HostFrame};
    let parsed = parse_host_frame(r#"{"t":"some_future_frame","payload":1}"#)
        .expect("an unknown host frame must parse");
    assert!(matches!(parsed, HostFrame::Unknown));
    assert!(
        parse_host_frame(r#"{"req":"x"}"#).is_err(),
        "still needs `t`"
    );
}

/// Kill-time reap (FR4): ending a hub session does not delete its files —
/// they stay until the 7-day mark, then go on the next maintenance pass,
/// which `kill` itself runs.
#[test]
fn killing_a_session_keeps_then_reaps_its_shelf_files() {
    use super::{terminal_spawn, KillGuard};

    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let store = store(&db);
    hub.wire_shelf(store.clone());
    let src = scratch_dir();
    let aged = |days: i64| (Utc::now() - Duration::days(days)).to_rfc3339();

    let workdir = scratch_dir();
    let (session, guard) = KillGuard::spawn(
        &hub,
        &terminal_spawn("cat", workdir.path().to_string_lossy().as_ref()),
    );
    let old = store
        .insert(
            &write_src(&src, "old.png", &png_bytes(64)),
            Some(&session.id),
            None,
            "doomed",
        )
        .unwrap();
    db.set_shelf_created_at(&old.file_id, &aged(8)).unwrap();
    let young = store
        .insert(
            &write_src(&src, "young.png", &png_bytes(64)),
            Some(&session.id),
            None,
            "doomed",
        )
        .unwrap();
    db.set_shelf_created_at(&young.file_id, &aged(6)).unwrap();

    hub.kill(&session.id);
    drop(guard); // second kill is a no-op

    assert!(
        store.get(&old.file_id).is_none(),
        "past the 7-day mark, kill's reap removes the file"
    );
    assert!(!store.dir().join(&old.file_id).exists(), "and its bytes");
    assert!(
        store.get(&young.file_id).is_some(),
        "under 7 days the files stay after the session is removed"
    );
}
