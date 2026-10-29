//! Event graph — polls crossterm events with a tick for data refresh.
//!
//! Navigation flow:
//!   Home (screensaver) → Preview (agent details) → Focus (log / PTY)
//!
//! Keys:
//!   Home:    ↑↓ → Preview, q quit, Esc confirm-quit, n new agent
//!   Preview: ↑↓ navigate, Enter → Focus, Esc → Home, agent actions
//!   Focus:   background → scroll log, interactive → PTY, `EscEsc` → Preview

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::time::Duration;

use crate::tui::app::types::{AgentEntry, App, Focus, ProjectTab, SidebarLayer};
use crate::tui::app::TerminalSearch;
use crate::tui::ui;

use agent_focus::handle_agent_key;
use context_transfer::handle_context_transfer_key;
use graph_editor::handle_graph_editor_key;
use graph_form::handle_graph_form_key;
use home_preview::{handle_home_key, handle_preview_key};
use launchpad::handle_launchpad_key;
use new_agent_dialog::handle_dialog_key;
use paste::handle_paste;
use prompt_template::handle_prompt_template_key;
use rag_transfer::handle_rag_transfer_key;

type Terminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

/// Main event loop: poll events → refresh → draw only when dirty.
///
/// A tick redraws only when something visible changed (FR1): input, PTY
/// output, a slow-refresh data change, or an animation frame on screen —
/// see [`redraw::tick`]. The tick cadence is unchanged, so timers and PTY
/// latency behave exactly as before.
pub fn run_event_graph(terminal: &mut Terminal, app: &mut App) -> Result<()> {
    let mut tick_state = redraw::TickState::default();
    let mut first = true;
    while app.running {
        let input = if event::poll(tick_duration(app))? {
            drain_pending_events(app)?;
            true
        } else {
            false
        };

        #[cfg(unix)]
        if crate::tui::agent::pty::SIGHUP_RECEIVED.load(std::sync::atomic::Ordering::Relaxed) {
            app.running = false;
        }

        let (dirty, slow_ran) = redraw::tick(app, &mut tick_state, input || first)?;
        first = false;
        redraw::perf_tick(redraw::PerfKind {
            drawn: dirty,
            slow: slow_ran,
        });
        if dirty {
            terminal.draw(|frame| ui::draw(frame, app))?;
        }
    }

    app.cleanup();
    Ok(())
}

fn tick_duration(app: &App) -> Duration {
    match app.focus {
        Focus::Agent
        | Focus::NewAgentDialog
        | Focus::LaunchpadDialog
        | Focus::KnowledgeDialog
        | Focus::KnowledgeViewer
        | Focus::ContextTransfer
        | Focus::RagTransfer
        | Focus::PromptTemplateDialog
        | Focus::GraphEditorDialog
        | Focus::GraphFormDialog => Duration::from_millis(50),
        Focus::ProjectRelationDialog => Duration::from_millis(50),
        Focus::Preview => Duration::from_millis(100),
        Focus::Home if app.home_brain.is_some() => Duration::from_millis(50),
        Focus::Home => Duration::from_millis(200),
    }
}

fn drain_pending_events(app: &mut App) -> Result<()> {
    loop {
        dispatch_event(app, event::read()?)?;
        if !event::poll(Duration::from_millis(0))? {
            return Ok(());
        }
    }
}

fn dispatch_event(app: &mut App, event: Event) -> Result<()> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            handle_key(app, key.code, key.modifiers)
        }
        Event::Mouse(mouse) => {
            app.notify_atmosphere_mouse(mouse.column, mouse.row);
            // Suppress particles while any mouse button is held
            match mouse.kind {
                ratatui::crossterm::event::MouseEventKind::Down(
                    ratatui::crossterm::event::MouseButton::Left,
                ) => {
                    app.atmosphere_left_click(mouse.column, mouse.row);
                    app.atmosphere_hidden = true;
                }
                ratatui::crossterm::event::MouseEventKind::Down(_) => {
                    app.atmosphere_hidden = true;
                }
                ratatui::crossterm::event::MouseEventKind::Up(_) => {
                    app.atmosphere_hidden = false;
                }
                _ => {}
            }
            // Only genuine motion seeds the brain: clicks and scroll do not.
            if matches!(
                mouse.kind,
                ratatui::crossterm::event::MouseEventKind::Moved
                    | ratatui::crossterm::event::MouseEventKind::Drag(_)
            ) {
                app.notify_mouse_move(mouse.column, mouse.row);
            }
            handle_mouse(app, mouse)
        }
        Event::Paste(text) => {
            handle_paste(app, &text);
            Ok(())
        }
        Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Key(_) => Ok(()),
    }
}

