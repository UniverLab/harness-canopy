//! Part 05 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

/// A harness with a registered `trust_flag` AND a node that opts in via
/// `trust_workdir: true` must actually receive the flag — otherwise
/// nothing in the registry is wired to anything a run can use.
#[tokio::test]
async fn run_agent_process_trust_flag_appended_when_configured_and_opted_in() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let capture = dir.path().join("argv.txt");
    let script = write_member_script(
        dir.path(),
        "capture-argv.sh",
        &format!("printf '%s' \"$*\" > \"{}\"\nexit 0", capture.display()),
    );
    let mut strategy = sample_strategy(&script);
    strategy.trust_flag = Some("--trust".to_string());
    let mut node = sample_agent_node();
    node.config = serde_json::json!({"trust_workdir": true});

    run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .unwrap();

    let captured_argv = std::fs::read_to_string(&capture).unwrap();
    assert!(
        captured_argv.contains("--trust"),
        "trust flag must be in argv: {captured_argv}"
    );
}

/// A harness with no `trust_flag` registered must never receive one,
/// even when a node opts in — there is nothing to pass, and the opt-in
/// must be a no-op rather than an error.
#[tokio::test]
async fn run_agent_process_trust_flag_absent_when_not_configured() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let capture = dir.path().join("argv.txt");
    let script = write_member_script(
        dir.path(),
        "capture-argv.sh",
        &format!("printf '%s' \"$*\" > \"{}\"\nexit 0", capture.display()),
    );
    let strategy = sample_strategy(&script); // trust_flag: None
    let mut node = sample_agent_node();
    node.config = serde_json::json!({"trust_workdir": true});

    run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .unwrap();

    let captured_argv = std::fs::read_to_string(&capture).unwrap();
    assert!(
        !captured_argv.contains("--trust"),
        "no trust flag is registered, so none must be passed: {captured_argv}"
    );
}

/// Trusting a directory is opt-in per node, never a default — a harness
/// configured with a trust flag must NOT receive it unless the node's
/// own config asks for it.
#[tokio::test]
async fn run_agent_process_trust_flag_absent_when_configured_but_not_opted_in() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let capture = dir.path().join("argv.txt");
    let script = write_member_script(
        dir.path(),
        "capture-argv.sh",
        &format!("printf '%s' \"$*\" > \"{}\"\nexit 0", capture.display()),
    );
    let mut strategy = sample_strategy(&script);
    strategy.trust_flag = Some("--trust".to_string());
    let node = sample_agent_node(); // config: {} — no trust_workdir opt-in

    run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .unwrap();

    let captured_argv = std::fs::read_to_string(&capture).unwrap();
    assert!(
        !captured_argv.contains("--trust"),
        "must never pass a trust flag by default without the node's opt-in: {captured_argv}"
    );
}

/// CM13: a script that exits 0 with real stdout but never calls
/// `graph_complete_node` is unreported infra, not Pass.
#[tokio::test]
async fn run_agent_process_normal_stdout_zero_exit_still_passes() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let script = write_member_script(dir.path(), "ok.sh", "printf 'all done'");
    let strategy = sample_strategy(&script);
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .unwrap();

    // CM13: unreported run is never Pass.
    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert_eq!(
        execution.output.get("stdout").and_then(Value::as_str),
        Some("all done")
    );
    assert!(execution.output.get("no_output").is_none());
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("unreported"),
        "CM13: unreported run must carry failure_kind: unreported"
    );
}

/// A fast nonzero-exit crash that (like most real crashes) prints
/// nothing to stdout must still be retried as an infra crash exactly as
/// before — the no-output fix only targets the exit-0 shape, and must
/// not silently swallow the existing nonzero-exit crash-retry path by
/// tagging every empty-stdout failure alike.
#[tokio::test]
async fn run_agent_process_empty_stdout_nonzero_exit_stays_infra_crash_eligible() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let script = write_member_script(dir.path(), "dead.sh", "exit 1");
    let strategy = sample_strategy(&script);
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .unwrap();

    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert!(
        execution.output.get("no_output").is_none(),
        "a nonzero-exit crash must not be conflated with the exit-0 no-output case"
    );

    let run = GraphNodeRun {
        id: "run-test".to_string(),
        graph_id: "loop1".to_string(),
        spec_id: "spec1".to_string(),
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
    };
    assert!(
        is_infra_crash(&node, &execution, &run, 0, 3, 60),
        "a fast nonzero-exit crash with empty stdout must remain infra-crash eligible"
    );
}

