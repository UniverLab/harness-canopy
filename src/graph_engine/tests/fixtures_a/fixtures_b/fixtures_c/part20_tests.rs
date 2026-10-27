//! Part 20 of graph-engine `tests`: idea-driven graph resumes — the stored
//! idea is what a `run_from_node` / plain resume picks back up (FR1-FR4).
use super::*;

/// Argv-echo CLI whose own verdict flips by visit: the first invocation
/// files `fail`, every later one `pass` (counted per `$COUNT_FILE`). That is
/// what leaves a *failed* idea graph after run 1 and lets the resume visit
/// complete it, with both visits recorded to the same `$ARGV_FILE` for
/// prompt assertions.
fn write_fail_once_cli(dir: &std::path::Path) -> std::path::PathBuf {
    let script = dir.join("fail-once-cli");
    std::fs::write(
        &script,
        r#"#!/bin/sh
{
  for a in "$@"; do printf '%s\n' "$a"; done
  printf '===\n'
} >> "$ARGV_FILE"
COUNT=$(cat "$COUNT_FILE" 2>/dev/null || echo 0)
COUNT=$((COUNT + 1))
printf '%s\n' "$COUNT" > "$COUNT_FILE"
STATUS=pass
[ "$COUNT" -eq 1 ] && STATUS=fail
if [ -n "$SELF_REPORT_EXE" ]; then
  CANOPY_TEST_SELF_REPORT_DB="$SELF_REPORT_DB" \
  CANOPY_TEST_SELF_REPORT_NODE_ID="$SELF_REPORT_NODE_ID" \
  CANOPY_TEST_SELF_REPORT_STATUS="$STATUS" \
  CANOPY_TEST_SELF_REPORT_STDOUT="B visit $COUNT" \
    "$SELF_REPORT_EXE" --exact graph_engine::tests::fixtures_a::fixtures_b::fixtures_c::part10_tests::self_report_verdict_writer --nocapture >/dev/null
fi
echo done
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// The idea-graph fixture the FR1/FR4 tests share: a spec-less graph whose
/// top-level graph is `A --pass--> B` — node `A` a check that prints
/// `A output`, node `B` an argv-echo agent that fails its first visit.
/// Returns `(dir, db, engine, graph_id, argv_file, home)`; the caller holds
/// the `HomeGuard` for every dispatch.
fn fail_once_idea_fixture() -> (
    tempfile::TempDir,
    Arc<Database>,
    GraphEngine,
    String,
    std::path::PathBuf,
    tempfile::TempDir,
) {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let argv_file = dir.path().join("argv.log");
    let script = write_fail_once_cli(dir.path());
    let mut env = HashMap::new();
    env.insert(
        "ARGV_FILE".to_string(),
        argv_file.to_string_lossy().into_owned(),
    );
    env.insert(
        "COUNT_FILE".to_string(),
        dir.path()
            .join("visit-count")
            .to_string_lossy()
            .into_owned(),
    );
    env.insert("SELF_REPORT_NODE_ID".to_string(), "node-b".to_string());
    env.insert("SELF_REPORT_EXE".to_string(), self_report_exe());
    env.insert(
        "SELF_REPORT_DB".to_string(),
        dir.path()
            .join(TEST_DB_FILENAME)
            .to_string_lossy()
            .into_owned(),
    );
    let cli = argv_cli_config(&script, env, None, None, None);
    let home = write_resume_cli_home(cli);

    db.insert_graph_node(&GraphNode {
        id: "node-a".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "A".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf 'A output'",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-b".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "B".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({ "platform": "resume-cli" }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-a-b".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        from_node: "node-a".to_string(),
        to_node: "node-b".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();

    (dir, db, engine, graph_id, argv_file, home)
}

/// FR1: `run_from_node` on a failed idea graph resumes THAT run from the
/// named node — the failed placeholder's spec id is kept (never purged),
/// the named node gets a new run, and `{{spec_content}}` on it renders the
/// stored idea. Also covers FR2 (the spec-less guard passes a stored idea)
/// and the FR1 storage itself.
#[tokio::test]
async fn run_from_node_on_failed_idea_graph_runs_named_node_with_stored_idea() {
    let (_dir, db, engine, graph_id, argv_file, home) = fail_once_idea_fixture();
    let guard = HomeGuard::set(home.path());

    // Run 1 (fresh, with the idea): A passes, B fails its first visit.
    let _ = engine
        .run_graph(
            graph_id.clone(),
            None,
            None,
            Some("IDEA TEXT".to_string()),
            None,
            None,
        )
        .await;

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Failed,
        "the first run must leave a failed idea graph"
    );
    let bound = db.list_graph_specs(&graph_id).unwrap();
    assert_eq!(bound.len(), 1, "one blank-name placeholder: {bound:?}");
    let placeholder = &bound[0];
    assert!(placeholder.name.trim().is_empty());
    assert_eq!(
        placeholder.description.as_deref(),
        Some("IDEA TEXT"),
        "the placeholder carries the idea as {{spec_content}}"
    );
    assert_eq!(placeholder.status, GraphSpecStatus::Failed);
    assert_eq!(
        db.graph_active_run_idea(&graph_id).unwrap().as_deref(),
        Some("IDEA TEXT"),
        "FR1: the run's idea is stored on the graph row"
    );
    let runs_before = db.list_graph_runs_for_spec(&placeholder.id).unwrap();
    assert!(
        runs_before.iter().any(|run| run.node_id == "node-a"),
        "the failed run has a run on A: {runs_before:?}"
    );
    let spec_id = placeholder.id.clone();

    // The directed resume: run from node B with the stored idea.
    engine
        .run_graph_dispatch(
            graph_id.clone(),
            None,
            None,
            DispatchMode::resume_from(DirectedStart {
                spec_id: spec_id.clone(),
                node_arg: "B".to_string(),
            }),
            None,
            None,
        )
        .await
        .unwrap();
    drop(guard);

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);
    let specs = db.list_graph_specs(&graph_id).unwrap();
    assert_eq!(
        specs.len(),
        1,
        "the resume must not purge or recreate the placeholder: {specs:?}"
    );
    assert_eq!(
        specs[0].id, spec_id,
        "the failed placeholder's spec id must be kept"
    );
    assert_eq!(specs[0].status, GraphSpecStatus::Completed);
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let b_runs: Vec<_> = runs.iter().filter(|run| run.node_id == "node-b").collect();
    assert!(
        b_runs.len() >= 2,
        "the named node must get a NEW run on top of the failed visit: {runs:?}"
    );
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(
        argv.contains("IDEA TEXT"),
        "the resumed node's prompt must render {{spec_content}} from the stored idea: {argv}"
    );
}

/// FR1/FR4 plumbing for an idea graph whose specs are gone: a plain resume
/// recreates the blank-name placeholder FROM the stored idea (the check node
/// only passes when `$CANOPY_SPEC_FILE` holds that idea) and the graph runs
/// to completion over it.
#[tokio::test]
async fn resume_recreates_placeholder_from_stored_idea_when_specs_are_gone() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "Check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": r#"test -s "$CANOPY_SPEC_FILE" && grep -q "IDEA TEXT" "$CANOPY_SPEC_FILE""#,
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.set_graph_active_run_idea(&graph_id, Some("IDEA TEXT"))
        .unwrap();

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

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Completed,
        "the check reading $CANOPY_SPEC_FILE must have found the stored idea"
    );
    let specs = db.list_graph_specs(&graph_id).unwrap();
    assert_eq!(
        specs.len(),
        1,
        "the placeholder must be recreated for the stored idea: {specs:?}"
    );
    assert!(specs[0].name.trim().is_empty());
    assert_eq!(specs[0].description.as_deref(), Some("IDEA TEXT"));
    assert_eq!(specs[0].status, GraphSpecStatus::Completed);
}

/// FR5: a graph with a named bound spec is spec-driven even when a stale
/// idea sits on its row — it is not an idea graph, and a resume never
/// inherits the stored idea (today's spec-driven behaviour, unchanged).
#[tokio::test]
async fn named_spec_graph_with_stored_idea_is_not_an_idea_graph() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    db.insert_graph_spec(&second_spec(&graph_id, "sid-named", 1))
        .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "Check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "true", "success_condition": "exit_code_0" }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.set_graph_active_run_idea(&graph_id, Some("STALE IDEA"))
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert!(
        !engine.is_idea_graph(&lp).unwrap(),
        "a named bound spec makes the graph spec-driven, queue or not"
    );
    assert!(
        engine.effective_idea(&lp, None, true).unwrap().is_none(),
        "FR5: a spec-driven graph's resume must not fall back to the stored idea"
    );
}

