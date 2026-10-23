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

/// Main event loop: draw → poll events → refresh data.
pub fn run_event_graph(terminal: &mut Terminal, app: &mut App) -> Result<()> {
    while app.running {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(tick_duration(app))? {
            drain_pending_events(app)?;
        }

        #[cfg(unix)]
        if crate::tui::agent::pty::SIGHUP_RECEIVED.load(std::sync::atomic::Ordering::Relaxed) {
            app.running = false;
        }

        app.refresh()?;
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
mod tests {
    use super::search_picker::resolve_cd_picker_selection;
    use crate::tui::terminal_history::{PickerMode, SuggestionItem, SuggestionPicker};
    use std::path::PathBuf;

    #[test]
    fn test_cd_picker_selection_keeps_downstream_path() {
        let picker = SuggestionPicker {
            input: "./alpha".to_string(),
            mode: PickerMode::CdDirectory,
            all_items: vec![SuggestionItem {
                text: "./beta".to_string(),
                label: "./beta".to_string(),
                count: 0,
            }],
            items: vec![SuggestionItem {
                text: "./beta".to_string(),
                label: "./beta".to_string(),
                count: 0,
            }],
            selected: 0,
            scroll_offset: 0,
            cd_base_dir: Some(PathBuf::from("/repo")),
            cd_current_dir: Some(PathBuf::from("/repo/alpha")),
        };

        let resolved = resolve_cd_picker_selection(&picker).unwrap();
        assert_eq!(resolved, "alpha/beta");
    }

    #[test]
    fn test_cd_picker_selection_keeps_parent_path_relative_to_base() {
        let picker = SuggestionPicker {
            input: "./alpha/beta".to_string(),
            mode: PickerMode::CdDirectory,
            all_items: vec![SuggestionItem {
                text: "..".to_string(),
                label: "../".to_string(),
                count: 0,
            }],
            items: vec![SuggestionItem {
                text: "..".to_string(),
                label: "../".to_string(),
                count: 0,
            }],
            selected: 0,
            scroll_offset: 0,
            cd_base_dir: Some(PathBuf::from("/repo")),
            cd_current_dir: Some(PathBuf::from("/repo/alpha/beta")),
        };

        let resolved = resolve_cd_picker_selection(&picker).unwrap();
        assert_eq!(resolved, "alpha");
    }
}

#[cfg(test)]
mod sidebar_mouse_tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::models::{Agent, Cli, Trigger};
    use chrono::Utc;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Arc<Database> {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(Database::new(&path).expect("create test db"))
    }

    fn cron_agent(id: &str) -> Agent {
        Agent {
            id: id.to_string(),
            prompt: "prompt".to_string(),
            trigger: Some(Trigger::Cron {
                schedule_expr: "0 9 * * *".to_string(),
            }),
            cli: Cli::new("claude"),
            model: None,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: Utc::now(),
            log_path: "/tmp/test-sidebar-mouse.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: crate::domain::models::AgentAction::Headless,
        }
    }

    /// Builds an App with `count` background agents and a `sidebar_click_map`
    /// matching the row layout `draw_agent_list` produces (3-row cards, 1-row
    /// gap): agent `i` occupies rows `[i*4, i*4+3)`.
    fn app_with_agents(count: usize) -> App {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = (0..count)
            .map(|i| AgentEntry::Agent(cron_agent(&format!("agent-{i}"))))
            .collect();
        app.sidebar_visible = true;
        app.sidebar_click_map = (0..count)
            .map(|i| (i, (i * 4) as u16, (i * 4 + 3) as u16))
            .collect();
        app.selected = 0;
        app.focus = Focus::Preview;
        app
    }

    #[test]
    fn moved_updates_hovered_row_without_changing_selection() {
        let mut app = app_with_agents(3);
        assert_eq!(app.hovered_row, None);

        let mouse = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 5,
            row: 5, // falls in agent #1's rows [4, 7)
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_sidebar_mouse(&mut app, &mouse);

        assert!(consumed);
        assert_eq!(app.hovered_row, Some(1));
        assert_eq!(app.selected, 0, "hover must not change selection");
    }

    #[test]
    fn left_click_selects_agent_under_cursor() {
        let mut app = app_with_agents(3);

        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 9, // falls in agent #2's rows [8, 11)
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_sidebar_mouse(&mut app, &mouse);

        assert!(consumed);
        assert_eq!(app.selected, 2);
        assert!(matches!(app.focus, Focus::Preview));
    }

    #[test]
    fn left_click_on_already_selected_agent_enters_it() {
        let mut app = app_with_agents(3);
        app.selected = 2;

        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 9, // agent #2 again
            modifiers: KeyModifiers::NONE,
        };
        handle_sidebar_mouse(&mut app, &mouse);

        assert_eq!(app.selected, 2);
        assert!(matches!(app.focus, Focus::Agent));
    }

    #[test]
    fn scroll_down_increments_offset_within_bounds() {
        // 20 agents, 8 rows visible per the last render.
        let mut app = app_with_agents(20);
        app.sidebar_visible_capacity = 8;
        assert_eq!(app.sidebar_scroll_offset, 0);

        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        handle_sidebar_mouse(&mut app, &mouse);

        assert_eq!(app.sidebar_scroll_offset, 1);
    }

    #[test]
    fn clamp_sidebar_scroll_never_exceeds_max_offset() {
        // total=20, max_visible=8 → max_offset=12.
        assert_eq!(clamp_sidebar_scroll(0, 20, 8, -1), 1);
        assert_eq!(clamp_sidebar_scroll(12, 20, 8, -1), 12);
        assert_eq!(clamp_sidebar_scroll(1, 20, 8, 1), 0);
        assert_eq!(clamp_sidebar_scroll(0, 20, 8, 1), 0);
    }

    #[test]
    fn right_click_behaves_like_f2() {
        let mut via_key = app_with_agents(3);
        via_key.focus = Focus::Agent;
        let key_handled = handle_global_key(&mut via_key, KeyCode::F(2), KeyModifiers::NONE);

        let mut via_click = app_with_agents(3);
        via_click.focus = Focus::Agent;
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        let click_handled = handle_sidebar_mouse(&mut via_click, &mouse);

        assert!(key_handled);
        assert!(click_handled);
        assert!(via_key.sidebar_layer == via_click.sidebar_layer);
        assert!(matches!(via_key.focus, Focus::Preview));
        assert!(matches!(via_click.focus, Focus::Preview));
    }

    #[test]
    fn f2_cycles_sidebar_layer_skipping_empty_layers() {
        let mut app = app_with_agents(3);
        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Live);

        assert!(handle_global_key(
            &mut app,
            KeyCode::F(2),
            KeyModifiers::NONE
        ));
        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Automation);

        // Live and Knowledge are both empty, so a second press has nowhere
        // else to land and stays on Automation.
        assert!(handle_global_key(
            &mut app,
            KeyCode::F(2),
            KeyModifiers::NONE
        ));
        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Automation);
    }

    #[test]
    fn f2_cycles_through_all_three_tabs_when_all_have_content() {
        use crate::domain::models::{SplitGroup, SplitOrientation};

        // 3 background agents (Automation) from `app_with_agents`, plus a
        // split group (Live) and a project (Knowledge) so every tab has
        // something to land on and the ring wraps all the way around.
        let mut app = app_with_agents(3);
        app.agents.push(AgentEntry::Group(0));
        app.split_groups.push(SplitGroup {
            id: "g1".to_string(),
            orientation: SplitOrientation::Horizontal,
            session_a: "a".to_string(),
            session_b: "b".to_string(),
            created_at: Utc::now(),
        });
        app.projects
            .push(crate::domain::project::Project::new("/tmp/proj"));

        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Live);

        assert!(handle_global_key(
            &mut app,
            KeyCode::F(2),
            KeyModifiers::NONE
        ));
        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Automation);

        assert!(handle_global_key(
            &mut app,
            KeyCode::F(2),
            KeyModifiers::NONE
        ));
        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Knowledge);

        assert!(handle_global_key(
            &mut app,
            KeyCode::F(2),
            KeyModifiers::NONE
        ));
        assert_eq!(
            app.sidebar_layer,
            crate::tui::app::SidebarLayer::Live,
            "the ring wraps back to Live"
        );
    }

    #[test]
    fn shift_arrows_step_one_sidebar_tab_each_way_and_wrap() {
        let mut app = app_with_agents(3);
        app.focus = Focus::Home;
        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Live);

        assert!(handle_global_key(
            &mut app,
            KeyCode::Right,
            KeyModifiers::SHIFT
        ));
        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Automation);

        // Knowledge is empty here — unlike F2, the arrow still lands on it.
        assert!(handle_global_key(
            &mut app,
            KeyCode::Right,
            KeyModifiers::SHIFT
        ));
        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Knowledge);

        assert!(handle_global_key(
            &mut app,
            KeyCode::Right,
            KeyModifiers::SHIFT
        ));
        assert_eq!(
            app.sidebar_layer,
            crate::tui::app::SidebarLayer::Live,
            "stepping right off the end wraps to the first tab"
        );

        assert!(handle_global_key(
            &mut app,
            KeyCode::Left,
            KeyModifiers::SHIFT
        ));
        assert_eq!(
            app.sidebar_layer,
            crate::tui::app::SidebarLayer::Knowledge,
            "stepping left off the start wraps to the last tab"
        );
    }

    #[test]
    fn unshifted_arrows_do_not_switch_sidebar_tabs() {
        // Plain ←/→ belongs to the graph expand/collapse handler; only the
        // shifted pair is ours.
        let mut app = app_with_agents(3);
        app.focus = Focus::Home;
        assert!(!handle_global_key(
            &mut app,
            KeyCode::Right,
            KeyModifiers::NONE
        ));
        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Live);
    }

    #[test]
    fn shift_arrows_step_sidebar_tab_inside_a_focused_agent_without_a_split() {
        // Functional requirement 1: the sidebar tab strip must be reachable
        // from focus, not only from Home/Preview, as long as nothing else
        // (a split) already owns Shift+←/→ there.
        let mut app = app_with_agents(3);
        app.focus = Focus::Agent;
        app.selected = 0;

        assert!(handle_global_key(
            &mut app,
            KeyCode::Right,
            KeyModifiers::SHIFT
        ));

        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Automation);
        assert!(
            matches!(app.focus, Focus::Agent),
            "stepping tabs from focus must not disturb what is focused"
        );
    }

    #[test]
    fn shift_arrows_defer_to_split_focus_when_a_split_is_active() {
        // With a split active, Shift+←/→ keeps meaning split-pane focus —
        // the established binding that would otherwise be shadowed by
        // widening the sidebar-tab-step scope into Focus::Agent.
        let mut app = app_with_agents(3);
        app.focus = Focus::Agent;
        app.active_split_id = Some("split-1".to_string());

        assert!(!handle_global_key(
            &mut app,
            KeyCode::Right,
            KeyModifiers::SHIFT
        ));
        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Live);
    }

    #[test]
    fn clicking_a_sidebar_tab_switches_it() {
        let mut app = app_with_agents(3);
        app.sidebar_tab_click_map = vec![
            (crate::tui::app::SidebarLayer::Live, 0, 0, 11),
            (crate::tui::app::SidebarLayer::Automation, 0, 11, 22),
            (crate::tui::app::SidebarLayer::Knowledge, 0, 22, 33),
        ];
        app.focus = Focus::Agent;

        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 15, // inside the Automation cell [11, 22)
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_sidebar_mouse(&mut app, &mouse);

        assert!(consumed);
        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Automation);
        assert!(
            matches!(app.focus, Focus::Preview),
            "clicking a tab must back out of a deep Focus::Agent view"
        );
    }

    #[test]
    fn clicking_an_empty_sidebar_tab_still_switches_to_it() {
        // Knowledge has no projects — a keyboard F2 cycle would skip it, but
        // a deliberate click on its label must still land there so the
        // empty state ("No registered projects") is reachable.
        let mut app = app_with_agents(3);
        app.sidebar_tab_click_map = vec![
            (crate::tui::app::SidebarLayer::Live, 0, 0, 11),
            (crate::tui::app::SidebarLayer::Automation, 0, 11, 22),
            (crate::tui::app::SidebarLayer::Knowledge, 0, 22, 33),
        ];

        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 25,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        handle_sidebar_mouse(&mut app, &mouse);

        assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Knowledge);
    }

    #[test]
    fn f6_no_longer_opens_a_panel_picker_in_preview_or_agent_focus() {
        // The F6 panel picker is retired: the pin lives in
        // F1 → Settings → TUI Appearance now. In both surfaces where F6
        // used to open the picker it must be a no-op — most visibly, the
        // global handler no longer claims the key.
        let mut preview = app_with_agents(1);
        assert!(matches!(preview.focus, Focus::Preview));
        assert!(
            !handle_global_key(&mut preview, KeyCode::F(6), KeyModifiers::NONE),
            "F6 must not be claimed by the global handler anymore"
        );
        handle_key(&mut preview, KeyCode::F(6), KeyModifiers::NONE).unwrap();
        assert!(!preview.hub_open, "F6 must not open the hub in Preview");
        assert!(
            matches!(preview.focus, Focus::Preview),
            "F6 must leave Preview untouched"
        );

        let mut agent = app_with_agents(1);
        agent.focus = Focus::Agent;
        assert!(
            !handle_global_key(&mut agent, KeyCode::F(6), KeyModifiers::NONE),
            "F6 must not be claimed by the global handler anymore"
        );
        handle_key(&mut agent, KeyCode::F(6), KeyModifiers::NONE).unwrap();
        assert!(!agent.hub_open, "F6 must not open the hub in Agent focus");
        assert!(
            matches!(agent.focus, Focus::Agent),
            "F6 must leave the focused session untouched"
        );
    }
}

