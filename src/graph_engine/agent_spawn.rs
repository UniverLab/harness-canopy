use super::*;

/// A failure to build or spawn the child process, classified by whether it
/// can plausibly resolve itself between attempts (B39).
///
/// Permanent failures — an unresolvable binary, a non-executable file — are
/// deterministic: retrying spends the infra-retry budget and its doubling
/// backoff on a state that cannot change. `permanent_reason` carries the
/// operator-facing "why we did not retry", set from the *kind* of the
/// underlying error (typed [`BinaryResolutionError`][ce], `io::ErrorKind`) and
/// never from matching the rendered message per CLI.
///
/// [ce]: crate::domain::cli_strategy::BinaryResolutionError
pub(crate) struct SpawnError {
    pub(crate) message: String,
    pub(crate) permanent_reason: Option<&'static str>,
}

impl SpawnError {
    /// A failure while building the command — chiefly resolving the CLI's
    /// configured binary, which is where a missing CLI surfaces.
    pub(crate) fn from_build(error: &anyhow::Error) -> Self {
        let permanent_reason = error
            .downcast_ref::<crate::domain::cli_strategy::BinaryResolutionError>()
            .map(|_| "cli binary could not be resolved");
        Self {
            message: error.to_string(),
            permanent_reason,
        }
    }

    /// A failure from the spawn/wait syscalls themselves.
    pub(crate) fn from_io(error: &std::io::Error) -> Self {
        let permanent_reason = match error.kind() {
            std::io::ErrorKind::NotFound => Some("cli binary not found at its resolved path"),
            std::io::ErrorKind::PermissionDenied => Some("cli binary is not executable"),
            _ => None,
        };
        Self {
            message: error.to_string(),
            permanent_reason,
        }
    }

    /// A failure with no reason to believe a retry would land differently is
    /// transient by default, so the B19/B26 retry path is unchanged.
    pub(crate) fn transient(message: String) -> Self {
        Self {
            message,
            permanent_reason: None,
        }
    }
}

/// Outcome of actually running the child process to completion, as opposed
/// to failing to build/spawn it (see [`spawn_and_wait_cli_process`]'s `Err`).
pub(crate) enum CliProcessOutcome {
    Finished {
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    /// The process started but didn't finish within `timeout_minutes`. Its
    /// process group has already been killed (B12) by the time this variant
    /// is returned — callers only need to decide how to record the failure.
    TimedOut,
}

/// Build the CLI command, spawn it, and wait for it (or a timeout) — the
/// one spawn path shared by every detached single-agent execution the
/// engine runs, node or hook alike: a graph agent node
/// ([`run_agent_process`]) and the `on_completed` hook
/// ([`run_completion_hook_process`]).
///
/// Returns `Err` only for a failure to build/spawn the process itself (e.g.
/// `E2BIG` from an oversized argv, binary not found, permission denied) —
/// callers turn that into their own kind of "failed" record rather than a
/// hard error, since a spawn failure must never abort anything wider (the
/// whole graph run, for a node; the graph's already-finalized status, for the
/// hook).
///
/// A timeout is a different failure class (the process started; it just
/// didn't finish in time). Unlike a build/spawn failure, the spawned process
/// group is actually killed here (B12) before returning
/// [`CliProcessOutcome::TimedOut`] — dropping the timed-out future used to
/// leave it running indefinitely (`Command::output` gives the caller no
/// handle to kill), which is exactly what let a `mimo run` child outlive its
/// node run by 42+ minutes in the 2026-07-12 incident.
#[expect(clippy::too_many_arguments)]
pub(crate) async fn spawn_and_wait_cli_process(
    strategy: &crate::domain::cli_strategy::CliStrategy,
    prompt: &str,
    model: Option<&str>,
    effort: Option<&str>,
    workdir: &str,
    timeout_minutes: u64,
    session_id: Option<&str>,
    resume_session_id: Option<&str>,
    trust_workdir: bool,
    run_id: Option<&str>,
    on_pid: impl FnOnce(u32),
) -> Result<CliProcessOutcome, SpawnError> {
    // A resume (RS2) uses the by-id resume flag and continues an existing
    // session; a cold start uses the set-at-spawn flag (if any). The two are
    // mutually exclusive — the caller passes at most one.
    let mut command = match resume_session_id {
        Some(sid) => strategy
            .build_resume_command(sid, prompt, model, Some(workdir), None)
            .map_err(|error| SpawnError::from_build(&error))?,
        None => strategy
            .build_command_with_session(prompt, model, Some(workdir), session_id, effort, run_id)
            .map_err(|error| SpawnError::from_build(&error))?,
    };
    // Only appended when the caller opted in (per node.config["trust_workdir"])
    // AND the harness has a registered trust flag — never a silent default
    // (see [`CliConfig::trust_flag`]).
    if trust_workdir {
        if let Some(flag) = strategy.trust_flag.as_deref() {
            command.arg(flag);
        }
    }
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let child = command
        .spawn()
        .map_err(|error| SpawnError::from_io(&error))?;
    // Captured before `wait_with_output` below takes ownership of `child`.
    let pid = child.id();
    if let Some(pid) = pid {
        on_pid(pid);
    }

    let timeout_result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_minutes * 60),
        child.wait_with_output(),
    )
    .await;

    match timeout_result {
        Ok(Ok(output)) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Ok(CliProcessOutcome::Finished {
                exit_code,
                stdout,
                stderr,
            })
        }
        Ok(Err(error)) => Err(SpawnError::transient(error.to_string())),
        Err(_elapsed) => {
            if let Some(pid) = pid {
                crate::daemon::process::terminate_process_group_async(pid as i64, KILL_GRACE);
            }
            Ok(CliProcessOutcome::TimedOut)
        }
    }
}

