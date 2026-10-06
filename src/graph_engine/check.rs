use super::*;

/// Result of [`execute_shell_command`] — reused by both check nodes and
/// command hooks so there is exactly one way to run a command in the
/// codebase.
pub(crate) struct ShellCommandResult {
    pub(crate) status: GraphRunStatus,
    /// Check-shaped output JSON, with `stdout`/`stderr` already truncated to
    /// [`CHECK_OUTPUT_MAX_BYTES`] for storage (CB5 contract).
    pub(crate) output: Value,
    pub(crate) summary: String,
    /// The command's *full*, untruncated stdout / stderr as it emitted them.
    /// A check node evaluates its `success_condition` (`output_contains` /
    /// `output_not_contains`) against everything the command wrote, not just
    /// the tail kept in `output`. Empty when `timed_out` is set.
    pub(crate) full_stdout: String,
    pub(crate) full_stderr: String,
    /// The command exceeded its timeout and its process group was killed.
    /// A check node treats this as an unconditional fail (B28) rather than
    /// evaluating its `success_condition` against the partial output.
    pub(crate) timed_out: bool,
}
/// Run a shell command with the same capture contract as a check node:
/// `command`, `exit_code`, `stdout`, `stderr`, `passed` in the output JSON,
/// whether it succeeded or failed (CB5). Reused by command hooks so there
/// is exactly one way to run a command in the codebase.
pub(crate) async fn execute_shell_command(
    db: &Database,
    run_id: &str,
    command: &str,
    workdir: &str,
    timeout_seconds: u64,
    env_vars: &[(String, String)],
) -> Result<ShellCommandResult> {
    let spawned = spawn_shell_command(db, run_id, command, workdir, env_vars)?;
    let timeout_result = run_with_timeout(
        spawned.child,
        spawned.stdout_handle,
        spawned.stderr_handle,
        timeout_seconds,
    )
    .await?;

    match timeout_result {
        TimeoutOutcome::TimedOut => handle_timeout(
            db,
            run_id,
            spawned.pid,
            &spawned.stdout_buf,
            &spawned.stderr_buf,
            command,
            timeout_seconds,
        ),
        TimeoutOutcome::Completed(status) => handle_completion(
            db,
            run_id,
            status,
            &spawned.stdout_buf,
            &spawned.stderr_buf,
            command,
        ),
    }
}

pub(crate) fn lock_string(buf: &Arc<std::sync::Mutex<String>>) -> String {
    buf.lock().map(|g| g.clone()).unwrap_or_default()
}

pub(crate) struct SpawnedShellCommand {
    pub(crate) child: tokio::process::Child,
    pub(crate) pid: Option<u32>,
    pub(crate) stdout_buf: Arc<std::sync::Mutex<String>>,
    pub(crate) stderr_buf: Arc<std::sync::Mutex<String>>,
    pub(crate) stdout_handle: tokio::task::JoinHandle<()>,
    pub(crate) stderr_handle: tokio::task::JoinHandle<()>,
}

pub(crate) fn spawn_shell_command(
    db: &Database,
    run_id: &str,
    command: &str,
    workdir: &str,
    env_vars: &[(String, String)],
) -> Result<SpawnedShellCommand> {
    let mut process = shell_command(command);
    process.current_dir(workdir);
    for (key, value) in env_vars {
        process.env(key, value);
    }
    let mut child = process
        .spawn()
        .with_context(|| format!("Failed to spawn command: {command}"))?;
    let pid = child.id();
    if let Some(pid) = pid {
        let _ = db.set_graph_run_pid(run_id, pid as i64, crate::system::boot_id().as_deref());
    }

    let stdout_pipe = child.stdout.take().expect("shell_command pipes stdout");
    let stderr_pipe = child.stderr.take().expect("shell_command pipes stderr");
    let stdout_buf = Arc::new(std::sync::Mutex::new(String::new()));
    let stderr_buf = Arc::new(std::sync::Mutex::new(String::new()));
    let stdout_handle = spawn_check_output_reader(
        stdout_pipe,
        run_id.to_string(),
        "stdout",
        db.clone(),
        stdout_buf.clone(),
    );
    let stderr_handle = spawn_check_output_reader(
        stderr_pipe,
        run_id.to_string(),
        "stderr",
        db.clone(),
        stderr_buf.clone(),
    );

    Ok(SpawnedShellCommand {
        child,
        pid,
        stdout_buf,
        stderr_buf,
        stdout_handle,
        stderr_handle,
    })
}

