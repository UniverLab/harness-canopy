//! Shell command execution for check nodes and command hooks.

use super::*;
use crate::daemon::cpu_quota;
use crate::db::Database;
use crate::domain::graphs::GraphRunStatus;
use anyhow::Context;

/// Result of [`execute_shell_command`] — reused by both check nodes and
/// command hooks so there is exactly one way to run a command in the
/// codebase.
pub(crate) struct ShellCommandResult {
    pub(crate) status: GraphRunStatus,
    /// Check-shaped output JSON, with `stdout`/`stderr` already truncated to
    /// tail of `graphs.check_output_max_kib` KiB (CB5 contract).
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
    // FR4: size the child's worker counts to the daemon's CPU budget.
    cpu_quota::export_quota_env(&mut process, cpu_quota::loaded_quota_percent());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// FR4: every check/command-hook spawn sizes the child's worker counts to
    /// the daemon's CPU budget. The expectation is derived from the same
    /// loader the spawn uses, so this holds on any machine — and fails
    /// with the export removed, since nothing else sets all three.
    #[tokio::test]
    async fn shell_spawn_exports_quota_worker_counts_to_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let workdir = dir.path().to_string_lossy().to_string();

        let result = execute_shell_command(
            &db,
            "run-quota-env",
            "printf '%s\\n%s\\n%s\\n' \"$CARGO_BUILD_JOBS\" \"$NEXTEST_TEST_THREADS\" \"$RUST_TEST_THREADS\"",
            &workdir,
            30,
            &[],
        )
        .await
        .unwrap();

        assert!(!result.timed_out);
        assert_eq!(result.status, GraphRunStatus::Pass);
        let expected = cpu_quota::cores_from_quota(cpu_quota::loaded_quota_percent()).to_string();
        assert_eq!(
            result.full_stdout.lines().collect::<Vec<_>>(),
            vec![expected.as_str(), expected.as_str(), expected.as_str()],
            "child must see the quota worker counts: {}",
            result.full_stdout
        );
    }
}
