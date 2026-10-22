use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::tui::app::types::{AgentEntry, App, Focus, GraphLiveFocus, ProjectTab, SidebarLayer};

use super::project_filter::preview_project_filter_key;

// ── Home: screensaver — arrows enter Preview ────────────────────────

// ── Home: screensaver — arrows enter Preview ────────────────────────

pub fn handle_home_key(app: &mut App, code: KeyCode, _modifiers: KeyModifiers) -> Result<()> {
    // Quit-confirmation overlay intercepts all keys
    if app.quit_confirm {
        match code {
            KeyCode::Char('y') | KeyCode::Enter => app.running = false,
            _ => app.quit_confirm = false,
        }
        return Ok(());
    }

    let has_sidebar_preview = !app.agents.is_empty()
        || !app.projects.is_empty()
        || !app.visible_graphs().is_empty()
        || app.rag_info.has_rag_activity();

    match code {
        KeyCode::F(10) => {
            if app.focus == Focus::Agent {
                app.focus = Focus::Preview;
            } else if app.focus == Focus::Preview {
                app.focus = Focus::Home;
            } else {
                app.quit_confirm = true;
            }
        }
        KeyCode::Esc => {
            if app.focus == Focus::Agent {
                app.focus = Focus::Preview;
            } else if app.focus == Focus::Preview {
                app.focus = Focus::Home;
            } else {
                app.quit_confirm = true;
            }
        }
        KeyCode::F(1) => {
            app.open_hub();
        }
        KeyCode::Down | KeyCode::Char('j') if has_sidebar_preview => {
            app.dismiss_brain();
            app.focus_sidebar_from_edge(true);
            app.log_scroll = 0;
            app.focus = Focus::Preview;
        }
        KeyCode::Up | KeyCode::Char('k') if has_sidebar_preview => {
            app.dismiss_brain();
            app.focus_sidebar_from_edge(false);
            app.log_scroll = 0;
            app.focus = Focus::Preview;
        }
        KeyCode::Enter if has_sidebar_preview => {
            app.dismiss_brain();
            app.log_scroll = 0;
            app.focus = Focus::Preview;
        }
        KeyCode::Char('n') => app.open_new_agent_dialog(),
        _ => {}
    }
    Ok(())
}

// ── Preview: navigate agents, Enter → Focus ─────────────────────────

pub fn handle_preview_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    if let Some(res) = preview_modal_intercept(app, code, modifiers) {
        return res;
    }
    if handle_playground_key(app, code, modifiers) {
        return Ok(());
    }

    if handle_project_relation_dialog_key(app, code) {
        return Ok(());
    }

    // CT36: runs first so a plain letter types into the project type-ahead
    // instead of triggering its command (j/k/h/l, R) on that one list.
    if preview_project_filter_key(app, code, modifiers) {
        return Ok(());
    }

    let on_graph = app.sidebar_layer == SidebarLayer::Automation
        && app.automation_kind == crate::tui::app::AutomationKind::Graph;

    if preview_frame_shift_nav_key(app, code, modifiers) {
        return Ok(());
    }
    if preview_esc_graph_key(app, code, on_graph) {
        return Ok(());
    }
    if preview_enter_key(app, code, on_graph) {
        return Ok(());
    }
    if preview_plain_scroll_key(app, code, on_graph) {
        return Ok(());
    }
    if preview_tail_pane_key(app, code, on_graph) {
        return Ok(());
    }
    if preview_tail_toggle_key(app, code, on_graph, modifiers) {
        return Ok(());
    }
    if preview_manual_nav_key(app, code, on_graph) {
        return Ok(());
    }
    if preview_strip_spec_key(app, code, on_graph) {
        return Ok(());
    }
    if preview_edit_key(app, code, on_graph) {
        return Ok(());
    }
    if preview_graph_control_key(app, code, on_graph) {
        return Ok(());
    }
    if preview_agent_misc_key(app, code, on_graph) {
        return Ok(());
    }
    if preview_graph_meta_key(app, code, modifiers, on_graph) {
        return Ok(());
    }
    if preview_function_key(app, code, modifiers, on_graph) {
        return Ok(());
    }
    if preview_esc_home_key(app, code) {
        return Ok(());
    }
    Ok(())
}

// ── Preview key-group helpers (extracted from handle_preview_key) ───────

fn preview_modal_intercept(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Option<Result<()>> {
    if app.delete_project_confirm {
        return Some(preview_delete_project_confirm_key(app, code));
    }
    if app.archive_graph_confirm {
        return Some(preview_archive_graph_confirm_key(app, code));
    }
    if app.permanent_delete_graph_confirm {
        return Some(preview_permanent_delete_graph_confirm_key(app, code));
    }
    if app.graph_reset_confirm {
        return Some(preview_graph_reset_confirm_key(app, code));
    }
    if app.graph_autorun_dialog.is_some() {
        return Some(preview_graph_autorun_dialog_key(app, code, modifiers));
    }
    None
}

fn preview_delete_project_confirm_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Char('y') | KeyCode::Enter => {
            let _ = app.delete_selected_project();
            app.delete_project_confirm = false;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.delete_project_confirm = false;
        }
        _ => {}
    }
    Ok(())
}

fn preview_archive_graph_confirm_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Char('y') | KeyCode::Enter => {
            let _ = app.archive_selected_graph();
            app.archive_graph_confirm = false;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.archive_graph_confirm = false;
        }
        _ => {}
    }
    Ok(())
}

fn preview_permanent_delete_graph_confirm_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Char('y') | KeyCode::Enter => {
            let _ = app.permanent_delete_selected_archived_graph();
            app.permanent_delete_graph_confirm = false;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.permanent_delete_graph_confirm = false;
        }
        _ => {}
    }
    Ok(())
}

fn preview_graph_reset_confirm_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Char('y') | KeyCode::Enter => {
            app.confirm_reset_selected_graph();
            app.graph_reset_confirm = false;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.graph_reset_confirm = false;
        }
        _ => {}
    }
    Ok(())
}