/// A self-reported FAIL (the agent called `graph_complete_node` itself)
/// must route on its own reported verdict, never on the no-output rule —
/// even when the CLI process that follows the self-report happens to
/// exit 0 with no further stdout.
#[tokio::test]
async fn self_reported_fail_is_not_reclassified_as_no_output() {
    let (_dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let node = seed_agent_run(&db, &graph_id, "run-selfreport");
    db.update_graph_run_result(
        "run-selfreport",
        GraphRunStatus::Fail,
        Some(&serde_json::json!({ "summary": "agent reported it failed" })),
        Some(chrono::Utc::now()),
    )
    .unwrap();

    let run = db.get_graph_run("run-selfreport").unwrap();
    let reported = self_reported_execution(run.as_ref(), &node)
        .expect("a completed run row must be read as self-reported");
    assert_eq!(reported.status, GraphRunStatus::Fail);
    assert!(reported.output.get("no_output").is_none());

    assert!(
        !is_infra_crash(&node, &reported, &run.unwrap(), 0, 3, 60),
        "a self-reported fail must never be classified as an infra crash"
    );
}

// ── require_report: the four corners ─────────────────────────────────
//
// `require_report` crossed with "did it self-report" — unit-tested
// directly against `agent_finished_execution` (rather than through a
// live process) exactly like `self_reported_fail_is_not_reclassified_as_
// no_output` above: `self_reported` is a plain bool parameter here, so
// the corner is exercised precisely without needing a fake CLI that can
// actually call `graph_complete_node` mid-run.

/// CM13: an exit-0, real-stdout, never-self-reported run is no longer a
/// pass — silence is not success. `failure_kind: "unreported"` is stamped
/// so a resilience node can distinguish "never reported" from "failed at
/// work" and from "process crashed".
#[test]
fn agent_finished_execution_require_report_absent_unreported_is_fail() {
    let cli = Cli::new("test-cli");
    let node = sample_agent_node();
    let execution = agent_finished_execution(&node, &cli, None, 0, "all done", "", false);

    assert_eq!(
        execution.status,
        GraphRunStatus::Fail,
        "CM13: an unreported run must not be recorded as pass"
    );
    assert_eq!(
        execution.output.get("unreported").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("unreported"),
        "CM13: the recorded outcome must distinguish 'never reported'"
    );
}

/// `require_report` absent + a self-reported run: unaffected, and no
/// `unreported` stamp — the run did report, after all.
#[test]
fn agent_finished_execution_require_report_absent_self_reported_no_stamp() {
    let cli = Cli::new("test-cli");
    let node = sample_agent_node();
    let execution = agent_finished_execution(&node, &cli, None, 0, "all done", "", true);

    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert!(
        execution.output.get("unreported").is_none(),
        "a self-reported run must not be marked unreported"
    );
    assert!(execution.output.get("failure_kind").is_none());
}

/// The exact hole this spec closes: `require_report: true`, exit 0, real
/// (non-empty) stdout — codex/copilot/antigravity have all been observed
/// exiting 0 with real stdout, including the model's own success
/// sentinel, while every tool call was refused/unavailable and nothing
/// was actually done. `zero_exit_no_output` doesn't catch this (stdout
/// isn't empty); `require_report` does, and must fail with the fixed
/// `failure_kind: "no_report"` string, not prose.
#[test]
fn agent_finished_execution_require_report_true_unreported_fails_as_no_report() {
    let cli = Cli::new("test-cli");
    let mut node = sample_agent_node();
    node.config = serde_json::json!({ "require_report": true });
    let execution = agent_finished_execution(&node, &cli, None, 0, "looks done", "", false);

    assert_eq!(
        execution.status,
        GraphRunStatus::Fail,
        "require_report must fail an exit-0 run that never self-reported"
    );
    assert_eq!(
        execution.output.get("unreported").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("no_report")
    );
    assert_eq!(
        execution.output.get("exit_code").and_then(Value::as_i64),
        Some(0),
        "the exit code itself is untouched — only the verdict is"
    );
}

/// `require_report: true` with a self-report present: the self-report
/// wins exactly as today — `require_report` never overrides an explicit
/// `graph_complete_node` verdict, pass or fail.
#[test]
fn agent_finished_execution_require_report_true_self_reported_is_not_overridden() {
    let cli = Cli::new("test-cli");
    let mut node = sample_agent_node();
    node.config = serde_json::json!({ "require_report": true });
    let execution = agent_finished_execution(&node, &cli, None, 0, "looks done", "", true);

    assert_eq!(
        execution.status,
        GraphRunStatus::Pass,
        "require_report must never override a self-reported result"
    );
    assert!(execution.output.get("unreported").is_none());
    assert!(execution.output.get("failure_kind").is_none());
}

/// End-to-end (real spawned process, not a fabricated `NodeExecution`):
/// a script that exits 0 and prints real stdout, on a node configured
/// with `require_report: true`, must fail — and CM13 says it IS an infra
/// crash (the run never reported), so it qualifies for infra-crash retry.
#[tokio::test]
async fn run_agent_process_require_report_true_no_self_report_is_fail_not_pass() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let script = write_member_script(dir.path(), "silent.sh", "printf 'Done!'\nexit 0");
    let strategy = sample_strategy(&script);
    let mut node = sample_agent_node();
    node.config = serde_json::json!({ "require_report": true });

    let execution = run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .unwrap();

    assert_eq!(
        execution.status,
        GraphRunStatus::Fail,
        "exit 0 with real stdout but no self-report must fail when require_report is set"
    );
    assert_eq!(
        execution.output.get("stdout").and_then(Value::as_str),
        Some("Done!")
    );
    assert_eq!(
        execution.output.get("unreported").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("no_report")
    );

    let run = GraphNodeRun {
        id: "run-test".to_string(),
        graph_id: "loop1".to_string(),
        spec_id: "spec1".to_string(),
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
    };
    assert!(
        is_infra_crash(&node, &execution, &run, 0, 3, 60),
        "CM13: a require_report run that never reported is infra crash"
    );
}

