//! Part 20 of `endpoint_tests`: `session_send_file` (the session shelf).
use super::*;

/// A tiny file starting with the PNG magic the shelf sniffer checks — the
/// sniffer only needs the 8 magic bytes, not a valid image.
fn write_test_png(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join(format!(
        "shelf-{}.png",
        crate::sessions::shelf::new_file_id()
    ));
    std::fs::write(
        &path,
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0],
    )
    .expect("write test png");
    path
}

/// Point `$HOME` at the test dir, so `ensure_data_dir` and
/// `default_shelf_dir` stay inside temp state, and return the created shelf
/// dir. The returned guard restores `$HOME` on drop: keep it alive for the
/// whole test.
fn home_shelf_dir(dir: &tempfile::TempDir) -> (HomeVar, std::path::PathBuf) {
    let home = HomeVar::set(dir.path());
    let shelf = crate::sessions::shelf::default_shelf_dir();
    std::fs::create_dir_all(&shelf).expect("temp shelf dir");
    (home, shelf)
}

/// Write the PNG-magic fixture at an explicit path.
fn write_png_at(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::write(
        path,
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0],
    )
    .expect("write test png");
    path.to_path_buf()
}

/// Request parts carrying the canopy identity header, the way an HTTP
/// transport attaches the caller's session id.
fn parts_with_identity(agent_id: &str) -> axum::http::request::Parts {
    axum::http::Request::builder()
        .header(
            crate::shared::sync_identity::CANOPY_AGENT_ID_HEADER,
            agent_id,
        )
        .body(())
        .unwrap()
        .into_parts()
        .0
}

/// Call `session_send_file` directly, the way the endpoint tests call the
/// other `#[tool]` methods. Panics only when the handler itself errors —
/// a rejected *input* comes back as an `error_result` success channel
/// value, asserted with `is_err`.
async fn send_files(
    handler: &TaskTriggerHandler,
    paths: Vec<String>,
    caption: Option<String>,
    session_id: Option<String>,
    parts: Option<axum::http::request::Parts>,
) -> CallToolResult {
    handler
        .session_send_file(
            Parameters(SessionSendFileParams {
                paths,
                caption,
                session_id,
            }),
            OptionalExtension(parts),
        )
        .await
        .unwrap()
}

/// The JSON body of a successful result's first text block.
fn json_body(result: &CallToolResult) -> serde_json::Value {
    serde_json::from_str(&raw_text(result))
        .unwrap_or_else(|e| panic!("expected JSON body, got {:?}: {e}", raw_text(result)))
}

/// FR2 case 1: an explicit `session_id` wins even when the identity
/// header names a different, existing session — while FR1 still labels the
/// file with the *sender's* name, not the receiving shelf's.
#[tokio::test]
async fn session_send_file_explicit_session_id_wins_over_identity_header() {
    let (dir, db, handler) = endpoint_test_handler();
    let _home = HomeVar::set(dir.path());
    let workdir = dir.path().to_string_lossy().into_owned();
    db.insert_interactive_session(
        "caller",
        "caller-name",
        "claude",
        &workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.insert_interactive_session(
        "other",
        "other-name",
        "claude",
        &workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    let png = write_test_png(dir.path());

    let result = send_files(
        &handler,
        vec![png.to_string_lossy().into_owned()],
        None,
        Some("other".to_string()),
        Some(parts_with_identity("caller")),
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));

    let on_other = db.list_shelf_files(Some("other")).unwrap();
    assert_eq!(on_other.len(), 1, "explicit id wins over the header id");
    assert!(
        db.list_shelf_files(Some("caller")).unwrap().is_empty(),
        "the header-named session must receive nothing"
    );
    assert_eq!(
        on_other[0].from_label, "caller-name",
        "FR1: `from` is the sender's session name, not the target shelf's"
    );
}

/// FR2 case 2: no explicit id — the session the caller runs in (identity
/// header of an existing session row) receives the file, labelled with
/// that session's name, and the bytes exist on disk under the returned
/// `file_id`.
#[tokio::test]
async fn session_send_file_files_under_header_session_when_id_omitted() {
    let (dir, db, handler) = endpoint_test_handler();
    let _home = HomeVar::set(dir.path());
    let workdir = dir.path().to_string_lossy().into_owned();
    db.insert_interactive_session(
        "hdr-session",
        "hedda",
        "claude",
        &workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    let png = write_test_png(dir.path());

    let result = send_files(
        &handler,
        vec![png.to_string_lossy().into_owned()],
        Some("from the test".to_string()),
        None,
        Some(parts_with_identity("hdr-session")),
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));

    let body = json_body(&result);
    assert_eq!(body["errors"].as_array().map(Vec::len), Some(0));
    let file_id = body["items"][0]["file_id"].as_str().expect("file_id");

    let rows = db.list_shelf_files(Some("hdr-session")).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].file_id, file_id);
    assert_eq!(rows[0].mime, "image/png");
    assert_eq!(rows[0].caption.as_deref(), Some("from the test"));
    assert_eq!(rows[0].from_label, "hedda");

    // The stored bytes sit inside the store's dir under the returned id.
    let path = handler.shelf.path_for(file_id).expect("valid shelf id");
    assert!(path.is_file(), "missing shelf file {}", path.display());
    assert_eq!(path, handler.shelf.dir().join(file_id));
}

