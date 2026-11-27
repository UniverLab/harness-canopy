//! Rendering for the Hub (`F1`) Calendar tab: Timeline & Milestones sub-views
//! and the inline add-event / add-milestone form.

use chrono::Local;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph};
use ratatui::Frame;

use crate::domain::calendar::{CalendarMilestone, MilestoneStatus};
use crate::tui::app::hub_calendar::{AddFormMode, CalendarSubView};
use crate::tui::app::types::App;
use crate::tui::ui::theme::Theme;

/// Draw the Calendar tab body inside `area`.
pub fn draw_calendar_tab(frame: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    if area.height < 3 || area.width < 10 {
        return;
    }

    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    draw_subview_bar(frame, app, theme, chunks[0]);

    if app.hub.calendar.add_form.open {
        draw_add_form(frame, app, theme, chunks[1]);
        return;
    }

    match app.hub.calendar.sub_view {
        CalendarSubView::Timeline => draw_timeline(frame, app, theme, chunks[1]),
        CalendarSubView::Milestones => draw_milestones(frame, app, theme, chunks[1]),
    }
}

fn draw_subview_bar(frame: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    let timeline_label = "[Timeline]";
    let milestones_label = "[Milestones]";

    let active_style = Style::default()
        .fg(theme.accent_fg)
        .bg(theme.header_color)
        .add_modifier(Modifier::BOLD);
    let inactive_style = Style::default().fg(theme.dim_text);

    let (t_style, m_style) = match app.hub.calendar.sub_view {
        CalendarSubView::Timeline => (active_style, inactive_style),
        CalendarSubView::Milestones => (inactive_style, active_style),
    };

    let t_w = timeline_label.len() as u16;
    let m_w = milestones_label.len() as u16;
    app.hub.calendar.timeline_rect = Some(Rect::new(area.x, area.y, t_w.min(area.width), 1));
    let m_x = area.x.saturating_add(t_w + 2);
    app.hub.calendar.milestones_rect = (m_x < area.x + area.width)
        .then(|| Rect::new(m_x, area.y, m_w.min(area.x + area.width - m_x), 1));

    let mut spans = vec![
        Span::styled(timeline_label, t_style),
        Span::raw("  "),
        Span::styled(milestones_label, m_style),
    ];
    if let Some(msg) = app.hub.calendar.status_message.as_deref() {
        spans.push(Span::raw("   "));
        spans.push(Span::styled(msg, Style::default().fg(theme.header_color)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_timeline(frame: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    app.hub.calendar.list_rect = Some(area);
    let items = &app.hub.calendar.timeline;
    if items.is_empty() {
        let empty = Paragraph::new(
            "No upcoming events, feeds, schedules, or missed runs in the next 14 days.",
        )
        .style(Style::default().fg(theme.dim_text));
        frame.render_widget(empty, area);
        return;
    }

    let selected = app
        .hub
        .calendar
        .timeline_selected
        .min(items.len().saturating_sub(1));
    let height = area.height as usize;
    let start = selected.saturating_sub(height.saturating_sub(1));

    let list_items: Vec<ListItem> = items
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(idx, item)| {
            let is_sel = idx == selected;
            let prefix = if is_sel { "> " } else { "  " };
            let local_ts = item
                .at
                .with_timezone(&Local)
                .format("%m-%d %H:%M")
                .to_string();
            let tag_color = if item.pinned {
                theme.error
            } else if item.tag.starts_with("[ics:") {
                theme.header_color
            } else if item.tag == "[event]" {
                theme.success
            } else {
                theme.dim_text
            };
            let row_style = if is_sel {
                Style::default()
                    .fg(theme.accent_fg)
                    .bg(theme.header_color)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_primary)
            };
            let tag_style = if is_sel {
                row_style
            } else {
                Style::default().fg(tag_color)
            };

            let mut spans = vec![
                Span::styled(format!("{prefix}{local_ts}  "), row_style),
                Span::styled(format!("{:<14} ", item.tag), tag_style),
                Span::styled(item.title.clone(), row_style),
            ];
            if let Some(detail) = item.detail.as_deref().filter(|s| !s.trim().is_empty()) {
                let d_style = if is_sel {
                    row_style
                } else {
                    Style::default().fg(theme.dim_text)
                };
                spans.push(Span::styled(format!("  ({detail})"), d_style));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    frame.render_widget(List::new(list_items), area);
}

enum MilestoneRow<'a> {
    ProjectHeader {
        name: &'a str,
        path: &'a str,
    },
    Item {
        index: usize,
        ms: &'a CalendarMilestone,
    },
}

fn build_milestone_rows(milestones: &[CalendarMilestone]) -> Vec<MilestoneRow<'_>> {
    let mut rows = Vec::new();
    let mut current_project: Option<&str> = None;
    for (idx, ms) in milestones.iter().enumerate() {
        if current_project != Some(ms.project_path.as_str()) {
            current_project = Some(ms.project_path.as_str());
            let name = std::path::Path::new(&ms.project_path)
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|n| !n.is_empty())
                .unwrap_or(&ms.project_path);
            rows.push(MilestoneRow::ProjectHeader {
                name,
                path: &ms.project_path,
            });
        }
        rows.push(MilestoneRow::Item { index: idx, ms });
    }
    rows
}

fn render_milestone_row(
    row: &MilestoneRow<'_>,
    selected: usize,
    theme: &Theme,
) -> ListItem<'static> {
    match row {
        MilestoneRow::ProjectHeader { name, path } => {
            let label = if name == path {
                (*name).to_string()
            } else {
                format!("{name} ({path})")
            };
            ListItem::new(Line::from(Span::styled(
                label,
                Style::default()
                    .fg(theme.header_color)
                    .add_modifier(Modifier::BOLD),
            )))
        }
        MilestoneRow::Item { index, ms } => {
            let is_sel = *index == selected;
            let prefix = if is_sel { "  > " } else { "    " };
            let status_color = match ms.status {
                MilestoneStatus::Planned => theme.dim_text,
                MilestoneStatus::Active => theme.header_color,
                MilestoneStatus::Done => theme.success,
            };
            let row_style = if is_sel {
                Style::default()
                    .fg(theme.accent_fg)
                    .bg(theme.header_color)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_primary)
            };
            let badge_style = if is_sel {
                row_style
            } else {
                Style::default().fg(status_color)
            };
            let due = ms.due_at.as_deref().unwrap_or("unscheduled");

            ListItem::new(Line::from(vec![
                Span::styled(prefix, row_style),
                Span::styled(format!("[{:<7}] ", ms.status.as_str()), badge_style),
                Span::styled(ms.title.clone(), row_style),
                Span::styled(
                    format!("  ({due})"),
                    if is_sel {
                        row_style
                    } else {
                        Style::default().fg(theme.dim_text)
                    },
                ),
            ]))
        }
    }
}