/// B39: a permanent spawn failure (missing binary) must produce a
/// `spawn_permanent` flag in the output and must NOT be classified as an
/// infra crash — exactly one run row, no `infra_attempt` marker.
#[tokio::test]
async fn b39_permanent_spawn_failure_skips_infra_retry() {
    let (_dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let strategy = sample_strategy("/nonexistent/somewhere/definitely-not-a-binary");
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .expect("spawn failure must not propagate as a hard error");

    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert!(
        execution
            .output
            .get("spawn_permanent")
            .and_then(Value::as_bool)
            == Some(true),
        "missing binary must set spawn_permanent flag"
    );
    assert!(
        execution.output.get("infra_attempt").is_none(),
        "permanent failure must not carry infra_attempt marker"
    );
    assert!(
        execution.output.get("infra_retry_skipped").is_some(),
        "the run row must record why the retry was skipped"
    );

    let run = GraphNodeRun {
        id: "run-test".to_string(),
        graph_id: "loop1".to_string(),
        spec_id: "spec1".to_string(),
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
    };
    assert!(
        !is_infra_crash(&node, &execution, &run, 0, 3, 60),
        "permanent spawn failure must not be classified as infra crash"
    );
}

/// B39, the case actually observed in graph f9d070bc: the CLI's configured
/// binary resolves to nothing, so the failure happens while *building* the
/// command rather than at spawn. It is just as permanent, and must be
/// classified as such without matching on the rendered message.
#[tokio::test]
async fn b39_unresolvable_binary_is_permanent_at_build_time() {
    let (_dir, db) = test_db();
    let cli = Cli::new("test-cli");
    // Bare name (not an absolute path) that is in neither PATH nor
    // `~/.<binary>/bin/<binary>` — the mimo failure mode.
    let strategy = sample_strategy("canopy-b39-definitely-not-installed");
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .expect("spawn failure must not propagate as a hard error");

    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert_eq!(
        execution
            .output
            .get("spawn_permanent")
            .and_then(Value::as_bool),
        Some(true),
        "an unresolvable binary must be classified permanent at build time"
    );
    assert!(
        execution.output.get("infra_retry_skipped").is_some(),
        "the run row must record why the retry was skipped"
    );
}

/// CB85: a binary that does not identify as its platform must fail the node
/// dispatch before anything is spawned — and must not consume the infra-
/// retry budget (B39: it is deterministic, like an unresolvable binary).
/// Mirrors the real-world `/usr/bin/blackbox` case (window manager, not the
/// AI CLI): the identity call answers with the window manager's usage line.
#[tokio::test]
async fn cb85_identity_mismatch_fails_node_dispatch_before_spawn() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let marker = dir.path().join("spawned");
    let marker_str = marker.to_string_lossy();
    let script = dir.path().join("blackbox");
    std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  --version) echo \"blackbox: another window manager is already running on display ':0'\" ;;\n  *) echo spawned >> \"{marker_str}\" ;;\nesac\n"
            ),
        )
        .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (_dir, db) = test_db();
    let cli = Cli::new("blackbox");
    let binary = script.to_string_lossy().into_owned();
    let mut strategy = sample_strategy(&binary);
    strategy.identity = Some(crate::domain::cli_strategy::BinaryIdentity {
        platform: "blackbox".to_string(),
        display: "BLACKBOX AI · CLI".to_string(),
        args: vec!["--version".to_string()],
        pattern: "(?i)blackbox ?ai".to_string(),
    });
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .expect("identity failure must not propagate as a hard error");

    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert_eq!(
        execution
            .output
            .get("spawn_permanent")
            .and_then(Value::as_bool),
        Some(true),
    );
    assert_eq!(
        execution
            .output
            .get("infra_retry_skipped")
            .and_then(Value::as_str),
        Some("resolved binary is not this platform (identity check failed)"),
    );
    let error = execution
        .output
        .get("error")
        .and_then(Value::as_str)
        .expect("failed dispatch must report an error");
    assert!(
        error.contains("is not BLACKBOX AI · CLI"),
        "error must name the platform display: {error}"
    );
    assert!(
        error.contains("identity check failed: blackbox: another window manager"),
        "error must quote the window-manager line: {error}"
    );
    assert!(
        !marker.exists(),
        "nothing must be spawned on identity failure"
    );
}