/// Functional requirement 4's mouse surface: the project Focus tab bar and
/// per-tab list rows in the main panel, reusing the same
/// click-map-populated-during-draw pattern as the sidebar.
#[cfg(test)]
mod project_panel_mouse_tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::graphs::{GraphSpec, GraphSpecStatus};
    use crate::domain::project::Project;
    use crate::tui::app::types::ProjectTab;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Arc<Database> {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(Database::new(&path).expect("create test db"))
    }

    fn backlog_spec(id: &str, name: &str) -> GraphSpec {
        GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: name.to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        }
    }

    /// Builds an App focused on a single project's Backlog tab, with the
    /// main panel occupying rows `[2, 22)` / columns `[0, 40)` — matching
    /// what `draw_project_tabs_panel` would have set on the last frame —
    /// and a tab bar + row click map for 3 backlog specs at rows `[3, 6)`.
    fn app_with_project_backlog(count: usize) -> App {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.projects = vec![Project::new("/tmp/project")];
        app.sidebar_layer = crate::tui::app::SidebarLayer::Knowledge;
        app.selected_project = 0;
        app.project_focus = Some(ProjectTab::Backlog);
        app.backlog_specs = (0..count)
            .map(|i| backlog_spec(&format!("spec-{i}"), &format!("Spec {i}")))
            .collect();
        app.selected_backlog = 0;
        app.focus = Focus::Agent;

        app.last_panel_x = 0;
        app.last_panel_y = 2;
        app.last_panel_inner = (40, 20);
        app.project_tab_click_map = vec![
            (ProjectTab::Overview, 0, 9),
            (ProjectTab::Backlog, 9, 17),
            (ProjectTab::Knowledge, 17, 27),
            (ProjectTab::History, 27, 35),
        ];
        app.project_tab_row_click_map = (0..count)
            .map(|i| (i, (3 + i) as u16, (4 + i) as u16))
            .collect();
        app
    }

    #[test]
    fn clicking_tab_bar_switches_active_tab() {
        let mut app = app_with_project_backlog(3);
        assert_eq!(app.project_focus, Some(ProjectTab::Backlog));

        // Row == last_panel_y is the tab-bar row; column 20 falls inside the
        // Knowledge tab's span [17, 27).
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row: app.last_panel_y,
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_project_panel_mouse(&mut app, &mouse);

        assert!(consumed);
        assert_eq!(app.project_focus, Some(ProjectTab::Knowledge));
    }

    #[test]
    fn clicking_a_list_row_selects_it_without_changing_tab() {
        let mut app = app_with_project_backlog(3);

        // Row 5 falls in spec #2's span [5, 6) from `project_tab_row_click_map`.
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_project_panel_mouse(&mut app, &mouse);

        assert!(consumed);
        assert_eq!(app.selected_backlog, 2);
        assert_eq!(
            app.project_focus,
            Some(ProjectTab::Backlog),
            "clicking a row must not cycle the tab"
        );
    }

    #[test]
    fn scroll_down_in_panel_advances_the_active_tabs_list() {
        let mut app = app_with_project_backlog(3);
        assert_eq!(app.selected_backlog, 0);

        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_project_panel_mouse(&mut app, &mouse);

        assert!(consumed);
        assert_eq!(app.selected_backlog, 1);
    }

    #[test]
    fn scroll_up_in_panel_retreats_the_active_tabs_list() {
        let mut app = app_with_project_backlog(3);
        app.selected_backlog = 1;

        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 5,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_project_panel_mouse(&mut app, &mouse);

        assert!(consumed);
        assert_eq!(app.selected_backlog, 0);
    }

    #[test]
    fn click_outside_the_panel_rect_is_not_consumed() {
        let mut app = app_with_project_backlog(3);

        // Row 30 is past last_panel_y (2) + last_panel_inner.1 (20) = 22.
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 30,
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_project_panel_mouse(&mut app, &mouse);

        assert!(!consumed);
        assert_eq!(app.selected_backlog, 0);
        assert_eq!(app.project_focus, Some(ProjectTab::Backlog));
    }

    #[test]
    fn click_is_not_consumed_when_no_project_is_focused() {
        let mut app = app_with_project_backlog(3);
        app.project_focus = None;

        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row: app.last_panel_y,
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_project_panel_mouse(&mut app, &mouse);

        assert!(!consumed);
    }

    #[test]
    fn click_is_not_consumed_outside_the_knowledge_sidebar_layer() {
        let mut app = app_with_project_backlog(3);
        app.sidebar_layer = crate::tui::app::SidebarLayer::Live;

        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row: app.last_panel_y,
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_project_panel_mouse(&mut app, &mouse);

        assert!(!consumed);
        assert_eq!(
            app.project_focus,
            Some(ProjectTab::Backlog),
            "leaving Knowledge must not itself change the remembered tab"
        );
    }
}

