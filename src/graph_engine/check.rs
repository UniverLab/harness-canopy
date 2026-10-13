use super::*;

/// Result of [`execute_shell_command`] — reused by both check nodes and
/// command hooks so there is exactly one way to run a command in the
/// codebase.
pub(crate) struct ShellCommandResult {
    pub(crate) status: GraphRunStatus,
    /// Check-shaped output JSON, with `stdout`/`stderr` already truncated to
    /// [`RUN_OUTPUT_TAIL_BYTES`] for storage (CB5 contract).
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
    let stdout_handle = spawn_output_reader(
        stdout_pipe,
        run_id.to_string(),
        "stdout",
        db.clone(),
        stdout_buf.clone(),
    );
    let stderr_handle = spawn_output_reader(
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
    let _ = db.finish_graph_run_output(run_id, Some(&stdout_snap), Some(&stderr_snap));
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

    let _ = db.finish_graph_run_output(run_id, Some(&stdout), Some(&stderr));

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

/// The `{{...}}` markers a check command expands through POSIX shell
/// quoting (CM38 FR1) — their values (graph/spec names, a directory) may
/// contain spaces or quotes, so they must never be spliced in raw. Shared
/// with `graph_preflight` via [`CHECK_COMMAND_MARKERS`]; the split matters
/// because the pre-quoted-marker stripping below applies only to these.
pub(crate) const CHECK_COMMAND_QUOTED_MARKERS: &[&str] =
    &["graph_name", "workdir", "spec_id", "spec_name"];

/// The `{{...}}` markers a check command expands RAW (CM38 FR2) — a git SHA
/// or a dirty-count integer, exactly as before CM38. Stripping a
/// `'{{marker}}'` wrapper here would change `test -n '{{spec_start_head}}'`
/// from false to true on a non-git workdir, so they are never normalized.
pub(crate) const CHECK_COMMAND_RAW_MARKERS: &[&str] =
    &["spec_start_head", "spec_start_dirty", "spec_committed_head"];

/// Every marker a check command can bind (CM38) — the one vocabulary both
/// [`render_check_command`] and `graph_preflight` enforce, so the engine and
/// the audit can never drift. Built from the quoted and raw vocabularies so
/// the three can never drift from each other either.
pub(crate) const CHECK_COMMAND_MARKERS: &[&str] = &[
    CHECK_COMMAND_QUOTED_MARKERS[0],
    CHECK_COMMAND_QUOTED_MARKERS[1],
    CHECK_COMMAND_QUOTED_MARKERS[2],
    CHECK_COMMAND_QUOTED_MARKERS[3],
    CHECK_COMMAND_RAW_MARKERS[0],
    CHECK_COMMAND_RAW_MARKERS[1],
    CHECK_COMMAND_RAW_MARKERS[2],
];

/// Everything a check command's markers and environment are rendered from.
/// `workdir` is the directory the command actually runs in (the graph's
/// workdir, or the run's sandbox worktree / `workdir_override` when one
/// applies) — never `lp.workdir` read at a distance.
pub(crate) struct CheckCommandCtx<'a> {
    pub(crate) graph_id: &'a str,
    pub(crate) graph_name: &'a str,
    pub(crate) workdir: &'a str,
    pub(crate) spec_id: &'a str,
    pub(crate) spec_name: &'a str,
    pub(crate) spec_start_head: Option<&'a str>,
    pub(crate) spec_start_dirty: Option<i64>,
    pub(crate) spec_committed_head: Option<&'a str>,
}

/// Render a check node's command (CM38). Same contract as the CB48 hook
/// command renderer: refuse any `{{...}}` marker the vocabulary cannot bind
/// (first unknown marker wins, message is exactly
/// `unknown marker {{x}} in check command`), strip a single-quote wrapper a
/// template may already put around one of the QUOTED markers
/// (`'{{spec_name}}'` → `{{spec_name}}`) so the shell quoting is never
/// nested, then expand. Quoted markers go through `shell_quote`; the
/// head/dirty markers expand raw. A `\{{x}}` escape stays literal text.
pub(crate) fn render_check_command(template: &str, ctx: &CheckCommandCtx<'_>) -> Result<String> {
    if let Some(name) =
        crate::domain::prompts::unbindable_placeholders(template, CHECK_COMMAND_MARKERS).first()
    {
        bail!("unknown marker {{{{{name}}}}} in check command");
    }
    let normalized =
        CHECK_COMMAND_QUOTED_MARKERS
            .iter()
            .fold(template.to_string(), |template, marker| {
                template.replace(&format!("'{{{{{marker}}}}}'"), &format!("{{{{{marker}}}}}"))
            });
    Ok(crate::domain::prompts::render_template(
        &normalized,
        |raw| match raw {
            "graph_name" => Some(shell_quote(ctx.graph_name)),
            "workdir" => Some(shell_quote(ctx.workdir)),
            "spec_id" => Some(shell_quote(ctx.spec_id)),
            "spec_name" => Some(shell_quote(ctx.spec_name)),
            "spec_start_head" => Some(ctx.spec_start_head.unwrap_or("").to_string()),
            "spec_start_dirty" => Some(
                ctx.spec_start_dirty
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            "spec_committed_head" => Some(ctx.spec_committed_head.unwrap_or("").to_string()),
            _ => None,
        },
    ))
}

/// The `unknown marker {{x}} in check command` error for one check node, or
/// `None` when every marker binds. Scans a whole `GraphDetails` — the
/// graph's own nodes and every spec's nodes — so `graph_preflight` can
/// report the defect before spending any probe quota (CM38 FR4).
pub(crate) fn check_command_marker_error(
    details: &crate::domain::graphs::GraphDetails,
) -> Option<String> {
    let all_nodes = details
        .graph_nodes
        .iter()
        .chain(details.specs.iter().flat_map(|s| s.nodes.iter()));
    for node in all_nodes {
        if node.kind != GraphNodeKind::Check {
            continue;
        }
        let Some(command) = node.config.get("command").and_then(Value::as_str) else {
            continue;
        };
        if let Some(name) =
            crate::domain::prompts::unbindable_placeholders(command, CHECK_COMMAND_MARKERS).first()
        {
            return Some(format!(
                "unknown marker {{{{{name}}}}} in check command (check node '{}').",
                node.name
            ));
        }
    }
    None
}

/// The environment every check-node command runs with (CM38 FR6). The
/// command's working directory already IS the workdir, so there is no
/// `CANOPY_WORKDIR`.
pub(crate) fn check_command_env_vars(
    ctx: &CheckCommandCtx<'_>,
    spec_file: &str,
) -> Vec<(String, String)> {
    [
        ("CANOPY_SPEC_ID", ctx.spec_id),
        ("CANOPY_SPEC_NAME", ctx.spec_name),
        ("CANOPY_GRAPH_ID", ctx.graph_id),
        ("CANOPY_GRAPH_NAME", ctx.graph_name),
        ("CANOPY_SPEC_START_HEAD", ctx.spec_start_head.unwrap_or("")),
        ("CANOPY_SPEC_FILE", spec_file),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect()
}

/// Write the spec text (CM38 FR6) to `spec.md` inside a fresh scratch dir
/// for this node execution, and return the dir handle with the file's
/// absolute path. The caller holds the `TempDir` until the node finishes,
/// which removes the file afterwards on every path. The content is the
/// spec's full `description` — which on an idea run IS the idea text; a
/// spec with no description gets an empty file so the variable is always
/// set.
fn write_check_spec_file(
    node: &GraphNode,
    spec: &GraphSpec,
) -> Result<(tempfile::TempDir, String)> {
    let scratch = tempfile::tempdir().with_context(|| {
        format!(
            "Failed to create scratch dir for check node '{}'.",
            node.name
        )
    })?;
    let path = scratch.path().join("spec.md");
    std::fs::write(&path, spec.description.as_deref().unwrap_or("")).with_context(|| {
        format!(
            "Failed to write the spec file for check node '{}'.",
            node.name
        )
    })?;
    Ok((scratch, path.display().to_string()))
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
    let ctx = CheckCommandCtx {
        graph_id: &lp.id,
        graph_name: &lp.name,
        workdir,
        spec_id: &spec.id,
        spec_name: &spec.name,
        spec_start_head,
        spec_start_dirty: spec.spec_start_dirty,
        spec_committed_head,
    };
    // CM38 FR3: a command carrying a marker the engine cannot bind must
    // fail the node before anything reaches the shell.
    let command = match render_check_command(raw_command, &ctx) {
        Ok(command) => command,
        Err(error) => {
            return Ok(check_marker_violation(
                db,
                run_id,
                lp,
                spec,
                node,
                raw_command,
                &error.to_string(),
            ));
        }
    };

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

    // CM38 FR6: the spec text rides in a scratch file — never a marker —
    // so there is no quoting problem and no argv size limit. The TempDir is
    // held until this function returns, which removes the file once the
    // node has finished (pass, fail and timeout paths alike).
    let (_spec_scratch, spec_file) = write_check_spec_file(node, spec)?;
    let env_vars = check_command_env_vars(&ctx, &spec_file);
    let result =
        execute_shell_command(db, run_id, &command, workdir, timeout_seconds, &env_vars).await?;

    // B28: a timeout is a check fail, not a hard error and not something to
    // run the success condition against — it must route through the fail
    // edge like any other check failure, never abort the whole spec and
    // never turn into a pass because a non-exit-code condition happened to
    // match the partial output.
    if result.timed_out {
        return Ok(check_timeout_execution(
            lp,
            spec,
            node,
            timeout_seconds,
            result.output,
        ));
    }

    finalize_check_result(lp, spec, node, success_condition, result)
}

/// CM38 FR3: settle a check node whose command carried an unbindable
/// `{{...}}` marker — a Fail verdict routed through the node's `fail` edge,
/// with the refusal message recorded on the run row. Nothing ever spawns.
fn check_marker_violation(
    db: &Database,
    run_id: &str,
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    raw_command: &str,
    message: &str,
) -> NodeExecution {
    let output_json = serde_json::json!({
        "kind": "check",
        "command": raw_command,
        "error": message,
        "graph_id": lp.id,
        "spec_id": spec.id,
        "node_id": node.id,
    });
    let _ = db.update_graph_run_result(
        run_id,
        GraphRunStatus::Fail,
        Some(&output_json),
        Some(chrono::Utc::now()),
    );
    NodeExecution {
        status: GraphRunStatus::Fail,
        output: output_json,
        summary: format!("Check node '{}': {message}.", node.name),
    }
}

/// The timed-out shape of a check node (B28): Fail verdict plus the graph/
/// spec/node metadata, with no success condition evaluated.
fn check_timeout_execution(
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    timeout_seconds: u64,
    output: Value,
) -> NodeExecution {
    let mut output_json = output;
    output_json["graph_id"] = serde_json::json!(lp.id);
    output_json["spec_id"] = serde_json::json!(spec.id);
    output_json["node_id"] = serde_json::json!(node.id);
    NodeExecution {
        status: GraphRunStatus::Fail,
        output: output_json,
        summary: format!(
            "Check node '{}' timed out after {timeout_seconds}s.",
            node.name
        ),
    }
}

/// Evaluate a finished check command's `success_condition` against the
/// *full* captured output — the `output_contains` / `output_not_contains`
/// conditions must see everything the command emitted, not just the tail
/// kept for storage — and build the check node's result with the graph/
/// spec/node metadata.
fn finalize_check_result(
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    success_condition: &str,
    result: ShellCommandResult,
) -> Result<NodeExecution> {
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

    let mut output_json = result.output;
    output_json["graph_id"] = serde_json::json!(lp.id);
    output_json["spec_id"] = serde_json::json!(spec.id);
    output_json["node_id"] = serde_json::json!(node.id);
    output_json["success_condition"] = serde_json::json!(success_condition);
    output_json["passed"] = serde_json::json!(passed);

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

#[cfg(test)]
mod check_command_tests {
    use super::*;

    fn ctx<'a>(spec_name: &'a str, head: Option<&'a str>) -> CheckCommandCtx<'a> {
        CheckCommandCtx {
            graph_id: "wf-test",
            graph_name: "Graph",
            workdir: "/work/dir",
            spec_id: "spec-test",
            spec_name,
            spec_start_head: head,
            spec_start_dirty: Some(2),
            spec_committed_head: None,
        }
    }

    #[test]
    fn check_command_quotes_spec_name_with_spaces() {
        let rendered =
            render_check_command("test {{spec_name}} = 'a b'", &ctx("a b", None)).unwrap();
        assert_eq!(rendered, "test 'a b' = 'a b'");
    }

    #[test]
    fn check_command_round_trips_an_injection_shaped_name() {
        let name = "x'; touch /tmp/p; '";
        let rendered = render_check_command("printf %s {{spec_name}}", &ctx(name, None)).unwrap();
        assert_eq!(rendered, format!("printf %s {}", shell_quote(name)));
    }

    #[test]
    fn check_command_strips_prequoted_quoted_markers() {
        let rendered =
            render_check_command("n='{{spec_name}}'; echo $n", &ctx("a b", None)).unwrap();
        assert_eq!(rendered, "n='a b'; echo $n");
    }

    #[test]
    fn check_command_head_markers_stay_raw() {
        let rendered = render_check_command(
            "{{spec_start_head}}|{{spec_start_dirty}}|{{spec_committed_head}}",
            &ctx("s", Some("abc")),
        )
        .unwrap();
        assert_eq!(rendered, "abc|2|");
    }

    #[test]
    fn check_command_refuses_unknown_marker_with_exact_message() {
        let err = render_check_command("echo {{nope}}", &ctx("s", None)).unwrap_err();
        assert_eq!(err.to_string(), "unknown marker {{nope}} in check command");
        assert_eq!(
            CHECK_COMMAND_RAW_MARKERS,
            &["spec_start_head", "spec_start_dirty", "spec_committed_head"]
        );
    }

    #[test]
    fn check_command_env_vars_lists_every_variable() {
        let vars = check_command_env_vars(&ctx("a b", Some("abc")), "/scratch/spec.md");
        let pairs: Vec<(String, String)> = [
            ("CANOPY_SPEC_ID", "spec-test"),
            ("CANOPY_SPEC_NAME", "a b"),
            ("CANOPY_GRAPH_ID", "wf-test"),
            ("CANOPY_GRAPH_NAME", "Graph"),
            ("CANOPY_SPEC_START_HEAD", "abc"),
            ("CANOPY_SPEC_FILE", "/scratch/spec.md"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        assert_eq!(vars, pairs);
    }

    #[test]
    fn check_command_spec_file_holds_description_and_is_removed_on_drop() {
        let node = GraphNode {
            id: "n".to_string(),
            spec_id: None,
            graph_id: None,
            name: "check".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({}),
            position: 1,
            created_at: chrono::Utc::now(),
        };
        let spec = GraphSpec {
            id: "s".to_string(),
            graph_id: None,
            name: "Spec".to_string(),
            description: Some("Objective:\n- quoted".to_string()),
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        let (scratch, path) = write_check_spec_file(&node, &spec).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "Objective:\n- quoted"
        );
        assert!(!path.is_empty());
        drop(scratch);
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn check_command_marker_error_names_node_and_marker() {
        let node = |command: &str, name: &str| GraphNode {
            id: "n".to_string(),
            spec_id: None,
            graph_id: Some("g".to_string()),
            name: name.to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({ "command": command }),
            position: 1,
            created_at: chrono::Utc::now(),
        };
        let details = crate::domain::graphs::GraphDetails {
            lp: crate::domain::graphs::Graph {
                archived: false,
                paused_by_reconciliation: false,
                allow_dirty_start: false,
                infra_node_id: None,
                id: "g".to_string(),
                name: "Graph".to_string(),
                description: None,
                workdir: "/w".to_string(),
                status: crate::domain::graphs::GraphStatus::Draft,
                trigger: None,
                created_at: chrono::Utc::now(),
                started_at: None,
                completed_at: None,
                autorun_at: None,
                auto_continue_at: None,
                auto_continue_action: None,
                active_run_queue_id: None,
                hooks: std::collections::BTreeMap::new(),
            },
            graph_nodes: vec![node("echo {{nope}}", "gatekeeper")],
            graph_edges: vec![],
            specs: vec![],
            completion_hook_runs: vec![],
        };
        let message = check_command_marker_error(&details).unwrap();
        assert_eq!(
            message,
            "unknown marker {{nope}} in check command (check node 'gatekeeper')."
        );

        // FR4 must also reach check nodes bound to a spec, not just the
        // graph's own nodes — drop the graph-level one and keep the defect
        // inside `specs[].nodes` only.
        let mut spec_only = details;
        spec_only.graph_nodes.clear();
        spec_only.specs = vec![crate::domain::graphs::GraphSpecDetails {
            spec: GraphSpec {
                id: "s".to_string(),
                graph_id: Some("g".to_string()),
                name: "Spec".to_string(),
                description: None,
                position: 1,
                parallelizable: false,
                status: GraphSpecStatus::Pending,
                started_at: None,
                completed_at: None,
                spec_start_head: None,
                spec_start_dirty: None,
                spec_end_dirty: None,
                spec_end_dirty_paths: None,
                spec_committed_head: None,
                workdir: None,
                completed_via: None,
                completed_via_reason: None,
                completed_via_at: None,
            },
            nodes: vec![node("echo {{specnope}}", "spec-gate")],
            edges: vec![],
        }];
        assert_eq!(
            check_command_marker_error(&spec_only).unwrap(),
            "unknown marker {{specnope}} in check command (check node 'spec-gate')."
        );
    }
}
