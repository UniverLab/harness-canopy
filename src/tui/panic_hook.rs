//! Panic hook: restore the terminal and log the panic.
//!
//! Without this a panic leaves the terminal in raw mode on the alternate
//! screen. Installed first in [`run_tui`](crate::tui::run_tui), before raw
//! mode is ever enabled.

use std::path::{Path, PathBuf};

use super::logging::{append_log, panic_log_path};

/// Append one panic report; best-effort, never panics itself.
pub fn write_panic_log(path: &Path, text: &str) {
    append_log(path, text);
}

/// One panic report: timestamp, payload, backtrace.
pub(crate) fn format_panic_report(payload: &str, backtrace: &str) -> String {
    format!(
        "{}\npayload: {payload}\nbacktrace:\n{backtrace}",
        chrono::Utc::now().to_rfc3339()
    )
}

/// Install the hook: restore the terminal, append the report, then run the
/// default hook so the panic still prints.
pub fn install_panic_hook() {
    install_panic_hook_at(panic_log_path());
}

/// [`install_panic_hook`] with an explicit log path, so a test writes to a
/// scratch file instead of the user's log directory.
fn install_panic_hook_at(path: PathBuf) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal_best_effort();
        // Forced: `capture()` honours `RUST_BACKTRACE` and the log would
        // usually hold "disabled backtrace" instead of any frames.
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        write_panic_log(
            &path,
            &format_panic_report(&panic_payload(info), &backtrace),
        );
        default(info);
    }));
}

/// The payload as text, or the location when it is not a string.
fn panic_payload(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(text) = info.payload().downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = info.payload().downcast_ref::<String>() {
        text.clone()
    } else {
        match info.location() {
            Some(location) => format!("non-string panic at {location}"),
            None => "non-string panic at unknown location".to_string(),
        }
    }
}

/// Leave raw mode, alternate screen, mouse capture and bracketed paste.
/// Every step is best-effort: this runs inside a panic.
fn restore_terminal_best_effort() {
    use ratatui::crossterm::{
        event::{DisableBracketedPaste, DisableMouseCapture},
        execute,
        terminal::{disable_raw_mode, LeaveAlternateScreen},
    };
    let _ = disable_raw_mode();
    let _ = execute!(
        std::io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_backtrace_captures_frames_without_the_env_flag() {
        // The hook must log real frames on a plain user run, where
        // `RUST_BACKTRACE` is unset and `Backtrace::capture()` stays disabled.
        assert_eq!(
            std::backtrace::Backtrace::force_capture().status(),
            std::backtrace::BacktraceStatus::Captured
        );
    }

    #[test]
    fn report_format_carries_payload_and_backtrace() {
        let text = format_panic_report("boom-payload", "fake-backtrace");
        assert!(text.contains("boom-payload"), "report: {text}");
        assert!(text.contains("fake-backtrace"), "report: {text}");
    }

    #[test]
    fn write_appends_to_the_log_path() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("tui-panic.log");
        write_panic_log(&path, "first");
        write_panic_log(&path, "second");
        let content = std::fs::read_to_string(&path).expect("panic log");
        assert!(content.contains("first"), "log: {content}");
        assert!(content.contains("second"), "log: {content}");
    }

    #[test]
    fn installed_hook_appends_the_panic_to_the_log() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("tui-panic.log");
        let previous = std::panic::take_hook();
        install_panic_hook_at(path.clone());
        let outcome = std::panic::catch_unwind(|| panic!("hook-fixture-payload"));
        std::panic::set_hook(previous);
        assert!(outcome.is_err(), "the fixture panic must unwind");
        let content = std::fs::read_to_string(&path).expect("hook wrote the log");
        assert!(content.contains("hook-fixture-payload"), "log: {content}");
        assert!(content.contains("backtrace:"), "log: {content}");
    }

    #[test]
    fn log_path_is_tui_panic_log_under_the_canopy_logs_dir() {
        let path = crate::tui::logging::panic_log_path();
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("tui-panic.log")
        );
        let parent = path
            .parent()
            .and_then(|parent| parent.to_str())
            .unwrap_or_default();
        assert!(
            parent.ends_with(".canopy/logs") || parent.ends_with("/tmp/canopy/logs"),
            "parent: {parent}"
        );
    }
}
