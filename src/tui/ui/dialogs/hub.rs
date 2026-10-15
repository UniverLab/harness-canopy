//! CT31: the F1 Canopy hub overlay — the tab bar, the body dispatch to the
//! three tabs, and the footer hint.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use super::{draw_dialog_left_wave, hub_keys, hub_legend, hub_settings};
use crate::tui::app::hub::{HubTab, HUB_TABS};
use crate::tui::app::types::App;
use crate::tui::ui::theme::Theme;

/// Draw the full-screen hub.
pub fn draw_hub(frame: &mut Frame, app: &mut App, theme: &Theme) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Canopy Hub ")
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.header_color))
        .style(Style::default().bg(theme.dialog_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    draw_dialog_left_wave(frame, area, app.animation_tick.into());

    if area.width >= 80 {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);
        draw_tab_bar(frame, app, theme, rows[0]);
        draw_body(frame, app, theme, rows[1]);
        draw_footer(frame, theme, rows[2]);
    } else {
        // Narrow: no tab bar, so the Settings tab's own narrow rule (right
        // column only, with the group shown as a header) is what the body shows.
        let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);
        draw_body(frame, app, theme, rows[0]);
        draw_footer(frame, theme, rows[1]);
    }
}

fn draw_tab_bar(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();
    for tab in HUB_TABS {
        let active = tab == app.hub.tab;
        let style = if active {
            Style::default()
                .fg(theme.accent_fg)
                .bg(theme.selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.dim_text)
        };
        spans.push(Span::styled(format!(" {} ", tab.label()), style));
        spans.push(Span::raw(" "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_body(frame: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    match app.hub.tab {
        HubTab::Settings => hub_settings::draw_settings_tab(frame, app, theme, area),
        HubTab::Keys => hub_keys::draw_keys_tab(frame, app, theme, area),
        HubTab::Missions => hub_legend::draw_missions_tab(frame, app, theme, area),
    }
}

fn draw_footer(frame: &mut Frame, theme: &Theme, area: Rect) {
    let text = Line::from(Span::styled(
        "F1/Esc close   Tab/1·2·3 tabs",
        Style::default().fg(theme.dim_text),
    ));
    frame.render_widget(Paragraph::new(text), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::tui::app::hub_settings::SettingRow;
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

    fn row(key: &str, effective: &str, source: &str, status: &str) -> SettingRow {
        SettingRow {
            key: key.to_string(),
            group: "General".to_string(),
            description: format!("{key} description"),
            effective: effective.to_string(),
            default: "\"classic\"".to_string(),
            source: source.to_string(),
            status: status.to_string(),
        }
    }

    fn draw(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_hub(frame, app, &theme);
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
    fn draw_hub_renders_settings_tab() {
        let mut app = test_app();
        app.hub_open = true;
        app.hub.tab = HubTab::Settings;
        app.hub.settings.rows = vec![
            // One row per status/source badge, plus one row per apply mode:
            // `theme` and `tui.panel_dwell_seconds` are `hot`,
            // `scheduler.retry_max` is `next run`, `spec_attempt_limit` is
            // `restart daemon` and `temperature_unit` is `restart tui` (the
            // registry's only `Apply::RestartTui` key).
            row("theme", "\"classic\"", "file", "ok"),
            row("tui.panel_dwell_seconds", "10", "file", "pending_restart"),
            row("spec_attempt_limit", "0", "file", "invalid: 1..=20"),
            row("mystery_key", "1", "file", "unknown"),
            row("scheduler.retry_max", "5", "env", "ok"),
            row("temperature_unit", "\"fahrenheit\"", "file", "ok"),
        ];
        let text = draw(&mut app, 80, 24);

        assert!(text.contains("General"), "group label: {text}");
        assert!(text.contains("theme"), "known key: {text}");
        assert!(text.contains("pending_restart"), "status badge: {text}");
        assert!(text.contains("invalid"), "invalid badge: {text}");
        assert!(text.contains("env"), "env source badge: {text}");
        // One assertion per apply mode: dropping any `Apply::badge` arm fails.
        assert!(text.contains("hot"), "hot apply badge: {text}");
        assert!(text.contains("next run"), "next run apply badge: {text}");
        assert!(
            text.contains("restart daemon"),
            "restart daemon apply badge: {text}"
        );
        assert!(
            text.contains("restart tui"),
            "restart tui apply badge: {text}"
        );
        // The default always renders, dimmed when equal to the effective value.
        assert!(text.contains("(default"), "default shown: {text}");
    }

    #[test]
    fn draw_hub_renders_keys_tab() {
        use crate::tui::keymap_reference::{rows_for, CONTEXTS};

        let mut app = test_app();
        app.hub_open = true;
        app.hub.tab = HubTab::Keys;

        // The whole table is taller than a 24-row frame, so scroll to each
        // context's header and assert it — and its rows — render.
        let mut offset = 0usize;
        for context in CONTEXTS {
            app.hub.keys_scroll = offset;
            let text = draw(&mut app, 80, 24);
            assert!(
                text.contains(context),
                "context {context} not rendered: {text}"
            );
            if *context == "Home and sidebar" {
                assert!(text.contains("F10"), "a key row renders: {text}");
            }
            offset += 1 + rows_for(context).count();
        }
    }

    #[test]
    fn draw_hub_renders_missions_tab() {
        let mut app = test_app();
        app.hub_open = true;
        app.hub.tab = HubTab::Missions;
        let text = draw(&mut app, 80, 24);
        assert!(text.contains("Missions"), "{text}");
    }

    #[test]
    fn draw_hub_narrow_settings_shows_right_column_only() {
        let mut app = test_app();
        app.hub_open = true;
        app.hub.tab = HubTab::Settings;
        app.hub.settings.rows = vec![row("theme", "\"classic\"", "file", "ok")];
        let text = draw(&mut app, 60, 24);

        assert!(text.contains("General"), "selected group header: {text}");
        assert!(
            !text.contains("TUI & appearance"),
            "left-column-only labels must not show when narrow: {text}"
        );
    }
}