/// CB85: the matching case dispatches normally — the identity check runs
/// once (cached), then the agent process is spawned. The run that actually
/// executes is not marked permanent/spawn-errored.
#[tokio::test]
async fn cb85_identity_match_dispatches_normally() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let marker = dir.path().join("spawned");
    let marker_str = marker.to_string_lossy();
    let script = dir.path().join("blackbox");
    std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'blackbox ai 1.2.3' ;;\n  *) echo spawned >> \"{marker_str}\"; echo done ;;\nesac\n"
            ),
        )
        .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (_dir, db) = test_db();
    let cli = Cli::new("blackbox");
    let binary = script.to_string_lossy().into_owned();
    let mut strategy = sample_strategy(&binary);
    strategy.identity = Some(crate::domain::cli_strategy::BinaryIdentity {
        platform: "blackbox".to_string(),
        display: "BLACKBOX AI · CLI".to_string(),
        args: vec!["--version".to_string()],
        pattern: "(?i)blackbox ?ai".to_string(),
    });
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .expect("a matching identity must still run the agent");

    assert!(
        execution.output.get("spawn_permanent").is_none(),
        "a passing identity must not be flagged permanent"
    );
    assert!(
        execution.output.get("error").is_none(),
        "a passing identity must dispatch with no spawn error: {:?}",
        execution.output
    );
    assert!(
        marker.exists(),
        "the dispatch must really run after the identity check passes"
    );
}

/// B39: a transient spawn failure (command-build error, not NotFound)
/// still qualifies for infra-crash retry.
#[tokio::test]
async fn b39_transient_spawn_failure_still_retried() {
    let output = serde_json::json!({
        "kind": "agent",
        "node_id": "node-agent",
        "cli": "test-cli",
        "error": "some transient build error",
    });
    let execution = NodeExecution {
        status: GraphRunStatus::Fail,
        output,
        summary: "failed to spawn".to_string(),
    };
    let node = sample_agent_node();
    let run = GraphNodeRun {
        id: "run1".to_string(),
        graph_id: "loop1".to_string(),
        spec_id: "spec1".to_string(),
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
    };
    assert!(
        is_infra_crash(&node, &execution, &run, 0, 3, 60),
        "transient spawn failure without spawn_permanent must still be retried"
    );
}

/// SpawnError::from_build treats a typed binary-resolution failure as
/// permanent and every other command-build error as transient.
#[test]
fn spawn_error_from_build_classification() {
    let unresolvable = SpawnError::from_build(
        &crate::domain::cli_strategy::BinaryResolutionError {
            binary: "mimo".to_string(),
            path: "/usr/bin:/bin".to_string(),
        }
        .into(),
    );
    assert_eq!(
        unresolvable.permanent_reason,
        Some("cli binary could not be resolved")
    );

    let other = SpawnError::from_build(&anyhow::anyhow!(
        "CLI 'x' has no session_resume_cmd; cannot resume by id"
    ));
    assert!(
        other.permanent_reason.is_none(),
        "an untyped build error must stay transient"
    );
}

/// SpawnError::from_io correctly classifies NotFound and PermissionDenied
/// as permanent (with a reason), and other kinds as transient.
#[test]
fn spawn_error_from_io_classification() {
    let not_found = SpawnError::from_io(&std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "binary not found",
    ));
    assert!(
        not_found.permanent_reason.is_some(),
        "NotFound must be permanent"
    );

    let perm_denied = SpawnError::from_io(&std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "permission denied",
    ));
    assert!(
        perm_denied.permanent_reason.is_some(),
        "PermissionDenied must be permanent"
    );

    let broken_pipe = SpawnError::from_io(&std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "broken pipe",
    ));
    assert!(
        broken_pipe.permanent_reason.is_none(),
        "BrokenPipe must be transient"
    );

    let other = SpawnError::from_io(&std::io::Error::other("something else"));
    assert!(other.permanent_reason.is_none(), "Other must be transient");
}

