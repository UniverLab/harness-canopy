//! CB85: binary identity enforcement — a platform whose resolved binary is
//! not the expected program is "not installed", never invoked.
//!
//! The registry declares `identity_args` (argv appended to the resolved
//! binary, e.g. `["--version"]`) and `identity_pattern` (a regex the combined
//! stdout+stderr must match). When both are set, every surface that resolves
//! the platform's binary for use — probe, node dispatch, `agent_add`
//! validation, doctor — checks the resolved path once per (path, mtime) and
//! reports the platform as not installed on a mismatch or a timeout.
//!
//! Split out of `cli_strategy.rs` to keep every production file within the
//! file-size gate. Sibling of `identity.rs` (CB44's diagnosis-only check,
//! which stays as it is): this module's verdict *is* enforced on dispatch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::cli_config::CliConfig;

use super::identity::IDENTITY_CHECK_TIMEOUT;

/// Maximum characters of the identity call's first output line kept in the
/// message. The evidence slot names what answered, not a transcript.
const IDENTITY_LINE_LIMIT: usize = 200;

/// The registry's declaration that a platform's binary must identify itself.
/// Mirrored onto `CliStrategy` so dispatch needs no config load.
#[derive(Debug, Clone)]
pub struct BinaryIdentity {
    /// Platform slug (`CliConfig::name`) — the message names it.
    pub platform: String,
    /// Display name (`CliConfig::display_name()`) — the message names it.
    pub display: String,
    /// Args appended to the resolved binary, e.g. `["--version"]`.
    pub args: Vec<String>,
    /// Regex the combined stdout+stderr must match.
    pub pattern: String,
}

impl BinaryIdentity {
    /// `Some` exactly when the platform declares BOTH a non-empty
    /// `identity_args` and a non-empty `identity_pattern`. `None` — the
    /// common case, and every platform until the registry ships the fields —
    /// keeps every caller on today's behaviour with zero per-call tax.
    pub fn from_cli_config(cli: &CliConfig) -> Option<Self> {
        let args = cli.identity_args.as_ref()?;
        if args.is_empty() {
            return None;
        }
        let pattern = cli.identity_pattern.as_deref()?.trim();
        if pattern.is_empty() {
            return None;
        }
        Some(Self {
            platform: cli.name.clone(),
            display: cli.display_name(),
            args: args.clone(),
            pattern: pattern.to_string(),
        })
    }
}

/// The resolved binary is not the program this platform expects (CB85):
/// `/usr/bin/blackbox` is the Blackbox X11 window manager, not the CLI.
///
/// This wording (not "platform unreachable") is the whole value of the
/// change — it ends the "blackbox is broken" misdiagnosis by naming what
/// was actually found, on every surface, with one consistent sentence.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{platform}: '{path}' is not {display} (identity check failed: {detail})")]
pub struct NotInstalled {
    pub platform: String,
    pub display: String,
    pub path: String,
    pub detail: String,
}

impl NotInstalled {
    /// The operator-facing message. One wording for probe, dispatch,
    /// `agent_add` and doctor so they can never drift apart.
    pub fn message(&self) -> String {
        self.to_string()
    }

    fn new(identity: &BinaryIdentity, resolved: &Path, detail: String) -> Self {
        Self {
            platform: identity.platform.clone(),
            display: identity.display.clone(),
            path: resolved.display().to_string(),
            detail,
        }
    }
}

/// The verdict cache: `(resolved path, mtime) -> verdict`, process-lifetime.
/// The daemon resolves the same binary on every dispatch, so the identity
/// call runs once per path+mtime; a changed mtime (a replaced or reinstalled
/// binary at the same path) re-checks.
type IdentityCache = HashMap<(PathBuf, SystemTime), Result<(), NotInstalled>>;

fn cache() -> &'static Mutex<IdentityCache> {
    static CACHE: OnceLock<Mutex<IdentityCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cache key: the resolved path plus its mtime. A path whose metadata cannot
/// be read keys on `UNIX_EPOCH` (never cached as a hit by accident — it can
/// only ever be re-checked).
fn cache_key(resolved: &Path) -> (PathBuf, SystemTime) {
    let mtime = std::fs::metadata(resolved)
        .and_then(|m| m.modified())
        .unwrap_or(UNIX_EPOCH);
    (resolved.to_path_buf(), mtime)
}

fn cached_verdict(key: &(PathBuf, SystemTime)) -> Option<Result<(), NotInstalled>> {
    let guard = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.get(key).cloned()
}

fn store_verdict(key: (PathBuf, SystemTime), verdict: Result<(), NotInstalled>) {
    let mut guard = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.insert(key, verdict);
}

