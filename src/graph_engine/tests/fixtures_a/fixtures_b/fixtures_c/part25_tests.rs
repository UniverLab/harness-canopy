//! Part 25 of graph-engine `tests`: per-spec hooks (`spec_ids`).
use super::*;

fn spec_command_hook(spec_ids: Option<Vec<String>>) -> GraphCompletionHook {
    GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some("true".to_string()),
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,
        crew: None,
        spec_ids,
    }
}

fn spec_ctx<'a>(
    lp: &'a Graph,
    completed: &'a [(String, String)],
    spec_id: Option<&'a str>,
) -> HookContext<'a> {
    HookContext {
        graph_name: &lp.name,
        workdir: &lp.workdir,
        completed_specs: completed,
        spec_name: None,
        spec_id,
        blocker: None,
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    }
}

/// A hook bound to spec A fires for A and leaves no run row for B, on
/// `on_spec_completed`.
#[tokio::test]
async fn spec_bound_hook_fires_for_a_not_b_on_spec_completed() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnSpecCompleted,
        vec![spec_command_hook(Some(vec![spec_id.clone()]))],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let completed = vec![("Spec".to_string(), "ok".to_string())];

    engine
        .fire_hooks(
            &lp,
            GraphHookEvent::OnSpecCompleted,
            &spec_ctx(&lp, &completed, Some("spec-other")),
        )
        .await;
    assert!(
        db.list_graph_completion_hook_runs(&graph_id)
            .unwrap()
            .is_empty(),
        "a skipped hook must leave no run row"
    );

    engine
        .fire_hooks(
            &lp,
            GraphHookEvent::OnSpecCompleted,
            &spec_ctx(&lp, &completed, Some(&spec_id)),
        )
        .await;
    let runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
    assert_eq!(runs[0].hook_index, 0);
}

/// A hook bound to spec A fires for A and leaves no run row for B, on
/// `on_failed`.
#[tokio::test]
async fn spec_bound_hook_fires_for_a_not_b_on_failed() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnFailed,
        vec![spec_command_hook(Some(vec![spec_id.clone()]))],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let completed = Vec::new();
    let other = HookContext {
        blocker: Some("broke"),
        node_name: Some("builder"),
        ..spec_ctx(&lp, &completed, Some("spec-other"))
    };
    engine
        .fire_hooks(&lp, GraphHookEvent::OnFailed, &other)
        .await;
    assert!(
        db.list_graph_completion_hook_runs(&graph_id)
            .unwrap()
            .is_empty(),
        "a skipped hook must leave no run row"
    );

    let matched = HookContext {
        blocker: Some("broke"),
        node_name: Some("builder"),
        ..spec_ctx(&lp, &completed, Some(&spec_id))
    };
    engine
        .fire_hooks(&lp, GraphHookEvent::OnFailed, &matched)
        .await;
    let runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
}

/// An unbound hook fires for every spec of the event.
#[tokio::test]
async fn unbound_hook_fires_for_both_specs() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnSpecCompleted,
        vec![spec_command_hook(None)],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let completed = vec![("Spec".to_string(), "ok".to_string())];
    engine
        .fire_hooks(
            &lp,
            GraphHookEvent::OnSpecCompleted,
            &spec_ctx(&lp, &completed, Some(&spec_id)),
        )
        .await;
    engine
        .fire_hooks(
            &lp,
            GraphHookEvent::OnSpecCompleted,
            &spec_ctx(&lp, &completed, Some("spec-other")),
        )
        .await;
    let runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|run| run.status == GraphRunStatus::Pass));
}

/// On a run failure with no current spec, only unbound hooks fire: the
/// bound hook is skipped before its run row, so the single run row carries
/// the unbound hook's index.
#[tokio::test]
async fn on_failed_without_spec_fires_only_unbound_hooks() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnFailed,
        vec![
            spec_command_hook(Some(vec![spec_id.clone()])),
            spec_command_hook(None),
        ],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let completed = Vec::new();
    let ctx = HookContext {
        blocker: Some("broke"),
        node_name: Some("builder"),
        ..spec_ctx(&lp, &completed, None)
    };
    engine.fire_hooks(&lp, GraphHookEvent::OnFailed, &ctx).await;
    let runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].hook_index, 1);
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
}
