//! CT28: the live-output "tee" shared by the engine's spawned CLI children.
//!
//! When an engine node passes an [`OutputTee`], [`spawn_and_wait_tee`] waits
//! for the child while two `spawn_output_reader` tasks stream its stdout and
//! stderr into `graph_run_output`, then snapshots the truncated per-stream
//! tails onto the run row and deletes the run's chunks in one transaction
//! (`Database::finish_graph_run_output`). Callers that pass no tee keep the
//! old `wait_with_output` behaviour in `spawn_and_wait_cli_process`.

use std::sync::{Arc, Mutex};

use super::agent_spawn::{CliProcessOutcome, SpawnError};
use super::shell_utils::truncate_check_output;
use super::KILL_GRACE;

/// CT28: when `Some`, the child's stdout/stderr are streamed live to
/// `graph_run_output` (via `spawn_output_reader`) while the process runs,
/// and snapshotted+deleted at the end (see `finish_graph_run_output`).
pub(crate) struct OutputTee {
    pub(crate) db: crate::db::Database,
    pub(crate) run_id: String,
}

/// CT28: wait on a child whose output is being streamed, snapshot the tails,
/// delete the chunks, and build the [`CliProcessOutcome`].
#[expect(clippy::too_many_arguments)]
pub(crate) async fn spawn_and_wait_tee(
    mut child: tokio::process::Child,
    pid: Option<u32>,
    stdout_reader: tokio::task::JoinHandle<()>,
    stderr_reader: tokio::task::JoinHandle<()>,
    stdout_buf: &Arc<Mutex<String>>,
    stderr_buf: &Arc<Mutex<String>>,
    tee: OutputTee,
    timeout_minutes: u64,
) -> Result<CliProcessOutcome, SpawnError> {
    let timeout_result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_minutes * 60),
        child.wait(),
    )
    .await;

    match timeout_result {
        Ok(Ok(status)) => {
            let _ = stdout_reader.await;
            let _ = stderr_reader.await;
            let (stdout, stderr) = read_buffers(stdout_buf, stderr_buf);
            let _ = snapshot_and_delete(&tee, &stdout, &stderr);
            Ok(CliProcessOutcome::Finished {
                exit_code: status.code().unwrap_or(-1),
                stdout,
                stderr,
            })
        }
        Ok(Err(error)) => Err(SpawnError::transient(error.to_string())),
        Err(_elapsed) => {
            if let Some(pid) = pid {
                crate::daemon::process::terminate_process_group_async(pid as i64, KILL_GRACE);
            }
            // Best-effort: the readers end when the pipes close.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                let _ = stdout_reader.await;
                let _ = stderr_reader.await;
            })
            .await;
            let (stdout, stderr) = read_buffers(stdout_buf, stderr_buf);
            let _ = snapshot_and_delete(&tee, &stdout, &stderr);
            Ok(CliProcessOutcome::TimedOut)
        }
    }
}

fn read_buffers(
    stdout_buf: &Arc<Mutex<String>>,
    stderr_buf: &Arc<Mutex<String>>,
) -> (String, String) {
    let stdout = stdout_buf.lock().map(|g| g.clone()).unwrap_or_default();
    let stderr = stderr_buf.lock().map(|g| g.clone()).unwrap_or_default();
    (
        String::from_utf8_lossy(stdout.as_bytes())
            .trim()
            .to_string(),
        String::from_utf8_lossy(stderr.as_bytes())
            .trim()
            .to_string(),
    )
}

fn snapshot_and_delete(tee: &OutputTee, stdout: &str, stderr: &str) -> anyhow::Result<()> {
    let stdout_snap = truncate_check_output(stdout.to_string()).0;
    let stderr_snap = truncate_check_output(stderr.to_string()).0;
    tee.db
        .finish_graph_run_output(&tee.run_id, Some(&stdout_snap), Some(&stderr_snap))
}
