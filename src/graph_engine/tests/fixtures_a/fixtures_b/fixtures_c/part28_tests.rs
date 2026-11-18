//! Part 28 of graph-engine `tests`: the commit-subject guard on commit
//! nodes. A check or agent node that would commit with a malformed prebuilt
//! message (a hand-corrupted stored subject) fails with
//! `failure_kind: "commit_subject_invalid"` and commits nothing — including an
//! agent committer and an ensemble member of a committing ensemble; every
//! well-formed shape passes through instead.
use super::*;
use crate::graph_engine::commit_guard::commit_subject_invalid_execution;

fn commit_guard_spec(db: &Database, graph_id: &str, name: &str) -> String {
    let mut spec = standalone_spec("spec-commit-guard", 1);
    spec.graph_id = Some(graph_id.to_string());
    spec.name = name.to_string();
    db.insert_graph_spec(&spec).unwrap();
    spec.id
}

fn commit_guard_node(db: &Database, graph_id: &str, rights: bool) {
    db.insert_graph_node(&GraphNode {
        id: "commit-node".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        name: "commit".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "git commit -q --allow-empty -m \"$CANOPY_SPEC_COMMIT_MESSAGE\"",
            "success_condition": "exit_code_0",
            "commit_rights": rights,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
}

async fn corrupting_guard_run(
    _engine: &GraphEngine,
    db: &Database,
    graph_id: &str,
    spec_id: &str,
    subject: &str,
    rights: bool,
) -> NodeExecution {
    let mut spec = db.get_graph_spec(spec_id).unwrap().unwrap();
    spec.name = subject.to_string();
    spec.spec_type = Some(SpecType::Fix);
    spec.spec_scope = Some("tui".to_string());
    let node = GraphNode {
        id: "commit-node".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        name: "commit".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "git commit -q --allow-empty -m \"$CANOPY_SPEC_COMMIT_MESSAGE\"",
            "success_condition": "exit_code_0",
            "commit_rights": rights,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    let lp = db.get_graph(graph_id).unwrap().unwrap();
    let workdir = lp.workdir.clone();
    execute_check_node(db, "run-guard", &lp, &spec, &node, None, None, &workdir)
        .await
        .unwrap()
}

/// A corrupted stored subject fails the commit node with
/// `commit_subject_invalid` and commits nothing.
#[tokio::test]
async fn corrupt_subject_fails_commit_node_and_commits_nothing() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    std::fs::write(
        dir.path().join(".gitignore"),
        format!("{TEST_DB_FILENAME}*\n"),
    )
    .unwrap();
    init_git_repo(dir.path());
    commit_guard_node(&db, &graph_id, true);
    let spec_id = commit_guard_spec(&db, &graph_id, "clean subject");
    let before = git_head(dir.path());

    let execution = corrupting_guard_run(
        &engine,
        &db,
        &graph_id,
        &spec_id,
        "fix(tui): fix(tui): open F1",
        true,
    )
    .await;
    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("commit_subject_invalid"),
        "{}",
        execution.output
    );
    assert_eq!(git_head(dir.path()), before, "nothing was committed");
}

/// A check command that consumes the prebuilt message is a commit node even
/// without `commit_rights: true`: it can hand the message straight to git, so
/// a corrupted subject fails it before the shell runs.
#[tokio::test]
async fn corrupt_subject_fails_message_consuming_check_without_rights() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    std::fs::write(
        dir.path().join(".gitignore"),
        format!("{TEST_DB_FILENAME}*\n"),
    )
    .unwrap();
    init_git_repo(dir.path());
    commit_guard_node(&db, &graph_id, false);
    let spec_id = commit_guard_spec(&db, &graph_id, "clean subject");
    let before = git_head(dir.path());

    let execution = corrupting_guard_run(
        &engine,
        &db,
        &graph_id,
        &spec_id,
        "fix(tui): fix(tui): open F1",
        false,
    )
    .await;
    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("commit_subject_invalid"),
        "{}",
        execution.output
    );
    assert_eq!(git_head(dir.path()), before, "nothing was committed");
}