/// FR2 case 3: identity resolves but maps to no session row, so
/// resolution falls through to the host shelf (`None`), labelled `host`.
/// The `CANOPY_AGENT_ID` env fallback is pinned to a known ghost id so a
/// session id inherited from the environment can never decide this test.
#[tokio::test]
async fn session_send_file_without_mapped_session_files_on_host_shelf() {
    let (dir, db, handler) = endpoint_test_handler();
    // No session row: only the shelf directory is an allowed root, so the
    // fixture lives under it.
    let (_home, shelf_dir) = home_shelf_dir(&dir);
    let _guard = AgentIdVar::set("ghost-session");
    let png = write_png_at(&shelf_dir.join("shot.png"));

    let result = send_files(
        &handler,
        vec![png.to_string_lossy().into_owned()],
        None,
        None,
        None,
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));

    let host = db.list_shelf_files(None).unwrap();
    assert_eq!(host.len(), 1, "unmapped identity falls to the host shelf");
    assert_eq!(host[0].from_label, "host");

    let body = json_body(&result);
    let file_id = body["items"][0]["file_id"].as_str().expect("file_id");
    let path = handler.shelf.path_for(file_id).expect("valid shelf id");
    assert!(path.is_file(), "missing shelf file {}", path.display());
}

/// One bad path never blocks the others: a mixed list yields one item and
/// one `not_found` error entry, and the HTTP-level result is still a
/// success (FR2).
#[tokio::test]
async fn session_send_file_partial_success_reports_per_path_errors() {
    let (dir, db, handler) = endpoint_test_handler();
    // No session row: the shelf directory is the only allowed root, so both
    // the good file and the missing path sit under it (the missing one still
    // reports `not_found`, not `forbidden`).
    let (_home, shelf_dir) = home_shelf_dir(&dir);
    let _guard = AgentIdVar::set("ghost-session");
    let png = write_png_at(&shelf_dir.join("shot.png"));
    let missing = shelf_dir.join(format!(
        "no-such-{}.png",
        crate::sessions::shelf::new_file_id()
    ));
    assert!(!missing.exists());

    let result = send_files(
        &handler,
        vec![
            png.to_string_lossy().into_owned(),
            missing.to_string_lossy().into_owned(),
        ],
        None,
        None,
        None,
    )
    .await;
    assert!(
        !is_err(&result),
        "partial success is a tool success: {}",
        text(&result)
    );

    let body = json_body(&result);
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["mime"], "image/png");
    let errors = body["errors"].as_array().expect("errors array");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["code"], "not_found");
    assert_eq!(
        errors[0]["path"].as_str(),
        Some(missing.to_str().expect("utf8 temp path"))
    );

    // The good path still landed on the host shelf.
    assert_eq!(db.list_shelf_files(None).unwrap().len(), 1);
}

