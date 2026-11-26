//! Non-interactive harness inventory: every registry harness with its
//! install status and a dated free-tier fact.
//!
//! `canopy setup --list-harnesses` prints the same rows the setup wizard
//! shows, and `--json` renders them for scripts. Detection shares
//! [`resolve_harness_binary`] with `is_platform_available`, so the list,
//! the wizard and the spawner can never disagree about what is installed.

use crate::domain::cli_strategy::{ensure_binary_identity_blocking, BinaryIdentity};
use crate::setup_module::models::Platform;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Install status of one registry harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessInstallStatus {
    /// Binary resolves and answers its identity probe.
    Installed,
    /// Binary resolves but fails its identity probe.
    InstalledNotResponding,
    /// No binary on `PATH` or under `install_paths`.
    NotInstalled,
}

/// One registry harness with its resolved install status.
#[derive(Debug, Clone)]
pub struct HarnessEntry {
    pub platform: Platform,
    pub status: HarnessInstallStatus,
    pub binary_path: Option<PathBuf>,
}

/// Resolve a harness binary: the registry `cli.binary` on `PATH` first,
/// then each registry `install_paths` location (`~/` expands against
/// `home`). Only existing files count, so a stale `PATH` entry pointing
/// at a deleted file does not read as installed.
pub fn resolve_harness_binary(
    p: &Platform,
    home: Option<&Path>,
    path_env: &str,
) -> Option<PathBuf> {
    if let Some(resolved) = resolve_on_path(p, path_env) {
        return Some(resolved);
    }
    resolve_in_install_paths(p, home)
}

/// Resolve the registry `cli.binary` against an explicit `PATH` string.
pub(crate) fn resolve_on_path(p: &Platform, path_env: &str) -> Option<PathBuf> {
    let binary = p.cli.as_ref()?.get("binary")?.as_str()?;
    let (resolved, _) = crate::domain::cli_strategy::resolve_binary_in(binary, path_env).ok()?;
    resolved.is_file().then_some(resolved)
}