/// The first non-empty line of the identity call's combined stdout+stderr,
/// trimmed and length-capped: what actually answered the name.
fn first_line(combined: &str) -> String {
    let line = combined
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no output");
    let truncated: String = line.chars().take(IDENTITY_LINE_LIMIT).collect();
    if truncated.chars().count() == line.chars().count() {
        truncated
    } else {
        format!("{truncated}…")
    }
}

fn combine(output: &std::process::Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Judge combined output against the declared regex. Fails closed: an
/// uncompilable pattern is a failure, never a silent pass.
fn verdict_from_output(
    identity: &BinaryIdentity,
    resolved: &Path,
    combined: &str,
) -> Result<(), NotInstalled> {
    match regex::Regex::new(&identity.pattern) {
        Ok(re) if re.is_match(combined) => Ok(()),
        Ok(_) => Err(NotInstalled::new(identity, resolved, first_line(combined))),
        Err(e) => Err(NotInstalled::new(
            identity,
            resolved,
            format!("invalid identity_pattern regex: {e}"),
        )),
    }
}

async fn run_check_async(resolved: &Path, identity: &BinaryIdentity) -> Result<(), NotInstalled> {
    let mut cmd = tokio::process::Command::new(resolved);
    cmd.args(&identity.args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.kill_on_drop(true);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Err(NotInstalled::new(
                identity,
                resolved,
                format!("identity check could not run: {e}"),
            ))
        }
    };
    let combined =
        match tokio::time::timeout(IDENTITY_CHECK_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(output)) => combine(&output),
            Ok(Err(e)) => {
                return Err(NotInstalled::new(
                    identity,
                    resolved,
                    format!("identity check could not run: {e}"),
                ))
            }
            Err(_) => {
                return Err(NotInstalled::new(
                    identity,
                    resolved,
                    "identity check timed out".to_string(),
                ))
            }
        };
    verdict_from_output(identity, resolved, &combined)
}

fn run_check_blocking(resolved: &Path, identity: &BinaryIdentity) -> Result<(), NotInstalled> {
    let (tx, rx) = std::sync::mpsc::channel();
    let resolved_owned = resolved.to_path_buf();
    let args = identity.args.clone();
    std::thread::spawn(move || {
        let out = std::process::Command::new(&resolved_owned)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output();
        let _ = tx.send(out);
    });
    let output = match rx.recv_timeout(IDENTITY_CHECK_TIMEOUT) {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            return Err(NotInstalled::new(
                identity,
                resolved,
                format!("identity check could not run: {e}"),
            ))
        }
        Err(_) => {
            return Err(NotInstalled::new(
                identity,
                resolved,
                "identity check timed out".to_string(),
            ))
        }
    };
    verdict_from_output(identity, resolved, &combine(&output))
}

/// Verify `resolved` is the platform's binary, through the shared cache.
/// `Ok(())` when the check passes (or when the platform declares nothing —
/// callers gate on `BinaryIdentity::from_cli_config` first).
pub async fn ensure_binary_identity(
    resolved: &Path,
    identity: &BinaryIdentity,
) -> Result<(), NotInstalled> {
    let key = cache_key(resolved);
    if let Some(cached) = cached_verdict(&key) {
        return cached;
    }
    let verdict = run_check_async(resolved, identity).await;
    store_verdict(key, verdict.clone());
    verdict
}

/// [`ensure_binary_identity`] for sync callers (doctor), same cache, same
/// 5 s timeout (enforced on a helper thread so the caller is never wedged).
pub fn ensure_binary_identity_blocking(
    resolved: &Path,
    identity: &BinaryIdentity,
) -> Result<(), NotInstalled> {
    let key = cache_key(resolved);
    if let Some(cached) = cached_verdict(&key) {
        return cached;
    }
    let verdict = run_check_blocking(resolved, identity);
    store_verdict(key, verdict.clone());
    verdict
}

/// `agent_add`/`agent_watch` validation with an injectable home directory.
///
/// `Ok(())` — and today's behaviour continues — when the canopy config
/// cannot be read, the platform is not configured, the platform declares no
/// identity fields, or its binary does not resolve (the existing
/// spawn-time paths report a missing binary). `Err` only when the platform
/// is configured, declares both identity fields, and the resolved binary is
/// a different program.
pub async fn ensure_cli_installed_in(home: &Path, platform: &str) -> Result<(), NotInstalled> {
    let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
    let Some(cli) = config.get_cli(platform) else {
        return Ok(());
    };
    let Some(identity) = BinaryIdentity::from_cli_config(cli) else {
        return Ok(());
    };
    let Ok((resolved, _step)) = cli.resolve() else {
        return Ok(());
    };
    ensure_binary_identity(&resolved, &identity).await
}

