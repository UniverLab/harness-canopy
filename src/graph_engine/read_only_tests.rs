//! Behavioural tests for read-only agent nodes: engine enforcement, ensemble
//! inheritance, validation refusals and the fingerprint's cost budget.

use std::path::Path;
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::tempdir;

use super::read_only::{
    apply_to_execution, begin_for_node, begin_for_run, capture_fingerprint, is_excluded,
    member_is_read_only, node_has_read_only, read_only_failure, PathChange,
};
use super::NodeExecution;
use crate::domain::graphs::{
    Ensemble, EnsembleDetails, EnsembleKind, EnsembleMember, GraphEdgeCondition, GraphNode,
    GraphNodeKind, GraphRunStatus,
};

fn git(dir: &Path, args: &[&str]) {
    let status = StdCommand::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {:?} failed", args);
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "test@test"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("tracked.txt"), "base\n").unwrap();
    git(dir, &["add", "tracked.txt"]);
    git(
        dir,
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"],
    );
}

fn graph_node(id: &str, config: Value) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: format!("node-{id}"),
        kind: GraphNodeKind::Agent,
        config,
        position: 0,
        created_at: chrono::Utc::now(),
    }
}

fn pass_execution() -> NodeExecution {
    NodeExecution {
        status: GraphRunStatus::Pass,
        output: json!({"verdict": "pass"}),
        summary: "ok".to_string(),
    }
}

fn ensemble_details(read_only: bool, commit_rights: bool, members: &[&str]) -> EnsembleDetails {
    let ensemble = Ensemble {
        mutex: Vec::new(),
        commit_rights,
        read_only,
        id: "ens-1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "Reviewers".to_string(),
        prompt_template: "review".to_string(),
        join_node_id: "join".to_string(),
        entry_from_node: "entry".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: members.len() as i64,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 10,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "done".to_string(),
        on_fail_to: None,
        kind: EnsembleKind::Parallel,
        round_robin_index: None,
        created_at: chrono::Utc::now(),
        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let members = members
        .iter()
        .enumerate()
        .map(|(i, id)| EnsembleMember {
            ensemble_id: ensemble.id.clone(),
            node_id: id.to_string(),
            position: i as i64,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        })
        .collect();
    EnsembleDetails { ensemble, members }
}

#[tokio::test]
async fn untouched_read_only_node_passes_on_dirty_tree() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("tracked.txt"), "already dirty\n").unwrap();
    std::fs::write(dir.path().join("scratch.txt"), "untracked\n").unwrap();
    let workdir = dir.path().to_str().unwrap();
    let node = graph_node("r1", json!({"read_only": true}));

    let (watch, unenforced) = begin_for_run(true, &node, workdir).await.unwrap();
    assert!(
        watch.is_some(),
        "a read-only node in a git workdir must be enforced"
    );
    assert!(!unenforced);

    let execution = apply_to_execution(
        &node,
        "Node 'node-r1'",
        workdir,
        watch.as_ref(),
        unenforced,
        pass_execution(),
    )
    .await
    .unwrap();
    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert!(execution.output.get("read_only_violation").is_none());
}

#[tokio::test]
async fn changed_tree_fails_the_run_and_keeps_node_output() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    let workdir = dir.path().to_str().unwrap();
    let node = graph_node("r1", json!({"read_only": true}));
    let (watch, unenforced) = begin_for_run(true, &node, workdir).await.unwrap();

    std::fs::write(dir.path().join("new.txt"), "written\n").unwrap();

    let execution = apply_to_execution(
        &node,
        "Node 'node-r1'",
        workdir,
        watch.as_ref(),
        unenforced,
        pass_execution(),
    )
    .await
    .unwrap();
    assert_eq!(execution.status, GraphRunStatus::Fail);
    let violation = execution.output.get("read_only_violation").unwrap();
    assert_eq!(violation["paths"][0], "added: new.txt");
    assert_eq!(execution.output["node_output"]["verdict"], "pass");
}

