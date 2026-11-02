//! Blocking daemon-version notice: drawn last, above every other dialog.

use ratatui::Frame;

use super::simple_modals::draw_modal_confirm;
use crate::tui::app::types::App;
use crate::tui::ui::theme::Theme;

/// Draw the version notice, if any. Keys are intercepted while it is up (see
/// `event::version_notice`), so this is purely the overlay itself.
pub fn draw_version_notice_dialog(frame: &mut Frame, app: &App, theme: &Theme) {
    let Some(notice) = app.version_notice.as_ref() else {
        return;
    };
    let text = notice.message_lines().join("\n");
    draw_modal_confirm(frame, " Daemon Version ", &text, theme, theme.warn_bg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::tui::app::version_check::VersionNotice;

    fn rendered_with(notice: Option<VersionNotice>) -> String {
        let tmp = tempfile::NamedTempFile::new().expect("scratch db file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = std::sync::Arc::new(crate::db::Database::new(&path).expect("scratch db"));
        let data_dir = tempfile::tempdir().expect("scratch dir");
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("test app");
        app.version_notice = notice;
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| draw_version_notice_dialog(frame, &app, &app.theme))
            .expect("draw");
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
    fn mismatch_renders_both_versions() {
        let text = rendered_with(Some(VersionNotice::mismatch(
            "1.2.3-fixture",
            "9.9.9-fixture".to_string(),
        )));
        assert!(text.contains("1.2.3-fixture"), "screen: {text}");
        assert!(text.contains("9.9.9-fixture"), "screen: {text}");
        assert!(text.contains('R'), "screen: {text}");
    }

    #[test]
    fn no_notice_renders_nothing() {
        let text = rendered_with(None);
        assert!(
            !text.contains("Daemon version"),
            "screen must be empty: {text}"
        );
    }
}