fn preview_graph_autorun_dialog_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<()> {
    use crate::tui::app::dialog::GraphAutorunMode;
    match code {
        KeyCode::Esc => app.close_graph_autorun_dialog(),
        KeyCode::Enter => app.submit_graph_autorun_dialog(),
        KeyCode::Tab | KeyCode::BackTab => {
            if let Some(dialog) = app.graph_autorun_dialog.as_mut() {
                dialog.toggle_mode();
            }
        }
        KeyCode::Up
            if app.graph_autorun_dialog.as_ref().map(|d| d.mode)
                == Some(GraphAutorunMode::Picker) =>
        {
            if let Some(dialog) = app.graph_autorun_dialog.as_mut() {
                dialog.picker.adjust(1);
                dialog.error = None;
            }
        }
        KeyCode::Down
            if app.graph_autorun_dialog.as_ref().map(|d| d.mode)
                == Some(GraphAutorunMode::Picker) =>
        {
            if let Some(dialog) = app.graph_autorun_dialog.as_mut() {
                dialog.picker.adjust(-1);
                dialog.error = None;
            }
        }
        KeyCode::Left
            if app.graph_autorun_dialog.as_ref().map(|d| d.mode)
                == Some(GraphAutorunMode::Picker) =>
        {
            if let Some(dialog) = app.graph_autorun_dialog.as_mut() {
                dialog.picker.move_field(-1);
            }
        }
        KeyCode::Right
            if app.graph_autorun_dialog.as_ref().map(|d| d.mode)
                == Some(GraphAutorunMode::Picker) =>
        {
            if let Some(dialog) = app.graph_autorun_dialog.as_mut() {
                dialog.picker.move_field(1);
            }
        }
        KeyCode::Backspace => {
            if let Some(dialog) = app.graph_autorun_dialog.as_mut() {
                if dialog.mode == GraphAutorunMode::QuotaMessage {
                    dialog.quota_input.pop();
                }
            }
        }
        KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(dialog) = app.graph_autorun_dialog.as_mut() {
                match dialog.mode {
                    GraphAutorunMode::Picker => {
                        if let Some(digit) = c.to_digit(10) {
                            dialog.picker.type_digit(digit);
                            dialog.error = None;
                        }
                    }
                    GraphAutorunMode::QuotaMessage => {
                        dialog.quota_input.push(c);
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn graph_view_owns_plain_arrows(app: &App, on_graph: bool) -> bool {
    on_graph && app.graph_live_focus == GraphLiveFocus::Graph && !app.graph_live_follow
}

fn preview_frame_shift_nav_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if !modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    match code {
        KeyCode::Down => {
            app.select_next();
        }
        KeyCode::Up => {
            app.select_prev();
        }
        _ => return false,
    }
    true
}

fn preview_esc_graph_key(app: &mut App, code: KeyCode, on_graph: bool) -> bool {
    if code != KeyCode::Esc || !on_graph {
        return false;
    }
    if app.node_tail_dialog_active() {
        app.close_node_tail_dialog();
        return true;
    }
    if app.graph_live_focus == GraphLiveFocus::SpecStrip {
        app.graph_live_focus = GraphLiveFocus::Graph;
        app.graph_spec_strip_selected = None;
        return true;
    }
    if !app.graph_live_follow {
        app.graph_live_reset_follow();
        return true;
    }
    false
}

fn preview_esc_home_key(app: &mut App, code: KeyCode) -> bool {
    if matches!(code, KeyCode::Esc | KeyCode::Char('h')) {
        app.focus = Focus::Home;
        return true;
    }
    false
}

fn preview_enter_key(app: &mut App, code: KeyCode, on_graph: bool) -> bool {
    if code == KeyCode::Enter && on_graph {
        // CT28: Enter in follow mode enters manual mode (CT24). Enter in
        // manual mode with the pane open closes it; otherwise opens the pane
        // on the highlighted node.
        if app.graph_live_follow {
            app.graph_live_enter();
        } else if app.node_tail_dialog_active() {
            app.close_node_tail_dialog();
        } else {
            app.open_node_tail_pane();
        }
        return true;
    }
    if !matches!(code, KeyCode::Enter | KeyCode::Char('l')) {
        return false;
    }
    if app.agents_rag_focused {
        app.activate_playground();
        return true;
    }
    if app.sidebar_layer == SidebarLayer::Knowledge {
        // CT36: with a type-ahead that matches nothing there is no project to
        // enter, so Enter is a no-op rather than entering a hidden one.
        if !app
            .visible_project_indices()
            .contains(&app.selected_project)
        {
            return true;
        }
        app.enter_project_focus(ProjectTab::Overview);
        app.focus = Focus::Agent;
        return true;
    }
    if on_graph {
        let _ = app.open_graph_editor_dialog();
        return true;
    }
    if let Some(AgentEntry::Group(idx)) = app.selected_agent() {
        let idx = *idx;
        if let Some(group) = app.split_groups.get(idx) {
            let id = group.id.clone();
            app.active_split_id = Some(id);
            app.split_right_focused = false;
        }
        app.focus = Focus::Agent;
        return true;
    }
    app.log_scroll = 0;
    app.focus = Focus::Agent;
    true
}

fn preview_plain_scroll_key(app: &mut App, code: KeyCode, on_graph: bool) -> bool {
    if !matches!(
        code,
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Up | KeyCode::Char('k')
    ) {
        return false;
    }
    if graph_view_owns_plain_arrows(app, on_graph) {
        match code {
            KeyCode::Down | KeyCode::Char('j') => app.graph_live_navigate_sibling(true),
            KeyCode::Up | KeyCode::Char('k') => app.graph_live_navigate_sibling(false),
            _ => unreachable!(),
        }
        return true;
    }
    match code {
        KeyCode::Down | KeyCode::Char('j') => app.select_next(),
        KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
        _ => unreachable!(),
    }
    true
}

fn preview_tail_toggle_key(
    app: &mut App,
    code: KeyCode,
    on_graph: bool,
    modifiers: KeyModifiers,
) -> bool {
    if !on_graph || !matches!(code, KeyCode::Char('t')) || modifiers.contains(KeyModifiers::CONTROL)
    {
        return false;
    }
    if app.node_tail_dialog_active() {
        app.close_node_tail_dialog();
    } else {
        app.open_node_tail_pane();
    }
    true
}

/// CT28: pane-specific keys while the pane is open on the graph view.
/// `PageUp`/`PageDown` scroll by half the body height, `End` returns to the
/// live edge, `,`/`.` move the ensemble member cursor.
fn preview_tail_pane_key(app: &mut App, code: KeyCode, on_graph: bool) -> bool {
    if !on_graph || !app.node_tail_dialog_active() {
        return false;
    }
    match code {
        KeyCode::PageUp => {
            let half = app.node_tail_pane_body_height / 2;
            app.node_tail_scroll_by(-(half as isize));
            true
        }
        KeyCode::PageDown => {
            let half = app.node_tail_pane_body_height / 2;
            app.node_tail_scroll_by(half as isize);
            true
        }
        KeyCode::End => {
            app.node_tail_scroll_to_live();
            true
        }
        KeyCode::Char(',') => {
            app.node_tail_member_step(-1);
            true
        }
        KeyCode::Char('.') => {
            app.node_tail_member_step(1);
            true
        }
        _ => false,
    }
}

fn preview_manual_nav_key(app: &mut App, code: KeyCode, on_graph: bool) -> bool {
    if !on_graph || app.graph_live_focus != GraphLiveFocus::Graph || app.graph_live_follow {
        return false;
    }
    match code {
        KeyCode::Right => app.graph_live_navigate_child(),
        KeyCode::Left => app.graph_live_navigate_parent(),
        KeyCode::Down => app.graph_live_navigate_sibling(true),
        KeyCode::Up => app.graph_live_navigate_sibling(false),
        _ => return false,
    }
    true
}

fn preview_strip_spec_key(app: &mut App, code: KeyCode, on_graph: bool) -> bool {
    if !on_graph {
        return false;
    }
    match code {
        KeyCode::Tab | KeyCode::BackTab => {
            app.graph_live_toggle_focus();
        }
        KeyCode::Left if app.graph_live_focus == GraphLiveFocus::SpecStrip => {
            app.graph_spec_strip_move_selection(false);
        }
        KeyCode::Right if app.graph_live_focus == GraphLiveFocus::SpecStrip => {
            app.graph_spec_strip_move_selection(true);
        }
        KeyCode::Char('[') => app.cycle_graph_spec(false),
        KeyCode::Char(']') => app.cycle_graph_spec(true),
        _ => return false,
    }
    true
}

fn preview_edit_key(app: &mut App, code: KeyCode, on_graph: bool) -> bool {
    match code {
        KeyCode::Char('e') if !app.agents_rag_focused => {
            if on_graph {
                let _ = app.open_graph_editor_dialog();
            } else if app.sidebar_layer != SidebarLayer::Knowledge {
                app.open_edit_dialog();
            } else {
                return false;
            }
        }
        KeyCode::Char('d') if on_graph => {
            let _ = app.duplicate_selected_graph_node();
        }
        KeyCode::Char('w') if on_graph => {
            let _ = app.open_graph_edges_dialog();
        }
        KeyCode::Char('d')
            if !app.agents_rag_focused && app.sidebar_layer != SidebarLayer::Knowledge =>
        {
            let _ = app.toggle_enable();
        }
        _ => return false,
    }
    true
}

fn preview_graph_control_key(app: &mut App, code: KeyCode, on_graph: bool) -> bool {
    if !on_graph {
        return false;
    }
    match code {
        KeyCode::Char('r') => app.run_selected_graph(),
        KeyCode::Char('p') => app.pause_selected_graph(),
        KeyCode::Char('c') => app.continue_selected_graph_retry(),
        KeyCode::Char('C') => app.continue_selected_graph_skip(),
        KeyCode::Char('x') => app.open_graph_reset_confirm(),
        KeyCode::Char('a') => app.open_graph_autorun_dialog(),
        _ => return false,
    }
    true
}

fn preview_agent_misc_key(app: &mut App, code: KeyCode, on_graph: bool) -> bool {
    if !matches!(
        code,
        KeyCode::Char('r') | KeyCode::Char('p') | KeyCode::Char('n')
    ) {
        return false;
    }
    if !app.agents_rag_focused
        && app.sidebar_layer != SidebarLayer::Knowledge
        && matches!(code, KeyCode::Char('r'))
    {
        let _ = app.rerun_selected();
    } else if app.agents_rag_focused && matches!(code, KeyCode::Char('p')) {
        app.toggle_rag_pause();
    } else if matches!(code, KeyCode::Char('n')) {
        if on_graph {
            app.open_new_graph_dialog();
        } else if app.sidebar_layer != SidebarLayer::Knowledge {
            app.open_new_agent_dialog();
        } else {
            return false;
        }
    } else {
        return false;
    }
    true
}

fn preview_graph_meta_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    on_graph: bool,
) -> bool {
    match code {
        // CT36: moved off plain `R`, which is now a type-ahead character on
        // the project list.
        KeyCode::Char('r')
            if modifiers.contains(KeyModifiers::CONTROL)
                && app.sidebar_layer == SidebarLayer::Knowledge =>
        {
            // CT36: a type-ahead that hides the selected project must not
            // relate that hidden project — the keypress is consumed, nothing opens.
            if app
                .visible_project_indices()
                .contains(&app.selected_project)
            {
                let _ = app.open_project_relation_dialog();
            }
        }
        KeyCode::Char('R') if on_graph && app.graph_view_archived => {
            let _ = app.restore_selected_archived_graph();
        }
        KeyCode::Char('E') if on_graph => app.open_edit_graph_dialog(),
        KeyCode::Char('A') if on_graph => app.toggle_graph_archive_view(),
        _ => return false,
    }
    true
}

fn preview_function_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    on_graph: bool,
) -> bool {
    if !matches!(
        code,
        KeyCode::Char('t') | KeyCode::F(4) | KeyCode::F(10) | KeyCode::F(1)
    ) {
        return false;
    }
    match code {
        KeyCode::Char('t') if modifiers.contains(KeyModifiers::CONTROL) => {
            if matches!(
                app.selected_agent(),
                Some(AgentEntry::Interactive(_)) | Some(AgentEntry::Terminal(_))
            ) {
                app.open_context_transfer_modal();
            }
        }
        KeyCode::F(4) => {
            if app.sidebar_layer == SidebarLayer::Knowledge {
                app.delete_project_confirm = true;
            } else if on_graph && app.graph_view_archived {
                app.permanent_delete_graph_confirm = true;
            } else if on_graph {
                app.archive_graph_confirm = true;
            } else if !app.agents_rag_focused {
                let _ = app.delete_selected();
            }
        }
        KeyCode::F(10) => app.focus = Focus::Home,
        KeyCode::F(1) => app.open_hub(),
        _ => return false,
    }
    true
}

pub(super) fn handle_playground_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if !app.playground_active {
        return false;
    }

    if app.playground_detail_mode {
        match code {
            KeyCode::Esc | KeyCode::F(10) => {
                app.playground_detail_mode = false;
                app.playground_scroll = 0;
            }
            KeyCode::Up if modifiers.contains(KeyModifiers::SHIFT) => {
                app.deactivate_playground();
                if app.focus == Focus::Agent {
                    app.prev_interactive();
                } else {
                    app.select_prev();
                }
            }
            KeyCode::Down if modifiers.contains(KeyModifiers::SHIFT) => {
                app.deactivate_playground();
                if app.focus == Focus::Agent {
                    app.next_interactive();
                } else {
                    app.select_next();
                }
            }
            KeyCode::Up => {
                app.playground_scroll = app.playground_scroll.saturating_sub(3);
            }
            KeyCode::Down => {
                app.playground_scroll = app.playground_scroll.saturating_add(3);
            }
            KeyCode::Char('t') if modifiers.contains(KeyModifiers::CONTROL) => {
                app.open_rag_transfer_modal();
            }
            _ => {}
        }
        return true;
    }

    match code {
        KeyCode::F(10) => {
            app.deactivate_playground();
            app.focus = Focus::Preview;
        }
        KeyCode::Up if modifiers.contains(KeyModifiers::SHIFT) => {
            app.deactivate_playground();
            if app.focus == Focus::Agent {
                app.prev_interactive();
            } else {
                app.select_prev();
            }
        }
        KeyCode::Down if modifiers.contains(KeyModifiers::SHIFT) => {
            app.deactivate_playground();
            if app.focus == Focus::Agent {
                app.next_interactive();
            } else {
                app.select_next();
            }
        }
        KeyCode::Up | KeyCode::Down if !app.playground_results.is_empty() => {
            app.playground_selected = crate::tui::selection::move_index(
                app.playground_selected,
                app.playground_results.len(),
                matches!(code, KeyCode::Down),
            );
        }
        KeyCode::Enter | KeyCode::Char('l') => {
            if app.playground_last_executed_query != app.playground_query.trim() {
                app.playground_search_pending = true;
                app.playground_last_search =
                    std::time::Instant::now() - std::time::Duration::from_secs(1);
            } else if !app.playground_results.is_empty() {
                app.playground_detail_mode = true;
                app.playground_scroll = 0;
            }
        }
        KeyCode::Backspace => {
            app.playground_query.pop();
            if app.playground_query.is_empty() {
                app.playground_results.clear();
                app.playground_selected = 0;
                app.playground_last_executed_query.clear();
            }
            // Deleting does not trigger auto-search.
            app.playground_search_pending = false;
            app.playground_last_search = std::time::Instant::now();
        }
        KeyCode::Char('t') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.open_rag_transfer_modal();
        }
        KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => {
            app.playground_query.push(c);
            app.playground_last_search = std::time::Instant::now();
            app.playground_search_pending = true;
        }
        _ => {}
    }

    true
}

