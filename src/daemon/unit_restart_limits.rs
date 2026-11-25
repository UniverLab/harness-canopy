//! Migrating an installed systemd unit's restart limits into `[Unit]`.
//!
//! systemd reads `StartLimitIntervalSec=` and `StartLimitBurst=` only from
//! the `[Unit]` section; an older template wrote them under `[Service]`,
//! where every boot logs `Unknown key name 'StartLimitIntervalSec' in
//! section 'Service', ignoring` and the limit is never applied. The same
//! migration also adds the `network-online.target` wait, so the relay and
//! announcements clients no longer start before DNS works.
//!
//! The migration strips the two directives from `[Service]` (remembering
//! hand-edited values) and inserts the missing lines directly after the
//! `[Unit]` header — every other line stays exactly as it is, and drop-in
//! directories are never touched.

use anyhow::Result;

use crate::daemon::process::{CommandRunner, RealRunner};

/// A line the migrated `[Unit]` section must contain.
const AFTER_ONLINE: &str = "After=network-online.target";
/// A line the migrated `[Unit]` section must contain.
const WANTS_ONLINE: &str = "Wants=network-online.target";

/// What the scan of a unit's contents found.
struct UnitScan {
    seen_unit_header: bool,
    has_interval: bool,
    has_burst: bool,
    has_after_online: bool,
    has_wants_online: bool,
    remembered_interval: Option<String>,
    remembered_burst: Option<String>,
    service_limit_lines: usize,
}

/// Whether `line` (already trimmed) opens `section`.
fn is_header(trimmed: &str, section: &str) -> bool {
    trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed == section
}

/// Scan the `[Unit]` and `[Service]` sections: which `[Unit]` lines are
/// already present, and which `StartLimit*` values `[Service]` still holds.
fn scan_unit(unit_content: &str) -> UnitScan {
    let mut scan = UnitScan {
        seen_unit_header: false,
        has_interval: false,
        has_burst: false,
        has_after_online: false,
        has_wants_online: false,
        remembered_interval: None,
        remembered_burst: None,
        service_limit_lines: 0,
    };
    let mut in_unit = false;
    let mut in_service = false;
    for line in unit_content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_unit = is_header(trimmed, "[Unit]");
            in_service = is_header(trimmed, "[Service]");
            scan.seen_unit_header |= in_unit;
            continue;
        }
        if in_unit {
            if trimmed.starts_with("StartLimitIntervalSec=") {
                scan.has_interval = true;
            } else if trimmed.starts_with("StartLimitBurst=") {
                scan.has_burst = true;
            } else if trimmed == AFTER_ONLINE {
                scan.has_after_online = true;
            } else if trimmed == WANTS_ONLINE {
                scan.has_wants_online = true;
            }
        } else if in_service {
            if let Some(value) = trimmed.strip_prefix("StartLimitIntervalSec=") {
                scan.remembered_interval = Some(value.trim().to_string());
                scan.service_limit_lines += 1;
            } else if let Some(value) = trimmed.strip_prefix("StartLimitBurst=") {
                scan.remembered_burst = Some(value.trim().to_string());
                scan.service_limit_lines += 1;
            }
        }
    }
    scan
}

/// The lines to insert directly after the `[Unit]` header: every required
/// line `[Unit]` does not already contain. A remembered hand-edited value
/// survives the move; otherwise the template default applies.
fn missing_unit_lines(scan: &UnitScan) -> Vec<String> {
    let mut missing = Vec::new();
    if !scan.has_interval {
        missing.push(format!(
            "StartLimitIntervalSec={}",
            scan.remembered_interval.as_deref().unwrap_or("60")
        ));
    }
    if !scan.has_burst {
        missing.push(format!(
            "StartLimitBurst={}",
            scan.remembered_burst.as_deref().unwrap_or("5")
        ));
    }
    if !scan.has_after_online {
        missing.push(AFTER_ONLINE.to_string());
    }
    if !scan.has_wants_online {
        missing.push(WANTS_ONLINE.to_string());
    }
    missing
}

/// Whether `line` (already trimmed) is a `StartLimit*` directive that must
/// leave the `[Service]` section.
fn is_service_limit_line(trimmed: &str) -> bool {
    trimmed.starts_with("StartLimitIntervalSec=") || trimmed.starts_with("StartLimitBurst=")
}

