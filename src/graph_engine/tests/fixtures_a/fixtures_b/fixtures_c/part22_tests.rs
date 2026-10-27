//! Part 22 of graph-engine `tests`: the reporting nudge — an agent attempt
//! that exits 0 with output but never files a verdict is re-dispatched once,
//! with an explicit reporting instruction, before the node fails.
use super::*;

/// `argv_cli_config` clone named `nudge-cli`: no resume support, so a
/// nudged retry cold-starts and the prompt (nudge section included) arrives
/// via argv, where the test script dumps it.
fn nudge_cli_config(
    binary: &std::path::Path,
    env: HashMap<String, String>,
) -> crate::domain::cli_config::CliConfig {
    let mut cli = argv_cli_config(binary, env, None, None, None);
    cli.name = "nudge-cli".to_string();
    cli
}

/// Writes the nudge-test CLI at `dir/name`: dumps `$@` to `$ARGV_FILE` (when
/// set), counts invocations in `counter`, then exits 0 printing a promise —
/// filing `status` via [`self_report_invocation`] first once the invocation
/// count reaches `report_from` (`None` = never reports). Returns the
/// script's path as a string, like [`write_member_script`].
fn write_nudge_cli(
    dir: &std::path::Path,
    name: &str,
    counter: &std::path::Path,
    db_path: &std::path::Path,
    node_id: &str,
    report_from: Option<u32>,
    status: &str,
) -> String {
    let report = match report_from {
        Some(from) => format!(
            "if [ \"$n\" -ge {from} ]; then\n{}fi\n",
            self_report_invocation(db_path, node_id, status, None),
        ),
        None => String::new(),
    };
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ -n \"$ARGV_FILE\" ]; then\n  {{\n    for a in \"$@\"; do printf '%s\\n' \"$a\"; done\n    printf '===\\n'\n  }} >> \"$ARGV_FILE\"\nfi\nn=$(cat \"{c}\" 2>/dev/null || echo 0); n=$((n+1)); echo \"$n\" > \"{c}\"\n{report}echo \"I'll call the graph completion node.\"\nexit 0\n",
            c = counter.display(),
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.to_string_lossy().to_string()
}