/// Whole-call rejects: path count outside 1..=10, a relative path (named
/// in the message), and a caption past 500 characters.
#[tokio::test]
async fn session_send_file_rejects_bad_input() {
    let (dir, _db, handler) = endpoint_test_handler();
    let _guard = AgentIdVar::set("ghost-session");
    let png = write_test_png(dir.path());
    let png_str = png.to_string_lossy().into_owned();

    // 0 paths.
    let result = send_files(&handler, vec![], None, None, None).await;
    assert!(is_err(&result), "0 paths: {}", text(&result));
    assert!(
        text(&result).contains("Pass between 1 and 10"),
        "0 paths: {}",
        text(&result)
    );

    // 11 paths.
    let result = send_files(&handler, vec![png_str.clone(); 11], None, None, None).await;
    assert!(is_err(&result), "11 paths: {}", text(&result));
    assert!(
        text(&result).contains("Pass between 1 and 10"),
        "11 paths: {}",
        text(&result)
    );

    // Relative path — the message names the offending path.
    let result = send_files(
        &handler,
        vec!["relative/shot.png".to_string()],
        None,
        None,
        None,
    )
    .await;
    assert!(is_err(&result), "relative path: {}", text(&result));
    assert!(
        text(&result).contains("relative/shot.png"),
        "relative path: {}",
        text(&result)
    );

    // Caption over 500 characters.
    let result = send_files(&handler, vec![png_str], Some("c".repeat(501)), None, None).await;
    assert!(is_err(&result), "long caption: {}", text(&result));
    assert!(
        text(&result).contains("caption"),
        "long caption: {}",
        text(&result)
    );
}

/// FR2 case 3b: no identity at all (no header, no env) — the host shelf,
/// not an error. An anonymous caller carries no agent id, so there is no
/// session to attribute the upload to; it falls through to the host shelf
/// like an unmapped identity does.
#[tokio::test]
async fn session_send_file_anonymous_caller_uses_host_shelf() {
    let (dir, db, handler) = endpoint_test_handler();
    let (_home, shelf_dir) = home_shelf_dir(&dir);
    let _guard = AgentIdVar::clear();
    let png = write_png_at(&shelf_dir.join("shot.png"));

    let result = send_files(
        &handler,
        vec![png.to_string_lossy().into_owned()],
        None,
        None,
        None,
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));

    let host = db.list_shelf_files(None).unwrap();
    assert_eq!(host.len(), 1, "anonymous callers use the host shelf");
    assert_eq!(host[0].from_label, "host");
}

/// Call `session_list_files` directly.
async fn list_shelf(handler: &TaskTriggerHandler, session_id: Option<String>) -> CallToolResult {
    handler
        .session_list_files(
            Parameters(SessionListFilesParams { session_id }),
            OptionalExtension(None),
        )
        .await
        .unwrap()
}

/// Call `session_read_file` directly.
async fn read_shelf(handler: &TaskTriggerHandler, file_id: String) -> CallToolResult {
    handler
        .session_read_file(Parameters(SessionReadFileParams { file_id }))
        .await
        .unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Any type is accepted through the same sniff/store path: a Markdown file
/// with no magic bytes lands as `application/octet-stream`.
#[tokio::test]
async fn session_send_file_accepts_markdown() {
    let (dir, db, handler) = endpoint_test_handler();
    let (_home, shelf_dir) = home_shelf_dir(&dir);
    let path = shelf_dir.join("notes.md");
    std::fs::write(&path, b"# a markdown note\n").unwrap();

    let result = send_files(
        &handler,
        vec![path.to_string_lossy().into_owned()],
        None,
        None,
        None,
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));
    let body = json_body(&result);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["items"][0]["mime"], "application/octet-stream");
    assert_eq!(body["items"][0]["name"], "notes.md");

    let rows = db.list_shelf_files(None).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].origin, "agent");
    assert_eq!(rows[0].size, b"# a markdown note\n".len() as i64);
}

