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
mod playground_key_tests;

#[cfg(test)]
mod home_key_tests;

#[cfg(test)]
mod preview_key_tests;
