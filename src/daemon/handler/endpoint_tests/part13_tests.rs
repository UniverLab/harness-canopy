//! Part 13 of `endpoint_tests`: `session_send_file` (the session shelf).
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
    db.insert_interactive_session(
        "caller",
        "caller-name",
        "claude",
        "/proj",
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
        "/proj2",
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
    db.insert_interactive_session(
        "hdr-session",
        "hedda",
        "claude",
        "/proj",
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
    let _guard = AgentIdVar::set("ghost-session");
    let png = write_test_png(dir.path());

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
    let _guard = AgentIdVar::set("ghost-session");
    let png = write_test_png(dir.path());
    let missing = std::env::temp_dir().join(format!(
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
/// not an error. The nursery gate only applies to callers that HAVE an
/// identity to check; an anonymous caller has no session whose type could
/// be `nursery`, so it falls through to the host shelf like an unmapped
/// identity does.
#[tokio::test]
async fn session_send_file_anonymous_caller_uses_host_shelf() {
    let (dir, db, handler) = endpoint_test_handler();
    let _guard = AgentIdVar::clear();
    let png = write_test_png(dir.path());

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