/// Surgically move the restart limits into `[Unit]` and add the
/// network-online wait, keeping every other line exactly as it is. `None`
/// when the content has no `[Unit]` header (a launchd plist) or when it
/// already needs nothing.
pub(crate) fn migrate_restart_limits(unit_content: &str) -> Option<String> {
    let scan = scan_unit(unit_content);
    if !scan.seen_unit_header {
        return None;
    }
    let missing = missing_unit_lines(&scan);
    if scan.service_limit_lines == 0 && missing.is_empty() {
        return None;
    }
    let mut updated = String::with_capacity(unit_content.len() + 128);
    let mut in_service = false;
    let mut inserted = false;
    for chunk in unit_content.split_inclusive('\n') {
        let trimmed = chunk.trim_end_matches(['\n', '\r']);
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_service = is_header(trimmed, "[Service]");
        }
        if in_service && is_service_limit_line(trimmed) {
            continue;
        }
        updated.push_str(chunk);
        if !inserted && is_header(trimmed, "[Unit]") {
            let newline = if chunk.ends_with("\r\n") {
                "\r\n"
            } else {
                "\n"
            };
            if !chunk.ends_with('\n') {
                updated.push('\n');
            }
            for line in &missing {
                updated.push_str(line);
                updated.push_str(newline);
            }
            inserted = true;
        }
    }
    Some(updated)
}

/// Migrate an installed unit that predates the `[Unit]` restart limits:
/// move the limits, add the network-online wait, and reload systemd.
/// Returns whether the file changed. Never restarts the daemon.
pub(crate) fn ensure_restart_limits(unit_path: &std::path::Path) -> Result<bool> {
    ensure_restart_limits_with(&RealRunner, unit_path)
}

