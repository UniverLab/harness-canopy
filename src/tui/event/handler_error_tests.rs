//! Handler-error routing: handler errors notify and continue, only a
//! terminal read error exits. Launch failures on every launch path surface
//! as `Could not open session: ...` with the loop still running.

use std::sync::Arc;

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::handler_errors::{report_handler_error, route_error, ErrorRoute, ErrorSource};
use super::launchpad::handle_launchpad_key;
use super::new_agent_dialog::handle_dialog_key;
use super::new_agent_mode_step::handle_mode_step_key;
use crate::db::Database;
use crate::domain::models::Cli;
use crate::tui::app::dialog::launchpad::LaunchpadDialog;
use crate::tui::app::dialog::new_agent::mode_step::NEW_ROW;
use crate::tui::app::dialog::{DialogStep, NewAgentDialog, NewTaskType};
use crate::tui::app::types::{App, Focus};
use crate::tui::app::version_check::VersionNotice;

/// Point `$HOME` at `path` for the test body: the launchpad path persists CLI
/// usage stats under the real home (`App::record_cli_usage`), and a test must
/// never touch the developer's `~/.canopy`. Same guard as
/// `daemon::handler::endpoint_tests`: safe under nextest's one process per
/// test, which is how the gates run.
struct HomeVar {
    prev: Option<std::ffi::OsString>,
}

impl HomeVar {
    fn set(path: &std::path::Path) -> Self {
        let prev = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", path) };
        HomeVar { prev }
    }
}

