//! Part 02 of `tests` (split from `src/daemon/doctor.rs`, pure move).
use super::*;

#[tokio::test]
async fn gather_path_copies_report_two_dirs_different_versions_diverges_in_path_order() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let path_a = write_fake_canopy(dir_a.path(), "canopy 2.0.0");
    let path_b = write_fake_canopy(dir_b.path(), "canopy 1.0.0");
    let path_var = synthetic_path_var(&[dir_a.path(), dir_b.path()]);

    let report = gather_path_copies_report(&path_var, dir_a.path()).await;
    match &report {
        PathCopiesReport::Diverging { copies } => {
            assert_eq!(copies.len(), 2);
            assert_eq!(copies[0].0, path_a, "PATH order must be preserved");
            assert_eq!(copies[0].1.as_deref(), Some("canopy 2.0.0"));
            assert_eq!(copies[1].0, path_b);
            assert_eq!(copies[1].1.as_deref(), Some("canopy 1.0.0"));
        }
        other => panic!("expected Diverging, got {other:?}"),
    }

    let mut issues = Vec::new();
    print_path_copies_report(report, &mut issues);
    assert_eq!(issues.len(), 1, "differing versions must raise a warning");
    assert!(issues[0].contains("different versions"));
}

#[tokio::test]
async fn gather_path_copies_report_two_dirs_same_version_is_consistent_and_silent() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    write_fake_canopy(dir_a.path(), "canopy 1.0.0");
    write_fake_canopy(dir_b.path(), "canopy 1.0.0");
    let path_var = synthetic_path_var(&[dir_a.path(), dir_b.path()]);

    let report = gather_path_copies_report(&path_var, dir_a.path()).await;
    match &report {
        PathCopiesReport::Consistent { copies } => assert_eq!(copies.len(), 2),
        other => panic!("expected Consistent, got {other:?}"),
    }

    let mut issues = Vec::new();
    print_path_copies_report(report, &mut issues);
    assert!(
        issues.is_empty(),
        "identical versions must not raise a warning"
    );
}

#[tokio::test]
async fn gather_path_copies_report_one_copy_is_single_and_silent() {
    let dir_a = tempfile::tempdir().unwrap();
    let path_a = write_fake_canopy(dir_a.path(), "canopy 1.0.0");
    let path_var = synthetic_path_var(&[dir_a.path()]);

    let report = gather_path_copies_report(&path_var, dir_a.path()).await;
    match &report {
        PathCopiesReport::Single { path, version } => {
            assert_eq!(path, &path_a);
            assert_eq!(version.as_deref(), Some("canopy 1.0.0"));
        }
        other => panic!("expected Single, got {other:?}"),
    }

    let mut issues = Vec::new();
    print_path_copies_report(report, &mut issues);
    assert!(issues.is_empty(), "one copy must not raise a warning");
}

#[test]
fn find_canopy_copies_skips_nonexistent_path_entry_without_failing() {
    let dir_a = tempfile::tempdir().unwrap();
    let path_a = write_fake_canopy(dir_a.path(), "canopy 1.0.0");
    let missing = dir_a.path().join("does-not-exist-xyz");
    let path_var = synthetic_path_var(&[&missing, dir_a.path()]);

    let copies = find_canopy_copies(&path_var, dir_a.path());
    assert_eq!(copies, vec![path_a]);
}

#[test]
fn find_canopy_copies_ignores_non_executable_file_with_right_name() {
    let dir_a = tempfile::tempdir().unwrap();
    let path = dir_a.path().join("canopy");
    std::fs::write(&path, "not executable").unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    let path_var = synthetic_path_var(&[dir_a.path()]);

    let copies = find_canopy_copies(&path_var, dir_a.path());
    assert!(
        copies.is_empty(),
        "a non-executable file must not be reported as a copy that would run"
    );
}

/// C21 regression test: the bug this spec fixes. A shell profile
/// sourced more than once leaves the same directory repeated in `PATH`
/// — one file, several `PATH` entries — and the check must still report
/// it as one copy, not one per repetition.
#[test]
fn find_canopy_copies_dedupes_directory_repeated_on_path() {
    let dir_a = tempfile::tempdir().unwrap();
    let path_a = write_fake_canopy(dir_a.path(), "canopy 1.0.0");
    let path_var = synthetic_path_var(&[dir_a.path(), dir_a.path(), dir_a.path()]);

    let copies = find_canopy_copies(&path_var, dir_a.path());
    assert_eq!(
        copies,
        vec![path_a],
        "one file reachable through three PATH entries must be reported once"
    );
}

