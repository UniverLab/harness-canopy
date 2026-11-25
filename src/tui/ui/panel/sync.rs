use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::tui::app::types::{App, PanelFace};
use crate::tui::ui::theme::Theme;
use crate::tui::ui::{truncate_str, ERROR_COLOR, STATUS_OK};

/// CT1 multi-face right panel: one bordered panel, three faces. The title
/// names the face; when the panel switched on its own the badge names the
/// reason, so a face never appears unexplained.
pub(crate) fn draw_panel_face(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let face = app.panel_face;
    let badge = app.panel_face_badge();
    let mut title = format!(" {} ", face.label());
    if let Some(reason) = badge {
        title = format!("{title}· {reason} ");
    }
    // While the user drags the panel's left border, the border takes the
    // accent colour so the drag target stays visible.
    let border = if app.activity_panel_dragging {
        theme.header_color
    } else {
        theme.border_color
    };
    let block = Block::default()
        .title(
            Line::from(Span::styled(title, Style::default().fg(theme.dim_text)))
                .alignment(ratatui::layout::Alignment::Right),
        )
        .borders(crate::tui::ui::borders_for(theme))
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    app.last_activity_rect = None;
    app.last_knowledge_graph_rect = None;
    app.last_knowledge_list_rect = None;
    app.last_graph_face_rect = None;
    app.graph_card_click_map.clear();

    match face {
        PanelFace::Activity => {
            app.last_activity_rect = Some(inner);
            if app.activity_panel_state().is_some() {
                draw_graph_face(frame, inner, app, theme);
            } else {
                draw_panel_placeholder(frame, inner, "no activity", theme);
            }
        }
        PanelFace::Knowledge => draw_knowledge_face(frame, inner, app, theme),
        PanelFace::Graph => draw_graph_face(frame, inner, app, theme),
        // CT30: the Crews face is read-only (`&App` reborrow) and manages
        // its own refresh through the tick, not through rendering.
        PanelFace::Crews => super::crews::draw_crews_face(frame, inner, app, theme),
    }
}

pub(super) fn draw_panel_placeholder(frame: &mut Frame, area: Rect, text: &str, theme: &Theme) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(theme.dim_text),
        ))),
        area,
    );
}

/// Knowledge face: the activity section moved under it, then the
/// project-relations graph (CB16's scrollable renderer, reused — not
/// redrawn), then the knowledge/backlog list for the selected project.
fn draw_knowledge_face(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let graph_rows = knowledge_graph_height(app, area.height);
    if area.height < 8 || graph_rows == 0 {
        // Too short to split honestly: the graph cards keep the whole panel.
        app.last_activity_rect = Some(area);
        if app.selected_activity_state().is_some() {
            draw_graph_face(frame, area, app, theme);
        } else {
            draw_panel_placeholder(frame, area, "no activity", theme);
        }
        return;
    }

    let [activity_area, graph_area, list_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(graph_rows),
        Constraint::Min(knowledge_list_min_height(app)),
    ])
    .areas(area);

    app.last_activity_rect = Some(activity_area);
    if app.selected_activity_state().is_some() {
        draw_graph_face(frame, activity_area, app, theme);
    } else {
        draw_panel_placeholder(frame, activity_area, "no activity", theme);
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "project graph",
            Style::default()
                .fg(theme.dim_text)
                .add_modifier(Modifier::BOLD),
        ))),
        Rect::new(graph_area.x, graph_area.y, graph_area.width, 1),
    );
    let graph_inner = Rect::new(
        graph_area.x,
        graph_area.y + 1,
        graph_area.width,
        graph_area.height.saturating_sub(1),
    );
    app.last_knowledge_graph_rect = Some(graph_inner);
    crate::tui::ui::sidebar::draw_project_graph(
        frame,
        graph_inner,
        app,
        theme,
        app.knowledge_graph_scroll,
    );

    app.last_knowledge_list_rect = Some(list_area);
    draw_knowledge_list(frame, list_area, app, theme);
}

