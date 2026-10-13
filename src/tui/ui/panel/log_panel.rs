use super::*;

pub(super) fn draw_log_panel_focus(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    theme: &Theme,
) -> bool {
    match app.focus {
        Focus::Home => {
            draw_home_panel(frame, area, app, theme);
            true
        }
        Focus::Preview => draw_preview_panel(frame, area, app, theme),
        Focus::Agent if app.sidebar_layer == SidebarLayer::Knowledge => {
            draw_project_tabs_panel(frame, area, app, theme);
            true
        }
        Focus::Agent => draw_agent_panel(frame, area, app, theme),
        Focus::NewAgentDialog => draw_new_agent_dialog_background(frame, area, app, theme),
        Focus::LaunchpadDialog
        | Focus::KnowledgeDialog
        | Focus::ContextTransfer
        | Focus::RagTransfer
        | Focus::PromptTemplateDialog
        | Focus::GraphEditorDialog
        | Focus::GraphFormDialog => false,
        Focus::ProjectRelationDialog => {
            draw_project_preview_card(frame, area, app, theme);
            true
        }
    }
}

pub(super) fn draw_preview_panel(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    theme: &Theme,
) -> bool {
    if app.playground_active {
        draw_playground_panel(frame, area, app, theme);
        return true;
    }

    if app.sidebar_layer == SidebarLayer::Knowledge {
        draw_project_preview_card(frame, area, app, theme);
        return true;
    }

    if app.sidebar_layer == SidebarLayer::Automation
        && app.automation_kind == crate::tui::app::AutomationKind::Graph
    {
        draw_graph_live_view(frame, area, app, theme);
        return true;
    }

    if app.agents_rag_focused && app.rag_info.has_rag_activity() {
        draw_rag_info_overview(frame, area, app, theme);
        return true;
    }

    let Some(selected) = app.selected_agent() else {
        return false;
    };

    draw_selected_preview(frame, area, app, selected, theme)
}

pub(super) fn draw_agent_panel(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    theme: &Theme,
) -> bool {
    if app.playground_active {
        draw_playground_panel(frame, area, app, theme);
        return true;
    }

    let Some(selected) = app.selected_agent() else {
        return false;
    };

    match selected {
        AgentEntry::Interactive(idx) => draw_focused_interactive_panel(frame, area, app, *idx),
        AgentEntry::Terminal(idx) => draw_focused_terminal_panel(frame, area, app, *idx),
        AgentEntry::Group(idx) => {
            draw_group_details(frame, area, app, *idx, theme);
            true
        }
        AgentEntry::Agent(agent) => {
            draw_background_agent_panel(frame, area, agent, app, theme);
            true
        }
        AgentEntry::Corrupt(corrupt) => {
            draw_corrupt_agent_panel(frame, area, corrupt);
            true
        }
        AgentEntry::Orphaned(idx) => {
            if let Some(session) = app.orphaned_sessions.get(*idx) {
                let text = format!(
                    "Orphaned session: {}\nCLI: {}  Workdir: {}\n\nPress 'r' to revive or 'd' to dismiss.",
                    session.name, session.cli, session.working_dir
                );
                let paragraph = ratatui::widgets::Paragraph::new(text)
                    .style(ratatui::style::Style::default().fg(ratatui::style::Color::Yellow));
                frame.render_widget(paragraph, area);
            }
            true
        }
    }
}

pub(super) fn draw_interactive_preview(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    idx: usize,
) -> bool {
    let Some(agent) = app.interactive_agents.get(idx) else {
        return false;
    };
    let Some(snap) = agent.screen_snapshot() else {
        return false;
    };

    render_snapshot(frame, area, &snap, app, false, false, None);
    true
}

pub(super) fn draw_terminal_preview(frame: &mut Frame, area: Rect, app: &App, idx: usize) -> bool {
    let Some(agent) = app.terminal_agents.get(idx) else {
        return false;
    };
    let Some(snap) = agent.screen_snapshot() else {
        return false;
    };

    render_snapshot(frame, area, &snap, app, false, false, None);
    render_command_chips(frame, area, app, &agent.name);
    true
}

pub(super) fn draw_focused_interactive_panel(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    idx: usize,
) -> bool {
    let Some(agent) = app.interactive_agents.get(idx) else {
        return false;
    };
    let Some(snap) = agent.screen_snapshot() else {
        return false;
    };

    render_snapshot(
        frame,
        area,
        &snap,
        app,
        agent.is_sensitive_input_active(),
        false,
        pane_selection(app, false, idx),
    );
    set_focused_interactive_cursor(frame, area, &snap, agent);
    true
}

