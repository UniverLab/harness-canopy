//! Schedule tests: `prompt_schedule` stores a composed draft with the
//! session's workdir; bad times, unknown presets, exited or terminal sessions
//! are refused with nothing stored; `scheduled_list` orders by fire time with
//! char-boundary previews; `scheduled_cancel` drops only its own session's
//! row.

use super::{
    agent_spawn, connect_client, recv_json, send_json, serve_state, terminal_spawn, test_db,
    test_hub, KillGuard,
};
use crate::domain::prompt_draft::{build_prompt_text, DraftSection, PromptContext, PromptDraft};
use crate::sessions::catalog::{scheduled_frame, scheduled_list_frame, ScheduledPromptItem};
use crate::sessions::protocol::{parse_host_frame, HostFrame};
use crate::sessions::ws::SessionsState;

fn sections_draft() -> PromptDraft {
    PromptDraft {
        sections: vec![DraftSection {
            title: "instruction_1".to_string(),
            content: "echo remote-schedule-ok".to_string(),
        }],
        ..PromptDraft::default()
    }
}

fn draft_json(draft: &PromptDraft) -> serde_json::Value {
    serde_json::to_value(draft).expect("draft serializes")
}

#[tokio::test]
async fn schedule_ok_inserts_row_with_composed_text_and_workdir() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let workdir_str = workdir.path().to_string_lossy().to_string();
    let hub = test_hub(&db);
    let (session, _guard) = KillGuard::spawn(&hub, &agent_spawn("cat", &workdir_str));
    let id = session.id.clone();
    drop(session);

    let state = SessionsState::for_test(hub, "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    let fire_at = (chrono::Utc::now() + chrono::Duration::hours(2))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let draft = sections_draft();
    send_json(
        &mut client,
        &serde_json::json!({
            "t": "prompt_schedule", "req": "s1", "id": id,
            "draft": draft_json(&draft), "fire_at": fire_at,
        }),
    )
    .await;
    let scheduled = recv_json(&mut client, "scheduled").await;
    assert_eq!(scheduled["t"], "scheduled");
    assert_eq!(scheduled["req"], "s1");
    let item = &scheduled["item"];
    assert!(
        item["id"].as_str().is_some_and(|id| id.starts_with("ss-")),
        "schedule id shape: {scheduled}"
    );
    assert_eq!(item["fire_at"], fire_at.as_str());

    let expected = build_prompt_text(&draft, &PromptContext::default()).expect("builds");
    let truncated: String = expected.chars().take(120).collect();
    assert_eq!(item["preview"], truncated.as_str());

    let rows = db
        .list_pending_scheduled_prompts_for_session(&id)
        .expect("lists");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, item["id"].as_str().expect("id"));
    assert_eq!(rows[0].prompt, expected);
    assert_eq!(rows[0].target_session_id, id);
    assert_eq!(rows[0].workdir.as_deref(), Some(workdir_str.as_str()));
    assert_eq!(
        rows[0]
            .fire_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        fire_at
    );
    assert!(rows[0].builder_state.is_none());
    assert!(rows[0].provenance.is_none());
    server.abort();
}

#[tokio::test]
async fn schedule_past_and_garbage_fire_at_refused_without_row() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) =
        KillGuard::spawn(&hub, &agent_spawn("cat", &workdir.path().to_string_lossy()));
    let id = session.id.clone();
    drop(session);

    let state = SessionsState::for_test(hub, "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    for (req, fire_at) in [
        ("s-past", "2000-01-01T00:00:00Z"),
        ("s-garbage", "tomorrow-ish"),
    ] {
        send_json(
            &mut client,
            &serde_json::json!({
                "t": "prompt_schedule", "req": req, "id": id,
                "draft": draft_json(&sections_draft()), "fire_at": fire_at,
            }),
        )
        .await;
        let error = recv_json(&mut client, "error").await;
        assert_eq!(error["t"], "error");
        assert_eq!(error["code"], "bad_request");
        assert_eq!(error["req"], req);
        assert_eq!(error["message"], "fire_at must be a future RFC 3339 time");
    }

    assert!(
        db.list_pending_scheduled_prompts_for_session(&id)
            .expect("lists")
            .is_empty(),
        "refused schedules store nothing"
    );
    server.abort();
}

