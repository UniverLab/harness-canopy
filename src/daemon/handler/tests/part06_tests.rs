//! Part 06 of `tests`: per-spec hook validation (`spec_ids`).
use super::*;
use crate::daemon::handler_graph_updates::parse_graph_hooks_param;
use crate::domain::graphs::GraphHookEvent;

fn spec_hook_params(spec_ids: Option<Vec<String>>) -> GraphCompletionHookParams {
    GraphCompletionHookParams {
        platform: None,
        crew: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some("true".to_string()),
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,
        spec_ids,
    }
}

fn hooks_param(
    event: &str,
    params: Vec<GraphCompletionHookParams>,
) -> Option<std::collections::BTreeMap<String, Vec<GraphCompletionHookParams>>> {
    let mut map = std::collections::BTreeMap::new();
    map.insert(event.to_string(), params);
    Some(map)
}

/// Graph with two bound specs plus a queue-only spec on its active run's
/// queue — the candidate set `spec_ids` resolves against.
fn spec_ids_test_db() -> (tempfile::TempDir, std::sync::Arc<Database>, String) {
    let (_dir, db, _handler) = queue_test_handler();
    let graph_id = "graph-spec-ids";
    db.insert_graph(&Graph {
        trigger_prompt: None,
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: graph_id.to_string(),
        name: graph_id.to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status: GraphStatus::Draft,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        enabled: true,
        enable_at: None,
        expires_at: None,
    })
    .unwrap();
    for (position, spec) in ["spec-alpha", "spec-beta"].iter().enumerate() {
        let mut row = standalone_spec(spec);
        row.graph_id = Some(graph_id.to_string());
        row.position = position as i64 + 1;
        db.insert_graph_spec(&row).unwrap();
    }
    db.insert_graph_spec(&standalone_spec("spec-queue"))
        .unwrap();
    insert_queue(&db, "queue-spec-ids");
    db.append_queue_member("queue-spec-ids", "spec-queue", None)
        .unwrap();
    db.set_graph_active_run_queue(graph_id, Some("queue-spec-ids"))
        .unwrap();
    (_dir, db, graph_id.to_string())
}

/// An unambiguous prefix resolves to the full bound spec id at write time.
#[test]
fn spec_ids_prefix_resolves_to_full_id() {
    let (_dir, db, graph_id) = spec_ids_test_db();
    let parsed = parse_graph_hooks_param(
        &db,
        &graph_id,
        &hooks_param(
            "on_spec_completed",
            vec![spec_hook_params(Some(vec!["spec-al".to_string()]))],
        ),
    )
    .unwrap()
    .unwrap();
    let hooks = &parsed[&GraphHookEvent::OnSpecCompleted];
    assert_eq!(
        hooks[0].spec_ids,
        Some(vec!["spec-alpha".to_string()]),
        "the stored hook must carry the full id"
    );
}

/// A queue-only spec resolves through the active run's queue members.
#[test]
fn spec_ids_queue_member_resolves_via_active_run_queue() {
    let (_dir, db, graph_id) = spec_ids_test_db();
    let parsed = parse_graph_hooks_param(
        &db,
        &graph_id,
        &hooks_param(
            "on_failed",
            vec![spec_hook_params(Some(vec!["spec-qu".to_string()]))],
        ),
    )
    .unwrap()
    .unwrap();
    let hooks = &parsed[&GraphHookEvent::OnFailed];
    assert_eq!(
        hooks[0].spec_ids,
        Some(vec!["spec-queue".to_string()]),
        "queue members are candidates too"
    );
}

/// An entry matching nothing is refused, naming the entry.
#[test]
fn spec_ids_unknown_entry_names_it() {
    let (_dir, db, graph_id) = spec_ids_test_db();
    let err = parse_graph_hooks_param(
        &db,
        &graph_id,
        &hooks_param(
            "on_spec_completed",
            vec![spec_hook_params(Some(vec!["spec-zzz".to_string()]))],
        ),
    )
    .unwrap_err();
    let text = result_text(&err);
    assert!(text.contains("spec-zzz"), "{text}");
}

/// An empty list is refused.
#[test]
fn spec_ids_empty_list_is_refused() {
    let (_dir, db, graph_id) = spec_ids_test_db();
    let err = parse_graph_hooks_param(
        &db,
        &graph_id,
        &hooks_param("on_spec_completed", vec![spec_hook_params(Some(vec![]))]),
    )
    .unwrap_err();
    let text = result_text(&err);
    assert!(text.contains("spec_ids"), "{text}");
}

