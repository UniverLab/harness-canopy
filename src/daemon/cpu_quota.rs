//! CPU quota cap for the daemon's systemd unit.
//!
//! Graph work must never saturate every core of the host: agents run cargo
//! builds, tests, npm and Playwright outside the `cpu` mutex gate, so the
//! gate alone cannot bound them. The daemon instead caps its own cgroup
//! (`CPUQuota=` on `canopy.service`), which bounds every agent and gate it
//! spawns. The configured value is [`CONFIG_KEY`]; `0` disables the cap.
//! A changed value applies to the running daemon without a restart via
//! `systemctl --user set-property` (runtime and persistent).

use crate::daemon::process::{CommandRunner, SYSTEMD_UNIT_NAME};
use crate::domain::canopy_config::CanopyConfig;

/// The setting holding the cap, in percent of one core.
pub(crate) const CONFIG_KEY: &str = "daemon.cpu_quota_percent";

/// Job/thread count tools should size their worker counts to under a quota of
/// `quota_percent`: the cap in whole cores, rounded up, never below 2 so
/// a small budget still makes progress instead of serializing everything.
pub(crate) fn cores_from_quota(quota_percent: u32) -> u32 {
    quota_percent.div_ceil(100).max(2)
}

/// The `[Service]` directive for `quota_percent`, or `None` when the cap
/// is disabled and the unit must carry no line at all.
pub(crate) fn quota_unit_line(quota_percent: u32) -> Option<String> {
    if quota_percent == 0 {
        return None;
    }
    Some(format!("CPUQuota={quota_percent}%"))
}

/// Parse the applied quota, in percent, out of
/// `systemctl show -p CPUQuotaPerSecUSec` output. `None` means no finite
/// quota is set (`infinity`, the u64 maximum systemd uses for "unset", or
/// output that does not carry the property at all).
pub(crate) fn parse_quota_usec(output: &str) -> Option<u32> {
    let raw = output
        .lines()
        .find_map(|line| line.strip_prefix("CPUQuotaPerSecUSec="))?;
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("infinity") {
        return None;
    }
    let usec: u64 = raw.parse().ok()?;
    if usec == u64::MAX {
        return None;
    }
    // `CPUQuota=600%` reports as 6_000_000: percent is usec / 10_000.
    // Clamped, never `as`-truncated: a garbage value must not wrap into a
    // small percent the status line then reports as fact.
    Some(u32::try_from(usec / 10_000).unwrap_or(u32::MAX))
}

/// Where `systemctl --user set-property` persists a quota: a drop-in the
/// user (or a previous live apply) owns. Never written by canopy, only
/// respected: when it exists the unit carries no `CPUQuota=` line of its
/// own so the file that actually governs stays the only one.
pub(crate) fn drop_in_path() -> Option<std::path::PathBuf> {
    dirs::home_dir()
        .map(|home| home.join(".config/systemd/user.control/canopy.service.d/50-CPUQuota.conf"))
}

/// What the cgroup currently enforces on the daemon's unit.
pub(crate) struct QuotaProbe {
    /// Whether systemd could be reached at all. `false` on non-Linux and
    /// when the user instance is unavailable — distinct from "reached and
    /// no quota set", which still counts as reachable.
    pub(crate) reachable: bool,
    /// The quota systemd reports, in percent. `None` when no finite quota
    /// is set (or systemd is unreachable).
    pub(crate) applied_percent: Option<u32>,
    /// Why, in operator-facing words.
    pub(crate) reason: String,
}

impl QuotaProbe {
    /// Whether the cgroup enforces a quota right now.
    pub(crate) fn enforced(&self) -> bool {
        self.applied_percent.is_some()
    }

    fn unreachable(reason: &str) -> Self {
        Self {
            reachable: false,
            applied_percent: None,
            reason: reason.to_string(),
        }
    }
}

/// Read the quota systemd currently enforces on the daemon's unit. A pure
/// read: it never changes the unit, the drop-in, or the running daemon.
pub(crate) fn probe_with(runner: &dyn CommandRunner) -> QuotaProbe {
    if !cfg!(target_os = "linux") {
        return QuotaProbe::unreachable("CPU quota requires Linux with systemd");
    }
    let Some(output) = runner.run_capture(
        "systemctl",
        &[
            "--user",
            "show",
            SYSTEMD_UNIT_NAME,
            "-p",
            "CPUQuotaPerSecUSec",
        ],
    ) else {
        return QuotaProbe::unreachable(
            "systemd user instance unavailable (systemctl --user show failed)",
        );
    };
    match parse_quota_usec(&output) {
        Some(percent) => QuotaProbe {
            reachable: true,
            applied_percent: Some(percent),
            reason: "cgroup quota active".to_string(),
        },
        None => QuotaProbe {
            reachable: true,
            applied_percent: None,
            reason: format!("no CPUQuota set on {SYSTEMD_UNIT_NAME}"),
        },
    }
}

