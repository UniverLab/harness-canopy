//! Wheel scrolling: sidebar/panel paging, focused-agent scrollback and log
//! scroll, plus the terminal-like agent accessors they share.
//! Split out of `event/mod.rs` to keep every production file within
//! the file-size gate.

use crate::tui::agent::InteractiveAgent;
use crate::tui::app::types::{AgentEntry, App, Focus};

use super::context_transfer::resolve_split_focused_terminal_like;

pub(super) fn scroll_sidebar(app: &mut App, dir: i32) {
    let total = app.sidebar_click_map.len();
    let max_visible = app.sidebar_visible_capacity.max(1);
    app.sidebar_scroll_offset =
        clamp_sidebar_scroll(app.sidebar_scroll_offset, total, max_visible, dir);
}

/// Pure clamped increment/decrement for the sidebar's manual scroll offset:
/// scrolling down moves further into the list (up to the last page),
/// scrolling up retreats back toward the top.
pub(super) fn clamp_sidebar_scroll(
    offset: usize,
    total_items: usize,
    max_visible: usize,
    dir: i32,
) -> usize {
    let max_offset = total_items.saturating_sub(max_visible);
    if dir < 0 {
        (offset + 1).min(max_offset)
    } else {
        offset.saturating_sub(1)
    }
}

fn scroll_speed(app: &App) -> usize {
    let elapsed_ms = app.last_scroll_at.elapsed().as_millis();
    match elapsed_ms {
        0..=60 => 8,
        61..=120 => 4,
        121..=200 => 2,
        _ => 1,
    }
}

pub(super) fn handle_scroll(app: &mut App, dir: i32) {
    match app.focus {
        Focus::Agent | Focus::Preview => {
            let speed = scroll_speed(app);
            app.last_scroll_at = std::time::Instant::now();
            scroll_focused_agent(app, dir * speed as i32);
        }
        Focus::Home => {
            if dir > 0 {
                app.select_prev();
            } else {
                app.select_next();
            }
        }
        Focus::NewAgentDialog => {
            if let Some(dialog) = &mut app.new_agent_dialog {
                let len = dialog.filtered_dir_entries().len();
                if len != 0 {
                    let forward = dir < 0;
                    dialog.dir_selected =
                        crate::tui::selection::move_index(dialog.dir_selected, len, forward);
                    dialog.update_dir_preview();
                }
            }
        }
        Focus::LaunchpadDialog
        | Focus::KnowledgeDialog
        | Focus::ContextTransfer
        | Focus::RagTransfer
        | Focus::PromptTemplateDialog
        | Focus::GraphEditorDialog
        | Focus::GraphFormDialog => {}
        Focus::ProjectRelationDialog => {}
    }
}

fn scroll_focused_agent(app: &mut App, dir: i32) {
    let step = dir.unsigned_abs() as usize;

    if let Some((is_terminal, idx)) = selected_terminal_like(app) {
        let _ = with_terminal_like_agent_mut(app, is_terminal, idx, |agent| {
            scroll_terminal_like_agent(agent, dir, step);
        });
        return;
    }

    scroll_log(app, dir, step);
}

pub(super) fn scroll_terminal_like_agent(agent: &mut InteractiveAgent, dir: i32, step: usize) {
    if agent.in_alternate_screen() {
        let _ = agent.forward_scroll(dir > 0);
        return;
    }

    if dir > 0 {
        let max = agent.max_scroll();
        agent.scroll_offset = (agent.scroll_offset + step).min(max);
        return;
    }

    agent.scroll_offset = agent.scroll_offset.saturating_sub(step);
}

fn scroll_log(app: &mut App, dir: i32, step: usize) {
    for _ in 0..step {
        if dir > 0 {
            app.scroll_log_up();
        } else {
            app.scroll_log_down();
        }
    }
}

pub(super) fn selected_terminal_like(app: &App) -> Option<(bool, usize)> {
    match app.selected_agent()? {
        AgentEntry::Interactive(idx) => Some((false, *idx)),
        AgentEntry::Terminal(idx) => Some((true, *idx)),
        _ => None,
    }
}

/// Resolve the terminal-like agent that currently owns PTY input: the
/// focused half of an active split, or the sidebar-selected agent otherwise.
pub(super) fn focused_terminal_like(app: &App) -> Option<(bool, usize)> {
    if app.active_split_id.is_some() {
        return resolve_split_focused_terminal_like(app);
    }
    selected_terminal_like(app)
}

pub(super) fn with_selected_terminal_like<R>(
    app: &App,
    f: impl FnOnce(&InteractiveAgent) -> R,
) -> Option<R> {
    let (is_terminal, idx) = focused_terminal_like(app)?;
    with_terminal_like_agent(app, is_terminal, idx, f)
}

pub(super) fn with_selected_terminal_like_mut<R>(
    app: &mut App,
    f: impl FnOnce(&mut InteractiveAgent) -> R,
) -> Option<R> {
    let (is_terminal, idx) = focused_terminal_like(app)?;
    with_terminal_like_agent_mut(app, is_terminal, idx, f)
}

pub(super) fn with_terminal_like_agent<R>(
    app: &App,
    is_terminal: bool,
    idx: usize,
    f: impl FnOnce(&InteractiveAgent) -> R,
) -> Option<R> {
    if is_terminal {
        return app.terminal_agents.get(idx).map(f);
    }

    app.interactive_agents.get(idx).map(f)
}

pub(super) fn with_terminal_like_agent_mut<R>(
    app: &mut App,
    is_terminal: bool,
    idx: usize,
    f: impl FnOnce(&mut InteractiveAgent) -> R,
) -> Option<R> {
    if is_terminal {
        return app.terminal_agents.get_mut(idx).map(f);
    }

    app.interactive_agents.get_mut(idx).map(f)
}
