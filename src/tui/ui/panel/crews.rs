//! CT30: rendering for the Crews panel face — Pairs and Crews sections,
//! the status line and the three dialogs. Read-only: the draw takes `&App`
//! and writes only the `Cell` list height the key handler reads back.

use chrono::{DateTime, Local};
use ratatui::layout::Alignment;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::app::crews_dialog::CrewsDialog;
use crate::tui::app::crews_face::CrewsSection;
use crate::tui::app::crews_logic::{keep_visible, CrewBindingView, CrewView, PairRow};
use crate::tui::app::dialog::GRAPH_ACTION_MESSAGE_TTL;
use crate::tui::app::types::App;
use crate::tui::ui::theme::Theme;

/// Draw the Crews face inside the panel's inner rect: header row (section
/// tabs + `refreshing…` hint), the active section's list, and the status
/// line while its message TTL holds.
pub(super) fn draw_crews_face(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let has_message = app.crews.message.is_some()
        && app
            .crews
            .message_at
            .is_some_and(|at| at.elapsed() < GRAPH_ACTION_MESSAGE_TTL);
    let [header_area, list_area, message_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(u16::from(has_message)),
    ])
    .areas(area);

    frame.render_widget(Paragraph::new(header_line(app, theme)), header_area);
    if app.crews.snapshot_rx.is_some() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "refreshing…",
                Style::default().fg(theme.dim_text),
            )))
            .alignment(Alignment::Right),
            header_area,
        );
    }

    match app.crews.section {
        CrewsSection::Pairs => draw_pairs_list(frame, list_area, app, theme),
        CrewsSection::Crews => draw_crews_list(frame, list_area, app, theme),
    }

    if has_message {
        if let Some(message) = &app.crews.message {
            let color = if message.is_error {
                theme.error
            } else {
                theme.success
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    message.text.clone(),
                    Style::default().fg(color),
                ))),
                message_area,
            );
        }
    }

    draw_dialog(frame, area, app, theme);
}

/// The tab strip: the active section bold in `text_primary`, the other dim.
fn header_line(app: &App, theme: &Theme) -> Line<'static> {
    let active = Style::default()
        .fg(theme.text_primary)
        .add_modifier(Modifier::BOLD);
    let inactive = Style::default().fg(theme.dim_text);
    let pairs = if matches!(app.crews.section, CrewsSection::Pairs) {
        active
    } else {
        inactive
    };
    let crews = if matches!(app.crews.section, CrewsSection::Crews) {
        active
    } else {
        inactive
    };
    Line::from(vec![
        Span::styled("Pairs", pairs),
        Span::raw("   "),
        Span::styled("Crews", crews),
    ])
}

/// The availability glyph and color for a pair (or a member's pair): `●`
/// available, `○` unavailable, `◐` available but its entry has expired.
fn glyph_style(available: bool, expired: bool, theme: &Theme) -> (&'static str, Style) {
    if !available {
        ("○", Style::default().fg(theme.error))
    } else if expired {
        ("◐", Style::default().fg(theme.warning))
    } else {
        ("●", Style::default().fg(theme.success))
    }
}

/// `until` as a local `YYYY-MM-DD HH:MM`, or `—` when absent/unparseable.
fn until_label(until: Option<&str>) -> String {
    let Some(until) = until else {
        return "—".to_string();
    };
    match DateTime::parse_from_rfc3339(until) {
        Ok(parsed) => parsed
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        Err(_) => until.to_string(),
    }
}

fn placeholder(frame: &mut Frame, area: Rect, text: &str, theme: &Theme) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(theme.dim_text),
        ))),
        area,
    );
}