// ── Prompt Template Dialog ──────────────────────────────────────

mod agent_focus;
pub(crate) use agent_focus::focused_child_claimed_keyboard;
mod context_transfer;
pub(crate) mod ctrl_c;
mod home_preview;
pub(crate) mod knowledge_dialog;
pub(crate) mod knowledge_viewer;
mod launchpad;
mod mouse;
mod new_agent_dialog;
pub(crate) mod new_agent_mode_step;
mod paste;
mod project_filter;
mod prompt_template;
mod rag_transfer;
mod redraw;
mod scroll;
mod search_picker;
mod terminal_warp;

use knowledge_dialog::handle_knowledge_dialog_key;
use knowledge_viewer::handle_knowledge_viewer_key;
mod crews_keys;
mod graph_editor;
mod graph_form;
mod hub;

use mouse::handle_mouse;
use scroll::{selected_terminal_like, with_terminal_like_agent, with_terminal_like_agent_mut};

#[cfg(test)]
use crate::tui::agent::InteractiveAgent;
#[cfg(test)]
use mouse::{
    clamped_pty_position, handle_copy_click, handle_graph_live_panel_mouse,
    handle_preview_focus_click, handle_project_panel_mouse, handle_selection_mouse,
    handle_sidebar_mouse, handle_sync_panel_scroll, mouse_pty_position, rect_contains_point,
    scroll_direction, sidebar_agent_at, sidebar_tab_at,
};
#[cfg(test)]
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
#[cfg(test)]
use scroll::{
    clamp_sidebar_scroll, focused_terminal_like, scroll_terminal_like_agent,
    with_selected_terminal_like,
};

pub fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    // CT30: while the panel is focused on the Crews face (or one of its
    // dialogs is open) the key belongs to the face first; it reports
    // whether it consumed the key so global keys still work otherwise.
    if crews_keys::handle_crews_face_key(app, code, modifiers) {
        return Ok(());
    }
    if hub::handle_hub_key(app, code, modifiers) || handle_global_key(app, code, modifiers) {
        return Ok(());
    }

    if app.terminal_search.is_some() {
        return handle_terminal_search_key(app, code);
    }

    dispatch_focus_key(app, code, modifiers)
}

fn handle_global_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if code == KeyCode::Char('n') && modifiers.contains(KeyModifiers::CONTROL) {
        app.open_new_agent_dialog();
        return true;
    }

    if code == KeyCode::Char('b')
        && modifiers.contains(KeyModifiers::CONTROL)
        && matches!(app.focus, Focus::Agent)
        && !is_terminal_agent_selected(app)
    {
        app.open_simple_prompt_dialog(None);
        return true;
    }

    if code == KeyCode::F(2) {
        cycle_sidebar_layer_and_normalize_focus(app);
        return true;
    }

    // Shift+←/→ walks the sidebar tab strip — one convention for both tab
    // strips in canopy (see `agent_focus::handle_project_focus_key` for the
    // project-tab side), reachable from Home, Preview, and now Focus::Agent
    // too. It used to be scoped to Home/Preview only because Focus::Agent
    // already spends Shift+←/→ on split-pane focus (see
    // `agent_focus::handle_split_panel_focus_shortcut`); the two can't both
    // hold the key at once. Resolved by context: while a split is active,
    // Shift+←/→ still means split focus — that binding is established and
    // arguably more delicate to lose since it's how you jump to the *other*
    // pane. Wherever a split isn't active, Shift+←/→ steps the sidebar tab
    // instead — matching Home/Preview and, per functional requirement 1,
    // making the strip reachable without backing out of focus first. Plain
    // ←/→ is untouched either way (graph collapse/expand on Home, forwarded
    // to the PTY / cursor movement everywhere else), and project focus /
    // playground keep opting out entirely per the existing guard below.
    if matches!(code, KeyCode::Left | KeyCode::Right)
        && modifiers.contains(KeyModifiers::SHIFT)
        && sidebar_tab_step_applies(app)
        && !app.playground_active
        && app.project_focus.is_none()
    {
        let forward = code == KeyCode::Right;
        app.step_sidebar_tab(forward);
        if app.focus == Focus::Agent && app.sidebar_layer == SidebarLayer::Knowledge {
            app.enter_project_focus(ProjectTab::Overview);
        }
        return true;
    }

    if code == KeyCode::F(3) {
        app.toggle_activity_panel();
        return true;
    }

    if code == KeyCode::Char('f')
        && modifiers.contains(KeyModifiers::CONTROL)
        && matches!(app.focus, Focus::Agent)
    {
        open_terminal_search(app);
        return true;
    }

    false
}

