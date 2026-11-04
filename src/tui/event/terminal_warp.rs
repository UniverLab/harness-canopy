use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use std::time::Duration;

use super::agent_focus::is_reserved_focus_key;
use super::search_picker::resolve_cd_path;
use crate::tui::agent::{key_to_bytes, InteractiveAgent};
use crate::tui::app::terminal_search::TerminalSearch;
use crate::tui::app::types::App;

// ── Clipboard paste in terminal warp mode ─────────────────────────────

fn paste_clipboard_to_terminal(app: &mut App, idx: usize) {
    let Some(text) = read_clipboard_from_terminal(app, idx) else {
        return;
    };
    let agent = &mut app.terminal_agents[idx];
    if agent.should_bypass_warp_input() || agent.warp_passthrough {
        let _ = agent.paste_to_pty(&text);
    } else if agent.warp_mode {
        if let Ok(mut buf) = agent.input_buffer.lock() {
            let pos = agent.warp_cursor.min(buf.len());
            buf.insert_str(pos, &text);
            agent.warp_cursor = pos + text.len();
        }
    }
}

fn read_clipboard_from_terminal(app: &App, idx: usize) -> Option<String> {
    let agent = app.terminal_agents.get(idx)?;
    if agent.should_bypass_warp_input() || agent.warp_passthrough || agent.warp_mode {
        arboard::Clipboard::new().ok()?.get_text().ok()
    } else {
        None
    }
}

const DIRECT_SYNC_WAIT_MS: u64 = 35;
const TAB_SYNC_WAIT_MS: u64 = 90;
const ETX: u8 = 0x03;
const EOT: u8 = 0x04;
const FF: u8 = 0x0c;

// ── Terminal warp-mode key handling ─────────────────────────────────

/// Handle keystrokes for a terminal agent in warp mode.
/// Keys are accumulated in the input buffer and only sent to PTY on Enter.
pub fn sync_terminal_warp_buffer_from_pty(app: &mut App, idx: usize, wait_ms: u64) {
    let agent = &app.terminal_agents[idx];
    if should_skip_warp_sync(agent) {
        return;
    }

    let Some(input) = agent.sync_warp_input_from_pty(Duration::from_millis(wait_ms)) else {
        return;
    };
    let Some(()) = replace_input_buffer(agent, &input) else {
        return;
    };

    app.terminal_agents[idx].warp_cursor = input.len();
    app.terminal_agents[idx].warp_passthrough = true;
}

pub fn handle_terminal_direct_pty_key(
    app: &mut App,
    idx: usize,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<()> {
    // CT16: while the child holds the alternate screen, canopy's reserved
    // keys stay with canopy (single source of truth: `is_reserved_focus_key`,
    // shared with the interactive path). Never forward them to the PTY.
    if app.terminal_agents[idx].in_alternate_screen() && is_reserved_focus_key(code, modifiers) {
        return Ok(());
    }
    let bytes = key_to_bytes(code, modifiers);
    let sensitive_input = {
        let agent = &app.terminal_agents[idx];
        if !bytes.is_empty() {
            let _ = agent.write_to_pty(&bytes);
        }
        agent.is_sensitive_input_active()
    };

    if sensitive_input {
        reset_warp_input(&mut app.terminal_agents[idx]);
        return Ok(());
    }

    if should_sync_direct_input(&app.terminal_agents[idx], code, modifiers, &bytes) {
        sync_terminal_warp_buffer_from_pty(app, idx, wait_ms_for_key(code));
    }

    match code {
        KeyCode::Enter => submit_direct_passthrough(app, idx),
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
            reset_warp_input(&mut app.terminal_agents[idx]);
        }
        _ => {
            app.terminal_agents[idx].history_index = None;
        }
    }

    Ok(())
}

pub fn handle_terminal_warp_key(
    app: &mut App,
    idx: usize,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<()> {
    if app.terminal_agents[idx].warp_passthrough
        || app.terminal_agents[idx].should_bypass_warp_input()
    {
        return handle_terminal_direct_pty_key(app, idx, code, modifiers);
    }

    if modifiers.contains(KeyModifiers::CONTROL) {
        handle_terminal_warp_control_key(app, idx, code);
        return Ok(());
    }

    match code {
        KeyCode::Enter => submit_warp_input(app, idx),
        KeyCode::Tab => return handle_warp_tab(app, idx),
        KeyCode::Char(c) => insert_char_at_cursor(&mut app.terminal_agents[idx], c),
        KeyCode::Backspace => delete_before_cursor(&mut app.terminal_agents[idx]),
        KeyCode::Delete => delete_at_cursor(&mut app.terminal_agents[idx]),
        KeyCode::Left => move_cursor_left(&mut app.terminal_agents[idx]),
        KeyCode::Right => handle_warp_right_key(app, idx),
        KeyCode::Home => app.terminal_agents[idx].warp_cursor = 0,
        KeyCode::End => move_cursor_to_end(&mut app.terminal_agents[idx]),
        KeyCode::Up => handle_warp_up_key(app, idx),
        KeyCode::Down => handle_warp_down_key(app, idx),
        KeyCode::PageUp => scroll_up(&mut app.terminal_agents[idx], app.tui_settings.page_step),
        KeyCode::PageDown => scroll_down(&mut app.terminal_agents[idx], app.tui_settings.page_step),
        _ => {}
    }

    Ok(())
}

fn should_skip_warp_sync(agent: &InteractiveAgent) -> bool {
    agent.in_alternate_screen() || agent.is_sensitive_input_active() || agent.warp_command_running
}

fn should_sync_direct_input(
    agent: &InteractiveAgent,
    code: KeyCode,
    modifiers: KeyModifiers,
    bytes: &[u8],
) -> bool {
    if bytes.is_empty() || is_direct_submit(code, modifiers) {
        return false;
    }

    !should_skip_warp_sync(agent)
}

fn is_direct_submit(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(code, KeyCode::Enter)
        || (code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL))
        || (code == KeyCode::Char('d') && modifiers.contains(KeyModifiers::CONTROL))
}