#[tokio::test]
async fn project_paths_excludes_tracked_modifications_and_untracked_writes() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
    std::fs::write(dir.path().join(".claude/settings.json"), "{}\n").unwrap();
    git(dir.path(), &["add", "-A"]);
    git(
        dir.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "harness",
        ],
    );
    let exclusions = vec![".claude/".to_string(), "CLAUDE.local.md".to_string()];
    let workdir = dir.path().to_str().unwrap();

    let before = capture_fingerprint(workdir, &exclusions).await.unwrap();
    std::fs::write(dir.path().join(".claude/settings.json"), "{\"edited\":1}\n").unwrap();
    std::fs::write(dir.path().join(".claude/state.json"), "{}\n").unwrap();
    std::fs::write(dir.path().join("CLAUDE.local.md"), "note\n").unwrap();
    let after = capture_fingerprint(workdir, &exclusions).await.unwrap();

    assert!(before.diff_paths(&after).is_empty());
    assert_eq!(before.hash, after.hash);
}

#[tokio::test]
async fn non_git_workdir_runs_unenforced_and_says_so() {
    let dir = tempdir().unwrap();
    let workdir = dir.path().to_str().unwrap();
    let node = graph_node("r1", json!({"read_only": true}));

    let (watch, unenforced) = begin_for_run(true, &node, workdir).await.unwrap();
    assert!(watch.is_none());
    assert!(unenforced);

    let execution = apply_to_execution(
        &node,
        "Node 'node-r1'",
        workdir,
        watch.as_ref(),
        unenforced,
        pass_execution(),
    )
    .await
    .unwrap();
    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert_eq!(
        execution.output["read_only"],
        "read_only not enforced: not a git workdir"
    );
    assert_eq!(execution.output["verdict"], "pass");
}

#[test]
fn ensemble_read_only_flag_reaches_every_member() {
    let read_only = ensemble_details(true, false, &["m1", "m2"]);
    for id in ["m1", "m2"] {
        let node = graph_node(id, json!({}));
        assert!(
            node_has_read_only(&node, std::slice::from_ref(&read_only)),
            "member {id} must inherit the ensemble's read_only flag"
        );
    }

    let committer = ensemble_details(false, true, &["m1"]);
    let node = graph_node("m1", json!({}));
    assert!(!node_has_read_only(&node, &[committer]));
}

#[test]
fn node_validation_refuses_read_only_with_commit_rights() {
    let config = json!({
        "platform": "claude",
        "prompt_template": "go",
        "read_only": true,
        "commit_rights": true,
    });
    let err = crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Agent, &config)
        .unwrap_err();
    assert!(
        err.contains("read_only") && err.contains("commit_rights"),
        "refusal must name both fields: {err}"
    );
}

#[test]
fn ensemble_document_refuses_read_only_with_commit_rights() {
    let config = json!({
        "prompt_template": "review",
        "members": [{}, {}],
        "read_only": true,
        "commit_rights": true,
    });
    let parsed = crate::domain::nodes::EnsembleNodeConfig::from_value("doc", &config).unwrap();
    let err = parsed.validate("doc").unwrap_err();
    assert!(
        err.contains("read_only") && err.contains("commit_rights"),
        "refusal must name both fields: {err}"
    );
}

#[test]
fn graph_validation_refuses_cross_read_only_and_commit_rights() {
    let details = ensemble_details(false, true, &["m1"]);
    let nodes = vec![graph_node("m1", json!({"read_only": true}))];
    let err = crate::domain::validation::validate_ensembles_in_graph(&[details], &nodes, &[])
        .unwrap_err();
    assert!(
        err.contains("read_only") && err.contains("commit_rights"),
        "refusal must name both fields: {err}"
    );
}

#[tokio::test]
async fn synthetic_large_repo_fingerprints_under_two_seconds() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    for sub in 0..50 {
        let sub_dir = dir.path().join(format!("dir{sub:02}"));
        std::fs::create_dir_all(&sub_dir).unwrap();
        for file in 0..100 {
            std::fs::write(
                sub_dir.join(format!("f{file:03}.txt")),
                format!("{sub}-{file}\n"),
            )
            .unwrap();
        }
    }
    git(dir.path(), &["add", "-A"]);
    git(
        dir.path(),
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "bulk"],
    );
    for i in 0..500 {
        std::fs::write(
            dir.path().join(format!("untracked-{i:03}.txt")),
            format!("u{i}\n"),
        )
        .unwrap();
    }

    let workdir = dir.path().to_str().unwrap();
    let started = Instant::now();
    let before = capture_fingerprint(workdir, &[]).await.unwrap();
    let after = capture_fingerprint(workdir, &[]).await.unwrap();
    let elapsed = started.elapsed();

    assert_eq!(before.hash, after.hash);
    assert!(
        elapsed < Duration::from_secs(2),
        "two fingerprints of 5,000 tracked + 500 untracked files took {elapsed:?}"
    );
}

