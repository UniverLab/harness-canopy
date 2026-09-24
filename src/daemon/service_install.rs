//! System service installation and uninstallation.
//!
//! Supports:
//! - **Linux/WSL**: systemd user unit at `~/.config/systemd/user/canopy.service`
//! - **macOS**: launchd agent at `~/Library/LaunchAgents/com.canopy.plist`

use anyhow::Result;

/// Install the daemon as a system service that starts on boot.
pub fn install_service(exe_path: &std::path::Path, port: u16) -> Result<()> {
    let exe = exe_path
        .canonicalize()
        .unwrap_or_else(|_| exe_path.to_path_buf());

    if cfg!(target_os = "macos") {
        install_launchd_service(&exe, port)
    } else if cfg!(target_os = "linux") {
        install_systemd_service(&exe, port)
    } else {
        anyhow::bail!(
            "Service installation is not supported on this platform (only Linux and macOS)"
        )
    }
}

/// Uninstall the system service.
pub fn uninstall_service() -> Result<()> {
    if cfg!(target_os = "macos") {
        uninstall_launchd_service()
    } else if cfg!(target_os = "linux") {
        uninstall_systemd_service()
    } else {
        anyhow::bail!("Service uninstallation is not supported on this platform")
    }
}

// -- systemd (Linux/WSL) ------------------------------------------------------

const SYSTEMD_SERVICE_NAME: &str = "canopy.service";

/// CB70: the only extra variables ever copied into the systemd unit.
/// Nothing else from the process environment may be added here (constraint).
const BROWSER_ENV_KEYS: [&str; 3] = ["BROWSER", "DISPLAY", "WAYLAND_DISPLAY"];

fn systemd_unit_dir() -> Result<std::path::PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(home.join(".config/systemd/user"))
}

fn install_systemd_service(exe: &std::path::Path, port: u16) -> Result<()> {
    let unit_dir = systemd_unit_dir()?;
    std::fs::create_dir_all(&unit_dir)?;

    let unit_path = unit_dir.join(SYSTEMD_SERVICE_NAME);
    let exe_str = exe.display().to_string();

    // Capture PATH from the process environment rather than shelling out to a
    // login shell. This is the correct choice because `canopy daemon install` is
    // run by the same user who installed their CLIs, and the process already
    // inherits that user's PATH. Shelling out to `bash -lc 'echo $PATH'` would
    // risk picking up a different shell profile (or failing in headless/SSH
    // environments). The daemon's PATH is written once at install time and
    // kept up to date by re-running `canopy daemon install`.
    let current_path = std::env::var("PATH").unwrap_or_default();
    let path_value = reconcile_path(unit_path.as_path(), &current_path);

    let current_browser_env = collect_browser_env_from_process();
    let extra_env = reconcile_browser_env(unit_path.as_path(), &current_browser_env);

    let unit_content = render_unit_content(&exe_str, port, &path_value, &extra_env);

    std::fs::write(&unit_path, unit_content)?;
    println!("Created {}", unit_path.display());

    ensure_linger_enabled();
    reload_and_enable_service()?;

    Ok(())
}

/// Render the systemd unit file contents.
///
/// Includes `Environment=PATH=` so CLI binaries installed under a user's
/// home directory (e.g. `~/.opencode/bin`) resolve under the daemon's
/// minimal systemd PATH, not just when the daemon inherits a shell's PATH.
fn render_unit_content(
    exe_str: &str,
    port: u16,
    path_value: &str,
    extra_env: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut extra_lines = String::new();
    for key in BROWSER_ENV_KEYS {
        if let Some(val) = extra_env.get(key) {
            if val.is_empty() {
                continue;
            }
            extra_lines.push_str(&format!("Environment={key}={}\n", systemd_quote_value(val)));
        }
    }
    format!(
        r#"[Unit]
Description=canopy daemon
After=network.target

[Service]
Type=simple
ExecStart={exe_str} serve --port {port}
Restart=on-failure
RestartSec=5
StartLimitIntervalSec=60
StartLimitBurst=5
Environment=RUST_LOG=info
Environment=PATH={path_value}
{extra_lines}
[Install]
WantedBy=default.target
"#
    )
}

