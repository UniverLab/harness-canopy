//! CT31: the F1 Canopy hub overlay — the tab bar, the body dispatch to the
//! three tabs, and the footer hint.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use super::{
    centered_rect, draw_dialog_left_wave, hub_keys, hub_legend, hub_remote, hub_settings, hub_shelf,
};
use crate::tui::app::hub::{HubTab, HUB_TABS};
use crate::tui::app::hub_settings::SettingsFocus;
use crate::tui::app::types::App;
use crate::tui::ui::effective_animation_tick;
use crate::tui::ui::theme::Theme;

/// Draw the hub as a centered dialog (requirement 1): 80% of the frame's
/// width and height, cleared only within its own area.
pub fn draw_hub(frame: &mut Frame, app: &mut App, theme: &Theme) {
    let frame_area = frame.area();
    let height = frame_area.height * 4 / 5;
    let area = centered_rect(80, height, frame_area);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Canopy Hub ")
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.header_color))
        .style(Style::default().bg(theme.dialog_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    draw_dialog_left_wave(frame, area, effective_animation_tick(app).into());

    if area.width >= 80 {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);
        draw_tab_bar(frame, app, theme, rows[0]);
        draw_body(frame, app, theme, rows[1]);
        draw_footer(frame, app, theme, rows[2]);
    } else {
        // Narrow: no tab bar, so the Settings tab's own narrow rule (right
        // column only, with the group shown as a header) is what the body shows.
        let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);
        draw_body(frame, app, theme, rows[0]);
        draw_footer(frame, app, theme, rows[1]);
    }

    // The pairing overlay floats over everything else in the hub.
    if app.hub.tab == HubTab::Remote {
        hub_remote::draw_pair_overlay(frame, app, theme);
    }
}