/// Rows the graph section needs: one header line plus one row per edge,
/// capped so the activity section above never starves.
fn knowledge_graph_height(app: &App, panel_height: u16) -> u16 {
    if panel_height < 8 {
        return 0;
    }
    let edges = app.project_graph_edges.len();
    let rows = if edges == 0 { 1 } else { edges.min(5) as u16 };
    (rows + 1).min(panel_height / 3).max(2)
}

/// Minimum rows for the knowledge/backlog list: header plus the selected
/// row with one line of context on each side when space allows (CB15's
/// height floor — the section holding the cursor keeps its rows).
fn knowledge_list_min_height(app: &App) -> u16 {
    if app.project_knowledge.is_empty() && app.backlog_specs.is_empty() {
        2
    } else {
        4
    }
}

fn draw_knowledge_list(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if area.height == 0 {
        return;
    }
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "knowledge",
            Style::default()
                .fg(theme.dim_text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " ({}) · backlog ({})",
                app.project_knowledge.len(),
                app.backlog_specs.len()
            ),
            Style::default().fg(theme.dim_text),
        ),
    ])];

    if app.project_knowledge.is_empty() {
        lines.push(Line::from(Span::styled(
            "  none yet",
            Style::default().fg(theme.dim_text),
        )));
    } else {
        let total = app.project_knowledge.len();
        let visible = (area.height as usize).saturating_sub(1).max(1);
        let max_start = total.saturating_sub(visible);
        let start = (app.knowledge_list_scroll as usize).min(max_start);
        for (idx, node) in app
            .project_knowledge
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
        {
            let selected = idx == app.selected_knowledge;
            let marker = if selected { "›" } else { " " };
            let kind_color = if node.kind == "fact" {
                Color::Cyan
            } else {
                Color::Magenta
            };
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(theme.header_color)),
                Span::raw(" "),
                Span::styled(format!("[{}] ", node.kind), Style::default().fg(kind_color)),
                Span::styled(
                    truncate_str(&node.title, area.width.saturating_sub(10) as usize),
                    if selected {
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_graph_face(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    super::graph_cards::draw_graph_cards(frame, area, app, theme);
}

pub(super) fn graph_status_color(status: crate::domain::graphs::GraphStatus) -> Color {
    match status {
        crate::domain::graphs::GraphStatus::Running => STATUS_OK,
        crate::domain::graphs::GraphStatus::Failed => ERROR_COLOR,
        crate::domain::graphs::GraphStatus::Pausing => Color::Yellow,
        crate::domain::graphs::GraphStatus::Paused => Color::Yellow,
        _ => Color::White,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::tui::app::types::App;

    fn test_db() -> std::sync::Arc<Database> {
        let path = crate::test_support::temp_db_path();
        std::sync::Arc::new(Database::new(&path).expect("create test db"))
    }

    #[test]
    fn knowledge_graph_height_caps_so_activity_never_starves() {
        let db = test_db();
        let data_dir = tempfile::tempdir().expect("create data dir");
        let mut app = App::new(
            std::sync::Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        // No edges: one placeholder row + header.
        assert_eq!(knowledge_graph_height(&app, 30), 2);
        // Many edges: capped at 5 rows + header.
        app.project_graph_edges = (0..20)
            .map(|i| crate::tui::app::types::ProjectGraphEdge {
                from_name: format!("a{i}"),
                to_name: format!("b{i}"),
                from_hash: format!("ha{i}"),
                to_hash: format!("hb{i}"),
                relation: "relates_to".to_string(),
            })
            .collect();
        assert_eq!(knowledge_graph_height(&app, 30), 6);
        // A short panel refuses to split at all.
        assert_eq!(knowledge_graph_height(&app, 7), 0);
    }

    #[test]
    fn knowledge_list_min_height_keeps_rows_for_the_cursor() {
        let db = test_db();
        let data_dir = tempfile::tempdir().expect("create data dir");
        let app = App::new(
            std::sync::Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        // Empty: header + placeholder only.
        assert_eq!(knowledge_list_min_height(&app), 2);
    }
}
