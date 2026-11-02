//! Log-file locations and the append helper shared by the TUI's own
//! diagnostics.
//!
//! Both files sit in the canopy log directory (`~/.canopy/logs`, with the
//! same `/tmp` fallback the agent log path uses), so a user looking for
//! agent logs finds the TUI's too.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// `~/.canopy/logs`, or `/tmp/canopy/logs` when there is no home directory.
pub(crate) fn canopy_logs_dir() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".canopy/logs"))
        .unwrap_or_else(|| PathBuf::from("/tmp/canopy/logs"))
}

/// Append one line to `path`, creating its directory. Best-effort: the panic
/// hook calls this, so it must never panic or unwind.
pub(crate) fn append_log(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{text}");
    }
}

/// `~/.canopy/logs/tui-panic.log`: panic payloads with backtraces.
pub fn panic_log_path() -> PathBuf {
    canopy_logs_dir().join("tui-panic.log")
}

/// `<data_dir>/logs/tui.log` (normally `~/.canopy/logs/tui.log`): handler
/// errors the TUI surfaced.
pub(crate) fn tui_log_path(data_dir: &Path) -> PathBuf {
    data_dir.join("logs").join("tui.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_creates_the_file_and_keeps_every_line() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("nested/tui.log");
        append_log(&path, "first");
        append_log(&path, "second");
        let content = std::fs::read_to_string(&path).expect("log");
        assert!(content.contains("first"), "log: {content}");
        assert!(content.contains("second"), "log: {content}");
    }

    #[test]
    fn log_paths_are_under_the_canopy_logs_dir() {
        assert_eq!(
            panic_log_path().file_name().and_then(|name| name.to_str()),
            Some("tui-panic.log")
        );
        let data_dir = Path::new("/tmp/scratch-canopy");
        assert_eq!(
            tui_log_path(data_dir)
                .file_name()
                .and_then(|name| name.to_str()),
            Some("tui.log")
        );
        assert!(tui_log_path(data_dir).starts_with(data_dir));
        let panic_path = panic_log_path();
        let parent = panic_path
            .parent()
            .and_then(|parent| parent.to_str())
            .unwrap_or_default();
        assert!(
            parent.ends_with(".canopy/logs") || parent.ends_with("/tmp/canopy/logs"),
            "parent: {parent}"
        );
    }
}