#[cfg(test)]
mod graph_live_panel_mouse_tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::graphs::{GraphSpecStatus, GraphStatus};
    use crate::tui::app::graph_live_state::{GraphLiveState, SpecQueueEntry};
    use crate::tui::app::{AutomationKind, GraphLiveFocus};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Arc<Database> {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(Database::new(&path).expect("create test db"))
    }

    fn spec_entry(id: &str, status: GraphSpecStatus) -> SpecQueueEntry {
        SpecQueueEntry {
            spec_id: id.to_string(),
            spec_name: format!("Spec {id}"),
            status,
            failure_reason: None,
            spec_code: None,
            spec_type: None,
            spec_scope: None,
        }
    }

    /// Builds an App on the live graph view with a 3-spec queue and a marker
    /// strip click map matching what `draw_graph_live_view` would have
    /// produced for chips at columns `[0,3)`, `[4,7)`, `[8,11)` on row 5.
    fn app_on_graph_live_view() -> App {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.focus = Focus::Preview;
        app.sidebar_layer = SidebarLayer::Automation;
        app.automation_kind = AutomationKind::Graph;
        app.graph_live_state = Some(GraphLiveState {
            graph_id: "lp1".to_string(),
            graph_name: "graph".to_string(),
            graph_status: GraphStatus::Running,
            workdir: "/tmp".to_string(),
            trigger_type: "manual".to_string(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            spec_queue: vec![
                spec_entry("s1", GraphSpecStatus::Completed),
                spec_entry("s2", GraphSpecStatus::Running),
                spec_entry("s3", GraphSpecStatus::Failed),
            ],
            done_count: 1,
            total_count: 3,
            current_spec_id: Some("s2".to_string()),
            effective_nodes: Vec::new(),
            effective_edges: Vec::new(),
            ensembles: Vec::new(),
            router_taken_routes: HashMap::new(),
            current_node_id: None,
            current_node_status: None,
            current_node_started_at: None,
            current_node_iteration: None,
        });

        app.last_panel_x = 0;
        app.last_panel_y = 2;
        app.last_panel_inner = (40, 20);
        app.graph_spec_strip_click_map = vec![
            ("s1".to_string(), 5, 0, 3),
            ("s2".to_string(), 5, 4, 7),
            ("s3".to_string(), 5, 8, 11),
        ];
        app.graph_spec_strip_capacity = 3;
        app
    }

    #[test]
    fn clicking_a_marker_selects_that_spec_and_focuses_the_strip() {
        let mut app = app_on_graph_live_view();
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 9,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_graph_live_panel_mouse(&mut app, &mouse);

        assert!(consumed);
        assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s3"));
        assert_eq!(app.graph_live_focus, GraphLiveFocus::SpecStrip);
    }

    #[test]
    fn clicking_the_gap_between_chips_is_not_consumed() {
        let mut app = app_on_graph_live_view();
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_graph_live_panel_mouse(&mut app, &mouse);

        assert!(!consumed);
        assert!(app.graph_spec_strip_selected.is_none());
    }

    #[test]
    fn scroll_over_the_marker_row_pages_the_strip() {
        let mut app = app_on_graph_live_view();
        app.graph_spec_strip_capacity = 1; // force scrolling to matter
        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_graph_live_panel_mouse(&mut app, &mouse);

        assert!(consumed);
        assert_eq!(app.graph_spec_strip_scroll, 1);
    }

    #[test]
    fn mouse_ignored_when_not_on_the_graph_live_view() {
        let mut app = app_on_graph_live_view();
        app.automation_kind = AutomationKind::Agent;
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 9,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        let consumed = handle_graph_live_panel_mouse(&mut app, &mouse);

        assert!(!consumed);
        assert!(app.graph_spec_strip_selected.is_none());
    }
}