pub(crate) enum TimeoutOutcome {
    TimedOut,
    Completed(std::process::ExitStatus),
}

pub(crate) async fn run_with_timeout(
    mut child: tokio::process::Child,
    stdout_handle: tokio::task::JoinHandle<()>,
    stderr_handle: tokio::task::JoinHandle<()>,
    timeout_seconds: u64,
) -> Result<TimeoutOutcome> {
    let execution = async {
        let status = child.wait().await?;
        let drained = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let _ = stdout_handle.await;
            let _ = stderr_handle.await;
        })
        .await
        .is_ok();
        Ok::<_, std::io::Error>((status, drained))
    };
    let timeout_result =
        tokio::time::timeout(std::time::Duration::from_secs(timeout_seconds), execution).await;

    match timeout_result {
        Ok(result) => {
            let (status, drained) = result?;
            if drained {
                Ok(TimeoutOutcome::Completed(status))
            } else {
                Ok(TimeoutOutcome::TimedOut)
            }
        }
        Err(_elapsed) => Ok(TimeoutOutcome::TimedOut),
    }
}

pub(crate) fn handle_timeout(
    db: &Database,
    run_id: &str,
    pid: Option<u32>,
    stdout_buf: &Arc<std::sync::Mutex<String>>,
    stderr_buf: &Arc<std::sync::Mutex<String>>,
    command: &str,
    timeout_seconds: u64,
) -> Result<ShellCommandResult> {
    if let Some(pid) = pid {
        crate::daemon::process::terminate_process_group_async(pid as i64, KILL_GRACE);
    }
    let partial_stdout = lock_string(stdout_buf);
    let partial_stderr = lock_string(stderr_buf);
    let (stdout_snap, _) = truncate_check_output(partial_stdout.trim().to_string());
    let (stderr_snap, _) = truncate_check_output(partial_stderr.trim().to_string());
    let _ = db.set_graph_run_tail_snapshot(run_id, Some(&stdout_snap), Some(&stderr_snap));
    let output = serde_json::json!({
        "kind": "check",
        "command": command,
        "error": "timed out",
        "timeout_seconds": timeout_seconds,
        "stdout": stdout_snap,
        "stderr": stderr_snap,
    });
    let _ = db.update_graph_run_result(
        run_id,
        GraphRunStatus::Fail,
        Some(&output),
        Some(chrono::Utc::now()),
    );
    Ok(ShellCommandResult {
        status: GraphRunStatus::Fail,
        output,
        summary: format!("Command timed out after {timeout_seconds}s."),
        full_stdout: String::new(),
        full_stderr: String::new(),
        timed_out: true,
    })
}

pub(crate) fn handle_completion(
    db: &Database,
    run_id: &str,
    status: std::process::ExitStatus,
    stdout_buf: &Arc<std::sync::Mutex<String>>,
    stderr_buf: &Arc<std::sync::Mutex<String>>,
    command: &str,
) -> Result<ShellCommandResult> {
    let exit_code = status.code().unwrap_or(-1);
    let full_stdout = lock_string(stdout_buf).trim().to_string();
    let full_stderr = lock_string(stderr_buf).trim().to_string();

    let (stdout, truncated_stdout) = truncate_check_output(full_stdout.clone());
    let (stderr, truncated_stderr) = truncate_check_output(full_stderr.clone());
    let truncated = truncated_stdout || truncated_stderr;

    let mut output_json = serde_json::json!({
        "kind": "check",
        "command": command,
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "passed": exit_code == 0,
    });
    if truncated {
        output_json["truncated"] = serde_json::Value::Bool(true);
    }

    let _ = db.set_graph_run_tail_snapshot(run_id, Some(&stdout), Some(&stderr));

    let status = if exit_code == 0 {
        GraphRunStatus::Pass
    } else {
        GraphRunStatus::Fail
    };

    Ok(ShellCommandResult {
        status,
        output: output_json,
        summary: format!("Command exited with code {exit_code}."),
        full_stdout,
        full_stderr,
        timed_out: false,
    })
}