#[tokio::test]
async fn gather_path_copies_report_directory_repeated_on_path_is_single_and_silent() {
    let dir_a = tempfile::tempdir().unwrap();
    write_fake_canopy(dir_a.path(), "canopy 1.0.0");
    let path_var = synthetic_path_var(&[dir_a.path(), dir_a.path(), dir_a.path()]);

    let report = gather_path_copies_report(&path_var, dir_a.path()).await;
    match &report {
        PathCopiesReport::Single { .. } => {}
        other => panic!("expected Single from a directory repeated on PATH, got {other:?}"),
    }

    let mut issues = Vec::new();
    print_path_copies_report(report, &mut issues);
    assert!(
        issues.is_empty(),
        "a duplicated PATH entry pointing at one file must not raise a warning"
    );
}

/// A symlink and its target are one binary, not two — someone who
/// symlinks `~/bin/canopy` to `~/.local/bin/canopy` has one install.
#[cfg(unix)]
#[test]
fn find_canopy_copies_dedupes_symlink_to_binary_in_another_dir() {
    let real_dir = tempfile::tempdir().unwrap();
    let link_dir = tempfile::tempdir().unwrap();
    let real_path = write_fake_canopy(real_dir.path(), "canopy 1.0.0");
    let link_path = link_dir.path().join("canopy");
    std::os::unix::fs::symlink(&real_path, &link_path).unwrap();
    let path_var = synthetic_path_var(&[link_dir.path(), real_dir.path()]);

    let copies = find_canopy_copies(&path_var, link_dir.path());
    assert_eq!(
        copies.len(),
        1,
        "a symlink and its target must be counted as one binary, got {copies:?}"
    );
    assert_eq!(
        copies[0], link_path,
        "PATH order must be preserved: the symlink resolves first"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn gather_path_copies_report_symlink_to_binary_in_another_dir_is_single_and_silent() {
    let real_dir = tempfile::tempdir().unwrap();
    let link_dir = tempfile::tempdir().unwrap();
    let real_path = write_fake_canopy(real_dir.path(), "canopy 1.0.0");
    let link_path = link_dir.path().join("canopy");
    std::os::unix::fs::symlink(&real_path, &link_path).unwrap();
    let path_var = synthetic_path_var(&[link_dir.path(), real_dir.path()]);

    let report = gather_path_copies_report(&path_var, link_dir.path()).await;
    match &report {
        PathCopiesReport::Single { .. } => {}
        other => panic!("expected Single from a symlink and its target, got {other:?}"),
    }

    let mut issues = Vec::new();
    print_path_copies_report(report, &mut issues);
    assert!(
        issues.is_empty(),
        "a symlink alongside its target must not raise a warning"
    );
}

/// The green ✓ glyph doctor uses for a verified capability must only
/// ever be printed by the `success`/`success_nested` helpers — never
/// typed inline by an individual check. This is what stops a future
/// check from reintroducing the bug class this module fixes (a green
/// tick for something that was only declared, not exercised): as long
/// as every tick routes through one place, that place is the one spot
/// that has to earn the reviewer's trust, instead of every check site.
#[test]
fn success_glyph_only_printed_by_shared_helper() {
    let source = include_str!("../../doctor.rs");
    // Scan only the non-test code: test bodies legitimately reference
    // the glyph sequence in assertions (e.g. the corrupt-store test
    // above), which isn't the thing this test guards against.
    let production_code = source
        .split("mod tests {")
        .next()
        .expect("this file always contains the literal \"mod tests {\"");
    // Raw string: `include_str!` reads the file's literal text, where
    // an escape sequence like `\x1b` is four literal characters
    // (backslash, x, 1, b), not an evaluated control byte — the search
    // pattern must match that literal text, not the compiled string.
    let raw_glyph_occurrences = production_code.matches(r"\x1b[32m✓\x1b[0m").count();
    assert_eq!(
        raw_glyph_occurrences, 2,
        "the ✓ glyph must only be embedded by `success` and `success_nested` \
             (2 occurrences expected: one per helper's println!) — a new direct \
             occurrence means some check is printing a tick without going through \
             the shared helper"
    );
}

// ── CB19: diagnose_embeddings_config (non-ignored) ───────────

#[test]
fn diagnose_empty_model_is_not_configured() {
    // "no provider configured" must be a distinct verdict from
    // "configured but this build can't run it" — empty string is the
    // former, and must never collapse into ProviderUnavailable.
    assert_eq!(
        diagnose_embeddings_config(""),
        EmbeddingsConfigDiagnosis::NotConfigured
    );
}

#[test]
fn diagnose_remote_model_is_configured_on_every_build() {
    // Remote providers are compiled into every binary — a cloud model
    // string is always Configured, never ProviderUnavailable.
    assert_eq!(
        diagnose_embeddings_config("text-embedding-3-small"),
        EmbeddingsConfigDiagnosis::Configured
    );
    assert_eq!(
        diagnose_embeddings_config("gemini-embedding-001"),
        EmbeddingsConfigDiagnosis::Configured
    );
}

#[test]
#[cfg(not(feature = "local-embeddings"))]
fn diagnose_local_model_unavailable_without_feature() {
    assert_eq!(
        diagnose_embeddings_config("baai/bge-small-en-v1.5"),
        EmbeddingsConfigDiagnosis::ProviderUnavailable {
            provider_name: "Local"
        }
    );
}

#[test]
#[cfg(feature = "local-embeddings")]
fn diagnose_local_model_configured_with_feature() {
    assert_eq!(
        diagnose_embeddings_config("baai/bge-small-en-v1.5"),
        EmbeddingsConfigDiagnosis::Configured
    );
}

#[test]
fn diagnose_unknown_model_is_still_configured() {
    // Unknown ids fall through to the "not supported" path in doctor,
    // but they are not "not configured" and not a build-capability gap.
    assert_eq!(
        diagnose_embeddings_config("totally-unknown-embedding-model"),
        EmbeddingsConfigDiagnosis::Configured
    );
}

#[test]
fn find_stale_2x_name_hits_lists_a_stale_graph_prompt_and_a_stale_agent_prompt() {
    use crate::domain::graphs::{
        Graph, GraphCompletionHook, GraphHookEvent, GraphNode, GraphNodeKind, GraphStatus,
    };

    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();

    // One graph with one node whose prompt still names the removed tool,
    // plus one `on_failed` hook whose agent prompt names another. The
    // hook is deliberately not an `on_completed` one: a scanner that
    // skipped hooks, or labelled every hook a "completion hook", is
    // caught by the `on_failed hook` assertion below.
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnFailed,
        vec![GraphCompletionHook {
            platform: Some("claude".to_string()),
            model: None,
            effort: None,
            prompt: Some("on failure, call loop_report_blocker with details".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,

            crew: None,
        }],
    );
    let graph_id = "graph-stale-prompt";
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: graph_id.to_string(),
        name: "Resilience Graph".to_string(),
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
        hooks,
        require_spec_type: false,
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-resilience".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        name: "Resilience".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "prompt_template": "end with exactly one loop_complete_node call",
            "platform": "claude",
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // One scheduled agent whose prompt still names the gone CLI subcommand.
    let mut agent = sample_agent("loop-watchdog");
    agent.prompt = "run `canopy loop info` and alert on drift".to_string();
    db.upsert_agent(&agent).unwrap();

    let report = find_stale_2x_name_hits(&db).unwrap();
    assert_eq!(report.graphs.len(), 1, "one graph, one grouped line");
    let graph = &report.graphs[0];
    assert_eq!(graph.graph_name, "Resilience Graph");
    assert_eq!(graph.ref_count, 2, "one node hit plus one hook hit");
    assert!(
        graph.names.contains(&StaleNameHit {
            old: "loop_complete_node".to_string(),
            new: "graph_complete_node".to_string(),
        }),
        "{:?}",
        graph.names
    );
    assert!(
        graph.names.contains(&StaleNameHit {
            old: "loop_report_blocker".to_string(),
            new: "graph_report_blocker".to_string(),
        }),
        "{:?}",
        graph.names
    );

    assert_eq!(report.agents.len(), 1);
    let agent = &report.agents[0];
    assert_eq!(agent.agent_id, "loop-watchdog");
    assert!(
        agent.names.contains(&StaleNameHit {
            old: "canopy loop".to_string(),
            new: "canopy graph".to_string(),
        }),
        "{:?}",
        agent.names
    );
}

/// Covers the scanner sections the fixture above doesn't reach: a
/// hook's `command` (not just its prompt), an ensemble's shared prompt,
/// and a member's own prompt override.
#[test]
fn find_stale_2x_name_hits_scans_hook_commands_and_ensemble_prompts() {
    use crate::domain::graphs::{
        Ensemble, EnsembleDetails, EnsembleKind, EnsembleMember, GraphCompletionHook,
        GraphEdgeCondition, GraphHookEvent,
    };

    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnCompleted,
        vec![GraphCompletionHook {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: Some("canopy loop run daily".to_string()),
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,

            crew: None,
        }],
    );

    let ensembles = vec![EnsembleDetails {
        ensemble: Ensemble {
            mutex: Vec::new(),
            id: "ens-reviewers".to_string(),
            spec_id: None,
            graph_id: Some("graph-reviewers".to_string()),
            name: "reviewers".to_string(),
            prompt_template: "end with exactly one loop_complete_node call".to_string(),
            join_node_id: "join-reviewers".to_string(),
            entry_from_node: "entry-reviewers".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 1,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: "done".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            commit_rights: false,
            created_at: chrono::Utc::now(),

            crew_id: None,
            crew_name: None,
            crew_version_applied: None,
        },
        members: vec![EnsembleMember {
            ensemble_id: "ens-reviewers".to_string(),
            node_id: "member-reviewer".to_string(),
            position: 0,
            platform: "claude".to_string(),
            model: None,
            prompt_override: Some("on failure, call loop_report_blocker".to_string()),
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }],
    }];

    let mut hits = graph_hook_hits(&hooks);
    hits.extend(graph_ensemble_hits(&ensembles));

    assert!(
        hits.contains(&StaleNameHit {
            old: "canopy loop".to_string(),
            new: "canopy graph".to_string(),
        }),
        "{hits:?}"
    );
    assert!(
        hits.contains(&StaleNameHit {
            old: "loop_complete_node".to_string(),
            new: "graph_complete_node".to_string(),
        }),
        "{hits:?}"
    );
    assert!(
        hits.contains(&StaleNameHit {
            old: "loop_report_blocker".to_string(),
            new: "graph_report_blocker".to_string(),
        }),
        "{hits:?}"
    );
    assert_eq!(hits.len(), 3, "{hits:?}");
}

#[test]
fn find_stale_2x_name_hits_is_empty_on_a_clean_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let mut agent = sample_agent("clean-agent");
    agent.prompt = "summarize the diff and open a PR".to_string();
    db.upsert_agent(&agent).unwrap();

    let report = find_stale_2x_name_hits(&db).unwrap();
    assert!(report.graphs.is_empty());
    assert!(report.agents.is_empty());
}

