//! Read-only knowledge viewer dialog — one entry's body, scrollable.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::app::types::App;
use crate::tui::ui::theme::Theme;

pub fn draw_knowledge_viewer(frame: &mut Frame, app: &mut App, theme: &Theme) {
    let Some(dialog) = app.knowledge_viewer.as_mut() else {
        return;
    };

    let dialog_area = centered_rect(60, 50, frame.area());

    let block = Block::default()
        .title(format!(" {} · {} ", dialog.kind, dialog.title))
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.header_color))
        .style(Style::default().bg(theme.dialog_bg));

    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let lines: Vec<Line> = dialog.body.lines().map(Line::from).collect();
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });

    // `scroll` counts *wrapped* rows, so the bound is the body's rendered line
    // count minus the rows on screen — the drawer owns the width and height
    // the key handler cannot see. Bodies are routinely a single flowing
    // `\n`-less paragraph (agents write them as one blob), and a bound counted
    // in hard lines left such an entry unscrollable: its tail was off screen
    // with no way to reach it.
    dialog.max_scroll = (paragraph.line_count(inner.width) as u16).saturating_sub(inner.height);
    dialog.scroll = dialog.scroll.min(dialog.max_scroll);

    frame.render_widget(paragraph.scroll((dialog.scroll, 0)), inner);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(r);

    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::tui::app::types::Focus;
    use crate::tui::event::knowledge_viewer::{handle_knowledge_viewer_key, open_knowledge_viewer};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;
    use ratatui::Terminal;
    use std::sync::Arc;

    const WIDTH: u16 = 80;
    const HEIGHT: u16 = 24;

    fn app_with_body(body: String) -> (App, tempfile::TempDir) {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).expect("create test db"));
        let data_dir = tempfile::tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.project_knowledge = vec![crate::db::intelligence::IntelligenceNodeRecord {
            id: "n1".to_string(),
            kind: "fact".to_string(),
            status: "noted".to_string(),
            title: "A long entry".to_string(),
            body,
            metadata: None,
            project_hash: None,
            session_id: None,
            created_at: 0,
            updated_at: 0,
        }];
        app.selected_knowledge = 0;
        app.focus = Focus::Agent;
        open_knowledge_viewer(&mut app);
        assert!(app.knowledge_viewer.is_some(), "viewer must open");
        (app, data_dir)
    }

    /// Draw the viewer over a `WIDTH`x`HEIGHT` terminal and read the cells
    /// back as text — exactly what the user sees.
    fn draw_to_text(app: &mut App) -> String {
        let backend = TestBackend::new(WIDTH, HEIGHT);
        let mut terminal = Terminal::new(backend).expect("create terminal");
        let theme = Theme::classic();
        terminal
            .draw(|frame| draw_knowledge_viewer(frame, app, &theme))
            .expect("draw viewer");

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

    fn key(app: &mut App, code: KeyCode) {
        handle_knowledge_viewer_key(app, code).expect("handle key");
    }

    /// FR2: `↑↓`/`PgUp`/`PgDn` scroll the wrapped body. A one-paragraph
    /// entry — the shape agents actually write — is many rendered rows
    /// despite carrying no `\n`, so its last line must be reachable, and the
    /// scroll must then stop instead of running off the end of the body.
    #[test]
    fn page_down_scrolls_a_wrapped_body_to_its_last_line() {
        let body = format!("{}THE-END", "lorem ipsum dolor ".repeat(40));
        let (mut app, _data_dir) = app_with_body(body);

        let first = draw_to_text(&mut app);
        assert!(
            !first.contains("THE-END"),
            "the tail starts below the fold:\n{first}"
        );

        for _ in 0..40 {
            key(&mut app, KeyCode::PageDown);
        }

        let dialog = app.knowledge_viewer.as_ref().expect("viewer still open");
        let max_scroll = dialog.max_scroll;
        assert!(
            max_scroll > 0,
            "a body this long must be scrollable, max_scroll was {max_scroll}"
        );
        assert_eq!(
            dialog.scroll, max_scroll,
            "PgDn must land on the last page, not past it"
        );

        let last = draw_to_text(&mut app);
        assert!(
            last.contains("THE-END"),
            "the last line must be reachable by scrolling:\n{last}"
        );
    }

    /// The other half of the same rule: a body that fits the dialog must not
    /// scroll its own first line away.
    #[test]
    fn scrolling_a_short_body_leaves_it_whole() {
        let (mut app, _data_dir) = app_with_body("short first line\nshort second line".to_string());

        for _ in 0..5 {
            key(&mut app, KeyCode::PageDown);
            key(&mut app, KeyCode::Down);
        }

        let dialog = app.knowledge_viewer.as_ref().expect("viewer still open");
        assert_eq!(dialog.max_scroll, 0, "a body that fits cannot be scrolled");
        assert_eq!(dialog.scroll, 0);

        let text = draw_to_text(&mut app);
        assert!(
            text.contains("short first line"),
            "body must stay put:\n{text}"
        );
        assert!(text.contains("short second line"));
    }

    /// `↑` walks back up through the body and stops on the first line.
    #[test]
    fn up_scrolls_back_to_the_first_line() {
        let body = "lorem ipsum dolor ".repeat(40);
        let (mut app, _data_dir) = app_with_body(body);

        draw_to_text(&mut app);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        assert_eq!(app.knowledge_viewer.as_ref().unwrap().scroll, 2);

        for _ in 0..5 {
            key(&mut app, KeyCode::Up);
        }
        assert_eq!(app.knowledge_viewer.as_ref().unwrap().scroll, 0);

        let text = draw_to_text(&mut app);
        assert!(text.contains("lorem ipsum"), "first line is back:\n{text}");
    }
}