/// Run an agent node's process via [`spawn_and_wait_cli_process`], turning
/// any failure to build or spawn the process into a failed `NodeExecution`
/// rather than propagating a hard error — routed through the graph's fail
/// edge for resilience triage, never aborting the whole graph run.
///
/// A timeout (B28) is likewise a failed `NodeExecution`, not a hard error:
/// it exceeds `infra_crash_max_seconds` by definition, so it's always a
/// semantic fail routed through the fail edge like any other, never an
/// infra-crash retry.
pub(crate) fn effort_notice(
    platform: &str,
    strategy: &crate::domain::cli_strategy::CliStrategy,
    effort: Option<&str>,
) -> Option<serde_json::Value> {
    let effort = effort?;
    crate::domain::cli_config::effort_rejection_reason(
        strategy.effort_declaration.as_ref(),
        platform,
        effort,
    )
    .map(serde_json::Value::from)
}

/// CB34 mirror of [`effort_notice`]: a `model` requested on a platform whose
/// `model_flag` is absent or blank cannot be honoured. Record that fact in
/// the run record — named platform, named model — rather than silently
/// dropping it (the `yolo_flag`-in-headless mistake the effort rule exists to
/// avoid repeating). `None` when no model was requested or the platform can
/// select one.
pub(crate) fn model_notice(
    platform: &str,
    strategy: &crate::domain::cli_strategy::CliStrategy,
    model: Option<&str>,
) -> Option<serde_json::Value> {
    let model = model?;
    crate::domain::cli_config::model_rejection_reason(
        strategy.model_flag.as_deref(),
        platform,
        model,
    )
    .map(serde_json::Value::from)
}