// ── Project Relation Dialog ──────────────────────────────────────

fn handle_project_relation_dialog_key(app: &mut App, code: KeyCode) -> bool {
    if !matches!(app.focus, Focus::ProjectRelationDialog) {
        return false;
    }
    let Some(dialog) = app.project_relation_dialog.as_mut() else {
        return false;
    };

    match code {
        KeyCode::Esc => {
            app.close_project_relation_dialog();
        }
        KeyCode::Enter => {
            let _ = app.confirm_project_relation();
        }
        KeyCode::Up | KeyCode::Char('k') => dialog.move_up(),
        KeyCode::Down | KeyCode::Char('j') => dialog.move_down(),
        KeyCode::Left | KeyCode::Char('h') => dialog.cycle_relation(false),
        KeyCode::Right | KeyCode::Char('l') => dialog.cycle_relation(true),
        KeyCode::Backspace => {
            dialog.filter_buffer.pop();
            dialog.rebuild_filtered();
        }
        KeyCode::Char(c) if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' => {
            dialog.filter_buffer.push(c);
            dialog.rebuild_filtered();
        }
        _ => return true,
    }
    true
}

// ── Focus: PTY interaction or log scroll ────────────────────────────

#[cfg(test)]
mod playground_key_tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::models::{Agent, Cli, Trigger};
    use crate::tui::app::types::App;
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
            log_path: "/tmp/test-playground.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: crate::domain::models::AgentAction::Headless,
        }
    }

    fn app_with_agents() -> App {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
        app.selected = 0;
        app
    }

    #[test]
    fn playground_inactive_returns_false() {
        let mut app = app_with_agents();
        app.playground_active = false;
        assert!(!handle_playground_key(
            &mut app,
            KeyCode::Char('a'),
            KeyModifiers::NONE
        ));
    }

    #[test]
    fn playground_active_returns_true_for_any_key() {
        let mut app = app_with_agents();
        app.playground_active = true;
        assert!(handle_playground_key(
            &mut app,
            KeyCode::Char('a'),
            KeyModifiers::NONE
        ));
    }

    #[test]
    fn playground_f10_deactivates() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_selected = 2;
        handle_playground_key(&mut app, KeyCode::F(10), KeyModifiers::NONE);
        assert!(!app.playground_active);
        assert!(matches!(app.focus, Focus::Preview));
    }

    #[test]
    fn playground_detail_mode_esc_closes_detail() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_detail_mode = true;
        app.playground_scroll = 5;
        handle_playground_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.playground_detail_mode);
        assert_eq!(app.playground_scroll, 0);
        assert!(app.playground_active, "should stay active");
    }

    #[test]
    fn playground_detail_mode_f10_closes_detail() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_detail_mode = true;
        handle_playground_key(&mut app, KeyCode::F(10), KeyModifiers::NONE);
        assert!(!app.playground_detail_mode);
    }

    #[test]
    fn playground_up_navigates_results() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_selected = 2;
        app.playground_results = (0..4)
            .map(|i| crate::rag::vector_store::SearchResult {
                id: format!("id{i}"),
                file_path: format!("r{i}"),
                content: format!("r{i}"),
                created_at: 0,
                distance: None,
            })
            .collect();
        handle_playground_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.playground_selected, 1);
    }

    #[test]
    fn playground_up_at_zero_stays() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_selected = 0;
        app.playground_results = vec![crate::rag::vector_store::SearchResult {
            id: "id0".into(),
            file_path: "r0".into(),
            content: "r0".into(),
            created_at: 0,
            distance: None,
        }];
        handle_playground_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.playground_selected, 0);
    }

    #[test]
    fn playground_down_navigates_results() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_selected = 0;
        app.playground_results = (0..3)
            .map(|i| crate::rag::vector_store::SearchResult {
                id: format!("id{i}"),
                file_path: format!("r{i}"),
                content: format!("r{i}"),
                created_at: 0,
                distance: None,
            })
            .collect();
        handle_playground_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.playground_selected, 1);
    }

    #[test]
    fn playground_down_at_end_stays() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_results = vec![crate::rag::vector_store::SearchResult {
            id: "id0".into(),
            file_path: "r0".into(),
            content: "r0".into(),
            created_at: 0,
            distance: None,
        }];
        app.playground_selected = 0;
        handle_playground_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.playground_selected, 0);
    }

    #[test]
    fn playground_backspace_removes_char() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_query = "hello".into();
        handle_playground_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(app.playground_query, "hell");
        assert!(!app.playground_search_pending);
    }

    #[test]
    fn playground_backspace_empty_clears_results() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_query.clear();
        app.playground_results = vec![crate::rag::vector_store::SearchResult {
            id: "id1".into(),
            file_path: "r1".into(),
            content: "r1".into(),
            created_at: 0,
            distance: None,
        }];
        app.playground_selected = 2;
        handle_playground_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert!(app.playground_results.is_empty());
        assert_eq!(app.playground_selected, 0);
    }

    #[test]
    fn playground_char_appends_to_query() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_query.clear();
        handle_playground_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.playground_query, "x");
        assert!(app.playground_search_pending);
    }

    #[test]
    fn playground_ctrl_char_ignored() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_query.clear();
        handle_playground_key(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert!(app.playground_query.is_empty());
    }

    #[test]
    fn playground_detail_shift_up_deactivates() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_detail_mode = true;
        app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
        app.selected = 0;
        app.focus = Focus::Agent;
        handle_playground_key(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
        assert!(!app.playground_active);
    }

    #[test]
    fn playground_detail_shift_down_deactivates() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_detail_mode = true;
        app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
        app.selected = 0;
        app.focus = Focus::Agent;
        handle_playground_key(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        assert!(!app.playground_active);
    }

    #[test]
    fn playground_non_detail_shift_up_deactivates() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_detail_mode = false;
        app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
        app.selected = 0;
        app.focus = Focus::Agent;
        handle_playground_key(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
        assert!(!app.playground_active);
    }

    #[test]
    fn playground_detail_scroll_down() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_detail_mode = true;
        app.playground_scroll = 0;
        handle_playground_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.playground_scroll, 3);
    }

    #[test]
    fn playground_detail_scroll_up() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_detail_mode = true;
        app.playground_scroll = 5;
        handle_playground_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.playground_scroll, 2);
    }

    #[test]
    fn playground_detail_scroll_up_saturates() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_detail_mode = true;
        app.playground_scroll = 1;
        handle_playground_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.playground_scroll, 0);
    }

    #[test]
    fn playground_enter_triggers_search_when_query_changed() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_query = "test".into();
        app.playground_last_executed_query = "other".into();
        handle_playground_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.playground_search_pending);
    }

    #[test]
    fn playground_enter_opens_detail_when_results_match() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_results = vec![crate::rag::vector_store::SearchResult {
            id: "id1".into(),
            file_path: "r1".into(),
            content: "r1".into(),
            created_at: 0,
            distance: None,
        }];
        app.playground_query = "r1".into();
        app.playground_last_executed_query = "r1".into();
        handle_playground_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.playground_detail_mode);
    }

    #[test]
    fn playground_ctrl_t_opens_rag_transfer() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_results = vec![crate::rag::vector_store::SearchResult {
            id: "id1".into(),
            file_path: "r1".into(),
            content: "r1".into(),
            created_at: 0,
            distance: None,
        }];
        handle_playground_key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(app.rag_transfer_modal.is_some());
    }

    #[test]
    fn playground_detail_ctrl_t_opens_rag_transfer() {
        let mut app = app_with_agents();
        app.playground_active = true;
        app.playground_detail_mode = true;
        app.playground_results = vec![crate::rag::vector_store::SearchResult {
            id: "id1".into(),
            file_path: "r1".into(),
            content: "r1".into(),
            created_at: 0,
            distance: None,
        }];
        handle_playground_key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(app.rag_transfer_modal.is_some());
    }
}