/// Runs one agent node (`node-impl` on `nudge-cli`, backoff 0, no edges)
/// through `run_graph`. Returns the fixture dir (kept alive), the db, the
/// spec id, and the argv dump path.
async fn run_single_nudge_node(
    report_from: Option<u32>,
    status: &str,
) -> (TempDir, Arc<Database>, String, std::path::PathBuf) {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let argv_file = dir.path().join("argv.log");
    let counter = dir.path().join("nudge.counter");
    let script = write_nudge_cli(
        dir.path(),
        "nudge-cli",
        &counter,
        &db_path,
        "node-impl",
        report_from,
        status,
    );
    let mut env = HashMap::new();
    env.insert(
        "ARGV_FILE".to_string(),
        argv_file.to_string_lossy().into_owned(),
    );
    let cli = nudge_cli_config(std::path::Path::new(&script), env);
    let home = write_resume_cli_home(cli);
    db.insert_graph_node(&GraphNode {
        id: "node-impl".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "impl".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({ "platform": "nudge-cli", "infra_backoff_seconds": 0 }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    let _guard = HomeGuard::set(home.path());
    engine
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();
    drop(_guard);
    (dir, db, spec_id, argv_file)
}

/// The nudge path end to end: the first attempt exits 0 promising to report
/// but never does, so it is re-dispatched once with the nudge section — and
/// the verdict reported on the retry passes the node.
#[tokio::test]
async fn nudge_retry_passes_when_agent_reports() {
    let (_dir, db, spec_id, argv_file) = run_single_nudge_node(Some(2), "pass").await;
    let runs = member_runs(&db, &spec_id, "node-impl");
    assert_eq!(
        runs.len(),
        2,
        "an unreported exit is nudged exactly once, then the agent reports"
    );
    assert_eq!(runs[0].status, GraphRunStatus::Fail);
    assert_eq!(
        runs[0].output.as_ref().unwrap()["infra_crash"],
        serde_json::Value::Bool(true),
        "the nudged-away attempt keeps the infra marker"
    );
    assert_eq!(runs[0].output.as_ref().unwrap()["infra_attempt"], 0);
    assert_eq!(runs[1].status, GraphRunStatus::Pass);
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().status,
        GraphSpecStatus::Completed
    );

    let argv = std::fs::read_to_string(&argv_file).unwrap();
    let chunks: Vec<&str> = argv.split("===\n").collect();
    assert_eq!(
        chunks.len(),
        3,
        "two invocations + trailing empty split segment"
    );
    assert!(
        !chunks[0].contains(REPORT_NUDGE_TAG),
        "the first attempt is never nudged"
    );
    assert!(
        chunks[1].contains(REPORT_NUDGE_TAG),
        "the retry prompt carries the reporting nudge: {}",
        chunks[1]
    );
}

/// A fail reported on the nudge retry is honoured as a genuine verdict —
/// not stamped with the unreported kind the first attempt carried.
#[tokio::test]
async fn nudge_retry_honours_reported_fail() {
    let (_dir, db, spec_id, _argv) = run_single_nudge_node(Some(2), "fail").await;
    let runs = member_runs(&db, &spec_id, "node-impl");
    assert_eq!(runs.len(), 2);
    assert_eq!(
        runs[0].output.as_ref().unwrap()["failure_kind"],
        "unreported",
        "the first attempt is the unreported shape that earns the nudge"
    );
    assert_eq!(runs[1].status, GraphRunStatus::Fail);
    assert!(
        runs[1]
            .output
            .as_ref()
            .unwrap()
            .get("failure_kind")
            .is_none(),
        "a reported fail is a genuine verdict, not an unreported one: {:?}",
        runs[1].output
    );
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().status,
        GraphSpecStatus::Failed
    );
}

/// Two unreported exits fail the node with both attempts visible: the first
/// carries the retry marker, the settled second carries none.
#[tokio::test]
async fn unreported_twice_fails_with_two_attempts() {
    let (_dir, db, spec_id, _argv) = run_single_nudge_node(None, "pass").await;
    let runs = member_runs(&db, &spec_id, "node-impl");
    assert_eq!(
        runs.len(),
        2,
        "the nudge fires once; the second unreported exit settles the node"
    );
    for run in &runs {
        assert_eq!(run.status, GraphRunStatus::Fail);
        assert_eq!(
            run.output.as_ref().unwrap()["failure_kind"],
            "unreported",
            "both attempts are visible as unreported exits"
        );
    }
    assert!(
        runs[1]
            .output
            .as_ref()
            .unwrap()
            .get("infra_crash")
            .is_none(),
        "the settled attempt carries no retry marker: {:?}",
        runs[1].output
    );
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().status,
        GraphSpecStatus::Failed
    );
}

/// An agent that reports on its first attempt is never retried and never
/// sees the nudge tag.
#[tokio::test]
async fn agent_reporting_first_time_is_never_nudged() {
    let (_dir, db, spec_id, argv_file) = run_single_nudge_node(Some(1), "pass").await;
    let runs = member_runs(&db, &spec_id, "node-impl");
    assert_eq!(runs.len(), 1, "a reported verdict settles immediately");
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(
        !argv.contains(REPORT_NUDGE_TAG),
        "no retry means no nudge tag anywhere in the prompt: {argv}"
    );
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().status,
        GraphSpecStatus::Completed
    );
}

/// An ensemble member that ends unreported retries in place: `m-report`
/// reports on its nudge retry and wins the cascade without consuming
/// `m-backup` at all.
#[tokio::test]
async fn ensemble_member_is_nudged_in_place() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let counter = dir.path().join("m-report.counter");
    let report_script = write_nudge_cli(
        dir.path(),
        "report.sh",
        &counter,
        &db_path,
        "m-report",
        Some(2),
        "pass",
    );
    let backup_script = write_member_script(dir.path(), "backup.sh", "printf backup");
    let fake_home = setup_multi_cli_home(&[
        ("nudge-report", report_script.as_str()),
        ("backup", backup_script.as_str()),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Cascade,
        &[("m-report", "nudge-report"), ("m-backup", "backup")],
    );

    let _guard = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();
    drop(_guard);

    assert_eq!(
        member_runs(&db, &spec_id, "m-report").len(),
        2,
        "the member retries in place on its nudge"
    );
    assert!(
        member_runs(&db, &spec_id, "m-backup").is_empty(),
        "no extra ensemble member is consumed by the nudge"
    );
    assert_eq!(
        join_run(&db, &spec_id, "join1").status,
        GraphRunStatus::Pass,
        "the verdict reported on the retry wins the cascade"
    );
}
