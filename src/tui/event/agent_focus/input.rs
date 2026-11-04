//! Keyboard input, scroll reset and the input buffer for the focused agent.
//! Split out of `agent_focus.rs` to keep every production file within the
//! file-size gate.

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::super::terminal_warp::{
    handle_terminal_direct_pty_key, handle_terminal_warp_key, record_terminal_command,
};
use super::{focused_agent, focused_agent_mut, FocusedAgent};
use crate::tui::agent::key_to_bytes;
use crate::tui::app::types::App;

pub(super) fn reset_scroll_on_input(app: &mut App, target: FocusedAgent, code: KeyCode) {
    if !matches!(
        code,
        KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace | KeyCode::Tab
    ) {
        return;
    }

    let Some(agent) = focused_agent_mut(app, target) else {
        return;
    };
    if agent.scroll_offset == 0 {
        return;
    }

    agent.scroll_offset = 0;
}

pub(super) fn handle_target_input(
    app: &mut App,
    target: FocusedAgent,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<bool> {
    match target {
        FocusedAgent::Interactive(idx) => {
            handle_interactive_input(app, idx, code, modifiers);
            Ok(false)
        }
        FocusedAgent::Terminal(idx) => handle_terminal_input(app, idx, code, modifiers),
    }
}

fn handle_interactive_input(app: &mut App, idx: usize, code: KeyCode, modifiers: KeyModifiers) {
    let target = FocusedAgent::Interactive(idx);
    if code == KeyCode::Enter {
        record_interactive_prompt(app, idx);
        clear_input_buffer(app, target);
        return;
    }

    track_plain_input(app, target, code, modifiers);
}

fn record_interactive_prompt(app: &mut App, idx: usize) {
    if app.interactive_agents[idx].is_sensitive_input_active() {
        return;
    }

    let Some(captured) = trimmed_input_buffer(app, FocusedAgent::Interactive(idx)) else {
        return;
    };
    if captured.is_empty() {
        return;
    }

    app.interactive_agents[idx].record_prompt(&captured);
}

fn handle_terminal_input(
    app: &mut App,
    idx: usize,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<bool> {
    if code == KeyCode::Char('w') && modifiers.contains(KeyModifiers::CONTROL) {
        toggle_terminal_warp_mode(app, idx);
        return Ok(true);
    }

    if app.terminal_agents[idx].warp_mode {
        return handle_terminal_warp_input(app, idx, code, modifiers);
    }

    track_terminal_input(app, idx, code, modifiers);
    Ok(false)
}

fn toggle_terminal_warp_mode(app: &mut App, idx: usize) {
    app.terminal_agents[idx].warp_mode = !app.terminal_agents[idx].warp_mode;
    app.terminal_agents[idx].warp_passthrough = false;
    // Manual override in both directions: the automatic rule re-asserts on
    // the next submit or prompt return.
    app.terminal_agents[idx].warp_command_running = false;
}

fn handle_terminal_warp_input(
    app: &mut App,
    idx: usize,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<bool> {
    app.terminal_agents[idx].refresh_warp_command_state();
    if app.terminal_agents[idx].should_bypass_warp_input() {
        handle_terminal_direct_pty_key(app, idx, code, modifiers)?;
        return Ok(true);
    }

    handle_terminal_warp_key(app, idx, code, modifiers)?;
    Ok(true)
}

fn track_terminal_input(app: &mut App, idx: usize, code: KeyCode, modifiers: KeyModifiers) {
    let target = FocusedAgent::Terminal(idx);
    if code == KeyCode::Enter {
        let captured = trimmed_input_buffer(app, target).unwrap_or_default();
        record_terminal_command(app, idx, &captured);
        clear_input_buffer(app, target);
        return;
    }
    if code == KeyCode::Tab {
        return;
    }

    track_plain_input(app, target, code, modifiers);
}

fn track_plain_input(app: &mut App, target: FocusedAgent, code: KeyCode, modifiers: KeyModifiers) {
    let KeyCode::Char(ch) = code else {
        if code == KeyCode::Backspace {
            pop_input_buffer(app, target);
        }
        return;
    };
    if modifiers.contains(KeyModifiers::CONTROL) {
        return;
    }

    let _ = with_input_buffer_mut(app, target, |input| input.push(ch));
}

fn trimmed_input_buffer(app: &App, target: FocusedAgent) -> Option<String> {
    let agent = focused_agent(app, target)?;
    let Ok(input) = agent.input_buffer.lock() else {
        return None;
    };

    Some(input.trim().to_string())
}

fn with_input_buffer_mut<R>(
    app: &mut App,
    target: FocusedAgent,
    f: impl FnOnce(&mut String) -> R,
) -> Option<R> {
    let agent = focused_agent_mut(app, target)?;
    let Ok(mut input) = agent.input_buffer.lock() else {
        return None;
    };

    Some(f(&mut input))
}

fn clear_input_buffer(app: &mut App, target: FocusedAgent) {
    let _ = with_input_buffer_mut(app, target, String::clear);
}

fn pop_input_buffer(app: &mut App, target: FocusedAgent) {
    let _ = with_input_buffer_mut(app, target, |input| {
        input.pop();
    });
}

pub(super) fn forward_key_to_focused_agent(
    app: &mut App,
    target: FocusedAgent,
    code: KeyCode,
    modifiers: KeyModifiers,
) {
    // CT26: swallow a copy-reflex Ctrl+C before it becomes a byte on the PTY
    // (see `event::ctrl_c`). Every other key, and every terminal pane, falls
    // through to today's forwarding unchanged.
    if super::super::ctrl_c::handle_ctrl_c(app, target, code, modifiers) {
        return;
    }
    let bytes = key_to_bytes(code, modifiers);
    if bytes.is_empty() {
        return;
    }

    let Some(agent) = focused_agent_mut(app, target) else {
        return;
    };
    let _ = agent.write_to_pty(&bytes);
}

#[cfg(test)]
mod warp_toggle_override_tests {
    use super::*;
    use crate::db::Database;
    use crate::tui::agent::InteractiveAgent;
    use crate::tui::app::types::{AgentEntry, Focus};
    use ratatui::style::Color;
    use std::sync::{Arc, Mutex};
    use tempfile::{tempdir, NamedTempFile};

    fn test_app(agent: InteractiveAgent) -> App {
        let tmp = NamedTempFile::new().expect("create temp database");
        let db = Arc::new(Database::new(&tmp.path().to_path_buf()).expect("create database"));
        let data_dir = tempdir().expect("create data directory");
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.terminal_agents.push(agent);
        app.agents = vec![AgentEntry::Terminal(0)];
        app.selected = 0;
        app.focus = Focus::Agent;
        app
    }

    fn warp_active(app: &App) -> bool {
        app.terminal_agents[0].warp_mode && !app.terminal_agents[0].should_bypass_warp_input()
    }

    fn buffer_text(app: &App) -> String {
        app.terminal_agents[0]
            .input_buffer
            .lock()
            .expect("lock input buffer")
            .clone()
    }

    fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        handle_terminal_input(app, 0, code, modifiers).expect("terminal input");
    }

    fn ctrl_w(app: &mut App) {
        press(app, KeyCode::Char('w'), KeyModifiers::CONTROL);
    }

    #[test]
    fn toggle_clears_the_latch_both_ways_and_submit_reasserts() {
        let mut agent = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("toggle-override"),
            &[],
            Color::White,
        )
        .expect("spawn terminal");
        let written = Arc::new(Mutex::new(Vec::new()));
        agent.test_sink = Some(Arc::clone(&written));
        let mut app = test_app(agent);

        // Submit latches; the box withdraws.
        {
            let buf = app.terminal_agents[0].input_buffer.clone();
            buf.lock().expect("lock buffer").push_str("sleep 9");
            app.terminal_agents[0].warp_cursor = 7;
        }
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.terminal_agents[0].warp_command_running);
        assert!(!warp_active(&app));

        // Warp -> native clears the latch (override, direction one).
        ctrl_w(&mut app);
        assert!(!app.terminal_agents[0].warp_mode);
        assert!(!app.terminal_agents[0].warp_command_running);

        // A command is still running behind the native pane; toggling back
        // clears the latch again so the box comes back anyway (direction two).
        app.terminal_agents[0].warp_command_running = true;
        app.terminal_agents[0]
            .vt
            .lock()
            .expect("lock vt")
            .process(b"? Pick one: ");
        ctrl_w(&mut app);
        assert!(app.terminal_agents[0].warp_mode);
        assert!(!app.terminal_agents[0].warp_command_running);
        assert!(warp_active(&app), "box back despite the running command");

        // During the override keys go back to the box, not the PTY.
        written.lock().expect("lock sink").clear();
        press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(buffer_text(&app), "y");
        assert!(written.lock().expect("lock sink").is_empty());

        // The next submit re-asserts the automatic rule.
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.terminal_agents[0].warp_command_running);
        assert!(!warp_active(&app));
        app.terminal_agents[0].kill();
    }

    #[test]
    fn warp_key_path_returns_the_box_at_an_idle_prompt() {
        let mut agent = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("key-refresh"),
            &[],
            Color::White,
        )
        .expect("spawn terminal");
        let written = Arc::new(Mutex::new(Vec::new()));
        agent.test_sink = Some(Arc::clone(&written));
        let mut app = test_app(agent);
        // Latch staged as a submit would; the shell is already back.
        app.terminal_agents[0].warp_command_running = true;
        app.terminal_agents[0]
            .vt
            .lock()
            .expect("lock vt")
            .process(b"user@host:~$ ");

        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);

        assert!(!app.terminal_agents[0].warp_command_running);
        assert_eq!(buffer_text(&app), "x");
        assert!(written.lock().expect("lock sink").is_empty());
        assert!(warp_active(&app));
        app.terminal_agents[0].kill();
    }
}
