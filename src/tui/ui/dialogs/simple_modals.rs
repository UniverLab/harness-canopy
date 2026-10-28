use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use super::centered_rect;
use crate::tui::app::types::App;
use crate::tui::ui::theme::Theme;

pub fn draw_quit_confirm(frame: &mut Frame, theme: &Theme) {
    draw_modal_confirm(
        frame,
        " Quit? ",
        "Press y/Enter to quit, any key to cancel",
        theme,
        theme.dialog_bg,
    );
}

pub fn draw_delete_project_confirm(frame: &mut Frame, theme: &Theme) {
    draw_modal_confirm(
        frame,
        " Delete Project? ",
        "Are you sure you want to delete this project?\nY/Enter = Confirm  N/Esc = Cancel",
        theme,
        theme.warn_bg,
    );
}

pub fn draw_archive_graph_confirm(frame: &mut Frame, theme: &Theme) {
    draw_modal_confirm(
        frame,
        " Archive Graph? ",
        "Archive this graph? It leaves the main list but its specs and run history stay intact — restore it anytime from the archive.\nY/Enter = Confirm  N/Esc = Cancel",
        theme,
        theme.dialog_bg,
    );
}

pub fn draw_permanent_delete_graph_confirm(frame: &mut Frame, theme: &Theme) {
    draw_modal_confirm(
        frame,
        " Permanently Delete Graph? ",
        "Permanently delete this graph? This destroys its full run history — every node run, output, and session id — forever. This cannot be undone.\nY/Enter = Confirm  N/Esc = Cancel",
        theme,
        theme.warn_bg,
    );
}

/// Confirmation for `graph_control` (`x` on a completed/failed graph) — same
/// wording as the CLI's own prompt (`daemon::graph_cli::confirm_reset`), so
/// the two surfaces never teach different levels of caution.
pub fn draw_graph_reset_confirm(frame: &mut Frame, app: &App, theme: &Theme) {
    let graph_name = app
        .selected_graph()
        .map(|lp| lp.name.as_str())
        .unwrap_or("");
    draw_modal_confirm(
        frame,
        " Reset Graph? ",
        &format!(
            "Reset graph '{graph_name}' back to pending? This clears progress on its non-completed specs.\ny: reset, n/Esc: abort"
        ),
        theme,
        theme.warn_bg,
    );
}

