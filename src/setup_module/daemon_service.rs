use crate::setup_module::models::{is_platform_available, Platform};
use crate::setup_module::platform_adapter::run_install_our_servers;
use crate::setup_module::registry_fetch::fetch_registry;
use crate::setup_module::PlatformWithCli;
use anyhow::{Context, Result};

/// Stop the daemon if it is running.
pub(crate) fn stop_daemon() -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;

    // FR3: if a unit is installed and it owns a live process, stop through
    // the manager first — the wizard's stop→start sequence would otherwise
    // SIGTERM the managed PID, which reads as an unclean exit under
    // `Restart=on-failure` and lets systemd resurrect the daemon mid-install
    // (the CB72 orphan race, from inside setup).
    if crate::daemon::process::service_unit_installed()
        && crate::daemon::process::service_manager_facts().is_some_and(|m| m.pid.is_some())
        && crate::daemon::process::service_manager_stop()
    {
        let _ = std::fs::remove_file(data_dir.join("daemon.pid"));
        return Ok(());
    }

    let pid_path = data_dir.join("daemon.pid");
    if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            #[cfg(unix)]
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
            // Wait for process to stop (up to 3s)
            for _ in 0..12 {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if !is_process_running(pid as u32) {
                    break;
                }
            }
            let _ = std::fs::remove_file(&pid_path);
        }
    }
    Ok(())
}

pub(crate) fn start_daemon_if_needed() -> Result<bool> {
    let data_dir = crate::ensure_data_dir()?;

    // CB72: same shape as the TUI auto-start — if anything (managed daemon
    // or orphan) already holds the port, start nothing and never kill:
    // setup runs non-interactively and must not SIGTERM a user's process
    // mid-wizard.
    let port = crate::resolve_port(None);
    if crate::daemon::process::resolve_port_pid(port).is_some() {
        return Ok(false);
    }

    let exe = std::env::current_exe()?;
    crate::daemon::daemon_start::start_daemon_live(
        port,
        crate::daemon::daemon_start::StartIntent::Auto,
        &exe,
        None,
        &data_dir,
    )?;
    Ok(true)
}

pub(crate) fn install_service_if_needed() -> Result<bool> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let home = dirs::home_dir().context("No home directory")?;

    #[cfg(target_os = "macos")]
    {
        if home.join("Library/LaunchAgents/com.canopy.plist").exists() {
            return Ok(false);
        }
    }

    #[cfg(target_os = "linux")]
    {
        if home.join(".config/systemd/user/canopy.service").exists() {
            return Ok(false);
        }
    }

    let exe = std::env::current_exe()?;
    crate::daemon::service_install::install_service(&exe, 7755)?;
    Ok(true)
}

fn is_process_running(pid: u32) -> bool {
    crate::daemon::process::is_process_running(pid)
}

/// Check if auto-setup should run (no CLI config found).
pub fn needs_setup() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
    !config.is_configured()
}

/// Run setup silently (no prompts, auto-detect all platforms).
#[allow(dead_code)]
pub fn run_setup_silent() -> Result<()> {
    let home = dirs::home_dir().context("No home directory")?;

    let registry = fetch_registry()?;

    let detected: Vec<&Platform> = registry
        .platforms
        .iter()
        .filter(|p| is_platform_available(p))
        .collect();

    run_install_our_servers(&home, &detected, &registry.canonical_servers)?;

    // Save CLI config
    let platforms_with_cli: Vec<PlatformWithCli> = detected
        .iter()
        .map(|p| p.to_platform_with_cli())
        .filter(|p| p.cli.is_some())
        .collect();

    let cli_registry =
        crate::domain::cli_config::CliRegistry::detect_available(&platforms_with_cli);
    let canopy_dir = home.join(".canopy");
    std::fs::create_dir_all(&canopy_dir)?;

    // Save unified config
    let mut config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
    config.mark_configured();
    config.clis = cli_registry.available_clis;
    config.save(&canopy_dir)?;

    Ok(())
}
