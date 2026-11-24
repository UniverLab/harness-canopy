//! Who to believe: the pid file or the port.
//!
//! After a crash the pid file can name a pid the OS has since reused for
//! an unrelated process, while nothing listens on the daemon's port. The
//! rule here is the whole fix: a pid-file pid is believed only when that
//! process is alive **and** it is the process listening on the port
//! ([`super::process::resolve_port_pid`], the same predicate the managed
//! start path already uses to wait for the port). This module also owns
//! [`wait_until_ready`], the direct-spawn readiness wait that replaces the
//! old "sleep 500 ms and check the child is alive" check.

use super::process::read_pid;

/// How long [`wait_until_ready`] waits for the port to answer: 80 polls
/// of the caller's 250 ms tick.
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The belief rule: return the pid-file pid only when that process is alive
/// and it is the process listening on `port`; otherwise `None` (stale or
/// missing pid file, reused pid, or occupant that is not the pid-file pid).
///
/// `alive` and `port_pid` are parameters so tests can fake them; production
/// passes `process::is_process_running` and `process::resolve_port_pid`.
pub(crate) fn pid_file_is_live(
    data_dir: &std::path::Path,
    port: u16,
    alive: &dyn Fn(u32) -> bool,
    port_pid: &dyn Fn(u16) -> Option<u32>,
) -> Option<u32> {
    let pid = read_pid(data_dir)?;
    if alive(pid) && port_pid(port) == Some(pid) {
        Some(pid)
    } else {
        None
    }
}

/// Which pids `daemon stop` may signal: the believed pid-file pid (never an
/// unbelieved one, whose pid may have been reused by an unrelated program),
/// plus the live port occupant when it is not already listed. Pure function
/// so the selection rule is unit-testable; the I/O stays in `handle_stop`.
pub(crate) fn stop_targets(
    pid_file_pid: Option<u32>,
    believed_pid: Option<u32>,
    port_pid: Option<u32>,
    alive: &dyn Fn(u32) -> bool,
) -> Vec<u32> {
    let mut targets: Vec<u32> = Vec::new();
    if let Some(pid) = believed_pid {
        if Some(pid) == pid_file_pid {
            targets.push(pid);
        }
    }
    if let Some(pid) = port_pid {
        if alive(pid) && !targets.contains(&pid) {
            targets.push(pid);
        }
    }
    targets
}

/// Wait for the just-spawned daemon to listen on `port`, polling every
/// 250 ms for up to 20 s. `log_base_len` is the length `daemon.log` had
/// before the spawn, so the failure message only quotes lines the daemon
/// itself appended.
///
/// Returns `Ok` as soon as something listens on the port. When the child
/// dies first, returns `daemon exited during start: <line>` plus the last
/// 20 lines of the log; when 20 s pass with no listener, returns the
/// timeout error naming the log path.
///
/// The eight parameters are the plan's doing, not sprawl: every probe is
/// injected so tests stay hermetic (fake clock, fake port, no spawn).
#[expect(clippy::too_many_arguments)]
pub(crate) fn wait_until_ready(
    port: u16,
    child_pid: u32,
    log_path: &std::path::Path,
    log_base_len: u64,
    alive: &dyn Fn(u32) -> bool,
    port_open: &dyn Fn(u16) -> bool,
    tick: &dyn Fn(),
    now: &dyn Fn() -> std::time::Instant,
) -> anyhow::Result<()> {
    let deadline = now() + READY_TIMEOUT;
    loop {
        if port_open(port) {
            return Ok(());
        }
        if !alive(child_pid) {
            anyhow::bail!("{}", child_died_message(log_path, log_base_len));
        }
        if now() >= deadline {
            anyhow::bail!(
                "daemon did not become ready in 20 s (log: {})",
                log_path.display()
            );
        }
        tick();
    }
}

/// Build the `daemon exited during start: ...` message: `<line>` is the
/// first appended line containing `ERROR`, `FATAL` or `panicked`, else the
/// last non-empty appended line, else `no output`; followed by the last 20
/// lines of the log, each indented by two spaces.
fn child_died_message(log_path: &std::path::Path, log_base_len: u64) -> String {
    let whole = std::fs::read_to_string(log_path).unwrap_or_default();
    let appended = append_since(&whole, log_base_len);
    let headline = pick_headline(&appended);
    let mut message = format!("daemon exited during start: {headline}");
    for line in tail_lines(&whole) {
        message.push_str(&format!("\n  {line}"));
    }
    message
}

/// The log bytes appended since the spawn: everything from `base_len`, or
/// the whole file when it is shorter (rotated or recreated mid-start).
fn append_since(whole: &str, base_len: u64) -> String {
    let base = usize::try_from(base_len).unwrap_or(usize::MAX);
    if base <= whole.len() && whole.is_char_boundary(base) {
        whole[base..].to_string()
    } else {
        whole.to_string()
    }
}

/// First appended line containing `ERROR`, `FATAL` or `panicked`; else the
/// last non-empty appended line; else `no output`.
fn pick_headline(appended: &str) -> String {
    if let Some(line) = appended
        .lines()
        .find(|line| line.contains("ERROR") || line.contains("FATAL") || line.contains("panicked"))
    {
        return line.to_string();
    }
    if let Some(line) = appended.lines().rfind(|line| !line.trim().is_empty()) {
        return line.to_string();
    }
    "no output".to_string()
}

/// Last up to 20 lines of the log.
fn tail_lines(whole: &str) -> Vec<&str> {
    let lines: Vec<&str> = whole.lines().collect();
    let start = lines.len().saturating_sub(20);
    lines[start..].to_vec()
}

#[cfg(test)]
#[path = "pid_truth_tests.rs"]
mod pid_truth_tests;
