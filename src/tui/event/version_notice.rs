//! Keys for the blocking daemon-version notice: `R` restarts the daemon
//! and re-checks, `Esc` dismisses the notice for this run.

use anyhow::Result;
use ratatui::crossterm::event::KeyCode;

use crate::tui::app::types::App;
use crate::tui::app::version_check::check_daemon_version;

use super::handler_errors::report_handler_error;

/// Handle one key while the version notice is up. Every other key is
/// swallowed: the notice is blocking until dismissed or cleared.
pub(crate) fn handle_version_notice_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Esc => {
            app.version_notice = None;
        }
        KeyCode::Char('r' | 'R') => restart_daemon_and_recheck(app),
        _ => {}
    }
    Ok(())
}

/// Restart the daemon through the existing restart entry point, then
/// re-check: a restarted daemon records the new version and the notice
/// clears itself when both sides match again.
fn restart_daemon_and_recheck(app: &mut App) {
    let port = app.hub_port();
    match crate::daemon::daemon_start::restart_daemon_live(port) {
        Ok(_) => {
            let db = std::sync::Arc::clone(&app.db);
            app.version_notice = check_daemon_version(&db);
        }
        Err(e) => report_handler_error(app, "Could not restart daemon", &e),
    }
}