/// CB43: the model string actually handed to the CLI argv — `None` when no
/// model was requested, blank, or the platform's `model_flag` cannot select
/// one. This is the stored `executed_model`, never the requested value when
/// it was not applied.
pub(crate) fn resolved_model_for_run(
    model_flag: Option<&str>,
    model: Option<&str>,
) -> Option<String> {
    if !crate::domain::cli_config::model_flag_selects_model(model_flag) {
        return None;
    }
    model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

/// CB43: the platform+model pair resolved at dispatch for a node run —
/// recorded on the run row itself so readers never join to the node's
/// current config. `Cli::resolve` failure (unknown platform) stores the
/// configured values as-is rather than dropping them. Check/gate nodes
/// carry no platform and record `(None, None)`.
pub(crate) fn executed_pair_for_node(node: &GraphNode) -> (Option<String>, Option<String>) {
    let platform = node
        .config
        .get("platform")
        .or_else(|| node.config.get("cli"))
        .and_then(Value::as_str);
    let model = node.config.get("model").and_then(Value::as_str);
    executed_pair_for_platform_model(platform, model)
}

/// CB43: same as [`executed_pair_for_node`] for ensemble members and hooks,
/// whose platform/model live outside a node config.
///
/// Never panics: an unknown platform (no registry entry) stores the
/// requested model as-is rather than dropping it — the record is what was
/// handed to dispatch, and the engine must not invent a gate it cannot see.
pub(crate) fn executed_pair_for_platform_model(
    platform: Option<&str>,
    model: Option<&str>,
) -> (Option<String>, Option<String>) {
    let platform = platform
        .map(str::trim)
        .filter(|platform| !platform.is_empty())
        .map(str::to_string);
    if platform.is_none() {
        return (None, None);
    }
    let trimmed_model = model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string);
    let flag = platform.as_deref().and_then(cli_model_flag);
    match flag {
        Some(flag) => (
            platform,
            resolved_model_for_run(flag.as_deref(), trimmed_model.as_deref()),
        ),
        None => (platform, trimmed_model),
    }
}

/// CB43: the registry's `model_flag` for `platform`, without panicking
/// (unlike `Cli::strategy`). `None` means unknown — no home directory or no
/// registry entry — and the caller stores the model as-is.
pub(crate) fn cli_model_flag(platform: &str) -> Option<Option<String>> {
    let home = dirs::home_dir()?;
    let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
    config.get_cli(platform).map(|cli| cli.model_flag.clone())
}

#[expect(clippy::too_many_arguments)]
pub(crate) async fn run_agent_process(
    db: &Database,
    run_id: &str,
    cli: &Cli,
    strategy: &crate::domain::cli_strategy::CliStrategy,
    node: &GraphNode,
    prompt: &str,
    model: Option<&str>,
    effort: Option<&str>,
    workdir: &str,
    timeout_minutes: u64,
    resume_session_id: Option<&str>,
) -> Result<NodeExecution> {
    let capture = prepare_session_capture(db, run_id, strategy, workdir, resume_session_id).await;

    // Opt-in only (per node config), never a default — see [`CliConfig::trust_flag`].
    let trust_workdir = node
        .config
        .get("trust_workdir")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let effort_not_applied = effort_notice(cli.as_str(), strategy, effort);
    let model_not_applied = model_notice(cli.as_str(), strategy, model);
    let outcome = spawn_and_wait_cli_process(
        strategy,
        prompt,
        model,
        effort,
        workdir,
        timeout_minutes,
        capture.set_at_spawn.as_deref(),
        resume_session_id,
        trust_workdir,
        // CB84: cold starts stamp `canopy-<run_id>` as the session title so a
        // terminated run whose id was never captured can still be found by
        // title. Resumes continue an existing session and must not rename it.
        if resume_session_id.is_none() {
            Some(run_id)
        } else {
            None
        },
        |pid| {
            let _ = db.set_graph_run_pid(run_id, pid as i64, crate::system::boot_id().as_deref());
        },
    )
    .await;

    // Attribute the session the run just created (RS1 phase 2). Only when a
    // pre-snapshot was taken AND the process actually started — a spawn `Err`
    // means nothing ran, so there's nothing new to attribute. Never affects
    // the verdict: capture only ever writes `session_id`, and any failure
    // leaves it NULL.
    if let Some(pre) = capture.pre {
        if outcome.is_ok() {
            capture_session_id_after_run(db, run_id, strategy, workdir, &pre).await;
        }
    }

    match outcome {
        Err(error) => {
            let mut exec = agent_spawn_failure(node, cli, model, &error);
            stamp_effort_model_notices(
                &mut exec.output,
                effort,
                effort_not_applied.as_ref(),
                model_not_applied.as_ref(),
            );
            Ok(exec)
        }
        Ok(CliProcessOutcome::TimedOut) => {
            let mut output = serde_json::json!({
                "kind": "agent",
                "node_id": node.id,
                "cli": cli.as_str(),
                "model": model,
                "error": "timed out",
                "timeout_minutes": timeout_minutes,
                "prompt_source": agent_prompt_source(&node.config),
            });
            stamp_effort_model_notices(
                &mut output,
                effort,
                effort_not_applied.as_ref(),
                model_not_applied.as_ref(),
            );
            let _ = db.update_graph_run_result(
                run_id,
                GraphRunStatus::Fail,
                Some(&output),
                Some(chrono::Utc::now()),
            );
            Ok(NodeExecution {
                status: GraphRunStatus::Fail,
                output,
                summary: format!(
                    "Agent node '{}' timed out after {timeout_minutes}m.",
                    node.name
                ),
            })
        }
        Ok(CliProcessOutcome::Finished {
            exit_code,
            stdout,
            stderr,
        }) => {
            // The self-report tool call (if any) lands on the run row over
            // MCP while the process is still alive, so by the time it has
            // exited and `wait` has returned here, the row already carries
            // its final word — reusing `self_reported_execution`'s own
            // "did it self-report" check (`run.status != Running`) rather
            // than re-deriving it.
            let run = db.get_graph_run(run_id)?;
            let self_reported = self_reported_execution(run.as_ref(), node).is_some();
            let mut exec = agent_finished_execution(
                node,
                cli,
                model,
                exit_code,
                &stdout,
                &stderr,
                self_reported,
            );
            stamp_effort_model_notices(
                &mut exec.output,
                effort,
                effort_not_applied.as_ref(),
                model_not_applied.as_ref(),
            );
            Ok(exec)
        }
    }
}

