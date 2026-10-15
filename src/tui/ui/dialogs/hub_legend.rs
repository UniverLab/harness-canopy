//! CT31: the hub's `Missions & stats` tab — the renderers moved verbatim out
//! of `simple_modals.rs::draw_legend`, now drawing into the hub's body area
//! instead of a self-owned centered modal.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::domain::gamification::{MissionCategory, MISSIONS};
use crate::tui::app::types::App;
use crate::tui::ui::theme::Theme;

/// The Missions & stats tab: the same sections `draw_legend` drew, into the
/// hub's body area instead of a self-owned centered modal.
pub(super) fn draw_missions_tab(frame: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    let label_style = Style::default().fg(theme.dim_text);
    let value_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let accent_style = Style::default().fg(theme.header_color);

    let unlocked_indices: Vec<usize> = MISSIONS
        .iter()
        .enumerate()
        .filter(|(_, def)| app.mission_manager.is_unlocked(def.id))
        .map(|(i, _)| i)
        .collect();

    let max_selected = unlocked_indices.len().saturating_sub(1);
    if app.legend_selected > max_selected {
        app.legend_selected = max_selected;
    }

    let selected = app.legend_selected;

    let has_missions = !unlocked_indices.is_empty();
    let sections = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(if has_missions { 1 } else { 0 }),
        Constraint::Length(if has_missions {
            unlocked_indices.len().min(15) as u16
        } else {
            0
        }),
        Constraint::Length(if has_missions { 1 } else { 0 }),
        Constraint::Length(if has_missions { 3 } else { 0 }),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .split(area);

    draw_legend_summary(
        frame,
        app,
        sections[0],
        label_style,
        value_style,
        accent_style,
    );
    draw_legend_missions(frame, &unlocked_indices, selected, theme, sections[2]);
    draw_legend_detail(frame, app, &unlocked_indices, selected, sections[4]);
    draw_legend_usage(frame, app, sections[5]);
    draw_legend_footer(frame, sections[6], label_style);
}

fn format_uptime_precise(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let mins = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    match (days, hours, mins) {
        (0, 0, 0) => format!("{secs}s"),
        (0, 0, m) => format!("{m}m {secs}s"),
        (0, h, m) => format!("{h}h {m}m {secs}s"),
        (d, h, m) => format!("{d}d {h}h {m}m {secs}s"),
    }
}

fn category_label(category: &MissionCategory) -> &'static str {
    match category {
        MissionCategory::Environment => "Environment",
        MissionCategory::Intelligence => "Intelligence",
        MissionCategory::Projects => "Projects",
        MissionCategory::Graph => "Graph",
        MissionCategory::Seeds => "Seeds",
        MissionCategory::SysInfo => "System",
    }
}

fn mission_unlock_text(icon: &str, title: &str, category: &MissionCategory) -> String {
    format!(
        "Unlocked medal {icon} {title} in {}.",
        category_label(category)
    )
}

/// Distinct accent color per mission category.
fn category_color(cat: &MissionCategory) -> Color {
    match cat {
        MissionCategory::Environment => Color::Rgb(100, 220, 100),
        MissionCategory::Intelligence => Color::Rgb(100, 180, 255),
        MissionCategory::Projects => Color::Rgb(255, 200, 80),
        MissionCategory::Graph => Color::Rgb(220, 120, 255),
        MissionCategory::Seeds => Color::Rgb(80, 220, 180),
        MissionCategory::SysInfo => Color::Rgb(255, 130, 80),
    }
}

/// Sine-based pulse in `0..=1` used to twinkle the mission icons.
fn twinkle(now_ms: f64, offset: usize) -> f64 {
    let phase = (now_ms / 1500.0) + (offset as f64 * 0.7);
    (phase.sin() + 1.0) / 2.0
}

/// Scale an RGB color toward black, keeping a minimum brightness floor.
fn dim_color(base: Color, factor: f64) -> Color {
    match base {
        Color::Rgb(r, g, b) => {
            let min_brightness = 0.35;
            let f = min_brightness + (1.0 - min_brightness) * factor;
            Color::Rgb(
                (r as f64 * f) as u8,
                (g as f64 * f) as u8,
                (b as f64 * f) as u8,
            )
        }
        _ => base,
    }
}

