//! Migrating an installed systemd unit to `KillMode=mixed`.
//!
//! systemd's default `KillMode=control-group` SIGTERMs the daemon and every
//! session child at the same instant, so the hub records the children's
//! deaths and finishes their rows before the daemon is gone. With `mixed`,
//! only the daemon is signalled; the children die with the control group
//! after it exits, when nobody is left to record their exit, and their rows
//! stay resumable.
//!
//! The migration inserts the two directives into `[Service]` and keeps every
//! other line of the file exactly as it is — re-rendering the whole unit
//! would clobber hand edits. Drop-in directories are never touched.

use anyhow::Result;

use crate::daemon::process::{CommandRunner, RealRunner};

/// The two directives that let session children outlive the daemon.
const KILL_MODE_LINES: [&str; 2] = ["KillMode=mixed", "TimeoutStopSec=30"];

/// Whether a unit's `[Service]` section lacks `KillMode=mixed`. Scoped to
/// that section: a directive in another section does not count.
pub(crate) fn unit_lacks_kill_mode_mixed(unit_content: &str) -> bool {
    let mut in_service = false;
    for line in unit_content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_service = trimmed == "[Service]";
            continue;
        }
        if in_service && trimmed == KILL_MODE_LINES[0] {
            return false;
        }
    }
    true
}

/// Surgically add [`KILL_MODE_LINES`] to a unit's `[Service]` section,
/// keeping every other line exactly as it is. `None` when the directive is
/// already there or the unit has no `[Service]` header to insert into.
pub(crate) fn insert_kill_mode_mixed(unit_content: &str) -> Option<String> {
    if !unit_lacks_kill_mode_mixed(unit_content) {
        return None;
    }
    let mut offset = 0usize;
    for line in unit_content.split_inclusive('\n') {
        let header = line.trim_end_matches(['\n', '\r']);
        if header == "[Service]" {
            let newline = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
            let mut updated = String::with_capacity(unit_content.len() + 48);
            updated.push_str(&unit_content[..offset + line.len()]);
            if !line.ends_with('\n') {
                updated.push('\n');
            }
            for directive in KILL_MODE_LINES {
                updated.push_str(directive);
                updated.push_str(newline);
            }
            updated.push_str(&unit_content[offset + line.len()..]);
            return Some(updated);
        }
        offset += line.len();
    }
    None
}

/// Migrate an installed unit that predates `KillMode=mixed`: insert the two
/// directives and reload systemd. Returns whether the file changed.
pub(crate) fn ensure_kill_mode_mixed(unit_path: &std::path::Path) -> Result<bool> {
    ensure_kill_mode_mixed_with(&RealRunner, unit_path)
}

/// [`ensure_kill_mode_mixed`] over an injected runner, so tests never call
/// the real `systemctl`.
pub(crate) fn ensure_kill_mode_mixed_with(
    runner: &dyn CommandRunner,
    unit_path: &std::path::Path,
) -> Result<bool> {
    let Ok(existing) = std::fs::read_to_string(unit_path) else {
        return Ok(false);
    };
    let Some(updated) = insert_kill_mode_mixed(&existing) else {
        return Ok(false);
    };
    std::fs::write(unit_path, updated)?;
    let _ = runner.run("systemctl", &["--user", "daemon-reload"]);
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

    #[test]
    fn unit_lacks_kill_mode_mixed_only_counts_the_service_section() {
        assert!(unit_lacks_kill_mode_mixed("[Service]\nExecStart=/x\n"));
        assert!(!unit_lacks_kill_mode_mixed(
            "[Service]\nKillMode=mixed\nExecStart=/x\n"
        ));
        assert!(
            unit_lacks_kill_mode_mixed("[Unit]\nKillMode=mixed\n\n[Service]\nExecStart=/x\n"),
            "a directive outside [Service] does not count"
        );
    }

    #[test]
    fn insert_kill_mode_mixed_preserves_every_other_line() {
        let original = "[Unit]\nDescription=canopy daemon\n\n[Service]\nType=simple\nExecStart=/usr/bin/canopy serve --port 7755\nEnvironment=FOO=bar\n\n[Install]\nWantedBy=default.target\n";
        let updated = insert_kill_mode_mixed(original).expect("migration inserts the directives");
        assert!(updated.contains("KillMode=mixed"));
        assert!(updated.contains("TimeoutStopSec=30"));
        assert!(!unit_lacks_kill_mode_mixed(&updated));
        for line in original.lines() {
            assert!(
                updated.lines().any(|kept| kept == line),
                "line lost: {line}"
            );
        }
        assert_eq!(updated.lines().count(), original.lines().count() + 2);
    }

    #[test]
    fn insert_kill_mode_mixed_returns_none_when_already_present() {
        let content = "[Service]\nKillMode=mixed\nTimeoutStopSec=30\nExecStart=/x\n";
        assert_eq!(insert_kill_mode_mixed(content), None);
    }

    #[test]
    fn insert_kill_mode_mixed_returns_none_without_a_service_section() {
        assert_eq!(insert_kill_mode_mixed("[Unit]\nDescription=x\n"), None);
    }

    #[test]
    fn ensure_kill_mode_mixed_migrates_the_unit_and_reloads_once() {
        let tmp = tempfile::tempdir().unwrap();
        let unit_path = tmp.path().join("canopy.service");
        let drop_in_dir = tmp.path().join("canopy.service.d");
        std::fs::create_dir_all(&drop_in_dir).unwrap();
        let drop_in = drop_in_dir.join("override.conf");
        let drop_in_content = "[Service]\nEnvironment=KEEP=yes\n";
        std::fs::write(&drop_in, drop_in_content).unwrap();
        std::fs::write(
            &unit_path,
            "[Service]\nExecStart=/usr/bin/canopy serve --port 7755\nEnvironment=FOO=bar\n",
        )
        .unwrap();

        let runner = RunRecorder::default();
        assert!(ensure_kill_mode_mixed_with(&runner, &unit_path).unwrap());

        let updated = std::fs::read_to_string(&unit_path).unwrap();
        assert!(updated.contains("KillMode=mixed"));
        assert!(updated.contains("TimeoutStopSec=30"));
        assert!(updated.contains("Environment=FOO=bar"));
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
    fn ensure_kill_mode_mixed_leaves_a_migrated_unit_byte_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let unit_path = tmp.path().join("canopy.service");
        let content = "[Service]\nKillMode=mixed\nTimeoutStopSec=30\nExecStart=/usr/bin/canopy serve --port 7755\n";
        std::fs::write(&unit_path, content).unwrap();

        let runner = RunRecorder::default();
        assert!(!ensure_kill_mode_mixed_with(&runner, &unit_path).unwrap());

        assert_eq!(std::fs::read_to_string(&unit_path).unwrap(), content);
        assert!(runner.calls().is_empty());
    }
}