#[cfg(test)]
mod split_selection_tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::models::{SplitGroup, SplitOrientation};
    use chrono::Utc;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Arc<Database> {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(Database::new(&path).expect("create test db"))
    }

    fn spawn_test_terminal(name: &str) -> InteractiveAgent {
        InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some(name),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn terminal")
    }

    /// Two terminal sessions in a horizontal split. Sets `last_panel_*` to
    /// pretend the *focused* half was just rendered at x=41 (the panel to
    /// the right of a 40-column left half), matching what `draw_split_panel`
    /// records for whichever side has focus.
    fn app_with_split_terminals(right_focused: bool) -> App {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.terminal_agents.push(spawn_test_terminal("left-term"));
        app.terminal_agents.push(spawn_test_terminal("right-term"));
        app.split_groups.push(SplitGroup {
            id: "split-1".to_string(),
            orientation: SplitOrientation::Horizontal,
            session_a: "left-term".to_string(),
            session_b: "right-term".to_string(),
            created_at: Utc::now(),
        });
        app.active_split_id = Some("split-1".to_string());
        app.split_right_focused = right_focused;
        app.focus = Focus::Agent;
        app.last_panel_x = if right_focused { 41 } else { 0 };
        app.last_panel_y = 1;
        app.last_panel_inner = (39, 20);
        app
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_pty_position_uses_focused_panel_x_offset() {
        let app = app_with_split_terminals(true);

        // Inside the right panel (starts at column 41).
        assert_eq!(
            mouse_pty_position(&app, &mouse(MouseEventKind::Moved, 45, 3)),
            Some((4, 2))
        );
        // Left of the focused panel's recorded x-offset: out of bounds.
        assert_eq!(
            mouse_pty_position(&app, &mouse(MouseEventKind::Moved, 10, 3)),
            None
        );
    }

    #[test]
    fn clamped_pty_position_clamps_to_focused_panel_bounds() {
        let app = app_with_split_terminals(true);

        assert_eq!(
            clamped_pty_position(
                &app,
                &mouse(MouseEventKind::Drag(MouseButton::Left), 999, 999)
            ),
            (38, 19)
        );
    }

    #[test]
    fn focused_terminal_like_follows_split_focus_not_sidebar_selection() {
        let mut app = app_with_split_terminals(false);
        // Sidebar selection points nowhere useful (default `selected = 0`
        // with no AgentEntry list) — the split's focused panel must still
        // resolve correctly.
        assert_eq!(focused_terminal_like(&app), Some((true, 0)));

        app.split_right_focused = true;
        assert_eq!(focused_terminal_like(&app), Some((true, 1)));
    }

    #[test]
    fn drag_selection_in_split_targets_focused_right_panel() {
        let mut app = app_with_split_terminals(true);
        app.terminal_agents[1].replay_scrollback_lines(&["RIGHTPANELTEXT".to_string()]);

        let down = mouse(MouseEventKind::Down(MouseButton::Left), 41, 1);
        assert!(handle_selection_mouse(&mut app, &down));
        let sel = app.terminal_selection.as_ref().expect("selection started");
        assert_eq!(sel.agent, (true, 1));

        let drag = mouse(MouseEventKind::Drag(MouseButton::Left), 55, 1);
        assert!(handle_selection_mouse(&mut app, &drag));

        app.show_copied = false;
        let up = mouse(MouseEventKind::Up(MouseButton::Left), 55, 1);
        assert!(handle_selection_mouse(&mut app, &up));

        assert!(app.terminal_selection.is_none());
        assert!(
            app.show_copied,
            "expected the focused (right) panel's real text to be copied"
        );
    }

    #[test]
    fn drag_selection_in_split_targets_focused_left_panel() {
        let mut app = app_with_split_terminals(false);
        app.terminal_agents[0].replay_scrollback_lines(&["LEFTPANELTEXT".to_string()]);

        let down = mouse(MouseEventKind::Down(MouseButton::Left), 0, 1);
        assert!(handle_selection_mouse(&mut app, &down));
        assert_eq!(
            app.terminal_selection
                .as_ref()
                .expect("selection started")
                .agent,
            (true, 0)
        );

        let drag = mouse(MouseEventKind::Drag(MouseButton::Left), 14, 1);
        assert!(handle_selection_mouse(&mut app, &drag));

        app.show_copied = false;
        let up = mouse(MouseEventKind::Up(MouseButton::Left), 14, 1);
        assert!(handle_selection_mouse(&mut app, &up));

        assert!(app.show_copied);
    }

    #[test]
    fn shift_click_copy_in_split_resolves_focused_panel_not_sidebar_selection() {
        let mut app = app_with_split_terminals(true);
        app.terminal_agents[1].replay_scrollback_lines(&["RIGHT SCREEN CONTENT".to_string()]);
        // `app.agents`/`app.selected` are left at their defaults (no sidebar
        // selection at all) — the old `selected_terminal_like`-based lookup
        // would resolve nothing and shift+click copy would silently no-op.

        let text = with_selected_terminal_like(&app, InteractiveAgent::get_plain_text_from_screen)
            .flatten()
            .unwrap_or_default();
        assert!(
            text.contains("RIGHT SCREEN CONTENT"),
            "expected shift+click's resolver to read the split-focused panel, got: {text:?}"
        );

        let handled = handle_copy_click(
            &mut app,
            &MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 41,
                row: 1,
                modifiers: KeyModifiers::SHIFT,
            },
        );
        assert!(handled);
    }
}