/// Compute the `PATH` to write into the unit, preserving any custom entries
/// from an existing unit's `Environment=PATH=` line that aren't present in
/// `new_path`.
///
/// Reinstalling used to silently overwrite the whole unit file, wiping out
/// any hand-added `Environment=PATH=` (e.g. one including `~/.opencode/bin`
/// or `~/.grok/bin`). Rather than clobber it again, entries that would be
/// lost are appended to the new PATH and reported on stdout.
fn reconcile_path(unit_path: &std::path::Path, new_path: &str) -> String {
    let Ok(existing) = std::fs::read_to_string(unit_path) else {
        return new_path.to_string();
    };

    let Some(old_path) = existing
        .lines()
        .find_map(|line| line.strip_prefix("Environment=PATH="))
    else {
        return new_path.to_string();
    };

    if old_path == new_path {
        return new_path.to_string();
    }

    let new_entries: std::collections::HashSet<&str> = new_path.split(':').collect();
    let preserved: Vec<&str> = old_path
        .split(':')
        .filter(|entry| !entry.is_empty() && !new_entries.contains(entry))
        .collect();

    if preserved.is_empty() {
        return new_path.to_string();
    }

    println!(
        "  Existing {} has PATH entries not in the new PATH: {}",
        SYSTEMD_SERVICE_NAME,
        preserved.join(":")
    );
    println!("  Keeping them appended so previously working CLIs don't break.");

    let mut merged = new_path.to_string();
    for entry in preserved {
        merged.push(':');
        merged.push_str(entry);
    }
    merged
}

/// CB70: quote a value for systemd's `Environment=` directive. Verbatim
/// unless it contains ASCII whitespace or a double-quote, in which case
/// wrap in `"..."` after escaping `\` then `"`.
fn systemd_quote_value(value: &str) -> String {
    if !value.contains(' ') && !value.contains('\t') && !value.contains('"') {
        return value.to_string();
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// CB70: parse previously stored BROWSER/DISPLAY/WAYLAND_DISPLAY values
/// from an existing unit's contents. Pure over `&str` so tests drive it
/// without real unit files. Strips one layer of surrounding double quotes
/// so a quoted old value round-trips to exactly one layer on re-render.
fn parse_existing_browser_env(unit_content: &str) -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    for line in unit_content.lines() {
        let Some(rest) = line.strip_prefix("Environment=") else {
            continue;
        };
        let Some((key, val)) = rest.split_once('=') else {
            continue;
        };
        if !BROWSER_ENV_KEYS.contains(&key) || val.is_empty() {
            continue;
        }
        let stored = if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
            val[1..val.len() - 1].to_string()
        } else {
            val.to_string()
        };
        if stored.is_empty() {
            continue;
        }
        map.insert(key.to_string(), stored);
    }
    map
}

/// CB70: reconcile the current environment's browser/display values with
/// any previously stored in the existing unit file. For each key, the
/// current env wins when non-empty; otherwise a non-empty old value is
/// kept and reported. Never touches `canopy.service.d/` (FR3).
fn reconcile_browser_env(
    unit_path: &std::path::Path,
    current: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    let mut result = std::collections::BTreeMap::new();
    for key in BROWSER_ENV_KEYS {
        if let Some(val) = current.get(key) {
            if !val.is_empty() {
                result.insert(key.to_string(), val.clone());
            }
        }
    }
    let Ok(existing) = std::fs::read_to_string(unit_path) else {
        return result;
    };
    let old = parse_existing_browser_env(&existing);
    for key in BROWSER_ENV_KEYS {
        let missing = result.get(key).is_none_or(|v| v.is_empty());
        if missing {
            if let Some(old_val) = old.get(key) {
                if !old_val.is_empty() {
                    println!(
                        "  Keeping existing Environment={key}={old_val} (not set in current environment)"
                    );
                    result.insert(key.to_string(), old_val.clone());
                }
            }
        }
    }
    result
}

/// CB70: read only the allow-listed keys from the process environment.
/// Only caller is `install_systemd_service`. No other variable may be
/// read here (constraint).
fn collect_browser_env_from_process() -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    for key in BROWSER_ENV_KEYS {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                map.insert(key.to_string(), v);
            }
        }
    }
    map
}

fn ensure_linger_enabled() {
    let Some(user) = std::env::var("USER").ok() else {
        return;
    };

    let linger_enabled = std::process::Command::new("loginctl")
        .args(["show-user", &user, "-p", "Linger"])
        .output()
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "Linger=yes")
        .unwrap_or(false);

    if linger_enabled {
        return;
    }

    match std::process::Command::new("loginctl")
        .args(["enable-linger", &user])
        .status()
    {
        Ok(s) if s.success() => {
            println!("  Lingering enabled (service survives logout/reboot)");
        }
        _ => {
            println!("  ⚠ Could not enable lingering — service may stop on logout/reboot.");
            println!("    Run manually: sudo loginctl enable-linger {user}");
        }
    }
}

