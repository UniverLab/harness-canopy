//! Explicit canopy updates and read-only release notices.
//!
//! The TUI may perform a throttled, read-only release lookup in the
//! background.  Only [`run_update`] is allowed to download and replace the
//! executable, and it always asks first (unless `--yes` was supplied).

mod notice;
mod restart;

#[cfg(test)]
mod tests;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub(crate) use notice::maybe_spawn_update_notice;
pub(crate) use notice::update_available_tag;
pub use restart::{restart_daemon_after_update, RealUpdateDaemonOps, UpdateDaemonOps};

/// The GitHub repository whose stable releases contain canopy binaries.
pub const GITHUB_REPO: &str = "UniverLab/harness-canopy";
const CHECK_INTERVAL_SECS: u64 = 24 * 3600;
const LAST_CHECK_FILE: &str = "last_update_check.txt";

/// The release fields needed to select a stable, published binary.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct GitHubRelease {
    pub tag_name: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
}

/// Injectable release JSON lookup used by the update core and TUI notice.
pub trait ReleaseFetcher {
    fn get(&self, url: &str) -> Result<String>;
}

/// Production release lookup.  All network and HTTP-status handling lives
/// behind [`ReleaseFetcher`], so unit tests can use a deterministic fake.
pub struct RealFetcher;

impl ReleaseFetcher for RealFetcher {
    fn get(&self, url: &str) -> Result<String> {
        let response = reqwest::blocking::Client::new()
            .get(url)
            .header("User-Agent", "canopy-update")
            .send()
            .context("failed to fetch GitHub releases")?;
        let status = response.status();
        if !status.is_success() {
            bail!("GitHub releases request failed: HTTP {status}");
        }
        response
            .text()
            .context("failed to read GitHub releases response")
    }
}

/// Injectable binary downloader.  The archive is decoded only after this
/// seam returns, keeping the updater tests entirely offline.
pub trait BinaryDownloader {
    fn download(&self, url: &str) -> Result<Vec<u8>>;
}

/// Production binary downloader.
pub struct RealDownloader;

impl BinaryDownloader for RealDownloader {
    fn download(&self, url: &str) -> Result<Vec<u8>> {
        let response = reqwest::blocking::Client::new()
            .get(url)
            .header("User-Agent", "canopy-update")
            .send()
            .context("failed to download update")?;
        let status = response.status();
        if !status.is_success() {
            bail!("Download failed: HTTP {status}");
        }
        Ok(response
            .bytes()
            .context("failed to read update archive")?
            .to_vec())
    }
}

/// Dependencies for the hermetic update command path.  The real command
/// uses the same flow with production I/O; tests provide all fallible
/// external facts here and never contact GitHub or a process manager.
pub struct UpdateDeps<'a> {
    pub current: &'a str,
    pub releases: std::result::Result<Vec<GitHubRelease>, String>,
    pub exe: &'a Path,
    pub cargo_bin: &'a Path,
    pub target: std::result::Result<&'a str, String>,
    pub downloader: &'a dyn BinaryDownloader,
    pub confirm: &'a dyn Fn() -> bool,
    pub daemon: &'a dyn UpdateDaemonOps,
    pub graph_running: bool,
    pub confirm_restart: &'a dyn Fn() -> bool,
}

// ── Public update entry points ───────────────────────────────────

/// Check for and, after consent, install the latest stable release.
///
/// The returned integer is the process exit code: `0` means no update was
/// installed (including a declined prompt or a cargo-installed binary), and
/// `1` is reserved for an available update in `--check` mode or a missing
/// binary in the downloaded archive.
pub fn run_update(check: bool, yes: bool) -> Result<i32> {
    let current = current_version();
    let releases = fetch_releases_with(&RealFetcher)?;

    // The first pass is intentionally limited to the network result.  The
    // same hermetic core below owns all user-visible output and consent, so
    // `--check` cannot touch a local path or daemon before it returns.
    let latest = select_latest_stable(&releases, current);
    if latest.is_none() || check {
        let placeholder = RealUpdateDaemonOps::new(PathBuf::new(), 0, PathBuf::new(), None);
        let deps = UpdateDeps {
            current,
            releases: Ok(releases),
            exe: Path::new("/tmp/canopy-update-test/canopy"),
            cargo_bin: Path::new("/tmp/canopy-update-test/not-cargo"),
            target: Ok("x86_64-unknown-linux-gnu"),
            downloader: &RealDownloader,
            confirm: &|| false,
            daemon: &placeholder,
            graph_running: false,
            confirm_restart: &|| false,
        };
        return run_update_with(check, yes, &deps);
    }

    // An actual install needs the executable and target facts.  Resolve them
    // only after the read-only check has established that a newer release
    // exists.  All status output, the cargo guard, and the prompt live in
    // the hermetic core below so unit tests exercise the same code.

    // Daemon facts are read once for the injected restart operation.  The
    // operation itself is not invoked until after replacement by the core.
    let latest = latest.expect("newer release was established above");
    let exe = std::env::current_exe().context("failed to locate canopy executable")?;
    let cargo_bin = cargo_bin_dir();
    let target = resolve_target()?;
    let data_dir = crate::ensure_data_dir()?;
    let port = crate::daemon::cli::configured_port(&data_dir);
    let graph_running = daemon_has_running_graph(&data_dir);
    let ops = RealUpdateDaemonOps::new(data_dir, port, exe.clone(), Some(port));
    let deps = UpdateDeps {
        current,
        releases: Ok(releases),
        exe: &exe,
        cargo_bin: &cargo_bin,
        target: Ok(target),
        downloader: &RealDownloader,
        confirm: &|| {
            inquire::Confirm::new(&format!("Update to {latest}? [y/N]"))
                .with_default(false)
                .prompt()
                .unwrap_or(false)
        },
        daemon: &ops,
        graph_running,
        confirm_restart: &|| {
            inquire::Confirm::new(
                "A graph is running; restarting the daemon interrupts it. Restart now? [y/N]",
            )
            .with_default(false)
            .prompt()
            .unwrap_or(false)
        },
    };
    run_update_core(false, yes, &deps, true, true)
}