fn paths_of(diff: &[(String, PathChange)]) -> Vec<(String, &'static str)> {
    diff.iter()
        .map(|(p, c)| {
            (
                p.clone(),
                match c {
                    PathChange::Added => "added",
                    PathChange::Modified => "modified",
                    PathChange::Deleted => "deleted",
                },
            )
        })
        .collect()
}

#[test]
fn exclusion_matches_dir_prefix_and_root_file() {
    let exclusions = vec![".claude/".to_string(), "CLAUDE.local.md".to_string()];
    assert!(is_excluded(".claude/settings.json", &exclusions));
    assert!(is_excluded(".claude/", &exclusions));
    assert!(is_excluded("CLAUDE.local.md", &exclusions));
    assert!(!is_excluded("src/lib.rs", &exclusions));
}

#[tokio::test]
async fn unchanged_clean_tree_passes() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    let before = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    let after = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    assert_eq!(before.hash, after.hash);
    assert!(before.diff_paths(&after).is_empty());
}

#[tokio::test]
async fn unchanged_dirty_tree_passes() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("tracked.txt"), "dirty\n").unwrap();
    std::fs::write(dir.path().join("untracked.txt"), "u\n").unwrap();
    let before = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    let after = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    assert_eq!(before.hash, after.hash);
    assert!(before.diff_paths(&after).is_empty());
}

#[tokio::test]
async fn added_untracked_file_fails_with_path() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    let before = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    std::fs::write(dir.path().join("new.txt"), "hello\n").unwrap();
    let after = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    assert_ne!(before.hash, after.hash);
    assert_eq!(
        paths_of(&before.diff_paths(&after)),
        vec![("new.txt".to_string(), "added")]
    );
}

#[tokio::test]
async fn edited_tracked_file_fails_with_path() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    let before = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    std::fs::write(dir.path().join("tracked.txt"), "changed\n").unwrap();
    let after = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    assert_ne!(before.hash, after.hash);
    assert_eq!(
        paths_of(&before.diff_paths(&after)),
        vec![("tracked.txt".to_string(), "modified")]
    );
}

#[tokio::test]
async fn dirty_then_further_edit_fails_with_path() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("tracked.txt"), "dirty\n").unwrap();
    let before = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    std::fs::write(dir.path().join("tracked.txt"), "dirtier\n").unwrap();
    let after = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    assert_ne!(before.hash, after.hash);
    assert_eq!(
        paths_of(&before.diff_paths(&after)),
        vec![("tracked.txt".to_string(), "modified")]
    );
}

#[tokio::test]
async fn deleted_tracked_file_fails_with_path() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    let before = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    std::fs::remove_file(dir.path().join("tracked.txt")).unwrap();
    let after = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    assert_ne!(before.hash, after.hash);
    assert_eq!(
        paths_of(&before.diff_paths(&after)),
        vec![("tracked.txt".to_string(), "deleted")]
    );
}

#[tokio::test]
async fn binary_edit_fails_with_path() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("bin.dat"), [0u8, 1, 2, 3]).unwrap();
    git(dir.path(), &["add", "bin.dat"]);
    git(
        dir.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "add bin",
        ],
    );
    let before = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    std::fs::write(dir.path().join("bin.dat"), [0u8, 1, 2, 4, 255]).unwrap();
    let after = capture_fingerprint(dir.path().to_str().unwrap(), &[])
        .await
        .unwrap();
    assert_ne!(before.hash, after.hash);
    assert_eq!(
        paths_of(&before.diff_paths(&after)),
        vec![("bin.dat".to_string(), "modified")]
    );
}

