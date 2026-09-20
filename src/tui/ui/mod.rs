//! UI rendering — sidebar with agent cards, log panel, header, footer, and dialogs.

pub(crate) mod dialogs;
mod footer;
mod header;
mod panel;
mod sidebar;
mod system_dashboard;
pub(crate) mod theme;

use theme::Theme;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::Frame;

use super::app::types::App;

// ── Shared palette ──────────────────────────────────────────────

pub(crate) const ERROR_COLOR: Color = Color::Rgb(229, 57, 53);
pub(crate) const BG_HOVER: Color = Color::Rgb(30, 30, 30);
pub(crate) const INTERACTIVE_COLOR: Color = Color::Rgb(102, 187, 106);
pub(crate) const STATUS_DISABLED: Color = Color::Rgb(120, 120, 120);
pub(crate) const STATUS_RUNNING: Color = Color::Rgb(76, 175, 80);
// Active-session pulse phases (B21): a working PTY breathes between a
// muted gray-green and an illuminated green — never blank, so the
// indicator reads as a heartbeat instead of the status bar flickering out.
pub(crate) const STATUS_RUNNING_DIM: Color = Color::Rgb(74, 102, 77);
pub(crate) const STATUS_RUNNING_BRIGHT: Color = Color::Rgb(129, 230, 133);
pub(crate) const STATUS_OK: Color = Color::Rgb(66, 165, 245);
pub(crate) const STATUS_FAIL: Color = Color::Rgb(229, 57, 53);
pub(crate) const STATUS_WAIT_ON: Color = Color::Rgb(255, 255, 0);
pub(crate) const STATUS_WAIT_OFF: Color = Color::Rgb(30, 30, 30);

/// Border set for a themed panel: `ALL` for classic, `NONE` for modern
/// (which separates panels by background-color contrast instead).
pub(crate) fn borders_for(theme: &Theme) -> ratatui::widgets::Borders {
    if theme.show_borders {
        ratatui::widgets::Borders::ALL
    } else {
        ratatui::widgets::Borders::NONE
    }
}

/// Dialogs retain a visible edge in every theme, including modern's
/// otherwise borderless panels.
pub(crate) fn dialog_borders_for(_theme: &Theme) -> ratatui::widgets::Borders {
    ratatui::widgets::Borders::ALL
}

// ── Layout ──────────────────────────────────────────────────────

/// Width in columns of the agent sidebar when visible. Shared by the layout
/// split here and the mouse hit-testing in `tui::event` so both stay in sync.
pub(crate) const SIDEBAR_WIDTH: u16 = 33;
const MODERN_USES_SEPARATOR_COLUMN: bool = false;

// ── Main draw entry point ───────────────────────────────────────