fn reload_and_enable_service() -> Result<()> {
    match std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
    {
        Ok(s) if s.success() => {}
        _ => {
            println!("  ⚠ systemctl daemon-reload failed (systemd may not be fully available)");
            println!("    The unit file has been written — you can enable it manually:");
            println!("    systemctl --user enable --now {SYSTEMD_SERVICE_NAME}");
            return Ok(());
        }
    }

    match std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", SYSTEMD_SERVICE_NAME])
        .status()
    {
        Ok(s) if s.success() => {
            println!("  Service enabled and started");
            println!("    Check status: systemctl --user status {SYSTEMD_SERVICE_NAME}");
            println!("    View logs:    journalctl --user -u {SYSTEMD_SERVICE_NAME} -f");
        }
        _ => {
            println!("  ⚠ Failed to enable service automatically");
            println!("    Enable manually: systemctl --user enable --now {SYSTEMD_SERVICE_NAME}");
        }
    }

    Ok(())
}

#[allow(dead_code)]
fn uninstall_systemd_service() -> Result<()> {
    let unit_dir = systemd_unit_dir()?;
    let unit_path = unit_dir.join(SYSTEMD_SERVICE_NAME);

    if !unit_path.exists() {
        println!("Service is not installed (no unit file found)");
        return Ok(());
    }

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", SYSTEMD_SERVICE_NAME])
        .status();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", SYSTEMD_SERVICE_NAME])
        .status();

    std::fs::remove_file(&unit_path)?;
    println!("Removed {}", unit_path.display());

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    println!("Service stopped and uninstalled");
    Ok(())
}

// -- launchd (macOS) ----------------------------------------------------------

const LAUNCHD_LABEL: &str = "com.canopy";

fn launchd_plist_path() -> Result<std::path::PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(home.join(format!("Library/LaunchAgents/{LAUNCHD_LABEL}.plist")))
}

fn install_launchd_service(exe: &std::path::Path, port: u16) -> Result<()> {
    let plist_path = launchd_plist_path()?;
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let log_dir = home.join(".canopy");
    std::fs::create_dir_all(&log_dir)?;

    let exe_str = exe.display();
    let stdout_log = log_dir.join("daemon.log").display().to_string();
    let stderr_log = log_dir.join("daemon.err.log").display().to_string();

    let plist_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe_str}</string>
        <string>serve</string>
        <string>--port</string>
        <string>{port}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>{stdout_log}</string>
    <key>StandardErrorPath</key>
    <string>{stderr_log}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>RUST_LOG</key>
        <string>info</string>
    </dict>
</dict>
</plist>
"#
    );

    if plist_path.exists() {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &plist_path.display().to_string()])
            .status();
    }

    std::fs::write(&plist_path, plist_content)?;
    println!("Created {}", plist_path.display());

    let load = std::process::Command::new("launchctl")
        .args(["load", &plist_path.display().to_string()])
        .status()?;

    if load.success() {
        println!("Service loaded and started");
        println!("  Check status: launchctl list | grep {LAUNCHD_LABEL}");
        println!("  View logs:    tail -f {stdout_log}");
    } else {
        println!("Warning: launchctl load failed");
        println!("  Try manually: launchctl load {}", plist_path.display());
    }

    Ok(())
}

