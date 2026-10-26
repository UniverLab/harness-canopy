//! Sidebar rendering — RAG (pinned top) → tab bar (Live / Automation /
//! Knowledge, exactly one visible at a time, full remaining height) →
//! sysinfo (pinned bottom).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use super::theme::Theme;
use super::{borders_for, dialog_borders_for, last_two_segments, truncate_str, BG_HOVER};
use super::{
    STATUS_DISABLED, STATUS_FAIL, STATUS_OK, STATUS_RUNNING, STATUS_RUNNING_BRIGHT,
    STATUS_RUNNING_DIM, STATUS_WAIT_OFF, STATUS_WAIT_ON,
};
use crate::domain::graphs::{Graph, GraphStatus};
use crate::tui::agent::AgentStatus;
use crate::tui::app::types::{
    AgentEntry, AgentSectionFocus, App, AutomationKind, Focus, GraphSidebarMeta, SidebarLayer,
};
use ratatui::style::Color;

mod agent_cards;
mod knowledge_tab;
mod live_tab;
mod navigation;
mod project_graph;
mod rag;
mod shared_layout;

use agent_cards::*;
use knowledge_tab::*;
use live_tab::*;
use navigation::*;
use project_graph::*;
use rag::*;
use shared_layout::*;

pub(crate) use project_graph::draw_project_graph;
pub(crate) use shared_layout::draw_scroll_indicators;

pub(super) fn draw_sidebar(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    app.sidebar_click_map.clear();
    app.automation_graph_click_map.clear();
    app.project_click_map.clear();
    app.sidebar_tab_click_map.clear();
    app.sidebar_visible_capacity = 0;

    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(theme.sidebar_bg)),
        area,
    );

    let areas = split_sidebar_content(area, app, theme);

    let show_rag = app.rag_info.has_rag_activity() && areas.content.height >= 6;
    let (rag_area, content_below) = split_top_panel(areas.content, show_rag, 6);

    if let Some(rag_area) = rag_area.filter(|area| area.height >= 3) {
        render_titled_panel(
            frame,
            rag_area,
            rag_info_title(app),
            Style::default().fg(if is_rag_focused(app) {
                theme.header_color
            } else {
                theme.dim_text
            }),
            rag_border_style(app, theme),
            theme,
            |frame, inner| draw_rag_info(frame, inner, app, theme),
        );
    }

    let brain_area = draw_sidebar_tabs(frame, content_below, app, theme);
    let is_knowledge_tab = app.sidebar_layer == SidebarLayer::Knowledge;
    render_brain_or_graph(frame, brain_area, app, theme, is_knowledge_tab);

    render_dashboard_if_present(frame, areas.dashboard, app, theme);

    if let Some(dialog) = app.project_relation_dialog.as_ref() {
        draw_project_relation_dialog(frame, areas.content, app, dialog, theme);
    }
}

#[cfg(test)]
mod tests;
