//! CT31: the hub's Settings tab renderer — the group column, one row per
//! setting with its badges, the inline editors and the status line.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use super::centered_rect;
use crate::domain::settings::find;
use crate::tui::app::hub_settings::{
    display_value, effective_eq_default, settings_badge_for, SettingRow, SettingsEditor,
    SettingsFocus,
};
use crate::tui::app::types::App;
use crate::tui::selection::clamp_scroll;
use crate::tui::ui::theme::Theme;

pub(super) fn draw_settings_tab(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let categories_focused = app.hub.settings.focus == SettingsFocus::Categories;
    let (rows_area, props_focused) = if area.width >= 80 {
        let cols = Layout::horizontal([Constraint::Length(22), Constraint::Min(0)]).split(area);
        let left = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(cols[0]);
        let right = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(cols[1]);
        draw_column_header(frame, theme, left[0], "Categories", categories_focused);
        draw_group_column(frame, app, theme, left[1], categories_focused);
        draw_column_header(frame, theme, right[0], "Settings", !categories_focused);
        (right[1], !categories_focused)
    } else {
        // Narrow: only the property column shows, so it always has the focus.
        let split = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
        draw_group_header(frame, app, theme, split[0]);
        (split[1], true)
    };

    let rows_area = draw_filter_line(frame, app, theme, rows_area);
    draw_rows(frame, app, theme, rows_area, props_focused);
    draw_message(frame, app, theme, area);
    draw_editor(frame, app, theme, area);
    draw_dir_picker(frame, app, theme, area);
}

/// A column header: `selected_bg` + `header_color` when its column has the
/// focus, dimmed otherwise (requirement 3).
fn draw_column_header(frame: &mut Frame, theme: &Theme, area: Rect, label: &str, focused: bool) {
    let style = if focused {
        Style::default()
            .fg(theme.header_color)
            .bg(theme.selected_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.dim_text)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {label}"), style))),
        area,
    );
}

fn draw_group_column(frame: &mut Frame, app: &App, theme: &Theme, area: Rect, focused: bool) {
    let labels = App::settings_group_labels();
    for (index, label) in labels.iter().enumerate() {
        if index as u16 >= area.height {
            break;
        }
        let selected = index == app.hub.settings.group_idx;
        let style = if selected && focused {
            Style::default()
                .fg(theme.header_color)
                .bg(theme.selected_bg)
                .add_modifier(Modifier::BOLD)
        } else if !focused {
            Style::default().fg(theme.dim_text)
        } else {
            Style::default().fg(theme.text_primary)
        };
        let marker = if selected && focused { "▸" } else { " " };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(format!("{marker} {label}"), style))),
            Rect::new(area.x, area.y + index as u16, area.width, 1),
        );
    }
}

fn draw_group_header(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let labels = App::settings_group_labels();
    let index = app.hub.settings.group_idx.min(labels.len() - 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {}", labels[index]),
            Style::default()
                .fg(theme.header_color)
                .add_modifier(Modifier::BOLD),
        ))),
        area,
    );
}