pub fn draw(frame: &mut Frame, app: &mut App) {
    let theme = app.theme;
    let full = frame.area();
    frame.render_widget(
        ratatui::widgets::Paragraph::new("").style(Style::default().bg(theme.panel_bg)),
        full,
    );

    let [header_area, body, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let body_area = if app.sidebar_visible {
        let [sidebar, content] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(body);
        header::draw_header(frame, header_area, app, &theme);
        sidebar::draw_sidebar(frame, sidebar, app, &theme);
        if !theme.show_borders && MODERN_USES_SEPARATOR_COLUMN {
            let separator = Rect::new(sidebar.x + sidebar.width, sidebar.y, 1, sidebar.height);
            frame.render_widget(
                ratatui::widgets::Paragraph::new("").style(Style::default().bg(theme.border_color)),
                separator,
            );
        }
        content
    } else {
        header::draw_header(frame, header_area, app, &theme);
        body
    };

    // CT1 multi-face right panel: the visible face decides, and the face
    // decides whether there is anything to show. The layout width rule is
    // unchanged — only which content fills the panel moved.
    let panel_visible = app.panel_face_visible();
    let activity_width = app.activity_panel_layout_width(body_area.width, panel_visible);
    let (panel_area, sync_area) = if panel_visible {
        if activity_width > 0 {
            let [panel, sync] = Layout::horizontal([
                Constraint::Min(body_area.width.saturating_sub(activity_width)),
                Constraint::Length(activity_width),
            ])
            .areas(body_area);
            panel::draw_panel_face(frame, sync, app, &theme);
            app.last_sync_area = Some(sync);
            (panel, Some(sync))
        } else {
            app.last_sync_area = None;
            (body_area, None)
        }
    } else {
        app.last_sync_area = None;
        (body_area, None)
    };

    // Split view: render two panels side-by-side (or stacked) when a split is active
    if let Some(ref split_id) = app.active_split_id.clone() {
        if let Some(group) = app.split_groups.iter().find(|g| g.id == *split_id) {
            let session_a = group.session_a.clone();
            let session_b = group.session_b.clone();
            let orientation = group.orientation;
            let areas = match orientation {
                crate::domain::models::SplitOrientation::Horizontal => {
                    Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .areas(panel_area)
                }
                crate::domain::models::SplitOrientation::Vertical => {
                    Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .areas(panel_area)
                }
            };
            let [area_a, area_b]: [Rect; 2] = areas;
            panel::draw_split_panel(
                frame,
                area_a,
                app,
                &session_a,
                !app.split_right_focused,
                &theme,
            );
            panel::draw_split_panel(
                frame,
                area_b,
                app,
                &session_b,
                app.split_right_focused,
                &theme,
            );
        } else {
            // Group no longer exists — clear stale reference
            app.active_split_id = None;
            panel::draw_log_panel(frame, panel_area, app, &theme);
        }
    } else {
        panel::draw_log_panel(frame, panel_area, app, &theme);
    }

    footer::draw_footer(frame, footer_area, app, &theme);

    if app.new_agent_dialog.is_some() {
        dialogs::draw_new_agent_dialog(frame, app, &theme);
    }

    if app.launchpad_dialog.is_some() {
        dialogs::draw_launchpad_dialog(frame, app, &theme);
    }

    if app.quit_confirm {
        dialogs::draw_quit_confirm(frame, &theme);
    } else if app.delete_project_confirm {
        dialogs::draw_delete_project_confirm(frame, &theme);
    } else if app.archive_graph_confirm {
        dialogs::draw_archive_graph_confirm(frame, &theme);
    } else if app.permanent_delete_graph_confirm {
        dialogs::draw_permanent_delete_graph_confirm(frame, &theme);
    } else if app.graph_reset_confirm {
        dialogs::draw_graph_reset_confirm(frame, app, &theme);
    }

    if app.graph_autorun_dialog.is_some() {
        dialogs::draw_graph_autorun_dialog(frame, app, &theme);
    }

    if app.graph_action_message.is_some() {
        dialogs::draw_graph_action_message(frame, app, &theme);
    }

    if app.show_legend {
        dialogs::draw_legend(frame, app, &theme);
    }

    if app.context_transfer_modal.is_some() {
        dialogs::draw_context_transfer_modal(frame, app, &theme);
    }

    if app.rag_transfer_modal.is_some() {
        dialogs::draw_rag_transfer_modal(frame, app, &theme);
    }

    if app.simple_prompt_dialog.is_some() {
        let result = dialogs::draw_simple_prompt_dialog(frame, app, &theme);
        if let Some((tab_origin, content_rect)) = result {
            app.prompt_tab_origin = Some(tab_origin);
            app.prompt_raw_content_rect = content_rect;
        }
    }

    if app.graph_editor_dialog.is_some() {
        dialogs::draw_graph_editor_dialog(frame, app, &theme);
    }

    if app.graph_form_dialog.is_some() {
        dialogs::draw_graph_form_dialog(frame, app, &theme);
    }

    if app.knowledge_dialog.is_some() {
        dialogs::draw_knowledge_dialog(frame, app, &theme);
    }

    if app.split_picker_open {
        dialogs::draw_split_picker(frame, app, &theme);
    }

    if app.panel_picker_open {
        draw_panel_picker(frame, app, &theme);
    }

    if app.suggestion_picker.is_some() {
        dialogs::draw_suggestion_picker(frame, app, panel_area, &theme);
    }

    // Terminal search bar overlay (Ctrl+F)
    if let Some(search) = &app.terminal_search {
        let w = panel_area.width.min(50);
        let x = panel_area.x + panel_area.width.saturating_sub(w + 1);
        let y = panel_area.y;
        let area = Rect::new(x, y, w, 1);
        let match_info = if search.match_rows.is_empty() {
            if search.query.is_empty() {
                String::new()
            } else {
                " (no matches)".to_string()
            }
        } else {
            format!(" {}/{}", search.current_match + 1, search.match_rows.len())
        };
        let text = format!(" 🔍 {}{} ", search.query, match_info);
        let style = ratatui::style::Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(255, 235, 59));
        frame.render_widget(ratatui::widgets::Paragraph::new(text).style(style), area);
    }

    // Top-level overlays rendered last so they appear above all content
    if app.show_copied {
        let full = frame.area();
        let msg = " \u{2592} COPIED \u{2592} "; // ▒ COPIED ▒
        let w = msg.chars().count() as u16; // display width (char count, not bytes)
        if full.width > w + 2 {
            let x = full.x + full.width - w - 1;
            let y = full.y + 1; // just below header
            let area = ratatui::layout::Rect::new(x, y, w, 1);
            let widget = ratatui::widgets::Paragraph::new(msg).style(
                ratatui::style::Style::default()
                    .fg(theme.header_color)
                    .bg(Color::Black),
            );
            frame.render_widget(widget, area);
        }
    }

    // Atmosphere particles — absolute top layer, drawn last over everything
    if !app.atmosphere_hidden {
        let area = frame.area();
        app.atmosphere.tick(area, &mut app.atmosphere_ctx);
        // Reset mouse deltas after the scene has consumed them
        app.atmosphere_ctx.mouse_delta_col = 0;
        app.atmosphere_ctx.mouse_delta_row = 0;
        let buf = frame.buffer_mut();
        super::atmosphere::render_atmosphere(&app.atmosphere, buf, area);
    }

    let _ = sync_area;
}