#[test]
fn stale_name_report_groups_several_nodes_in_one_graph() {
    use crate::domain::graphs::{Graph, GraphNode, GraphNodeKind, GraphStatus};

    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let graph_id = "graph-many-nodes";
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: graph_id.to_string(),
        name: "Many Nodes".to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status: GraphStatus::Completed,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    })
    .unwrap();
    for i in 0..3 {
        db.insert_graph_node(&GraphNode {
            id: format!("node-{i}"),
            spec_id: None,
            graph_id: Some(graph_id.to_string()),
            name: format!("Node {i}"),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "prompt_template": "finish with loop_complete_node",
            }),
            position: i,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    }

    let report = find_stale_2x_name_hits(&db).unwrap();
    assert_eq!(
        report.graphs.len(),
        1,
        "three stale nodes in one graph must collapse to one entry"
    );
    assert_eq!(report.graphs[0].ref_count, 3);
    assert_eq!(
        report.graphs[0].names.len(),
        1,
        "the same old→new pair repeats: one distinct name"
    );
}

#[test]
fn stale_names_suggestion_archives_when_all_graphs_terminal() {
    let report = StaleNameReport {
        graphs: vec![
            StaleGraphNames {
                graph_name: "done-1".to_string(),
                status: GraphStatus::Completed,
                ref_count: 2,
                names: vec![],
            },
            StaleGraphNames {
                graph_name: "failed-1".to_string(),
                status: GraphStatus::Failed,
                ref_count: 1,
                names: vec![],
            },
        ],
        agents: vec![],
    };

    let suggestion = stale_names_suggestion(&report, 3);
    assert!(suggestion.contains("graph_edit"), "{suggestion}");
    assert!(suggestion.contains("graph_edit"), "{suggestion}");
    assert!(suggestion.contains("skips archived graphs"), "{suggestion}");
}