impl Drop for HomeVar {
    fn drop(&mut self) {
        match &self.prev {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

/// A bare `App` over a scratch database and data dir; the temp dir must stay
/// alive while the app (and its database) is used.
fn test_app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("scratch dir");
    let db = Arc::new(Database::new(&dir.path().join("test.db")).expect("scratch database"));
    let app = App::new(
        db,
        dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("test app");
    (app, dir)
}

fn background_dialog(workdir: &str) -> NewAgentDialog {
    let mut dialog = NewAgentDialog::new(Some(workdir));
    dialog.task_type = NewTaskType::Background;
    dialog.prompt = "test prompt".to_string();
    dialog.available_clis = vec![Cli::new("claude")];
    dialog.cli_configs = vec![None];
    dialog.cli_index = 0;
    dialog
}

#[test]
fn read_errors_route_to_exit() {
    assert_eq!(route_error(ErrorSource::Read), ErrorRoute::Exit);
}

#[test]
fn handler_errors_route_to_continue() {
    assert_eq!(route_error(ErrorSource::Handler), ErrorRoute::Continue);
}

#[test]
fn report_sets_the_notice_and_keeps_running() {
    let (mut app, _dir) = test_app();
    report_handler_error(
        &mut app,
        "Could not open session",
        &anyhow::anyhow!("hub connection closed"),
    );
    assert!(app.running);
    assert_eq!(
        app.hub_notice.as_deref(),
        Some("Could not open session: hub connection closed")
    );
}

#[test]
fn report_writes_the_notice_to_the_tui_log() {
    let (mut app, dir) = test_app();
    report_handler_error(
        &mut app,
        "Could not open session",
        &anyhow::anyhow!("hub connection closed"),
    );
    let log = std::fs::read_to_string(dir.path().join("logs/tui.log")).expect("tui log");
    assert!(
        log.contains("Could not open session: hub connection closed"),
        "log: {log}"
    );
}

#[test]
fn launchpad_launch_failure_sets_the_notice_and_keeps_running() {
    let (mut app, dir) = test_app();
    // The confirm reaches the real spawn path now, which records CLI usage
    // under `$HOME`; keep that out of the developer's `~/.canopy`.
    let _home = HomeVar::set(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();
    let mut dialog = NewAgentDialog::new(Some(workdir.as_str()));
    // `cat` always spawns, so the launch itself succeeds and only the refresh
    // below fails (a harness the machine lacks would fail the spawn first).
    dialog.available_clis = vec![Cli::new("cat")];
    dialog.cli_configs = vec![None];
    dialog.cli_index = 0;
    app.pending_launch_dialog = Some(dialog);
    app.launchpad_dialog = Some(LaunchpadDialog {
        workdir,
        recent_missions: Vec::new(),
        selected_index: 0,
        new_mission: "test mission".to_string(),
        cursor: 12,
        submit_blocked: false,
        prev_focus: None,
    });
    app.focus = Focus::LaunchpadDialog;
    // Break the refresh that runs after the launch: the DB error stands in for
    // the daemon-down failure of the original report. The bookkeeping write is
    // no longer a launch failure (it is logged and the open continues).
    app.db
        .exec_test_sql("DROP TABLE agents;")
        .expect("drop the agents table");

    handle_launchpad_key(&mut app, KeyCode::Enter).expect("launch errors must stay in the loop");

    assert!(app.running, "a failed launch must not stop the TUI");
    let notice = app
        .hub_notice
        .as_deref()
        .expect("a failed launch must set the notice");
    assert!(
        notice.starts_with("Could not open session: "),
        "notice: {notice}"
    );
    assert!(
        notice.contains("agents"),
        "the notice must carry the cause: {notice}"
    );
    assert!(
        app.launchpad_dialog.is_none() && matches!(app.focus, Focus::Preview),
        "the consumed dialog must not leave a dangling focus"
    );
    for agent in &mut app.interactive_agents {
        agent.kill();
    }
}

#[test]
fn dialog_launch_failure_sets_the_notice_and_keeps_running() {
    let (mut app, dir) = test_app();
    let workdir = dir.path().to_string_lossy().to_string();
    app.new_agent_dialog = Some(background_dialog(&workdir));
    app.focus = Focus::NewAgentDialog;
    app.db
        .exec_test_sql("DROP TABLE agents;")
        .expect("drop the agents table");

    handle_dialog_key(&mut app, KeyCode::Enter, KeyModifiers::empty())
        .expect("launch errors must stay in the loop");

    assert!(app.running, "a failed launch must not stop the TUI");
    let notice = app
        .hub_notice
        .as_deref()
        .expect("a failed launch must set the notice");
    assert!(
        notice.starts_with("Could not open session: "),
        "notice: {notice}"
    );
    assert!(
        app.new_agent_dialog.is_none() && matches!(app.focus, Focus::Home),
        "the consumed dialog must not leave a dangling focus"
    );
}

#[test]
fn version_notice_blocks_keys_until_esc_dismisses_it() {
    let (mut app, _dir) = test_app();
    app.focus = Focus::Home;
    app.version_notice = Some(VersionNotice::mismatch(
        "1.2.3-fixture",
        "9.9.9-fixture".to_string(),
    ));

    super::handle_key(&mut app, KeyCode::F(10), KeyModifiers::empty()).expect("keys");
    assert!(
        !app.quit_confirm,
        "F10 must be swallowed while the notice is up"
    );
    assert!(app.version_notice.is_some());

    super::handle_key(&mut app, KeyCode::Esc, KeyModifiers::empty()).expect("keys");
    assert!(app.version_notice.is_none(), "Esc dismisses the notice");
    assert!(
        !app.quit_confirm,
        "the dismissing Esc must not leak through"
    );

    super::handle_key(&mut app, KeyCode::F(10), KeyModifiers::empty()).expect("keys");
    assert!(app.quit_confirm, "keys must flow again after dismiss");
}

#[test]
fn mode_step_launch_failure_sets_the_notice_and_keeps_running() {
    let (mut app, dir) = test_app();
    let workdir = dir.path().to_string_lossy().to_string();
    let mut dialog = background_dialog(&workdir);
    dialog.step = DialogStep::Mode;
    dialog.mode_index = NEW_ROW;
    app.new_agent_dialog = Some(dialog);
    app.focus = Focus::NewAgentDialog;
    app.db
        .exec_test_sql("DROP TABLE agents;")
        .expect("drop the agents table");

    handle_mode_step_key(&mut app, KeyCode::Enter).expect("launch errors must stay in the loop");

    assert!(app.running, "a failed launch must not stop the TUI");
    let notice = app
        .hub_notice
        .as_deref()
        .expect("a failed launch must set the notice");
    assert!(
        notice.starts_with("Could not open session: "),
        "notice: {notice}"
    );
    assert!(
        app.new_agent_dialog.is_none() && matches!(app.focus, Focus::Home),
        "the consumed dialog must not leave a dangling focus"
    );
}