/// `on_completed` refuses `spec_ids`, naming the event.
#[test]
fn spec_ids_refused_on_completed_naming_event() {
    let (_dir, db, graph_id) = spec_ids_test_db();
    let err = parse_graph_hooks_param(
        &db,
        &graph_id,
        &hooks_param(
            "on_completed",
            vec![spec_hook_params(Some(vec!["spec-alpha".to_string()]))],
        ),
    )
    .unwrap_err();
    let text = result_text(&err);
    assert!(text.contains("on_completed"), "{text}");
    assert!(text.contains("spec_ids"), "{text}");
}

/// `on_blocked` refuses `spec_ids`, naming the event.
#[test]
fn spec_ids_refused_on_blocked_naming_event() {
    let (_dir, db, graph_id) = spec_ids_test_db();
    let err = parse_graph_hooks_param(
        &db,
        &graph_id,
        &hooks_param(
            "on_blocked",
            vec![spec_hook_params(Some(vec!["spec-alpha".to_string()]))],
        ),
    )
    .unwrap_err();
    let text = result_text(&err);
    assert!(text.contains("on_blocked"), "{text}");
    assert!(text.contains("spec_ids"), "{text}");
}

// ── subagent spawn notify_session ───────────────────────────────
// These pin the spawn-time half of the notify contract (resolution and
// refusal happen before any child spawns, so no platform binary runs).
// The delivery half lives in `daemon::subagent_notify` tests.

use crate::daemon::params::SubagentSpawnParams;
use crate::daemon::subagent::{spawn_subagent, spawn_subagent_blocking};
use std::sync::Arc;

fn notify_spawn_params(notify_session: Option<&str>, blocking: bool) -> SubagentSpawnParams {
    SubagentSpawnParams {
        prompt: "hi".to_string(),
        cli: None,
        workdir: None,
        model: None,
        effort: None,
        mcp_servers: None,
        timeout_minutes: None,
        ttl_minutes: None,
        blocking: Some(blocking),
        notify_session: notify_session.map(str::to_string),
    }
}

fn parts_for_session(session_id: &str) -> axum::http::request::Parts {
    let request = axum::http::Request::builder()
        .header(CANOPY_AGENT_ID_HEADER, session_id)
        .body(())
        .unwrap();
    let (parts, _) = request.into_parts();
    parts
}