/// FR4: `{{previous_feedback}}` on the resumed node of a failed idea run
/// carries the feedback of the failed run (node A's output), exactly as on a
/// spec-driven graph — while `{{spec_content}}` still renders the stored
/// idea. The last argv chunk is the resumed visit of agent B.
#[tokio::test]
async fn resumed_idea_run_carries_previous_feedback_and_stored_idea_to_agent() {
    let (_dir, db, engine, graph_id, argv_file, home) = fail_once_idea_fixture();
    let guard = HomeGuard::set(home.path());

    // Run 1: A passes with "A output", B fails — a failed idea run whose
    // feedback into B was A's output.
    let _ = engine
        .run_graph(
            graph_id.clone(),
            None,
            None,
            Some("IDEA TEXT".to_string()),
            None,
            None,
        )
        .await;
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed);
    let placeholder = db.list_graph_specs(&graph_id).unwrap()[0].clone();

    // The resume (graph_continue's non-directed path on a failed graph).
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
    drop(guard);

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    let visits: Vec<&str> = argv
        .split("===\n")
        .filter(|chunk| !chunk.trim().is_empty())
        .collect();
    let resumed = visits
        .last()
        .unwrap_or_else(|| panic!("agent B must have been invoked: {argv}"));
    assert!(
        resumed.contains("IDEA TEXT"),
        "the resumed node must render {{spec_content}} from the stored idea: {resumed}"
    );
    assert!(
        resumed.contains("A output"),
        "the resumed node must carry the feedback of the failed run as \
         {{previous_feedback}}: {resumed}"
    );
    assert_eq!(
        placeholder.description.as_deref(),
        Some("IDEA TEXT"),
        "the failed placeholder keeps the idea text between runs"
    );
}

