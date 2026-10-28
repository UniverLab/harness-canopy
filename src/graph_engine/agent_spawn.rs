use super::output_tee::{spawn_and_wait_tee, OutputTee};
use super::verdict_shutdown::{guard_wait, VerdictWatch};
use super::*;
use crate::shared::sync_identity::GraphIdentity;

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

    /// CB85: the resolved binary failed its platform identity check — a
    /// different program answering to the platform's name. Permanent: the
    /// verdict is deterministic (and cached per path+mtime), so the
    /// infra-retry budget must not be spent on it (B39).
    pub(crate) fn from_identity(error: &crate::domain::cli_strategy::NotInstalled) -> Self {
        Self {
            message: error.message(),
            permanent_reason: Some("resolved binary is not this platform (identity check failed)"),
        }
    }
}

/// Best-effort cleanup of a harness session when an agent node timed out
/// (CB84): the process group was already killed (B12), but the platform's
/// service-side session may keep running. Detached, never fails the caller.
async fn end_harness_session_on_timeout(db: &Database, run_id: &str, cli: &Cli, workdir: &str) {
    let Ok(Some(run)) = db.get_graph_run(run_id) else {
        return;
    };
    let platform = cli.as_str().to_string();
    let workdir = workdir.to_string();
    tokio::spawn(async move {
        crate::graph_engine::routing::end_harness_session(&platform, &run, &workdir).await;
    });
}