/// Whether Shift+←/→ should step the sidebar tab strip in the app's current
/// state: always from Home/Preview, and from Focus::Agent only when no
/// split is active (a split keeps Shift+←/→ for split-pane focus instead —
/// see the comment above this function's call site).
fn sidebar_tab_step_applies(app: &App) -> bool {
    match app.focus {
        Focus::Home | Focus::Preview => true,
        Focus::Agent => app.active_split_id.is_none(),
        _ => false,
    }
}

/// Shared by the F2 key and a sidebar right-click.
fn cycle_sidebar_layer_and_normalize_focus(app: &mut App) {
    app.cycle_sidebar_layer();
    // CT14: defensive (idempotent with the `cycle/step/switch` clears in
    // `App` itself) — no dangling `project_focus` may survive a layer change.
    if app.project_focus.is_some() && app.sidebar_layer != SidebarLayer::Knowledge {
        app.exit_project_focus();
    }
    if matches!(app.focus, Focus::Agent) {
        app.focus = Focus::Preview;
    }
}

/// A left-click on a sidebar tab label: switch to it and, like the F2/
/// right-click cycle, back out of a deep `Focus::Agent` view to Preview —
/// the newly active tab's own selection (if any) is what should be shown,
/// not whatever agent happened to be focused on the previous tab.
fn switch_sidebar_tab_and_normalize_focus(app: &mut App, layer: SidebarLayer) {
    app.switch_sidebar_tab(layer);
    // CT14: defensive (idempotent) — see `cycle_sidebar_layer_and_normalize_focus`.
    if app.project_focus.is_some() && app.sidebar_layer != SidebarLayer::Knowledge {
        app.exit_project_focus();
    }
    if matches!(app.focus, Focus::Agent) {
        app.focus = Focus::Preview;
    }
}

fn dispatch_focus_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    match app.focus {
        Focus::Home => handle_home_key(app, code, modifiers),
        Focus::Preview => handle_preview_key(app, code, modifiers),
        Focus::NewAgentDialog => handle_dialog_key(app, code, modifiers),
        Focus::LaunchpadDialog => handle_launchpad_key(app, code),
        Focus::KnowledgeDialog => handle_knowledge_dialog_key(app, code),
        Focus::KnowledgeViewer => handle_knowledge_viewer_key(app, code),
        Focus::Agent => handle_agent_key(app, code, modifiers),
        Focus::ContextTransfer => handle_context_transfer_key(app, code),
        Focus::RagTransfer => handle_rag_transfer_key(app, code),
        Focus::PromptTemplateDialog => handle_prompt_template_key(app, code, modifiers),
        Focus::GraphEditorDialog => handle_graph_editor_key(app, code, modifiers),
        Focus::GraphFormDialog => handle_graph_form_key(app, code, modifiers),
        Focus::ProjectRelationDialog => handle_preview_key(app, code, modifiers),
    }
}

// ── Mouse: scroll wheel + Shift+Click to copy selection ─────────────