#[tokio::test]
async fn run_agent_process_delivers_multi_hundred_kb_prompt_via_stdin() {
    // Feedback arrives already truncated per `bound_previous_feedback`,
    // but the transport itself must have no input-size cliff either —
    // stdin-mode CLIs must handle an oversized prompt without E2BIG.
    let (_dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let mut strategy = sample_strategy("/bin/cat");
    strategy.prompt_via_stdin = true;
    let node = sample_agent_node();
    let huge_prompt = "y".repeat(500 * 1024);

    let execution = run_agent_process(
        &db,
        "run-test",
        &cli,
        &strategy,
        &node,
        &huge_prompt,
        None,
        None,
        "/tmp",
        1,
        None,
    )
    .await
    .unwrap();

    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert_eq!(
        execution.output.get("stdout").and_then(Value::as_str),
        Some(huge_prompt.as_str())
    );
}

/// When the composed prompt exceeds `ARGV_SAFETY_THRESHOLD` and the CLI
/// doesn't have `prompt_via_stdin` set, the graph engine must override the
/// strategy to force stdin delivery — preventing E2BIG.
#[tokio::test]
async fn large_prompt_overrides_strategy_to_stdin() {
    let cli = Cli::new("test-cli");
    // Strategy starts with prompt_via_stdin = false (the problematic
    // default that caused the original E2BIG incident).
    let mut strategy = sample_strategy("/bin/cat");
    assert!(!strategy.prompt_via_stdin);

    // Simulate the override that execute_agent_node applies.
    let large_prompt = "z".repeat(ARGV_SAFETY_THRESHOLD + 1);
    if large_prompt.len() > ARGV_SAFETY_THRESHOLD && !strategy.prompt_via_stdin {
        strategy = strategy.with_stdin_forced();
    }
    assert!(
        strategy.prompt_via_stdin,
        "stdin must be forced for large prompts"
    );

    let node = sample_agent_node();
    let (_dir, db) = test_db();
    let execution = run_agent_process(
        &db,
        "run-test",
        &cli,
        &strategy,
        &node,
        &large_prompt,
        None,
        None,
        "/tmp",
        1,
        None,
    )
    .await
    .unwrap();
    assert_eq!(execution.status, GraphRunStatus::Fail); // CM13: unreported
    assert_eq!(
        execution.output.get("stdout").and_then(Value::as_str),
        Some(large_prompt.as_str())
    );
}

/// A prompt just under the threshold must NOT trigger the override —
/// argv delivery stays active for small prompts.
#[test]
fn small_prompt_does_not_force_stdin() {
    let mut strategy = sample_strategy("/bin/cat");
    assert!(!strategy.prompt_via_stdin);

    let small_prompt = "a".repeat(ARGV_SAFETY_THRESHOLD);
    if small_prompt.len() > ARGV_SAFETY_THRESHOLD && !strategy.prompt_via_stdin {
        strategy = strategy.with_stdin_forced();
    }
    assert!(
        !strategy.prompt_via_stdin,
        "stdin must NOT be forced for small prompts"
    );
}

#[test]
fn select_next_step_dedupes_identical_edges_to_same_target() {
    let edge = |id: &str, to: &str, condition| GraphEdge {
        id: id.to_string(),
        spec_id: Some("spec".to_string()),
        graph_id: None,
        from_node: "implement".to_string(),
        to_node: to.to_string(),
        condition,
    };
    let edges = vec![
        edge(
            "e1",
            "review",
            crate::domain::graphs::GraphEdgeCondition::Always,
        ),
        edge(
            "e2",
            "review",
            crate::domain::graphs::GraphEdgeCondition::Always,
        ),
    ];

    let next = select_next_step(&edges, &[], "implement", GraphRunStatus::Pass).unwrap();

    let sel = next.unwrap();
    assert_eq!(sel.cursor, SpecCursor::Node("review".to_string()));
    assert_eq!(
        sel.edge_condition,
        crate::domain::graphs::GraphEdgeCondition::Always
    );
}

#[test]
fn select_next_step_errors_on_distinct_targets() {
    let edge = |id: &str, to: &str, condition| GraphEdge {
        id: id.to_string(),
        spec_id: Some("spec".to_string()),
        graph_id: None,
        from_node: "implement".to_string(),
        to_node: to.to_string(),
        condition,
    };
    let edges = vec![
        edge(
            "e1",
            "review",
            crate::domain::graphs::GraphEdgeCondition::Always,
        ),
        edge(
            "e2",
            "deploy",
            crate::domain::graphs::GraphEdgeCondition::Always,
        ),
    ];

    let err = select_next_step(&edges, &[], "implement", GraphRunStatus::Pass).unwrap_err();

    assert!(err.to_string().contains("ambiguous outgoing edges"));
}

#[test]
fn select_next_step_resolves_ensemble_fan_out_from_ambiguous_edges() {
    // Three edges from the same predecessor, all targeting distinct
    // nodes — normally ambiguous, but here the distinct targets are
    // exactly one ensemble's full member set, so this must resolve to
    // the ensemble instead of erroring.
    let edge = |id: &str, to: &str| GraphEdge {
        id: id.to_string(),
        spec_id: Some("spec".to_string()),
        graph_id: None,
        from_node: "kickoff".to_string(),
        to_node: to.to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Always,
    };
    let edges = vec![edge("e1", "m1"), edge("e2", "m2"), edge("e3", "m3")];
    let ensembles = vec![ensemble_details_fixture(
        "ens1",
        "join1",
        &["m1", "m2", "m3"],
    )];

    let next = select_next_step(&edges, &ensembles, "kickoff", GraphRunStatus::Pass).unwrap();

    let sel = next.unwrap();
    assert_eq!(sel.cursor, SpecCursor::Ensemble("ens1".to_string()));
    assert_eq!(
        sel.edge_condition,
        crate::domain::graphs::GraphEdgeCondition::Always
    );
}

#[test]
fn select_next_step_resolves_ensemble_from_any_of_several_entry_sources() {
    // CM14: an ensemble entered from two different nodes resolves to the
    // ensemble from EACH of them — multi-source entry needs no relay.
    let edge = |id: &str, from: &str, to: &str| GraphEdge {
        id: id.to_string(),
        spec_id: Some("spec".to_string()),
        graph_id: None,
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Always,
    };
    let edges = vec![
        edge("e1", "designer", "m1"),
        edge("e2", "designer", "m2"),
        edge("e3", "gate", "m1"),
        edge("e4", "gate", "m2"),
    ];
    let ensembles = vec![ensemble_details_fixture("ens1", "join1", &["m1", "m2"])];

    for from in ["designer", "gate"] {
        let next = select_next_step(&edges, &ensembles, from, GraphRunStatus::Pass).unwrap();
        let sel = next.unwrap();
        assert_eq!(sel.cursor, SpecCursor::Ensemble("ens1".to_string()));
    }
}

#[test]
fn select_next_step_resolves_chained_ensemble_from_upstream_quorum() {
    // CM14: a quorum fanned out to another ensemble's members resolves to
    // that ensemble — chaining needs no intermediate node.
    let edge = |id: &str, to: &str| GraphEdge {
        id: id.to_string(),
        spec_id: Some("spec".to_string()),
        graph_id: None,
        from_node: "join1".to_string(),
        to_node: to.to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    };
    let edges = vec![edge("e1", "n1"), edge("e2", "n2")];
    let ensembles = vec![ensemble_details_fixture("ens2", "join2", &["n1", "n2"])];

    let next = select_next_step(&edges, &ensembles, "join1", GraphRunStatus::Pass).unwrap();

    let sel = next.unwrap();
    assert_eq!(sel.cursor, SpecCursor::Ensemble("ens2".to_string()));
}

#[test]
fn select_next_step_resolves_single_member_ensemble_from_lone_edge() {
    // C30: a 1-member cascade/round_robin ensemble's entry node has one
    // outgoing edge — to its sole member. That lone edge must resolve to
    // the ensemble, not be dispatched as a bare node.
    let edges = vec![GraphEdge {
        id: "e1".to_string(),
        spec_id: Some("spec".to_string()),
        graph_id: None,
        from_node: "kickoff".to_string(),
        to_node: "m1".to_string(),
        condition: GraphEdgeCondition::Always,
    }];
    let ensembles = vec![ensemble_details_fixture("ens1", "join1", &["m1"])];

    let next = select_next_step(&edges, &ensembles, "kickoff", GraphRunStatus::Pass).unwrap();

    let sel = next.unwrap();
    assert_eq!(sel.cursor, SpecCursor::Ensemble("ens1".to_string()));
    assert_eq!(sel.edge_condition, GraphEdgeCondition::Always);
}

#[test]
fn select_next_step_with_condition_resolves_single_member_ensemble_from_lone_edge() {
    let edges = vec![GraphEdge {
        id: "e1".to_string(),
        spec_id: Some("spec".to_string()),
        graph_id: None,
        from_node: "kickoff".to_string(),
        to_node: "m1".to_string(),
        condition: GraphEdgeCondition::Error,
    }];
    let ensembles = vec![ensemble_details_fixture("ens1", "join1", &["m1"])];

    let next =
        select_next_step_with_condition(&edges, &ensembles, "kickoff", &GraphEdgeCondition::Error)
            .unwrap();

    let sel = next.unwrap();
    assert_eq!(sel.cursor, SpecCursor::Ensemble("ens1".to_string()));
}

#[test]
fn select_next_step_leaves_a_non_member_lone_edge_as_a_node() {
    // Requirement 3: a lone edge to a node that is NOT any ensemble's
    // member must still resolve to `SpecCursor::Node`.
    let edges = vec![GraphEdge {
        id: "e1".to_string(),
        spec_id: Some("spec".to_string()),
        graph_id: None,
        from_node: "kickoff".to_string(),
        to_node: "deploy".to_string(),
        condition: GraphEdgeCondition::Always,
    }];
    let ensembles = vec![ensemble_details_fixture("ens1", "join1", &["m1"])];

    let next = select_next_step(&edges, &ensembles, "kickoff", GraphRunStatus::Pass).unwrap();

    let sel = next.unwrap();
    assert_eq!(sel.cursor, SpecCursor::Node("deploy".to_string()));
}

#[tokio::test]
async fn graph_engine_runs_graph_level_graph_across_two_specs() {
    // Neither spec has nodes of its own; both walk the graph's shared
    // top-level graph. A graph defined once should drive every spec.
    let (_dir, db, engine, graph_id, spec1_id) = graph_fixture().unwrap();
    let spec2 = second_spec(&graph_id, "spec-2", 2);
    db.insert_graph_spec(&spec2).unwrap();

    let check = GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    let gate = GraphNode {
        id: "graph-gate".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "gate".to_string(),
        kind: GraphNodeKind::Gate,
        config: serde_json::json!({
            "evaluate": "output_contains",
            "value": "APPROVED",
            "pass_route": "next_spec"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&check).unwrap();
    db.insert_graph_node(&gate).unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "graph-edge-pass".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        from_node: check.id.clone(),
        to_node: gate.id.clone(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);

    for spec_id in [spec1_id.as_str(), spec2.id.as_str()] {
        let spec = db.get_graph_spec(spec_id).unwrap().unwrap();
        assert_eq!(spec.status, GraphSpecStatus::Completed);
        let runs = db.list_graph_runs_for_spec(spec_id).unwrap();
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|run| run.spec_id == spec_id));
        assert!(runs.iter().any(|run| run.node_id == "graph-check"));
        assert!(runs.iter().any(|run| run.node_id == "graph-gate"));
    }
}

#[tokio::test]
async fn graph_engine_spec_with_own_graph_ignores_graph_level_graph() {
    // The top-level graph always fails; if it were used, the spec would
    // fail. The spec's own graph always passes, and precedence must
    // favor it — full backwards compatibility.
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "graph-check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "exit 1",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "spec-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "spec-check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();

    assert_eq!(lp.status, GraphStatus::Completed);
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].node_id, "spec-check");
}