#[cfg(test)]
mod tick_duration_tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn app_for_tick(focus: Focus) -> App {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.focus = focus;
        app
    }

    #[test]
    fn agent_focus_returns_50ms() {
        let app = app_for_tick(Focus::Agent);
        assert_eq!(tick_duration(&app), Duration::from_millis(50));
    }

    #[test]
    fn preview_focus_returns_100ms() {
        let app = app_for_tick(Focus::Preview);
        assert_eq!(tick_duration(&app), Duration::from_millis(100));
    }

    #[test]
    fn home_without_brain_returns_200ms() {
        let app = app_for_tick(Focus::Home);
        assert_eq!(tick_duration(&app), Duration::from_millis(50));
    }

    #[test]
    fn new_agent_dialog_returns_50ms() {
        let app = app_for_tick(Focus::NewAgentDialog);
        assert_eq!(tick_duration(&app), Duration::from_millis(50));
    }

    #[test]
    fn prompt_template_dialog_returns_50ms() {
        let app = app_for_tick(Focus::PromptTemplateDialog);
        assert_eq!(tick_duration(&app), Duration::from_millis(50));
    }
}

#[cfg(test)]
mod scroll_direction_tests {
    use super::*;

    #[test]
    fn scroll_up_returns_positive() {
        assert_eq!(scroll_direction(MouseEventKind::ScrollUp), Some(1));
    }

    #[test]
    fn scroll_down_returns_negative() {
        assert_eq!(scroll_direction(MouseEventKind::ScrollDown), Some(-1));
    }

    #[test]
    fn other_mouse_kinds_return_none() {
        assert!(scroll_direction(MouseEventKind::Moved).is_none());
        assert!(scroll_direction(MouseEventKind::Down(MouseButton::Left)).is_none());
        assert!(scroll_direction(MouseEventKind::Up(MouseButton::Left)).is_none());
        assert!(scroll_direction(MouseEventKind::Drag(MouseButton::Left)).is_none());
    }
}

#[cfg(test)]
mod rect_contains_point_tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn point_inside_rect() {
        let rect = Rect::new(5, 10, 20, 10);
        assert!(rect_contains_point(rect, 10, 12));
        assert!(rect_contains_point(rect, 5, 10)); // top-left corner
        assert!(rect_contains_point(rect, 24, 19)); // bottom-right corner
    }

    #[test]
    fn point_outside_rect() {
        let rect = Rect::new(5, 10, 20, 10);
        assert!(!rect_contains_point(rect, 4, 10)); // left
        assert!(!rect_contains_point(rect, 25, 10)); // right edge
        assert!(!rect_contains_point(rect, 10, 9)); // above
        assert!(!rect_contains_point(rect, 10, 20)); // below
    }

    #[test]
    fn zero_size_rect() {
        let rect = Rect::new(5, 10, 0, 0);
        assert!(!rect_contains_point(rect, 5, 10));
    }
}

#[cfg(test)]
mod clamp_sidebar_scroll_tests {
    use super::*;

    #[test]
    fn scroll_down_increments() {
        assert_eq!(clamp_sidebar_scroll(0, 20, 8, -1), 1);
    }

    #[test]
    fn scroll_down_clamps_at_max() {
        // total=20, max_visible=8 → max_offset=12
        assert_eq!(clamp_sidebar_scroll(12, 20, 8, -1), 12);
        assert_eq!(
            clamp_sidebar_scroll(13, 20, 8, -1),
            12,
            "should not exceed max"
        );
    }

    #[test]
    fn scroll_up_decrements() {
        assert_eq!(clamp_sidebar_scroll(5, 20, 8, 1), 4);
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        assert_eq!(clamp_sidebar_scroll(0, 20, 8, 1), 0);
    }

    #[test]
    fn empty_list_stays_at_zero() {
        assert_eq!(clamp_sidebar_scroll(0, 0, 8, -1), 0);
        assert_eq!(clamp_sidebar_scroll(0, 0, 8, 1), 0);
    }
}

#[cfg(test)]
mod sidebar_tab_at_tests {
    use super::*;
    use std::sync::Arc;

    fn make_app() -> App {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let dir = tempfile::tempdir().unwrap();
        App::new(
            db,
            dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn finds_tab_by_position() {
        let mut app = make_app();
        app.sidebar_tab_click_map = vec![
            (SidebarLayer::Live, 0, 0, 11),
            (SidebarLayer::Automation, 0, 11, 22),
            (SidebarLayer::Knowledge, 0, 22, 33),
        ];

        assert_eq!(sidebar_tab_at(&app, 0, 5), Some(SidebarLayer::Live));
        assert_eq!(sidebar_tab_at(&app, 0, 15), Some(SidebarLayer::Automation));
        assert_eq!(sidebar_tab_at(&app, 0, 25), Some(SidebarLayer::Knowledge));
    }

    #[test]
    fn wrong_row_returns_none() {
        let mut app = make_app();
        app.sidebar_tab_click_map = vec![(SidebarLayer::Live, 0, 0, 11)];

        assert!(sidebar_tab_at(&app, 1, 5).is_none());
    }
}

#[cfg(test)]
mod sidebar_agent_at_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn finds_agent_by_row() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.sidebar_click_map = vec![(0, 0, 3), (1, 4, 7), (2, 8, 11)];

        assert_eq!(sidebar_agent_at(&app, 1), Some(0));
        assert_eq!(sidebar_agent_at(&app, 5), Some(1));
        assert_eq!(sidebar_agent_at(&app, 9), Some(2));
        assert!(sidebar_agent_at(&app, 3).is_none());
    }
}

#[cfg(test)]
mod is_terminal_agent_selected_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn no_selection_returns_false() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let app = App::new(
            db,
            dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        assert!(!is_terminal_agent_selected(&app));
    }
}

#[cfg(test)]
mod mouse_pty_position_tests {
    use super::*;
    use std::sync::Arc;