/// `session_list_files` and `session_read_file` round-trip both origins:
/// an agent-sent text file reads as text, a remote-uploaded binary reads as
/// base64.
#[tokio::test]
async fn session_list_files_and_session_read_file_round_trip() {
    let (dir, db, handler) = endpoint_test_handler();
    let (_home, shelf_dir) = home_shelf_dir(&dir);
    let path = shelf_dir.join("brief.md");
    std::fs::write(&path, b"hello from the agent").unwrap();
    let sent = send_files(
        &handler,
        vec![path.to_string_lossy().into_owned()],
        None,
        None,
        None,
    )
    .await;
    assert!(!is_err(&sent), "{}", text(&sent));
    let agent_id = json_body(&sent)["items"][0]["file_id"]
        .as_str()
        .unwrap()
        .to_string();

    // A remote-style upload into the host shelf: origin `user`.
    let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
    let user_id = handler
        .shelf
        .begin_upload(None, "photo.png", png.len() as u64, None, &sha256_hex(&png))
        .unwrap();
    handler
        .shelf
        .upload_part(&user_id, 0, &png)
        .expect("append part");
    handler.shelf.upload_end(&user_id).expect("commit upload");

    let listed = list_shelf(&handler, None).await;
    assert!(!is_err(&listed), "{}", text(&listed));
    let items = json_body(&listed)["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 2, "{items:?}");
    let by_id = |id: &str| {
        items
            .iter()
            .find(|item| item["file_id"] == id)
            .unwrap_or_else(|| panic!("missing {id} in {items:?}"))
            .clone()
    };
    let agent_item = by_id(&agent_id);
    assert_eq!(agent_item["origin"], "agent");
    assert_eq!(agent_item["mime"], "application/octet-stream");
    let user_item = by_id(&user_id);
    assert_eq!(user_item["origin"], "user");
    assert_eq!(user_item["mime"], "image/png");
    assert!(user_item["created_at"].as_str().is_some());

    let text_read = read_shelf(&handler, agent_id).await;
    assert!(!is_err(&text_read), "{}", text(&text_read));
    let text_body = json_body(&text_read);
    assert_eq!(text_body["enc"], "text");
    assert_eq!(text_body["content"], "hello from the agent");
    assert_eq!(text_body["truncated"], false);

    let binary_read = read_shelf(&handler, user_id).await;
    assert!(!is_err(&binary_read), "{}", text(&binary_read));
    let binary_body = json_body(&binary_read);
    assert_eq!(binary_body["enc"], "b64");
    assert_eq!(binary_body["truncated"], false);
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(binary_body["content"].as_str().unwrap())
        .unwrap();
    assert_eq!(decoded, png, "base64 round-trips the binary bytes");

    let missing = read_shelf(&handler, "ffffffffffffffff".to_string()).await;
    assert!(is_err(&missing), "unknown id: {}", text(&missing));
    let _ = db;
}

/// Reads past the 1 MiB cap come back truncated: binary as the first
/// mebibyte of base64, text as the first mebibyte of UTF-8 floored at a
/// character boundary (never mislabelled as binary when the cap splits a
/// multi-byte character).
#[tokio::test]
async fn session_read_file_truncates_past_one_mebibyte() {
    let (_dir, _db, handler) = endpoint_test_handler();
    const CAP: usize = 1024 * 1024;
    use base64::Engine as _;

    let big = vec![0xFFu8; CAP + 100];
    let item = handler
        .shelf
        .insert_bytes(&big, "big.bin".to_string(), None, None, "test")
        .unwrap();
    let read = read_shelf(&handler, item.file_id).await;
    assert!(!is_err(&read), "{}", text(&read));
    let body = json_body(&read);
    assert_eq!(body["enc"], "b64");
    assert_eq!(body["truncated"], true);
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(body["content"].as_str().unwrap())
        .unwrap();
    assert_eq!(decoded.len(), CAP);
    assert_eq!(decoded, big[..CAP]);

    // `é` straddles the cap: 1 MiB − 1 ASCII bytes, then two bytes of `é`.
    let mut text_bytes = vec![b'a'; CAP - 1];
    text_bytes.extend_from_slice("é".as_bytes());
    text_bytes.resize(text_bytes.len() + 100, b'b');
    let item = handler
        .shelf
        .insert_bytes(&text_bytes, "big.txt".to_string(), None, None, "test")
        .unwrap();
    let read = read_shelf(&handler, item.file_id).await;
    assert!(!is_err(&read), "{}", text(&read));
    let body = json_body(&read);
    assert_eq!(body["enc"], "text");
    assert_eq!(body["truncated"], true);
    assert_eq!(body["content"].as_str().unwrap(), "a".repeat(CAP - 1));
}

/// A caller session row whose workdir is a fresh subdir of the test dir,
/// with the caller identity pointing at it.
fn confined_session(
    db: &std::sync::Arc<Database>,
    id: &str,
    dir: &tempfile::TempDir,
) -> std::path::PathBuf {
    let workdir = dir.path().join("work");
    std::fs::create_dir_all(&workdir).expect("work dir");
    db.insert_interactive_session(
        id,
        "confined",
        "claude",
        workdir.to_str().expect("utf8 workdir"),
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    workdir
}

/// The `code` of the error entry for `path` in a `session_send_file` body.
fn error_code<'body>(body: &'body serde_json::Value, path: &str) -> &'body str {
    body["errors"]
        .as_array()
        .unwrap_or_else(|| panic!("no errors array in {body}"))
        .iter()
        .find(|entry| entry["path"] == path)
        .unwrap_or_else(|| panic!("no error for {path} in {body}"))["code"]
        .as_str()
        .expect("a code per error")
}

