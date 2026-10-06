use super::*;

/// Every `canopy` executable found on `path_var`, in the order the shell
/// would resolve them — first match wins. Backed by `which::which_in_all`,
/// which already applies the rule this check needs: a PATH entry that
/// doesn't exist or can't be read is skipped rather than failing the whole
/// search, and a file named `canopy` without the execute bit (or, on
/// Windows, without a recognized `PATHEXT` extension) is not a match.
///
/// `which_in_all` yields one candidate per matching `PATH` entry, not one
/// per distinct file — a directory repeated in `PATH` (ordinary when a
/// shell profile is sourced more than once), or a symlink alongside its
/// own target, would otherwise be counted as separate binaries. Dedupe by
/// canonical path, keeping the first `PATH` entry that reaches each file so
/// resolution order — and which one is "the one that runs" — is preserved.
/// A candidate whose canonical path can't be determined (broken symlink,
/// permission error) is kept as its own distinct entry rather than dropped:
/// a check that can silently lose a candidate on failure is worse than one
/// that occasionally over-reports.
pub(super) fn find_canopy_copies(path_var: &str, cwd: &Path) -> Vec<PathBuf> {
    let candidates: Vec<PathBuf> = which::which_in_all("canopy", Some(path_var), cwd)
        .map(Iterator::collect)
        .unwrap_or_default();

    let mut seen = HashSet::new();
    let mut copies = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let key = std::fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
        if seen.insert(key) {
            copies.push(candidate);
        }
    }
    copies
}

/// How long to wait on one copy's `--version` before giving up on it. This
/// check walks every `canopy` found on PATH, so a hung or wrapper binary
/// must not stall doctor for the rest of them.
pub(super) const PATH_COPY_VERSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Best-effort `<path> --version`, trimmed, bounded by
/// `PATH_COPY_VERSION_TIMEOUT`. `None` on any failure, including a timeout —
/// `kill_on_drop` ensures a timed-out child is reaped rather than left
/// running, since dropping the in-flight `wait_with_output` future drops the
/// `Child` that owns it.
pub(super) async fn probe_path_copy_version(path: &Path) -> Option<String> {
    let mut command = tokio::process::Command::new(path);
    command
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let child = command.spawn().ok()?;
    let output = tokio::time::timeout(PATH_COPY_VERSION_TIMEOUT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// What doctor should report about the `canopy` copies found on `$PATH`,
/// given each copy's path (in PATH resolution order — index 0 is the one
/// that runs) and its version if one could be determined. Pure — no I/O —
/// so every branch is reachable from synthetic inputs without spawning real
/// processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PathCopiesReport {
    /// No `canopy` found on PATH at all (e.g. only ever run by absolute path).
    None,
    /// Exactly one copy — the normal case, nothing to warn about.
    Single {
        path: PathBuf,
        version: Option<String>,
    },
    /// Two or more copies whose known versions all agree (or too few
    /// versions could be determined to tell them apart) — harmless, and
    /// reported plainly rather than as a warning.
    Consistent {
        copies: Vec<(PathBuf, Option<String>)>,
    },
    /// Two or more copies with at least two differing known versions — the
    /// hazard this check exists to catch: the copy that runs may not be the
    /// one that was last updated.
    Diverging {
        copies: Vec<(PathBuf, Option<String>)>,
    },
}

/// Decide which [`PathCopiesReport`] variant `copies` (in PATH order)
/// describes. A version that couldn't be determined is excluded from the
/// agreement check rather than treated as "different" — an unknown version
/// is not evidence of a mismatch, and a false warning here is exactly the
/// noise the "silent when there's nothing to say" rule exists to prevent.
pub(super) fn diagnose_path_copies(copies: Vec<(PathBuf, Option<String>)>) -> PathCopiesReport {
    match copies.len() {
        0 => PathCopiesReport::None,
        1 => {
            let (path, version) = copies.into_iter().next().expect("len == 1");
            PathCopiesReport::Single { path, version }
        }
        _ => {
            let distinct_versions: HashSet<&str> =
                copies.iter().filter_map(|(_, v)| v.as_deref()).collect();
            if distinct_versions.len() > 1 {
                PathCopiesReport::Diverging { copies }
            } else {
                PathCopiesReport::Consistent { copies }
            }
        }
    }
}

/// One line describing a single copy: its path, version (or "version
/// unknown" if the probe failed/timed out), and — for the copy at index 0,
/// the one PATH resolution would actually run — a marker saying so.
pub(super) fn describe_path_copy(path: &Path, version: &Option<String>, is_winner: bool) -> String {
    let label = version.as_deref().unwrap_or("version unknown");
    let winner = if is_winner { " — this one runs" } else { "" };
    format!("{} ({label}){winner}", path.display())
}

/// Render a [`PathCopiesReport`], pushing an issue only for the `Diverging`
/// case — a single copy, or several at the same version, must produce no
/// warning at all (the normal case must not read as something to worry
/// about).
pub(super) fn print_path_copies_report(report: PathCopiesReport, issues: &mut Vec<String>) {
    match report {
        PathCopiesReport::None => {}
        PathCopiesReport::Single { path, version } => {
            success(format!(
                "canopy on PATH: {}",
                describe_path_copy(&path, &version, false)
            ));
        }
        PathCopiesReport::Consistent { copies } => {
            success(format!(
                "{} copies of canopy on PATH, all at the same version:",
                copies.len()
            ));
            for (i, (path, version)) in copies.iter().enumerate() {
                println!("     {}", describe_path_copy(path, version, i == 0));
            }
        }
        PathCopiesReport::Diverging { copies } => {
            println!(
                " \x1b[33m⚠\x1b[0m {} copies of canopy on PATH report different versions:",
                copies.len()
            );
            for (i, (path, version)) in copies.iter().enumerate() {
                println!("     {}", describe_path_copy(path, version, i == 0));
            }
            issues.push(
                "Multiple `canopy` binaries are on your PATH at different versions — updating \
                 one (self-update, cargo install, or the install script) doesn't update the \
                 others. The copy marked \"this one runs\" above is the one actually in effect."
                    .to_string(),
            );
        }
    }
}

/// Resolve every `canopy` copy on `path_var` and probe each one's version,
/// in PATH order. Split out from [`report_canopy_path_copies`] so tests can
/// drive it over a synthetic PATH instead of the real one.
pub(super) async fn gather_path_copies_report(path_var: &str, cwd: &Path) -> PathCopiesReport {
    let paths = find_canopy_copies(path_var, cwd);
    let mut copies = Vec::with_capacity(paths.len());
    for path in paths {
        let version = probe_path_copy_version(&path).await;
        copies.push((path, version));
    }
    diagnose_path_copies(copies)
}

/// Real-`$PATH` entry point for the duplicate-binary check (C17).
pub(super) async fn report_canopy_path_copies(issues: &mut Vec<String>) {
    let path_var = std::env::var("PATH").unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    print_path_copies_report(gather_path_copies_report(&path_var, &cwd).await, issues);
}