#[test]
fn stale_names_suggestion_edits_when_graph_not_terminal() {
    let report = StaleNameReport {
        graphs: vec![StaleGraphNames {
            graph_name: "still-drafting".to_string(),
            status: GraphStatus::Draft,
            ref_count: 1,
            names: vec![],
        }],
        agents: vec![],
    };

    let suggestion = stale_names_suggestion(&report, 1);
    assert!(
        suggestion.contains("edit them to the 3.x names"),
        "{suggestion}"
    );
    assert!(!suggestion.contains("graph_edit"), "{suggestion}");
}

#[tokio::test]
#[ignore]
async fn run_doctor_health_routine_never_run_never_deferred() {
    let (home, _canopy_dir, _db) = fresh_home_with_db();

    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok());
    assert!(
        output.contains("Daily health routine: never run"),
        "{output}"
    );
    assert!(!output.contains("never completed"), "{output}");
    assert!(!output.contains("72h"), "{output}");
}

#[tokio::test]
#[ignore]
async fn run_doctor_health_routine_never_completed_but_deferred() {
    let (home, _canopy_dir, db) = fresh_home_with_db();
    crate::daemon::health_routine::save_status(
        &db,
        &DbHealthStatus {
            last_run_at: None,
            deferred_count: 3,
            first_deferred_at: Some(Utc::now() - chrono::Duration::hours(2)),
            last_skip_at: Some(Utc::now() - chrono::Duration::hours(1)),
            last_skip_reason: Some("2 graph(s) currently running".to_string()),
            ..Default::default()
        },
    )
    .unwrap();

    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok());
    assert!(
        output.contains("never completed — deferred 3 times since"),
        "{output}"
    );
    assert!(output.contains("it runs anyway after 72h"), "{output}");
    assert!(output.contains("canopy daemon health-check"), "{output}");
}