/// Well-formed messages — with scope, without scope, and a subject carrying
/// `!` — pass through to the shell and commit.
#[tokio::test]
async fn well_formed_messages_commit() {
    for (scope, name, rights) in [
        (Some("tui"), "open F1", true),
        (None, "add a feature", false),
        (Some("tui"), "ship v2! now", true),
    ] {
        let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
        std::fs::write(
            dir.path().join(".gitignore"),
            format!("{TEST_DB_FILENAME}*\n"),
        )
        .unwrap();
        init_git_repo(dir.path());
        commit_guard_node(&db, &graph_id, rights);
        let mut spec = standalone_spec("spec-commit-ok", 1);
        spec.graph_id = Some(graph_id.clone());
        spec.name = name.to_string();
        spec.spec_type = Some(SpecType::Fix);
        spec.spec_scope = scope.map(str::to_string);
        db.insert_graph_spec(&spec).unwrap();
        let lp = db.get_graph(&graph_id).unwrap().unwrap();
        let node = db.get_graph_node("commit-node").unwrap().unwrap();
        let before = git_head(dir.path());
        let execution = execute_check_node(
            &db,
            "run-ok",
            &lp,
            &spec,
            &node,
            None,
            None,
            lp.workdir.as_str(),
        )
        .await
        .unwrap();
        assert_eq!(
            execution.status,
            GraphRunStatus::Pass,
            "{}",
            execution.output
        );
        assert_ne!(git_head(dir.path()), before, "the commit landed");
        let _ = &engine;
    }
}

