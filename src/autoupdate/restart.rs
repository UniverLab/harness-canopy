use anyhow::{bail, Result};
use std::path::PathBuf;

use crate::daemon::daemon_start::{start_daemon_live, StartIntent};
use crate::daemon::process;

/// Operations needed after an explicit binary replacement.  The trait keeps
/// restart decisions testable without touching a real process, systemd, or
/// launchd.
pub trait UpdateDaemonOps {
    /// Whether the service manager owns the live daemon.
    fn is_managed(&self) -> bool;

    /// Restart through the owning service manager. `false` means the manager
    /// command failed and the caller should report the named command.
    fn restart_managed(&self) -> bool;

    /// Stop an unmanaged daemon through the detached signal path.
    fn stop(&self) -> Result<()>;

    /// Start an unmanaged daemon through the shared TUI/daemon start path.
    fn start(&self) -> Result<()>;

    /// Whether a daemon is currently running.  The default keeps small test
    /// fakes source-compatible; production overrides it with port/PID facts.
    fn is_running(&self) -> bool {
        true
    }
}

/// Production process/service-manager implementation.
pub struct RealUpdateDaemonOps {
    data_dir: PathBuf,
    port: u16,
    exe: PathBuf,
    port_arg: Option<u16>,
}

impl RealUpdateDaemonOps {
    pub(crate) fn new(data_dir: PathBuf, port: u16, exe: PathBuf, port_arg: Option<u16>) -> Self {
        Self {
            data_dir,
            port,
            exe,
            port_arg,
        }
    }
}

impl UpdateDaemonOps for RealUpdateDaemonOps {
    fn is_running(&self) -> bool {
        process::resolve_port_pid(self.port).is_some()
            || process::read_pid(&self.data_dir).is_some_and(process::is_process_running)
    }

    fn is_managed(&self) -> bool {
        let Some(occupant) = process::resolve_port_pid(self.port) else {
            return false;
        };
        process::service_unit_installed()
            && process::service_manager_facts().is_some_and(|facts| facts.pid == Some(occupant))
    }

    fn restart_managed(&self) -> bool {
        process::service_manager_restart()
    }

    fn stop(&self) -> Result<()> {
        let mut targets = Vec::new();
        if let Some(pid) =
            process::read_pid(&self.data_dir).filter(|pid| process::is_process_running(*pid))
        {
            targets.push(pid);
        }
        if let Some(pid) =
            process::resolve_port_pid(self.port).filter(|pid| process::is_process_running(*pid))
        {
            if !targets.contains(&pid) {
                targets.push(pid);
            }
        }

        for pid in &targets {
            process::send_signal(*pid);
        }
        for _ in 0..20 {
            if targets.iter().all(|pid| !process::is_process_running(*pid)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        process::remove_pid_file(&self.data_dir);
        Ok(())
    }

    fn start(&self) -> Result<()> {
        start_daemon_live(
            self.port,
            StartIntent::Explicit,
            &self.exe,
            self.port_arg,
            &self.data_dir,
        )
    }
}

/// Decide whether a post-update daemon restart is safe and perform it.
pub fn restart_daemon_after_update(
    ops: &dyn UpdateDaemonOps,
    graph_running: bool,
    confirm_restart: &dyn Fn() -> bool,
) -> Result<()> {
    if !ops.is_running() {
        return Ok(());
    }

    if graph_running {
        let restart = confirm_restart();
        if !restart {
            println!(
                "Daemon left running — run 'systemctl --user restart canopy.service' or 'canopy daemon restart' later to finish the update."
            );
            return Ok(());
        }
    }

    if ops.is_managed() {
        if !ops.restart_managed() {
            bail!("failed to restart the managed daemon via {MANAGED_RESTART_COMMAND}");
        }
        println!("Restarted daemon via {MANAGED_RESTART_COMMAND}");
    } else {
        ops.stop()?;
        ops.start()?;
        println!("Stopped and restarted the daemon (canopy daemon stop/start path)");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
const MANAGED_RESTART_COMMAND: &str = "systemctl --user restart canopy.service";

#[cfg(target_os = "macos")]
const MANAGED_RESTART_COMMAND: &str = "launchctl";

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const MANAGED_RESTART_COMMAND: &str = "the platform service manager";

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct FakeOps {
        running: bool,
        managed: bool,
        restart_ok: bool,
        restarted: Cell<bool>,
        stopped: Cell<bool>,
        started: Cell<bool>,
    }

    impl UpdateDaemonOps for FakeOps {
        fn is_managed(&self) -> bool {
            self.managed
        }

        fn restart_managed(&self) -> bool {
            self.restarted.set(true);
            self.restart_ok
        }

        fn stop(&self) -> Result<()> {
            self.stopped.set(true);
            Ok(())
        }

        fn start(&self) -> Result<()> {
            self.started.set(true);
            Ok(())
        }

        fn is_running(&self) -> bool {
            self.running
        }
    }

    fn fake(running: bool, managed: bool) -> FakeOps {
        FakeOps {
            running,
            managed,
            restart_ok: true,
            restarted: Cell::new(false),
            stopped: Cell::new(false),
            started: Cell::new(false),
        }
    }

    #[test]
    fn stopped_daemon_is_left_alone() {
        let ops = fake(false, false);
        restart_daemon_after_update(&ops, false, &|| true).unwrap();
        assert!(!ops.restarted.get());
        assert!(!ops.stopped.get());
        assert!(!ops.started.get());
    }

    #[test]
    fn managed_daemon_uses_one_restart_operation() {
        let ops = fake(true, true);
        restart_daemon_after_update(&ops, false, &|| true).unwrap();
        assert!(ops.restarted.get());
        assert!(!ops.stopped.get());
        assert!(!ops.started.get());
    }

    #[test]
    fn unmanaged_daemon_stops_then_starts() {
        let ops = fake(true, false);
        restart_daemon_after_update(&ops, false, &|| true).unwrap();
        assert!(!ops.restarted.get());
        assert!(ops.stopped.get());
        assert!(ops.started.get());
    }

    #[test]
    fn running_graph_can_decline_restart_without_touching_daemon() {
        let ops = fake(true, false);
        restart_daemon_after_update(&ops, true, &|| false).unwrap();
        assert!(!ops.restarted.get());
        assert!(!ops.stopped.get());
        assert!(!ops.started.get());
    }

    #[test]
    fn running_graph_can_accept_restart() {
        let ops = fake(true, true);
        restart_daemon_after_update(&ops, true, &|| true).unwrap();
        assert!(ops.restarted.get());
    }
}