fn add_notify_session(db: &Database, id: &str, name: &str, workdir: &str) {
    db.insert_interactive_session(
        id,
        name,
        "opencode",
        workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
}

#[tokio::test]
async fn subagent_spawn_blocking_with_notify_session_is_refused() {
    let (_dir, _db, handler) = queue_test_handler();
    let result = handler
        .subagent_spawn_impl(notify_spawn_params(Some("orch"), true), None)
        .await
        .unwrap();
    assert!(
        queue_is_err(&result),
        "blocking+notify must fail: {result:?}"
    );
    let text = queue_raw_text(&result);
    assert!(
        text.contains("blocking") && text.contains("already returns"),
        "{text}"
    );
}

#[tokio::test]
async fn subagent_spawn_unknown_notify_target_is_refused() {
    let (_dir, _db, handler) = queue_test_handler();
    let result = handler
        .subagent_spawn_impl(notify_spawn_params(Some("ghost"), false), None)
        .await
        .unwrap();
    assert!(
        queue_is_err(&result),
        "unknown target must fail: {result:?}"
    );
    let text = queue_raw_text(&result);
    assert!(
        text.contains("ghost") && text.contains("no longer live"),
        "{text}"
    );
}

#[tokio::test]
async fn subagent_spawn_ambiguous_notify_name_is_refused() {
    let (_dir, db, handler) = queue_test_handler();
    add_notify_session(&db, "sess-1", "orch", "/tmp");
    add_notify_session(&db, "sess-2", "orch", "/tmp");
    let result = handler
        .subagent_spawn_impl(notify_spawn_params(Some("orch"), false), None)
        .await
        .unwrap();
    assert!(
        queue_is_err(&result),
        "ambiguous name must fail: {result:?}"
    );
    assert!(queue_raw_text(&result).contains("live sessions"));
}

#[tokio::test]
async fn subagent_spawn_notify_target_in_other_workdir_is_refused_naming_both() {
    let spawn_dir = tempdir().unwrap();
    let target_dir = tempdir().unwrap();
    let (_dir, db, handler) = queue_test_handler();
    let target_path = target_dir.path().to_str().unwrap();
    add_notify_session(&db, "sess-1", "orch", target_path);
    let mut params = notify_spawn_params(Some("orch"), false);
    params.workdir = Some(spawn_dir.path().to_str().unwrap().to_string());
    let result = handler.subagent_spawn_impl(params, None).await.unwrap();
    assert!(
        queue_is_err(&result),
        "other-workdir target must fail: {result:?}"
    );
    let text = queue_raw_text(&result);
    assert!(
        text.contains(target_path),
        "refusal must name target workdir: {text}"
    );
    assert!(
        text.contains(spawn_dir.path().to_str().unwrap()),
        "refusal must name spawn workdir: {text}"
    );
}

#[tokio::test]
async fn subagent_spawn_finished_target_is_refused_as_not_live() {
    let (_dir, db, handler) = queue_test_handler();
    add_notify_session(&db, "sess-1", "orch", "/tmp");
    db.finish_interactive_session("sess-1", 0).unwrap();
    let result = handler
        .subagent_spawn_impl(notify_spawn_params(Some("orch"), false), None)
        .await
        .unwrap();
    assert!(
        queue_is_err(&result),
        "finished session must fail: {result:?}"
    );
    assert!(queue_raw_text(&result).contains("no longer live"));
}

#[tokio::test]
async fn subagent_spawn_notify_resolves_before_workdir_validation() {
    // Resolution runs in the handler before `spawn_subagent` validates the
    // workdir: an unknown target with a missing workdir must fail on the
    // target, never on the directory.
    let (_dir, _db, handler) = queue_test_handler();
    let mut params = notify_spawn_params(Some("ghost"), false);
    params.workdir = Some("/nonexistent-notify-spawn-cwd".to_string());
    let result = handler.subagent_spawn_impl(params, None).await.unwrap();
    assert!(
        queue_is_err(&result),
        "unknown target must fail: {result:?}"
    );
    let text = queue_raw_text(&result);
    assert!(
        text.contains("ghost") && text.contains("no longer live"),
        "{text}"
    );
}

#[tokio::test]
async fn subagent_spawn_default_target_passes_resolution_to_workdir_check() {
    // Omitted target with a live caller resolves (not refused), so a
    // missing workdir is what fails — the spawn never runs.
    let (_dir, db, handler) = queue_test_handler();
    add_notify_session(&db, "sess-1", "orch", "/tmp");
    let parts = parts_for_session("sess-1");
    let mut params = notify_spawn_params(None, false);
    params.workdir = Some("/nonexistent-notify-spawn-cwd".to_string());
    let result = handler
        .subagent_spawn_impl(params, Some(&parts))
        .await
        .unwrap();
    assert!(
        queue_is_err(&result),
        "missing workdir must fail: {result:?}"
    );
    assert!(queue_raw_text(&result).contains("does not exist"));
}

// ── spawn-side workdir validation wiring (FR2) ───────────────────
// These pin that the spawn functions actually CALL `validate_workdir`
// before anything else runs: removing the call makes the spawn walk past
// it, so the explicit refusal (and the `validate_workdir` unit tests)
// alone cannot keep the feature alive. They take the direct-spawn path
// rather than the handler because the handler's success path would
// spawn a real platform binary.

#[tokio::test]
async fn spawn_subagent_refuses_a_missing_workdir_naming_the_path() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::new(&tmp.path().join("test.db")).unwrap());
    let missing = "/nonexistent-subagent-spawn-cwd-check";
    let err = spawn_subagent(
        &db,
        "opencode",
        "prompt",
        None,
        missing,
        &[],
        15,
        60,
        None,
        None,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("does not exist") && err.contains(missing),
        "spawn must refuse before touching the registry, config, DB or process, got: {err}"
    );
}

#[tokio::test]
async fn spawn_subagent_blocking_refuses_a_missing_workdir_naming_the_path() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::new(&tmp.path().join("test.db")).unwrap());
    let missing = "/nonexistent-subagent-blocking-cwd-check";
    let err =
        match spawn_subagent_blocking(&db, "opencode", "prompt", None, missing, &[], 15, 60, None)
            .await
        {
            Ok(_) => panic!("blocking spawn must refuse the missing workdir, not spawn through it"),
            Err(e) => e.to_string(),
        };
    assert!(
        err.contains("does not exist") && err.contains(missing),
        "blocking spawn must refuse before touching the registry, config, DB or process, got: {err}"
    );
}
