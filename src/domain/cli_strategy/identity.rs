//! Binary identity verification (CB44): confirm a resolved binary really is
//! the platform it was resolved for, before dispatch/probe trusts it. Split
//! out of `cli_strategy.rs` to keep every production file within the
//! file-size gate.

use std::path::{Path, PathBuf};

use crate::domain::cli_config::CliConfig;

/// How long an identity check may run before it is treated as a failure.
/// Diagnosis-only (probe/doctor, CB44) — never on dispatch.
pub const IDENTITY_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Maximum characters of captured output kept in a [`WrongBinaryError`].
pub const IDENTITY_CHECK_OUTPUT_LIMIT: usize = 500;

/// The resolved binary does not identify as the platform it was resolved
/// for (CB44): the registry's `identity_check` ran against the resolved
/// path and its output did not contain the expected substring.
///
/// The captured output is evidence for the report and nothing else — callers
/// must not scrape it for quota/model meaning.
#[derive(Debug, thiserror::Error)]
#[error("Resolved '{resolved}' which does not identify as the {platform} CLI (expected '{expected}' in output; saw: {output}")]
pub struct WrongBinaryError {
    pub platform: String,
    pub resolved: PathBuf,
    pub expected: String,
    pub output: String,
}

impl WrongBinaryError {
    /// The full failure text: absolute path, platform, expected substring,
    /// invoked command, and truncated evidence. This wording (not "platform
    /// unreachable") is the whole value of CB44 — it ends the months-long
    /// "blackbox is broken" misdiagnosis by naming what was actually found.
    pub fn report(&self, binary: &str, cmd: &str) -> String {
        format!(
            "Resolved '{}', which does not identify as the {} CLI (expected '{}' in output of '{} {}'; saw: {})",
            self.resolved.display(),
            self.platform,
            self.expected,
            binary,
            cmd,
            self.output,
        )
    }
}

/// Truncate captured output to [`IDENTITY_CHECK_OUTPUT_LIMIT`] chars for
/// the report, keeping the head (where `--version`-shaped output lives).
pub fn truncate_identity_output(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.len() <= IDENTITY_CHECK_OUTPUT_LIMIT {
        return trimmed.to_string();
    }
    format!("{}…", &trimmed[..IDENTITY_CHECK_OUTPUT_LIMIT])
}

/// Case-insensitive substring check of combined stdout+stderr against the
/// registry's expected token. Pure so unit tests can pin the matching rule
/// without spawning a process.
fn identity_output_matches(combined: &str, expected: &str) -> bool {
    combined.to_lowercase().contains(&expected.to_lowercase())
}

fn wrong_binary_error(
    cli: &CliConfig,
    resolved: &Path,
    expected: &str,
    output: &str,
) -> WrongBinaryError {
    WrongBinaryError {
        platform: cli.name.clone(),
        resolved: resolved.to_path_buf(),
        expected: expected.to_string(),
        output: truncate_identity_output(output),
    }
}

/// Synchronous identity check for sync contexts (doctor). Runs
/// `<resolved> <check.cmd>` with stdin nulled, no network, no credentials,
/// and judges combined stdout+stderr against `check.contains`.
///
/// - `Ok(())` when the platform declares no check (backward compatible) or
///   the output contains the expected substring.
/// - Never searches for another candidate binary: reports what was found
///   and lets a human point at the right one via the absolute-path override.
/// - Never called on dispatch (constraint 3) — probe/doctor only.
pub fn verify_identity(
    cli: &CliConfig,
    resolved: &Path,
) -> std::result::Result<(), WrongBinaryError> {
    let Some(check) = cli.identity_check.as_ref() else {
        return Ok(());
    };
    let args = match shell_words::split(&check.cmd) {
        Ok(args) => args,
        Err(e) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                &format!("failed to parse identity check command: {e}"),
            ));
        }
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let resolved_owned = resolved.to_path_buf();
    std::thread::spawn(move || {
        let out = std::process::Command::new(&resolved_owned)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output();
        let _ = tx.send(out);
    });
    let output = match rx.recv_timeout(IDENTITY_CHECK_TIMEOUT) {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                &format!("failed to run identity check: {e}"),
            ));
        }
        Err(_) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                "identity check timed out",
            ));
        }
    };
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if identity_output_matches(&combined, &check.contains) {
        Ok(())
    } else {
        Err(wrong_binary_error(
            cli,
            resolved,
            &check.contains,
            &combined,
        ))
    }
}

/// Async identity check for async contexts (probe). Same rule as
/// [`verify_identity`]: `<resolved> <check.cmd>` judged against
/// `check.contains`, case-insensitively, on combined stdout+stderr.
pub async fn verify_identity_async(
    cli: &CliConfig,
    resolved: &Path,
) -> std::result::Result<(), WrongBinaryError> {
    let Some(check) = cli.identity_check.as_ref() else {
        return Ok(());
    };
    let args = match shell_words::split(&check.cmd) {
        Ok(args) => args,
        Err(e) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                &format!("failed to parse identity check command: {e}"),
            ));
        }
    };
    let mut cmd = tokio::process::Command::new(resolved);
    cmd.args(&args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.kill_on_drop(true);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                &format!("failed to run identity check: {e}"),
            ));
        }
    };
    let output = match tokio::time::timeout(IDENTITY_CHECK_TIMEOUT, child.wait_with_output()).await
    {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                &format!("failed to run identity check: {e}"),
            ));
        }
        Err(_) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                "identity check timed out",
            ));
        }
    };
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if identity_output_matches(&combined, &check.contains) {
        Ok(())
    } else {
        Err(wrong_binary_error(
            cli,
            resolved,
            &check.contains,
            &combined,
        ))
    }
}