#[tokio::test]
async fn project_paths_exclusion_does_not_fail() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
    let exclusions = vec![".claude/".to_string(), "CLAUDE.local.md".to_string()];
    let before = capture_fingerprint(dir.path().to_str().unwrap(), &exclusions)
        .await
        .unwrap();
    std::fs::write(dir.path().join(".claude/settings.json"), "{}\n").unwrap();
    std::fs::write(dir.path().join("CLAUDE.local.md"), "note\n").unwrap();
    let after = capture_fingerprint(dir.path().to_str().unwrap(), &exclusions)
        .await
        .unwrap();
    assert_eq!(before.hash, after.hash);
    assert!(before.diff_paths(&after).is_empty());
}

fn check_node(id: &str, config: Value) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: format!("node-{id}"),
        kind: GraphNodeKind::Check,
        config,
        position: 0,
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn begin_for_node_enforces_only_agent_nodes() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    let workdir = dir.path().to_str().unwrap();

    let agent = graph_node("r1", json!({"read_only": true}));
    let (watch, unenforced) = begin_for_node(&agent, &[], workdir).await.unwrap();
    assert!(watch.is_some());
    assert!(!unenforced);

    let plain = graph_node("r2", json!({}));
    let (watch, unenforced) = begin_for_node(&plain, &[], workdir).await.unwrap();
    assert!(watch.is_none());
    assert!(!unenforced);

    // `read_only` on a non-agent kind is never enforced: the key is not
    // even accepted there, so a stale config must stay a no-op.
    let check = check_node("c1", json!({"read_only": true}));
    let (watch, unenforced) = begin_for_node(&check, &[], workdir).await.unwrap();
    assert!(watch.is_none());
    assert!(!unenforced);
}

#[tokio::test]
async fn begin_for_node_inherits_ensemble_flag() {
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    let workdir = dir.path().to_str().unwrap();
    let details = ensemble_details(true, false, &["m1"]);

    let member = graph_node("m1", json!({}));
    let (watch, unenforced) = begin_for_node(&member, std::slice::from_ref(&details), workdir)
        .await
        .unwrap();
    assert!(watch.is_some());
    assert!(!unenforced);
}

#[tokio::test]
async fn begin_for_node_marks_non_git_workdir_unenforced() {
    let dir = tempdir().unwrap();
    let workdir = dir.path().to_str().unwrap();
    let node = graph_node("r1", json!({"read_only": true}));
    let (watch, unenforced) = begin_for_node(&node, &[], workdir).await.unwrap();
    assert!(watch.is_none());
    assert!(unenforced);
}

#[test]
fn member_is_read_only_covers_ensemble_and_node_flags() {
    let enforced_ensemble = ensemble_details(true, false, &["m1"]);
    let plain_ensemble = ensemble_details(false, false, &["m1"]);
    let plain = graph_node("m1", json!({}));
    let flagged = graph_node("m1", json!({"read_only": true}));
    assert!(member_is_read_only(&enforced_ensemble, &plain));
    assert!(member_is_read_only(&plain_ensemble, &flagged));
    assert!(!member_is_read_only(&plain_ensemble, &plain));
}

#[test]
fn read_only_violation_lists_touched_paths() {
    let touched = vec![
        ("a.txt".to_string(), PathChange::Added),
        ("b.txt".to_string(), PathChange::Modified),
        ("c.txt".to_string(), PathChange::Deleted),
    ];
    let exec = read_only_failure(
        "Node 'worker'",
        "n1",
        &touched,
        serde_json::json!({"ok": true}),
    );
    assert_eq!(exec.status, GraphRunStatus::Fail);
    let v = exec.output.get("read_only_violation").unwrap();
    assert_eq!(v.get("node_id").and_then(Value::as_str), Some("n1"));
    let paths = v.get("paths").and_then(Value::as_array).unwrap();
    assert_eq!(paths[0].as_str(), Some("added: a.txt"));
    assert_eq!(paths[1].as_str(), Some("modified: b.txt"));
    assert_eq!(paths[2].as_str(), Some("deleted: c.txt"));
    assert!(v
        .get("message")
        .and_then(Value::as_str)
        .unwrap()
        .contains("left in place"));
    assert!(exec.output.get("node_output").is_some());
}
