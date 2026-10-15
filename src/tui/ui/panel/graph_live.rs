//! The live graph view: a read-only render of [`GraphLiveState`] for the
//! main panel — header, spec queue, current-spec graph (auto-following the
//! engine's current node, or a manually-highlighted one), and a detail
//! footer. Pure over the snapshot; the only I/O is the `App` glue in
//! [`draw_graph_live_view`], which assembles plain values before handing off
//! to [`render_graph_live_view`].

use chrono::{DateTime, Utc};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::super::theme::Theme;
use crate::tui::app::graph_live_state::{GraphLiveState, NodeRunInfo};
use crate::tui::app::types::App;

mod lines;
mod render;

use render::render_graph_live_view;

#[cfg(test)]
use crate::domain::graphs::{GraphEdgeCondition, GraphRunStatus};
#[cfg(test)]
use crate::tui::app::graph_live_state::{EnsembleLiveInfo, SpecQueueEntry};
#[cfg(test)]
use lines::{ensemble_box_lines, graph_lines, node_box_lines};
#[cfg(test)]
use ratatui::style::Color;
#[cfg(test)]
use render::{spec_strip_layout, SPEC_CHIP_WIDTH};
#[cfg(test)]
use std::collections::HashMap;

pub(crate) fn draw_graph_live_view(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        app.graph_spec_strip_click_map.clear();
        return;
    }
    let Some(state) = app.graph_live_state.as_ref() else {
        app.graph_spec_strip_click_map.clear();
        frame.render_widget(
            Paragraph::new("No graph selected").style(Style::default().fg(theme.dim_text)),
            area,
        );
        return;
    };

    let blocked = app
        .graph_sidebar_meta
        .get(&state.graph_id)
        .is_some_and(|meta| meta.blocked);
    let highlighted = app.graph_live_highlighted_node_id().map(str::to_string);
    let node_info = app.graph_live_highlighted_node_run_info();
    let selected_spec_id = app.graph_spec_strip_selected.clone();
    let spec_scroll = app.graph_spec_strip_scroll;

    // CT28: split the area when the tail pane is open — graph on the left
    // (≥140 cols) or top (<140), pane taking the rest.
    let (graph_area, pane_area) = if app.node_tail_dialog_active() {
        tail_pane_split(area)
    } else {
        (area, Rect::default())
    };

    let result = render_graph_live_view(
        frame,
        graph_area,
        &LiveViewContext {
            state,
            follow: app.graph_live_follow,
            follow_anchor: app.graph_live_follow_anchor.as_deref(),
            highlighted_node_id: highlighted.as_deref(),
            node_info: &node_info,
            blocked,
            now: Utc::now(),
            theme,
            time_format: &app.tui_settings.time_format,
            selected_spec_id: selected_spec_id.as_deref(),
            spec_scroll,
            scroll: app.graph_live_view_scroll,
        },
    );

    app.graph_spec_strip_click_map = result.click_map;
    app.graph_spec_strip_capacity = result.capacity;
    app.graph_live_view_total_lines = result.total_lines;
    app.graph_live_view_scroll = result.clamped_scroll;
    app.graph_live_follow_anchor = result.follow_anchor;

    // CT28: live-tail pane renders beside the graph. Read-only viewer.
    if let Some(dialog) = app.node_tail_dialog.as_ref() {
        super::super::dialogs::draw_node_tail_pane(frame, pane_area, dialog, theme, Utc::now());
    }
    app.node_tail_pane_body_height = pane_area.height.saturating_sub(2) as usize;
}

/// CT28: split `area` into (graph, pane). Width ≥ 140 → horizontal (50/50);
/// otherwise vertical (graph 40%, ≥ 8 rows, pane below).
fn tail_pane_split(area: Rect) -> (Rect, Rect) {
    if area.width >= 140 {
        let cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        (cols[0], cols[1])
    } else {
        let graph_h = ((area.height * 40) / 100).max(8).min(area.height);
        let rows = Layout::vertical([Constraint::Length(graph_h), Constraint::Min(0)]).split(area);
        (rows[0], rows[1])
    }
}

/// Everything the pure renderer needs, gathered by [`draw_graph_live_view`]
/// so the render itself stays a plain function of already-computed values
/// (no `Database`/`App` access), which is what makes it testable without a
/// full `App`.
struct LiveViewContext<'a> {
    state: &'a GraphLiveState,
    follow: bool,
    /// CT8: the node id the graph last re-centred on while auto-following
    /// (mirrors `App::graph_live_follow_anchor`). Compared against
    /// `highlighted_node_id` — which equals the engine's current node id
    /// whenever `follow` is true — to decide whether this frame is a real
    /// node transition that should re-centre the scroll.
    follow_anchor: Option<&'a str>,
    highlighted_node_id: Option<&'a str>,
    node_info: &'a NodeRunInfo,
    blocked: bool,
    now: DateTime<Utc>,
    theme: &'a Theme,
    /// CM52: chrono format for the time-only labels (`tui.time_format`).
    time_format: &'a str,
    /// Spec id manually selected in the marker strip, if any — independent
    /// of `highlighted_node_id`/`follow`, which are the *graph's* own
    /// follow/manual state.
    selected_spec_id: Option<&'a str>,
    /// First visible index into `state.spec_queue` for the marker strip.
    spec_scroll: usize,
    scroll: u16,
}

/// What [`render_graph_live_view`] hands back to its `App`-owning caller:
/// where the marker strip's chips actually landed on screen, for mouse
/// hit-testing next frame (mirrors `sidebar_tab_click_map`'s shape).
struct LiveViewRenderResult {
    click_map: Vec<(String, u16, u16, u16)>,
    capacity: usize,
    total_lines: u16,
    clamped_scroll: u16,
    /// CT8: the anchor to persist for next frame. In auto-follow this is the
    /// current node id (adopted whether or not we re-centred this frame); in
    /// manual it is carried through unchanged.
    follow_anchor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::graphs::{
        GraphEdge, GraphNode, GraphNodeKind, GraphSpecStatus, GraphStatus as DomainGraphStatus,
    };
    use crate::tui::app::types::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use serde_json::json;
    use std::sync::Arc;

    fn render_to_text(width: u16, height: u16, draw: impl FnOnce(&mut Frame, Rect)) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw(frame, area);
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

    fn team_nodes() -> Vec<GraphNode> {
        let kinds = [
            ("Implement", GraphNodeKind::Agent),
            ("Cargo gates", GraphNodeKind::Check),
            ("Review + commit", GraphNodeKind::Agent),
            ("Check committed", GraphNodeKind::Check),
            ("Resilience", GraphNodeKind::Agent),
        ];
        kinds
            .iter()
            .enumerate()
            .map(|(i, (name, kind))| GraphNode {
                id: format!("n{i}"),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: name.to_string(),
                kind: *kind,
                config: json!({}),
                position: i as i64,
                created_at: Utc::now(),
            })
            .collect()
    }