    fn mouse_at(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Moved,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn make_app() -> App {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let dir = tempfile::tempdir().unwrap();
        App::new(
            db,
            dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn inside_panel_returns_relative_coords() {
        let mut app = make_app();
        app.last_panel_x = 10;
        app.last_panel_y = 5;
        app.last_panel_inner = (40, 20);

        assert_eq!(mouse_pty_position(&app, &mouse_at(15, 8)), Some((5, 3)));
    }

    #[test]
    fn outside_panel_returns_none() {
        let mut app = make_app();
        app.last_panel_x = 10;
        app.last_panel_y = 5;
        app.last_panel_inner = (40, 20);

        assert!(mouse_pty_position(&app, &mouse_at(3, 8)).is_none());
        assert!(mouse_pty_position(&app, &mouse_at(55, 8)).is_none());
        assert!(mouse_pty_position(&app, &mouse_at(15, 3)).is_none());
        assert!(mouse_pty_position(&app, &mouse_at(15, 30)).is_none());
    }
}

#[cfg(test)]
mod clamped_pty_position_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn clamps_to_panel_bounds() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.last_panel_x = 10;
        app.last_panel_y = 5;
        app.last_panel_inner = (40, 20);

        let mouse = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 999,
            row: 999,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(clamped_pty_position(&app, &mouse), (39, 19));
    }
}

// C6: `scroll_terminal_like_agent` is the mouse-wheel routing gate that
// decides whether a scroll tick moves canopy's own buffer or goes to the
// child. Spawns a real (harmless) `cat` child so `in_alternate_screen()`
// reflects genuine vt100 state rather than a mock.
#[cfg(test)]
mod scroll_terminal_like_agent_tests {
    use super::*;
    use crate::domain::models::Cli;
    use ratatui::style::Color;

    fn spawn_cat_agent() -> InteractiveAgent {
        InteractiveAgent::spawn(
            Cli::new("cat"),
            ".",
            80,
            24,
            None,
            None,
            Color::Reset,
            Some("scroll-test-agent"),
            &[],
            None,
            None,
            None,
        )
        .expect("spawn cat as a stand-in interactive child")
    }

    #[test]
    fn outside_alternate_screen_scrolls_canopy_own_buffer() {
        let mut agent = spawn_cat_agent();
        assert!(!agent.in_alternate_screen());
        agent.scroll_offset = 5;

        scroll_terminal_like_agent(&mut agent, -1, 3);

        // Routed locally: scroll_offset moved, no PTY write was needed.
        assert_eq!(agent.scroll_offset, 2);
        agent.kill();
    }

    #[test]
    fn inside_alternate_screen_defers_to_the_child_instead() {
        let mut agent = spawn_cat_agent();
        // Feed the alternate-screen DECSET directly into the vt100 parser,
        // as a real full-screen child's own output would (rather than
        // relying on pty echo, which `cat` may or may not perform).
        agent.vt.lock().expect("vt lock").process(b"\x1b[?1049h");
        assert!(agent.in_alternate_screen());
        agent.scroll_offset = 5;

        scroll_terminal_like_agent(&mut agent, -1, 3);

        // Routed to the child: local scroll_offset is untouched.
        assert_eq!(agent.scroll_offset, 5);
        agent.kill();
    }
}

// CT6: a left-click on the central preview area enters focus on the
// previewed session — the mouse equivalent of Enter in Preview. Spawns
// real (harmless) `cat` children so the terminal-like entries are genuine.
#[cfg(test)]
mod preview_focus_click_tests {
    use super::*;
    use crate::application::ports::AgentRepository;
    use crate::domain::models::Cli;
    use ratatui::style::Color;
    use std::sync::Arc;

    fn spawn_cat_interactive(name: &str) -> InteractiveAgent {
        InteractiveAgent::spawn(
            Cli::new("cat"),
            ".",
            80,
            24,
            None,
            None,
            Color::Reset,
            Some(name),
            &[],
            None,
            None,
            None,
        )
        .expect("spawn cat as a stand-in interactive child")
    }

    fn spawn_cat_terminal(name: &str) -> InteractiveAgent {
        InteractiveAgent::spawn_terminal("cat", "/tmp", 80, 24, Some(name), &[], Color::White)
            .expect("spawn cat as a stand-in terminal child")
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Previewing an interactive session with the central panel last
    /// rendered at (30, 5) of size 80x20 — so (35, 10) is inside it.
    fn app_previewing_interactive() -> App {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.interactive_agents = vec![spawn_cat_interactive("ct6-click")];
        app.agents = vec![AgentEntry::Interactive(0)];
        app.selected = 0;
        app.focus = Focus::Preview;
        app.last_panel_x = 30;
        app.last_panel_y = 5;
        app.last_panel_inner = (80, 20);
        app
    }

    #[test]
    fn central_left_click_on_interactive_session_enters_focus() {
        let mut app = app_previewing_interactive();
        app.log_scroll = 7;

        let consumed = handle_preview_focus_click(
            &mut app,
            &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10),
        );

        assert!(consumed, "central click must be consumed, not forwarded");
        assert!(matches!(app.focus, Focus::Agent));
        assert_eq!(app.log_scroll, 0, "same reset as keyboard focus entry");
        app.interactive_agents[0].kill();
    }

    #[test]
    fn central_left_click_on_terminal_session_enters_focus() {
        let mut app = app_previewing_interactive();
        app.terminal_agents = vec![spawn_cat_terminal("ct6-click-term")];
        app.agents = vec![AgentEntry::Terminal(0)];

        let consumed = handle_preview_focus_click(
            &mut app,
            &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10),
        );

        assert!(consumed);
        assert!(matches!(app.focus, Focus::Agent));
        app.terminal_agents[0].kill();
        app.interactive_agents[0].kill();
    }