#[cfg(test)]
mod home_key_tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::models::{Agent, Cli, Trigger};
    use crate::tui::app::types::App;
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
            log_path: "/tmp/test-home.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: crate::domain::models::AgentAction::Headless,
        }
    }

    fn app_with_agents() -> App {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
        app.selected = 0;
        app
    }

    #[test]
    fn home_f10_shows_quit_confirm() {
        let mut app = app_with_agents();
        app.focus = Focus::Home;
        handle_home_key(&mut app, KeyCode::F(10), KeyModifiers::NONE).unwrap();
        assert!(app.quit_confirm);
        assert!(app.running);
    }

    #[test]
    fn home_quit_confirm_y_exits() {
        let mut app = app_with_agents();
        app.focus = Focus::Home;
        app.quit_confirm = true;
        handle_home_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE).unwrap();
        assert!(!app.running);
    }

    #[test]
    fn home_quit_confirm_enter_exits() {
        let mut app = app_with_agents();
        app.focus = Focus::Home;
        app.quit_confirm = true;
        handle_home_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(!app.running);
    }

    #[test]
    fn home_quit_confirm_other_key_cancels() {
        let mut app = app_with_agents();
        app.focus = Focus::Home;
        app.quit_confirm = true;
        handle_home_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
        assert!(!app.quit_confirm);
        assert!(app.running);
    }

    #[test]
    fn home_down_moves_to_preview() {
        let mut app = app_with_agents();
        app.focus = Focus::Home;
        handle_home_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();
        assert!(matches!(app.focus, Focus::Preview));
    }

    #[test]
    fn home_up_moves_to_preview() {
        let mut app = app_with_agents();
        app.focus = Focus::Home;
        handle_home_key(&mut app, KeyCode::Up, KeyModifiers::NONE).unwrap();
        assert!(matches!(app.focus, Focus::Preview));
    }

    #[test]
    fn home_j_moves_to_preview() {
        let mut app = app_with_agents();
        app.focus = Focus::Home;
        handle_home_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE).unwrap();
        assert!(matches!(app.focus, Focus::Preview));
    }

    #[test]
    fn home_k_moves_to_preview() {
        let mut app = app_with_agents();
        app.focus = Focus::Home;
        handle_home_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE).unwrap();
        assert!(matches!(app.focus, Focus::Preview));
    }

    #[test]
    fn home_enter_moves_to_preview() {
        let mut app = app_with_agents();
        app.focus = Focus::Home;
        handle_home_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(matches!(app.focus, Focus::Preview));
    }

    #[test]
    fn home_n_opens_new_agent_dialog() {
        let mut app = app_with_agents();
        app.focus = Focus::Home;
        handle_home_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
        assert!(matches!(app.focus, Focus::NewAgentDialog));
    }

    #[test]
    fn home_f1_shows_legend() {
        let mut app = app_with_agents();
        app.focus = Focus::Home;
        app.hub_open = false;
        handle_home_key(&mut app, KeyCode::F(1), KeyModifiers::NONE).unwrap();
        assert!(app.hub_open);
    }

    #[test]
    fn home_esc_shows_quit_confirm() {
        let mut app = app_with_agents();
        app.focus = Focus::Home;
        handle_home_key(&mut app, KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(app.quit_confirm);
    }
}