    fn team_edges() -> Vec<GraphEdge> {
        // Mirrors the real 5-node team graph: cycles back to Implement (n0)
        // on failure at any later stage, and Resilience (n4) graphs back to
        // Implement on its own pass.
        vec![
            ("n0", "n1", GraphEdgeCondition::Pass),
            ("n0", "n4", GraphEdgeCondition::Fail),
            ("n4", "n0", GraphEdgeCondition::Pass),
            ("n1", "n2", GraphEdgeCondition::Pass),
            ("n1", "n0", GraphEdgeCondition::Fail),
            ("n2", "n3", GraphEdgeCondition::Pass),
            ("n2", "n0", GraphEdgeCondition::Fail),
            ("n3", "n2", GraphEdgeCondition::Fail),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (from, to, condition))| GraphEdge {
            id: format!("e{i}"),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: from.to_string(),
            to_node: to.to_string(),
            condition,
        })
        .collect()
    }

    fn running_state() -> GraphLiveState {
        GraphLiveState {
            graph_id: "lp1".to_string(),
            graph_name: "canopy-ux-notifications".to_string(),
            graph_status: DomainGraphStatus::Running,
            workdir: "/home/user/Projects/harness-canopy".to_string(),
            trigger_type: "manual".to_string(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            spec_queue: vec![
                SpecQueueEntry {
                    spec_id: "s1".to_string(),
                    spec_name: "B1 fix".to_string(),
                    status: GraphSpecStatus::Completed,
                    failure_reason: None,
                },
                SpecQueueEntry {
                    spec_id: "s2".to_string(),
                    spec_name: "U1b live view".to_string(),
                    status: GraphSpecStatus::Running,
                    failure_reason: None,
                },
                SpecQueueEntry {
                    spec_id: "s3".to_string(),
                    spec_name: "T1 theme".to_string(),
                    status: GraphSpecStatus::Pending,
                    failure_reason: None,
                },
            ],
            done_count: 1,
            total_count: 3,
            current_spec_id: Some("s2".to_string()),
            effective_nodes: team_nodes(),
            effective_edges: team_edges(),
            ensembles: Vec::new(),
            router_taken_routes: HashMap::new(),
            current_node_id: Some("n0".to_string()),
            current_node_status: Some(GraphRunStatus::Running),
            current_node_started_at: Some(Utc::now() - chrono::Duration::seconds(75)),
            current_node_iteration: Some(2),
        }
    }

    #[test]
    fn running_graph_renders_header_queue_graph_and_footer_with_current_node_highlighted() {
        let state = running_state();
        let node_info = NodeRunInfo {
            status: state.current_node_status,
            started_at: state.current_node_started_at,
            iteration: state.current_node_iteration,
            chosen_route: None,
        };

        let text = render_to_text(80, 40, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: state.current_node_id.as_deref(),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });

        // Header.
        assert!(text.contains("canopy-ux-notifications"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(text.contains("1/3 specs"), "{text}");
        assert!(text.contains("harness-canopy"), "{text}");
        // Queue.
        assert!(text.contains("U1b live view"), "{text}");
        // Graph — all five team nodes present.
        for name in [
            "Implement",
            "Cargo gates",
            "Review + commit",
            "Check committed",
            "Resilience",
        ] {
            assert!(text.contains(name), "missing node {name} in:\n{text}");
        }
        assert!(text.contains("pass →"), "{text}");
        assert!(text.contains("fail →"), "{text}");
        // The auto-followed current node (Implement/n0) uses the solid
        // follow marker.
        assert!(text.contains("●"), "expected follow marker in:\n{text}");
        // Footer.
        assert!(text.contains("iter 2"), "{text}");
        assert!(text.contains("elapsed"), "{text}");
    }

    #[test]
    fn manual_selection_switches_footer_to_the_picked_node() {
        let state = running_state();

        let node_info = NodeRunInfo {
            status: Some(GraphRunStatus::Pass),
            started_at: Some(Utc::now() - chrono::Duration::seconds(10)),
            iteration: Some(1),
            chosen_route: None,
        };
        let text = render_to_text(80, 40, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: false,
                    follow_anchor: None,
                    highlighted_node_id: Some("n2"),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });

        assert!(text.contains("manual"), "{text}");
        // Manual pick uses the `›` marker, not the follow `●`.
        assert!(text.contains('›'), "expected manual marker in:\n{text}");
    }

    #[test]
    fn spec_strip_shows_distinct_markers_and_selected_detail_with_failure_reason() {
        let mut state = running_state();
        // s1 stays Completed (✓); make s3 Failed with a recorded reason and
        // add a fourth, Skipped spec — before this, Failed/Skipped/Completed
        // all rendered the same `✓`.
        state.spec_queue[2].status = GraphSpecStatus::Failed;
        state.spec_queue[2].failure_reason = Some("cargo test failed: 3 tests failing".to_string());
        state.spec_queue.push(SpecQueueEntry {
            spec_id: "s4".to_string(),
            spec_name: "T2 skipped thing".to_string(),
            status: GraphSpecStatus::Skipped,
            failure_reason: Some("superseded by s5".to_string()),
        });

        let node_info = NodeRunInfo::default();
        let text = render_to_text(80, 40, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: None,
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: Some("s3"),
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });

        // Distinct glyphs for completed (s1), failed (s3), and skipped (s4).
        assert!(text.contains('✓'), "{text}");
        assert!(text.contains('✗'), "{text}");
        assert!(text.contains('⊘'), "{text}");
        // The selected marker (s3, failed) is bracketed, distinct from an
        // unselected chip and from the graph's `●` follow marker.
        assert!(
            text.contains("[✗]"),
            "expected bracketed selection marker in:\n{text}"
        );
        // Selecting a spec shows its name, status, and the recorded failure
        // reason — not just the running spec's name shown pre-selection.
        assert!(text.contains("T1 theme"), "{text}");
        assert!(text.contains("[failed]"), "{text}");
        assert!(
            text.contains("cargo test failed: 3 tests failing"),
            "{text}"
        );
    }

    #[test]
    fn spec_strip_wraps_across_multiple_lines_when_many_specs_fit() {
        let mut state = running_state();
        state.spec_queue = (0..20)
            .map(|i| SpecQueueEntry {
                spec_id: format!("s{i}"),
                spec_name: format!("Spec {i}"),
                status: GraphSpecStatus::Pending,
                failure_reason: None,
            })
            .collect();
        state.current_spec_id = None;
        state.total_count = 20;

        let node_info = NodeRunInfo::default();
        let text = render_to_text(40, 40, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: None,
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });

        assert!(
            !text.contains("of 20"),
            "with tall area all 20 chips should wrap without truncation, but got:\n{text}"
        );
        // All 20 markers present — each ○ occupies a chip, so at least 20 appear.
        assert!(
            text.matches('○').count() >= 20,
            "expected all 20 spec glyphs visible across wrapped lines in:\n{text}"
        );
    }

    #[test]
    fn spec_strip_wraps_21_specs_across_rows_and_click_map_tracks_row() {
        let mut state = running_state();
        state.spec_queue = (0..21)
            .map(|i| SpecQueueEntry {
                spec_id: format!("s{i}"),
                spec_name: format!("Spec {i}"),
                status: GraphSpecStatus::Pending,
                failure_reason: None,
            })
            .collect();
        state.current_spec_id = None;
        state.total_count = 21;

        let theme = Theme::classic();
        // Direct layout check with narrow but tall area: 40 wide -> 10 per line, tall enough for all.
        let layout = spec_strip_layout(&state, None, 0, 40, 40, &theme);
        assert_eq!(
            layout.click_map.len(),
            21,
            "all 21 chips must be present when height allows"
        );
        assert_eq!(
            layout.chip_line_count, 3,
            "21 chips at 10/line needs 3 lines"
        );
        // First 10 on row 0, next 10 on row 1, last on row 2.
        for i in 0..10 {
            assert_eq!(layout.click_map[i].1, 0, "spec s{i} should be on row 0");
        }
        for i in 10..20 {
            assert_eq!(layout.click_map[i].1, 1, "spec s{i} should be on row 1");
        }
        assert_eq!(layout.click_map[20].1, 2);

        // Clicking spec at index 15 (row 1) returns correct spec id.
        let (row, col) = (layout.click_map[15].1, layout.click_map[15].2);
        let hit = layout
            .click_map
            .iter()
            .find(|(_, r, c0, c1)| *r == row && *c0 <= col && col < *c1)
            .map(|(id, _, _, _)| id.as_str());
        assert_eq!(hit, Some("s15"));

        // Also visible in rendered text with no range indicator.
        let node_info = NodeRunInfo::default();
        let text = render_to_text(40, 40, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: None,
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });
        assert!(
            !text.contains("of 21"),
            "all chips visible so no range suffix:\n{text}"
        );
        assert!(
            text.matches('○').count() >= 21,
            "expected 21 glyphs in:\n{text}"
        );
    }

    #[test]
    fn spec_strip_falls_back_to_scroll_when_height_is_tight() {
        let mut state = running_state();
        state.spec_queue = (0..21)
            .map(|i| SpecQueueEntry {
                spec_id: format!("s{i}"),
                spec_name: format!("Spec {i}"),
                status: GraphSpecStatus::Pending,
                failure_reason: None,
            })
            .collect();
        state.current_spec_id = None;
        state.total_count = 21;

        let theme = Theme::classic();
        // Height 10 -> max_chip_lines = 2, so only 2 lines fit -> truncated.
        let layout = spec_strip_layout(&state, None, 0, 40, 10, &theme);
        assert!(
            layout.capacity < 21,
            "capacity {} should be < 21 when height is tight",
            layout.capacity
        );
        assert_eq!(layout.chip_line_count, 2);

        let node_info = NodeRunInfo::default();
        let text = render_to_text(40, 10, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: None,
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });
        // The suffix "(1-17 of 21)" may wrap across two buffer rows when the
        // area is narrow; normalize whitespace so the assertion is not fragile
        // to buffer padding while still requiring the indicator to be present.
        let flat = text.replace('\n', " ");
        let normalized = flat.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized.contains("of 21"),
            "expected range indicator when height is tight in:\n{text}"
        );
    }

    #[test]
    fn spec_strip_single_line_when_few_specs() {
        let state = running_state();
        // running_state has 3 specs.
        let theme = Theme::classic();
        let layout = spec_strip_layout(&state, None, 0, 80, 40, &theme);
        assert_eq!(layout.chip_line_count, 1);
        assert!(
            !layout.lines.iter().any(|l| l.to_string().contains("of ")),
            "no range indicator with few specs"
        );
        assert_eq!(layout.click_map.len(), 3);
        assert!(layout.click_map.iter().all(|(_, row, _, _)| *row == 0));

        let node_info = NodeRunInfo::default();
        let text = render_to_text(80, 40, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: None,
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });
        assert!(!text.contains("of "), "no range with few specs in:\n{text}");
    }

    #[test]
    fn spec_strip_click_map_matches_rendered_chip_positions() {
        let state = running_state();
        let node_info = NodeRunInfo::default();
        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut result_holder = None;
        terminal
            .draw(|frame| {
                let area = frame.area();
                result_holder = Some(render_graph_live_view(
                    frame,
                    area,
                    &LiveViewContext {
                        state: &state,
                        follow: true,
                        follow_anchor: None,
                        highlighted_node_id: None,
                        node_info: &node_info,
                        blocked: false,
                        now: Utc::now(),
                        theme: &Theme::classic(),
                        time_format: "%H:%M",
                        selected_spec_id: None,
                        spec_scroll: 0,
                        scroll: 0,
                    },
                ));
            })
            .unwrap();
        let result = result_holder.unwrap();

        assert_eq!(result.click_map.len(), 3);
        assert_eq!(result.capacity, (80 / SPEC_CHIP_WIDTH) as usize);
        let ids: Vec<&str> = result
            .click_map
            .iter()
            .map(|(id, _, _, _)| id.as_str())
            .collect();
        assert_eq!(ids, vec!["s1", "s2", "s3"]);
        // All three chips render on the same row.
        let row = result.click_map[0].1;
        assert!(result.click_map.iter().all(|&(_, r, _, _)| r == row));
        // Columns are ordered and non-overlapping.
        assert!(result.click_map[0].2 < result.click_map[1].2);
        assert!(result.click_map[1].2 < result.click_map[2].2);
    }

    #[test]
    fn esc_restores_follow_via_app_state() {
        let (db, data_dir) = test_db_and_dir();
        seed_running_graph(&db);

        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        let auto_node = app.graph_live_highlighted_node_id().map(str::to_string);
        assert!(app.graph_live_follow);

        app.graph_live_move_highlight(true);
        assert!(!app.graph_live_follow);
        let manual_node = app.graph_live_highlighted_node_id().map(str::to_string);
        assert_ne!(auto_node, manual_node);

        app.graph_live_reset_follow();
        assert!(app.graph_live_follow);
        assert_eq!(
            app.graph_live_highlighted_node_id().map(str::to_string),
            auto_node
        );
    }

    #[test]
    fn narrow_width_render_does_not_panic() {
        let state = running_state();
        let node_info = NodeRunInfo::default();
        let ctx = LiveViewContext {
            state: &state,
            follow: true,
            follow_anchor: None,
            highlighted_node_id: None,
            node_info: &node_info,
            blocked: false,
            now: Utc::now(),
            theme: &Theme::classic(),
            time_format: "%H:%M",
            selected_spec_id: None,
            spec_scroll: 0,
            scroll: 0,
        };
        render_to_text(1, 5, |frame, area| {
            render_graph_live_view(frame, area, &ctx);
        });
        render_to_text(0, 0, |frame, area| {
            render_graph_live_view(frame, area, &ctx);
        });
    }

    #[test]
    fn completed_graph_renders_statically_with_completed_icon() {
        let mut state = running_state();
        state.graph_status = DomainGraphStatus::Completed;
        state.current_node_status = Some(GraphRunStatus::Pass);
        state.done_count = 3;
        state.current_spec_id = None;

        let node_info = NodeRunInfo {
            status: state.current_node_status,
            started_at: state.current_node_started_at,
            iteration: state.current_node_iteration,
            chosen_route: None,
        };

        let text = render_to_text(80, 40, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: state.current_node_id.as_deref(),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });

        assert!(text.contains("completed"), "{text}");
        assert!(text.contains("3/3 specs"), "{text}");
    }

    fn ensemble_live_fixture() -> EnsembleLiveInfo {
        EnsembleLiveInfo {
            ensemble_id: "ens1".to_string(),
            name: "Proposers".to_string(),
            join_node_id: "join1".to_string(),
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            members: vec![
                crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
                    node_id: "m1".to_string(),
                    label: "openrouter/deepseek".to_string(),
                    status: Some(GraphRunStatus::Pass),
                },
                crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
                    node_id: "m2".to_string(),
                    label: "openrouter/qwen".to_string(),
                    status: Some(GraphRunStatus::Running),
                },
                crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
                    node_id: "m3".to_string(),
                    label: "openrouter/llama".to_string(),
                    status: None,
                },
            ],
        }
    }

    #[test]
    fn ensemble_renders_as_one_collapsed_box_with_per_member_status() {
        let mut state = running_state();
        // kickoff -> {m1, m2, m3} -> join -> arbiter, replacing the plain
        // team graph so the ensemble is the only thing on screen.
        state.effective_nodes = vec![
            GraphNode {
                id: "kickoff".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Kickoff".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "m1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Proposers [1]".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 1,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "m2".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Proposers [2]".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 2,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "m3".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Proposers [3]".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 3,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "join1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Proposers (quorum)".to_string(),
                kind: GraphNodeKind::Join,
                config: json!({}),
                position: 4,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "arbiter".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Arbiter".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 5,
                created_at: Utc::now(),
            },
        ];
        state.effective_edges = vec![
            ("kickoff", "m1", GraphEdgeCondition::Always),
            ("kickoff", "m2", GraphEdgeCondition::Always),
            ("kickoff", "m3", GraphEdgeCondition::Always),
            ("m1", "join1", GraphEdgeCondition::Always),
            ("m2", "join1", GraphEdgeCondition::Always),
            ("m3", "join1", GraphEdgeCondition::Always),
            ("join1", "arbiter", GraphEdgeCondition::Pass),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (from, to, condition))| GraphEdge {
            id: format!("ee{i}"),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: from.to_string(),
            to_node: to.to_string(),
            condition,
        })
        .collect();
        state.ensembles = vec![ensemble_live_fixture()];
        state.current_node_id = Some("kickoff".to_string());

        let node_info = NodeRunInfo::default();
        let text = render_to_text(80, 40, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: state.current_node_id.as_deref(),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });

        // Collapsed to one box with the member count, not three separate
        // member boxes or a fourth box for the join.
        assert!(text.contains("Proposers [3 models]"), "{text}");
        assert!(!text.contains("Proposers [1]"), "{text}");
        assert!(!text.contains("Proposers [2]"), "{text}");
        assert!(!text.contains("Proposers (quorum)"), "{text}");
        // Per-member live status inside the collapsed box.
        assert!(text.contains("[pass]"), "{text}");
        assert!(text.contains("[running]"), "{text}");
        assert!(text.contains("[pending]"), "{text}");
        // The fan-out from kickoff collapses to one edge, and the join's own
        // routing to the arbiter still renders.
        assert!(text.contains("Kickoff"), "{text}");
        assert!(text.contains("Arbiter"), "{text}");
    }

    fn ensemble_fixture_with_labels(labels: &[&str]) -> EnsembleLiveInfo {
        EnsembleLiveInfo {
            ensemble_id: "ens1".to_string(),
            name: "Implementer".to_string(),
            join_node_id: "join1".to_string(),
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            members: labels
                .iter()
                .enumerate()
                .map(
                    |(i, label)| crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
                        node_id: format!("m{i}"),
                        label: label.to_string(),
                        status: Some(GraphRunStatus::Pass),
                    },
                )
                .collect(),
        }
    }

    #[test]
    fn ensemble_box_rows_share_one_width_with_a_trailing_blank_column() {
        let long = "openrouter/very-long-model-name-that-will-not-fit-in-the-box";
        let ensemble = ensemble_fixture_with_labels(&[long, long, long, long]);
        let inner = 20;
        let lines = ensemble_box_lines(&ensemble, false, false, inner, &Theme::classic(), 0);

        // top border, title, 4 members, bottom border
        assert_eq!(lines.len(), 7);
        let expected_width = lines[0].width();
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(
                line.width(),
                expected_width,
                "line {i} width mismatch: {line}"
            );
        }
        // Every content row (not the pure border rows) has a space just before
        // the closing │.
        for (i, line) in lines.iter().enumerate().skip(1).take(5) {
            let text = line.to_string();
            let before_border = text.chars().rev().nth(1);
            assert_eq!(
                before_border,
                Some(' '),
                "line {i} touches border: {text:?}"
            );
        }
    }

    #[test]
    fn ensemble_box_rows_equal_width_with_wide_characters() {
        let wide = "漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字";
        let ensemble = ensemble_fixture_with_labels(&[wide, wide, wide, wide]);
        let inner = 20;
        let lines = ensemble_box_lines(&ensemble, false, false, inner, &Theme::classic(), 0);
        let expected_width = lines[0].width();
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(
                line.width(),
                expected_width,
                "line {i} width mismatch: {line}"
            );
        }
    }

    fn node_fixture(name: &str, kind: GraphNodeKind) -> GraphNode {
        GraphNode {
            id: "n1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: name.to_string(),
            kind,
            config: json!({}),
            position: 0,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn node_box_rows_share_one_width_with_a_trailing_blank_column() {
        let node = node_fixture(
            "A very long node name that will not fit in the box at all",
            GraphNodeKind::Router,
        );
        let inner = 20;
        let lines = node_box_lines(&node, false, false, inner, &Theme::classic(), 0);
        assert_eq!(lines.len(), 3);
        let expected_width = lines[0].width();
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(
                line.width(),
                expected_width,
                "line {i} width mismatch: {line}"
            );
        }
        let text = lines[1].to_string();
        let before_border = text.chars().rev().nth(1);
        assert_eq!(
            before_border,
            Some(' '),
            "name row touches border: {text:?}"
        );
    }

    #[test]
    fn node_box_rows_equal_width_with_wide_characters() {
        let node = node_fixture("漢字漢字漢字漢字漢字漢字漢字漢字", GraphNodeKind::Agent);
        let inner = 20;
        let lines = node_box_lines(&node, false, false, inner, &Theme::classic(), 0);
        let expected_width = lines[0].width();
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(
                line.width(),
                expected_width,
                "line {i} width mismatch: {line}"
            );
        }
    }

    #[test]
    fn router_node_renders_distinctly_with_every_route_legible_at_real_width() {
        let mut state = running_state();
        state.effective_nodes = vec![
            GraphNode {
                id: "router".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Classify request".to_string(),
                kind: GraphNodeKind::Router,
                config: json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "billing".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Billing specialist".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 1,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "technical".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Technical specialist".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 2,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "sales".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Sales specialist".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 3,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "escalation".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Human escalation".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 4,
                created_at: Utc::now(),
            },
        ];
        state.effective_edges = vec![
            (
                "router",
                "billing",
                GraphEdgeCondition::Route("billing".to_string()),
            ),
            (
                "router",
                "technical",
                GraphEdgeCondition::Route("technical".to_string()),
            ),
            (
                "router",
                "sales",
                GraphEdgeCondition::Route("sales".to_string()),
            ),
            (
                "router",
                "escalation",
                GraphEdgeCondition::Route("escalation".to_string()),
            ),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (from, to, condition))| GraphEdge {
            id: format!("re{i}"),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: from.to_string(),
            to_node: to.to_string(),
            condition,
        })
        .collect();
        state.router_taken_routes = [("router".to_string(), "technical".to_string())]
            .into_iter()
            .collect();
        state.current_node_id = Some("router".to_string());

        let node_info = NodeRunInfo {
            chosen_route: Some("technical".to_string()),
            ..NodeRunInfo::default()
        };
        // A "real" panel width — wider than the box's own clamp — so a
        // router with 4 routes has plenty of room; nothing here should ever
        // need to wrap or truncate.
        let text = render_to_text(100, 40, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: state.current_node_id.as_deref(),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });

        // Router carries its own kind tag alongside the agent boxes it
        // routes to.
        assert!(text.contains("[router]"), "{text}");
        assert!(text.contains("[agent]"), "{text}");
        // Every one of the 4 declared routes is fully legible: its label,
        // arrow, and target name all appear intact — none truncated with
        // "…" or split by an unwanted wrap.
        for (route, target) in [
            ("billing", "Billing specialist"),
            ("technical", "Technical specialist"),
            ("sales", "Sales specialist"),
            ("escalation", "Human escalation"),
        ] {
            let expected = format!("{route} → {target}");
            assert!(text.contains(&expected), "missing {expected:?} in:\n{text}");
        }
        // The route the completed run actually took is marked distinctly
        // from the other three.
        assert!(
            text.contains("✓ technical → Technical specialist"),
            "{text}"
        );
        assert!(text.contains("route → technical"), "{text}");
    }

    fn test_db_and_dir() -> (Arc<Database>, tempfile::TempDir) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        (db, data_dir)
    }

    fn seed_running_graph(db: &Database) {
        use crate::domain::graphs::{Graph, GraphNodeRun, GraphSpec};

        let lp = Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "lp1".to_string(),
            name: "team graph".to_string(),
            description: None,
            workdir: "/tmp/test".to_string(),
            status: DomainGraphStatus::Running,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        };
        db.insert_graph(&lp).unwrap();

        db.insert_graph_spec(&GraphSpec {
            id: "s1".to_string(),
            graph_id: Some("lp1".to_string()),
            name: "spec one".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Running,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();

        for node in team_nodes() {
            db.insert_graph_node(&GraphNode {
                spec_id: Some("s1".to_string()),
                ..node
            })
            .unwrap();
        }
        for edge in team_edges() {
            db.insert_graph_edge(&GraphEdge {
                spec_id: Some("s1".to_string()),
                ..edge
            })
            .unwrap();
        }

        db.insert_graph_run(&GraphNodeRun {
            id: "run1".to_string(),
            graph_id: "lp1".to_string(),
            spec_id: "s1".to_string(),
            node_id: "n0".to_string(),
            status: GraphRunStatus::Running,
            input: None,
            output: None,
            started_at: Utc::now(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .unwrap();
    }

    fn many_nodes(count: usize) -> Vec<GraphNode> {
        (0..count)
            .map(|i| GraphNode {
                id: format!("m{i}"),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: format!("Node {i}"),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: i as i64,
                created_at: Utc::now(),
            })
            .collect()
    }

    fn many_edges(count: usize) -> Vec<GraphEdge> {
        (0..count.saturating_sub(1))
            .map(|i| GraphEdge {
                id: format!("me{i}"),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: format!("m{i}"),
                to_node: format!("m{}", i + 1),
                condition: GraphEdgeCondition::Pass,
            })
            .collect()
    }

    #[test]
    fn graph_taller_than_panel_is_clipped_with_indicators() {
        let mut state = running_state();
        state.effective_nodes = many_nodes(12);
        state.effective_edges = many_edges(12);
        state.current_node_id = Some("m0".to_string());

        let node_info = NodeRunInfo::default();

        // At top, only ▼ should show.
        let text_top = render_to_text(80, 15, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: false,
                    follow_anchor: None,
                    highlighted_node_id: Some("m0"),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });
        assert!(
            !text_top.contains('▲'),
            "no ▲ when at top, but got:\n{text_top}"
        );
        assert!(
            text_top.contains('▼'),
            "expected ▼ when content overflows below at top:\n{text_top}"
        );

        // Scrolled mid-way, both indicators.
        let text_mid = render_to_text(80, 15, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: false,
                    follow_anchor: None,
                    highlighted_node_id: Some("m0"),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 10,
                },
            );
        });
        assert!(
            text_mid.contains('▲'),
            "expected ▲ when scrolled down:\n{text_mid}"
        );
        assert!(
            text_mid.contains('▼'),
            "expected ▼ when not at bottom:\n{text_mid}"
        );
    }

    #[test]
    fn graph_that_fits_has_no_indicators() {
        let state = running_state();
        let node_info = NodeRunInfo::default();
        let text = render_to_text(80, 60, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: state.current_node_id.as_deref(),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });
        assert!(!text.contains('▲'), "no ▲ when graph fits:\n{text}");
        assert!(!text.contains('▼'), "no ▼ when graph fits:\n{text}");
    }

    #[test]
    fn scroll_clamped_to_valid_range() {
        let mut state = running_state();
        state.effective_nodes = many_nodes(12);
        state.effective_edges = many_edges(12);
        let node_info = NodeRunInfo::default();
        let backend = TestBackend::new(80, 15);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut clamped = None;
        terminal
            .draw(|frame| {
                let area = frame.area();
                let result = render_graph_live_view(
                    frame,
                    area,
                    &LiveViewContext {
                        state: &state,
                        follow: false,
                        follow_anchor: None,
                        highlighted_node_id: None,
                        node_info: &node_info,
                        blocked: false,
                        now: Utc::now(),
                        theme: &Theme::classic(),
                        time_format: "%H:%M",
                        selected_spec_id: None,
                        spec_scroll: 0,
                        scroll: 1000,
                    },
                );
                clamped = Some((result.clamped_scroll, result.total_lines));
            })
            .unwrap();
        let (clamped_scroll, total_lines) = clamped.unwrap();
        // With the strip removed, the full viewport height is available.
        let max = total_lines.saturating_sub(15);
        assert_eq!(
            clamped_scroll, max,
            "scroll must clamp to total_lines - height ({max}), got {clamped_scroll} with total {total_lines}"
        );
    }

    #[test]
    fn auto_follow_keeps_highlighted_node_visible() {
        let mut state = running_state();
        state.effective_nodes = many_nodes(15);
        state.effective_edges = many_edges(15);
        state.current_node_id = Some("m14".to_string());
        let node_info = NodeRunInfo {
            status: Some(GraphRunStatus::Running),
            ..NodeRunInfo::default()
        };
        // Render with scroll=0 but follow=true — the highlighted last node
        // must be auto-scrolled into the 15-row viewport.
        let text = render_to_text(80, 15, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: Some("m14"),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });
        assert!(
            text.contains("Node 14"),
            "auto-follow must keep highlighted node visible, missing Node 14 in:\n{text}"
        );
    }
    #[test]
    fn dfs_layout_renders_children_immediately_after_edges() {
        // A pass-> B pass-> C : DFS order should be A,B,C with B box immediately after A's edges
        let mut state = running_state();
        state.effective_nodes = vec![
            GraphNode {
                id: "a".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "A".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "b".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "B".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 1,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "c".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "C".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 2,
                created_at: Utc::now(),
            },
        ];
        state.effective_edges = vec![
            GraphEdge {
                id: "e0".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "a".to_string(),
                to_node: "b".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
            GraphEdge {
                id: "e1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "b".to_string(),
                to_node: "c".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
        ];
        state.ensembles = Vec::new();
        state.current_node_id = Some("a".to_string());
        let node_info = NodeRunInfo::default();
        let text = render_to_text(80, 40, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: state.current_node_id.as_deref(),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });
        // Ensure B appears after A and C after B
        // For robustness, just check order of node names in rendered text
        let ai = text.find("A").unwrap();
        let bi = text.find("B").unwrap();
        let ci = text.find("C").unwrap();
        assert!(
            ai < bi,
            "A should appear before B in DFS layout, got ai={ai} bi={bi} text:\n{text}"
        );
        assert!(
            bi < ci,
            "B should appear before C in DFS layout, got bi={bi} ci={ci} text:\n{text}"
        );
        // Also ensure B's box appears directly after A's edge lines (no other box between)
        let pass_a = text.find("pass → B").unwrap();
        assert!(
            pass_a < bi,
            "B box should appear after its incoming edge, pass_a={pass_a} bi={bi}"
        );
    }

    #[test]
    fn dfs_layout_shows_back_reference_for_cycles() {
        let mut state = running_state();
        state.effective_nodes = vec![
            GraphNode {
                id: "a".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "A".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "b".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "B".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 1,
                created_at: Utc::now(),
            },
        ];
        state.effective_edges = vec![
            GraphEdge {
                id: "e0".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "a".to_string(),
                to_node: "b".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
            GraphEdge {
                id: "e1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "b".to_string(),
                to_node: "a".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
        ];
        state.ensembles = Vec::new();
        state.current_node_id = Some("a".to_string());
        let node_info = NodeRunInfo::default();
        let text = render_to_text(80, 40, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: state.current_node_id.as_deref(),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });
        assert!(
            text.contains("↩"),
            "cycle back-edge should render with ↩ marker, got:\n{text}"
        );
        // A's box should appear only once (the back-reference is a line, not a box)
        let box_count = text.matches("┌").count();
        assert_eq!(box_count, 2, "expected 2 boxes (A and B), back-reference should not add a third, got {box_count} in:\n{text}");
    }

    #[test]
    fn dfs_layout_pass_before_fail() {
        let mut state = running_state();
        state.effective_nodes = vec![
            GraphNode {
                id: "a".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "A".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "b".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "B".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 1,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "c".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "C".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 2,
                created_at: Utc::now(),
            },
        ];
        state.effective_edges = vec![
            GraphEdge {
                id: "e0".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "a".to_string(),
                to_node: "c".to_string(),
                condition: GraphEdgeCondition::Fail,
            },
            GraphEdge {
                id: "e1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "a".to_string(),
                to_node: "b".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
        ];
        state.ensembles = Vec::new();
        state.current_node_id = Some("a".to_string());
        let node_info = NodeRunInfo::default();
        let text = render_to_text(80, 40, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: state.current_node_id.as_deref(),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });
        let pass_idx = text.find("pass → B").expect("pass edge missing");
        let fail_idx = text.find("fail → C").expect("fail edge missing");
        assert!(
            pass_idx < fail_idx,
            "pass should appear before fail, pass={pass_idx} fail={fail_idx} text:\n{text}"
        );
    }

    #[test]
    fn graph_colors_come_from_theme() {
        // Drive the real render path (`graph_lines`), not a helper, so this
        // actually guards the requirement that the drawing routes colors
        // through `Theme`.
        let theme = Theme {
            status_ok: Color::Rgb(1, 2, 3),
            kind_router: Color::Rgb(4, 5, 6),
            ..Theme::classic()
        };
        let mut state = running_state();
        state.effective_nodes = vec![
            GraphNode {
                id: "r".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Router".to_string(),
                kind: GraphNodeKind::Router,
                config: json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "t".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Target".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 1,
                created_at: Utc::now(),
            },
        ];
        state.effective_edges = vec![GraphEdge {
            id: "e0".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "r".to_string(),
            to_node: "t".to_string(),
            condition: GraphEdgeCondition::Route("myroute".to_string()),
        }];
        state.ensembles = Vec::new();
        state.router_taken_routes = [("r".to_string(), "myroute".to_string())]
            .into_iter()
            .collect();

        let taken = graph_lines(&state, None, true, 80, &theme);
        let route_line = taken
            .lines
            .iter()
            .find(|l| l.to_string().contains("myroute → Target"))
            .expect("route edge line missing");
        assert_eq!(
            route_line.spans[0].style.fg,
            Some(Color::Rgb(1, 2, 3)),
            "taken route edge should use theme.status_ok"
        );
        assert!(
            taken.lines.iter().any(|line| line
                .spans
                .iter()
                .any(|s| s.style.fg == Some(Color::Rgb(4, 5, 6)))),
            "router box should use theme.kind_router"
        );

        // With no route recorded as taken, the branch falls back to dim_text.
        state.router_taken_routes = HashMap::new();
        let untaken = graph_lines(&state, None, true, 80, &theme);
        let route_line = untaken
            .lines
            .iter()
            .find(|l| l.to_string().contains("myroute → Target"))
            .expect("route edge line missing");
        assert_eq!(
            route_line.spans[0].style.fg,
            Some(theme.dim_text),
            "non-taken route edge should use theme.dim_text"
        );
    }

    #[test]
    fn back_reference_uses_theme_dim_text() {
        let mut state = running_state();
        state.effective_nodes = vec![
            GraphNode {
                id: "a".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "A".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "b".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "B".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 1,
                created_at: Utc::now(),
            },
        ];
        state.effective_edges = vec![
            GraphEdge {
                id: "e0".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "a".to_string(),
                to_node: "b".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
            GraphEdge {
                id: "e1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "b".to_string(),
                to_node: "a".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
        ];
        state.ensembles = Vec::new();
        let theme = Theme::classic();
        // Verify graph_lines returns lines where the back-reference edge uses dim_text
        let result = graph_lines(&state, Some("a"), true, 80, &theme);
        let back_line = result
            .lines
            .iter()
            .find(|l| l.to_string().contains("↩"))
            .expect("back ref line missing");
        let fg = back_line.spans[0].style.fg;
        assert_eq!(
            fg,
            Some(theme.dim_text),
            "back-reference should use theme.dim_text, got {:?}",
            fg
        );
    }

    // ---- CT8: scroll ownership + mode indicator ---------------------------

    #[test]
    fn ct8_auto_follow_user_scroll_survives_when_current_node_unchanged() {
        // The measured bug: reading a big graph of a running graph, scrolling
        // down, and being yanked back to the running node every frame.
        let mut state = running_state();
        state.effective_nodes = many_nodes(30);
        state.effective_edges = many_edges(30);
        state.current_node_id = Some("m1".to_string());
        let node_info = NodeRunInfo::default();

        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        // Frame 1: anchor is None -> renderer adopts "m1" as the anchor.
        let mut anchor: Option<String> = None;
        terminal
            .draw(|frame| {
                let area = frame.area();
                let r = render_graph_live_view(
                    frame,
                    area,
                    &LiveViewContext {
                        state: &state,
                        follow: true,
                        follow_anchor: None,
                        highlighted_node_id: Some("m1"),
                        node_info: &node_info,
                        blocked: false,
                        now: Utc::now(),
                        theme: &Theme::classic(),
                        time_format: "%H:%M",
                        selected_spec_id: None,
                        spec_scroll: 0,
                        scroll: 0,
                    },
                );
                anchor = r.follow_anchor;
            })
            .unwrap();
        assert_eq!(
            anchor.as_deref(),
            Some("m1"),
            "renderer must adopt the current node as the anchor"
        );

        // Frame 2: user has scrolled to 10, current node is still "m1".
        let mut clamped = None;
        terminal
            .draw(|frame| {
                let area = frame.area();
                let r = render_graph_live_view(
                    frame,
                    area,
                    &LiveViewContext {
                        state: &state,
                        follow: true,
                        follow_anchor: anchor.as_deref(),
                        highlighted_node_id: Some("m1"),
                        node_info: &node_info,
                        blocked: false,
                        now: Utc::now(),
                        theme: &Theme::classic(),
                        time_format: "%H:%M",
                        selected_spec_id: None,
                        spec_scroll: 0,
                        scroll: 10,
                    },
                );
                clamped = Some(r.clamped_scroll);
            })
            .unwrap();
        assert_eq!(
            clamped,
            Some(10),
            "auto-follow must NOT override the user's scroll when the current node is unchanged"
        );
    }
    // Breaks if: the `if node_changed` guard is removed and auto-follow
    // re-centres every frame again.

    #[test]
    fn ct8_auto_follow_recentres_when_current_node_changes() {
        let mut state = running_state();
        state.effective_nodes = many_nodes(30);
        state.effective_edges = many_edges(30);
        state.current_node_id = Some("m25".to_string());
        let node_info = NodeRunInfo::default();

        // User had scrolled far away (anchor still on "m0"); the engine's
        // current node has moved to "m25". The view must jump to it.
        let text = render_to_text(80, 20, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: Some("m0"),
                    highlighted_node_id: Some("m25"),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });
        assert!(
            text.contains("Node 25"),
            "a real node transition must re-centre the view on the new current node:\n{text}"
        );
    }
    // Breaks if: auto-follow stops re-centring on node change (e.g. the
    // node-change branch always returns `clamped_from_input`).

    #[test]
    fn ct27_auto_follow_centres_a_tall_ensemble_box_in_the_panel() {
        // The 2026-10-08 regression, replayed: on the transition
        // "Lock repo + clear scratch" -> "Designer [8 models]" the panel
        // scrolled just far enough to park the box's first three lines on the
        // last visible rows, leaving the remaining six members below the
        // bottom edge. Auto-follow must bring the whole box into view.
        let mut state = running_state();
        state.effective_nodes = vec![
            GraphNode {
                id: "kickoff".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Lock repo + clear scratch".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "join1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Designer (quorum)".to_string(),
                kind: GraphNodeKind::Join,
                config: json!({}),
                position: 9,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "arbiter".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Arbiter".to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: 10,
                created_at: Utc::now(),
            },
        ]
        .into_iter()
        .chain((0..8).map(|i| GraphNode {
            id: format!("m{i}"),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: format!("Designer [{i}]"),
            kind: GraphNodeKind::Agent,
            config: json!({}),
            position: i + 1,
            created_at: Utc::now(),
        }))
        .collect();
        let mut edges: Vec<GraphEdge> = (0..8)
            .flat_map(|i| {
                [
                    (
                        "kickoff".to_string(),
                        format!("m{i}"),
                        GraphEdgeCondition::Always,
                    ),
                    (
                        format!("m{i}"),
                        "join1".to_string(),
                        GraphEdgeCondition::Always,
                    ),
                ]
            })
            .enumerate()
            .map(|(i, (from, to, condition))| GraphEdge {
                id: format!("ee{i}"),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: from,
                to_node: to,
                condition,
            })
            .collect();
        edges.push(GraphEdge {
            id: "ee16".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "join1".to_string(),
            to_node: "arbiter".to_string(),
            condition: GraphEdgeCondition::Pass,
        });
        state.effective_edges = edges;
        state.ensembles = vec![ensemble_fixture_with_labels(&[
            "openrouter/deepseek",
            "openrouter/qwen",
            "openrouter/llama",
            "openrouter/mistral",
            "openrouter/gemma",
            "openrouter/phi",
            "openrouter/yi",
            "openrouter/grok",
        ])];
        state.current_node_id = Some("m0".to_string());

        let text = render_to_text(80, 20, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    // The engine just moved off the previous node, so this
                    // frame is a node change and must re-centre.
                    follow_anchor: Some("kickoff"),
                    highlighted_node_id: Some("m0"),
                    node_info: &NodeRunInfo::default(),
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });

        // The collapsed box is titled with its member count...
        assert!(
            text.contains("Implementer [8 models]"),
            "ensemble box title missing in:\n{text}"
        );
        // ...and EVERY member row is on screen, not only the first three.
        for label in [
            "openrouter/deepseek",
            "openrouter/qwen",
            "openrouter/llama",
            "openrouter/mistral",
            "openrouter/gemma",
            "openrouter/phi",
            "openrouter/yi",
            "openrouter/grok",
        ] {
            assert!(
                text.contains(label),
                "member {label} of the followed ensemble is below the bottom edge:\n{text}"
            );
        }
        // The box is complete on screen: its closing border renders directly
        // under the last member row, and the node that follows it is visible
        // below that border.
        let rows: Vec<&str> = text.lines().collect();
        let last_member_row = rows
            .iter()
            .position(|row| row.contains("openrouter/grok"))
            .expect("last member row must be on screen");
        assert!(
            rows[last_member_row + 1].contains('└'),
            "ensemble box bottom border must sit under the last member row:\n{text}"
        );
        assert!(
            text.contains("Arbiter"),
            "the node after the followed ensemble must be visible below it:\n{text}"
        );
    }
    // Breaks if: auto-follow returns to the span-3 `ensure_visible` (only the
    // title and the first two member rows would be on the last visible
    // lines), or if `highlighted_span` stops measuring the ensemble box.

    #[test]
    fn ct8_auto_follow_ignores_status_or_elapsed_change_for_same_node() {
        let mut state = running_state();
        state.effective_nodes = many_nodes(30);
        state.effective_edges = many_edges(30);
        state.current_node_id = Some("m1".to_string());

        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        let running = NodeRunInfo {
            status: Some(GraphRunStatus::Running),
            started_at: Some(Utc::now() - chrono::Duration::seconds(5)),
            ..NodeRunInfo::default()
        };
        // Frame 1 adopts the anchor "m1".
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_graph_live_view(
                    frame,
                    area,
                    &LiveViewContext {
                        state: &state,
                        follow: true,
                        follow_anchor: None,
                        highlighted_node_id: Some("m1"),
                        node_info: &running,
                        blocked: false,
                        now: Utc::now(),
                        theme: &Theme::classic(),
                        time_format: "%H:%M",
                        selected_spec_id: None,
                        spec_scroll: 0,
                        scroll: 0,
                    },
                );
            })
            .unwrap();

        // Frame 2: same node "m1", user scrolled to 12, but the node's status
        // flipped to Pass and elapsed time advanced. Scroll must not move.
        let passed = NodeRunInfo {
            status: Some(GraphRunStatus::Pass),
            started_at: Some(Utc::now() - chrono::Duration::seconds(600)),
            ..NodeRunInfo::default()
        };
        let mut clamped = None;
        terminal
            .draw(|frame| {
                let area = frame.area();
                let r = render_graph_live_view(
                    frame,
                    area,
                    &LiveViewContext {
                        state: &state,
                        follow: true,
                        follow_anchor: Some("m1"),
                        highlighted_node_id: Some("m1"),
                        node_info: &passed,
                        blocked: false,
                        now: Utc::now(),
                        theme: &Theme::classic(),
                        time_format: "%H:%M",
                        selected_spec_id: None,
                        spec_scroll: 0,
                        scroll: 12,
                    },
                );
                clamped = Some(r.clamped_scroll);
            })
            .unwrap();
        assert_eq!(
            clamped,
            Some(12),
            "a status/elapsed change for the same current node must not move the scroll"
        );
    }
    // Breaks if: re-centring keys off node status/elapsed instead of node id.

    #[test]
    fn ct8_manual_navigation_keeps_selection_below_viewport_on_screen() {
        let mut state = running_state();
        state.effective_nodes = many_nodes(30);
        state.effective_edges = many_edges(30);
        state.current_node_id = Some("m0".to_string());
        let node_info = NodeRunInfo::default();

        // Manual mode, selection near the bottom, user scroll still at the top.
        let text = render_to_text(80, 20, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: false,
                    follow_anchor: None,
                    highlighted_node_id: Some("m26"),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });
        assert!(
            text.contains("Node 26"),
            "manual navigation must scroll a below-viewport selection into view:\n{text}"
        );
    }
    // Breaks if: the manual branch returns `clamped_from_input` without
    // calling `ensure_visible`.

    #[test]
    fn ct8_manual_navigation_keeps_selection_above_viewport_on_screen() {
        let mut state = running_state();
        state.effective_nodes = many_nodes(30);
        state.effective_edges = many_edges(30);
        state.current_node_id = Some("m0".to_string());
        let node_info = NodeRunInfo::default();

        // Manual mode, selection back at the top, but user scroll left far down.
        let text = render_to_text(80, 20, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: false,
                    follow_anchor: None,
                    highlighted_node_id: Some("m0"),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 250,
                },
            );
        });
        assert!(
            text.contains("Node 0"),
            "manual navigation must scroll an above-viewport selection back into view:\n{text}"
        );
    }
    // Breaks if: the manual branch stops calling `ensure_visible`.

    #[test]
    fn strip_absent_after_ct11() {
        let state = running_state();
        let node_info = NodeRunInfo::default();

        let follow_text = render_to_text(80, 30, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: true,
                    follow_anchor: None,
                    highlighted_node_id: state.current_node_id.as_deref(),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });
        assert!(
            !follow_text.contains("AUTO-FOLLOW") && !follow_text.contains("MANUAL"),
            "strip must be absent from the live view in auto-follow mode:\n{follow_text}"
        );

        let manual_text = render_to_text(80, 30, |frame, area| {
            render_graph_live_view(
                frame,
                area,
                &LiveViewContext {
                    state: &state,
                    follow: false,
                    follow_anchor: None,
                    highlighted_node_id: Some("n2"),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &Theme::classic(),
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        });
        assert!(
            !manual_text.contains("AUTO-FOLLOW") && !manual_text.contains("MANUAL — press Esc"),
            "strip must be absent from the live view in manual mode:\n{manual_text}"
        );
    }
    // Breaks if: the strip is re-added to the live view instead of
    // the border title.
}

#[cfg(test)]
mod tail_pane_split_tests {
    use super::tail_pane_split;
    use ratatui::layout::Rect;

    #[test]
    fn split_horizontal_at_160x40() {
        let area = Rect::new(0, 0, 160, 40);
        let (graph, pane) = tail_pane_split(area);
        assert_eq!(graph.width, 80);
        assert_eq!(pane.width, 80);
        assert_eq!(graph.height, 40);
        assert_eq!(pane.height, 40);
        assert_eq!(graph.x, 0);
        assert_eq!(pane.x, 80);
    }

    #[test]
    fn split_vertical_at_100x40() {
        let area = Rect::new(0, 0, 100, 40);
        let (graph, pane) = tail_pane_split(area);
        assert_eq!(graph.height, 16); // 40 * 40% = 16
        assert_eq!(graph.y, 0);
        assert_eq!(pane.y, 16);
        assert_eq!(graph.width, 100);
        assert_eq!(pane.width, 100);
    }

    #[test]
    fn split_vertical_minimum_8_rows() {
        let area = Rect::new(0, 0, 80, 10);
        let (graph, _) = tail_pane_split(area);
        // 10 * 40% = 4, clamped to min 8
        assert_eq!(graph.height, 8);
    }
}