fn draw_tab_bar(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();
    for tab in HUB_TABS {
        let active = tab == app.hub.tab;
        let style = if active {
            // The pill pair (`accent_fg` on `header_color`, panel/mod.rs and
            // knowledge_tab.rs): ink on the accent, never on a dark raise.
            Style::default()
                .fg(theme.accent_fg)
                .bg(theme.header_color)
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
        HubTab::Shelf => hub_shelf::draw_shelf_tab(frame, app, theme, area),
        HubTab::Remote => hub_remote::draw_remote_tab(frame, app, theme, area),
    }
}

fn draw_footer(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let text = Line::from(Span::styled(
        footer_hint(app),
        Style::default().fg(theme.dim_text),
    ));
    frame.render_widget(Paragraph::new(text), area);
}

/// The keys of the current tab and focus level (requirement 5).
fn footer_hint(app: &App) -> &'static str {
    match app.hub.tab {
        HubTab::Settings => match app.hub.settings.focus {
            SettingsFocus::Categories => "↑↓ category · → open · Shift+←→ tab · Esc close",
            SettingsFocus::Properties => {
                "↑↓ setting · Enter edit · Space toggle · r reset · ← back"
            }
        },
        HubTab::Keys => "↑↓ scroll · Shift+←→ tab · Esc close",
        HubTab::Missions => "↑↓ mission · Shift+←→ tab · Esc close",
        HubTab::Shelf => "Enter open   d delete   c copy path",
        HubTab::Remote => "↑↓ move · Enter select · d disconnect · r revoke · Esc close",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::tui::app::hub_settings::SettingRow;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
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
        let text = draw(&mut app, 120, 24);

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

    /// Render without flattening, for style assertions on individual cells.
    fn render_buffer(app: &mut App, width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_hub(frame, app, &theme);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_text(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    fn find_in_row(buffer: &Buffer, y: u16, needle: &str) -> Option<u16> {
        let row = row_text(buffer, y);
        row.find(needle)
            .map(|byte| row[..byte].chars().count() as u16)
    }

    fn find_marker(buffer: &Buffer) -> (u16, u16) {
        for y in 0..buffer.area.height {
            if let Some(x) = find_in_row(buffer, y, "▸") {
                return (x, y);
            }
        }
        panic!("no ▸ marker in the buffer");
    }

    /// The hub is a centered dialog of 80% of the frame, so the frame's top
    /// row is outside it and stays blank while the title renders inside.
    #[test]
    fn draw_hub_is_a_centered_dialog_not_full_screen() {
        let mut app = test_app();
        app.hub_open = true;
        app.hub.tab = HubTab::Settings;

        let text = draw(&mut app, 80, 24);
        let first = text.lines().next().unwrap_or_default();
        assert!(
            first.chars().all(|cell| cell == ' '),
            "the frame's top row is outside the hub: {first:?}"
        );
        assert!(
            text.contains("Canopy Hub"),
            "the dialog title renders inside it: {text}"
        );
    }

    /// The footer names the keys of the tab and focus level on screen.
    #[test]
    fn footer_hint_follows_the_tab_and_focus_level() {
        let mut app = test_app();
        app.hub_open = true;
        app.hub.tab = HubTab::Settings;

        app.hub.settings.focus = SettingsFocus::Categories;
        let text = draw(&mut app, 100, 24);
        assert!(text.contains("→ open"), "categories footer: {text}");
        assert!(text.contains("Esc close"), "categories footer: {text}");

        app.hub.settings.focus = SettingsFocus::Properties;
        let text = draw(&mut app, 100, 24);
        assert!(text.contains("r reset"), "properties footer: {text}");
        assert!(text.contains("← back"), "properties footer: {text}");
    }

    /// The focused column's header and selected row carry header_color on
    /// selected_bg with the ▸ marker; the other column is dimmed.
    #[test]
    fn settings_focus_highlights_the_focused_column() {
        let mut app = test_app();
        app.hub_open = true;
        app.hub.tab = HubTab::Settings;
        app.hub.settings.rows = vec![row("theme", "\"classic\"", "file", "ok")];
        let theme = Theme::classic();

        // Categories focused: the marker sits on the selected category row.
        app.hub.settings.focus = SettingsFocus::Categories;
        let buffer = render_buffer(&mut app, 120, 24);
        let (marker_x, marker_y) = find_marker(&buffer);
        let grid = (0..buffer.area.height)
            .map(|y| row_text(&buffer, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(buffer[(marker_x, marker_y)].bg, theme.selected_bg);
        assert_eq!(buffer[(marker_x, marker_y)].fg, theme.header_color);
        assert!(
            row_text(&buffer, marker_y).contains("General"),
            "marker at ({marker_x},{marker_y}) grid:\n{grid}"
        );
        let header_y = marker_y - 1;
        let categories_x = find_in_row(&buffer, header_y, "Categories").unwrap();
        let settings_x = find_in_row(&buffer, header_y, "Settings").unwrap();
        assert_eq!(buffer[(categories_x, header_y)].fg, theme.header_color);
        assert_eq!(
            buffer[(settings_x, header_y)].fg,
            theme.dim_text,
            "the unfocused column header is dimmed"
        );
        let property_y = (0..buffer.area.height)
            .find(|y| row_text(&buffer, *y).contains("theme"))
            .expect("property row renders");
        let key_x = find_in_row(&buffer, property_y, "theme").unwrap();
        assert_eq!(
            buffer[(key_x, property_y)].fg,
            theme.dim_text,
            "the unfocused property row is dimmed"
        );

        // Properties focused: the marker moves to the property row.
        app.hub.settings.focus = SettingsFocus::Properties;
        let buffer = render_buffer(&mut app, 120, 24);
        let (marker_x, marker_y) = find_marker(&buffer);
        assert_eq!(buffer[(marker_x, marker_y)].bg, theme.selected_bg);
        assert_eq!(buffer[(marker_x, marker_y)].fg, theme.header_color);
        assert!(row_text(&buffer, marker_y).contains("theme"));
        let header_y = marker_y - 1;
        let categories_x = find_in_row(&buffer, header_y, "Categories").unwrap();
        assert_eq!(
            buffer[(categories_x, header_y)].fg,
            theme.dim_text,
            "the unfocused categories header is dimmed"
        );
    }

    /// The active hub tab uses the pill pair (`accent_fg` on
    /// `header_color`, bold) and the inactive tabs are dim — so the
    /// highlighted tab stays readable on the dark dialog background.
    #[test]
    fn draw_hub_active_tab_uses_accent_pair() {
        let mut app = test_app();
        app.hub_open = true;
        app.hub.tab = HubTab::Keys;
        let theme = Theme::classic();
        let buffer = render_buffer(&mut app, 100, 30);

        // The tab bar is draw_hub's rows[0]: the first inner row, right
        // under the title's border row.
        let title_y = (0..buffer.area.height)
            .find(|y| row_text(&buffer, *y).contains("Canopy Hub"))
            .expect("hub title renders");
        let tab_y = title_y + 1;

        let mut inactive_seen = false;
        for tab in HUB_TABS {
            let active = tab == app.hub.tab;
            let needle = format!(" {} ", tab.label());
            let x = find_in_row(&buffer, tab_y, &needle)
                .unwrap_or_else(|| panic!("tab label {needle:?} not in row {tab_y}"));
            let cell = &buffer[(x, tab_y)];
            if active {
                assert_eq!(cell.fg, theme.accent_fg, "active tab {tab:?} fg");
                assert_eq!(cell.bg, theme.header_color, "active tab {tab:?} bg");
                assert!(
                    cell.modifier.contains(Modifier::BOLD),
                    "active tab {tab:?} must be bold"
                );
            } else {
                assert_eq!(cell.fg, theme.dim_text, "inactive tab {tab:?} must be dim");
                assert_eq!(
                    cell.bg, theme.dialog_bg,
                    "inactive tab {tab:?} sits on the dialog ground, not a raise"
                );
                inactive_seen = true;
            }
        }
        assert!(inactive_seen, "at least one inactive tab was checked");
    }
}