/// A committing agent with a corrupted stored subject never spawns: the
/// guard fails before any CLI runs, so no agent process can commit the bad
/// message. The marker CLI would touch its file on any spawn.
#[tokio::test]
async fn corrupt_subject_fails_committing_agent_before_spawn() {
    let (dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let marker = dir.path().join("spawned.marker");
    let script = dir.path().join("marker-cli.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\necho done\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = write_resume_cli_home(crate::domain::cli_config::CliConfig {
        name: "marker-cli".to_string(),
        binary: script.to_string_lossy().into_owned(),
        headless_mode: "run".to_string(),
        prompt_via_stdin: true,
        ..Default::default()
    });
    let _guard = HomeGuard::set(home.path());

    let mut spec = standalone_spec("spec-agent-guard", 1);
    spec.graph_id = Some(graph_id.clone());
    spec.name = "fix(tui): fix(tui): open F1".to_string();
    spec.spec_type = Some(SpecType::Fix);
    spec.spec_scope = Some("tui".to_string());
    db.insert_graph_spec(&spec).unwrap();
    let node = GraphNode {
        id: "reviewer".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "reviewer".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "marker-cli",
            "prompt_template": "commit with {{spec_commit_message}}",
            "commit_rights": true,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&crate::domain::graphs::GraphNodeRun {
        id: "run-agent-guard".to_string(),
        graph_id: graph_id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();

    let execution = execute_agent_node(
        &db,
        &db.get_graph(&graph_id).unwrap().unwrap(),
        &spec,
        &node,
        None,
        "run-agent-guard",
        dir.path().to_str().unwrap(),
        None,
        false,
        false,
        None,
        &HashMap::new(),
        &[],
        true,
    )
    .await
    .unwrap();
    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("commit_subject_invalid"),
        "{}",
        execution.output
    );
    assert!(
        !marker.exists(),
        "the CLI must never have spawned, so nothing could commit"
    );
}

/// The same corrupted subject on a non-committing agent is not the guard's
/// business: the node proceeds past it (to whatever the harness does).
#[tokio::test]
async fn corrupt_subject_passes_through_for_non_committing_agent() {
    let (dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let marker = dir.path().join("spawned.marker");
    let script = dir.path().join("marker-cli.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\necho done\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = write_resume_cli_home(crate::domain::cli_config::CliConfig {
        name: "marker-cli".to_string(),
        binary: script.to_string_lossy().into_owned(),
        headless_mode: "run".to_string(),
        prompt_via_stdin: true,
        ..Default::default()
    });
    let _guard = HomeGuard::set(home.path());

    let mut spec = standalone_spec("spec-agent-plain", 1);
    spec.graph_id = Some(graph_id.clone());
    spec.name = "fix(tui): fix(tui): open F1".to_string();
    spec.spec_type = Some(SpecType::Fix);
    spec.spec_scope = Some("tui".to_string());
    db.insert_graph_spec(&spec).unwrap();
    let node = GraphNode {
        id: "impl".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "impl".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "marker-cli",
            "prompt_template": "work",
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&crate::domain::graphs::GraphNodeRun {
        id: "run-agent-plain".to_string(),
        graph_id: graph_id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();

    let execution = execute_agent_node(
        &db,
        &db.get_graph(&graph_id).unwrap().unwrap(),
        &spec,
        &node,
        None,
        "run-agent-plain",
        dir.path().to_str().unwrap(),
        None,
        false,
        false,
        None,
        &HashMap::new(),
        &[],
        false,
    )
    .await
    .unwrap();
    assert!(
        execution.output.get("failure_kind").and_then(Value::as_str)
            != Some("commit_subject_invalid"),
        "{}",
        execution.output
    );
    assert!(
        marker.exists(),
        "without commit rights the agent spawns as usual"
    );
}

/// A member of a committing ensemble is guarded too: the member run fails
/// with `commit_subject_invalid` and files a verdict (no infra retry).
#[tokio::test]
async fn corrupt_subject_fails_committing_ensemble_member() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let mut spec = standalone_spec("spec-member-guard", 1);
    spec.graph_id = Some(graph_id.clone());
    spec.name = "fix(tui): fix(tui): open F1".to_string();
    spec.spec_type = Some(SpecType::Fix);
    spec.spec_scope = Some("tui".to_string());
    db.insert_graph_spec(&spec).unwrap();
    let node = GraphNode {
        id: "member-1".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "member-1".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({ "platform": "member-cli" }),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node).unwrap();
    let lp = db.get_graph(&graph_id).unwrap().unwrap();

    let (execution, had_no_verdict) = engine
        .run_ensemble_member(
            &lp,
            &spec,
            &node,
            None,
            1,
            dir.path().to_str().unwrap(),
            None,
            5,
            &HashMap::new(),
            &[],
            None,
            &mut HashMap::new(),
            &[],
            true,
        )
        .await
        .unwrap();
    assert!(
        !had_no_verdict,
        "the guard files a verdict, never a no-verdict"
    );
    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("commit_subject_invalid"),
        "{}",
        execution.output
    );
}

/// A member whose node config itself declares `commit_rights` is guarded even
/// when the ensemble's flag is off — the ensemble paths pass only the
/// ensemble's flag, so the guard must read the node's own too.
#[tokio::test]
async fn corrupt_subject_fails_member_with_own_commit_rights() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let mut spec = standalone_spec("spec-member-own-rights", 1);
    spec.graph_id = Some(graph_id.clone());
    spec.name = "fix(tui): fix(tui): open F1".to_string();
    spec.spec_type = Some(SpecType::Fix);
    spec.spec_scope = Some("tui".to_string());
    db.insert_graph_spec(&spec).unwrap();
    let node = GraphNode {
        id: "member-own".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "member-own".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({ "platform": "member-cli", "commit_rights": true }),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node).unwrap();
    let lp = db.get_graph(&graph_id).unwrap().unwrap();

    let (execution, _) = engine
        .run_ensemble_member(
            &lp,
            &spec,
            &node,
            None,
            1,
            dir.path().to_str().unwrap(),
            None,
            5,
            &HashMap::new(),
            &[],
            None,
            &mut HashMap::new(),
            &[],
            false,
        )
        .await
        .unwrap();
    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("commit_subject_invalid"),
        "{}",
        execution.output
    );
}

/// The guard execution itself carries the invalid message, the corrected
/// rule, and the deterministic marker the retry machinery settles on.
#[test]
fn guard_execution_shape_names_message_and_rule() {
    let node = GraphNode {
        id: "n".to_string(),
        spec_id: None,
        graph_id: None,
        name: "reviewer".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    let execution = commit_subject_invalid_execution(
        &node,
        "fix(tui): fix(tui): open F1",
        "spec subject must not itself begin with a Conventional Commits header (repeated prefix): use 'open F1' instead.",
    );
    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("commit_subject_invalid")
    );
    assert_eq!(
        execution.output.get("message").and_then(Value::as_str),
        Some("fix(tui): fix(tui): open F1")
    );
    assert!(
        execution
            .output
            .get("spawn_permanent")
            .and_then(Value::as_bool)
            == Some(true),
        "deterministic so infra retries settle: {}",
        execution.output
    );
}
