use std::cmp::Reverse;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::domain::graphs::{Graph, GraphStatus};
use crate::tui::app::types::{App, GraphSidebarMeta};
use crate::tui::ui::theme::Theme;
use crate::tui::ui::{borders_for, last_two_segments, truncate_str};

use super::sync::{draw_panel_placeholder, graph_status_color};

const CARD_HEIGHT: u16 = 6;

/// Render one 6-row card per active (`Running` or `Pausing`) graph, or the
/// selected graph when none are active, with scroll indicator and click map.
pub(super) fn draw_graph_cards(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    app.last_graph_face_rect = Some(area);
    app.graph_card_click_map.clear();

    let cards = ordered_card_graphs(app);
    if cards.is_empty() {
        app.graph_face_total_lines = 0;
        draw_panel_placeholder(frame, area, "no graphs yet", theme);
        return;
    }

    let n = cards.len();
    app.graph_face_total_lines = (CARD_HEIGHT as usize * n) as u16;

    let all_fit = area.height as usize >= CARD_HEIGHT as usize * n;
    let cards_h = if all_fit {
        area.height
    } else {
        area.height.saturating_sub(1)
    };
    let visible = (cards_h / CARD_HEIGHT) as usize;
    let first = (app.graph_face_scroll as usize / CARD_HEIGHT as usize).min(n - 1);
    let end = (first + visible).min(n);

    for (idx, graph) in cards[first..end].iter().enumerate() {
        let y = area.y + (idx as u16) * CARD_HEIGHT;
        let card_rect = Rect::new(area.x, y, area.width, CARD_HEIGHT);
        app.graph_card_click_map
            .push((graph.id.clone(), y, y + CARD_HEIGHT));
        let meta = app
            .graph_sidebar_meta
            .get(&graph.id)
            .cloned()
            .unwrap_or_default();
        draw_one_card(frame, card_rect, graph, &meta, theme);
    }

    if !all_fit && end > first && area.height > 0 {
        let indicator = format!("({}-{} of {})", first + 1, end, n);
        let row = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_str(&indicator, area.width as usize),
                Style::default().fg(theme.dim_text),
            ))),
            row,
        );
    }
}

fn ordered_card_graphs(app: &App) -> Vec<Graph> {
    let mut active: Vec<Graph> = app
        .graphs
        .iter()
        .filter(|g| matches!(g.status, GraphStatus::Running | GraphStatus::Pausing))
        .cloned()
        .collect();
    if active.is_empty() {
        if let Some(selected) = app
            .selected_graph_id
            .as_deref()
            .and_then(|id| app.graphs.iter().find(|g| g.id == id))
        {
            active.push(selected.clone());
        }
        return active;
    }

    active.sort_by(|a, b| {
        let a_ts = app.graph_sidebar_meta.get(&a.id).map(|m| m.last_activity);
        let b_ts = app.graph_sidebar_meta.get(&b.id).map(|m| m.last_activity);
        Reverse(a_ts)
            .cmp(&Reverse(b_ts))
            .then_with(|| a.name.cmp(&b.name))
    });
    active
}