pub(crate) struct SessionCapture {
    pub(crate) set_at_spawn: Option<String>,
    pub(crate) pre: Option<std::collections::HashSet<String>>,
}

pub(crate) async fn prepare_session_capture(
    db: &Database,
    run_id: &str,
    strategy: &crate::domain::cli_strategy::CliStrategy,
    workdir: &str,
    resume_session_id: Option<&str>,
) -> SessionCapture {
    // Resume (RS2): the run continues an existing session. Record that same
    // id on this run row and SKIP capture entirely — set-at-spawn must not
    // mint a new UUID and list-after-run must not diff, because a resume
    // creates no new session to find.
    if let Some(sid) = resume_session_id {
        let _ = db.set_graph_run_session_id(run_id, sid);
    }

    // Set-at-spawn session id capture (RS1): when the platform accepts a
    // caller-chosen session id, mint one and record it on the run row
    // before spawning — the id is known without parsing any output, and
    // stays valid for resume however the run ends. Never on a resumed spawn.
    let set_at_spawn = if resume_session_id.is_none() {
        strategy
            .session_id_set_flag
            .as_ref()
            .map(|_| uuid::Uuid::new_v4().to_string())
    } else {
        None
    };
    if let Some(sid) = set_at_spawn.as_deref() {
        let _ = db.set_graph_run_session_id(run_id, sid);
    }

    // List-after-run session id capture (RS1 phase 2): for platforms that
    // can't set the id at spawn but do expose a session-list command, snapshot
    // the set of session ids BEFORE spawning so the new one can be diffed out
    // after the run. Skipped entirely when set-at-spawn already applied
    // (`set_at_spawn.is_some()`), which takes strict precedence, or when this
    // is a resumed spawn. Best-effort: a failed snapshot (`None`) just
    // disables capture for this run.
    let pre = if resume_session_id.is_none()
        && set_at_spawn.is_none()
        && strategy.can_capture_session_after_run()
    {
        list_session_ids(strategy, workdir).await
    } else {
        None
    };

    SessionCapture { set_at_spawn, pre }
}

pub(crate) fn stamp_effort_model_notices(
    output: &mut Value,
    effort: Option<&str>,
    effort_not_applied: Option<&Value>,
    model_not_applied: Option<&Value>,
) {
    if let Some(notice) = effort_not_applied {
        if let serde_json::Value::Object(map) = output {
            map.insert("effort_not_applied".to_string(), notice.clone());
        }
    } else if let Some(e) = effort {
        if let serde_json::Value::Object(map) = output {
            map.insert(
                "effort_applied".to_string(),
                serde_json::Value::String(e.to_string()),
            );
        }
    }
    if let Some(notice) = model_not_applied {
        if let serde_json::Value::Object(map) = output {
            map.insert("model_not_applied".to_string(), notice.clone());
        }
    }
}
/// Hard cap on how long a session-list invocation may run during
/// list-after-run capture (RS1 phase 2). Capture is best-effort and must
/// never stall a run's bookkeeping, so a slow/hung list command is abandoned
/// (its process group killed via `kill_on_drop`) and the id left NULL.
pub(crate) const SESSION_LIST_TIMEOUT_SECS: u64 = 10;