/// Apply `quota_percent` to the running daemon without a restart.
/// `systemctl set-property` is both runtime and persistent, so the value
/// survives a daemon restart; `0` resets the property (an empty
/// `CPUQuota=` is systemd's reset spelling).
pub(crate) fn apply_with(runner: &dyn CommandRunner, quota_percent: u32) -> Result<(), String> {
    let assignment = match quota_unit_line(quota_percent) {
        Some(line) => line,
        None => "CPUQuota=".to_string(),
    };
    let applied = runner.run(
        "systemctl",
        &["--user", "set-property", SYSTEMD_UNIT_NAME, &assignment],
    );
    if applied {
        Ok(())
    } else {
        Err(
            "systemctl --user set-property failed (no systemd user instance or no delegated cpu controller)"
                .to_string(),
        )
    }
}

/// The configured cap: the file's value, or the host-dependent default
/// when nothing is configured (or there is no home to load from).
pub(crate) fn loaded_quota_percent() -> u32 {
    dirs::home_dir()
        .map(|home| {
            CanopyConfig::load(&home.join(".canopy"))
                .daemon
                .cpu_quota_percent
        })
        .unwrap_or_else(|| CanopyConfig::default().daemon.cpu_quota_percent)
}

/// Size one spawned node's worker counts to the quota: cargo, nextest and the
/// Rust test harness all read these instead of defaulting to every core.
pub(crate) fn export_quota_env(command: &mut tokio::process::Command, quota_percent: u32) {
    let jobs = cores_from_quota(quota_percent).to_string();
    command.env("CARGO_BUILD_JOBS", &jobs);
    command.env("NEXTEST_TEST_THREADS", &jobs);
    command.env("RUST_TEST_THREADS", &jobs);
}