#[tokio::test]
async fn schedule_unknown_preset_is_bad_request_naming_it() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) =
        KillGuard::spawn(&hub, &agent_spawn("cat", &workdir.path().to_string_lossy()));
    let id = session.id.clone();
    drop(session);

    let state = SessionsState::for_test(hub, "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    let fire_at = (chrono::Utc::now() + chrono::Duration::hours(2))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    send_json(
        &mut client,
        &serde_json::json!({
            "t": "prompt_schedule", "req": "s-bad", "id": id,
            "draft": {"sections": [], "preset": "no-such-preset-xyz"},
            "fire_at": fire_at,
        }),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["code"], "bad_request");
    assert_eq!(error["req"], "s-bad");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("no-such-preset-xyz")),
        "must name the choice: {error}"
    );
    assert!(
        db.list_pending_scheduled_prompts_for_session(&id)
            .expect("lists")
            .is_empty(),
        "a failed build stores nothing"
    );
    server.abort();
}

#[tokio::test]
async fn schedule_exited_session_refused() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) =
        KillGuard::spawn(&hub, &agent_spawn("cat", &workdir.path().to_string_lossy()));
    let id = session.id.clone();
    session.mark_exited_for_test(0);
    drop(session);

    let state = SessionsState::for_test(hub, "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    let fire_at = (chrono::Utc::now() + chrono::Duration::hours(2))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    send_json(
        &mut client,
        &serde_json::json!({
            "t": "prompt_schedule", "req": "s-exit", "id": id,
            "draft": draft_json(&sections_draft()), "fire_at": fire_at,
        }),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["code"], "forbidden");
    assert_eq!(error["req"], "s-exit");
    assert_eq!(error["message"], "session has exited");
    server.abort();
}

#[tokio::test]
async fn schedule_terminal_and_unknown_sessions_refused() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) = KillGuard::spawn(
        &hub,
        &terminal_spawn("sh", &workdir.path().to_string_lossy()),
    );
    let id = session.id.clone();
    drop(session);

    let state = SessionsState::for_test(hub, "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    let fire_at = (chrono::Utc::now() + chrono::Duration::hours(2))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    send_json(
        &mut client,
        &serde_json::json!({
            "t": "prompt_schedule", "req": "s-term", "id": id,
            "draft": draft_json(&sections_draft()), "fire_at": fire_at,
        }),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["code"], "forbidden");
    assert_eq!(error["req"], "s-term");
    assert_eq!(
        error["message"],
        "only agent sessions take a scheduled prompt"
    );

    send_json(
        &mut client,
        &serde_json::json!({
            "t": "prompt_schedule", "req": "s-missing", "id": "missing",
            "draft": draft_json(&sections_draft()), "fire_at": fire_at,
        }),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["code"], "not_found");
    assert_eq!(error["req"], "s-missing");
    server.abort();
}

#[tokio::test]
async fn list_orders_by_fire_at_and_truncates_preview_on_char_boundary() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) =
        KillGuard::spawn(&hub, &agent_spawn("cat", &workdir.path().to_string_lossy()));
    let id = session.id.clone();
    drop(session);

    // Inserted later-first so the answer must reorder them.
    let later = chrono::Utc::now() + chrono::Duration::hours(2);
    let earlier = chrono::Utc::now() + chrono::Duration::hours(1);
    // 119 ASCII chars plus a 4-byte emoji at the cut: a byte cut would split
    // the emoji, a char cut keeps it whole as the 120th char.
    let wide = format!("{}🎉tail text that must not appear", "x".repeat(119));
    db.insert_scheduled_prompt("ss-later", "later prompt", &id, None, later, None, None)
        .expect("inserts");
    db.insert_scheduled_prompt("ss-earlier", &wide, &id, None, earlier, None, None)
        .expect("inserts");

    let state = SessionsState::for_test(hub, "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({"t": "scheduled_list", "req": "l1", "id": id}),
    )
    .await;
    let listed = recv_json(&mut client, "scheduled_list").await;
    assert_eq!(listed["t"], "scheduled_list");
    assert_eq!(listed["req"], "l1");
    assert_eq!(listed["id"], id.as_str());
    let items = listed["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], "ss-earlier");
    assert_eq!(items[1]["id"], "ss-later");

    let preview = items[0]["preview"].as_str().expect("preview");
    assert_eq!(preview.chars().count(), 120);
    assert!(preview.ends_with('🎉'), "emoji kept whole: {preview}");
    assert!(wide.starts_with(preview));
    assert!(!preview.contains("tail"), "cut at 120 chars: {preview}");
    assert_eq!(items[1]["preview"], "later prompt");
    server.abort();
}