// ── Shared helpers ──────────────────────────────────────────────

/// CT1 face picker: pin any panel face, or back to automatic. Rendered as a
/// small centered overlay like the split picker — keyboard only (↑↓/jk,
/// Enter, Esc), so it never steals or needs the mouse.
fn draw_panel_picker(frame: &mut Frame, app: &App, theme: &Theme) {
    use crate::tui::app::panel_face::PANEL_PICKER_OPTIONS;
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Clear, Paragraph};

    let rows: Vec<(String, bool)> = PANEL_PICKER_OPTIONS
        .iter()
        .map(|option| {
            let name = option.map_or("automatic", |face| face.label()).to_string();
            let pinned = *option == app.panel_pinned;
            let label = if pinned {
                format!("{name}  · pinned")
            } else {
                name
            };
            (label, pinned)
        })
        .collect();
    let width = rows
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(9)
        .max(22) as u16
        + 6;
    let height = rows.len() as u16 + 4;
    let area = centered_rect(40, height, frame.area());
    let area = Rect::new(
        area.x,
        area.y,
        width.min(frame.area().width.saturating_sub(2)),
        height.min(frame.area().height.saturating_sub(2)),
    );

    frame.render_widget(Clear, area);
    crate::tui::ui::dialogs::draw_dialog_left_wave(frame, area, app.animation_tick.into());
    let block = Block::default()
        .title(" panel face · F6 ")
        .borders(dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.header_color))
        .style(Style::default().bg(theme.dialog_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height < 3 {
        return;
    }

    let mut lines = vec![Line::from(Span::styled(
        "Enter pins · Esc closes",
        Style::default().fg(theme.dim_text),
    ))];
    for (idx, (label, _)) in rows.iter().enumerate() {
        let selected = idx == app.panel_picker_idx;
        let (style, marker) = if selected {
            (
                Style::default()
                    .bg(theme.selected_bg)
                    .add_modifier(Modifier::BOLD),
                "›",
            )
        } else {
            (Style::default(), " ")
        };
        lines.push(Line::from(vec![
            Span::styled(marker, style.fg(theme.header_color)),
            Span::raw(" "),
            Span::styled(label.clone(), style.fg(Color::White)),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Create a centered rect of given percentage width and fixed height.
pub(crate) fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let [_, center, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);

    let [_, center, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .areas(center);

    center
}

pub(crate) fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max > 1 {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    } else {
        String::new()
    }
}

pub(crate) fn truncate_str_keep_tail(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max <= 1 {
        return String::new();
    }

    let tail_len = max.saturating_sub(1);
    let chars: Vec<char> = s.chars().collect();
    let tail_start = chars.len().saturating_sub(tail_len);
    let tail: String = chars[tail_start..].iter().collect();
    format!("…{tail}")
}

/// Extract the last two path segments, e.g. `/a/b/c/d` → `c/d`.
pub(crate) fn last_two_segments(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    let parts: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return "/".to_string();
    }
    if parts.len() <= 2 {
        return trimmed.to_string();
    }
    format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sidebar_layout_uses_sidebar_width_constant() {
        assert_eq!(SIDEBAR_WIDTH, 33);

        let body = Rect::new(0, 1, 120, 40);
        let [sidebar, content] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(body);

        // The sidebar consumes exactly SIDEBAR_WIDTH columns and the content
        // takes the remaining width, starting immediately after it.
        assert_eq!(sidebar.width, SIDEBAR_WIDTH);
        assert_eq!(sidebar.x, 0);
        assert_eq!(content.x, SIDEBAR_WIDTH);
        assert_eq!(content.width, body.width - SIDEBAR_WIDTH);
    }

    #[test]
    fn modern_sidebar_and_work_area_have_distinct_rendered_boundary() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let (mut app, _data_dir) = picker_test_app();
        app.theme = Theme::modern();
        app.sidebar_visible = true;
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let boundary_x = SIDEBAR_WIDTH - 1;
        assert!((1..39).any(|y| {
            buffer[(boundary_x, y)].bg == app.theme.sidebar_bg
                && buffer[(SIDEBAR_WIDTH, y)].bg == app.theme.panel_bg
        }));
    }

    #[test]
    fn every_dialog_source_uses_dialog_border_helper() {
        for source in [
            include_str!("dialogs/at_picker.rs"),
            include_str!("dialogs/context_transfer.rs"),
            include_str!("dialogs/graph_control.rs"),
            include_str!("dialogs/graph_editor.rs"),
            include_str!("dialogs/graph_form.rs"),
            include_str!("dialogs/knowledge_dialog.rs"),
            include_str!("dialogs/launchpad.rs"),
            include_str!("dialogs/new_agent_dialog.rs"),
            include_str!("dialogs/node_tail.rs"),
            include_str!("dialogs/pickers.rs"),
            include_str!("dialogs/rag_transfer.rs"),
            include_str!("dialogs/section_picker.rs"),
            include_str!("dialogs/simple_modals.rs"),
            include_str!("dialogs/simple_prompt.rs"),
        ] {
            assert!(source.contains("dialog_borders_for"));
            assert!(!source.contains("::borders_for(theme)"));
            assert!(!source.contains(".borders(borders_for(theme))"));
        }
    }

    #[test]
    fn truncate_str_short_enough() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_exact_length() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_too_long() {
        assert_eq!(truncate_str("hello world", 5), "hell…");
    }

    #[test]
    fn truncate_str_max_one() {
        assert_eq!(truncate_str("hello", 1), "");
    }

    #[test]
    fn truncate_str_empty() {
        assert_eq!(truncate_str("", 5), "");
    }

    #[test]
    fn truncate_str_unicode() {
        // "café" = 4 chars; truncate to 3 → take(2) → "ca…"
        assert_eq!(truncate_str("café", 3), "ca…");
    }

    #[test]
    fn truncate_str_keep_tail_short_enough() {
        assert_eq!(truncate_str_keep_tail("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_keep_tail_exact() {
        assert_eq!(truncate_str_keep_tail("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_keep_tail_too_long() {
        assert_eq!(truncate_str_keep_tail("hello world", 5), "…orld");
    }

    #[test]
    fn truncate_str_keep_tail_max_one() {
        assert_eq!(truncate_str_keep_tail("hello", 1), "");
    }

    #[test]
    fn truncate_str_keep_tail_empty() {
        assert_eq!(truncate_str_keep_tail("", 5), "");
    }

    #[test]
    fn truncate_str_keep_tail_unicode() {
        assert_eq!(truncate_str_keep_tail("café", 3), "…fé");
    }

    #[test]
    fn last_two_segments_deep_path() {
        assert_eq!(last_two_segments("/a/b/c/d"), "c/d");
    }

    #[test]
    fn last_two_segments_two_levels() {
        assert_eq!(last_two_segments("/a/b"), "/a/b");
    }

    #[test]
    fn last_two_segments_single_level() {
        assert_eq!(last_two_segments("/a"), "/a");
    }

    #[test]
    fn last_two_segments_root() {
        assert_eq!(last_two_segments("/"), "/");
    }

    #[test]
    fn last_two_segments_trailing_slash() {
        // After trimming trailing slash: "/a/b/" → "/a/b" → parts ["a","b"] → "a/b"
        // Wait: parts are split by '/', filtered empty, so "/a/b" → ["a","b"]
        // With parts.len()=2, returns trimmed="/a/b". Hmm, actually the function
        // returns trimmed.to_string() when parts.len() <= 2. trimmed = "/a/b".
        assert_eq!(last_two_segments("/a/b/"), "/a/b");
    }

    #[test]
    fn last_two_segments_empty() {
        assert_eq!(last_two_segments(""), "/");
    }

    #[test]
    fn last_two_segments_three_levels() {
        assert_eq!(last_two_segments("/a/b/c"), "b/c");
    }

    #[test]
    fn centered_rect_basic() {
        let area = Rect::new(0, 0, 100, 40);
        let result = centered_rect(50, 10, area);
        assert!(result.width > 0);
        assert_eq!(result.height, 10);
    }

    #[test]
    fn centered_rect_full_width() {
        let area = Rect::new(0, 0, 100, 40);
        let result = centered_rect(100, 5, area);
        assert_eq!(result.height, 5);
    }

    // CT12 — panel picker shares the dialog chrome (waves rail + opaque body).
    fn picker_test_app() -> (App, tempfile::TempDir) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = std::sync::Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        (app, data_dir)
    }

    fn render_picker_buffer(
        app: &App,
        theme: &Theme,
        width: u16,
        height: u16,
        behind: Color,
    ) -> ratatui::buffer::Buffer {
        use ratatui::backend::TestBackend;
        use ratatui::widgets::{Paragraph, Wrap};
        use ratatui::Terminal;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let full = frame.area();
                // Distinctive backdrop standing in for the graph /
                // interactive output / knowledge view the picker floats over.
                let filler = vec!["#".repeat(width as usize); height as usize].join("\n");
                frame.render_widget(
                    Paragraph::new(filler)
                        .style(Style::default().bg(behind).fg(Color::White))
                        .wrap(Wrap { trim: false }),
                    full,
                );
                draw_panel_picker(frame, app, theme);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// Mirror of `draw_panel_picker`'s size/position math (labels + pinned
    /// state only — no chrome), so tests can scope assertions to the picker.
    fn expected_picker_area(app: &App, full: Rect) -> Rect {
        use crate::tui::app::panel_face::PANEL_PICKER_OPTIONS;
        let rows: Vec<String> = PANEL_PICKER_OPTIONS
            .iter()
            .map(|option| {
                let name = option.map_or("automatic", |face| face.label()).to_string();
                if *option == app.panel_pinned {
                    format!("{name}  · pinned")
                } else {
                    name
                }
            })
            .collect();
        let width = rows
            .iter()
            .map(|label| label.chars().count())
            .max()
            .unwrap_or(9)
            .max(22) as u16
            + 6;
        let height = rows.len() as u16 + 4;
        let center = centered_rect(40, height, full);
        Rect::new(
            center.x,
            center.y,
            width.min(full.width.saturating_sub(2)),
            height.min(full.height.saturating_sub(2)),
        )
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
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
    fn panel_picker_has_waves_rail() {
        for theme in [Theme::classic(), Theme::modern()] {
            let (app, _data_dir) = picker_test_app();
            let (width, height) = (100u16, 40u16);
            let buffer = render_picker_buffer(&app, &theme, width, height, Color::Blue);
            let area = expected_picker_area(&app, Rect::new(0, 0, width, height));
            assert!(area.x > 0, "picker must leave a gutter column for the rail");
            let rail_x = area.x.saturating_sub(1);
            let rail_y = area.y + area.height.saturating_sub(3) / 2;
            let glyphs: Vec<&str> = (0..3)
                .map(|i| buffer[(rail_x, rail_y + i as u16)].symbol())
                .collect();
            // Same `░▒░` rail the shared `draw_dialog_left_wave` paints.
            assert_eq!(
                glyphs,
                vec!["░", "▒", "░"],
                "panel picker must draw the shared waves rail"
            );
        }
    }

    #[test]
    fn panel_picker_body_is_opaque_over_every_background() {
        for theme in [Theme::classic(), Theme::modern()] {
            let (app, _data_dir) = picker_test_app();
            let behind = Color::Blue;
            assert_ne!(theme.dialog_bg, behind);
            assert_ne!(theme.selected_bg, behind);
            let (width, height) = (100u16, 40u16);
            let buffer = render_picker_buffer(&app, &theme, width, height, behind);
            let area = expected_picker_area(&app, Rect::new(0, 0, width, height));
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    let cell = &buffer[(x, y)];
                    assert_ne!(
                        cell.symbol(),
                        "#",
                        "picker cell ({x},{y}) leaks the backdrop symbol"
                    );
                    assert!(
                        cell.bg == theme.dialog_bg || cell.bg == theme.selected_bg,
                        "picker cell ({x},{y}) has backdrop bg {:?}, want {:?} or {:?}",
                        cell.bg,
                        theme.dialog_bg,
                        theme.selected_bg
                    );
                }
            }
        }
    }

    #[test]
    fn panel_picker_border_and_title_match_shared_chrome() {
        // Classic: box border in the header colour with the F6 title on top.
        let (app, _data_dir) = picker_test_app();
        let theme = Theme::classic();
        let (width, height) = (100u16, 40u16);
        let buffer = render_picker_buffer(&app, &theme, width, height, Color::Blue);
        let area = expected_picker_area(&app, Rect::new(0, 0, width, height));
        let text = buffer_text(&buffer);
        assert!(
            text.contains("panel face"),
            "picker must carry its title\n{text}"
        );
        assert_eq!(buffer[(area.x, area.y)].symbol(), "┌");
        assert_eq!(
            buffer[(area.x, area.y)].fg,
            theme.header_color,
            "picker border must use the shared header colour"
        );
        assert_eq!(buffer[(area.x + area.width - 1, area.y)].symbol(), "┐");
        assert_eq!(
            buffer[(area.x + 1, area.y)].fg,
            theme.header_color,
            "picker top border must use the shared header colour"
        );

        // Modern keeps the dialog exception: the picker must still have a
        // visible edge even though panels use `Borders::NONE`.
        let modern = Theme::modern();
        let modern_buffer = render_picker_buffer(&app, &modern, width, height, Color::Blue);
        let modern_area = expected_picker_area(&app, Rect::new(0, 0, width, height));
        assert_eq!(modern_buffer[(modern_area.x, modern_area.y)].symbol(), "┌");
        assert_eq!(
            modern_buffer[(modern_area.x, modern_area.y)].fg,
            modern.header_color
        );
        // The modern picker is still opaque: every cell carries the dialog
        // background (which equals the panel background on this theme) or
        // the selected-row background.
        for y in modern_area.y..modern_area.y + modern_area.height {
            for x in modern_area.x..modern_area.x + modern_area.width {
                let cell = &modern_buffer[(x, y)];
                assert!(
                    cell.bg == modern.dialog_bg || cell.bg == modern.selected_bg,
                    "modern picker cell ({x},{y}) has bg {:?}",
                    cell.bg
                );
            }
        }
    }

    #[test]
    fn dialog_borders_for_always_all() {
        use ratatui::widgets::Borders;

        assert_eq!(dialog_borders_for(&Theme::classic()), Borders::ALL);
        assert_eq!(dialog_borders_for(&Theme::modern()), Borders::ALL);
    }

    #[test]
    fn panel_picker_and_quit_confirm_share_dialog_bg() {
        use ratatui::backend::TestBackend;
        use ratatui::widgets::{Paragraph, Wrap};
        use ratatui::Terminal;
        for theme in [Theme::classic(), Theme::modern()] {
            let (app, _data_dir) = picker_test_app();
            let (width, height) = (100u16, 40u16);
            let full = Rect::new(0, 0, width, height);
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    let filler = vec!["#".repeat(width as usize); height as usize].join("\n");
                    frame.render_widget(
                        Paragraph::new(filler)
                            .style(Style::default().bg(Color::Blue).fg(Color::White))
                            .wrap(Wrap { trim: false }),
                        full,
                    );
                    crate::tui::ui::dialogs::draw_quit_confirm(frame, &theme);
                })
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            // `draw_modal_confirm` centers a 40%-wide rect; its height is the
            // wrapped message lines plus borders.
            let message = "Press y/Enter to quit, any key to cancel";
            let dialog_width = width * 40 / 100;
            let inner_width = dialog_width.saturating_sub(2).max(1) as usize;
            let needed = message.len().div_ceil(inner_width).max(1) as u16;
            let quit_area = centered_rect(40, needed + 2, full);
            let mut saw_dialog_bg = false;
            for y in quit_area.y..quit_area.y + quit_area.height {
                for x in quit_area.x..quit_area.x + quit_area.width {
                    let cell = &buffer[(x, y)];
                    assert_eq!(
                        cell.bg, theme.dialog_bg,
                        "quit confirm cell ({x},{y}) has bg {:?}, want {:?}",
                        cell.bg, theme.dialog_bg
                    );
                    saw_dialog_bg = true;
                }
            }
            assert!(saw_dialog_bg);

            // The picker paints that same background over its own rect.
            let picker_buffer = render_picker_buffer(&app, &theme, width, height, Color::Blue);
            let picker_area = expected_picker_area(&app, full);
            let picker_uses_dialog_bg = (picker_area.y..picker_area.y + picker_area.height)
                .flat_map(|y| {
                    (picker_area.x..picker_area.x + picker_area.width).map(move |x| (x, y))
                })
                .any(|(x, y)| picker_buffer[(x, y)].bg == theme.dialog_bg);
            assert!(
                picker_uses_dialog_bg,
                "picker must paint the shared dialog background {theme:?}"
            );
        }
    }
}