/// Hermetic update flow used by unit tests and embedders.  It intentionally
/// has no network, process-manager, database, or filesystem setup step;
/// callers provide those facts through [`UpdateDeps`].
pub fn run_update_with(check: bool, yes: bool, deps: &UpdateDeps<'_>) -> Result<i32> {
    run_update_core(check, yes, deps, true, false)
}

fn run_update_core(
    check: bool,
    yes: bool,
    deps: &UpdateDeps<'_>,
    print_status: bool,
    record_check_after_replace: bool,
) -> Result<i32> {
    let current = deps.current;
    let releases = deps
        .releases
        .as_ref()
        .map_err(|error| anyhow!("release lookup failed: {error}"))?;
    let latest = select_latest_stable(releases, current);

    let Some(latest) = latest else {
        if print_status {
            println!("canopy {current} is up to date");
        }
        return Ok(0);
    };
    if print_status {
        println!("canopy {current} → {latest}");
    }

    if check {
        return Ok(1);
    }

    if is_cargo_installed(deps.exe, deps.cargo_bin) {
        println!("installed with cargo — run: cargo install harness-canopy --force");
        return Ok(0);
    }

    let target = deps
        .target
        .as_ref()
        .map_err(|error| anyhow!("target resolution failed: {error}"))?;
    if !yes && !(deps.confirm)() {
        println!("Aborted.");
        return Ok(0);
    }

    let tmp = tempfile::tempdir().context("failed to create update staging directory")?;
    let tmp_bin = tmp.path().join("canopy-new");
    if !download_and_extract_with(deps.downloader, &latest, target, &tmp_bin)? {
        eprintln!("  ✗ Binary not found in archive");
        return Ok(1);
    }

    replace_binary(&tmp_bin, deps.exe)?;
    if record_check_after_replace {
        record_check()?;
    }
    println!("✓ updated to {latest}");

    if deps.daemon.is_running() {
        restart_daemon_after_update(deps.daemon, deps.graph_running, deps.confirm_restart)?;
    }
    Ok(0)
}

/// Read-only TUI helper.  Production delegates to the injectable form so
/// the no-replacement boundary is the same code the tests exercise: the
/// downloader is accepted and never touched.
pub fn check_and_update_notice() -> Option<String> {
    check_and_update_notice_with(&RealFetcher, &RealDownloader)
}

/// Injectable form of [`check_and_update_notice`].  Network failures are
/// quiet, as required for the TUI startup path.
pub fn check_and_update_notice_with(
    fetcher: &dyn ReleaseFetcher,
    _downloader: &dyn BinaryDownloader,
) -> Option<String> {
    let latest = fetch_latest_stable_with(fetcher, current_version())
        .ok()
        .flatten();
    if let Some(tag) = latest.as_deref() {
        notice::store_update_tag(tag);
    }
    latest
}

// ── Throttle ────────────────────────────────────────────────────

fn should_check() -> bool {
    let Ok(data_dir) = crate::ensure_data_dir() else {
        return true;
    };
    let Ok(content) = std::fs::read_to_string(data_dir.join(LAST_CHECK_FILE)) else {
        return true;
    };
    let Ok(last) = content.trim().parse::<u64>() else {
        return true;
    };
    let Ok(now) = now_secs() else {
        return true;
    };
    now.saturating_sub(last) >= CHECK_INTERVAL_SECS
}

fn record_check() -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let ts = now_secs()?;
    std::fs::write(data_dir.join(LAST_CHECK_FILE), ts.to_string())?;
    Ok(())
}

fn now_secs() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock error")?
        .as_secs())
}

// ── Version helpers ─────────────────────────────────────────────

fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn is_stable_version(tag: &str) -> bool {
    let value = tag.trim_start_matches('v');
    !value.is_empty() && value.chars().all(|c| c.is_ascii_digit() || c == '.')
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> Vec<u32> {
        s.trim_start_matches('v')
            .split('.')
            .filter_map(|part| part.parse().ok())
            .collect()
    };
    let (pa, pb) = (parse(a), parse(b));
    let len = pa.len().max(pb.len());
    for index in 0..len {
        let comparison = pa
            .get(index)
            .copied()
            .unwrap_or(0)
            .cmp(&pb.get(index).copied().unwrap_or(0));
        if comparison != std::cmp::Ordering::Equal {
            return comparison;
        }
    }
    std::cmp::Ordering::Equal
}