#[expect(clippy::too_many_arguments)]
pub(crate) async fn execute_check_node(
    db: &Database,
    run_id: &str,
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    spec_start_head: Option<&str>,
    spec_committed_head: Option<&str>,
    workdir: &str,
) -> Result<NodeExecution> {
    let raw_command = node
        .config
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Check node '{}' is missing a command.", node.name))?;
    let command = raw_command
        .replace("{{spec_start_head}}", spec_start_head.unwrap_or(""))
        .replace(
            "{{spec_start_dirty}}",
            &spec
                .spec_start_dirty
                .map(|value| value.to_string())
                .unwrap_or_default(),
        )
        .replace("{{spec_committed_head}}", spec_committed_head.unwrap_or(""));

    let success_condition = node
        .config
        .get("success_condition")
        .and_then(Value::as_str)
        .unwrap_or("exit_code_0");
    let timeout_seconds = node
        .config
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(120);

    let result = execute_shell_command(db, run_id, &command, workdir, timeout_seconds, &[]).await?;

    // B28: a timeout is a check fail, not a hard error and not something to
    // run the success condition against — it must route through the fail
    // edge like any other check failure, never abort the whole spec and
    // never turn into a pass because a non-exit-code condition happened to
    // match the partial output.
    if result.timed_out {
        let mut output_json = result.output;
        output_json["graph_id"] = serde_json::json!(lp.id);
        output_json["spec_id"] = serde_json::json!(spec.id);
        output_json["node_id"] = serde_json::json!(node.id);
        return Ok(NodeExecution {
            status: GraphRunStatus::Fail,
            output: output_json,
            summary: format!(
                "Check node '{}' timed out after {timeout_seconds}s.",
                node.name
            ),
        });
    }

    // Check nodes evaluate a success_condition against the *full* captured
    // output — the `output_contains` / `output_not_contains` conditions must
    // see everything the command emitted, not just the tail kept for storage.
    let exit_code = result
        .output
        .get("exit_code")
        .and_then(Value::as_i64)
        .unwrap_or(-1) as i32;
    let combined = if result.full_stderr.is_empty() {
        result.full_stdout
    } else if result.full_stdout.is_empty() {
        result.full_stderr
    } else {
        format!("{}\n{}", result.full_stdout, result.full_stderr)
    };
    let passed = evaluate_success_condition(success_condition, exit_code, &combined)?;

    // Build check-node-specific output with graph/spec/node metadata.
    let mut output_json = result.output;
    output_json["graph_id"] = serde_json::json!(lp.id);
    output_json["spec_id"] = serde_json::json!(spec.id);
    output_json["node_id"] = serde_json::json!(node.id);
    output_json["success_condition"] = serde_json::json!(success_condition);
    output_json["passed"] = serde_json::json!(passed);

    // Override the status from the success_condition evaluation.
    let status = if passed {
        GraphRunStatus::Pass
    } else {
        GraphRunStatus::Fail
    };

    Ok(NodeExecution {
        status,
        output: output_json,
        summary: format!(
            "Check node '{}' {}.",
            node.name,
            if passed { "passed" } else { "failed" }
        ),
    })
}

/// Force stdin prompt delivery for an oversized prompt. Node outputs are
/// arbitrarily large (e.g. a full `cargo test` log), and the composed prompt
/// embeds previous_output via `{{previous_feedback}}`. Even after elision the
/// total can exceed Linux's MAX_ARG_STRLEN (128KiB), crashing the spawn with
/// E2BIG — the temp-file + stdin transport has no size cliff.
pub(crate) fn sized_strategy(
    base: &crate::domain::cli_strategy::CliStrategy,
    prompt: &str,
) -> crate::domain::cli_strategy::CliStrategy {
    if prompt.len() > ARGV_SAFETY_THRESHOLD && !base.prompt_via_stdin {
        base.with_stdin_forced()
    } else {
        base.clone()
    }
}