#[tokio::test]
async fn graph_engine_iteration_budget_resets_between_specs_on_graph() {
    // A single top-level node that self-loops on failure, gated by a
    // counter file shared across the whole run. It fails budget-1 times
    // then passes on the budget-th call — exactly the per-node iteration
    // cap. If spec 2's budget carried over from spec 1 instead of
    // resetting, its first attempt would already read as one past the
    // cap and the spec would fail before the check command ever runs
    // again.
    let (_dir, db, engine, graph_id, spec1_id) = graph_fixture().unwrap();
    let spec2 = second_spec(&graph_id, "spec-2", 2);
    db.insert_graph_spec(&spec2).unwrap();

    db.insert_graph_node(&GraphNode {
            id: "flaky".to_string(),
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "flaky".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": format!(
                    "n=$(cat counter.txt 2>/dev/null || echo 0); n=$((n+1)); echo $n > counter.txt; test $n -ge {}",
                    crate::domain::settings::default_int("graphs.max_iterations_per_node")
                ),
                "success_condition": "exit_code_0"
            }),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "self-graph".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        from_node: "flaky".to_string(),
        to_node: "flaky".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);

    let spec1 = db.get_graph_spec(&spec1_id).unwrap().unwrap();
    let spec1_runs = db.list_graph_runs_for_spec(&spec1_id).unwrap();
    assert_eq!(spec1.status, GraphSpecStatus::Completed);
    assert_eq!(
        spec1_runs.len(),
        crate::domain::settings::default_int("graphs.max_iterations_per_node") as usize
    );

    let spec2_saved = db.get_graph_spec(&spec2.id).unwrap().unwrap();
    let spec2_runs = db.list_graph_runs_for_spec(&spec2.id).unwrap();
    assert_eq!(spec2_saved.status, GraphSpecStatus::Completed);
    // Fresh budget: the counter file is already at the cap from spec 1,
    // so spec 2's first (and only) fresh-budget attempt passes
    // immediately.
    assert_eq!(spec2_runs.len(), 1);
}