/// The `agent status` CPU quota section: the configured value, the value
/// the cgroup enforces, and whether it enforces one at all.
pub(crate) fn status_section(configured_percent: u32, runner: &dyn CommandRunner) -> String {
    let probe = probe_with(runner);
    let configured = if configured_percent == 0 {
        "disabled".to_string()
    } else {
        format!("{configured_percent}%")
    };
    let applied = probe
        .applied_percent
        .map(|percent| format!("{percent}%"))
        .unwrap_or_else(|| "none".to_string());
    let enforced = if probe.enforced() { "yes" } else { "no" };
    format!(
        "\nCPU quota: configured {configured}, applied {applied}, enforced: {enforced} ({})",
        probe.reason
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// In-memory [`CommandRunner`]: records every call and replays canned
    /// answers, so no test here shells out to the real `systemctl`.
    struct FakeRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
        capture: Option<String>,
        run_ok: bool,
    }

    impl FakeRunner {
        fn new(capture: Option<&str>, run_ok: bool) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                capture: capture.map(str::to_string),
                run_ok,
            }
        }

        fn recorded(&self) -> Vec<(String, Vec<String>)> {
            self.calls.borrow().clone()
        }

        fn record(&self, prog: &str, args: &[&str]) {
            self.calls.borrow_mut().push((
                prog.to_string(),
                args.iter().map(|arg| (*arg).to_string()).collect(),
            ));
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, prog: &str, args: &[&str]) -> bool {
            self.record(prog, args);
            self.run_ok
        }

        fn run_capture(&self, prog: &str, args: &[&str]) -> Option<String> {
            self.record(prog, args);
            self.capture.clone()
        }
    }

    #[test]
    fn cores_from_quota_rounds_up_to_whole_cores_with_a_floor_of_two() {
        assert_eq!(cores_from_quota(600), 6);
        assert_eq!(cores_from_quota(250), 3);
        assert_eq!(cores_from_quota(100), 2);
        assert_eq!(cores_from_quota(50), 2);
        assert_eq!(cores_from_quota(0), 2);
    }

    #[test]
    fn quota_unit_line_renders_percent_and_is_absent_when_disabled() {
        assert_eq!(quota_unit_line(600), Some("CPUQuota=600%".to_string()));
        assert_eq!(quota_unit_line(0), None);
    }

    #[test]
    fn parse_quota_usec_reads_percent_from_systemctl_show() {
        assert_eq!(parse_quota_usec("CPUQuotaPerSecUSec=6000000\n"), Some(600));
        assert_eq!(parse_quota_usec("CPUQuotaPerSecUSec=infinity\n"), None);
        assert_eq!(
            parse_quota_usec(&format!("CPUQuotaPerSecUSec={}\n", u64::MAX)),
            None
        );
        assert_eq!(parse_quota_usec("LoadState=loaded\n"), None);
        assert_eq!(parse_quota_usec("CPUQuotaPerSecUSec=not-a-number\n"), None);
        // Absurd but finite: clamps instead of wrapping into a small lie.
        assert_eq!(
            parse_quota_usec(&format!("CPUQuotaPerSecUSec={}\n", u64::MAX - 1)),
            Some(u32::MAX)
        );
    }

    #[test]
    fn drop_in_path_is_the_set_property_drop_in() {
        let path = drop_in_path().expect("test machine has a home directory");
        assert!(
            path.ends_with(".config/systemd/user.control/canopy.service.d/50-CPUQuota.conf"),
            "unexpected drop-in path: {}",
            path.display()
        );
    }

    #[test]
    fn probe_reports_the_enforced_quota() {
        let runner = FakeRunner::new(Some("CPUQuotaPerSecUSec=6000000\n"), true);
        let probe = probe_with(&runner);
        assert!(probe.reachable);
        assert_eq!(probe.applied_percent, Some(600));
        assert!(probe.enforced());
        assert_eq!(
            runner.recorded(),
            vec![(
                "systemctl".to_string(),
                vec![
                    "--user".to_string(),
                    "show".to_string(),
                    "canopy.service".to_string(),
                    "-p".to_string(),
                    "CPUQuotaPerSecUSec".to_string(),
                ],
            )],
        );
    }

    #[test]
    fn probe_is_reachable_but_unenforced_without_a_quota() {
        let runner = FakeRunner::new(Some("CPUQuotaPerSecUSec=infinity\n"), true);
        let probe = probe_with(&runner);
        assert!(probe.reachable);
        assert_eq!(probe.applied_percent, None);
        assert!(!probe.enforced());
    }

    #[test]
    fn probe_is_unreachable_when_systemctl_fails() {
        let runner = FakeRunner::new(None, false);
        let probe = probe_with(&runner);
        assert!(!probe.reachable);
        assert_eq!(probe.applied_percent, None);
        assert!(!probe.enforced());
        assert!(probe.reason.contains("systemd"), "{}", probe.reason);
    }

    #[test]
    fn apply_runs_set_property_with_the_percent() {
        let runner = FakeRunner::new(None, true);
        assert!(apply_with(&runner, 600).is_ok());
        assert_eq!(
            runner.recorded(),
            vec![(
                "systemctl".to_string(),
                vec![
                    "--user".to_string(),
                    "set-property".to_string(),
                    "canopy.service".to_string(),
                    "CPUQuota=600%".to_string(),
                ],
            )],
        );
    }

    #[test]
    fn apply_resets_the_property_when_disabled() {
        let runner = FakeRunner::new(None, true);
        assert!(apply_with(&runner, 0).is_ok());
        let recorded = runner.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].1.last().unwrap(), "CPUQuota=");
    }

    #[test]
    fn apply_reports_the_cause_when_systemctl_fails() {
        let runner = FakeRunner::new(None, false);
        let error = apply_with(&runner, 600).expect_err("failed run must error");
        assert!(error.contains("set-property"), "{error}");
    }

    #[test]
    fn export_sets_all_three_worker_counts_to_the_cap_in_cores() {
        let mut command = tokio::process::Command::new("sh");
        export_quota_env(&mut command, 600);
        let vars: std::collections::HashMap<_, _> = command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert_eq!(vars.get("CARGO_BUILD_JOBS").unwrap(), "6");
        assert_eq!(vars.get("NEXTEST_TEST_THREADS").unwrap(), "6");
        assert_eq!(vars.get("RUST_TEST_THREADS").unwrap(), "6");
    }

    #[test]
    fn status_shows_configured_applied_and_enforced() {
        let runner = FakeRunner::new(Some("CPUQuotaPerSecUSec=6000000\n"), true);
        let section = status_section(600, &runner);
        assert!(section.contains("configured 600%"), "{section}");
        assert!(section.contains("applied 600%"), "{section}");
        assert!(section.contains("enforced: yes"), "{section}");
    }

    #[test]
    fn status_names_a_disabled_cap_and_a_missing_quota() {
        let runner = FakeRunner::new(Some("CPUQuotaPerSecUSec=infinity\n"), true);
        let section = status_section(0, &runner);
        assert!(section.contains("configured disabled"), "{section}");
        assert!(section.contains("applied none"), "{section}");
        assert!(section.contains("enforced: no"), "{section}");
    }

    #[test]
    fn status_carries_the_reason_when_systemd_is_unreachable() {
        let runner = FakeRunner::new(None, false);
        let section = status_section(600, &runner);
        assert!(section.contains("enforced: no"), "{section}");
        assert!(section.contains("systemd"), "{section}");
    }
}