fn wait_ms_for_key(code: KeyCode) -> u64 {
    if code == KeyCode::Tab {
        TAB_SYNC_WAIT_MS
    } else {
        DIRECT_SYNC_WAIT_MS
    }
}

fn handle_warp_tab(app: &mut App, idx: usize) -> Result<()> {
    let input = trimmed_input_text(&app.terminal_agents[idx]);
    if is_cd_picker_request(&input) {
        return open_terminal_suggestion_picker(app, idx);
    }

    let text = input_text(&app.terminal_agents[idx]);
    let _ = app.terminal_agents[idx].write_to_pty(text.as_bytes());
    let _ = app.terminal_agents[idx].write_to_pty(b"\t");
    sync_terminal_warp_buffer_from_pty(app, idx, TAB_SYNC_WAIT_MS);
    Ok(())
}

fn handle_terminal_warp_control_key(app: &mut App, idx: usize, code: KeyCode) {
    match code {
        KeyCode::Char('f') => {
            app.terminal_search = Some(TerminalSearch::new(idx));
        }
        KeyCode::Char('c') => {
            let _ = app.terminal_agents[idx].write_to_pty(&[ETX]);
            reset_warp_input(&mut app.terminal_agents[idx]);
        }
        KeyCode::Char('d') => {
            let _ = app.terminal_agents[idx].write_to_pty(&[EOT]);
            app.terminal_agents[idx].warp_passthrough = false;
        }
        KeyCode::Char('l') => {
            let _ = app.terminal_agents[idx].write_to_pty(&[FF]);
        }
        KeyCode::Char('u') => clear_before_cursor(&mut app.terminal_agents[idx]),
        KeyCode::Char('k') => clear_after_cursor(&mut app.terminal_agents[idx]),
        KeyCode::Char('a') => app.terminal_agents[idx].warp_cursor = 0,
        KeyCode::Char('e') => move_cursor_to_end(&mut app.terminal_agents[idx]),
        KeyCode::Char('v') => paste_clipboard_to_terminal(app, idx),
        _ => {}
    }
}

fn handle_warp_up_key(app: &mut App, idx: usize) {
    let agent = &app.terminal_agents[idx];
    if agent.history_index.is_some() || input_is_blank(agent) {
        browse_history_up(app, idx);
        return;
    }

    scroll_up(&mut app.terminal_agents[idx], app.tui_settings.scroll_step);
}

fn handle_warp_down_key(app: &mut App, idx: usize) {
    if app.terminal_agents[idx].history_index.is_some() {
        browse_history_down(app, idx);
        return;
    }

    scroll_down(&mut app.terminal_agents[idx], app.tui_settings.scroll_step);
}

fn browse_history_up(app: &mut App, idx: usize) {
    let session_name = app.terminal_agents[idx].name.clone();
    let history_len = app
        .terminal_histories
        .get(&session_name)
        .map(|history| history.commands.len())
        .unwrap_or(0);
    if history_len == 0 {
        return;
    }

    let new_idx = app.terminal_agents[idx]
        .history_index
        .map_or(history_len - 1, |current| current.saturating_sub(1));
    app.terminal_agents[idx].history_index = Some(new_idx);
    load_history_entry(app, idx, &session_name, new_idx);
}

fn browse_history_down(app: &mut App, idx: usize) {
    let Some(current) = app.terminal_agents[idx].history_index else {
        return;
    };

    let session_name = app.terminal_agents[idx].name.clone();
    let history_len = app
        .terminal_histories
        .get(&session_name)
        .map(|history| history.commands.len())
        .unwrap_or(0);

    if current + 1 < history_len {
        app.terminal_agents[idx].history_index = Some(current + 1);
        load_history_entry(app, idx, &session_name, current + 1);
        return;
    }

    app.terminal_agents[idx].history_index = None;
    let _ = clear_input_buffer(&app.terminal_agents[idx]);
    app.terminal_agents[idx].warp_cursor = 0;
}

