//! The hub's Shelf tab — every shelf's files merged, newest first: one row
//! per file with its time, sender, name, size and (dim) caption; the
//! selected row is marked, and `d`'s confirmation shows on the last line.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::sessions::protocol::ShelfItem;
use crate::tui::app::types::App;
use crate::tui::ui::theme::Theme;

pub(super) fn draw_shelf_tab(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    if app.hub.shelf.items.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No shelf files yet — ask an agent for `session_send_file`.",
                Style::default().fg(theme.dim_text),
            ))),
            area,
        );
        return;
    }

    let confirm_rows = if app.hub.shelf.delete_confirm { 1 } else { 0 };
    let [list_area, confirm_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(confirm_rows)]).areas(area);

    let height = list_area.height as usize;
    let start = crate::tui::selection::clamp_scroll(
        app.hub.shelf.sel,
        0,
        app.hub.shelf.items.len(),
        height,
    );
    let end = (start + height).min(app.hub.shelf.items.len());
    let lines: Vec<Line> = (start..end)
        .map(|index| {
            shelf_row(
                &app.hub.shelf.items[index],
                index == app.hub.shelf.sel,
                list_area.width as usize,
                theme,
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list_area);

    if app.hub.shelf.delete_confirm {
        draw_delete_confirm(frame, app, theme, confirm_area);
    }
}

/// One `HH:MM  from  name  size  caption` row; the selected one carries the
/// `▸` marker on `selected_bg`.
fn shelf_row(item: &ShelfItem, selected: bool, width: usize, theme: &Theme) -> Line<'static> {
    let marker_style = if selected {
        Style::default()
            .fg(theme.header_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.dim_text)
    };
    let name_style = if selected {
        // The pill pair (`accent_fg` on `header_color`): ink on the
        // accent; the row's `selected_bg` raise covers the rest of the
        // line, so the name never sits dark on dark.
        Style::default()
            .fg(theme.accent_fg)
            .bg(theme.header_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text_primary)
    };

    let mut rest = width;
    let mut spans = vec![Span::styled(if selected { "▸" } else { " " }, marker_style)];
    rest = rest.saturating_sub(1);
    let push = |spans: &mut Vec<Span<'static>>, rest: &mut usize, text: String, style: Style| {
        if *rest == 0 {
            return;
        }
        let text = crate::tui::ui::truncate_str(&text, *rest);
        *rest -= text.chars().count();
        spans.push(Span::styled(text, style));
    };

    let dim = Style::default().fg(theme.dim_text);
    push(&mut spans, &mut rest, " ".to_string(), Style::default());
    push(&mut spans, &mut rest, shelf_time(&item.created_at), dim);
    push(&mut spans, &mut rest, "  ".to_string(), Style::default());
    push(&mut spans, &mut rest, item.from_label.clone(), dim);
    push(&mut spans, &mut rest, "  ".to_string(), Style::default());
    push(&mut spans, &mut rest, item.name.clone(), name_style);
    push(&mut spans, &mut rest, "  ".to_string(), Style::default());
    push(&mut spans, &mut rest, human_size(item.size), dim);
    if let Some(caption) = item.caption.as_deref().filter(|c| !c.is_empty()) {
        push(&mut spans, &mut rest, "  ".to_string(), Style::default());
        push(&mut spans, &mut rest, caption.to_string(), dim);
    }

    let line = Line::from(spans);
    if selected {
        line.style(Style::default().bg(theme.selected_bg))
    } else {
        line
    }
}

/// The confirmation line pinned to the tab's last row.
fn draw_delete_confirm(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let Some(item) = app.hub.shelf.items.get(app.hub.shelf.sel) else {
        return;
    };
    let text = format!("Delete {}?  y = confirm  n = cancel", item.name);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            crate::tui::ui::truncate_str(&text, area.width as usize),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ))),
        area,
    );
}