/// Render the uptimes and session counters summary block.
fn draw_legend_summary(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    label_style: Style,
    value_style: Style,
    accent_style: Style,
) {
    let session_uptime = format_uptime_precise(app.process_start_time.elapsed().as_secs());
    let canopy_uptime = format_uptime_precise(app.accumulated_uptime_secs());
    let interactive_count = app.db.count_interactive_sessions().unwrap_or(0);
    let terminal_count = app.db.count_terminal_sessions().unwrap_or(0);
    let bg_count = app.db.count_background_agents().unwrap_or(0);
    let runs_count = app.db.count_runs().unwrap_or(0);
    let total = MISSIONS.len();
    let unlocked_n = app.mission_manager.unlocked_count();

    let summary_lines = vec![
        Line::from(vec![
            Span::styled("Missions ", value_style),
            Span::styled(format!("{unlocked_n}/{total}"), accent_style),
            Span::raw("    "),
            Span::styled("Session ", label_style),
            Span::styled(&session_uptime, accent_style),
            Span::raw("    "),
            Span::styled("Canopy ", label_style),
            Span::styled(&canopy_uptime, accent_style),
        ]),
        Line::from(vec![
            Span::styled("Interactive ", label_style),
            Span::styled(format!("{interactive_count}"), value_style),
            Span::raw("  "),
            Span::styled("Terminal ", label_style),
            Span::styled(format!("{terminal_count}"), value_style),
            Span::raw("  "),
            Span::styled("BG ", label_style),
            Span::styled(format!("{bg_count}"), value_style),
            Span::raw("  "),
            Span::styled("Runs ", label_style),
            Span::styled(format!("{runs_count}"), value_style),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(summary_lines).alignment(Alignment::Center),
        area,
    );
}

/// Render the scrollable list of unlocked missions.
fn draw_legend_missions(
    frame: &mut Frame,
    unlocked_indices: &[usize],
    selected: usize,
    theme: &Theme,
    area: Rect,
) {
    let mut mission_lines: Vec<Line> = Vec::new();
    if !unlocked_indices.is_empty() {
        let visible_rows = area.height as usize;
        let start = selected.saturating_sub(visible_rows.saturating_sub(1) / 2);
        let start = crate::tui::selection::clamp_scroll(
            selected,
            start,
            unlocked_indices.len(),
            visible_rows,
        );
        let end = (start + visible_rows).min(unlocked_indices.len());
        let now_ms = chrono::Utc::now().timestamp_millis() as f64;
        for (i, idx) in unlocked_indices[start..end].iter().enumerate() {
            let def = &MISSIONS[*idx];
            let mission_index = start + i;
            let is_selected = mission_index == selected;
            let base_color = category_color(&def.category);
            let icon_color = dim_color(base_color, twinkle(now_ms, i));
            let marker = if is_selected { "▶" } else { "·" };
            let title_style = if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(200, 200, 200))
            };
            mission_lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(theme.header_color)),
                Span::raw(" "),
                Span::styled(
                    def.icon,
                    Style::default().fg(icon_color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(def.title, title_style),
            ]));
        }
    }
    frame.render_widget(
        Paragraph::new(mission_lines).alignment(Alignment::Center),
        area,
    );
}

/// Render the challenge, unlock text and completion date of the selected mission.
fn draw_legend_detail(
    frame: &mut Frame,
    app: &App,
    unlocked_indices: &[usize],
    selected: usize,
    area: Rect,
) {
    let selected_detail_lines = if let Some(idx) = unlocked_indices.get(selected) {
        let selected_def = &MISSIONS[*idx];
        let date_str = app
            .mission_manager
            .unlock_timestamp(selected_def.id)
            .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "Unknown".to_string());

        vec![
            Line::from(Span::styled(
                selected_def.challenge,
                Style::default().fg(Color::Rgb(190, 190, 190)),
            )),
            Line::from(Span::styled(
                mission_unlock_text(
                    selected_def.icon,
                    selected_def.title,
                    &selected_def.category,
                ),
                Style::default().fg(Color::Rgb(140, 205, 140)),
            )),
            Line::from(Span::styled(
                format!("Completed: {}", date_str),
                Style::default().fg(Color::Rgb(100, 100, 100)),
            )),
        ]
    } else {
        vec![]
    };
    frame.render_widget(
        Paragraph::new(selected_detail_lines).alignment(Alignment::Center),
        area,
    );
}

/// Render the per-harness invocation counts panel.
fn draw_legend_usage(frame: &mut Frame, app: &App, area: Rect) {
    let mut usage_pairs: Vec<_> = app.cli_usage.counts.iter().collect();
    usage_pairs.sort_by(|(name_a, count_a), (name_b, count_b)| {
        count_b.cmp(count_a).then_with(|| name_a.cmp(name_b))
    });
    let used_harnesses = usage_pairs.iter().filter(|(_, count)| **count > 0).count();
    let usage_lines = if usage_pairs.is_empty() {
        vec![Line::from(Span::styled(
            "No harness usage yet.",
            Style::default().fg(Color::White),
        ))]
    } else {
        let visible_rows = area.height.saturating_sub(1) as usize;
        usage_pairs
            .iter()
            .take(visible_rows.max(1))
            .map(|(name, count)| {
                Line::from(Span::styled(
                    format!("{name}: {count}"),
                    Style::default().fg(Color::White),
                ))
            })
            .collect::<Vec<_>>()
    };
    let mut usage_panel_lines = vec![Line::from(Span::styled(
        format!("Harnesses used: {used_harnesses}"),
        Style::default().fg(Color::Rgb(210, 210, 210)),
    ))];
    usage_panel_lines.extend(usage_lines);
    frame.render_widget(
        Paragraph::new(usage_panel_lines).alignment(Alignment::Center),
        area,
    );
}