/// [`ensure_restart_limits`] over an injected runner, so tests never call
/// the real `systemctl`.
pub(crate) fn ensure_restart_limits_with(
    runner: &dyn CommandRunner,
    unit_path: &std::path::Path,
) -> Result<bool> {
    let Ok(existing) = std::fs::read_to_string(unit_path) else {
        return Ok(false);
    };
    let Some(updated) = migrate_restart_limits(&existing) else {
        return Ok(false);
    };
    std::fs::write(unit_path, updated)?;
    let _ = runner.run("systemctl", &["--user", "daemon-reload"]);
    println!(
        "Updated the service unit: restart limits moved to [Unit] and network-online wait added; restart the daemon to apply it"
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory runner that records the commands it was asked to run.
    #[derive(Default)]
    struct RunRecorder {
        runs: std::cell::RefCell<Vec<(String, Vec<String>)>>,
    }

    impl RunRecorder {
        fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.runs.borrow().clone()
        }
    }

    impl CommandRunner for RunRecorder {
        fn run(&self, prog: &str, args: &[&str]) -> bool {
            self.runs.borrow_mut().push((
                prog.to_string(),
                args.iter().map(|arg| (*arg).to_string()).collect(),
            ));
            true
        }

        fn run_capture(&self, _prog: &str, _args: &[&str]) -> Option<String> {
            None
        }
    }

    /// Split a unit into its `[Unit]` and `[Service]` sections, so a
    /// directive that migrated into (or was duplicated into) another
    /// section can never be counted as if it were in the wrong one.
    fn section_lines(content: &str, header: &str) -> Vec<String> {
        let mut in_section = false;
        let mut lines = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                in_section = trimmed == header;
                continue;
            }
            if in_section {
                lines.push(trimmed.to_string());
            }
        }
        lines
    }

    /// The unit text the old template produced: limits under `[Service]`,
    /// no network-online wait.
    fn old_template_unit() -> String {
        "[Unit]\nDescription=canopy daemon\nAfter=network.target\n\n[Service]\nType=simple\nExecStart=/usr/bin/canopy serve --port 7755\nRestart=on-failure\nRestartSec=5\nStartLimitIntervalSec=60\nStartLimitBurst=5\n\n[Install]\nWantedBy=default.target\n".to_string()
    }

    #[test]
    fn migrate_moves_limits_and_adds_network_online_wait() {
        let original = old_template_unit();
        let updated = migrate_restart_limits(&original).expect("old unit needs migration");
        let unit = section_lines(&updated, "[Unit]");
        let service = section_lines(&updated, "[Service]");
        assert!(unit.contains(&"StartLimitIntervalSec=60".to_string()));
        assert!(unit.contains(&"StartLimitBurst=5".to_string()));
        assert!(unit.contains(&AFTER_ONLINE.to_string()));
        assert!(unit.contains(&WANTS_ONLINE.to_string()));
        assert!(service.iter().all(|line| !line.starts_with("StartLimit")));
        // The new lines land directly after the `[Unit]` header, in the
        // order `missing_unit_lines` produces them — nowhere else.
        let mut lines = updated.lines();
        assert_eq!(lines.next(), Some("[Unit]"));
        assert_eq!(lines.next(), Some("StartLimitIntervalSec=60"));
        assert_eq!(lines.next(), Some("StartLimitBurst=5"));
        assert_eq!(lines.next(), Some(AFTER_ONLINE));
        assert_eq!(lines.next(), Some(WANTS_ONLINE));
        assert_eq!(lines.next(), Some("Description=canopy daemon"));
        for line in original.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("StartLimitIntervalSec=")
                || trimmed.starts_with("StartLimitBurst=")
            {
                continue;
            }
            assert!(
                updated.lines().any(|kept| kept == line),
                "line lost: {line}"
            );
        }
    }

    #[test]
    fn migrate_keeps_a_hand_edited_burst_value() {
        let original = old_template_unit().replace("StartLimitBurst=5", "StartLimitBurst=9");
        let updated = migrate_restart_limits(&original).expect("old unit needs migration");
        let unit = section_lines(&updated, "[Unit]");
        assert!(unit.contains(&"StartLimitBurst=9".to_string()));
        assert!(!unit.contains(&"StartLimitBurst=5".to_string()));
    }

    #[test]
    fn migrate_keeps_a_hand_edited_interval_value() {
        let original =
            old_template_unit().replace("StartLimitIntervalSec=60", "StartLimitIntervalSec=120");
        let updated = migrate_restart_limits(&original).expect("old unit needs migration");
        let unit = section_lines(&updated, "[Unit]");
        assert!(unit.contains(&"StartLimitIntervalSec=120".to_string()));
    }

    #[test]
    fn migrate_returns_none_on_the_second_run() {
        let updated =
            migrate_restart_limits(&old_template_unit()).expect("old unit needs migration");
        assert_eq!(migrate_restart_limits(&updated), None);
    }

    #[test]
    fn migrate_returns_none_for_a_launchd_plist() {
        let plist = "<?xml version=\"1.0\"?>\n<plist>\n<dict>\n<key>Label</key>\n<string>com.canopy</string>\n</dict>\n</plist>\n";
        assert_eq!(migrate_restart_limits(plist), None);
    }

    #[test]
    fn migrate_returns_none_when_a_unit_is_already_current() {
        let current = "[Unit]\nDescription=canopy daemon\nAfter=network.target\nAfter=network-online.target\nWants=network-online.target\nStartLimitIntervalSec=60\nStartLimitBurst=5\n\n[Service]\nType=simple\nExecStart=/usr/bin/canopy serve --port 7755\n\n[Install]\nWantedBy=default.target\n";
        assert_eq!(migrate_restart_limits(current), None);
    }

    #[test]
    fn ensure_writes_the_file_and_reloads_once() {
        let tmp = tempfile::tempdir().unwrap();
        let unit_path = tmp.path().join("canopy.service");
        let drop_in_dir = tmp.path().join("canopy.service.d");
        std::fs::create_dir_all(&drop_in_dir).unwrap();
        let drop_in = drop_in_dir.join("override.conf");
        let drop_in_content = "[Service]\nEnvironment=KEEP=yes\n";
        std::fs::write(&drop_in, drop_in_content).unwrap();
        std::fs::write(&unit_path, old_template_unit()).unwrap();

        let runner = RunRecorder::default();
        assert!(ensure_restart_limits_with(&runner, &unit_path).unwrap());

        let updated = std::fs::read_to_string(&unit_path).unwrap();
        let unit = section_lines(&updated, "[Unit]");
        assert!(unit.contains(&"StartLimitBurst=5".to_string()));
        assert!(unit.contains(&AFTER_ONLINE.to_string()));
        assert_eq!(
            runner.calls(),
            vec![(
                "systemctl".to_string(),
                vec!["--user".to_string(), "daemon-reload".to_string()]
            )]
        );
        assert_eq!(
            std::fs::read_to_string(&drop_in).unwrap(),
            drop_in_content,
            "a drop-in directory is never touched"
        );
    }

    #[test]
    fn ensure_is_a_noop_for_a_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let unit_path = tmp.path().join("canopy.service");
        let runner = RunRecorder::default();
        assert!(!ensure_restart_limits_with(&runner, &unit_path).unwrap());
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn ensure_leaves_a_migrated_unit_byte_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let unit_path = tmp.path().join("canopy.service");
        let migrated =
            migrate_restart_limits(&old_template_unit()).expect("old unit needs migration");
        std::fs::write(&unit_path, &migrated).unwrap();

        let runner = RunRecorder::default();
        assert!(!ensure_restart_limits_with(&runner, &unit_path).unwrap());

        assert_eq!(std::fs::read_to_string(&unit_path).unwrap(), migrated);
        assert!(runner.calls().is_empty());
    }
}