/// Resolve the first registry `install_paths` entry that exists.
/// `~/...` expands against `home`; other values must be absolute.
fn resolve_in_install_paths(p: &Platform, home: Option<&Path>) -> Option<PathBuf> {
    for location in &p.install_paths {
        if let Some(candidate) = expand_install_path(location, home) {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Expand one `install_paths` entry against `home`.
fn expand_install_path(location: &str, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(rest) = location.strip_prefix("~/") {
        Some(home?.join(rest))
    } else {
        Some(PathBuf::from(location))
    }
}

/// Whether the resolved binary answers as the expected program: the
/// registry identity check when the platform declares one, else a plain
/// `--version` run.
pub(crate) fn check_responding(p: &Platform, bin: &Path) -> bool {
    if let Some(identity) = cli_identity(p) {
        return ensure_binary_identity_blocking(bin, &identity).is_ok();
    }
    fallback_version_check(bin)
}

/// The registry identity declaration for a platform, if it parses and
/// declares both `identity_args` and `identity_pattern`.
fn cli_identity(p: &Platform) -> Option<BinaryIdentity> {
    let cli = p.cli.as_ref()?;
    let config: crate::domain::cli_config::CliConfig = serde_json::from_value(cli.clone()).ok()?;
    BinaryIdentity::from_cli_config(&config)
}

/// Plain `--version` probe for platforms without a declared identity.
/// Output is swallowed; only the exit status matters.
fn fallback_version_check(bin: &Path) -> bool {
    std::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Status and resolved path for one platform under an explicit
/// environment (injectable for tests; production passes the real one).
fn status_of(
    p: &Platform,
    home: Option<&Path>,
    path_env: &str,
) -> (HarnessInstallStatus, Option<PathBuf>) {
    match resolve_harness_binary(p, home, path_env) {
        None => (HarnessInstallStatus::NotInstalled, None),
        Some(bin) if check_responding(p, &bin) => (HarnessInstallStatus::Installed, Some(bin)),
        Some(bin) => (HarnessInstallStatus::InstalledNotResponding, Some(bin)),
    }
}

/// Collect one entry per registry platform under an explicit environment.
fn collect_harnesses_with(
    platforms: &[Platform],
    home: Option<&Path>,
    path_env: &str,
) -> Vec<HarnessEntry> {
    platforms
        .iter()
        .map(|p| {
            let (status, binary_path) = status_of(p, home, path_env);
            HarnessEntry {
                platform: p.clone(),
                status,
                binary_path,
            }
        })
        .collect()
}

/// Collect one entry per registry platform using the real home and `PATH`.
pub fn collect_harnesses(platforms: &[Platform]) -> Vec<HarnessEntry> {
    let path_env = std::env::var("PATH").unwrap_or_default();
    collect_harnesses_with(platforms, dirs::home_dir().as_deref(), &path_env)
}

/// Normalized free-tier value: `"yes"`, `"no"`, or `"unknown"` for
/// anything else (absent or unrecognized is never a claim).
pub fn free_tier_value(p: &Platform) -> &'static str {
    match p.free_tier.as_deref() {
        Some("yes") => "yes",
        Some("no") => "no",
        _ => "unknown",
    }
}

/// Rank for free-tier-first sorting: `yes` before `no` before `unknown`.
pub(crate) fn free_tier_rank(p: &Platform) -> u8 {
    match free_tier_value(p) {
        "yes" => 0,
        "no" => 1,
        _ => 2,
    }
}

/// Sort in place: free tier first (`yes`, then `no`, then `unknown`),
/// alphabetical within a tier. Stable, so registry order survives ties.
pub fn sort_free_first(entries: &mut [HarnessEntry]) {
    entries.sort_by(|a, b| {
        free_tier_rank(&a.platform)
            .cmp(&free_tier_rank(&b.platform))
            .then_with(|| a.platform.name.cmp(&b.platform.name))
    });
}

/// Keep entries whose harness name contains `query` (case-insensitive).
/// An empty query keeps everything.
pub fn filter_harnesses<'a>(entries: &'a [HarnessEntry], query: &str) -> Vec<&'a HarnessEntry> {
    if query.is_empty() {
        return entries.iter().collect();
    }
    let needle = query.to_lowercase();
    entries
        .iter()
        .filter(|e| e.platform.name.to_lowercase().contains(&needle))
        .collect()
}

/// Dated free-tier label: `free tier: yes (checked 2026-11-10)`,
/// `free tier: no (...)`, or `free tier: unknown`. Never an amount.
pub fn format_free_tier_label(p: &Platform) -> String {
    match free_tier_value(p) {
        "unknown" => "free tier: unknown".to_string(),
        value => {
            let checked = p.free_tier_checked_at.as_deref().unwrap_or("unknown");
            format!("free tier: {value} (checked {checked})")
        }
    }
}

/// One-line status word for text rows.
fn status_label(status: HarnessInstallStatus) -> &'static str {
    match status {
        HarnessInstallStatus::Installed => "installed",
        HarnessInstallStatus::InstalledNotResponding => "installed (not responding)",
        HarnessInstallStatus::NotInstalled => "not installed",
    }
}

/// One text row per harness: name, status, free-tier label, and
/// `docs_url` when the harness is not installed.
pub fn format_harnesses_text(entries: &[HarnessEntry]) -> String {
    let mut out = String::new();
    for entry in entries {
        out.push_str(&format_harness_row(entry));
        out.push('\n');
    }
    out
}

/// Format one text row (split for the line-count lint).
fn format_harness_row(entry: &HarnessEntry) -> String {
    let mut row = format!(
        "{}: {}, {}",
        entry.platform.name,
        status_label(entry.status),
        format_free_tier_label(&entry.platform)
    );
    if entry.status == HarnessInstallStatus::NotInstalled {
        if let Some(url) = entry.platform.docs_url.as_deref() {
            row.push_str(&format!(", docs: {url}"));
        }
    }
    row
}

/// JSON array with the script-facing keys: `name`, `installed`,
/// `binary_path`, `free_tier`, `free_tier_checked_at`,
/// `free_tier_source`, `docs_url`.
pub fn format_harnesses_json(entries: &[HarnessEntry]) -> String {
    let items: Vec<serde_json::Value> = entries.iter().map(harness_json).collect();
    serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string())
}

/// One JSON object for one harness entry.
fn harness_json(entry: &HarnessEntry) -> serde_json::Value {
    serde_json::json!({
        "name": entry.platform.name,
        "installed": entry.status != HarnessInstallStatus::NotInstalled,
        "binary_path": entry.binary_path.as_ref()
            .map(|p| p.display().to_string()),
        "free_tier": free_tier_value(&entry.platform),
        "free_tier_checked_at": entry.platform.free_tier_checked_at,
        "free_tier_source": entry.platform.free_tier_source,
        "docs_url": entry.platform.docs_url,
    })
}