    #[test]
    fn click_with_agent_focus_does_not_reenter() {
        // Once focused, clicks belong to the child — the helper must not
        // consume them, preserving focused-child mouse behavior.
        let mut app = app_previewing_interactive();
        app.focus = Focus::Agent;

        assert!(!handle_preview_focus_click(
            &mut app,
            &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10)
        ));
        assert!(matches!(app.focus, Focus::Agent));
        app.interactive_agents[0].kill();
    }

    #[test]
    fn click_outside_the_central_rectangle_is_not_consumed() {
        let mut app = app_previewing_interactive();

        assert!(!handle_preview_focus_click(
            &mut app,
            &mouse(MouseEventKind::Down(MouseButton::Left), 5, 10)
        ));
        assert!(matches!(app.focus, Focus::Preview));
        app.interactive_agents[0].kill();
    }

    #[test]
    fn non_left_clicks_are_not_consumed() {
        let mut app = app_previewing_interactive();

        assert!(!handle_preview_focus_click(
            &mut app,
            &mouse(MouseEventKind::Down(MouseButton::Right), 35, 10)
        ));
        assert!(!handle_preview_focus_click(
            &mut app,
            &mouse(MouseEventKind::ScrollDown, 35, 10)
        ));
        assert!(matches!(app.focus, Focus::Preview));
        app.interactive_agents[0].kill();
    }

    #[test]
    fn background_agent_preview_is_not_consumed() {
        // A background (non-terminal) preview keeps its current behavior.
        let mut app = app_previewing_interactive();
        let db = app.db.clone();
        let agent = crate::domain::models::Agent {
            id: "bg-1".to_string(),
            prompt: "prompt".to_string(),
            trigger: None,
            cli: Cli::new("claude"),
            model: None,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: chrono::Utc::now(),
            log_path: "/tmp/ct6-click.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: crate::domain::models::AgentAction::Headless,
        };
        db.upsert_agent(&agent).expect("seed agent");
        app.agents = vec![AgentEntry::Agent(agent)];

        assert!(!handle_preview_focus_click(
            &mut app,
            &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10)
        ));
        assert!(matches!(app.focus, Focus::Preview));
        app.interactive_agents[0].kill();
    }

    #[test]
    fn graph_live_view_central_click_is_not_consumed() {
        // The graph owns the centre in the graph view — even with a
        // terminal-like sidebar selection underneath.
        let mut app = app_previewing_interactive();
        app.sidebar_layer = SidebarLayer::Automation;
        app.automation_kind = crate::tui::app::AutomationKind::Graph;

        assert!(!handle_preview_focus_click(
            &mut app,
            &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10)
        ));
        assert!(matches!(app.focus, Focus::Preview));
        app.interactive_agents[0].kill();
    }

    #[test]
    fn playground_central_click_is_not_consumed() {
        let mut app = app_previewing_interactive();
        app.playground_active = true;

        assert!(!handle_preview_focus_click(
            &mut app,
            &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10)
        ));
        assert!(matches!(app.focus, Focus::Preview));
        app.interactive_agents[0].kill();
    }

    #[test]
    fn missing_geometry_is_not_consumed() {
        // No frame rendered yet: leave dispatch unchanged.
        let mut app = app_previewing_interactive();
        app.last_panel_inner = (0, 0);

        assert!(!handle_preview_focus_click(
            &mut app,
            &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10)
        ));
        assert!(matches!(app.focus, Focus::Preview));
        app.interactive_agents[0].kill();
    }

    #[test]
    fn central_click_through_handle_mouse_enters_focus() {
        // End-to-end through the real dispatch order: the sidebar must not
        // see the click (hide it), panels above must pass, and the helper
        // consumes before PTY forwarding / selection below.
        let mut app = app_previewing_interactive();
        app.sidebar_visible = false;

        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 35, 10),
        )
        .expect("handle mouse");

        assert!(matches!(app.focus, Focus::Agent));
        assert!(app.terminal_selection.is_none());
        app.interactive_agents[0].kill();
    }
}

// ── CT14: defensive focus clears on the F2 / click wrappers ──────────────
#[cfg(test)]
mod ct14_sidebar_tests {
    use super::*;
    use crate::db::Database;
    use crate::tui::app::types::ProjectTab;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Arc<Database> {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(Database::new(&path).expect("create test db"))
    }

    #[test]
    fn ct14_f2_clears_dangling_project_focus_defensively() {
        // Inject the illegal state directly: a `project_focus` surviving on a
        // non-Knowledge layer. Both wrappers must clear it idempotently.
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        app.sidebar_layer = SidebarLayer::Live;
        app.project_focus = Some(ProjectTab::Backlog);
        cycle_sidebar_layer_and_normalize_focus(&mut app);
        assert!(
            app.project_focus.is_none(),
            "F2 cycle must clear a dangling project focus"
        );

        app.sidebar_layer = SidebarLayer::Live;
        app.project_focus = Some(ProjectTab::Backlog);
        switch_sidebar_tab_and_normalize_focus(&mut app, SidebarLayer::Automation);
        assert!(
            app.project_focus.is_none(),
            "tab click must clear a dangling project focus"
        );
    }
}

// ── CT19: per-region wheel scroll on the right panel ──────────────────────
#[cfg(test)]
mod ct19_panel_region_scroll_tests {
    use super::*;
    use crate::db::Database;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Arc<Database> {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(Database::new(&path).expect("create test db"))
    }