#[cfg(test)]
mod preview_key_tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::models::{Agent, Cli, Trigger};
    use crate::tui::app::types::App;
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
            log_path: "/tmp/test-preview.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: crate::domain::models::AgentAction::Headless,
        }
    }

    fn app_with_agents() -> App {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
        app.selected = 0;
        app
    }

    #[test]
    fn preview_esc_goes_home() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        handle_preview_key(&mut app, KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(matches!(app.focus, Focus::Home));
    }

    #[test]
    fn preview_h_goes_home() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        handle_preview_key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE).unwrap();
        assert!(matches!(app.focus, Focus::Home));
    }

    #[test]
    fn preview_f10_goes_home() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        handle_preview_key(&mut app, KeyCode::F(10), KeyModifiers::NONE).unwrap();
        assert!(matches!(app.focus, Focus::Home));
    }

    #[test]
    fn preview_down_selects_next() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.sidebar_layer = SidebarLayer::Automation;
        app.agents = vec![
            AgentEntry::Agent(cron_agent("a1")),
            AgentEntry::Agent(cron_agent("a2")),
        ];
        app.selected = 0;
        handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn preview_up_selects_prev() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.sidebar_layer = SidebarLayer::Automation;
        app.agents = vec![
            AgentEntry::Agent(cron_agent("a1")),
            AgentEntry::Agent(cron_agent("a2")),
        ];
        app.selected = 1;
        handle_preview_key(&mut app, KeyCode::Up, KeyModifiers::NONE).unwrap();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn preview_j_selects_next() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.sidebar_layer = SidebarLayer::Automation;
        app.agents = vec![
            AgentEntry::Agent(cron_agent("a1")),
            AgentEntry::Agent(cron_agent("a2")),
        ];
        app.selected = 0;
        handle_preview_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE).unwrap();
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn preview_k_selects_prev() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.sidebar_layer = SidebarLayer::Automation;
        app.agents = vec![
            AgentEntry::Agent(cron_agent("a1")),
            AgentEntry::Agent(cron_agent("a2")),
        ];
        app.selected = 1;
        handle_preview_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE).unwrap();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn preview_enter_focuses_agent() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(matches!(app.focus, Focus::Agent));
    }

    #[test]
    fn preview_l_focuses_agent() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        handle_preview_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE).unwrap();
        assert!(matches!(app.focus, Focus::Agent));
    }

    #[test]
    fn preview_f1_shows_legend() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.hub_open = false;
        handle_preview_key(&mut app, KeyCode::F(1), KeyModifiers::NONE).unwrap();
        assert!(app.hub_open);
    }

    #[test]
    fn preview_n_opens_new_agent_dialog() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        handle_preview_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
        assert!(matches!(app.focus, Focus::NewAgentDialog));
    }

    #[test]
    fn preview_delete_confirm_y_deletes() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.delete_project_confirm = true;
        handle_preview_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE).unwrap();
        assert!(!app.delete_project_confirm);
    }

    #[test]
    fn preview_delete_confirm_n_cancels() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.delete_project_confirm = true;
        handle_preview_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
        assert!(!app.delete_project_confirm);
    }

    #[test]
    fn preview_archive_graph_confirm_y_archives() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.archive_graph_confirm = true;
        handle_preview_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE).unwrap();
        assert!(!app.archive_graph_confirm);
    }

    #[test]
    fn preview_archive_graph_confirm_n_cancels() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.archive_graph_confirm = true;
        handle_preview_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
        assert!(!app.archive_graph_confirm);
    }

    #[test]
    fn preview_permanent_delete_graph_confirm_y_deletes() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.permanent_delete_graph_confirm = true;
        handle_preview_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE).unwrap();
        assert!(!app.permanent_delete_graph_confirm);
    }

    #[test]
    fn preview_permanent_delete_graph_confirm_n_cancels() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.permanent_delete_graph_confirm = true;
        handle_preview_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
        assert!(!app.permanent_delete_graph_confirm);
    }

    #[test]
    fn preview_f4_on_graph_opens_archive_confirm_not_permanent_delete() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.sidebar_layer = SidebarLayer::Automation;
        app.automation_kind = crate::tui::app::AutomationKind::Graph;
        app.graph_view_archived = false;
        handle_preview_key(&mut app, KeyCode::F(4), KeyModifiers::NONE).unwrap();
        assert!(app.archive_graph_confirm);
        assert!(!app.permanent_delete_graph_confirm);
    }

    #[test]
    fn preview_f4_in_archived_view_opens_permanent_delete_confirm() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.sidebar_layer = SidebarLayer::Automation;
        app.automation_kind = crate::tui::app::AutomationKind::Graph;
        app.graph_view_archived = true;
        handle_preview_key(&mut app, KeyCode::F(4), KeyModifiers::NONE).unwrap();
        assert!(app.permanent_delete_graph_confirm);
        assert!(!app.archive_graph_confirm);
    }

    #[test]
    fn preview_shift_a_toggles_graph_archive_view() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.sidebar_layer = SidebarLayer::Automation;
        app.automation_kind = crate::tui::app::AutomationKind::Graph;
        assert!(!app.graph_view_archived);

        handle_preview_key(&mut app, KeyCode::Char('A'), KeyModifiers::NONE).unwrap();
        assert!(app.graph_view_archived);

        handle_preview_key(&mut app, KeyCode::Char('A'), KeyModifiers::NONE).unwrap();
        assert!(!app.graph_view_archived);
    }

    #[test]
    fn preview_d_toggles_enable() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
        app.selected = 0;
        let _ = handle_preview_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        // The toggle call may fail on test agents, but the key is consumed
    }

    #[test]
    fn preview_r_reruns_selected() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
        app.selected = 0;
        let _ = handle_preview_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
    }

    #[test]
    fn preview_e_opens_edit_dialog() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
        app.selected = 0;
        let _ = handle_preview_key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    }

    // ── Graph run-time controls ──────────────────────────────────────

    fn graph_with_status(
        status: crate::domain::graphs::GraphStatus,
    ) -> crate::domain::graphs::Graph {
        crate::domain::graphs::Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "lp1".to_string(),
            name: "Nightly review".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        }
    }

    fn app_on_graph(status: crate::domain::graphs::GraphStatus) -> App {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.sidebar_layer = SidebarLayer::Automation;
        app.automation_kind = crate::tui::app::AutomationKind::Graph;
        app.graphs = vec![graph_with_status(status)];
        app.selected_graph_id = Some("lp1".to_string());
        app
    }

    /// The Running-graph live snapshot the CT23/CT24 graph-view key tests
    /// navigate: engine on `n1`, effective nodes `n1 -> n2` (pass). Since the
    /// temp DB has no graph rows, tests that drive Enter/Esc must install it
    /// themselves (a None live state makes Enter a no-op by design — CT24).
    fn running_live_state() -> crate::tui::app::graph_live_state::GraphLiveState {
        crate::tui::app::graph_live_state::GraphLiveState {
            graph_id: "lp1".to_string(),
            graph_name: "Nightly review".to_string(),
            graph_status: crate::domain::graphs::GraphStatus::Running,
            workdir: "/tmp".to_string(),
            trigger_type: "manual".to_string(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            spec_queue: Vec::new(),
            done_count: 0,
            total_count: 0,
            current_spec_id: None,
            effective_nodes: vec![
                crate::domain::graphs::GraphNode {
                    id: "n1".to_string(),
                    spec_id: None,
                    graph_id: Some("lp1".to_string()),
                    name: "Implement".to_string(),
                    kind: crate::domain::graphs::GraphNodeKind::Agent,
                    config: serde_json::json!({}),
                    position: 0,
                    created_at: Utc::now(),
                },
                crate::domain::graphs::GraphNode {
                    id: "n2".to_string(),
                    spec_id: None,
                    graph_id: Some("lp1".to_string()),
                    name: "Review".to_string(),
                    kind: crate::domain::graphs::GraphNodeKind::Check,
                    config: serde_json::json!({}),
                    position: 1,
                    created_at: Utc::now(),
                },
            ],
            effective_edges: vec![crate::domain::graphs::GraphEdge {
                id: "e1".to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                from_node: "n1".to_string(),
                to_node: "n2".to_string(),
                condition: crate::domain::graphs::GraphEdgeCondition::Pass,
            }],
            ensembles: Vec::new(),
            router_taken_routes: std::collections::HashMap::new(),
            current_node_id: Some("n1".to_string()),
            current_node_status: None,
            current_node_started_at: None,
            current_node_iteration: None,
        }
    }

    #[test]
    fn preview_r_on_a_completed_graph_dispatches_run() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Completed);
        handle_preview_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE).unwrap();
        assert!(app.graph_action_pending);
        assert!(app.graph_action_rx.is_some());
    }

    #[test]
    fn preview_r_on_a_running_graph_is_not_bound() {
        // Run is not a valid action for a running graph (decision 3) — 'r'
        // must be a no-op, not a dispatch the daemon then refuses.
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
        handle_preview_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE).unwrap();
        assert!(!app.graph_action_pending);
        assert!(app.graph_action_rx.is_none());
    }

    #[test]
    fn preview_p_on_a_running_graph_dispatches_pause() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
        handle_preview_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE).unwrap();
        assert!(app.graph_action_pending);
    }

    #[test]
    fn preview_p_on_a_non_running_graph_is_not_bound() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Draft);
        handle_preview_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE).unwrap();
        assert!(!app.graph_action_pending);
    }

    #[test]
    fn preview_c_and_shift_c_on_a_paused_graph_dispatch_continue() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Paused);
        handle_preview_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE).unwrap();
        assert!(app.graph_action_pending);

        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Paused);
        handle_preview_key(&mut app, KeyCode::Char('C'), KeyModifiers::SHIFT).unwrap();
        assert!(app.graph_action_pending);
    }

    #[test]
    fn preview_continue_keys_on_a_non_paused_graph_are_not_bound() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
        handle_preview_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE).unwrap();
        assert!(!app.graph_action_pending);
        handle_preview_key(&mut app, KeyCode::Char('C'), KeyModifiers::SHIFT).unwrap();
        assert!(!app.graph_action_pending);
    }

    #[test]
    fn preview_x_on_a_completed_graph_opens_reset_confirm() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Completed);
        handle_preview_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE).unwrap();
        assert!(app.graph_reset_confirm);
    }

    #[test]
    fn preview_x_on_a_running_graph_is_not_bound() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
        handle_preview_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE).unwrap();
        assert!(!app.graph_reset_confirm);
    }

    // ── CT3: tail dialog key bindings ──────────────────────────────

    fn app_with_open_tail() -> App {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
        app.node_tail_dialog = Some(crate::tui::app::dialog::NodeTailDialog {
            run_id: Some("run1".to_string()),
            node_name: "Node node1".to_string(),
            stdout_lines: vec!["out".to_string()],
            stderr_lines: Vec::new(),
            status: Some(crate::domain::graphs::GraphRunStatus::Running),
            timed_out: false,
            started_at: Some(chrono::Utc::now()),
            scroll: 0,
            member_pos: None,
            platform: None,
            model: None,
            member_cursor: 0,
            member_count: 0,
            no_run_spec_name: None,
            last_resolved: None,
        });
        app
    }

    #[test]
    fn preview_t_closes_open_tail_dialog() {
        let mut app = app_with_open_tail();
        handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::NONE).unwrap();
        assert!(!app.node_tail_dialog_active());
    }

    #[test]
    fn preview_esc_closes_open_tail_dialog() {
        let mut app = app_with_open_tail();
        handle_preview_key(&mut app, KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(!app.node_tail_dialog_active());
    }

    #[test]
    fn preview_t_with_no_highlighted_node_opens_the_pane() {
        // CT28: `t` toggles the pane in both modes. With nothing highlighted it
        // still opens (showing the waiting line), superseding CT3 decision 3
        // which refused non-running nodes.
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
        handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::NONE).unwrap();
        assert!(app.node_tail_dialog_active());
        handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::NONE).unwrap();
        assert!(!app.node_tail_dialog_active());
    }

    #[test]
    fn preview_ctrl_t_still_opens_context_transfer_not_tail() {
        // Guard against the plain-`t` arm swallowing Ctrl+T: with no live
        // state the tail cannot open, and the Ctrl+T arm must run instead
        // (no-op here without an interactive agent, but crucially not a
        // tail dialog).
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
        handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL).unwrap();
        assert!(!app.node_tail_dialog_active());
    }

    #[test]
    fn preview_page_up_down_scroll_open_tail_pane() {
        // CT28: Up/Down keep their graph meaning while the pane is open;
        // PageUp/PageDown scroll the pane body by half its height.
        let mut app = app_with_open_tail();
        app.node_tail_pane_body_height = 20;
        let before = app.node_tail_dialog.as_ref().unwrap().scroll;
        handle_preview_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE).unwrap();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().scroll,
            before + 10,
            "PageUp should scroll the pane body back by half height"
        );
        handle_preview_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE).unwrap();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().scroll,
            before,
            "PageDown should scroll forward by half height"
        );
    }

    // ── CT28: pane follows the highlight ───────────────────────────

    /// Seed graph `lp1` + spec `s1` + the given Agent nodes so the pane has
    /// runs to resolve (the temp DB the test App holds is otherwise empty).
    fn seed_live_graph(db: &Database, node_ids: &[&str]) {
        db.insert_graph(&crate::domain::graphs::Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "lp1".to_string(),
            name: "Nightly review".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: crate::domain::graphs::GraphStatus::Running,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        })
        .unwrap();
        db.insert_graph_spec(&crate::domain::graphs::GraphSpec {
            id: "s1".to_string(),
            graph_id: Some("lp1".to_string()),
            name: "Spec".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: crate::domain::graphs::GraphSpecStatus::Running,
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
        })
        .unwrap();
        for (i, id) in node_ids.iter().enumerate() {
            db.insert_graph_node(&crate::domain::graphs::GraphNode {
                id: id.to_string(),
                spec_id: Some("s1".to_string()),
                graph_id: None,
                name: format!("Node {id}"),
                kind: crate::domain::graphs::GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: i as i64,
                created_at: Utc::now(),
            })
            .unwrap();
        }
    }

    fn insert_live_run(
        db: &Database,
        run_id: &str,
        node_id: &str,
        status: crate::domain::graphs::GraphRunStatus,
        started_at: chrono::DateTime<Utc>,
    ) {
        db.insert_graph_run(&crate::domain::graphs::GraphNodeRun {
            id: run_id.to_string(),
            graph_id: "lp1".to_string(),
            spec_id: "s1".to_string(),
            node_id: node_id.to_string(),
            status,
            input: None,
            output: None,
            started_at,
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .unwrap();
    }

    #[test]
    fn follow_mode_node_transition_switches_pane_run_and_resets_scroll() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
        seed_live_graph(&app.db, &["n1", "n2"]);
        insert_live_run(
            &app.db,
            "run-n1",
            "n1",
            crate::domain::graphs::GraphRunStatus::Running,
            Utc::now(),
        );
        insert_live_run(
            &app.db,
            "run-n2",
            "n2",
            crate::domain::graphs::GraphRunStatus::Running,
            Utc::now(),
        );
        let mut state = running_live_state();
        state.current_spec_id = Some("s1".to_string());
        state.current_node_id = Some("n1".to_string());
        app.graph_live_state = Some(state);
        app.graph_live_follow = true;

        app.open_node_tail_pane();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-n1")
        );
        app.node_tail_scroll_by(-7);
        assert_eq!(app.node_tail_dialog.as_ref().unwrap().scroll, 7);

        // The engine moves to the next node: auto-follow retargets the pane.
        app.graph_live_state.as_mut().unwrap().current_node_id = Some("n2".to_string());
        app.poll_node_tail_dialog();
        let dialog = app.node_tail_dialog.as_ref().unwrap();
        assert_eq!(
            dialog.run_id.as_deref(),
            Some("run-n2"),
            "the pane must follow the run to the next node"
        );
        assert_eq!(
            dialog.scroll, 0,
            "a node transition returns the pane to the live edge"
        );
    }

    #[test]
    fn manual_enter_toggles_pane_on_selected_node() {
        // CT28 req 6: Enter in manual mode toggles the pane on the selected
        // node (follow mode keeps CT24's enter-manual behaviour — covered by
        // manual_down_moves_selection_and_pane_follows).
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
        seed_live_graph(&app.db, &["n1"]);
        insert_live_run(
            &app.db,
            "run-n1",
            "n1",
            crate::domain::graphs::GraphRunStatus::Running,
            Utc::now(),
        );
        let mut state = running_live_state();
        state.current_spec_id = Some("s1".to_string());
        state.current_node_id = Some("n1".to_string());
        app.graph_live_state = Some(state);
        app.graph_live_follow = false;
        app.graph_live_selected_node = Some("n1".to_string());

        handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(app.node_tail_dialog_active(), "Enter must open the pane");
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-n1"),
            "the pane must bind to the selected node's run"
        );

        handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(!app.node_tail_dialog_active(), "Enter must close the pane");
    }

    #[test]
    fn manual_down_moves_selection_and_pane_follows() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
        seed_live_graph(&app.db, &["n1", "n2"]);
        insert_live_run(
            &app.db,
            "run-n1",
            "n1",
            crate::domain::graphs::GraphRunStatus::Running,
            Utc::now(),
        );
        insert_live_run(
            &app.db,
            "run-n2",
            "n2",
            crate::domain::graphs::GraphRunStatus::Running,
            Utc::now(),
        );
        let mut state = running_live_state();
        state.current_spec_id = Some("s1".to_string());
        state.current_node_id = Some("n1".to_string());
        app.graph_live_state = Some(state);
        app.graph_live_follow = true;

        // Enter -> manual mode, seeded at the current node n1.
        handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(!app.graph_live_follow);
        app.open_node_tail_pane();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-n1")
        );

        // Down moves the manual selection; the pane follows on the next poll.
        handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();
        assert_eq!(app.graph_live_selected_node.as_deref(), Some("n2"));
        app.poll_node_tail_dialog();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-n2"),
            "in manual mode the pane shows the selected node's run"
        );
    }

    #[test]
    fn ensemble_dot_cycles_through_all_three_members() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
        seed_live_graph(&app.db, &["m1", "m2", "m3", "j1"]);
        let base = Utc::now();
        insert_live_run(
            &app.db,
            "run-m1",
            "m1",
            crate::domain::graphs::GraphRunStatus::Running,
            base,
        );
        insert_live_run(
            &app.db,
            "run-m2",
            "m2",
            crate::domain::graphs::GraphRunStatus::Pass,
            base + chrono::Duration::seconds(9),
        );
        insert_live_run(
            &app.db,
            "run-m3",
            "m3",
            crate::domain::graphs::GraphRunStatus::Pass,
            base + chrono::Duration::seconds(2),
        );

        let mut state = running_live_state();
        state.current_spec_id = Some("s1".to_string());
        state.current_node_id = Some("m1".to_string());
        state.effective_nodes = ["m1", "m2", "m3", "j1"]
            .iter()
            .enumerate()
            .map(|(i, id)| crate::domain::graphs::GraphNode {
                id: id.to_string(),
                spec_id: Some("s1".to_string()),
                graph_id: None,
                name: format!("Node {id}"),
                kind: crate::domain::graphs::GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: i as i64,
                created_at: Utc::now(),
            })
            .collect();
        state.effective_edges = ["m1", "m2", "m3"]
            .iter()
            .map(|m| crate::domain::graphs::GraphEdge {
                id: format!("e-{m}"),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                from_node: (*m).to_string(),
                to_node: "j1".to_string(),
                condition: crate::domain::graphs::GraphEdgeCondition::Always,
            })
            .collect();
        state.ensembles = vec![crate::tui::app::graph_live_state::EnsembleLiveInfo {
            ensemble_id: "e1".to_string(),
            name: "Ensemble".to_string(),
            join_node_id: "j1".to_string(),
            members: ["m1", "m2", "m3"]
                .iter()
                .map(
                    |m| crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
                        node_id: (*m).to_string(),
                        label: (*m).to_string(),
                        status: None,
                    },
                )
                .collect(),
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
        }];
        app.graph_live_state = Some(state);
        app.graph_live_follow = true;

        app.open_node_tail_pane();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-m1"),
            "the running member is shown first"
        );

        handle_preview_key(&mut app, KeyCode::Char('.'), KeyModifiers::NONE).unwrap();
        app.poll_node_tail_dialog();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-m2"),
            "`.` steps to the newest finished member"
        );

        handle_preview_key(&mut app, KeyCode::Char('.'), KeyModifiers::NONE).unwrap();
        app.poll_node_tail_dialog();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-m3"),
            "`.` steps to the older finished member"
        );

        handle_preview_key(&mut app, KeyCode::Char('.'), KeyModifiers::NONE).unwrap();
        app.poll_node_tail_dialog();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-m1"),
            "`.` wraps back to the running member"
        );
    }

    #[test]
    fn graph_reset_confirm_y_dispatches_reset_and_closes_the_modal() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
        app.graph_reset_confirm = true;
        handle_preview_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE).unwrap();
        assert!(!app.graph_reset_confirm);
        assert!(app.graph_action_pending);
    }

    #[test]
    fn graph_reset_confirm_n_cancels_without_dispatching() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
        app.graph_reset_confirm = true;
        handle_preview_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
        assert!(!app.graph_reset_confirm);
        assert!(!app.graph_action_pending);
    }

    #[test]
    fn preview_a_opens_autorun_dialog_regardless_of_status() {
        for status in [
            crate::domain::graphs::GraphStatus::Draft,
            crate::domain::graphs::GraphStatus::Running,
            crate::domain::graphs::GraphStatus::Paused,
            crate::domain::graphs::GraphStatus::Completed,
            crate::domain::graphs::GraphStatus::Failed,
        ] {
            let mut app = app_on_graph(status);
            handle_preview_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE).unwrap();
            assert!(app.graph_autorun_dialog.is_some(), "status {status:?}");
        }
    }

    #[test]
    fn graph_autorun_dialog_quota_message_typing_backspace_and_submit() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
        handle_preview_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE).unwrap();
        // Tab switches into the free-text quota-message mode (the picker is
        // the default mode on open).
        handle_preview_key(&mut app, KeyCode::Tab, KeyModifiers::NONE).unwrap();

        handle_preview_key(&mut app, KeyCode::Char('1'), KeyModifiers::NONE).unwrap();
        handle_preview_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE).unwrap();
        handle_preview_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE).unwrap();
        assert_eq!(app.graph_autorun_dialog.as_ref().unwrap().quota_input, "1");

        handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(app.graph_autorun_dialog.is_none());
        assert!(app.graph_action_pending);
    }

    #[test]
    fn graph_autorun_dialog_picker_mode_default_seed_is_rejected_on_submit() {
        // The picker seeds to "now" absent a pending autorun (see
        // `GraphAutorunDialog::new`), which is already past by the time
        // submit runs a moment later — the dialog must stay open with an
        // inline error rather than dispatch (requirement 7).
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
        handle_preview_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE).unwrap();
        handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(app.graph_autorun_dialog.is_some());
        assert!(!app.graph_action_pending);
        assert!(app.graph_autorun_dialog.as_ref().unwrap().error.is_some());
    }

    #[test]
    fn graph_autorun_dialog_picker_mode_future_time_submits() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
        handle_preview_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE).unwrap();
        // Field 0 (year) is focused on open; bump it a year forward so the
        // picked time is unambiguously in the future.
        handle_preview_key(&mut app, KeyCode::Up, KeyModifiers::NONE).unwrap();
        handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(app.graph_autorun_dialog.is_none());
        assert!(app.graph_action_pending);
    }

    #[test]
    fn graph_autorun_dialog_esc_closes_without_dispatching() {
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
        handle_preview_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE).unwrap();
        handle_preview_key(&mut app, KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(app.graph_autorun_dialog.is_none());
        assert!(!app.graph_action_pending);
    }

    #[test]
    fn graph_autorun_dialog_picker_arrows_and_digit_keep_editing() {
        use crate::tui::app::dialog::GraphAutorunMode;
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
        handle_preview_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE).unwrap();
        assert_eq!(
            app.graph_autorun_dialog.as_ref().unwrap().mode,
            GraphAutorunMode::Picker
        );

        // Move focus across fields, nudge the value, then type a digit: the
        // picker must stay open in Picker mode without dispatching.
        handle_preview_key(&mut app, KeyCode::Right, KeyModifiers::NONE).unwrap();
        handle_preview_key(&mut app, KeyCode::Left, KeyModifiers::NONE).unwrap();
        handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();
        handle_preview_key(&mut app, KeyCode::Up, KeyModifiers::NONE).unwrap();
        handle_preview_key(&mut app, KeyCode::Char('7'), KeyModifiers::NONE).unwrap();

        assert!(app.graph_autorun_dialog.is_some());
        assert_eq!(
            app.graph_autorun_dialog.as_ref().unwrap().mode,
            GraphAutorunMode::Picker
        );
        assert!(!app.graph_action_pending);
    }

    #[test]
    fn entering_a_graph_lets_plain_arrows_navigate_it() {
        // CT23 FR2/FR3: before Enter, plain arrows must not touch the
        // graph's internal navigation; after Enter, they must.
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
        app.graph_live_state = Some(crate::tui::app::graph_live_state::GraphLiveState {
            graph_id: "lp1".to_string(),
            graph_name: "Nightly review".to_string(),
            graph_status: crate::domain::graphs::GraphStatus::Running,
            workdir: "/tmp".to_string(),
            trigger_type: "manual".to_string(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            spec_queue: Vec::new(),
            done_count: 0,
            total_count: 0,
            current_spec_id: None,
            effective_nodes: vec![
                crate::domain::graphs::GraphNode {
                    id: "n1".to_string(),
                    spec_id: None,
                    graph_id: Some("lp1".to_string()),
                    name: "Implement".to_string(),
                    kind: crate::domain::graphs::GraphNodeKind::Agent,
                    config: serde_json::json!({}),
                    position: 0,
                    created_at: Utc::now(),
                },
                crate::domain::graphs::GraphNode {
                    id: "n2".to_string(),
                    spec_id: None,
                    graph_id: Some("lp1".to_string()),
                    name: "Review".to_string(),
                    kind: crate::domain::graphs::GraphNodeKind::Check,
                    config: serde_json::json!({}),
                    position: 1,
                    created_at: Utc::now(),
                },
            ],
            effective_edges: vec![crate::domain::graphs::GraphEdge {
                id: "e1".to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                from_node: "n1".to_string(),
                to_node: "n2".to_string(),
                condition: crate::domain::graphs::GraphEdgeCondition::Pass,
            }],
            ensembles: Vec::new(),
            router_taken_routes: std::collections::HashMap::new(),
            current_node_id: Some("n1".to_string()),
            current_node_status: None,
            current_node_started_at: None,
            current_node_iteration: None,
        });

        assert!(app.graph_live_follow);
        handle_preview_key(&mut app, KeyCode::Right, KeyModifiers::NONE).unwrap();
        assert!(
            app.graph_live_follow,
            "plain Right before Enter must not move the graph or leave auto-follow"
        );

        handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(!app.graph_live_follow, "Enter must switch to manual mode");

        handle_preview_key(&mut app, KeyCode::Right, KeyModifiers::NONE).unwrap();
        assert_eq!(
            app.graph_live_selected_node.as_deref(),
            Some("n2"),
            "plain Right after entering must move to the child node"
        );
    }

    #[test]
    fn esc_leaves_manual_navigation_back_to_auto() {
        // CT23 FR3/FR5: Esc must leave manual navigation, returning to auto.
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
        app.graph_live_state = Some(running_live_state());
        handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(!app.graph_live_follow, "Enter must switch to manual mode");

        handle_preview_key(&mut app, KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(
            app.graph_live_follow,
            "Esc must return the graph view to auto mode"
        );
    }

    /// Seeds a Completed graph `lp1` with a bound Completed spec `s1` and two
    /// graph-level nodes `n1 -> n2` (pass). No runs recorded. Used by the
    /// CT24 tests so the refresh path (`refresh_graphs_selection`) can
    /// reassemble a live state whose effective nodes still contain the graph.
    fn seed_completed_graph_with_nodes(db: &Database) {
        db.insert_graph(&crate::domain::graphs::Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "lp1".to_string(),
            name: "Nightly review".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: crate::domain::graphs::GraphStatus::Completed,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        })
        .unwrap();
        db.insert_graph_spec(&crate::domain::graphs::GraphSpec {
            id: "s1".to_string(),
            graph_id: Some("lp1".to_string()),
            name: "Spec".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: crate::domain::graphs::GraphSpecStatus::Completed,
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
        })
        .unwrap();
        for (position, id) in ["n1", "n2"].iter().enumerate() {
            db.insert_graph_node(&crate::domain::graphs::GraphNode {
                id: id.to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                name: id.to_string(),
                kind: crate::domain::graphs::GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: position as i64,
                created_at: Utc::now(),
            })
            .unwrap();
        }
        db.insert_graph_edge(&crate::domain::graphs::GraphEdge {
            id: "e1".to_string(),
            spec_id: None,
            graph_id: Some("lp1".to_string()),
            from_node: "n1".to_string(),
            to_node: "n2".to_string(),
            condition: crate::domain::graphs::GraphEdgeCondition::Pass,
        })
        .unwrap();
    }

    #[test]
    fn enter_on_completed_graph_seeds_manual_and_stays_manual() {
        // CT24 FR1/FR2: a finished graph has no engine current node, so Enter
        // must seed a manual selection (entry node) and the next refresh tick
        // must keep manual mode as long as that node still exists — never
        // flip back just because the graph is not running.
        let db = test_db();
        seed_completed_graph_with_nodes(&db);
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.focus = Focus::Preview;
        app.sidebar_layer = SidebarLayer::Automation;
        app.automation_kind = crate::tui::app::AutomationKind::Graph;
        app.graphs = vec![graph_with_status(
            crate::domain::graphs::GraphStatus::Completed,
        )];
        app.selected_graph_id = Some("lp1".to_string());
        app.graph_live_state = Some(crate::tui::app::graph_live_state::GraphLiveState {
            graph_id: "lp1".to_string(),
            graph_name: "Nightly review".to_string(),
            graph_status: crate::domain::graphs::GraphStatus::Completed,
            workdir: "/tmp".to_string(),
            trigger_type: "manual".to_string(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            spec_queue: Vec::new(),
            done_count: 0,
            total_count: 0,
            current_spec_id: None,
            effective_nodes: vec![
                crate::domain::graphs::GraphNode {
                    id: "n1".to_string(),
                    spec_id: None,
                    graph_id: Some("lp1".to_string()),
                    name: "Implement".to_string(),
                    kind: crate::domain::graphs::GraphNodeKind::Agent,
                    config: serde_json::json!({}),
                    position: 0,
                    created_at: Utc::now(),
                },
                crate::domain::graphs::GraphNode {
                    id: "n2".to_string(),
                    spec_id: None,
                    graph_id: Some("lp1".to_string()),
                    name: "Review".to_string(),
                    kind: crate::domain::graphs::GraphNodeKind::Check,
                    config: serde_json::json!({}),
                    position: 1,
                    created_at: Utc::now(),
                },
            ],
            effective_edges: vec![crate::domain::graphs::GraphEdge {
                id: "e1".to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                from_node: "n1".to_string(),
                to_node: "n2".to_string(),
                condition: crate::domain::graphs::GraphEdgeCondition::Pass,
            }],
            ensembles: Vec::new(),
            router_taken_routes: std::collections::HashMap::new(),
            current_node_id: None,
            current_node_status: None,
            current_node_started_at: None,
            current_node_iteration: None,
        });
        app.graph_live_follow = true;
        app.graph_live_selected_node = None;

        handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(!app.graph_live_follow, "Enter must switch to manual mode");
        assert!(
            app.graph_live_selected_node.is_some(),
            "a seed must exist on a finished graph"
        );

        app.refresh_graphs_selection();
        assert!(
            !app.graph_live_follow,
            "refresh must keep manual mode when the seed still exists"
        );

        let before = app.graph_live_selected_node.clone();
        handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();
        assert_ne!(
            app.graph_live_selected_node, before,
            "plain Down must move inside the graph"
        );
        assert_eq!(
            app.selected_graph_id.as_deref(),
            Some("lp1"),
            "plain Down must not move the sidebar to the next graph"
        );
    }

    #[test]
    fn enter_on_a_paused_graph_seeds_the_entry_node() {
        // CT24 FR1 (c): with no runs to consult, Enter must seed the graph's
        // entry node (first effective node) — not stay in follow with a None
        // selection.
        let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Paused);
        app.graph_live_state = Some(crate::tui::app::graph_live_state::GraphLiveState {
            graph_id: "lp1".to_string(),
            graph_name: "Nightly review".to_string(),
            graph_status: crate::domain::graphs::GraphStatus::Paused,
            workdir: "/tmp".to_string(),
            trigger_type: "manual".to_string(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            spec_queue: Vec::new(),
            done_count: 0,
            total_count: 0,
            current_spec_id: None,
            effective_nodes: vec![
                crate::domain::graphs::GraphNode {
                    id: "n1".to_string(),
                    spec_id: None,
                    graph_id: Some("lp1".to_string()),
                    name: "Implement".to_string(),
                    kind: crate::domain::graphs::GraphNodeKind::Agent,
                    config: serde_json::json!({}),
                    position: 0,
                    created_at: Utc::now(),
                },
                crate::domain::graphs::GraphNode {
                    id: "n2".to_string(),
                    spec_id: None,
                    graph_id: Some("lp1".to_string()),
                    name: "Review".to_string(),
                    kind: crate::domain::graphs::GraphNodeKind::Check,
                    config: serde_json::json!({}),
                    position: 1,
                    created_at: Utc::now(),
                },
            ],
            effective_edges: vec![crate::domain::graphs::GraphEdge {
                id: "e1".to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                from_node: "n1".to_string(),
                to_node: "n2".to_string(),
                condition: crate::domain::graphs::GraphEdgeCondition::Pass,
            }],
            ensembles: Vec::new(),
            router_taken_routes: std::collections::HashMap::new(),
            current_node_id: None,
            current_node_status: None,
            current_node_started_at: None,
            current_node_iteration: None,
        });
        app.graph_live_follow = true;
        app.graph_live_selected_node = None;

        handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(!app.graph_live_follow, "Enter must switch to manual mode");
        assert_eq!(
            app.graph_live_selected_node.as_deref(),
            Some("n1"),
            "the entry node must be selected when there is no run to seed from"
        );
    }

    fn app_on_two_graphs() -> App {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;
        app.sidebar_layer = SidebarLayer::Automation;
        app.automation_kind = crate::tui::app::AutomationKind::Graph;
        app.agents = Vec::new();
        let now = Utc::now();
        let mut g1 = graph_with_status(crate::domain::graphs::GraphStatus::Running);
        g1.id = "g1".to_string();
        g1.created_at = now;
        let mut g2 = graph_with_status(crate::domain::graphs::GraphStatus::Running);
        g2.id = "g2".to_string();
        g2.created_at = now - chrono::Duration::seconds(10);
        app.graphs = vec![g1, g2];
        app.selected_graph_id = Some("g1".to_string());
        app
    }

    #[test]
    fn shift_down_moves_between_graphs_without_entering_either() {
        // CT23 FR1/FR5: Shift+Down must move the sidebar selection to the
        // next graph, not navigate inside the first one, and the
        // destination must land in auto mode (not entered).
        let mut app = app_on_two_graphs();

        handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::SHIFT).unwrap();

        assert_eq!(app.selected_graph_id.as_deref(), Some("g2"));
        assert!(
            app.graph_live_follow,
            "landing on the next graph must not be in manual/entered mode"
        );
    }

    #[test]
    fn shift_down_reaches_next_graph_even_while_entered() {
        // CT23 FR1: once a graph is entered (manual navigation active),
        // Shift+Down must still move to the next graph — the previous bug
        // captured it as internal sibling navigation instead.
        let mut app = app_on_two_graphs();
        app.graph_live_state = Some(running_live_state());

        handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(!app.graph_live_follow, "Enter must switch to manual mode");

        handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::SHIFT).unwrap();

        assert_eq!(app.selected_graph_id.as_deref(), Some("g2"));
    }

    fn spawn_cat_agent(name: &str) -> crate::tui::agent::InteractiveAgent {
        crate::tui::agent::InteractiveAgent::spawn(
            Cli::new("cat"),
            ".",
            80,
            24,
            None,
            None,
            ratatui::style::Color::Reset,
            Some(name),
            &[],
            None,
            None,
            None,
        )
        .expect("spawn cat as a stand-in interactive child")
    }

    #[test]
    fn preview_ctrl_t_opens_context_transfer_for_interactive_selection() {
        let mut app = app_with_agents();
        app.interactive_agents = vec![spawn_cat_agent("interactive-1")];
        app.agents = vec![AgentEntry::Interactive(0)];
        app.selected = 0;
        app.focus = Focus::Preview;

        handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL).unwrap();

        assert!(matches!(app.focus, Focus::ContextTransfer));
        app.interactive_agents[0].kill();
    }

    #[test]
    fn preview_ctrl_t_does_nothing_for_non_session_selection() {
        let mut app = app_with_agents();
        app.focus = Focus::Preview;

        handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL).unwrap();

        assert!(matches!(app.focus, Focus::Preview));
    }

    #[test]
    fn preview_ctrl_t_opens_rag_transfer_not_context_transfer_while_playground_active() {
        let mut app = app_with_agents();
        app.interactive_agents = vec![spawn_cat_agent("interactive-2")];
        app.agents = vec![AgentEntry::Interactive(0)];
        app.selected = 0;
        app.focus = Focus::Preview;
        app.playground_active = true;

        handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL).unwrap();

        assert!(!matches!(app.focus, Focus::ContextTransfer));
        app.interactive_agents[0].kill();
    }

    // ── CT6: preview owns plain Up/Down regardless of child claim ────

    fn app_with_two_claimed_interactive_sessions() -> App {
        let first = spawn_cat_agent("ct6-first");
        *first.kitty_keyboard_flags.lock().expect("lock") = Some(7);
        let second = spawn_cat_agent("ct6-second");
        second.vt.lock().expect("vt lock").process(b"\x1b[?1049h");
        assert!(first.kitty_keyboard_negotiated());
        assert!(second.in_alternate_screen());

        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.interactive_agents = vec![first, second];
        app.agents = vec![AgentEntry::Interactive(0), AgentEntry::Interactive(1)];
        app.selected = 0;
        app.focus = Focus::Preview;
        app
    }

    #[test]
    fn preview_down_navigates_despite_claimed_selected_child() {
        // The selected session negotiated Kitty (Codex shape); plain Down
        // must still move the preview selection, not reach the child.
        let mut app = app_with_two_claimed_interactive_sessions();

        handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();

        assert_eq!(app.selected, 1);
        assert!(matches!(app.focus, Focus::Preview));
        app.interactive_agents[0].kill();
        app.interactive_agents[1].kill();
    }

    #[test]
    fn preview_up_navigates_despite_claimed_selected_child() {
        // The selected session entered the alternate screen (vim shape);
        // plain Up must still move the preview selection.
        let mut app = app_with_two_claimed_interactive_sessions();
        app.selected = 1;

        handle_preview_key(&mut app, KeyCode::Up, KeyModifiers::NONE).unwrap();

        assert_eq!(app.selected, 0);
        assert!(matches!(app.focus, Focus::Preview));
        app.interactive_agents[0].kill();
        app.interactive_agents[1].kill();
    }

    #[test]
    fn preview_enter_on_claimed_child_still_enters_focus() {
        // Entering focus by keyboard is unchanged: log scroll resets and
        // focus becomes Agent, exactly as for an unclaimed child.
        let mut app = app_with_two_claimed_interactive_sessions();
        app.log_scroll = 9;

        handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();

        assert!(matches!(app.focus, Focus::Agent));
        assert_eq!(app.log_scroll, 0);
        app.interactive_agents[0].kill();
        app.interactive_agents[1].kill();
    }
}