fn draw_modal_confirm(frame: &mut Frame, title: &str, text: &str, theme: &Theme, chrome_bg: Color) {
    let dialog_width = frame.area().width * 40 / 100;
    let inner_width = dialog_width.saturating_sub(2).max(1);
    let chars_per_line = inner_width as usize;
    let needed_lines = text
        .split('\n')
        .map(|line| (line.len().div_ceil(chars_per_line)).max(1) as u16)
        .sum::<u16>();
    let height = needed_lines + 2; // +2 for borders

    let area = centered_rect(40, height, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(title)
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.header_color))
        .style(Style::default().bg(chrome_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let msg = Paragraph::new(text)
        .style(Style::default().fg(theme.header_color))
        .alignment(ratatui::layout::Alignment::Center)
        .wrap(ratatui::widgets::Wrap { trim: true });
    frame.render_widget(msg, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn draw_quit_confirm_renders_without_panic() {
        use crate::db::Database;
        use std::sync::Arc;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let _app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_quit_confirm(frame, &theme);
            })
            .unwrap();
    }

    #[test]
    fn draw_delete_project_confirm_renders_without_panic() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_delete_project_confirm(frame, &theme);
            })
            .unwrap();
    }

    #[test]
    fn draw_archive_graph_confirm_renders_without_panic() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_archive_graph_confirm(frame, &theme);
            })
            .unwrap();
    }

    #[test]
    fn draw_permanent_delete_graph_confirm_renders_without_panic() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_permanent_delete_graph_confirm(frame, &theme);
            })
            .unwrap();
    }

    #[test]
    fn draw_modal_confirm_narrow_width() {
        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_modal_confirm(frame, " Title ", "Short text", &theme, theme.dialog_bg);
            })
            .unwrap();
    }

    #[test]
    fn draw_modal_confirm_very_narrow_width() {
        let backend = TestBackend::new(10, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_modal_confirm(frame, " Title ", "Text", &theme, theme.dialog_bg);
            })
            .unwrap();
    }

    #[test]
    fn draw_modal_confirm_long_text_wraps() {
        let backend = TestBackend::new(40, 15);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        let long_text =
            "This is a very long text that should wrap across multiple lines in the dialog box";
        terminal
            .draw(|frame| {
                draw_modal_confirm(frame, " Title ", long_text, &theme, theme.dialog_bg);
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
        assert!(text.contains("Title"), "Should show title: {text}");
    }

    #[test]
    fn draw_modal_confirm_multiline_text() {
        let backend = TestBackend::new(40, 15);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_modal_confirm(
                    frame,
                    " Title ",
                    "Line 1\nLine 2\nLine 3",
                    &theme,
                    theme.dialog_bg,
                );
            })
            .unwrap();
    }

    #[test]
    fn draw_quit_confirm_shows_quit_text() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_quit_confirm(frame, &theme);
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
        assert!(text.contains("Quit"), "Should show Quit: {text}");
    }

    /// Destructive confirmations (delete project, permanent graph delete,
    /// graph reset) sit on `warn_bg` while keeping their text label; the
    /// non-destructive quit/archive confirms stay on `dialog_bg`.
    #[test]
    fn destructive_confirms_use_warn_bg_with_labels_intact() {
        for (draw, label) in [
            (
                draw_delete_project_confirm as fn(&mut ratatui::Frame, &Theme),
                "Delete Project",
            ),
            (
                draw_permanent_delete_graph_confirm as fn(&mut ratatui::Frame, &Theme),
                "Permanently Delete Graph",
            ),
        ] {
            let backend = TestBackend::new(80, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            let theme = Theme::classic();
            terminal
                .draw(|frame| {
                    draw(frame, &theme);
                })
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            let mut text = String::new();
            let mut tinted = 0;
            for y in 0..buffer.area.height {
                for x in 0..buffer.area.width {
                    let cell = &buffer[(x, y)];
                    text.push_str(cell.symbol());
                    if cell.bg == theme.warn_bg {
                        tinted += 1;
                    }
                }
                text.push('\n');
            }
            assert!(text.contains(label), "Should keep its label: {text}");
            assert!(
                tinted >= 1,
                "{label} must paint cells on warn_bg, found none"
            );
        }
    }

    #[test]
    fn graph_reset_confirm_uses_warn_bg_with_label_intact() {
        use crate::db::Database;
        use std::sync::Arc;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_graph_reset_confirm(frame, &app, &theme);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        let mut tinted = 0;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                text.push_str(cell.symbol());
                if cell.bg == theme.warn_bg {
                    tinted += 1;
                }
            }
            text.push('\n');
        }
        assert!(
            text.contains("Reset Graph"),
            "Should keep its label: {text}"
        );
        assert!(
            tinted >= 1,
            "graph reset must paint cells on warn_bg, found none"
        );
    }

    #[test]
    fn quit_and_archive_confirms_stay_on_dialog_bg() {
        for draw in [
            draw_quit_confirm as fn(&mut ratatui::Frame, &Theme),
            draw_archive_graph_confirm as fn(&mut ratatui::Frame, &Theme),
        ] {
            let backend = TestBackend::new(80, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            let theme = Theme::classic();
            terminal
                .draw(|frame| {
                    draw(frame, &theme);
                })
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            let mut tinted = 0;
            for y in 0..buffer.area.height {
                for x in 0..buffer.area.width {
                    if buffer[(x, y)].bg == theme.dialog_bg {
                        tinted += 1;
                    }
                    assert_ne!(
                        buffer[(x, y)].bg,
                        theme.warn_bg,
                        "non-destructive confirm must not use warn_bg"
                    );
                }
            }
            assert!(tinted >= 1, "confirm must paint on dialog_bg");
        }
    }
}