/// Render the F1/Esc and ↑↓/jk hint line.
fn draw_legend_footer(frame: &mut Frame, area: Rect, label_style: Style) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("F1/Esc ", label_style),
            Span::styled("close   ", Style::default().fg(Color::White)),
            Span::styled("↑↓/jk ", label_style),
            Span::styled("select", Style::default().fg(Color::White)),
        ]))
        .alignment(Alignment::Center),
        area,
    );
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

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
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
    fn format_uptime_precise_seconds_only() {
        assert_eq!(format_uptime_precise(0), "0s");
        assert_eq!(format_uptime_precise(59), "59s");
        assert_eq!(format_uptime_precise(1), "1s");
    }

    #[test]
    fn format_uptime_precise_minutes_and_seconds() {
        assert_eq!(format_uptime_precise(60), "1m 0s");
        assert_eq!(format_uptime_precise(125), "2m 5s");
        assert_eq!(format_uptime_precise(3599), "59m 59s");
    }

    #[test]
    fn format_uptime_precise_hours_minutes_seconds() {
        assert_eq!(format_uptime_precise(3600), "1h 0m 0s");
        assert_eq!(format_uptime_precise(3661), "1h 1m 1s");
        assert_eq!(format_uptime_precise(86399), "23h 59m 59s");
    }

    #[test]
    fn format_uptime_precise_days_hours_minutes() {
        assert_eq!(format_uptime_precise(86400), "1d 0h 0m 0s");
        assert_eq!(format_uptime_precise(90061), "1d 1h 1m 1s");
        assert_eq!(format_uptime_precise(172800), "2d 0h 0m 0s");
    }

    #[test]
    fn format_uptime_precise_large_values() {
        assert_eq!(format_uptime_precise(86400 * 365), "365d 0h 0m 0s");
        assert_eq!(
            format_uptime_precise(86400 * 365 + 3600 + 61),
            "365d 1h 1m 1s"
        );
    }

    #[test]
    fn format_uptime_precise_boundary_59_minutes() {
        assert_eq!(format_uptime_precise(3540), "59m 0s");
    }

    #[test]
    fn category_label_all_variants() {
        assert_eq!(category_label(&MissionCategory::Environment), "Environment");
        assert_eq!(
            category_label(&MissionCategory::Intelligence),
            "Intelligence"
        );
        assert_eq!(category_label(&MissionCategory::Projects), "Projects");
        assert_eq!(category_label(&MissionCategory::Graph), "Graph");
        assert_eq!(category_label(&MissionCategory::Seeds), "Seeds");
        assert_eq!(category_label(&MissionCategory::SysInfo), "System");
    }

    #[test]
    fn mission_unlock_text_formats_correctly() {
        let result = mission_unlock_text("🏆", "First Login", &MissionCategory::Seeds);
        assert_eq!(result, "Unlocked medal 🏆 First Login in Seeds.");
    }

    #[test]
    fn mission_unlock_text_different_category() {
        let result = mission_unlock_text("⭐", "Code Master", &MissionCategory::Intelligence);
        assert_eq!(result, "Unlocked medal ⭐ Code Master in Intelligence.");
    }

    #[test]
    fn mission_unlock_text_empty_strings() {
        let result = mission_unlock_text("", "", &MissionCategory::Environment);
        assert_eq!(result, "Unlocked medal   in Environment.");
    }

    #[test]
    fn mission_unlock_text_all_categories() {
        for cat in &[
            MissionCategory::Environment,
            MissionCategory::Intelligence,
            MissionCategory::Projects,
            MissionCategory::Graph,
            MissionCategory::Seeds,
            MissionCategory::SysInfo,
        ] {
            let result = mission_unlock_text("i", "t", cat);
            assert!(result.starts_with("Unlocked medal i t in "));
            assert!(result.ends_with("."));
        }
    }

    #[test]
    fn mission_unlock_text_with_empty_icon() {
        let result = mission_unlock_text("", "Title", &MissionCategory::Seeds);
        assert!(result.contains("Title"));
        assert!(result.contains("Seeds"));
    }

    #[test]
    fn draw_missions_tab_renders_without_panic() {
        let mut app = test_app();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_missions_tab(frame, &mut app, &theme, frame.area());
            })
            .unwrap();
    }

    #[test]
    fn draw_missions_tab_with_zero_missions() {
        let mut app = test_app();
        app.legend_selected = 0;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                draw_missions_tab(frame, &mut app, &theme, frame.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("Missions"),
            "Should show Missions title: {text}"
        );
    }
}
