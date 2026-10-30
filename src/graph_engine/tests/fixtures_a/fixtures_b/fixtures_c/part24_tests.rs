//! Part 24 of graph-engine `tests`: a run's workdir override survives resumes and hooks.
use super::*;

/// A run launched with a workdir override persists it, and a resume-mode
/// dispatch (what `graph_control`'s `retry_current_node` triggers via
/// `resume_background`) re-dispatches in the override — the check node's cwd
/// and its rendered `{{workdir}}` both read the override, not the stored
/// workdir. Without the resume fallback the resumed run silently expires
/// back to the stored workdir.
#[tokio::test]
async fn resume_keeps_run_workdir() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let stored = dir.path().to_string_lossy().to_string();
    let override_dir = tempdir().unwrap();
    let override_path = override_dir.path().to_string_lossy().to_string();
    let marker = dir.path().join("workdir-marker.txt");

    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": format!(
                "printf '%s\\n%s\\n' \"$(pwd)\" {{{{workdir}}}} > \"{}\"",
                marker.display()
            ),
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // Fresh dispatch with the override: it runs in the override and the
    // override is persisted with the run.
    engine
        .run_graph(
            graph_id.clone(),
            None,
            Some(override_path.clone()),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Completed
    );
    assert_eq!(
        db.graph_active_run_workdir(&graph_id).unwrap().as_deref(),
        Some(override_path.as_str()),
        "the fresh dispatch must persist the override it ran with"
    );
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        format!("{override_path}\n{override_path}\n"),
        "fresh dispatch: cwd and {{{{workdir}}}} must both be the override"
    );

    // Back to pending, then resume WITHOUT an override — the resumed nodes
    // must still run in the override.
    db.update_graph_spec_status(&spec_id, GraphSpecStatus::Pending, None, None)
        .unwrap();
    std::fs::remove_file(&marker).unwrap();
    engine
        .run_graph_dispatch(
            graph_id.clone(),
            None,
            None,
            DispatchMode::resume(),
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Completed
    );
    assert_eq!(
        db.graph_active_run_workdir(&graph_id).unwrap().as_deref(),
        Some(override_path.as_str()),
        "the resume must keep the persisted run workdir"
    );
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        format!("{override_path}\n{override_path}\n"),
        "resumed dispatch: cwd and {{{{workdir}}}} must both still be the override, not the stored workdir {stored}"
    );
}

/// An `on_blocked` command hook of a run launched with an override renders
/// `{{workdir}}` and `CANOPY_HOOK_WORKDIR` as the run's workdir — not the
/// stored one. Without the hook fix both read the stored workdir.
#[tokio::test]
async fn blocked_hook_renders_run_workdir() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let stored = dir.path().to_string_lossy().to_string();
    let override_dir = tempdir().unwrap();
    let override_path = override_dir.path().to_string_lossy().to_string();
    let hook_file = dir.path().join("hook-workdir.txt");

    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnBlocked,
        vec![crate::domain::graphs::GraphCompletionHook {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: Some(format!(
                "printf '%s\\n%s\\n' {{{{workdir}}}} \"$CANOPY_HOOK_WORKDIR\" > \"{}\"",
                hook_file.display()
            )),
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(1),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
            crew: None,
        }],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();
    db.set_graph_active_run_workdir(&graph_id, Some(&override_path))
        .unwrap();

    engine
        .fire_on_blocked_hooks(&graph_id, "needs human ruling", Some("node"))
        .await;

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Pass);
    assert_eq!(
        std::fs::read_to_string(&hook_file).unwrap(),
        format!("{override_path}\n{override_path}\n"),
        "{{{{workdir}}}} and CANOPY_HOOK_WORKDIR must both be the run workdir, not the stored workdir {stored}"
    );
}
