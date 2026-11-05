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