#[tokio::test]
#[ignore]
async fn run_doctor_health_routine_ran_with_backup_and_deferral() {
    let (home, canopy_dir, db) = fresh_home_with_db();
    let backup = canopy_dir.join("background_agents.db.backup");
    std::fs::write(&backup, b"backup bytes").unwrap();
    crate::daemon::health_routine::save_status(
        &db,
        &DbHealthStatus {
            last_run_at: Some(Utc::now() - chrono::Duration::hours(1)),
            outcome: Some(crate::domain::db_health::DbHealthOutcome::Passed),
            integrity_result: Some("ok".to_string()),
            backup_path: Some(backup.display().to_string()),
            deferred_count: 4,
            ..Default::default()
        },
    )
    .unwrap();

    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok());
    assert!(
        output.contains("Daily health routine: passed (last run"),
        "{output}"
    );
    assert!(output.contains(&backup.display().to_string()), "{output}");
    assert!(output.contains("age:"), "{output}");
    assert!(
        output.contains("deferred 4 times since the last run"),
        "{output}"
    );
}

/// Agents-only stale names (every affected graph already archived): the
/// header must count the agents without also printing a "0 graph(s)"
/// clause, and the agent line still lists the distinct old→new pair.
#[tokio::test]
#[ignore]
async fn run_doctor_stale_names_header_counts_agents_when_no_graph_is_affected() {
    let (home, _canopy_dir, db) = fresh_home_with_db();
    let mut agent = sample_agent("agent-1");
    agent.prompt = "run `canopy loop info` and alert on drift".to_string();
    db.upsert_agent(&agent).unwrap();

    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok());
    assert!(
        output.contains("Stale 2.x names (1 found in 1 scheduled agent(s)):"),
        "{output}"
    );
    assert!(!output.contains("0 graph(s)"), "{output}");
    assert!(
        output.contains("scheduled agent 'agent-1' (1 refs): canopy loop → canopy graph"),
        "{output}"
    );
}