/// Print every registry harness non-interactively: plain text rows, or
/// JSON with `--json`, for scripts and the install/login specs.
pub fn run_list_harnesses(json: bool) -> Result<()> {
    let registry = super::registry_fetch::fetch_registry()?;
    let entries = collect_harnesses(&registry.platforms);
    if json {
        println!("{}", format_harnesses_json(&entries));
    } else {
        print!("{}", format_harnesses_text(&entries));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal fixture platform: registry `cli.binary` plus optional
    /// free-tier/docs metadata.
    fn fixture_platform(name: &str, binary: &str) -> Platform {
        Platform {
            name: name.to_string(),
            config_path: format!(".config/{name}"),
            config_format: None,
            toml_array_format: false,
            command_format: "separate".to_string(),
            mcp_servers_key: vec![],
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: std::collections::HashMap::new(),
            required_fields: std::collections::HashMap::new(),
            server_extras: std::collections::HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            instruction_precedence: vec![],
            user_instruction_file: None,
            provider: None,
            tool_name: None,
            project_paths: Vec::new(),
            docs_url: None,
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: Vec::new(),
            install_commands: Vec::new(),
            login_command: None,
            login_check_command: None,
            login_checked_at: None,
            cli: Some(serde_json::json!({"binary": binary})),
        }
    }

    /// Write an executable stub script answering `--version` with `body`.
    fn stub_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    /// Temp bin dir on an isolated `PATH`, plus a home dir.
    fn fixture_env() -> (tempfile::TempDir, tempfile::TempDir, String) {
        let bin = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let path_env = bin.path().display().to_string();
        (bin, home, path_env)
    }

    #[test]
    fn detects_installed_missing_and_not_responding() {
        let (bin, home, path_env) = fixture_env();
        stub_script(bin.path(), "good-cli", "exit 0");
        stub_script(bin.path(), "bad-cli", "exit 1");
        let platforms = vec![
            fixture_platform("good", "good-cli"),
            fixture_platform("missing", "no-such-cli-xyz"),
            fixture_platform("bad", "bad-cli"),
        ];
        let entries = collect_harnesses_with(&platforms, Some(home.path()), &path_env);
        assert_eq!(entries[0].status, HarnessInstallStatus::Installed);
        assert!(entries[0].binary_path.as_ref().unwrap().is_file());
        assert_eq!(entries[1].status, HarnessInstallStatus::NotInstalled);
        assert_eq!(entries[1].binary_path, None);
        assert_eq!(
            entries[2].status,
            HarnessInstallStatus::InstalledNotResponding
        );
        assert!(entries[2].binary_path.as_ref().unwrap().is_file());
    }

    #[test]
    fn detects_binary_via_install_paths() {
        let (bin, home, _) = fixture_env();
        let _ = bin;
        let local_bin = home.path().join(".local").join("bin");
        std::fs::create_dir_all(&local_bin).unwrap();
        stub_script(&local_bin, "hid-cli", "exit 0");
        let mut platform = fixture_platform("hid", "hid-cli");
        platform.install_paths = vec!["~/.local/bin/hid-cli".to_string()];
        let entries = collect_harnesses_with(
            std::slice::from_ref(&platform),
            Some(home.path()),
            "/nonexistent-path-dir",
        );
        assert_eq!(entries[0].status, HarnessInstallStatus::Installed);
        assert_eq!(
            entries[0].binary_path.as_ref().unwrap(),
            &local_bin.join("hid-cli")
        );
    }

    #[test]
    fn formats_three_free_tier_states_and_date() {
        let mut yes = fixture_platform("a", "a-cli");
        yes.free_tier = Some("yes".to_string());
        yes.free_tier_checked_at = Some("2026-11-10".to_string());
        let mut no = fixture_platform("b", "b-cli");
        no.free_tier = Some("no".to_string());
        no.free_tier_checked_at = Some("2026-11-10".to_string());
        let unknown = fixture_platform("c", "c-cli");
        assert_eq!(
            format_free_tier_label(&yes),
            "free tier: yes (checked 2026-11-10)"
        );
        assert_eq!(
            format_free_tier_label(&no),
            "free tier: no (checked 2026-11-10)"
        );
        assert_eq!(format_free_tier_label(&unknown), "free tier: unknown");
    }

    #[test]
    fn sorts_free_tier_first_and_filters() {
        let mut entries: Vec<HarnessEntry> = ["z-no", "a-unknown", "m-yes"]
            .iter()
            .map(|name| {
                let mut p = fixture_platform(name, "cli");
                if name.ends_with("yes") {
                    p.free_tier = Some("yes".to_string());
                } else if name.ends_with("no") {
                    p.free_tier = Some("no".to_string());
                }
                HarnessEntry {
                    platform: p,
                    status: HarnessInstallStatus::NotInstalled,
                    binary_path: None,
                }
            })
            .collect();
        sort_free_first(&mut entries);
        let names: Vec<&str> = entries.iter().map(|e| e.platform.name.as_str()).collect();
        assert_eq!(names, vec!["m-yes", "z-no", "a-unknown"]);
        let filtered = filter_harnesses(&entries, "Z-");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].platform.name, "z-no");
        assert_eq!(filter_harnesses(&entries, "").len(), 3);
    }

    #[test]
    fn formats_text_and_json_shape() {
        let (bin, home, path_env) = fixture_env();
        stub_script(bin.path(), "ok-cli", "exit 0");
        let mut present = fixture_platform("present", "ok-cli");
        present.free_tier = Some("yes".to_string());
        present.free_tier_checked_at = Some("2026-11-10".to_string());
        let mut absent = fixture_platform("absent", "absent-cli");
        absent.docs_url = Some("https://example.com/install".to_string());
        let entries = collect_harnesses_with(&[present, absent], Some(home.path()), &path_env);
        let text = format_harnesses_text(&entries);
        assert!(text.contains("present: installed, free tier: yes (checked 2026-11-10)"));
        assert!(text.contains(
            "absent: not installed, free tier: unknown, docs: https://example.com/install"
        ));
        let parsed: serde_json::Value =
            serde_json::from_str(&format_harnesses_json(&entries)).unwrap();
        let items = parsed.as_array().unwrap();
        assert_eq!(items.len(), 2);
        for item in items {
            let obj = item.as_object().unwrap();
            let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
            for key in [
                "name",
                "installed",
                "binary_path",
                "free_tier",
                "free_tier_checked_at",
                "free_tier_source",
                "docs_url",
            ] {
                assert!(keys.contains(&key), "missing key {key}");
            }
        }
        assert_eq!(items[0]["installed"], true);
        assert!(items[0]["binary_path"]
            .as_str()
            .unwrap()
            .ends_with("ok-cli"));
        assert_eq!(items[0]["free_tier"], "yes");
        assert_eq!(items[0]["free_tier_checked_at"], "2026-11-10");
        assert_eq!(items[1]["installed"], false);
        assert!(items[1]["binary_path"].is_null());
        assert_eq!(items[1]["free_tier"], "unknown");
        assert!(items[1]["free_tier_checked_at"].is_null());
    }

    #[test]
    fn parses_platform_toml_with_and_without_new_keys_and_absolute_install_path() {
        let dir = tempfile::tempdir().unwrap();
        let abs_bin = stub_script(dir.path(), "abs-cli", "exit 0");
        let full_toml = format!(
            r#"
name = "full"
config_path = ".config/full"
mcp_servers_key = ["mcpServers"]
docs_url = "https://example.com/docs"
free_tier = "yes"
free_tier_checked_at = "2026-11-10"
free_tier_source = "https://example.com/pricing"
install_paths = ["{}"]

[cli]
binary = "abs-cli"
"#,
            abs_bin.display()
        );
        let full: Platform = toml::from_str(&full_toml).unwrap();
        assert_eq!(full.docs_url.as_deref(), Some("https://example.com/docs"));
        assert_eq!(full.free_tier.as_deref(), Some("yes"));
        assert_eq!(full.free_tier_checked_at.as_deref(), Some("2026-11-10"));
        assert_eq!(
            full.free_tier_source.as_deref(),
            Some("https://example.com/pricing")
        );
        assert_eq!(full.install_paths, vec![abs_bin.display().to_string()]);
        // Absolute install_paths resolves even when home is None and PATH is empty.
        assert_eq!(
            resolve_harness_binary(&full, None, "/nonexistent-path-dir").as_deref(),
            Some(abs_bin.as_path())
        );

        let minimal_toml = r#"
name = "minimal"
config_path = ".config/minimal"
mcp_servers_key = ["mcpServers"]
"#;
        let minimal: Platform = toml::from_str(minimal_toml).unwrap();
        assert_eq!(minimal.docs_url, None);
        assert_eq!(minimal.free_tier, None);
        assert!(minimal.install_paths.is_empty());
        assert_eq!(format_free_tier_label(&minimal), "free tier: unknown");
    }
}