/// Run the platform's session-list command (cwd = node workdir) with a short
/// timeout and return the extracted set of session ids. `None` means the
/// capability isn't configured, or the command failed / timed out — capture
/// is best-effort, so callers treat `None` as "leave the id NULL", never an
/// error. Registry-driven end to end: the subcommand, the machine-readable
/// args, and the id regex all come from the platform config.
pub(crate) async fn list_session_ids(
    strategy: &crate::domain::cli_strategy::CliStrategy,
    workdir: &str,
) -> Option<std::collections::HashSet<String>> {
    let mut cmd = build_session_list_command(strategy, workdir)?;
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(%error, "session id capture: session-list command failed to spawn");
            return None;
        }
    };

    let stdout = collect_session_list_output(child).await?;
    Some(strategy.extract_session_ids(&stdout))
}

pub(crate) fn build_session_list_command(
    strategy: &crate::domain::cli_strategy::CliStrategy,
    workdir: &str,
) -> Option<tokio::process::Command> {
    match strategy.build_session_list_command(workdir) {
        Ok(Some(cmd)) => Some(cmd),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%error, "session id capture: could not build session-list command");
            None
        }
    }
}

pub(crate) async fn collect_session_list_output(child: tokio::process::Child) -> Option<String> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(SESSION_LIST_TIMEOUT_SECS),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            Some(stdout.into_owned())
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "session id capture: session-list command errored");
            None
        }
        Err(_elapsed) => {
            // Dropping the future drops the Child; `kill_on_drop` (set in
            // `build_session_list_command`) reaps the process group.
            tracing::warn!(
                timeout_secs = SESSION_LIST_TIMEOUT_SECS,
                "session id capture: session-list command timed out"
            );
            None
        }
    }
}

/// After a run finishes, list the platform's sessions again and attribute the
/// single id that wasn't in `pre` to this run via `set_graph_run_session_id`.
/// A diff of exactly one is recorded; zero or many is logged and the id left
/// NULL (non-fatal). Same-platform parallel runs can race and produce
/// multiple new ids — that's logged, not solved. Never touches the verdict.
pub(crate) async fn capture_session_id_after_run(
    db: &Database,
    run_id: &str,
    strategy: &crate::domain::cli_strategy::CliStrategy,
    workdir: &str,
    pre: &std::collections::HashSet<String>,
) {
    let Some(post) = list_session_ids(strategy, workdir).await else {
        tracing::warn!(
            run_id,
            "session id capture: post-run session list unavailable; leaving session_id NULL"
        );
        return;
    };
    record_session_diff(db, run_id, classify_new_session(&post, pre));
}

#[derive(Clone, Copy)]
pub(crate) enum NewSessionDiff<'a> {
    One(&'a String),
    None,
    Many(usize),
}

pub(crate) fn classify_new_session<'a>(
    post: &'a std::collections::HashSet<String>,
    pre: &'a std::collections::HashSet<String>,
) -> NewSessionDiff<'a> {
    let mut new = post.difference(pre);
    match (new.next(), new.next()) {
        (Some(only), None) => NewSessionDiff::One(only),
        (None, _) => NewSessionDiff::None,
        (Some(_), Some(_)) => NewSessionDiff::Many(post.difference(pre).count()),
    }
}

pub(crate) fn record_session_diff(db: &Database, run_id: &str, diff: NewSessionDiff<'_>) {
    match diff {
        NewSessionDiff::One(only) => {
            if let Err(error) = db.set_graph_run_session_id(run_id, only) {
                tracing::warn!(run_id, %error, "session id capture: failed to persist session id");
            }
        }
        NewSessionDiff::None => tracing::warn!(
            run_id,
            "session id capture: no new session appeared; leaving session_id NULL"
        ),
        NewSessionDiff::Many(candidates) => tracing::warn!(
            run_id,
            candidates,
            "session id capture: multiple new sessions (same-platform parallel runs?); \
             cannot attribute, leaving session_id NULL"
        ),
    }
}