/// [`ensure_cli_installed_in`] against the daemon's home
/// (`CANOPY_HOME_OVERRIDE` first, then `dirs::home_dir`, mirroring
/// [`crate::domain::models::Cli::strategy`]).
pub async fn ensure_cli_installed(platform: &str) -> Result<(), NotInstalled> {
    let Some(home) = std::env::var_os("CANOPY_HOME_OVERRIDE")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
    else {
        return Ok(());
    };
    ensure_cli_installed_in(&home, platform).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    fn write_script(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A counting script that, for `--version`, appends a line to `counter`
    /// and echoes the version line. For other args, echoes `$1`.
    fn counting_script(dir: &tempfile::TempDir, counter: &Path) -> PathBuf {
        let counter_str = counter.to_string_lossy().to_string();
        let body = format!(
            "if [ \"$1\" = \"--version\" ]; then printf 'x\\n' >> '{counter_str}'; echo 'blackbox ai 1.2.3'; else echo \"$1\"; fi"
        );
        write_script(dir, "counting-cli", &body)
    }

    fn identity(pattern: &str) -> BinaryIdentity {
        BinaryIdentity {
            platform: "blackbox".to_string(),
            display: "BLACKBOX AI · CLI".to_string(),
            args: vec!["--version".to_string()],
            pattern: pattern.to_string(),
        }
    }

    fn cli_with_identity(
        binary: &Path,
        args: Option<Vec<&str>>,
        pattern: Option<&str>,
    ) -> CliConfig {
        CliConfig {
            name: "blackbox".to_string(),
            binary: binary.to_string_lossy().into_owned(),
            identity_args: args.map(|a| a.into_iter().map(String::from).collect()),
            identity_pattern: pattern.map(String::from),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn matching_identity_passes() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "matching-cli",
            "if [ \"$1\" = \"--version\" ]; then echo 'blackbox ai 1.2.3'; else echo \"$1\"; fi",
        );
        let result = ensure_binary_identity(&script, &identity("(?i)blackbox ?ai")).await;
        assert!(result.is_ok(), "matching identity must pass: {result:?}");
    }

    #[tokio::test]
    async fn non_matching_identity_fails_with_exact_message() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "wm-cli",
            "if [ \"$1\" = \"--version\" ]; then echo \"blackbox: another window manager is already running on display ':0'\"; else echo \"$1\"; fi",
        );
        let err = ensure_binary_identity(&script, &identity("(?i)blackbox ?ai"))
            .await
            .unwrap_err();
        assert_eq!(
            err.message(),
            format!(
                "blackbox: '{}' is not BLACKBOX AI · CLI (identity check failed: blackbox: another window manager is already running on display ':0')",
                script.display()
            ),
        );
    }

    #[tokio::test]
    async fn timeout_is_reported_as_identity_check_failed() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(&dir, "slow-cli", "sleep 10");
        let err = ensure_binary_identity(&script, &identity("(?i)blackbox"))
            .await
            .unwrap_err();
        assert!(
            err.message()
                .ends_with("(identity check failed: identity check timed out)"),
            "must report timeout as identity check failed: {err}",
        );
    }

    #[tokio::test]
    async fn cache_prevents_second_identity_call_for_same_path_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("calls");
        let script = counting_script(&dir, &counter);

        assert!(
            ensure_binary_identity(&script, &identity("(?i)blackbox ?ai"))
                .await
                .is_ok()
        );
        assert!(
            ensure_binary_identity(&script, &identity("(?i)blackbox ?ai"))
                .await
                .is_ok()
        );

        let lines = std::fs::read_to_string(&counter).unwrap().lines().count();
        assert_eq!(
            lines, 1,
            "identity check must run at most once for same path+mtime"
        );
    }

    #[tokio::test]
    async fn changed_mtime_rechecks() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("calls");
        let script = counting_script(&dir, &counter);

        assert!(
            ensure_binary_identity(&script, &identity("(?i)blackbox ?ai"))
                .await
                .is_ok()
        );
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().lines().count(),
            1,
        );

        // Bump the mtime so the cache key no longer matches. Chained so the
        // write-only handle drops before the identity check re-spawns the
        // script (an O_WRONLY handle held across exec corrupts the spawn).
        std::fs::File::options()
            .write(true)
            .open(&script)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(2)),
            )
            .unwrap();

        assert!(
            ensure_binary_identity(&script, &identity("(?i)blackbox ?ai"))
                .await
                .is_ok()
        );
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().lines().count(),
            2,
            "changed mtime must trigger a re-check",
        );
    }

    #[test]
    fn undeclared_identity_returns_none() {
        let cli_none = cli_with_identity(Path::new("/bin/true"), None, None);
        assert!(BinaryIdentity::from_cli_config(&cli_none).is_none());

        let cli_args_only =
            cli_with_identity(Path::new("/bin/true"), Some(vec!["--version"]), None);
        assert!(BinaryIdentity::from_cli_config(&cli_args_only).is_none());

        let cli_pattern_only = cli_with_identity(Path::new("/bin/true"), None, Some("x"));
        assert!(BinaryIdentity::from_cli_config(&cli_pattern_only).is_none());

        let cli_empty_args = cli_with_identity(Path::new("/bin/true"), Some(vec![]), Some("x"));
        assert!(BinaryIdentity::from_cli_config(&cli_empty_args).is_none());

        let cli_blank_pattern =
            cli_with_identity(Path::new("/bin/true"), Some(vec!["--version"]), Some("   "));
        assert!(BinaryIdentity::from_cli_config(&cli_blank_pattern).is_none());
    }

    #[tokio::test]
    async fn invalid_pattern_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(&dir, "bad-pattern-cli", "echo 'blackbox ai 1.2.3'");
        let err = ensure_binary_identity(&script, &identity("([unclosed"))
            .await
            .unwrap_err();
        assert!(
            err.message()
                .contains("identity check failed: invalid identity_pattern regex:"),
            "broken regex must fail closed: {err}",
        );
    }

    #[tokio::test]
    async fn ensure_cli_installed_in_passes_for_unconfigured_platform() {
        let home = tempfile::tempdir().unwrap();
        assert!(ensure_cli_installed_in(home.path(), "blackbox")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn ensure_cli_installed_in_fails_for_wrong_binary() {
        let home = tempfile::tempdir().unwrap();
        let script = write_script(
            &home,
            "blackbox",
            "if [ \"$1\" = \"--version\" ]; then echo \"blackbox: another window manager is already running on display ':0'\"; else echo \"$1\"; fi",
        );
        let cli = cli_with_identity(&script, Some(vec!["--version"]), Some("(?i)blackbox ?ai"));
        crate::domain::canopy_config::CanopyConfig {
            clis: vec![cli],
            ..Default::default()
        }
        .save_for_tests(&home.path().join(".canopy"))
        .unwrap();

        let err = ensure_cli_installed_in(home.path(), "blackbox")
            .await
            .unwrap_err();
        assert!(
            err.message().contains("is not"),
            "must report not-installed: {err}"
        );
        assert!(err.message().contains("identity check failed"));
    }

    #[tokio::test]
    async fn ensure_cli_installed_in_passes_for_matching_binary_and_for_undeclared_fields() {
        let home = tempfile::tempdir().unwrap();

        let matching = write_script(
            &home,
            "bb-ok",
            "if [ \"$1\" = \"--version\" ]; then echo 'blackbox ai 1.2.3'; else echo \"$1\"; fi",
        );
        let cli = cli_with_identity(&matching, Some(vec!["--version"]), Some("(?i)blackbox ?ai"));
        crate::domain::canopy_config::CanopyConfig {
            clis: vec![cli],
            ..Default::default()
        }
        .save_for_tests(&home.path().join(".canopy"))
        .unwrap();
        assert!(ensure_cli_installed_in(home.path(), "bb-ok").await.is_ok());

        let no_identity = write_script(&home, "plain-cli", "echo \"$1\"");
        crate::domain::canopy_config::CanopyConfig {
            clis: vec![cli_with_identity(&no_identity, None, None)],
            ..Default::default()
        }
        .save_for_tests(&home.path().join(".canopy"))
        .unwrap();
        assert!(ensure_cli_installed_in(home.path(), "plain-cli")
            .await
            .is_ok());
    }

    #[test]
    fn blocking_variant_shares_the_cache_with_the_async_one() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("calls");
        let script = counting_script(&dir, &counter);

        // Two blocking calls, then one async call on the same script.
        assert!(ensure_binary_identity_blocking(&script, &identity("(?i)blackbox ?ai")).is_ok());
        assert!(ensure_binary_identity_blocking(&script, &identity("(?i)blackbox ?ai")).is_ok());

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            assert!(
                ensure_binary_identity(&script, &identity("(?i)blackbox ?ai"))
                    .await
                    .is_ok()
            );
        });

        let lines = std::fs::read_to_string(&counter).unwrap().lines().count();
        assert_eq!(
            lines, 1,
            "cache must be shared between sync and async entry points"
        );
    }
}