/// F-01: a `..` escape and an absolute path outside the workdir are
/// `forbidden`, while a file inside the workdir still lands on the shelf.
#[tokio::test]
async fn session_send_file_refuses_paths_outside_workdir() {
    let (dir, db, handler) = endpoint_test_handler();
    let _home = HomeVar::set(dir.path());
    let workdir = confined_session(&db, "me", &dir);
    let _guard = AgentIdVar::set("me");

    let ok = write_png_at(&workdir.join("ok.png"));
    let outside = write_png_at(&dir.path().join("escape.png"));
    let dotdot = workdir.join("..").join("escape.png");

    let result = send_files(
        &handler,
        vec![
            ok.to_string_lossy().into_owned(),
            dotdot.to_string_lossy().into_owned(),
            outside.to_string_lossy().into_owned(),
        ],
        None,
        None,
        None,
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));
    let body = json_body(&result);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(error_code(&body, &dotdot.to_string_lossy()), "forbidden");
    assert_eq!(error_code(&body, &outside.to_string_lossy()), "forbidden");
    assert_eq!(db.list_shelf_files(Some("me")).unwrap().len(), 1);
}

/// F-01: secret names inside the workdir, a symlink to the daemon token,
/// and a file under the data dir outside `shelf/` are all `forbidden` with
/// the same message, even though they sit inside an allowed root.
#[tokio::test]
async fn session_send_file_refuses_secrets_and_data_dir() {
    let (dir, db, handler) = endpoint_test_handler();
    let (_home, shelf_dir) = home_shelf_dir(&dir);
    let workdir = confined_session(&db, "me", &dir);
    let _guard = AgentIdVar::set("me");
    let data_dir = shelf_dir.parent().expect("data dir").to_path_buf();

    let token = write_png_at(&data_dir.join("daemon.token"));
    let link = workdir.join("token-link.png");
    std::os::unix::fs::symlink(&token, &link).expect("symlink");
    let secret_names = ["daemon.token", "agent.key", "agent.pem", "id_rsa"];
    let mut paths = vec![link.to_string_lossy().into_owned()];
    for name in secret_names {
        let path = write_png_at(&workdir.join(name));
        paths.push(path.to_string_lossy().into_owned());
    }
    let config = write_png_at(&data_dir.join("config.toml"));
    paths.push(config.to_string_lossy().into_owned());

    let result = send_files(&handler, paths.clone(), None, None, None).await;
    assert!(!is_err(&result), "{}", text(&result));
    let body = json_body(&result);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors.len(), paths.len(), "{body}");
    for error in errors {
        assert_eq!(error["code"], "forbidden");
        assert_eq!(error["message"], "path is outside this session's workdir");
    }
    assert!(db.list_shelf_files(Some("me")).unwrap().is_empty());
}

/// F-01: the shelf directory is an allowed root too, so a caller with a
/// session can send a file from under it.
#[tokio::test]
async fn session_send_file_passes_for_shelf_dir_with_session() {
    let (dir, db, handler) = endpoint_test_handler();
    let (_home, shelf_dir) = home_shelf_dir(&dir);
    confined_session(&db, "me", &dir);
    let _guard = AgentIdVar::set("me");

    let png = write_png_at(&shelf_dir.join("from-shelf.png"));
    let result = send_files(
        &handler,
        vec![png.to_string_lossy().into_owned()],
        None,
        None,
        None,
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));
    let body = json_body(&result);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["errors"].as_array().unwrap().len(), 0);
}