fn open_terminal_search(app: &mut App) {
    let Some((is_terminal, idx)) = selected_terminal_like(app) else {
        return;
    };

    app.terminal_search = Some(if is_terminal {
        TerminalSearch::new(idx)
    } else {
        TerminalSearch::new_interactive(idx)
    });
}

// ── Terminal scrollback search (Ctrl+F) ─────────────────────────────

fn handle_terminal_search_key(app: &mut App, code: KeyCode) -> Result<()> {
    let Some(mut search) = app.terminal_search.take() else {
        return Ok(());
    };

    if code == KeyCode::Esc {
        return Ok(());
    }

    match code {
        KeyCode::Enter => {
            jump_terminal_search_match(&search, app);
            search.next_match();
        }
        KeyCode::Up => {
            search.prev_match();
            jump_terminal_search_match(&search, app);
        }
        KeyCode::Down => {
            search.next_match();
            jump_terminal_search_match(&search, app);
        }
        KeyCode::Char(c) => {
            search.query.push(c);
            refresh_terminal_search(&mut search, app);
            if !search.match_rows.is_empty() {
                search.current_match = 0;
                jump_terminal_search_match(&search, app);
            }
        }
        KeyCode::Backspace => {
            search.query.pop();
            refresh_terminal_search(&mut search, app);
        }
        _ => {}
    }

    app.terminal_search = Some(search);
    Ok(())
}

fn refresh_terminal_search(search: &mut TerminalSearch, app: &App) {
    let _ = with_terminal_like_agent(app, search.is_terminal, search.agent_idx, |agent| {
        search.search(agent);
    });
}

fn jump_terminal_search_match(search: &TerminalSearch, app: &mut App) {
    let _ = with_terminal_like_agent_mut(app, search.is_terminal, search.agent_idx, |agent| {
        search.jump_to_match(agent);
    });
}

/// Check if the currently selected agent is a Terminal agent.
fn is_terminal_agent_selected(app: &App) -> bool {
    matches!(app.selected_agent(), Some(AgentEntry::Terminal(_)))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod sidebar_mouse_tests;

/// Functional requirement 4's mouse surface: the project Focus tab bar and
/// per-tab list rows in the main panel, reusing the same
/// click-map-populated-during-draw pattern as the sidebar.
#[cfg(test)]
mod project_panel_mouse_tests;

#[cfg(test)]
mod graph_live_panel_mouse_tests;

#[cfg(test)]
mod split_selection_tests;

#[cfg(test)]
mod tick_duration_tests;

#[cfg(test)]
mod scroll_direction_tests;

#[cfg(test)]
mod rect_contains_point_tests;

#[cfg(test)]
mod clamp_sidebar_scroll_tests;

#[cfg(test)]
mod sidebar_tab_at_tests;

#[cfg(test)]
mod sidebar_agent_at_tests;

#[cfg(test)]
mod is_terminal_agent_selected_tests;

#[cfg(test)]
mod mouse_pty_position_tests;

#[cfg(test)]
mod clamped_pty_position_tests;

// C6: `scroll_terminal_like_agent` is the mouse-wheel routing gate that
// decides whether a scroll tick moves canopy's own buffer or goes to the
// child. Spawns a real (harmless) `cat` child so `in_alternate_screen()`
// reflects genuine vt100 state rather than a mock.
#[cfg(test)]
mod scroll_terminal_like_agent_tests;

// CT6: a left-click on the central preview area enters focus on the
// previewed session — the mouse equivalent of Enter in Preview. Spawns
// real (harmless) `cat` children so the terminal-like entries are genuine.
#[cfg(test)]
mod preview_focus_click_tests;

// ── CT14: defensive focus clears on the F2 / click wrappers ──────────────
#[cfg(test)]
mod ct14_sidebar_tests;

// ── CT19: per-region wheel scroll on the right panel ──────────────────────
#[cfg(test)]
mod ct19_panel_region_scroll_tests;

// ── CT29: the pointer seeds the brain; clicks and scroll do not ───────────
#[cfg(test)]
mod brain_seed_tests;

// ── Firefly Catcher: the click itself is hit-tested ────────────────────
#[cfg(test)]
mod firefly_click_tests;
