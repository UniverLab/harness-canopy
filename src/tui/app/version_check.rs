//! Daemon version check: the TUI compares its own version with the one
//! the running daemon recorded, and raises a blocking notice on mismatch.

use crate::application::ports::StateRepository;
use crate::db::Database;

/// Blocking notice: the TUI and the daemon disagree about the version, or
/// the daemon's version could not be read at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VersionNotice {
    /// This TUI's version (`CARGO_PKG_VERSION`).
    pub tui_version: String,
    /// The daemon's recorded version; `None` when unreadable.
    pub daemon_version: Option<String>,
}

impl VersionNotice {
    /// The daemon recorded a version, but it is not ours.
    pub fn mismatch(tui_version: &str, daemon_version: String) -> Self {
        Self {
            tui_version: tui_version.to_string(),
            daemon_version: Some(daemon_version),
        }
    }

    /// The daemon's version could not be read (daemon down, DB error).
    pub fn unreachable(tui_version: &str) -> Self {
        Self {
            tui_version: tui_version.to_string(),
            daemon_version: None,
        }
    }

    /// True when there is no daemon version to name.
    pub fn is_unreachable(&self) -> bool {
        self.daemon_version.is_none()
    }

    /// Lines for the blocking dialog: what disagrees, what to do, the keys.
    pub fn message_lines(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(3);
        if self.is_unreachable() {
            lines.push(format!(
                "Could not read the daemon version (TUI version {}).",
                self.tui_version
            ));
        } else if let Some(daemon) = self.daemon_version.as_deref() {
            lines.push(format!(
                "Daemon version {daemon} does not match TUI version {}.",
                self.tui_version
            ));
        }
        lines.push("Restart the daemon so both sides match.".to_string());
        lines.push("R: restart daemon · Esc: dismiss".to_string());
        lines
    }
}

/// Read the daemon's recorded version and compare it with this TUI's.
/// `None` when they match; `Some` on mismatch or when the daemon's version
/// is unreadable. Reads the same `version` key `canopy daemon status` prints.
pub(crate) fn check_daemon_version(db: &Database) -> Option<VersionNotice> {
    decide_version_notice(env!("CARGO_PKG_VERSION"), db.get_state("version"))
}

/// Pure decision behind [`check_daemon_version`]: equal versions clear the
/// notice, anything else raises it.
pub(crate) fn decide_version_notice(
    tui_version: &str,
    stored: anyhow::Result<Option<String>>,
) -> Option<VersionNotice> {
    match stored {
        Ok(Some(daemon)) if daemon == tui_version => None,
        Ok(Some(daemon)) => Some(VersionNotice::mismatch(tui_version, daemon)),
        Ok(None) | Err(_) => Some(VersionNotice::unreachable(tui_version)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_versions_yield_no_notice() {
        let notice = decide_version_notice("1.2.3-fixture", Ok(Some("1.2.3-fixture".to_string())));
        assert!(notice.is_none());
    }

    #[test]
    fn mismatch_names_both_versions() {
        let notice = decide_version_notice("1.2.3-fixture", Ok(Some("9.9.9-fixture".to_string())))
            .expect("a version mismatch must raise the notice");
        assert!(!notice.is_unreachable());
        let text = notice.message_lines().join("\n");
        assert!(text.contains("1.2.3-fixture"), "text: {text}");
        assert!(text.contains("9.9.9-fixture"), "text: {text}");
    }

    #[test]
    fn db_error_yields_the_unreachable_notice() {
        let notice = decide_version_notice(
            "1.2.3-fixture",
            Err(anyhow::anyhow!("daemon database unreadable")),
        )
        .expect("a DB error must raise the notice");
        assert!(notice.is_unreachable());
    }

    #[test]
    fn missing_key_yields_the_unreachable_notice() {
        let notice = decide_version_notice("1.2.3-fixture", Ok(None))
            .expect("a missing version key must raise the notice");
        assert!(notice.is_unreachable());
    }

    #[test]
    fn scratch_db_round_trip_matches_and_mismatches() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let db = Database::new(&dir.path().join("test.db")).expect("scratch database");
        db.set_state("version", env!("CARGO_PKG_VERSION"))
            .expect("record the TUI version");
        assert!(
            check_daemon_version(&db).is_none(),
            "equal versions must clear the notice"
        );
        db.set_state("version", "0.0.0-fixture")
            .expect("record a fixture version");
        let notice = check_daemon_version(&db).expect("a fixture mismatch must raise the notice");
        assert_eq!(notice.daemon_version.as_deref(), Some("0.0.0-fixture"));
    }
}