fn draw_one_card(
    frame: &mut Frame,
    area: Rect,
    graph: &Graph,
    meta: &GraphSidebarMeta,
    theme: &Theme,
) {
    let block = Block::default()
        .borders(borders_for(theme))
        .border_style(Style::default().fg(theme.border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let w = inner.width as usize;
    let icon = match graph.status {
        GraphStatus::Running => "▶",
        GraphStatus::Pausing => "‖",
        _ => "○",
    };
    let row1 = Line::from(vec![
        Span::styled(icon, Style::default().fg(graph_status_color(graph.status))),
        Span::raw(" "),
        Span::styled(
            truncate_str(&graph.name, w.saturating_sub(2)),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let path = last_two_segments(&graph.workdir);
    let row2 = Line::from(Span::styled(
        truncate_str(&path, w),
        Style::default().fg(theme.dim_text),
    ));

    let row3 = progress_bar_line(meta.done, meta.total, w, graph.status, theme);

    let spec_text = match (
        meta.current_spec_code.is_empty(),
        meta.current_spec_intent.is_empty(),
    ) {
        (true, true) => String::new(),
        (true, false) => meta.current_spec_intent.clone(),
        (false, true) => meta.current_spec_code.clone(),
        (false, false) => format!("{} {}", meta.current_spec_code, meta.current_spec_intent),
    };
    let row4 = Line::from(Span::styled(
        truncate_str(&spec_text, w),
        Style::default().fg(Color::White),
    ));

    frame.render_widget(Paragraph::new(vec![row1, row2, row3, row4]), inner);
}

fn progress_bar_line(
    done: usize,
    total: usize,
    width: usize,
    status: GraphStatus,
    theme: &Theme,
) -> Line<'static> {
    let counter = format!("{done}/{total} ");
    let counter_w = counter.chars().count();
    let bar_w = width.saturating_sub(counter_w);
    let filled = (done * bar_w).checked_div(total).unwrap_or(0).min(bar_w);
    let empty = bar_w - filled;
    Line::from(vec![
        Span::styled(
            truncate_str(&counter, width),
            Style::default().fg(theme.dim_text),
        ),
        Span::styled(
            "█".repeat(filled),
            Style::default().fg(graph_status_color(status)),
        ),
        Span::styled("░".repeat(empty), Style::default().fg(theme.dim_text)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::tui::app::types::{AutomationKind, Focus, SidebarLayer};
    use chrono::{Duration, Utc};
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use std::collections::BTreeMap;

    fn test_app() -> App {
        let path = crate::test_support::temp_db_path();
        let db = std::sync::Arc::new(Database::new(&path).expect("create test db"));
        let data_dir = tempfile::tempdir().expect("create data dir");
        let dir_path = data_dir.path().to_path_buf();
        crate::test_support::hold(data_dir);
        App::new(
            db,
            &dir_path,
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app")
    }

    fn make_graph(id: &str, name: &str, workdir: &str, status: GraphStatus) -> Graph {
        Graph {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            workdir: workdir.to_string(),
            status,
            trigger: None,
            trigger_prompt: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: BTreeMap::new(),
            require_spec_type: false,
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            enabled: true,
            enable_at: None,
            expires_at: None,
        }
    }

    fn render_cards(app: &mut App, width: u16, height: u16) -> String {
        let theme = Theme::classic();
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_graph_cards(frame, area, app, &theme);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn card_renders_running_and_pausing_icons() {
        let mut app = test_app();
        app.graphs = vec![
            make_graph("g1", "Alpha", "/tmp/proj/alpha", GraphStatus::Running),
            make_graph("g2", "Beta", "/tmp/proj/beta", GraphStatus::Pausing),
        ];
        app.graph_sidebar_meta.insert(
            "g1".into(),
            GraphSidebarMeta {
                last_activity: Utc::now(),
                done: 1,
                total: 2,
                current_spec_code: "C-1".into(),
                current_spec_intent: "build cards".into(),
                ..Default::default()
            },
        );
        app.graph_sidebar_meta.insert(
            "g2".into(),
            GraphSidebarMeta {
                last_activity: Utc::now() - Duration::seconds(10),
                done: 0,
                total: 1,
                current_spec_code: String::new(),
                current_spec_intent: "pause work".into(),
                ..Default::default()
            },
        );

        let text = render_cards(&mut app, 40, 12);
        assert!(text.contains("▶ Alpha"), "missing running icon:\n{text}");
        assert!(text.contains("‖ Beta"), "missing pausing icon:\n{text}");
        assert!(text.contains("C-1 build cards"), "missing spec:\n{text}");
        assert!(text.contains("pause work"), "missing intent:\n{text}");
    }

    #[test]
    fn card_truncates_at_20_40_and_80_columns() {
        let long_name = "VeryLongGraphNameThatExceedsWidth";
        let long_intent = "A".repeat(90);
        for width in [20u16, 40, 80] {
            let mut app = test_app();
            app.graphs = vec![make_graph(
                "g1",
                long_name,
                "/a/b/c/workdir",
                GraphStatus::Running,
            )];
            app.graph_sidebar_meta.insert(
                "g1".into(),
                GraphSidebarMeta {
                    done: 1,
                    total: 2,
                    current_spec_code: "LONG-99".into(),
                    current_spec_intent: long_intent.clone(),
                    ..Default::default()
                },
            );
            let text = render_cards(&mut app, width, 6);
            for line in text.lines() {
                assert_eq!(
                    line.chars().count(),
                    width as usize,
                    "row width mismatch at {width}:\n{text}"
                );
            }
        }
    }

    #[test]
    fn progress_bar_renders_0_0_3_10_and_10_10() {
        let theme = Theme::classic();
        let line_text = |done, total| -> String {
            progress_bar_line(done, total, 20, GraphStatus::Running, &theme)
                .spans
                .into_iter()
                .map(|s| s.content.into_owned())
                .collect()
        };

        // "0/0 " = 4 chars, bar_w = 16 -> 0 filled, 16 empty
        assert_eq!(line_text(0, 0), format!("0/0 {}", "░".repeat(16)));
        // "3/10 " = 5 chars, bar_w = 15 -> 3 * 15 / 10 = 4 filled, 11 empty
        assert_eq!(
            line_text(3, 10),
            format!("3/10 {}{}", "█".repeat(4), "░".repeat(11))
        );
        // "10/10 " = 6 chars, bar_w = 14 -> 14 filled, 0 empty
        assert_eq!(line_text(10, 10), format!("10/10 {}", "█".repeat(14)));
    }

    #[test]
    fn cards_ordered_by_last_activity_desc_then_name_asc() {
        let mut app = test_app();
        let now = Utc::now();
        app.graphs = vec![
            make_graph("g-old", "Zeta", "/tmp/z", GraphStatus::Running),
            make_graph("g-tie-b", "Bravo", "/tmp/b", GraphStatus::Running),
            make_graph("g-tie-a", "Alpha", "/tmp/a", GraphStatus::Running),
        ];
        app.graph_sidebar_meta.insert(
            "g-old".into(),
            GraphSidebarMeta {
                last_activity: now - Duration::seconds(60),
                ..Default::default()
            },
        );
        app.graph_sidebar_meta.insert(
            "g-tie-b".into(),
            GraphSidebarMeta {
                last_activity: now,
                ..Default::default()
            },
        );
        app.graph_sidebar_meta.insert(
            "g-tie-a".into(),
            GraphSidebarMeta {
                last_activity: now,
                ..Default::default()
            },
        );

        let text = render_cards(&mut app, 40, 18);
        let alpha = text.find("Alpha").unwrap();
        let bravo = text.find("Bravo").unwrap();
        let zeta = text.find("Zeta").unwrap();
        assert!(alpha < bravo && bravo < zeta, "wrong order:\n{text}");
    }

    #[test]
    fn scroll_indicator_appears_when_more_cards_than_fit() {
        let mut app = test_app();
        let now = Utc::now();
        app.graphs = (0..3)
            .map(|i| {
                make_graph(
                    &format!("g{i}"),
                    &format!("Graph{i}"),
                    "/tmp/p",
                    GraphStatus::Running,
                )
            })
            .collect();
        for i in 0..3 {
            app.graph_sidebar_meta.insert(
                format!("g{i}"),
                GraphSidebarMeta {
                    last_activity: now - Duration::seconds(i as i64),
                    ..Default::default()
                },
            );
        }

        // Height 13: 2 cards fit (12 rows) + 1 indicator row.
        let text = render_cards(&mut app, 40, 13);
        assert!(text.contains("(1-2 of 3)"), "missing indicator:\n{text}");
        assert_eq!(app.graph_face_total_lines, 18);
    }

    #[test]
    fn click_inside_card_selects_graph_and_outside_falls_through() {
        let mut app = test_app();
        app.graphs = vec![make_graph(
            "g1",
            "Alpha",
            "/tmp/alpha",
            GraphStatus::Running,
        )];
        let sync_area = Rect::new(40, 2, 40, 18);
        app.last_sync_area = Some(sync_area);
        let theme = Theme::classic();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_graph_cards(frame, sync_area, &mut app, &theme);
            })
            .unwrap();

        let click_outside = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 45,
            row: 12,
            modifiers: KeyModifiers::empty(),
        };
        assert!(crate::tui::event::mouse::handle_panel_mouse(
            &mut app,
            &click_outside
        ));
        assert!(app.panel_focused);
        assert_eq!(app.selected_graph_id, None);

        let click_card = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 45,
            row: 4,
            modifiers: KeyModifiers::empty(),
        };
        assert!(crate::tui::event::mouse::handle_panel_mouse(
            &mut app,
            &click_card
        ));
        assert_eq!(app.selected_graph_id.as_deref(), Some("g1"));
        assert_eq!(app.sidebar_layer, SidebarLayer::Automation);
        assert_eq!(app.automation_kind, AutomationKind::Graph);
        assert!(matches!(app.focus, Focus::Preview));
    }
}