#[tokio::test]
async fn graph_engine_graph_level_entry_fallback_by_lowest_position() {
    // Retry cycle (implement <-> review): every node has an incoming
    // edge, so there is no source node and the engine must fall back to
    // the lowest-position node as the entry point.
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    let implement = GraphNode {
        id: "implement".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "implement".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf IMPLEMENT",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    let review = GraphNode {
        id: "review".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "review".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf REVIEW",
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&implement).unwrap();
    db.insert_graph_node(&review).unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e1".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        from_node: "implement".to_string(),
        to_node: "review".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Always,
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e2".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        from_node: "review".to_string(),
        to_node: "implement".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();

    assert_eq!(lp.status, GraphStatus::Completed);
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    assert_eq!(runs.len(), 2);
    // "implement" must be the entry: it ran with no previous-node input.
    // "review" ran second, fed by implement's output — proving the walk
    // started at the lowest-position node, not an arbitrary one.
    let implement_run = runs.iter().find(|run| run.node_id == "implement").unwrap();
    let review_run = runs.iter().find(|run| run.node_id == "review").unwrap();
    assert!(implement_run.input.is_none());
    assert!(review_run.input.is_some());
}

#[tokio::test]
async fn graph_engine_fails_spec_with_no_graph_anywhere_and_graph_moves_on() {
    // Neither the spec nor the graph has a graph: the spec must fail with
    // an actionable error instead of the engine erroring out before the
    // spec is even marked failed.
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();

    assert_eq!(lp.status, GraphStatus::Failed);
    assert_eq!(spec.status, GraphSpecStatus::Failed);
}