pub(super) fn draw_focused_terminal_panel(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    idx: usize,
) -> bool {
    let Some(agent) = app.terminal_agents.get(idx) else {
        return false;
    };

    // CT16: the alternate screen must always be full-pane. `last_panel_inner`
    // is left at the full `inner` size on this path (only
    // `draw_terminal_warp_mode` overwrites it with the smaller `pty_area`),
    // which is what `resize_interactive_agents` reports to the child.
    debug_assert!(
        !agent.in_alternate_screen() || !(agent.warp_mode && !agent.should_bypass_warp_input()),
        "alt screen must be full-pane (warp_active false)"
    );
    // CT16 fallback: never render nothing. If the pane is too small to host
    // a full-screen child, say so on screen and point at an interactive
    // session instead of leaving a blank pane.
    if agent.in_alternate_screen() && (area.width < 20 || area.height < 6) {
        let msg = format!(
            "{} wants full screen — open as interactive session (Ctrl+N)",
            agent.shell
        );
        frame.render_widget(
            Paragraph::new(msg).style(Style::default().fg(Color::Yellow)),
            area,
        );
        return true;
    }
    let sensitive = agent.is_sensitive_input_active();
    // Warp input box only while the shell itself owns the terminal; when a
    // wizard/TUI/foreground command is running the PTY gets the whole pane.
    let warp_active = agent.warp_mode && !agent.should_bypass_warp_input();
    let snap = agent.screen_snapshot();

    if !warp_active {
        let Some(snap) = snap else {
            return false;
        };
        render_snapshot(
            frame,
            area,
            &snap,
            app,
            sensitive,
            true,
            pane_selection(app, true, idx),
        );
        return true;
    }

    draw_terminal_warp_mode(frame, area, app, idx, snap.as_ref(), sensitive);
    true
}

pub(super) fn draw_selected_preview(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    selected: &AgentEntry,
    theme: &Theme,
) -> bool {
    match selected {
        AgentEntry::Agent(agent) => {
            draw_agent_details(frame, area, agent, app, theme);
            true
        }
        AgentEntry::Corrupt(corrupt) => {
            draw_corrupt_agent_panel(frame, area, corrupt);
            true
        }
        AgentEntry::Interactive(idx) => draw_interactive_preview(frame, area, app, *idx),
        AgentEntry::Terminal(idx) => draw_terminal_preview(frame, area, app, *idx),
        AgentEntry::Group(idx) => {
            draw_group_details(frame, area, app, *idx, theme);
            true
        }
        AgentEntry::Orphaned(idx) => {
            if let Some(session) = app.orphaned_sessions.get(*idx) {
                let text = format!(
                    "Orphaned: {} ({})\nWorkdir: {}",
                    session.name, session.cli, session.working_dir
                );
                let paragraph = ratatui::widgets::Paragraph::new(text)
                    .style(ratatui::style::Style::default().fg(ratatui::style::Color::Yellow));
                frame.render_widget(paragraph, area);
            }
            true
        }
    }
}

pub(super) fn draw_terminal_warp_mode(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    idx: usize,
    snap: Option<&crate::tui::agent::ScreenSnapshot>,
    _sensitive: bool,
) {
    let (pty_area, input_area) = split_warp_areas(area, &warp_input_text(app, idx));
    if let Some(snap) = snap {
        render_snapshot(
            frame,
            pty_area,
            snap,
            app,
            false,
            false,
            pane_selection(app, true, idx),
        );
    }
    draw_warp_input_box(frame, input_area, app, idx);
    app.last_panel_inner = (pty_area.width, pty_area.height);
    app.last_panel_x = pty_area.x;
    app.last_panel_y = pty_area.y;
}

pub(super) fn draw_new_agent_dialog_background(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    theme: &Theme,
) -> bool {
    let previous_focus = app
        .new_agent_dialog
        .as_ref()
        .and_then(|dialog| dialog.prev_focus);
    if matches!(previous_focus, Some(Focus::Home) | None) {
        draw_home_panel(frame, area, app, theme);
        return true;
    }

    false
}

pub(super) fn set_focused_interactive_cursor(
    frame: &mut Frame,
    area: Rect,
    snap: &crate::tui::agent::ScreenSnapshot,
    agent: &crate::tui::agent::InteractiveAgent,
) {
    if snap.scrolled || area.width == 0 || area.height == 0 {
        return;
    }

    let cursor_col = adjusted_interactive_cursor_col(agent.cli.as_str(), snap);
    let cx = area.x + cursor_col.min(area.width.saturating_sub(1));
    let cy = area.y + snap.cursor_row.min(area.height.saturating_sub(1));
    frame.set_cursor_position((cx, cy));
}

pub(super) fn adjusted_interactive_cursor_col(
    cli_name: &str,
    snap: &crate::tui::agent::ScreenSnapshot,
) -> u16 {
    let cursor_col = snap.cursor_col;
    if !cli_name.to_ascii_lowercase().contains("copilot") {
        return cursor_col;
    }

    // Copilot renders its own in-band cursor as an inverse-highlighted cell.
    // Trust that decoration over the vt cursor coordinates.
    if let Some(row) = snap.cells.get(snap.cursor_row as usize) {
        if let Some((idx, _)) = row
            .iter()
            .enumerate()
            .find(|(_, cell)| cell.as_ref().is_some_and(|c| c.inverse))
        {
            return idx as u16;
        }
    }

    cursor_col
}