#[tokio::test]
async fn cancel_removes_the_row_and_answers_ok() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) =
        KillGuard::spawn(&hub, &agent_spawn("cat", &workdir.path().to_string_lossy()));
    let id = session.id.clone();
    drop(session);

    let fire_at = chrono::Utc::now() + chrono::Duration::hours(1);
    db.insert_scheduled_prompt("ss-drop", "drop me", &id, None, fire_at, None, None)
        .expect("inserts");

    let state = SessionsState::for_test(hub, "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({"t": "scheduled_cancel", "req": "c1", "id": id, "schedule_id": "ss-drop"}),
    )
    .await;
    let ok = recv_json(&mut client, "ok").await;
    assert_eq!(ok["t"], "ok");
    assert_eq!(ok["req"], "c1");
    assert!(
        db.list_pending_scheduled_prompts_for_session(&id)
            .expect("lists")
            .is_empty(),
        "cancel deletes the row"
    );
    server.abort();
}

#[tokio::test]
async fn cancel_other_session_row_refused_and_kept() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let workdir_str = workdir.path().to_string_lossy().to_string();
    let hub = test_hub(&db);
    let (first, _first_guard) = KillGuard::spawn(&hub, &agent_spawn("cat", &workdir_str));
    let (second, _second_guard) = KillGuard::spawn(&hub, &agent_spawn("cat", &workdir_str));
    let (first_id, second_id) = (first.id.clone(), second.id.clone());
    drop((first, second));

    let fire_at = chrono::Utc::now() + chrono::Duration::hours(1);
    db.insert_scheduled_prompt("ss-keep", "keep me", &second_id, None, fire_at, None, None)
        .expect("inserts");

    let state = SessionsState::for_test(hub, "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({
            "t": "scheduled_cancel", "req": "c2",
            "id": first_id, "schedule_id": "ss-keep",
        }),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["code"], "not_found");
    assert_eq!(error["req"], "c2");
    assert_eq!(error["message"], "no such scheduled prompt");

    send_json(
        &mut client,
        &serde_json::json!({
            "t": "scheduled_cancel", "req": "c3",
            "id": second_id, "schedule_id": "ss-missing",
        }),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["code"], "not_found");
    assert_eq!(error["message"], "no such scheduled prompt");

    let kept = db
        .list_pending_scheduled_prompts_for_session(&second_id)
        .expect("lists");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].id, "ss-keep");
    server.abort();
}

#[test]
fn scheduled_and_list_shapes_roundtrip() {
    let item = ScheduledPromptItem {
        id: "ss-1".to_string(),
        fire_at: "2030-01-01T00:00:00Z".to_string(),
        preview: "hello".to_string(),
    };
    match parse_host_frame(&scheduled_frame("s1", &item)) {
        Ok(frame @ HostFrame::Scheduled { .. }) => assert_eq!(frame.req(), Some("s1")),
        other => panic!("expected a scheduled frame, got {other:?}"),
    }
    match parse_host_frame(&scheduled_list_frame("l1", "sess", &[item])) {
        Ok(frame @ HostFrame::ScheduledList { .. }) => assert_eq!(frame.req(), Some("l1")),
        other => panic!("expected a scheduled_list frame, got {other:?}"),
    }
}