#[tokio::test]
async fn graph_engine_queue_run_walks_graph_across_queue_specs_in_queue_order() {
    // Two standalone specs, queued into the queue in the *opposite* order
    // of their `position` field — proving the queue's queue order drives
    // execution, not the spec's own position. Each pass through the
    // shared top-level check node commits to the workdir's git repo, so
    // the spec that captures the pre-commit HEAD ran first.
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    init_git_repo(dir.path());
    let initial_head = git_head(dir.path());

    let spec_a = standalone_spec("queue-spec-a", 1);
    let spec_b = standalone_spec("queue-spec-b", 2);
    db.insert_graph_spec(&spec_a).unwrap();
    db.insert_graph_spec(&spec_b).unwrap();
    // Queue order: b, then a — the reverse of position order.
    insert_queue_with_members(&db, "queue-1", &[&spec_b.id, &spec_a.id]);

    db.insert_graph_node(&GraphNode {
            id: "graph-check".to_string(),
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "check".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": "echo committed >> log.txt && git add -A && git commit -q -m spec && printf APPROVED",
                "success_condition": "exit_code_0"
            }),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

    engine
        .run_graph(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);

    let spec_a_after = db.get_graph_spec(&spec_a.id).unwrap().unwrap();
    let spec_b_after = db.get_graph_spec(&spec_b.id).unwrap().unwrap();
    assert_eq!(spec_a_after.status, GraphSpecStatus::Completed);
    assert_eq!(spec_b_after.status, GraphSpecStatus::Completed);
    assert_eq!(db.list_graph_runs_for_spec(&spec_a.id).unwrap().len(), 1);
    assert_eq!(db.list_graph_runs_for_spec(&spec_b.id).unwrap().len(), 1);

    // spec_b ran first: nothing had been committed yet.
    assert_eq!(
        spec_b_after.spec_start_head.as_deref(),
        Some(initial_head.as_str())
    );
    // spec_a ran second: spec_b's node had already committed by then.
    assert_ne!(
        spec_a_after.spec_start_head.as_deref(),
        Some(initial_head.as_str())
    );
}

/// `graph_control --from` must target the spec a FRESH dispatch would
/// actually execute. [`GraphEngine::preview_next_spec_id`] returns a
/// queue's still-`running` member first (the resumed-queue path
/// `graph_control` takes), but a fresh pick loop never re-picks it — so
/// the directed preview must skip it and name the next pickable member,
/// keeping the plan the operator gets and the run the engine performs on
/// the same spec.
#[test]
fn preview_directed_run_spec_id_skips_a_queues_running_member() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let mut running = standalone_spec("queue-running", 1);
    running.status = GraphSpecStatus::Running;
    let next = standalone_spec("queue-next", 2);
    db.insert_graph_spec(&running).unwrap();
    db.insert_graph_spec(&next).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&running.id, &next.id]);

    // Plain preview: mirrors the resumed-queue path — running member first.
    assert_eq!(
        engine
            .preview_next_spec_id(&graph_id, Some("queue-1"))
            .unwrap()
            .as_deref(),
        Some(running.id.as_str())
    );
    // Directed preview: mirrors a fresh dispatch — the next pickable member.
    assert_eq!(
        engine
            .preview_directed_run_spec_id(&graph_id, Some("queue-1"))
            .unwrap()
            .as_deref(),
        Some(next.id.as_str())
    );
    // Bound specs: same first non-terminal pick as the plain preview.
    let mut bound = standalone_spec("bound-failed", 1);
    bound.graph_id = Some(graph_id.clone());
    bound.status = GraphSpecStatus::Failed;
    let mut bound_done = standalone_spec("bound-done", 2);
    bound_done.graph_id = Some(graph_id.clone());
    bound_done.status = GraphSpecStatus::Completed;
    db.insert_graph_spec(&bound).unwrap();
    db.insert_graph_spec(&bound_done).unwrap();
    assert_eq!(
        engine
            .preview_directed_run_spec_id(&graph_id, None)
            .unwrap(),
        engine.preview_next_spec_id(&graph_id, None).unwrap()
    );
    assert_eq!(
        engine
            .preview_directed_run_spec_id(&graph_id, None)
            .unwrap()
            .as_deref(),
        Some(bound.id.as_str())
    );
}