/// A node's pinned `skills` (S2): an optional array of skill names in
/// `node.config["skills"]`, resolved through the dynamic skill store (S1) at
/// spawn time and appended to the composed prompt in listed order. Anything
/// other than an array of strings (key absent, wrong type, non-string
/// element) is treated as "no pins" — malformed config must never fail a
/// spawn, exactly like every other loosely-typed node config key.
pub(crate) fn node_pinned_skills(node: &GraphNode) -> Vec<String> {
    node.config
        .get("skills")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Stable delimiter a pinned skill's instructions are appended under (S2).
/// Keep this format stable — downstream prompts may reference it.
pub(crate) fn render_pinned_skill_section(name: &str, instructions: &str) -> String {
    format!("\n\n## Skill: {name}\n{instructions}")
}

/// One-line stand-in for a pinned skill that couldn't be resolved, under the
/// same `## Skill: <name>` delimiter so the section is still easy to spot.
pub(crate) fn render_unresolved_skill_note(name: &str) -> String {
    format!(
        "\n\n## Skill: {name}\n_Could not resolve pinned skill '{name}' — continuing without it._"
    )
}

/// Resolve every skill pinned on `node` (S2) through the dynamic skill store
/// and append each one's instructions to `prompt`, in listed order, under a
/// `## Skill: <name>` section (see [`render_pinned_skill_section`]). Applied
/// identically to cold-start and resumed prompts so every agent-spawn
/// flavor carries the same pins.
///
/// A skill that can't be resolved — unknown name, no store configured, the
/// store's blocking fetch task panicking, or (via [`crate::dynamic_skills`]'s
/// own TTL/network handling) a source that's unreachable — degrades to
/// [`render_unresolved_skill_note`] plus a WARN naming the node and skill.
/// Pinned skills are a convenience the agent gets automatically, never a
/// hard dependency for the spawn to proceed at all.
pub(crate) async fn append_pinned_skills(
    mut prompt: String,
    node: &GraphNode,
    dynamic_skills: Option<&Arc<crate::dynamic_skills::SkillStore>>,
) -> String {
    let names = node_pinned_skills(node);
    if names.is_empty() {
        return prompt;
    }

    for name in names {
        let resolved = resolve_pinned_skill(node, &name, dynamic_skills).await;
        append_skill_result(&mut prompt, &name, resolved);
    }

    prompt
}

pub(crate) async fn resolve_pinned_skill(
    node: &GraphNode,
    name: &str,
    dynamic_skills: Option<&Arc<crate::dynamic_skills::SkillStore>>,
) -> Option<String> {
    let Some(store) = dynamic_skills else {
        tracing::warn!(
            node = %node.name,
            skill = %name,
            "no dynamic skill store configured; pinned skill not injected"
        );
        return None;
    };

    let store = Arc::clone(store);
    let fetch_name = name.to_string();
    match tokio::task::spawn_blocking(move || store.get(&fetch_name)).await {
        Ok(Ok(content)) => Some(content.instructions),
        Ok(Err(e)) => {
            tracing::warn!(
                node = %node.name,
                skill = %name,
                error = %e,
                "could not resolve pinned skill; continuing without it"
            );
            None
        }
        Err(join_err) => {
            tracing::warn!(
                node = %node.name,
                skill = %name,
                error = %join_err,
                "pinned skill resolution task failed; continuing without it"
            );
            None
        }
    }
}

pub(crate) fn append_skill_result(prompt: &mut String, name: &str, resolved: Option<String>) {
    match resolved {
        Some(instructions) => {
            prompt.push_str(&render_pinned_skill_section(name, &instructions));
        }
        None => prompt.push_str(&render_unresolved_skill_note(name)),
    }
}

/// CM13: the single fact CM13 classifies on — did this run finalize its own
/// row by calling `graph_complete_node` / `graph_report_blocker`? The run row's
/// status leaves `Running` only when a report call wrote to it. Routing
/// (`is_infra_crash_shape`), the ensemble fallthrough (same function), and the
/// self-reported-result path (`self_reported_execution`) all read this, so the
/// three can never disagree.
pub(crate) fn run_self_reported(run: &GraphNodeRun) -> bool {
    run.status != GraphRunStatus::Running
}

/// If the agent finalized its own run row (called `graph_complete_node` /
/// `graph_report_blocker`), turn that self-reported status into the node's
/// result; otherwise `None` so the caller uses the process-derived execution.
pub(crate) fn self_reported_execution(
    run: Option<&GraphNodeRun>,
    node: &GraphNode,
) -> Option<NodeExecution> {
    let run = run?;
    if !run_self_reported(run) {
        return None;
    }
    Some(NodeExecution {
        status: run.status,
        output: run.output.clone().unwrap_or_else(|| serde_json::json!({})),
        summary: format!("Agent node '{}' reported its own result.", node.name),
    })
}