fn load_history_entry(app: &mut App, idx: usize, session_name: &str, history_idx: usize) {
    let Some(cmd) = app
        .terminal_histories
        .get(session_name)
        .and_then(|history| history.commands.get(history_idx))
        .map(|entry| entry.cmd.clone())
    else {
        return;
    };

    let Some(()) = replace_input_buffer(&app.terminal_agents[idx], &cmd) else {
        return;
    };
    app.terminal_agents[idx].warp_cursor = cmd.len();
}

fn submit_direct_passthrough(app: &mut App, idx: usize) {
    let captured = trimmed_input_text(&app.terminal_agents[idx]);
    record_terminal_command(app, idx, &captured);
    reset_warp_input(&mut app.terminal_agents[idx]);
    // Latch AFTER the reset (which must not clear it): the box stays
    // withdrawn and keys go straight to the PTY until the shell is idle.
    app.terminal_agents[idx].warp_command_running = true;
}

pub(super) fn submit_warp_input(app: &mut App, idx: usize) {
    let captured = input_text(&app.terminal_agents[idx]);
    write_line_to_pty(&app.terminal_agents[idx], &captured);
    record_terminal_command(app, idx, captured.trim());
    reset_warp_input(&mut app.terminal_agents[idx]);
    // Latch AFTER the reset (which must not clear it): the box stays
    // withdrawn and keys go straight to the PTY until the shell is idle.
    app.terminal_agents[idx].warp_command_running = true;
}

fn write_line_to_pty(agent: &InteractiveAgent, captured: &str) {
    if captured.is_empty() {
        let _ = agent.write_to_pty(b"\n");
        return;
    }

    let mut bytes = captured.as_bytes().to_vec();
    bytes.push(b'\n');
    let _ = agent.write_to_pty(&bytes);
}

fn reset_warp_input(agent: &mut InteractiveAgent) {
    let _ = clear_input_buffer(agent);
    agent.warp_cursor = 0;
    agent.history_index = None;
    agent.warp_passthrough = false;
}

fn insert_char_at_cursor(agent: &mut InteractiveAgent, c: char) {
    let cursor = agent.warp_cursor;
    let Some(new_cursor) = with_input_buffer(agent, |buf| {
        let pos = cursor.min(buf.len());
        buf.insert(pos, c);
        pos + c.len_utf8()
    }) else {
        return;
    };

    agent.warp_cursor = new_cursor;
    agent.history_index = None;
}

fn delete_before_cursor(agent: &mut InteractiveAgent) {
    let cursor = agent.warp_cursor;
    if cursor == 0 {
        return;
    }

    let Some(new_cursor) = with_input_buffer(agent, |buf| {
        let clamped = cursor.min(buf.len());
        let prev = buf[..clamped]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
        buf.remove(prev);
        prev
    }) else {
        return;
    };

    agent.warp_cursor = new_cursor;
}

fn delete_at_cursor(agent: &mut InteractiveAgent) {
    let cursor = agent.warp_cursor;
    let _ = with_input_buffer(agent, |buf| {
        let clamped = cursor.min(buf.len());
        if clamped < buf.len() {
            buf.remove(clamped);
        }
    });
}

fn move_cursor_left(agent: &mut InteractiveAgent) {
    let cursor = agent.warp_cursor;
    if cursor == 0 {
        return;
    }

    let Some(new_pos) = read_input_buffer(agent, |buf| {
        let clamped = cursor.min(buf.len());
        buf[..clamped]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }) else {
        return;
    };

    agent.warp_cursor = new_pos;
}

fn move_cursor_right(agent: &mut InteractiveAgent) {
    let cursor = agent.warp_cursor;
    let Some(new_pos) = read_input_buffer(agent, |buf| {
        let clamped = cursor.min(buf.len());
        if clamped >= buf.len() {
            return clamped;
        }

        buf[clamped..]
            .char_indices()
            .nth(1)
            .map(|(offset, _)| clamped + offset)
            .unwrap_or(buf.len())
    }) else {
        return;
    };

    agent.warp_cursor = new_pos;
}

/// Right arrow: if the cursor sits at the end of the input and there is
/// a ghost-suggestion (the first history entry starting with the current
/// input), accept the full completion in one go. Otherwise behave like a
/// normal cursor-right.
fn handle_warp_right_key(app: &mut App, idx: usize) {
    let agent = &mut app.terminal_agents[idx];
    let input_len = buffer_len(agent);
    if agent.warp_cursor == input_len {
        let current = input_text(agent);
        if let Some(ghost) = app
            .terminal_histories
            .get(&agent.name)
            .and_then(|h| h.ghost_suggestion(&current))
        {
            if ghost.len() > current.len() {
                let suffix = &ghost[current.len()..];
                let _ = with_input_buffer(agent, |buf| {
                    buf.push_str(suffix);
                });
                agent.warp_cursor = buffer_len(agent);
                agent.history_index = None;
                return;
            }
        }
    }
    move_cursor_right(agent);
}

fn move_cursor_to_end(agent: &mut InteractiveAgent) {
    agent.warp_cursor = buffer_len(agent);
}

fn clear_before_cursor(agent: &mut InteractiveAgent) {
    let cursor = agent.warp_cursor;
    let Some(()) = with_input_buffer(agent, |buf| {
        let clamped = cursor.min(buf.len());
        buf.drain(..clamped);
    }) else {
        return;
    };

    agent.warp_cursor = 0;
}