#[allow(dead_code)]
fn uninstall_launchd_service() -> Result<()> {
    let plist_path = launchd_plist_path()?;

    if !plist_path.exists() {
        println!("Service is not installed (no plist found)");
        return Ok(());
    }

    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.display().to_string()])
        .status();

    std::fs::remove_file(&plist_path)?;
    println!("Removed {}", plist_path.display());
    println!("Service stopped and uninstalled");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Does not touch the user's real `~/.config/systemd/user/canopy.service`
    /// — exercises the pure content-generation function directly.
    #[test]
    fn unit_template_includes_nonempty_path_environment() {
        let content = render_unit_content(
            "/usr/bin/canopy",
            4177,
            "/usr/bin:/bin",
            &std::collections::BTreeMap::new(),
        );

        let path_line = content
            .lines()
            .find(|line| line.starts_with("Environment=PATH="))
            .expect("unit must declare Environment=PATH=");
        let value = path_line.trim_start_matches("Environment=PATH=");

        assert!(!value.is_empty());
    }

    #[test]
    fn reconcile_path_returns_new_path_when_no_existing_unit() {
        let tmp = tempfile::tempdir().unwrap();
        let unit_path = tmp.path().join("canopy.service");

        let result = reconcile_path(&unit_path, "/usr/bin:/bin");

        assert_eq!(result, "/usr/bin:/bin");
    }

    #[test]
    fn reconcile_path_preserves_custom_entries_missing_from_new_path() {
        let tmp = tempfile::tempdir().unwrap();
        let unit_path = tmp.path().join("canopy.service");
        std::fs::write(
            &unit_path,
            "[Service]\nEnvironment=PATH=/home/u/.opencode/bin:/usr/bin:/bin\n",
        )
        .unwrap();

        let result = reconcile_path(&unit_path, "/usr/bin:/bin");

        assert!(result.contains("/home/u/.opencode/bin"));
        assert!(result.contains("/usr/bin"));
        assert!(result.contains("/bin"));
    }

    #[test]
    fn render_unit_content_includes_exe_and_port() {
        let content = render_unit_content(
            "/usr/bin/canopy",
            7755,
            "/usr/bin:/bin",
            &std::collections::BTreeMap::new(),
        );
        assert!(content.contains("/usr/bin/canopy"));
        assert!(content.contains("7755"));
        assert!(content.contains("[Unit]"));
        assert!(content.contains("[Service]"));
        assert!(content.contains("[Install]"));
        assert!(content.contains("ExecStart="));
        assert!(content.contains("Environment=PATH="));
    }

    #[test]
    fn render_unit_content_uses_custom_port() {
        let content = render_unit_content(
            "/usr/bin/canopy",
            9999,
            "/usr/bin",
            &std::collections::BTreeMap::new(),
        );
        assert!(content.contains("9999"));
        assert!(!content.contains("7755"));
    }

    #[test]
    fn render_unit_content_uses_custom_path() {
        let content = render_unit_content(
            "/usr/bin/canopy",
            7755,
            "/custom/path:/another/path",
            &std::collections::BTreeMap::new(),
        );
        assert!(content.contains("/custom/path:/another/path"));
    }

    #[test]
    fn render_unit_includes_browser_and_display_but_no_wayland() {
        let map = std::collections::BTreeMap::from([
            ("BROWSER".to_string(), "/x/wsl-browser".to_string()),
            ("DISPLAY".to_string(), ":0".to_string()),
        ]);
        let content = render_unit_content("/usr/bin/canopy", 7755, "/usr/bin:/bin", &map);
        assert!(content.contains("Environment=BROWSER=/x/wsl-browser"));
        assert!(content.contains("Environment=DISPLAY=:0"));
        assert!(!content.contains("WAYLAND_DISPLAY"));
    }

    #[test]
    fn reconcile_browser_env_keeps_old_browser_when_current_env_lacks_it() {
        let tmp = tempfile::tempdir().unwrap();
        let unit_path = tmp.path().join("canopy.service");
        std::fs::write(&unit_path, "[Service]\nEnvironment=BROWSER=/old\n").unwrap();

        let result = reconcile_browser_env(&unit_path, &std::collections::BTreeMap::new());

        assert_eq!(result.get("BROWSER"), Some(&"/old".to_string()));
    }

    #[test]
    fn render_unit_quotes_value_with_space() {
        let map = std::collections::BTreeMap::from([(
            "BROWSER".to_string(),
            "/x/my browser".to_string(),
        )]);
        let content = render_unit_content("/usr/bin/canopy", 7755, "/usr/bin:/bin", &map);
        assert!(content.contains("Environment=BROWSER=\"/x/my browser\""));
    }

    #[test]
    fn systemd_quote_value_plain_vs_space_vs_embedded_quote() {
        assert_eq!(systemd_quote_value("/x/wsl-browser"), "/x/wsl-browser");
        assert_eq!(systemd_quote_value("/x/my browser"), "\"/x/my browser\"");
        assert_eq!(
            systemd_quote_value("/x/say\"hi browser"),
            "\"/x/say\\\"hi browser\""
        );
    }

    #[test]
    fn parse_existing_browser_env_ignores_path_and_empty_values() {
        let content = "[Service]\nEnvironment=PATH=/usr/bin:/bin\nEnvironment=BROWSER=\nEnvironment=DISPLAY=:0\nEnvironment=RUST_LOG=info\n";
        let parsed = parse_existing_browser_env(content);
        assert_eq!(parsed.get("DISPLAY"), Some(&":0".to_string()));
        assert!(!parsed.contains_key("PATH"));
        assert!(!parsed.contains_key("BROWSER"));
        assert!(!parsed.contains_key("RUST_LOG"));
    }
}