/// CB85: the platform's binary must identify as the platform before
/// anything is spawned. `None` when the platform declares no identity
/// fields, or its binary does not resolve (the build below then reports
/// the canonical `BinaryResolutionError`, unchanged). Cached per
/// path+mtime, so calling this twice for one run is a hashmap hit.
pub(crate) async fn ensure_platform_identity(
    strategy: &crate::domain::cli_strategy::CliStrategy,
) -> Option<SpawnError> {
    let identity = strategy.identity.as_ref()?;
    let resolved = crate::domain::cli_strategy::resolve_binary(&strategy.binary).ok()?;
    crate::domain::cli_strategy::ensure_binary_identity(&resolved, identity)
        .await
        .err()
        .map(|error| SpawnError::from_identity(&error))
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
///
/// `graph_identity` is the run this spawn executes for: its ids are exported
/// as `CANOPY_GRAPH_ID`/`CANOPY_NODE_ID`/`CANOPY_NODE_RUN_ID` so the agent's
/// verdict never depends on reproducing UUIDs from memory. `None` (hooks,
/// tests) scrubs all three rather than inheriting a stale identity.
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
    tee: Option<OutputTee>,
    verdict_watch: Option<VerdictWatch>,
    node_profile: bool,
    graph_identity: Option<&GraphIdentity>,
) -> Result<CliProcessOutcome, SpawnError> {
    // CB85: identity check before anything is spawned.
    if let Some(error) = ensure_platform_identity(strategy).await {
        return Err(error);
    }

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
    // Graph agent/ensemble nodes run with the node MCP tool profile so their
    // session stays far under the 128-tool provider cap. Hooks and other
    // callers pass `node_profile = false` and keep the full tool list — the
    // variable is scrubbed rather than left alone because the daemon itself
    // may run with CANOPY_MCP_PROFILE set (e.g. spawned from a node-profile
    // session), which the child would otherwise inherit.
    if node_profile {
        command.env(crate::shared::sync_identity::CANOPY_MCP_PROFILE_ENV, "node");
    } else {
        command.env_remove(crate::shared::sync_identity::CANOPY_MCP_PROFILE_ENV);
    }
    apply_graph_identity_env(&mut command, graph_identity);
    // Set the child process cwd to the node's workdir, whether or not the
    // platform supports a working-dir flag. The flag behaviour for platforms
    // that have one is unchanged.
    if !workdir.is_empty() {
        command.current_dir(workdir);
    }
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

    let mut child = command
        .spawn()
        .map_err(|error| SpawnError::from_io(&error))?;
    // Captured before the wait path takes ownership of `child`.
    let pid = child.id();
    if let Some(pid) = pid {
        on_pid(pid);
    }

    // CT28: when the caller wants live output, split the pipes into reader
    // tasks before waiting. When `tee` is None the old behaviour is
    // preserved byte-for-byte.
    let stdout_buf = Arc::new(std::sync::Mutex::new(String::new()));
    let stderr_buf = Arc::new(std::sync::Mutex::new(String::new()));
    let readers = tee.as_ref().map(|tee| {
        let out = child.stdout.take().expect("stdout piped");
        let err = child.stderr.take().expect("stderr piped");
        (
            crate::graph_engine::shell_utils::spawn_output_reader(
                out,
                tee.run_id.clone(),
                "stdout",
                tee.db.clone(),
                Arc::clone(&stdout_buf),
            ),
            crate::graph_engine::shell_utils::spawn_output_reader(
                err,
                tee.run_id.clone(),
                "stderr",
                tee.db.clone(),
                Arc::clone(&stderr_buf),
            ),
        )
    });

    let watch = verdict_watch.as_ref();
    match readers {
        Some((stdout_reader, stderr_reader)) => {
            let wait = spawn_and_wait_tee(
                child,
                pid,
                stdout_reader,
                stderr_reader,
                &stdout_buf,
                &stderr_buf,
                tee.unwrap(),
                timeout_minutes,
            );
            guard_wait(watch, pid, wait).await
        }
        None => {
            guard_wait(
                watch,
                pid,
                super::verdict_shutdown::wait_untracked(child, pid, timeout_minutes),
            )
            .await
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

/// The effort/model rejection notices for one spawn, stamped onto every
/// outcome's output so the run row records why a requested effort/model was
/// not applied.
struct AgentNotices<'a> {
    effort: Option<&'a str>,
    effort_not_applied: Option<Value>,
    model_not_applied: Option<Value>,
}

impl AgentNotices<'_> {
    fn stamp(&self, output: &mut Value) {
        stamp_effort_model_notices(
            output,
            self.effort,
            self.effort_not_applied.as_ref(),
            self.model_not_applied.as_ref(),
        );
    }
}

/// The node execution for a run whose harness process timed out: the timeout
/// output JSON (with the effort/model notices stamped), the run row recorded
/// as failed, and the summary.
fn agent_timeout_execution(
    db: &Database,
    run_id: &str,
    node: &GraphNode,
    cli: &Cli,
    model: Option<&str>,
    timeout_minutes: u64,
    notices: &AgentNotices<'_>,
) -> NodeExecution {
    let mut output = serde_json::json!({
        "kind": "agent",
        "node_id": node.id,
        "cli": cli.as_str(),
        "model": model,
        "error": "timed out",
        "timeout_minutes": timeout_minutes,
        "prompt_source": agent_prompt_source(&node.config),
    });
    notices.stamp(&mut output);
    let _ = db.update_graph_run_result(
        run_id,
        GraphRunStatus::Fail,
        Some(&output),
        Some(chrono::Utc::now()),
    );
    NodeExecution {
        status: GraphRunStatus::Fail,
        output,
        summary: format!(
            "Agent node '{}' timed out after {timeout_minutes}m.",
            node.name
        ),
    }
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
    // Opt-in only (per node config), never a default — see [`CliConfig::trust_flag`].
    let trust_workdir = node
        .config
        .get("trust_workdir")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let effort_not_applied = effort_notice(cli.as_str(), strategy, effort);
    let model_not_applied = model_notice(cli.as_str(), strategy, model);
    let notices = AgentNotices {
        effort,
        effort_not_applied,
        model_not_applied,
    };

    // CB85: fail before spawning anything — including the session-list
    // capture, which also runs the platform's binary.
    if let Some(error) = ensure_platform_identity(strategy).await {
        let mut exec = agent_spawn_failure(node, cli, model, &error);
        notices.stamp(&mut exec.output);
        return Ok(exec);
    }

    let capture = prepare_session_capture(db, run_id, strategy, workdir, resume_session_id).await;

    let verdict = VerdictWatch::for_run(db, run_id);

    let graph_identity = graph_identity_for_run(db, node, run_id)?;

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
        resume_session_id.is_none().then_some(run_id),
        |pid| {
            let _ = db.set_graph_run_pid(run_id, pid as i64, crate::system::boot_id().as_deref());
        },
        // CT28: stream the agent's live output and snapshot it at completion.
        Some(OutputTee {
            db: db.clone(),
            run_id: run_id.to_string(),
        }),
        verdict.clone(),
        true,
        graph_identity.as_ref(),
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
            notices.stamp(&mut exec.output);
            Ok(exec)
        }
        Ok(CliProcessOutcome::TimedOut) => {
            // CB84: the process group is already killed by the spawn path
            // (B12), but the opencode v2 service-side session keeps ticking
            // — end it too. Best-effort, detached, never fails the caller.
            // The run row's `session_id` was recorded at spawn (set-at-spawn
            // or resume), so no title lookup is needed here.
            end_harness_session_on_timeout(db, run_id, cli, workdir).await;
            Ok(agent_timeout_execution(
                db,
                run_id,
                node,
                cli,
                model,
                timeout_minutes,
                &notices,
            ))
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
            super::verdict_shutdown::apply_verdict_end(
                db,
                cli.as_str(),
                run.as_ref(),
                workdir,
                run_id,
                verdict.as_ref(),
                &mut exec.output,
            )
            .await;
            notices.stamp(&mut exec.output);
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

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::domain::cli_config::CliConfig;
    use crate::domain::cli_strategy::CliStrategy;

    /// Every graph node spawn — cold start and resume — must run the child in
    /// the node's workdir, even when the platform has no working-dir flag
    /// (`supports_working_dir = false`, opencode's case). This does not use the
    /// flag: a fake harness records its own `pwd -P`, and both spawns must
    /// record the workdir. With the `current_dir` call removed, the fake
    /// records the test process's cwd and this fails.
    #[tokio::test]
    async fn spawn_sets_workdir_cwd_for_cold_and_resume() {
        let workdir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let record = scratch.path().join("cwd_record.txt");
        let record_str = record.to_string_lossy().to_string();

        // Fake harness: records its physical cwd and ignores its argv.
        let fake_bin = scratch.path().join("fake-cwd-cli");
        std::fs::write(&fake_bin, format!("#!/bin/sh\npwd -P >> '{record_str}'\n")).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // `supports_working_dir = false` means no flag can carry the workdir;
        // only the child's cwd can. `session_resume_cmd` gives the resume path
        // a flag to inject.
        let cli_config = CliConfig {
            name: "fake-cwd".to_string(),
            binary: fake_bin.to_string_lossy().to_string(),
            supports_working_dir: false,
            session_resume_cmd: Some("--resume".to_string()),
            ..Default::default()
        };
        let strategy = CliStrategy::from_cli_config(&cli_config);
        let workdir_str = workdir.path().to_string_lossy().to_string();

        // Cold start.
        let cold = spawn_and_wait_cli_process(
            &strategy,
            "prompt",
            None,
            None,
            &workdir_str,
            5,
            None,
            None,
            false,
            Some("run-cold"),
            |_| {},
            None,
            None,
            true,
            None,
        )
        .await;
        assert!(
            matches!(cold, Ok(CliProcessOutcome::Finished { exit_code: 0, .. })),
            "cold spawn must succeed"
        );

        // Resume of an existing session.
        let resume = spawn_and_wait_cli_process(
            &strategy,
            "prompt",
            None,
            None,
            &workdir_str,
            5,
            None,
            Some("ses_resume"),
            false,
            Some("run-resume"),
            |_| {},
            None,
            None,
            true,
            None,
        )
        .await;
        assert!(
            matches!(resume, Ok(CliProcessOutcome::Finished { exit_code: 0, .. })),
            "resume spawn must succeed"
        );

        let expected = std::fs::canonicalize(workdir.path()).unwrap();
        let recorded = std::fs::read_to_string(&record).unwrap();
        let lines: Vec<&str> = recorded.lines().collect();
        assert_eq!(lines.len(), 2, "one cwd record per spawn: {recorded}");
        for line in lines {
            assert_eq!(
                line.trim(),
                expected.to_string_lossy(),
                "child cwd must be the node workdir, not the daemon's: {recorded}"
            );
        }
    }

    /// Spawns a fake CLI that records its `CANOPY_MCP_PROFILE` env value and
    /// returns what it recorded (empty when the variable was unset).
    async fn profile_env_seen_by_child(node_profile: bool) -> String {
        let workdir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let record = scratch.path().join("profile_record.txt");
        let record_str = record.to_string_lossy().to_string();

        let fake_bin = scratch.path().join("fake-profile-cli");
        std::fs::write(
            &fake_bin,
            format!("#!/bin/sh\nprintf '%s' \"$CANOPY_MCP_PROFILE\" > '{record_str}'\n"),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let cli_config = CliConfig {
            name: "fake-profile".to_string(),
            binary: fake_bin.to_string_lossy().to_string(),
            ..Default::default()
        };
        let strategy = CliStrategy::from_cli_config(&cli_config);
        let workdir_str = workdir.path().to_string_lossy().to_string();

        let outcome = spawn_and_wait_cli_process(
            &strategy,
            "prompt",
            None,
            None,
            &workdir_str,
            5,
            None,
            None,
            false,
            Some("run-profile"),
            |_| {},
            None,
            None,
            node_profile,
            None,
        )
        .await;
        assert!(
            matches!(
                outcome,
                Ok(CliProcessOutcome::Finished { exit_code: 0, .. })
            ),
            "spawn must succeed"
        );

        std::fs::read_to_string(&record).unwrap_or_default()
    }

    #[tokio::test]
    async fn spawn_sets_node_mcp_profile_env() {
        assert_eq!(
            profile_env_seen_by_child(true).await,
            "node",
            "agent node spawn must set CANOPY_MCP_PROFILE=node"
        );
    }

    #[tokio::test]
    async fn spawn_without_node_profile_leaves_env_unset() {
        assert_eq!(
            profile_env_seen_by_child(false).await,
            "",
            "hook spawn must keep the full tool list (no CANOPY_MCP_PROFILE)"
        );
    }

    /// Spawns a fake CLI that records its three graph-identity env values
    /// and returns what it recorded (empty per var when unset).
    async fn identity_env_seen_by_child(identity: Option<GraphIdentity>) -> Vec<String> {
        let workdir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let record = scratch.path().join("identity_record.txt");
        let record_str = record.to_string_lossy().to_string();

        let fake_bin = scratch.path().join("fake-identity-cli");
        std::fs::write(
            &fake_bin,
            format!(
                "#!/bin/sh\nprintf '%s\\n%s\\n%s\\n' \"$CANOPY_GRAPH_ID\" \"$CANOPY_NODE_ID\" \"$CANOPY_NODE_RUN_ID\" > '{record_str}'\n"
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let cli_config = CliConfig {
            name: "fake-identity".to_string(),
            binary: fake_bin.to_string_lossy().to_string(),
            ..Default::default()
        };
        let strategy = CliStrategy::from_cli_config(&cli_config);
        let workdir_str = workdir.path().to_string_lossy().to_string();

        let outcome = spawn_and_wait_cli_process(
            &strategy,
            "prompt",
            None,
            None,
            &workdir_str,
            5,
            None,
            None,
            false,
            Some("run-identity"),
            |_| {},
            None,
            None,
            true,
            identity.as_ref(),
        )
        .await;
        assert!(
            matches!(
                outcome,
                Ok(CliProcessOutcome::Finished { exit_code: 0, .. })
            ),
            "spawn must succeed"
        );

        std::fs::read_to_string(&record)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[tokio::test]
    async fn spawn_exports_graph_identity_env_to_child() {
        let seen = identity_env_seen_by_child(Some(GraphIdentity {
            graph_id: "graph-123".to_string(),
            node_id: "node-456".to_string(),
            run_id: "run-789".to_string(),
        }))
        .await;
        assert_eq!(
            seen,
            vec![
                "graph-123".to_string(),
                "node-456".to_string(),
                "run-789".to_string()
            ],
            "agent node spawn must export all three identity vars"
        );
    }

    #[tokio::test]
    async fn spawn_without_identity_scrubs_identity_env() {
        let seen = identity_env_seen_by_child(None).await;
        assert_eq!(
            seen,
            vec!["".to_string(), "".to_string(), "".to_string()],
            "a spawn with no identity must unset all three vars, never inherit them"
        );
    }
}