/// `created_at` as `HH:MM`, falling back to the raw string when it does not
/// parse as RFC3339. Shared with the Remote tab's device rows.
pub(super) fn shelf_time(created_at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(created_at)
        .map(|dt| dt.format("%H:%M").to_string())
        .unwrap_or_else(|_| created_at.to_string())
}

/// Bytes as `B` / `KiB` / `MiB`.
fn human_size(bytes: i64) -> String {
    let bytes = bytes.max(0) as f64;
    if bytes < 1024.0 {
        format!("{bytes:.0} B")
    } else if bytes < 1024.0 * 1024.0 {
        format!("{:.1} KiB", bytes / 1024.0)
    } else {
        format!("{:.1} MiB", bytes / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::Arc;

    fn test_app() -> App {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap()
    }

    fn item(name: &str, from: &str, created_at: &str, caption: Option<&str>) -> ShelfItem {
        ShelfItem {
            file_id: "0000000000000001".to_string(),
            session_id: None,
            name: name.to_string(),
            mime: "image/png".to_string(),
            size: 2048,
            caption: caption.map(str::to_string),
            from_label: from.to_string(),
            created_at: created_at.to_string(),
        }
    }

    fn draw(app: &App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_shelf_tab(frame, app, &theme, frame.area());
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn shelf_tab_lists_items_newest_first_with_their_metadata() {
        let mut app = test_app();
        app.hub.shelf.items = vec![
            item(
                "new.png",
                "ada",
                "2026-01-01T11:05:00Z",
                Some("the newest file"),
            ),
            item("old.png", "host", "2026-01-01T10:00:00Z", None),
        ];

        let text = draw(&app, 100, 12);
        assert!(text.contains("11:05"), "arrival time: {text}");
        assert!(text.contains("ada"), "sender label: {text}");
        assert!(text.contains("new.png"), "file name: {text}");
        assert!(text.contains("2.0 KiB"), "human size: {text}");
        assert!(text.contains("the newest file"), "caption: {text}");
        assert!(
            text.contains("old.png"),
            "the older item renders too: {text}"
        );
        let new_at = text.find("new.png").expect("new.png renders");
        let old_at = text.find("old.png").expect("old.png renders");
        assert!(new_at < old_at, "newest first: {text}");
    }

    #[test]
    fn shelf_tab_marks_the_selected_row_and_shows_the_confirmation() {
        let mut app = test_app();
        app.hub.shelf.items = vec![
            item("a.png", "host", "2026-01-01T10:00:00Z", None),
            item("b.png", "host", "2026-01-01T11:00:00Z", None),
        ];
        app.hub.shelf.sel = 1;

        let text = draw(&app, 100, 12);
        let b_line = text
            .lines()
            .find(|line| line.contains("b.png"))
            .expect("b.png renders");
        assert!(
            b_line.contains('▸'),
            "the ▸ marker sits on the selected row: {text}"
        );
        let a_line = text
            .lines()
            .find(|line| line.contains("a.png"))
            .expect("a.png renders");
        assert!(!a_line.contains('▸'), "only the selected row: {text}");

        app.hub.shelf.delete_confirm = true;
        let text = draw(&app, 100, 12);
        assert!(
            text.contains("Delete b.png?  y = confirm  n = cancel"),
            "confirmation line: {text}"
        );
    }

    #[test]
    fn shelf_tab_empty_state_names_the_tool() {
        let app = test_app();
        let text = draw(&app, 100, 12);
        assert!(text.contains("No shelf files yet"), "{text}");
        assert!(text.contains("session_send_file"), "{text}");
    }

    #[test]
    fn human_size_scales_to_the_unit() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(20 * 1024 * 1024), "20.0 MiB");
        assert_eq!(human_size(-5), "0 B");
    }

    #[test]
    fn shelf_time_falls_back_to_the_raw_string() {
        assert_eq!(shelf_time("2026-01-01T11:05:00Z"), "11:05");
        assert_eq!(shelf_time("not-a-date"), "not-a-date");
    }
}