/// The Pairs section: one row per distinct pair, unavailable first (the
/// order `build_pair_rows` produced), selection on `pair_idx`.
fn draw_pairs_list(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    app.crews.last_list_height.set(area.height as usize);
    let rows = &app.crews.pair_rows;
    if rows.is_empty() {
        placeholder(frame, area, "no pairs", theme);
        return;
    }
    let height = area.height as usize;
    let scroll = keep_visible(app.crews.pair_idx, app.crews.pair_scroll, height);
    let lines: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(index, row)| pair_line(row, index == app.crews.pair_idx, theme))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn pair_line(row: &PairRow, selected: bool, theme: &Theme) -> Line<'static> {
    let (glyph, style) = glyph_style(row.available, row.expired, theme);
    let detail = format!(
        "{} · {}",
        row.reason.as_deref().unwrap_or("—"),
        until_label(row.until.as_deref())
    );
    let counts = format!("graphs: {} · crews: {}", row.graph_count, row.crew_count);
    let mut spans = vec![
        Span::styled(format!("{glyph} "), style),
        Span::styled(row.label(), Style::default().fg(theme.text_primary)),
        Span::styled(format!("  {detail}"), Style::default().fg(theme.dim_text)),
        Span::styled(format!("  {counts}"), Style::default().fg(theme.dim_text)),
    ];
    if !row.graph_names.is_empty() {
        spans.push(Span::styled(
            format!("  {}", row.graph_names.join(", ")),
            Style::default().fg(theme.dim_text),
        ));
    }
    if selected {
        for span in &mut spans {
            span.style = span.style.bg(theme.selected_bg);
        }
    }
    Line::from(spans)
}

/// The Crews section: crew rows with the expanded crew's members and
/// bindings inserted underneath (D16). Scroll keeps the selected line (the
/// member when expanded, else the crew) visible.
fn draw_crews_list(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    app.crews.last_list_height.set(area.height as usize);
    let Some(snapshot) = app.crews.snapshot.as_ref() else {
        placeholder(frame, area, "no crews", theme);
        return;
    };
    if snapshot.crews.is_empty() {
        placeholder(frame, area, "no crews", theme);
        return;
    }
    let height = area.height as usize;
    let expanded = app
        .crews
        .expanded
        .as_ref()
        .and_then(|name| snapshot.crews.iter().position(|crew| &crew.name == name));

    let mut flat: Vec<Line<'static>> = Vec::new();
    let mut selected_line = 0usize;
    let mut cursor = 0usize;
    for (index, crew) in snapshot.crews.iter().enumerate() {
        let crew_selected = index == app.crews.crew_idx;
        flat.push(crew_line(crew, crew_selected, theme));
        if crew_selected {
            selected_line = cursor;
        }
        cursor += 1;
        if expanded != Some(index) {
            continue;
        }
        for (position, pair) in crew.member_pairs.iter().enumerate() {
            let member_selected = crew_selected && position == app.crews.member_idx;
            flat.push(member_line(
                position,
                pair,
                member_selected,
                theme,
                &app.crews.pair_rows,
            ));
            if member_selected {
                selected_line = cursor;
            }
            cursor += 1;
        }
        for binding in &crew.bindings {
            flat.push(binding_line(binding, theme));
            cursor += 1;
        }
    }

    let scroll = keep_visible(selected_line, app.crews.crew_scroll, height);
    let visible: Vec<Line<'static>> = flat.into_iter().skip(scroll).take(height).collect();
    frame.render_widget(Paragraph::new(visible), area);
}

fn crew_line(crew: &CrewView, selected: bool, theme: &Theme) -> Line<'static> {
    let bindings = if crew.bindings.is_empty() {
        crew.binding_total
    } else {
        crew.bindings.len()
    };
    let text = format!(
        "{}  {} members  {} bindings  v{}",
        crew.name,
        crew.members.len(),
        bindings,
        crew.version
    );
    let mut spans = vec![Span::styled(text, Style::default().fg(theme.text_primary))];
    if let Some(description) = &crew.description {
        spans.push(Span::styled(
            format!("  {description}"),
            Style::default().fg(theme.dim_text),
        ));
    }
    if selected {
        for span in &mut spans {
            span.style = span.style.bg(theme.selected_bg);
        }
    }
    Line::from(spans)
}