    fn test_app() -> App {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app")
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Panel spans (0,0)-(40,20); knowledge list rect sits inside it at
    /// (0,10)-(40,20). Scrolling with the pointer inside the list rect must
    /// move only the list offset, not `sync_scroll_offset` — the CT19 bug
    /// was every region moving the activity offset.
    #[test]
    fn scroll_inside_knowledge_list_rect_moves_only_list_offset() {
        let mut app = test_app();
        app.last_sync_area = Some(ratatui::layout::Rect::new(0, 0, 40, 20));
        app.last_activity_rect = Some(ratatui::layout::Rect::new(0, 0, 40, 6));
        app.last_knowledge_list_rect = Some(ratatui::layout::Rect::new(0, 10, 40, 10));
        app.project_knowledge = (0..20)
            .map(|i| crate::db::intelligence::IntelligenceNodeRecord {
                id: format!("k{i}"),
                kind: "fact".to_string(),
                status: "active".to_string(),
                title: format!("node {i}"),
                body: String::new(),
                metadata: None,
                project_hash: None,
                session_id: None,
                created_at: 0,
                updated_at: 0,
            })
            .collect();
        app.sync_scroll_offset = 5;

        let consumed =
            handle_sync_panel_scroll(&mut app, &mouse(MouseEventKind::ScrollDown, 5, 15), -1);

        assert!(consumed);
        assert_eq!(app.sync_scroll_offset, 5, "activity offset must not move");
        assert_eq!(app.knowledge_list_scroll, 1);
        assert!(app.panel_interacting, "scroll must mark panel interaction");
    }

    /// Pointer inside the Graph face's rect calls the graph-face scroll,
    /// clamped by its own bookkeeping (not the main pane's).
    #[test]
    fn scroll_inside_graph_face_rect_calls_graph_face_scroll() {
        let mut app = test_app();
        app.last_sync_area = Some(ratatui::layout::Rect::new(0, 0, 40, 20));
        app.last_graph_face_rect = Some(ratatui::layout::Rect::new(0, 0, 40, 20));
        app.graph_face_total_lines = 50;
        app.graph_face_scroll = 0;

        let consumed =
            handle_sync_panel_scroll(&mut app, &mouse(MouseEventKind::ScrollDown, 5, 5), -1);

        assert!(consumed);
        assert_eq!(app.graph_face_scroll, 3);
        // The main pane's own field is untouched by this path.
        assert_eq!(app.graph_live_view_scroll, 0);
    }

    /// Pointer inside the panel but outside every recorded region: consumed
    /// (never falls through to the sidebar), and changes nothing.
    #[test]
    fn scroll_inside_panel_outside_every_region_is_consumed_and_noop() {
        let mut app = test_app();
        app.last_sync_area = Some(ratatui::layout::Rect::new(0, 0, 40, 20));
        app.last_activity_rect = Some(ratatui::layout::Rect::new(0, 0, 40, 6));
        app.last_knowledge_graph_rect = None;
        app.last_knowledge_list_rect = None;
        app.last_graph_face_rect = None;
        app.sync_scroll_offset = 5;
        app.knowledge_list_scroll = 2;

        // Row 15 is inside the panel (0..20) but outside the activity rect
        // (0..6) and no other region is recorded this frame.
        let consumed =
            handle_sync_panel_scroll(&mut app, &mouse(MouseEventKind::ScrollDown, 5, 15), -1);

        assert!(
            consumed,
            "a wheel event inside the panel is always consumed"
        );
        assert_eq!(app.sync_scroll_offset, 5);
        assert_eq!(app.knowledge_list_scroll, 2);
        assert!(app.panel_interacting);
    }

    /// Pointer inside the knowledge-graph rect moves only the graph offset,
    /// clamped to the edge count (functional requirement 5: never blank).
    #[test]
    fn scroll_inside_knowledge_graph_rect_clamps_to_edge_count() {
        let mut app = test_app();
        app.last_sync_area = Some(ratatui::layout::Rect::new(0, 0, 40, 20));
        app.last_knowledge_graph_rect = Some(ratatui::layout::Rect::new(0, 6, 40, 3));
        app.project_graph_edges = (0..4)
            .map(|i| crate::tui::app::types::ProjectGraphEdge {
                from_name: format!("a{i}"),
                to_name: format!("b{i}"),
                from_hash: format!("ha{i}"),
                to_hash: format!("hb{i}"),
                relation: "relates_to".to_string(),
            })
            .collect();

        // Scroll down repeatedly past the end: must clamp, not grow forever.
        for _ in 0..10 {
            handle_sync_panel_scroll(&mut app, &mouse(MouseEventKind::ScrollDown, 5, 7), -1);
        }

        // 4 edges, 3 visible rows -> max_offset = 1.
        assert_eq!(app.knowledge_graph_scroll, 1);
    }
}

// ── CT29: the pointer seeds the brain; clicks and scroll do not ───────────
#[cfg(test)]
mod brain_seed_tests {
    use super::*;
    use crate::db::Database;
    use crate::tui::brians_brain::{BriansBrain, CellState};
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_app() -> App {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).expect("create test db"));
        let data_dir = tempdir().expect("create data dir");
        App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app")
    }

    fn all_off_brain(rows: usize, cols: usize) -> BriansBrain {
        let mut brain = BriansBrain::new(rows, cols);
        for cell in brain.grid.iter_mut() {
            *cell = CellState::Off;
        }
        brain
    }

    fn app_with_home_brain() -> App {
        let mut app = test_app();
        app.home_brain = Some(all_off_brain(10, 20));
        app.home_brain_rect = Some(Rect::new(5, 5, 20, 10));
        app
    }

    fn on_count(app: &App) -> usize {
        app.home_brain.as_ref().map_or(0, |brain| {
            brain
                .grid
                .iter()
                .filter(|cell| **cell == CellState::On)
                .count()
        })
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn moved_event_seeds_home_brain() {
        let mut app = app_with_home_brain();
        dispatch_event(&mut app, Event::Mouse(mouse(MouseEventKind::Moved, 8, 7))).unwrap();
        assert!(
            on_count(&app) > 0,
            "a Moved event inside the home brain's rect must seed it"
        );
    }

    #[test]
    fn click_event_does_not_seed() {
        let mut app = app_with_home_brain();
        dispatch_event(
            &mut app,
            Event::Mouse(mouse(MouseEventKind::Down(MouseButton::Left), 8, 7)),
        )
        .unwrap();
        assert_eq!(on_count(&app), 0, "a click must not seed the brain");
    }

    #[test]
    fn moved_event_outside_rect_changes_nothing() {
        let mut app = app_with_home_brain();
        dispatch_event(
            &mut app,
            Event::Mouse(mouse(MouseEventKind::Moved, 100, 100)),
        )
        .unwrap();
        assert_eq!(
            on_count(&app),
            0,
            "a move outside both brain rects must change nothing"
        );
    }
}

// ── Firefly Catcher: the click itself is hit-tested ────────────────────
#[cfg(test)]
mod firefly_click_tests {
    use super::*;
    use crate::db::Database;
    use crate::tui::gamification::MissionEvent;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn night_app_with_fireflies() -> App {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).expect("create test db"));
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        // Seed the night scene so live fireflies exist to click on.
        app.atmosphere_ctx.hour = 23;
        app.atmosphere.tick(
            ratatui::layout::Rect::new(0, 0, 80, 24),
            &mut app.atmosphere_ctx,
        );
        app.atmosphere_hidden = false;
        app
    }

    fn find_hit_cell(app: &App) -> (u16, u16) {
        for row in 0..24u16 {
            for col in 0..80u16 {
                if app.atmosphere.catch_at(col, row) {
                    return (col, row);
                }
            }
        }
        panic!("seeded night scene must contain a live firefly");
    }

    fn click(app: &mut App, col: u16, row: u16) {
        dispatch_event(
            app,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: col,
                row,
                modifiers: KeyModifiers::NONE,
            }),
        )
        .unwrap();
    }

    #[test]
    fn firefly_click_near_queues_caught() {
        let mut app = night_app_with_fireflies();
        let (col, row) = find_hit_cell(&app);
        click(&mut app, col, row);
        assert!(
            app.mission_pending_events
                .contains(&MissionEvent::FireflyCaught),
            "a click within 1.5 cells of a live firefly must queue FireflyCaught"
        );
    }

    #[test]
    fn firefly_click_far_queues_nothing() {
        let mut app = night_app_with_fireflies();
        let (hit_col, hit_row) = find_hit_cell(&app);
        // Three cells away from the hit: outside the 1.5-cell catch range.
        let miss = [
            (hit_col.saturating_add(3), hit_row),
            (hit_col.saturating_sub(3), hit_row),
            (hit_col, hit_row.saturating_add(3)),
            (hit_col, hit_row.saturating_sub(3)),
        ]
        .into_iter()
        .find(|(col, row)| !app.atmosphere.catch_at(*col, *row))
        .expect("a cell 3 away from the hit must miss every fly");
        click(&mut app, miss.0, miss.1);
        assert!(
            !app.mission_pending_events
                .contains(&MissionEvent::FireflyCaught),
            "a click 3 cells away from every live firefly must not queue FireflyCaught"
        );
    }
}