/// FR1/FR3 for graphs that ran BEFORE the row carried the idea (the column
/// migrates as NULL — the motivating canopy-test-file-cap case): such a
/// graph keeps its idea only on the blank bookkeeping spec's description.
/// The resume must treat that as the stored idea — not refuse the graph as
/// "never run" — and the dispatch must self-heal the NULL row from it.
#[tokio::test]
async fn run_from_node_resumes_a_pre_migration_idea_graph_whose_row_idea_is_null() {
    let (_dir, db, engine, graph_id, argv_file, home) = fail_once_idea_fixture();
    let guard = HomeGuard::set(home.path());

    let _ = engine
        .run_graph(
            graph_id.clone(),
            None,
            None,
            Some("IDEA TEXT".to_string()),
            None,
            None,
        )
        .await;
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed);
    // Simulate a graph whose idea run predates the column: NULL on the row,
    // the idea surviving only on the bookkeeping spec's description.
    db.set_graph_active_run_idea(&graph_id, None).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert!(
        engine.idea_resume_ready(&lp).unwrap().is_none(),
        "a failed idea graph whose row idea is NULL but whose bookkeeping \
         spec carries the idea must be resumable, not refused as never-run"
    );

    let placeholder = db.list_graph_specs(&graph_id).unwrap()[0].clone();
    engine
        .run_graph_dispatch(
            graph_id.clone(),
            None,
            None,
            DispatchMode::resume_from(DirectedStart {
                spec_id: placeholder.id.clone(),
                node_arg: "B".to_string(),
            }),
            None,
            None,
        )
        .await
        .unwrap();
    drop(guard);

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Completed,
        "the pre-migration idea graph must resume from the named node"
    );
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(
        argv.contains("IDEA TEXT"),
        "the resumed node must render the bookkeeping spec's idea: {argv}"
    );
    assert_eq!(
        db.graph_active_run_idea(&graph_id).unwrap().as_deref(),
        Some("IDEA TEXT"),
        "the resume must self-heal the NULL row from the bookkeeping spec"
    );
}

/// FR5: the resume's placeholder-keep is scoped to IDEA graphs — a
/// spec-driven graph that carries a stale blank bookkeeping row keeps
/// today's behaviour: a resume purges that terminal debris instead of
/// carrying it along.
#[tokio::test]
async fn resume_of_a_spec_driven_graph_purges_a_stale_blank_placeholder() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let named = second_spec(&graph_id, "sid-named", 1);
    db.insert_graph_spec(&named).unwrap();
    db.update_graph_spec_status(&named.id, GraphSpecStatus::Completed, None, None)
        .unwrap();
    // Terminal blank bookkeeping debris left on a graph that is now
    // spec-driven: hidden from listings, and (like before this feature)
    // purged by the next resume rather than carried along.
    let mut stale = crate::graph_engine::no_spec_placeholder(&graph_id);
    stale.status = GraphSpecStatus::Completed;
    db.insert_graph_spec(&stale).unwrap();
    db.update_graph_status(&graph_id, GraphStatus::Failed, None, None)
        .unwrap();

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

    let specs = db.list_graph_specs(&graph_id).unwrap();
    assert_eq!(
        specs.len(),
        1,
        "the stale blank bookkeeping row must be purged on a spec-driven \
         resume, not carried along: {specs:?}"
    );
    assert_eq!(specs[0].id, named.id);
}