/// Select the newest stable release strictly newer than `current`.
pub fn select_latest_stable(releases: &[GitHubRelease], current: &str) -> Option<String> {
    releases
        .iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter(|release| is_stable_version(&release.tag_name))
        .filter(|release| compare_versions(&release.tag_name, current).is_gt())
        .max_by(|a, b| compare_versions(&a.tag_name, &b.tag_name))
        .map(|release| release.tag_name.clone())
}

// ── Release lookup ──────────────────────────────────────────────

fn fetch_releases_with(fetcher: &dyn ReleaseFetcher) -> Result<Vec<GitHubRelease>> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases");
    let body = fetcher.get(&url)?;
    serde_json::from_str(&body).context("failed to parse releases JSON")
}

/// Fetch and select the newest stable release through an injected fetcher.
pub fn fetch_latest_stable_with(
    fetcher: &dyn ReleaseFetcher,
    current: &str,
) -> Result<Option<String>> {
    let releases = fetch_releases_with(fetcher)?;
    Ok(select_latest_stable(&releases, current))
}

// ── Target and installation-path helpers ────────────────────────

/// Resolve a target triple from explicit OS/architecture inputs.
pub fn target_for(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        _ => bail!("unsupported target: {arch}-{os}"),
    }
}

/// Resolve the full Rust target triple used by the published release assets.
pub fn resolve_target() -> Result<&'static str> {
    target_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// Build the exact release asset name, retaining the tag's leading `v`.
pub fn asset_name(tag: &str, target: &str) -> String {
    format!("canopy-{tag}-{target}.tar.gz")
}

/// Return the cargo bin directory selected by the environment, falling back
/// to the conventional `$HOME/.cargo/bin` location.
pub fn cargo_bin_dir() -> PathBuf {
    std::env::var_os("CARGO_HOME")
        .filter(|value| !value.is_empty())
        .map(|value| PathBuf::from(value).join("bin"))
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".cargo")
                .join("bin")
        })
}

/// Test whether an executable lives below the cargo bin directory.  Both
/// paths are canonicalized when possible, with the raw paths as a safe
/// fallback for non-existent test paths and macOS temporary symlinks.
pub fn is_cargo_installed(exe: &Path, cargo_bin: &Path) -> bool {
    let exe = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    let cargo_bin = cargo_bin
        .canonicalize()
        .unwrap_or_else(|_| cargo_bin.to_path_buf());
    exe.starts_with(cargo_bin)
}

// ── Download, extraction, and atomic replacement ────────────────

/// Download and extract the `canopy` entry from a release tarball.
pub fn download_and_extract_with(
    downloader: &dyn BinaryDownloader,
    tag: &str,
    target: &str,
    output: &Path,
) -> Result<bool> {
    let asset = asset_name(tag, target);
    let url = format!("https://github.com/{GITHUB_REPO}/releases/download/{tag}/{asset}");
    let bytes = downloader.download(&url)?;
    let decoder = flate2::read::GzDecoder::new(bytes.as_slice());
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.file_name().is_some_and(|name| name == "canopy") {
            entry.unpack(output)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(output, std::fs::Permissions::from_mode(0o755))?;
            }
            return Ok(true);
        }
    }
    Ok(false)
}

/// Replace `current_exe` from a staged file using a same-directory rename.
/// If a platform cannot rename over an existing executable, fall back to a
/// copy after the staged file has been written beside the target.
pub fn replace_binary(staged: &Path, current_exe: &Path) -> Result<()> {
    let parent = current_exe
        .parent()
        .context("cannot determine the canopy executable directory")?;
    let temporary = tempfile::NamedTempFile::new_in(parent)
        .context("failed to create an adjacent update file")?;
    std::fs::copy(staged, temporary.path()).context("failed to stage canopy binary")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o755))?;
    }
    temporary
        .as_file()
        .sync_all()
        .context("failed to flush the staged canopy binary")?;

    if std::fs::rename(temporary.path(), current_exe).is_ok() {
        return Ok(());
    }

    // Windows cannot atomically rename over an existing file.  The command
    // is unsupported there today, but keeping this fallback makes the helper
    // safe to compile and preserves the same best-effort behavior.  The
    // NamedTempFile remains alive until this function returns, so the staged
    // bytes are still available after a failed rename.
    std::fs::copy(temporary.path(), current_exe)
        .context("failed to replace binary (copy fallback)")?;
    Ok(())
}

fn daemon_has_running_graph(data_dir: &Path) -> bool {
    let db_path = crate::domain::db_paths::database_path(data_dir);
    if !db_path.exists() {
        return false;
    }
    crate::db::Database::new_safe(&db_path, data_dir)
        .ok()
        .and_then(|db| db.has_running_graphs().ok())
        .unwrap_or(false)
}
