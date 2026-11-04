//! Error routing for the event loop: handler errors notify and continue,
//! terminal read errors exit.
//!
//! A failure inside a key/mouse/paste handler (a refused launch, a dropped
//! hub connection) must never kill the TUI: it lands in the footer's notice
//! slot and the loop keeps going. Only a failure of
//! [`crossterm::event::read`](ratatui::crossterm::event::read) itself —
//! terminal I/O is gone — exits the loop.

use crate::tui::app::types::App;

/// Where an event-loop error came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ErrorSource {
    /// `event::read` itself failed: the terminal is gone.
    Read,
    /// A key/mouse/paste handler failed: the terminal is fine.
    Handler,
}

/// What the event loop does with an error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ErrorRoute {
    /// Report the error and keep the loop running.
    Continue,
    /// Return the error so `run_tui` restores the terminal and exits.
    Exit,
}

/// Pure routing predicate: only a terminal read failure exits the loop.
pub(crate) fn route_error(source: ErrorSource) -> ErrorRoute {
    match source {
        ErrorSource::Read => ErrorRoute::Exit,
        ErrorSource::Handler => ErrorRoute::Continue,
    }
}

/// Surface a handler error in the footer's notice slot and log it; the loop
/// continues. `ctx` names the action that failed (`Could not open session`).
pub(crate) fn report_handler_error(app: &mut App, ctx: &str, err: &anyhow::Error) {
    let notice = format!("{ctx}: {err:#}");
    tracing::error!("{notice}");
    let line = format!("{} {notice}", chrono::Utc::now().to_rfc3339());
    crate::tui::logging::append_log(&crate::tui::logging::tui_log_path(&app.data_dir), &line);
    app.hub_notice = Some(notice);
}