fn clear_after_cursor(agent: &mut InteractiveAgent) {
    let cursor = agent.warp_cursor;
    let _ = with_input_buffer(agent, |buf| {
        let clamped = cursor.min(buf.len());
        buf.truncate(clamped);
    });
}

fn scroll_up(agent: &mut InteractiveAgent, amount: usize) {
    let max = agent.max_scroll();
    agent.scroll_offset = (agent.scroll_offset + amount).min(max);
}

fn scroll_down(agent: &mut InteractiveAgent, amount: usize) {
    agent.scroll_offset = agent.scroll_offset.saturating_sub(amount);
}

fn read_input_buffer<T>(agent: &InteractiveAgent, f: impl FnOnce(&String) -> T) -> Option<T> {
    let Ok(buf) = agent.input_buffer.lock() else {
        return None;
    };
    Some(f(&buf))
}

fn with_input_buffer<T>(agent: &InteractiveAgent, f: impl FnOnce(&mut String) -> T) -> Option<T> {
    let Ok(mut buf) = agent.input_buffer.lock() else {
        return None;
    };
    Some(f(&mut buf))
}

fn input_text(agent: &InteractiveAgent) -> String {
    read_input_buffer(agent, |buf| buf.to_string()).unwrap_or_default()
}

fn trimmed_input_text(agent: &InteractiveAgent) -> String {
    read_input_buffer(agent, |buf| buf.trim().to_string()).unwrap_or_default()
}

fn input_is_blank(agent: &InteractiveAgent) -> bool {
    read_input_buffer(agent, |buf| buf.trim().is_empty()).unwrap_or(true)
}

fn buffer_len(agent: &InteractiveAgent) -> usize {
    read_input_buffer(agent, |buf| buf.len()).unwrap_or(0)
}

fn replace_input_buffer(agent: &InteractiveAgent, text: &str) -> Option<()> {
    with_input_buffer(agent, |buf| {
        buf.clear();
        buf.push_str(text);
    })
}

fn clear_input_buffer(agent: &InteractiveAgent) -> Option<()> {
    with_input_buffer(agent, |buf| buf.clear())
}

fn is_cd_command(input: &str) -> bool {
    input == "cd" || input.starts_with("cd ") || input.starts_with("cd\t")
}

fn is_cd_picker_request(input: &str) -> bool {
    input.is_empty() || is_cd_command(input)
}

fn handle_cd_command(app: &mut App, idx: usize, trimmed: &str) -> bool {
    if !is_cd_command(trimmed) {
        return false;
    }

    let target = if trimmed == "cd" {
        dirs::home_dir()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string())
    } else {
        trimmed[2..].trim().to_string()
    };

    let current_dir = app.terminal_agents[idx].working_dir.clone();
    let Some(abs_path) = resolve_cd_path(&current_dir, &target) else {
        return true;
    };

    app.terminal_agents[idx].update_working_dir(&abs_path.to_string_lossy());
    let _ = app.db.update_terminal_session_working_dir(
        &app.terminal_agents[idx].id,
        &abs_path.to_string_lossy(),
    );
    true
}

/// Record a terminal command to the session history.
pub fn record_terminal_command(app: &mut App, idx: usize, captured: &str) {
    if captured.is_empty() {
        return;
    }

    let trimmed = captured.trim();
    if handle_cd_command(app, idx, trimmed) {
        return;
    }

    let session_name = app.terminal_agents[idx].name.clone();
    let cwd = app.terminal_agents[idx].working_dir.clone();
    let hist = app
        .terminal_histories
        .entry(session_name.clone())
        .or_default();
    hist.record(captured, &cwd);
    crate::tui::terminal_history::save_history(&app.data_dir, &session_name, hist);
    crate::tui::terminal_history::record_global_catalog(&app.data_dir, captured, &cwd);
}

