//! Port-holder naming for `daemon start` / `serve` refusals (F-13).
//!
//! When the daemon's port is already held by a process this daemon did not
//! start, the old code signalled whatever listened there. These helpers
//! name the holder instead so the start paths can fail clearly and never
//! signal a stranger.

/// Decision table for `handle_start`: whether the port state means start,
/// report the already-running daemon, or refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartDecision {
    Start,
    AlreadyRunning,
    Refuse,
}

/// Pure decision behind `handle_start`'s refusal: a believed pid-file daemon
/// or the service manager's own MainPID holding the port means the daemon
/// is already running; any other occupant — or an unidentifiable holder
/// detected via a bind probe (`in_use`) — means refuse.
pub(crate) fn start_decision(
    occupant: Option<u32>,
    in_use: bool,
    pidfile_live: bool,
    manager_pid: Option<u32>,
) -> StartDecision {
    if pidfile_live {
        return StartDecision::AlreadyRunning;
    }
    match occupant {
        Some(pid) if Some(pid) == manager_pid => StartDecision::AlreadyRunning,
        Some(_) => StartDecision::Refuse,
        None if in_use => StartDecision::Refuse,
        None => StartDecision::Start,
    }
}

/// Best-effort command line of `pid` for the refusal message: Linux reads
/// `/proc/<pid>/cmdline` directly, other platforms fall back to `ps`.
/// Anything unreadable reports `unknown command`.
pub(crate) fn holder_command(pid: u32) -> String {
    match read_holder_command(pid) {
        Some(raw) => normalise_command(&raw.replace('\0', " ")),
        None => "unknown command".to_string(),
    }
}

/// Raw command line of `pid`: `/proc/<pid>/cmdline` on Linux, `ps` output
/// elsewhere. `None` when the process cannot be read (exited, or owned by
/// another user).
#[cfg(target_os = "linux")]
fn read_holder_command(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/cmdline")).ok()
}

#[cfg(not(target_os = "linux"))]
fn read_holder_command(pid: u32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Trim a raw command line and cut it to 120 characters so the refusal
/// message stays one readable line.
fn normalise_command(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "unknown command".to_string();
    }
    trimmed.chars().take(120).collect()
}

/// True when binding `port` on loopback fails with `AddrInUse` — the probe
/// that sees holders `resolve_port_pid` cannot (processes owned by other
/// users are silently skipped by the `/proc` fd scan).
pub(crate) fn port_in_use(port: u16) -> bool {
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(_) => false,
        Err(e) => e.kind() == std::io::ErrorKind::AddrInUse,
    }
}

/// Refusal text for a port held by a stranger. `command_of` renders the
/// occupant's command line (normally [`holder_command`]); it is a
/// parameter so tests can use fake probes.
pub(crate) fn holder_message(
    port: u16,
    occupant: Option<u32>,
    command_of: &dyn Fn(u32) -> String,
) -> String {
    match occupant {
        Some(pid) => format!(
            "port {port} is held by pid {pid} ({}), which this daemon did not start; \
             run canopy daemon stop to stop it, or pick another port with --port",
            command_of(pid)
        ),
        None => format!(
            "port {port} is in use by a process that could not be identified \
             (likely another user's); pick another port with --port"
        ),
    }
}

/// Map a bind failure to the holder refusal (`Some`) when the port is
/// taken, or `None` when the error is anything else and must propagate.
/// The `serve` bind path and the bind-error test share this helper.
pub(crate) fn bind_refusal(
    port: u16,
    err: &std::io::Error,
    occupant: Option<u32>,
    command_of: &dyn Fn(u32) -> String,
) -> Option<String> {
    if err.kind() == std::io::ErrorKind::AddrInUse {
        Some(holder_message(port, occupant, command_of))
    } else {
        None
    }
}

/// Turn a `serve` bind failure into its final error: `AddrInUse` resolves
/// the occupant here, logs the refusal as `ERROR` (so `daemon.log` carries
/// it for `daemon start` to quote) and returns it; any other error
/// propagates untouched. Tests drive [`bind_refusal`] with fake probes.
pub(crate) fn serve_bind_error(
    port: u16,
    err: std::io::Error,
    occupant: Option<u32>,
) -> anyhow::Error {
    if let Some(refusal) = bind_refusal(port, &err, occupant, &holder_command) {
        tracing::error!("{refusal}");
        anyhow::anyhow!(refusal)
    } else {
        err.into()
    }
}

#[cfg(test)]
#[path = "port_holder_tests.rs"]
mod tests;