fn draw_milestones(frame: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    app.hub.calendar.list_rect = Some(area);
    app.hub.calendar.milestone_row_indices.clear();
    if app.hub.calendar.milestones.is_empty() {
        let empty = Paragraph::new("No project milestones. Press 'a' to add one.")
            .style(Style::default().fg(theme.dim_text));
        frame.render_widget(empty, area);
        return;
    }

    let selected = app
        .hub
        .calendar
        .milestone_selected
        .min(app.hub.calendar.milestones.len().saturating_sub(1));
    let rows = build_milestone_rows(&app.hub.calendar.milestones);
    let selected_row = rows
        .iter()
        .position(|r| matches!(r, MilestoneRow::Item { index, .. } if *index == selected))
        .unwrap_or(0);
    let height = area.height as usize;
    let start = selected_row.saturating_sub(height.saturating_sub(1));

    let mut row_indices = Vec::new();
    let list_items: Vec<ListItem> = rows
        .iter()
        .skip(start)
        .take(height)
        .map(|row| {
            row_indices.push(match row {
                MilestoneRow::ProjectHeader { .. } => None,
                MilestoneRow::Item { index, .. } => Some(*index),
            });
            render_milestone_row(row, selected, theme)
        })
        .collect();

    app.hub.calendar.milestone_row_indices = row_indices;
    frame.render_widget(List::new(list_items), area);
}

fn draw_add_form(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let form = &app.hub.calendar.add_form;
    let heading = match form.mode {
        AddFormMode::Event => "New Calendar Event",
        AddFormMode::Milestone => "New Project Milestone",
    };
    let mut lines = vec![
        Line::from(Span::styled(
            heading,
            Style::default()
                .fg(theme.header_color)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    let mut fields: Vec<(&str, &str)> = vec![
        ("Title", &form.title),
        (
            if form.mode == AddFormMode::Event {
                "At (RFC3339)"
            } else {
                "Due (RFC3339, opt)"
            },
            &form.at,
        ),
    ];
    if form.mode == AddFormMode::Milestone {
        fields.push(("Project", &form.project_path));
    }
    fields.push(("Notes (opt)", &form.notes));

    for (idx, (label, val)) in fields.into_iter().enumerate() {
        let active = idx == form.field;
        let marker = if active { "> " } else { "  " };
        let style = if active {
            Style::default()
                .fg(theme.accent_fg)
                .bg(theme.header_color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text_primary)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker}{label:<18}: "), style),
            Span::styled(val.to_string(), style),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Tab/Up/Down: next field   Enter: save   Esc: cancel",
        Style::default().fg(theme.dim_text),
    )));

    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::canopy_config::CanopyConfig;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::Arc;

    #[test]
    fn draw_milestones_groups_items_by_project_with_headers() {
        let path = crate::test_support::temp_db_path();
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        // Create interleaved milestones across two projects.
        db.create_calendar_milestone(
            "/tmp/alpha",
            "Alpha early",
            None,
            Some("2026-12-01T00:00:00Z"),
            None,
        )
        .unwrap();
        db.create_calendar_milestone(
            "/tmp/beta",
            "Beta mid",
            None,
            Some("2026-12-02T00:00:00Z"),
            None,
        )
        .unwrap();
        db.create_calendar_milestone(
            "/tmp/alpha",
            "Alpha late",
            None,
            Some("2026-12-03T00:00:00Z"),
            None,
        )
        .unwrap();

        let mut app = App::new(db, data_dir.path(), &CanopyConfig::default()).unwrap();
        app.refresh_calendar();
        app.hub.calendar.sub_view = CalendarSubView::Milestones;

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::default();
        terminal
            .draw(|frame| draw_calendar_tab(frame, &mut app, &theme, Rect::new(0, 0, 80, 10)))
            .unwrap();

        assert_eq!(
            app.hub.calendar.milestone_row_indices,
            vec![None, Some(0), Some(1), None, Some(2)]
        );
        let buf = terminal.backend().buffer();
        let rendered: Vec<String> = (0..10)
            .map(|y| {
                (0..80)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        assert!(rendered[1].contains("alpha (/tmp/alpha)"));
        assert!(rendered[2].contains("Alpha early"));
        assert!(rendered[3].contains("Alpha late"));
        assert!(rendered[4].contains("beta (/tmp/beta)"));
        assert!(rendered[5].contains("Beta mid"));
    }
}