/// F-01: a caller with no session row has the shelf directory only — a
/// workdir-style file is `forbidden`, a shelf file passes, and one never
/// blocks the other.
#[tokio::test]
async fn session_send_file_no_session_forbids_workdir_file() {
    let (dir, _db, handler) = endpoint_test_handler();
    let (_home, shelf_dir) = home_shelf_dir(&dir);
    let _guard = AgentIdVar::set("ghost-session");

    let shelf_png = write_png_at(&shelf_dir.join("shot.png"));
    let work_png = write_test_png(dir.path());
    let result = send_files(
        &handler,
        vec![
            shelf_png.to_string_lossy().into_owned(),
            work_png.to_string_lossy().into_owned(),
        ],
        None,
        None,
        None,
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));
    let body = json_body(&result);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(error_code(&body, &work_png.to_string_lossy()), "forbidden");
}

/// F-01: one forbidden path does not block the other paths of the same
/// call — a session caller gets one item and one `forbidden` error.
#[tokio::test]
async fn session_send_file_mixed_ok_and_forbidden() {
    let (dir, db, handler) = endpoint_test_handler();
    let _home = HomeVar::set(dir.path());
    let workdir = confined_session(&db, "me", &dir);
    let _guard = AgentIdVar::set("me");

    let ok = write_png_at(&workdir.join("ok.png"));
    let outside = write_png_at(&dir.path().join("outside.png"));
    let result = send_files(
        &handler,
        vec![
            ok.to_string_lossy().into_owned(),
            outside.to_string_lossy().into_owned(),
        ],
        None,
        None,
        None,
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));
    let body = json_body(&result);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(error_code(&body, &outside.to_string_lossy()), "forbidden");
    assert_eq!(db.list_shelf_files(Some("me")).unwrap().len(), 1);
}

/// A directory and a FIFO inside the workdir pass confinement and keep the
/// unchanged `not_a_file` from `insert`.
#[tokio::test]
async fn session_send_file_dir_and_fifo_are_not_a_file() {
    let (dir, db, handler) = endpoint_test_handler();
    let _home = HomeVar::set(dir.path());
    let workdir = confined_session(&db, "me", &dir);
    let _guard = AgentIdVar::set("me");

    let fifo = workdir.join("pipe");
    let cpath =
        std::ffi::CString::new(fifo.to_string_lossy().into_owned().into_bytes()).expect("cstring");
    // SAFETY: `mkfifo` on a NUL-terminated temp path with user read/write.
    let rc = unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo");
    let result = send_files(
        &handler,
        vec![
            workdir.to_string_lossy().into_owned(),
            fifo.to_string_lossy().into_owned(),
        ],
        None,
        None,
        None,
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));
    let body = json_body(&result);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(error_code(&body, &workdir.to_string_lossy()), "not_a_file");
    assert_eq!(error_code(&body, &fifo.to_string_lossy()), "not_a_file");
}

/// A missing path inside the workdir reaches `insert` (`not_found`); a
/// missing path outside every root gets the byte-identical `forbidden`
/// message as an existing file outside.
#[tokio::test]
async fn session_send_file_missing_inside_not_found_outside_forbidden() {
    let (dir, db, handler) = endpoint_test_handler();
    let _home = HomeVar::set(dir.path());
    let workdir = confined_session(&db, "me", &dir);
    let _guard = AgentIdVar::set("me");
    let outside_dir = tempfile::tempdir().expect("outside dir");

    let missing_inside = workdir.join("missing.png");
    let missing_outside = outside_dir.path().join("missing.png");
    let existing_outside = write_png_at(&outside_dir.path().join("real.png"));
    let result = send_files(
        &handler,
        vec![
            missing_inside.to_string_lossy().into_owned(),
            missing_outside.to_string_lossy().into_owned(),
            existing_outside.to_string_lossy().into_owned(),
        ],
        None,
        None,
        None,
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));
    let body = json_body(&result);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(
        error_code(&body, &missing_inside.to_string_lossy()),
        "not_found"
    );
    let errors = body["errors"].as_array().unwrap();
    let message_of = |path: &str| {
        errors
            .iter()
            .find(|entry| entry["path"] == path)
            .unwrap_or_else(|| panic!("no error for {path} in {body}"))["message"]
            .as_str()
            .expect("a message per error")
            .to_string()
    };
    assert_eq!(
        error_code(&body, &missing_outside.to_string_lossy()),
        "forbidden"
    );
    assert_eq!(
        error_code(&body, &existing_outside.to_string_lossy()),
        "forbidden"
    );
    assert_eq!(
        message_of(&missing_outside.to_string_lossy()),
        message_of(&existing_outside.to_string_lossy())
    );
    let _ = db;
}