fn member_line(
    position: usize,
    pair: &(String, String),
    selected: bool,
    theme: &Theme,
    rows: &[PairRow],
) -> Line<'static> {
    let (platform, model) = pair;
    let label = if model.is_empty() {
        format!("{platform}/(default)")
    } else {
        format!("{platform}/{model}")
    };
    let row = rows
        .iter()
        .find(|row| row.platform == *platform && row.model == *model);
    let (glyph, style) = match row {
        Some(row) => glyph_style(row.available, row.expired, theme),
        // No pair row for this member: a neutral dot in dim text.
        None => ("·", Style::default().fg(theme.dim_text)),
    };
    let mut spans = vec![
        Span::raw(format!("  {}. ", position + 1)),
        Span::styled(format!("{glyph} "), style),
        Span::styled(label, Style::default().fg(theme.text_primary)),
    ];
    if selected {
        for span in &mut spans {
            span.style = span.style.bg(theme.selected_bg);
        }
    }
    Line::from(spans)
}

fn binding_line(binding: &CrewBindingView, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{} › {}|{}", binding.graph, binding.kind, binding.name),
            Style::default().fg(theme.dim_text),
        ),
    ])
}

/// Overlay the open dialog at the top of the face (`dialog_bg` block).
fn draw_dialog(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some(dialog) = app.crews.dialog.as_ref() else {
        return;
    };
    let (lines, height) = dialog_lines(dialog, theme);
    let rect = Rect::new(area.x, area.y, area.width, height.min(area.height));
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.dialog_bg)),
        rect,
    );
}