/// The `/` filter line, drawn above the rows while it is typed or applied.
/// Returns the area left for the rows.
fn draw_filter_line(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) -> Rect {
    let settings = &app.hub.settings;
    if (!settings.filter_mode && settings.filter.is_empty()) || area.height < 2 {
        return area;
    }
    let cursor = if settings.filter_mode { "█" } else { "" };
    let line = Line::from(vec![
        Span::styled(
            " / ",
            Style::default()
                .fg(theme.header_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{}{cursor}", settings.filter),
            Style::default().fg(theme.text_primary),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line),
        Rect::new(area.x, area.y, area.width, 1),
    );
    Rect::new(area.x, area.y + 1, area.width, area.height - 1)
}

fn draw_rows(frame: &mut Frame, app: &App, theme: &Theme, area: Rect, props_focused: bool) {
    let height = area.height as usize;
    if height == 0 {
        return;
    }
    let visible = app.settings_visible_rows();
    let selected = app.hub.settings.row_idx;
    let start = selected.saturating_sub(height.saturating_sub(1) / 2);
    let start = clamp_scroll(selected, start, visible.len(), height);
    let end = (start + height).min(visible.len());
    // While filtering, rows come from every group, so each names its group.
    let show_group = !app.hub.settings.filter.is_empty();

    for (offset, (_, flat)) in visible[start..end].iter().enumerate() {
        let Some(row) = app.hub.settings.rows.get(*flat) else {
            continue;
        };
        let selected_row = start + offset == selected;
        let row_area = Rect::new(area.x, area.y + offset as u16, area.width, 1);
        draw_setting_row(
            frame,
            row,
            selected_row,
            show_group,
            props_focused,
            theme,
            row_area,
        );
    }
}

fn draw_setting_row(
    frame: &mut Frame,
    row: &SettingRow,
    selected: bool,
    show_group: bool,
    props_focused: bool,
    theme: &Theme,
    area: Rect,
) {
    let highlighted = selected && props_focused;
    let base = if props_focused {
        theme.text_primary
    } else {
        theme.dim_text
    };
    let marker = if highlighted { "▸" } else { " " };
    let marker_style = if highlighted {
        Style::default().fg(theme.header_color)
    } else {
        Style::default().fg(base)
    };
    let key_style = if highlighted {
        Style::default()
            .fg(theme.header_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(base).add_modifier(Modifier::BOLD)
    };

    let mut spans: Vec<Span> = vec![Span::styled(format!("{marker} "), marker_style)];
    if show_group {
        spans.push(Span::styled(
            format!("[{}] ", row.group),
            Style::default().fg(theme.dim_text),
        ));
    }
    spans.extend([
        Span::styled(row.key.clone(), key_style),
        Span::raw(" "),
        Span::styled(display_value(&row.effective), Style::default().fg(base)),
    ]);
    spans.push(Span::styled(
        format!(" (default {})", display_value(&row.default)),
        Style::default().fg(if props_focused && !effective_eq_default(row) {
            theme.text_primary
        } else {
            theme.dim_text
        }),
    ));
    if let Some(def) = find(&row.key) {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            settings_badge_for(def.apply),
            Style::default().fg(theme.muted_text),
        ));
    }
    if row.source == "env" {
        spans.push(Span::raw(" "));
        spans.push(Span::styled("env", Style::default().fg(theme.success)));
    }
    if row.status != "ok" {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(row.status.clone(), status_style(row, theme)));
    }

    let line = if highlighted {
        Line::from(spans).style(Style::default().bg(theme.selected_bg))
    } else {
        Line::from(spans)
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn status_style(row: &SettingRow, theme: &Theme) -> Style {
    if row.status == "unknown" {
        Style::default().fg(theme.error)
    } else {
        Style::default().fg(theme.warning)
    }
}

fn draw_message(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let Some(message) = &app.hub.settings.message else {
        return;
    };
    if area.height == 0 {
        return;
    }
    let style = if message.is_error {
        Style::default().fg(theme.error)
    } else {
        Style::default().fg(theme.success)
    };
    let y = area.y + area.height.saturating_sub(1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {}", message.text),
            style,
        ))),
        Rect::new(area.x, y, area.width, 1),
    );
}

fn draw_editor(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let Some(editor) = &app.hub.settings.editor else {
        return;
    };
    match editor {
        SettingsEditor::Text { key, buffer } => {
            draw_text_editor(frame, theme, area, key, buffer);
        }
        SettingsEditor::PathList { key, list, idx } => {
            draw_path_list_editor(frame, theme, area, key, list, *idx);
        }
    }
}

fn draw_text_editor(frame: &mut Frame, theme: &Theme, area: Rect, key: &str, buffer: &str) {
    let rect = centered_rect(60, 4, area);
    let inner = draw_editor_block(frame, theme, rect, key);
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(buffer.to_string(), Style::default().fg(theme.text_primary)),
            Span::styled("█", Style::default().fg(theme.header_color)),
        ])),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Enter save   Esc cancel",
            Style::default().fg(theme.dim_text),
        ))),
        rows[1],
    );
}

fn draw_path_list_editor(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    key: &str,
    list: &[String],
    idx: usize,
) {
    let height = (list.len().min(10) as u16) + 3;
    let rect = centered_rect(60, height, area);
    let inner = draw_editor_block(frame, theme, rect, key);
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);

    let mut lines: Vec<Line> = Vec::new();
    for (i, entry) in list.iter().enumerate() {
        let style = if i == idx {
            Style::default().fg(theme.accent_fg).bg(theme.selected_bg)
        } else {
            Style::default().fg(theme.text_primary)
        };
        lines.push(Line::from(Span::styled(format!(" {entry}"), style)));
    }
    frame.render_widget(Paragraph::new(lines), rows[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "a add   d delete   Shift+↑↓ reorder   Enter save   Esc cancel",
            Style::default().fg(theme.dim_text),
        ))),
        rows[1],
    );
}

/// Draw the editor's framed box, clearing beneath it, and return its inner
/// area.
fn draw_editor_block(frame: &mut Frame, theme: &Theme, rect: Rect, key: &str) -> Rect {
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .title(format!(" {key} "))
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.header_color))
        .style(Style::default().bg(theme.dialog_bg));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

fn draw_dir_picker(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let Some(picker) = &app.hub.settings.dir_picker else {
        return;
    };
    let height = (picker.entries.len().min(12) as u16) + 3;
    let rect = centered_rect(60, height, area);
    let title = picker.current.to_string_lossy().to_string();
    let inner = draw_editor_block(frame, theme, rect, &title);
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);

    let mut lines: Vec<Line> = Vec::new();
    for (i, entry) in picker.entries.iter().enumerate() {
        let style = if i == picker.idx {
            Style::default().fg(theme.accent_fg).bg(theme.selected_bg)
        } else {
            Style::default().fg(theme.text_primary)
        };
        lines.push(Line::from(Span::styled(format!(" {entry}/"), style)));
    }
    frame.render_widget(Paragraph::new(lines), rows[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Enter open   ← up   Space add this folder   Esc cancel",
            Style::default().fg(theme.dim_text),
        ))),
        rows[1],
    );
}