/// Open the suggestion picker for a terminal agent.
pub fn open_terminal_suggestion_picker(app: &mut App, idx: usize) -> Result<()> {
    let (blocked, input, cwd) = {
        let agent = &app.terminal_agents[idx];
        (
            should_skip_warp_sync(agent),
            input_text(agent),
            agent.working_dir.clone(),
        )
    };
    if blocked {
        return Ok(());
    }

    if is_cd_command(input.trim()) {
        let partial = if input.len() > 2 {
            input[3..].trim()
        } else {
            ""
        };
        let global = crate::tui::terminal_history::load_all_histories(&app.data_dir);
        app.suggestion_picker = Some(crate::tui::terminal_history::SuggestionPicker::for_cd(
            partial, &cwd, &global,
        ));
        return Ok(());
    }

    app.suggestion_picker = Some(crate::tui::terminal_history::from_global_catalog(
        &input,
        &app.data_dir,
        &cwd,
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn is_direct_submit_enter() {
        assert!(is_direct_submit(KeyCode::Enter, KeyModifiers::NONE));
    }

    #[test]
    fn is_direct_submit_ctrl_c() {
        assert!(is_direct_submit(KeyCode::Char('c'), KeyModifiers::CONTROL));
    }

    #[test]
    fn is_direct_submit_ctrl_d() {
        assert!(is_direct_submit(KeyCode::Char('d'), KeyModifiers::CONTROL));
    }

    #[test]
    fn is_direct_submit_regular_char() {
        assert!(!is_direct_submit(KeyCode::Char('a'), KeyModifiers::NONE));
    }

    #[test]
    fn is_direct_submit_ctrl_a() {
        assert!(!is_direct_submit(KeyCode::Char('a'), KeyModifiers::CONTROL));
    }

    #[test]
    fn is_direct_submit_esc() {
        assert!(!is_direct_submit(KeyCode::Esc, KeyModifiers::NONE));
    }

    #[test]
    fn wait_ms_for_key_tab() {
        assert_eq!(wait_ms_for_key(KeyCode::Tab), TAB_SYNC_WAIT_MS);
    }

    #[test]
    fn wait_ms_for_key_enter() {
        assert_eq!(wait_ms_for_key(KeyCode::Enter), DIRECT_SYNC_WAIT_MS);
    }

    #[test]
    fn wait_ms_for_key_char() {
        assert_eq!(wait_ms_for_key(KeyCode::Char('a')), DIRECT_SYNC_WAIT_MS);
    }

    #[test]
    fn wait_ms_for_key_esc() {
        assert_eq!(wait_ms_for_key(KeyCode::Esc), DIRECT_SYNC_WAIT_MS);
    }

    #[test]
    fn is_cd_command_exact() {
        assert!(is_cd_command("cd"));
    }

    #[test]
    fn is_cd_command_with_space() {
        assert!(is_cd_command("cd /tmp"));
    }

    #[test]
    fn is_cd_command_with_tab() {
        assert!(is_cd_command("cd\t"));
    }

    #[test]
    fn is_cd_command_not_cd() {
        assert!(!is_cd_command("ls"));
    }

    #[test]
    fn is_cd_command_not_cd_prefix() {
        assert!(!is_cd_command("cd2"));
    }

    #[test]
    fn is_cd_command_empty() {
        assert!(!is_cd_command(""));
    }

    #[test]
    fn is_cd_picker_request_empty() {
        assert!(is_cd_picker_request(""));
    }

    #[test]
    fn is_cd_picker_request_cd() {
        assert!(is_cd_picker_request("cd"));
    }

    #[test]
    fn is_cd_picker_request_cd_with_path() {
        assert!(is_cd_picker_request("cd /tmp"));
    }

    #[test]
    fn is_cd_picker_request_not_cd() {
        assert!(!is_cd_picker_request("ls"));
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn constants_are_sensible() {
        assert!(DIRECT_SYNC_WAIT_MS > 0);
        assert!(TAB_SYNC_WAIT_MS > DIRECT_SYNC_WAIT_MS);
        let scroll = crate::domain::settings::default_int("tui.scroll_step");
        let page = crate::domain::settings::default_int("tui.page_step");
        assert!(scroll > 0);
        assert!(page > scroll);
    }
}

#[cfg(test)]
mod buffer_helper_tests {
    use super::*;
    use crate::tui::agent::InteractiveAgent;

    fn make_agent() -> InteractiveAgent {
        InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("test-buffer"),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn")
    }

    #[test]
    fn input_text_empty_initially() {
        let agent = make_agent();
        assert_eq!(input_text(&agent), "");
    }

    #[test]
    fn trimmed_input_text_empty_initially() {
        let agent = make_agent();
        assert_eq!(trimmed_input_text(&agent), "");
    }

    #[test]
    fn input_is_blank_initially() {
        let agent = make_agent();
        assert!(input_is_blank(&agent));
    }

    #[test]
    fn buffer_len_zero_initially() {
        let agent = make_agent();
        assert_eq!(buffer_len(&agent), 0);
    }

    #[test]
    fn replace_input_buffer_sets_content() {
        let agent = make_agent();
        replace_input_buffer(&agent, "hello world");
        assert_eq!(input_text(&agent), "hello world");
        assert_eq!(buffer_len(&agent), 11);
        assert!(!input_is_blank(&agent));
    }

    #[test]
    fn clear_input_buffer_empties_content() {
        let agent = make_agent();
        replace_input_buffer(&agent, "hello");
        clear_input_buffer(&agent);
        assert_eq!(input_text(&agent), "");
        assert!(input_is_blank(&agent));
    }

    #[test]
    fn trimmed_input_text_trims_whitespace() {
        let agent = make_agent();
        replace_input_buffer(&agent, "  hello  ");
        assert_eq!(trimmed_input_text(&agent), "hello");
    }

    #[test]
    fn trimmed_input_text_blank_after_trim() {
        let agent = make_agent();
        replace_input_buffer(&agent, "   ");
        assert!(input_is_blank(&agent));
        assert_eq!(trimmed_input_text(&agent), "");
    }

    #[test]
    fn input_text_with_newlines() {
        let agent = make_agent();
        replace_input_buffer(&agent, "line1\nline2\n");
        assert_eq!(input_text(&agent), "line1\nline2\n");
        assert!(!input_is_blank(&agent));
    }
}

#[cfg(test)]
mod cd_picker_edge_cases {
    use super::*;

    #[test]
    fn is_cd_command_with_multiple_spaces() {
        assert!(is_cd_command("cd   /tmp"));
    }

    #[test]
    fn is_cd_command_cd_only() {
        assert!(is_cd_command("cd"));
    }

    #[test]
    fn is_cd_picker_request_matches_cd_command() {
        assert!(is_cd_picker_request("cd /some/path"));
    }

    #[test]
    fn is_cd_picker_request_matches_empty() {
        assert!(is_cd_picker_request(""));
    }

    #[test]
    fn is_cd_picker_request_rejects_non_cd() {
        assert!(!is_cd_picker_request("ls -la"));
        assert!(!is_cd_picker_request("pwd"));
        assert!(!is_cd_picker_request("echo hello"));
    }
}

#[cfg(test)]
mod is_direct_submit_edge_cases {
    use super::*;

    #[test]
    fn ctrl_d_is_direct_submit() {
        assert!(is_direct_submit(KeyCode::Char('d'), KeyModifiers::CONTROL));
    }

    #[test]
    fn enter_is_direct_submit() {
        assert!(is_direct_submit(KeyCode::Enter, KeyModifiers::NONE));
    }

    #[test]
    fn ctrl_c_is_direct_submit() {
        assert!(is_direct_submit(KeyCode::Char('c'), KeyModifiers::CONTROL));
    }

    #[test]
    fn regular_char_is_not() {
        assert!(!is_direct_submit(KeyCode::Char('x'), KeyModifiers::NONE));
    }

    #[test]
    fn ctrl_a_is_not() {
        assert!(!is_direct_submit(KeyCode::Char('a'), KeyModifiers::CONTROL));
    }

    #[test]
    fn shift_enter_is_not() {
        assert!(is_direct_submit(KeyCode::Enter, KeyModifiers::SHIFT));
    }
}

#[cfg(test)]
mod should_skip_warp_sync_tests {
    use super::*;
    use crate::tui::agent::InteractiveAgent;

    fn make_agent() -> InteractiveAgent {
        InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("test-skip"),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn")
    }

    #[test]
    fn agent_not_in_alternate_screen_does_not_skip() {
        let agent = make_agent();
        assert!(!should_skip_warp_sync(&agent));
    }
}

#[cfg(test)]
mod alternate_screen_key_routing_tests {
    use super::*;
    use crate::db::Database;
    use crate::tui::app::types::AgentEntry;
    use crate::tui::app::types::{App, Focus};
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

    #[test]
    fn alternate_screen_forwards_content_keys_but_keeps_reserved_keys_in_canopy() {
        let mut agent = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("routing"),
            &[],
            Color::White,
        )
        .expect("spawn terminal");
        agent.vt.lock().expect("lock vt").process(b"\x1b[?1049h");
        let written = Arc::new(Mutex::new(Vec::new()));
        agent.test_sink = Some(Arc::clone(&written));
        let mut app = test_app(agent);

        handle_terminal_direct_pty_key(&mut app, 0, KeyCode::F(10), KeyModifiers::NONE)
            .expect("reserved key handling");
        assert!(written.lock().expect("lock written bytes").is_empty());

        handle_terminal_direct_pty_key(&mut app, 0, KeyCode::Char('a'), KeyModifiers::NONE)
            .expect("content key handling");
        assert_eq!(&*written.lock().expect("lock written bytes"), b"a");
        app.terminal_agents[0].kill();
    }

    /// CM52: the terminal scroll step and page step come from
    /// `tui.scroll_step` / `tui.page_step`.
    #[test]
    fn scroll_and_page_steps_read_the_settings() {
        let agent = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("scroll"),
            &[],
            Color::White,
        )
        .expect("spawn terminal");
        let mut app = test_app(agent);
        app.tui_settings.scroll_step = 1;
        app.tui_settings.page_step = 5;
        app.terminal_agents[0].scroll_offset = 100;

        handle_terminal_warp_key(&mut app, 0, KeyCode::Down, KeyModifiers::NONE)
            .expect("scroll down");
        assert_eq!(app.terminal_agents[0].scroll_offset, 99);

        handle_terminal_warp_key(&mut app, 0, KeyCode::PageDown, KeyModifiers::NONE)
            .expect("page down");
        assert_eq!(app.terminal_agents[0].scroll_offset, 94);

        app.terminal_agents[0].kill();
    }
}

// Latch: from warp submit until the shell is back at an idle prompt the
// box stays withdrawn and keys go straight to the PTY — including for
// non-fullscreen children (wizards, y/n questions, long commands).
#[cfg(test)]
mod warp_command_latch_tests {
    use super::super::paste::handle_paste;
    use super::*;
    use crate::db::Database;
    use crate::tui::agent::AgentStatus;
    use crate::tui::app::types::AgentEntry;
    use crate::tui::app::types::{App, Focus};
    use ratatui::style::Color;
    use std::sync::{Arc, Mutex};
    use tempfile::{tempdir, NamedTempFile};

    fn spawn_term(name: &str) -> InteractiveAgent {
        InteractiveAgent::spawn_terminal("cat", "/tmp", 80, 24, Some(name), &[], Color::White)
            .expect("spawn terminal")
    }

    fn feed(agent: &InteractiveAgent, bytes: &[u8]) {
        agent.vt.lock().expect("lock vt").process(bytes);
    }

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

    fn type_command(app: &mut App, cmd: &str) {
        replace_input_buffer(&app.terminal_agents[0], cmd);
        app.terminal_agents[0].warp_cursor = cmd.len();
    }

    fn submit(app: &mut App) {
        handle_terminal_warp_key(app, 0, KeyCode::Enter, KeyModifiers::NONE).expect("submit");
    }

    #[test]
    fn idle_prompt_markers_count_as_idle() {
        let agent = spawn_term("idle-markers");
        feed(&agent, b"$ ");
        assert!(agent.shell_at_idle_prompt());
        feed(&agent, b"\r\nuser@host:~/repo$ ");
        assert!(agent.shell_at_idle_prompt());
        feed(&agent, b"\r\nuser@host:~/repo$");
        assert!(agent.shell_at_idle_prompt());
    }

    #[test]
    fn wizard_and_busy_lines_are_not_idle() {
        let agent = spawn_term("busy-lines");
        feed(&agent, b"? Pick a framework: ");
        assert!(!agent.shell_at_idle_prompt());
        feed(&agent, b"\r\n$ sleep 9");
        assert!(!agent.shell_at_idle_prompt());
        feed(&agent, b"\r\nplain output");
        assert!(!agent.shell_at_idle_prompt());
    }

    #[test]
    fn alternate_screen_is_never_idle() {
        let agent = spawn_term("alt-idle");
        feed(&agent, b"$ ");
        assert!(agent.shell_at_idle_prompt());
        feed(&agent, b"\x1b[?1049h");
        assert!(!agent.shell_at_idle_prompt());
    }

    #[test]
    fn refresh_clears_the_latch_only_at_an_idle_prompt() {
        let mut agent = spawn_term("refresh-idle");
        agent.warp_command_running = true;
        feed(&agent, b"$ sleep 9");
        agent.refresh_warp_command_state();
        assert!(agent.warp_command_running, "busy line keeps the latch");
        feed(&agent, b"\r\n? Pick one: ");
        agent.refresh_warp_command_state();
        assert!(agent.warp_command_running, "wizard prompt keeps the latch");
        feed(&agent, b"\r\nuser@host:~$ ");
        agent.refresh_warp_command_state();
        assert!(!agent.warp_command_running, "idle prompt clears the latch");
    }

    #[test]
    fn refresh_clears_the_latch_once_the_shell_has_exited() {
        let mut agent = spawn_term("refresh-exit");
        agent.warp_command_running = true;
        feed(&agent, b"$ sleep 9");
        agent.status = AgentStatus::Exited(0);
        agent.refresh_warp_command_state();
        assert!(
            !agent.warp_command_running,
            "a dead shell holds no running command"
        );
        agent.kill();
    }

    #[test]
    fn refresh_is_a_noop_without_a_latch_or_terminal() {
        let mut agent = spawn_term("refresh-noop");
        feed(&agent, b"? Pick one: ");
        agent.refresh_warp_command_state();
        assert!(!agent.warp_command_running);
    }

    #[test]
    fn submit_withdraws_the_box_and_routes_keys_to_the_pty() {
        let mut agent = spawn_term("submit-latch");
        let written = Arc::new(Mutex::new(Vec::new()));
        agent.test_sink = Some(Arc::clone(&written));
        let mut app = test_app(agent);
        assert!(warp_active(&app));

        type_command(&mut app, "sleep 9");
        submit(&mut app);

        assert!(app.terminal_agents[0].warp_command_running);
        assert!(app.terminal_agents[0].should_bypass_warp_input());
        assert!(!warp_active(&app), "box withdraws on submit");
        assert_eq!(buffer_text(&app), "");

        written.lock().expect("lock sink").clear();
        handle_terminal_warp_key(&mut app, 0, KeyCode::Char('y'), KeyModifiers::NONE)
            .expect("mid-command key");
        assert_eq!(&*written.lock().expect("lock sink"), b"y");
        assert_eq!(buffer_text(&app), "", "mid-command keys skip the box");
        app.terminal_agents[0].kill();
    }

    #[test]
    fn passthrough_submit_sets_the_latch_too() {
        let mut agent = spawn_term("passthrough-submit");
        let written = Arc::new(Mutex::new(Vec::new()));
        agent.test_sink = Some(Arc::clone(&written));
        let mut app = test_app(agent);
        app.terminal_agents[0].warp_passthrough = true;
        type_command(&mut app, "ls");

        submit(&mut app);

        assert!(app.terminal_agents[0].warp_command_running);
        assert!(!warp_active(&app));
        app.terminal_agents[0].kill();
    }

    #[test]
    fn box_returns_empty_at_the_next_idle_prompt() {
        let mut agent = spawn_term("box-returns");
        let written = Arc::new(Mutex::new(Vec::new()));
        agent.test_sink = Some(Arc::clone(&written));
        let mut app = test_app(agent);
        type_command(&mut app, "sleep 9");
        submit(&mut app);
        handle_terminal_warp_key(&mut app, 0, KeyCode::Char('y'), KeyModifiers::NONE)
            .expect("mid-command key");
        assert_eq!(buffer_text(&app), "");

        feed(&app.terminal_agents[0], b"\r\nuser@host:~$ ");
        app.terminal_agents[0].poll();

        assert!(!app.terminal_agents[0].warp_command_running);
        assert_eq!(buffer_text(&app), "", "box returns with an empty buffer");
        assert!(warp_active(&app), "box is back at the idle prompt");

        written.lock().expect("lock sink").clear();
        handle_terminal_warp_key(&mut app, 0, KeyCode::Char('l'), KeyModifiers::NONE)
            .expect("idle key");
        assert_eq!(buffer_text(&app), "l");
        assert!(written.lock().expect("lock sink").is_empty());
        app.terminal_agents[0].kill();
    }

    #[test]
    fn wizard_prompt_needs_no_toggle_for_keys_to_reach_it() {
        let mut agent = spawn_term("wizard-keys");
        let written = Arc::new(Mutex::new(Vec::new()));
        agent.test_sink = Some(Arc::clone(&written));
        let mut app = test_app(agent);
        type_command(&mut app, "gitkit clone");
        submit(&mut app);

        // An inquire-style wizard prompt renders; the user pressed nothing.
        feed(&app.terminal_agents[0], b"\r\n? Pick a framework: ");
        app.terminal_agents[0].refresh_warp_command_state();
        assert!(app.terminal_agents[0].warp_command_running);
        assert!(!warp_active(&app));
        assert!(app.terminal_agents[0].warp_mode, "no toggle was pressed");

        written.lock().expect("lock sink").clear();
        handle_terminal_warp_key(&mut app, 0, KeyCode::Char('y'), KeyModifiers::NONE)
            .expect("wizard answer");
        assert_eq!(&*written.lock().expect("lock sink"), b"y");
        assert_eq!(buffer_text(&app), "");
        app.terminal_agents[0].kill();
    }

    #[test]
    fn paste_goes_to_the_pty_while_a_command_runs() {
        let mut agent = spawn_term("paste-latch");
        let written = Arc::new(Mutex::new(Vec::new()));
        agent.test_sink = Some(Arc::clone(&written));
        let mut app = test_app(agent);
        type_command(&mut app, "sleep 9");
        submit(&mut app);
        feed(&app.terminal_agents[0], b"\r\n? yn: ");
        written.lock().expect("lock sink").clear();

        handle_paste(&mut app, "hello");

        assert_eq!(&*written.lock().expect("lock sink"), b"hello");
        assert_eq!(buffer_text(&app), "");
        app.terminal_agents[0].kill();
    }

    #[test]
    fn paste_wraps_markers_only_when_the_child_enabled_them() {
        let mut agent = spawn_term("paste-2004");
        let written = Arc::new(Mutex::new(Vec::new()));
        agent.test_sink = Some(Arc::clone(&written));
        let mut app = test_app(agent);
        type_command(&mut app, "sleep 9");
        submit(&mut app);
        feed(&app.terminal_agents[0], b"\x1b[?2004h");
        assert!(app.terminal_agents[0].bracketed_paste_enabled());
        written.lock().expect("lock sink").clear();

        handle_paste(&mut app, "hi");

        assert_eq!(
            &*written.lock().expect("lock sink"),
            b"\x1b[200~hi\x1b[201~"
        );
        app.terminal_agents[0].kill();
    }

    #[test]
    fn paste_lands_in_the_box_at_an_idle_prompt() {
        let mut agent = spawn_term("paste-idle");
        let written = Arc::new(Mutex::new(Vec::new()));
        agent.test_sink = Some(Arc::clone(&written));
        let mut app = test_app(agent);
        feed(&app.terminal_agents[0], b"$ ");

        handle_paste(&mut app, "ls -la");

        assert_eq!(buffer_text(&app), "ls -la");
        assert!(written.lock().expect("lock sink").is_empty());
        app.terminal_agents[0].kill();
    }

    #[test]
    fn paste_at_idle_prompt_clears_a_stale_latch_first() {
        let mut agent = spawn_term("paste-stale");
        let written = Arc::new(Mutex::new(Vec::new()));
        agent.test_sink = Some(Arc::clone(&written));
        let mut app = test_app(agent);
        // Latch staged as a submit would; the shell is already back.
        app.terminal_agents[0].warp_command_running = true;
        feed(&app.terminal_agents[0], b"$ ");

        handle_paste(&mut app, "ls");

        assert!(!app.terminal_agents[0].warp_command_running);
        assert_eq!(buffer_text(&app), "ls");
        assert!(written.lock().expect("lock sink").is_empty());
        app.terminal_agents[0].kill();
    }
}

// ── Dialog: new agent creation ──────────────────────────────────────