fn dialog_lines(dialog: &CrewsDialog, theme: &Theme) -> (Vec<Line<'static>>, u16) {
    let title = Style::default()
        .fg(theme.text_primary)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(theme.dim_text);
    let field = Style::default().fg(theme.text_primary);
    match dialog {
        CrewsDialog::Unavailable {
            platform,
            model,
            reason,
            until,
            field: active,
        } => {
            let label = if model.is_empty() {
                format!("{platform}/(default)")
            } else {
                format!("{platform}/{model}")
            };
            let reason_marker = if *active == 0 { "▌" } else { "" };
            let until_marker = if *active == 1 { "▌" } else { "" };
            (
                vec![
                    Line::from(Span::styled(format!("Unavailable {label}"), title)),
                    Line::from(Span::styled(
                        format!("Reason: {reason}{reason_marker}"),
                        if *active == 0 { field } else { dim },
                    )),
                    Line::from(Span::styled(
                        format!("Until (YYYY-MM-DD HH:MM, empty = none): {until}{until_marker}"),
                        if *active == 1 { field } else { dim },
                    )),
                ],
                3,
            )
        }
        CrewsDialog::ProviderPrefix {
            pattern, covers, ..
        } => (
            vec![
                Line::from(Span::styled(format!("Mark {pattern} unavailable"), title)),
                Line::from(Span::styled(format!("covers {covers} pair(s)"), dim)),
                Line::from(Span::styled("Enter confirm · Esc cancel", dim)),
            ],
            3,
        ),
        CrewsDialog::AddMember {
            crew,
            platform,
            query,
            suggestions,
            idx,
            ..
        } => {
            let mut lines = vec![
                Line::from(Span::styled(
                    format!("Add member to {crew} ({platform})"),
                    title,
                )),
                Line::from(Span::styled(format!(">{query}▌"), field)),
            ];
            for (index, entry) in suggestions.iter().take(8).enumerate() {
                let style = if index == *idx {
                    Style::default()
                        .fg(theme.text_primary)
                        .bg(theme.selected_bg)
                } else {
                    dim
                };
                lines.push(Line::from(Span::styled(entry.id.clone(), style)));
            }
            let height = 2 + suggestions.len().min(8) as u16;
            (lines, height)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::roster_entries::{RosterEntry, RosterState};
    use crate::db::Database;
    use crate::tui::app::crews_logic::{build_pair_rows, CrewView, CrewsSnapshot, PairSummary};
    use crate::tui::app::types::App;
    use chrono::Utc;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_app() -> App {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempdir().unwrap();
        App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap()
    }

    fn render_to_text(width: u16, height: u16, app: &App) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_crews_face(frame, area, app, &Theme::classic());
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

    fn summary(platform: &str, model: &str) -> PairSummary {
        PairSummary {
            platform: platform.to_string(),
            model: model.to_string(),
            graph_count: 1,
            graph_names: vec!["g1".to_string()],
        }
    }

    fn roster_entry(
        id: i64,
        platform: &str,
        model: Option<&str>,
        until: Option<&str>,
    ) -> RosterEntry {
        RosterEntry {
            id,
            platform: platform.to_string(),
            model_pattern: model.map(str::to_string),
            state: RosterState::Unavailable,
            reason: Some("quota".to_string()),
            until: until.map(str::to_string),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn renders_pairs_with_all_three_glyphs() {
        let mut app = test_app();
        let snapshot = CrewsSnapshot {
            roster: vec![
                // Unavailable for `opencode-go/b` (reason shown)…
                roster_entry(1, "opencode-go", Some("b"), None),
                // …and an expired entry for `opencode-go/c`.
                roster_entry(2, "opencode-go", Some("c"), Some("2020-01-01T00:00:00Z")),
            ],
            pairs: vec![
                summary("claude", "a"),
                summary("opencode-go", "b"),
                summary("opencode-go", "c"),
            ],
            crews: Vec::new(),
        };
        app.crews.pair_rows = build_pair_rows(&snapshot, &Utc::now());
        app.crews.snapshot = Some(snapshot);

        let text = render_to_text(90, 20, &app);
        assert!(text.contains('●'), "available glyph:\n{text}");
        assert!(text.contains('○'), "unavailable glyph:\n{text}");
        assert!(text.contains('◐'), "expired glyph:\n{text}");
        assert!(text.contains("opencode-go/b"), "row label:\n{text}");
        assert!(text.contains("quota"), "reason column:\n{text}");
    }

    #[test]
    fn renders_expanded_crew_members_and_bindings() {
        use crate::tui::app::crews_logic::CrewBindingView;
        use serde_json::json;

        let mut app = test_app();
        let members = vec![
            json!({"platform": "claude", "model": null}),
            json!({"platform": "opencode-go", "model": "x"}),
        ];
        let crew = CrewView {
            name: "review".to_string(),
            description: None,
            version: 3,
            member_pairs: vec![
                ("claude".to_string(), String::new()),
                ("opencode-go".to_string(), "x".to_string()),
            ],
            members,
            bindings: vec![CrewBindingView {
                graph: "g1".to_string(),
                kind: "ensemble".to_string(),
                name: "reviewers".to_string(),
            }],
            binding_total: 1,
        };
        let snapshot = CrewsSnapshot {
            roster: Vec::new(),
            pairs: Vec::new(),
            crews: vec![crew],
        };
        app.crews.snapshot = Some(snapshot);
        app.crews.section = CrewsSection::Crews;
        app.crews.expanded = Some("review".to_string());

        let text = render_to_text(90, 20, &app);
        assert!(text.contains("1."), "first member line:\n{text}");
        assert!(text.contains("2."), "second member line:\n{text}");
        assert!(
            text.contains("g1 › ensemble|reviewers"),
            "binding line:\n{text}"
        );
    }

    #[test]
    fn keep_visible_scrolls_to_selection() {
        assert_eq!(keep_visible(0, 0, 10), 0);
        assert_eq!(keep_visible(9, 0, 10), 0, "last visible row doesn't scroll");
        assert_eq!(
            keep_visible(10, 0, 10),
            1,
            "one past the window scrolls by 1"
        );
        assert_eq!(keep_visible(2, 5, 10), 2, "above the window jumps to it");
        assert_eq!(keep_visible(2, 5, 0), 5, "a zero-height window is a no-op");
    }
}
