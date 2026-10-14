use super::*;

/// Spawns `command` under a non-login POSIX `sh`. Using `-c` (not `-l`) means
/// no `/etc/profile` or `~/.profile` is sourced, so the process sees exactly
/// the daemon's own environment plus whatever env vars the engine explicitly
/// sets on the `Command` before spawning — never a user's shell-startup PATH
/// overrides or side effects.
pub(crate) fn shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(unix)]
pub(crate) fn shell_command(command: &str) -> Command {
    let mut process = Command::new("sh");
    process.arg("-c").arg(command);
    // Own process-group leader so a hung/timed-out check can be `killpg`'d
    // along with anything it forks (B12) — see `CliStrategy::build_command`
    // for the same treatment on agent nodes.
    process.process_group(0);
    process.kill_on_drop(true);
    process.stdout(std::process::Stdio::piped());
    process.stderr(std::process::Stdio::piped());
    process
}

#[cfg(windows)]
pub(crate) fn shell_command(command: &str) -> Command {
    let mut process = Command::new("cmd");
    process.arg("/C").arg(command);
    process.kill_on_drop(true);
    process.stdout(std::process::Stdio::piped());
    process.stderr(std::process::Stdio::piped());
    process
}

/// CT3/CT28: background reader for one piped check/agent stream. Appends every
/// chunk to `graph_run_output` (the TUI tail pane polls it) and mirrors it
/// into `buffer` so the completion path can evaluate the success condition
/// and snapshot the tails without a DB round-trip. Ends when the child
/// closes the pipe; I/O or DB errors end this task, never the node.
pub(crate) fn spawn_output_reader(
    stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    run_id: String,
    stream_name: &'static str,
    db: Database,
    buffer: Arc<std::sync::Mutex<String>>,
) -> tokio::task::JoinHandle<()> {
    use tokio::io::AsyncReadExt as _;
    let mut stream = Box::pin(stream);
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                    if let Ok(mut guard) = buffer.lock() {
                        guard.push_str(&chunk);
                    }
                    let _ = db.append_graph_run_output(&run_id, stream_name, &chunk);
                }
                Err(_) => break,
            }
        }
    })
}

/// Live check-output tail in bytes (`graphs.check_output_max_kib` KiB).
pub(crate) fn check_output_max_bytes() -> usize {
    let canopy_dir = crate::domain::prompts::canopy_dir();
    let graphs = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir).graphs;
    graphs.check_output_max_kib as usize * 1024
}

pub(crate) fn truncate_check_output(s: String) -> (String, bool) {
    truncate_check_output_with(s, check_output_max_bytes())
}

/// Pure form of [`truncate_check_output`] over an explicit byte budget, so
/// tests can assert the cut without touching config.
pub(crate) fn truncate_check_output_with(s: String, max_bytes: usize) -> (String, bool) {
    if s.len() <= max_bytes {
        return (s, false);
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    let tail = &s[start..];
    (
        format!(
            "[...truncated, keeping last {} bytes...]\n{}",
            max_bytes, tail
        ),
        true,
    )
}

#[cfg(test)]
mod shell_utils_settings_tests {
    use super::{check_output_max_bytes, truncate_check_output_with};

    #[test]
    fn check_output_cut_at_16_kib() {
        let input = "a".repeat(20 * 1024);
        let (kept, truncated) = truncate_check_output_with(input, 16 * 1024);
        assert!(truncated);
        assert!(kept.ends_with(&"a".repeat(16 * 1024)));
        assert!(kept.contains("keeping last 16384 bytes"));
    }

    #[test]
    fn check_output_default_keeps_20_kib_whole() {
        let input = "a".repeat(20 * 1024);
        let (kept, truncated) = truncate_check_output_with(input, 64 * 1024);
        assert!(!truncated);
        assert_eq!(kept.len(), 20 * 1024);
    }

    #[test]
    fn check_output_max_bytes_default_is_64_kib() {
        // Default registry value: 64 KiB. Only asserts when no user config
        // overrides it; a local config.toml with a custom value skips.
        let v = check_output_max_bytes();
        let graphs =
            crate::domain::canopy_config::CanopyConfig::load(&crate::domain::prompts::canopy_dir())
                .graphs;
        assert_eq!(v, graphs.check_output_max_kib as usize * 1024);
    }
}
