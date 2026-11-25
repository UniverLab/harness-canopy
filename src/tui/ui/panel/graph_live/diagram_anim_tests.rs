use super::*;
use crate::domain::graphs::{
    GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphRunStatus, GraphStatus,
};
use crate::tui::app::graph_live_state::{EnsembleLiveInfo, EnsembleMemberLiveInfo};
use crate::tui::ui::theme::Theme;
use chrono::Utc;
use serde_json::json;
use std::collections::HashMap;

fn blank_state() -> GraphLiveState {
    GraphLiveState {
        graph_id: String::new(),
        graph_name: String::new(),
        graph_status: GraphStatus::Running,
        workdir: String::new(),
        trigger_type: "manual".to_string(),
        schedule_expr: None,
        watch_path: None,
        autorun_at: None,
        pause_waiting_for: Vec::new(),
        spec_queue: Vec::new(),
        done_count: 0,
        total_count: 0,
        current_spec_id: None,
        effective_nodes: Vec::new(),
        effective_edges: Vec::new(),
        ensembles: Vec::new(),
        router_taken_routes: HashMap::new(),
        current_node_id: None,
        current_node_status: None,
        current_node_started_at: None,
        current_node_iteration: None,
    }
}

fn node(id: &str, position: i64) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Agent,
        config: json!({}),
        position,
        created_at: Utc::now(),
    }
}

fn edge(id: &str, from: &str, to: &str, condition: GraphEdgeCondition) -> GraphEdge {
    GraphEdge {
        id: id.to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition,
    }
}

fn diamond_state() -> GraphLiveState {
    let mut state = blank_state();
    state.effective_nodes = vec![node("a", 0), node("b", 1), node("c", 2), node("d", 3)];
    state.effective_edges = vec![
        edge("e1", "a", "b", GraphEdgeCondition::Pass),
        edge("e2", "a", "c", GraphEdgeCondition::Fail),
        edge("e3", "b", "d", GraphEdgeCondition::Pass),
        edge("e4", "c", "d", GraphEdgeCondition::Pass),
    ];
    state
}

fn line_strings(result: &GraphLinesResult<'_>) -> Vec<String> {
    result
        .lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

fn find_char_cells(
    result: &GraphLinesResult<'_>,
    ch: char,
) -> Vec<(usize, usize, ratatui::style::Color)> {
    let mut out = Vec::new();
    for (y, line) in result.lines.iter().enumerate() {
        let mut x = 0usize;
        for span in &line.spans {
            for c in span.content.chars() {
                if c == ch {
                    out.push((x, y, span.style.fg.unwrap_or(ratatui::style::Color::Reset)));
                }
                x += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            }
        }
    }
    out
}

#[test]
fn diamond_dot_follows_path_and_trail() {
    let state = diamond_state();
    let theme = Theme::classic();
    // Find active path into D from B by inspecting tick 0: path starts at first vertical cell below B
    let base = diagram_lines(
        &state,
        Some("d"),
        true,
        80,
        &theme,
        &DiagramAnim {
            running_node_id: Some("d"),
            tick: Some(0),
        },
    )
    .unwrap();
    let dots0 = find_char_cells(&base, '●');
    assert!(!dots0.is_empty(), "expected dot at tick 0");
    // There are two incoming paths to D; pick the path whose dot matches formula for each tick.
    // Build a known path by collecting all cells that become '●' across ticks 0..n*3.
    // For determinism, observe path from tick progression on the edge entering D from left (B->D).
    // Reconstruct the B->D path by observing dot positions at t=0,3,6,...
    let mut ordered: Vec<(usize, usize)> = Vec::new();
    for step in 0..60u32 {
        let t = step * 3;
        let res = diagram_lines(
            &state,
            Some("d"),
            true,
            80,
            &theme,
            &DiagramAnim {
                running_node_id: Some("d"),
                tick: Some(t),
            },
        )
        .unwrap();
        let dots = find_char_cells(&res, '●');
        let dot = dots
            .iter()
            .min_by_key(|(x, y, _)| (*y, *x))
            .copied()
            .unwrap();
        let cell = (dot.0, dot.1);
        if !ordered.is_empty() && cell == ordered[0] {
            break;
        }
        ordered.push(cell);
    }
    let path = ordered;
    let n = path.len();
    assert!(n >= 7, "expected path length >= 7, got {n}");
    // Verify dot at ticks 0..21 sits at (t/3)%n
    for t in 0..22u32 {
        let res = diagram_lines(
            &state,
            Some("d"),
            true,
            80,
            &theme,
            &DiagramAnim {
                running_node_id: Some("d"),
                tick: Some(t),
            },
        )
        .unwrap();
        let dots = find_char_cells(&res, '●');
        let dot = dots
            .iter()
            .min_by_key(|(x, y, _)| (*y, *x))
            .copied()
            .unwrap();
        let expected = path[(t / 3) as usize % n];
        assert_eq!((dot.0, dot.1), expected, "tick {t}");
        // trail: i-1, i-2, no wrap
        let trails = find_char_cells(&res, '·');
        let idx = (t / 3) as usize % n;
        let mut expected_trails: Vec<(usize, usize)> = Vec::new();
        if idx >= 1 {
            expected_trails.push(path[idx - 1]);
        }
        if idx >= 2 {
            expected_trails.push(path[idx - 2]);
        }
        for et in &expected_trails {
            assert!(
                trails.iter().any(|(x, y, _)| (*x, *y) == *et),
                "tick {t} missing trail {et:?}"
            );
        }
        // no wrap: at idx 0 no trail, at idx 1 only idx 0
        if idx == 0 {
            // no trail from this path; other path may have trails, so just check our cells
            assert!(!trails.iter().any(|(x, y, _)| (*x, *y) == path[n - 1]));
        }
    }
}

#[test]
fn pulse_border_alternates_every_8_ticks() {
    let state = diamond_state();
    let theme = Theme::classic();
    for t in 0..32u32 {
        let res = diagram_lines(
            &state,
            Some("d"),
            true,
            80,
            &theme,
            &DiagramAnim {
                running_node_id: Some("d"),
                tick: Some(t),
            },
        )
        .unwrap();
        let rows = line_strings(&res);
        let title_idx = rows
            .iter()
            .position(|r| r.contains("│● d") || r.contains("│ d"))
            .unwrap();
        let top_idx = title_idx - 1;
        let bottom_idx = rows[top_idx..]
            .iter()
            .position(|r| r.contains('└'))
            .unwrap()
            + top_idx;
        let top_fg = res.lines[top_idx]
            .spans
            .iter()
            .find(|s| s.content.contains('┌'))
            .and_then(|s| s.style.fg)
            .unwrap();
        let bottom_fg = res.lines[bottom_idx]
            .spans
            .iter()
            .find(|s| s.content.contains('└'))
            .and_then(|s| s.style.fg)
            .unwrap();
        let expected = if (t / 8) % 2 == 0 {
            theme.status_running
        } else {
            super::compose::bright_accent(theme.status_running)
        };
        assert_eq!(top_fg, expected, "tick {t} top");
        assert_eq!(bottom_fg, expected, "tick {t} bottom");
    }
}

#[test]
fn tick_none_gives_solid_accent_no_dot_and_bright_border() {
    let state = diamond_state();
    let theme = Theme::classic();
    let res = diagram_lines(
        &state,
        Some("d"),
        true,
        80,
        &theme,
        &DiagramAnim {
            running_node_id: Some("d"),
            tick: None,
        },
    )
    .unwrap();
    let anim_dots: Vec<_> = find_char_cells(&res, '●')
        .into_iter()
        .filter(|(x, y, c)| {
            *c == theme.status_running
                && !line_strings(&res)[*y]
                    .chars()
                    .collect::<Vec<_>>()
                    .get(*x + 1)
                    .is_some_and(|ch| *ch == ' ')
        })
        .collect();
    assert!(
        anim_dots.is_empty(),
        "dots: {:?}, rows: {:?}",
        anim_dots,
        line_strings(&res)
    );
    assert!(!res.anim_drawn);
    let rows = line_strings(&res);
    let title_idx = rows
        .iter()
        .position(|r| r.contains("│● d") || r.contains("│ d"))
        .unwrap();
    let top_idx = title_idx - 1;
    let bottom_idx = rows[top_idx..]
        .iter()
        .position(|r| r.contains('└'))
        .unwrap()
        + top_idx;
    let expected = super::compose::bright_accent(theme.status_running);
    assert_eq!(
        res.lines[top_idx]
            .spans
            .iter()
            .find(|s| s.content.contains('┌'))
            .and_then(|s| s.style.fg)
            .unwrap(),
        expected
    );
    assert_eq!(
        res.lines[bottom_idx]
            .spans
            .iter()
            .find(|s| s.content.contains('└'))
            .and_then(|s| s.style.fg)
            .unwrap(),
        expected
    );
    // active path cells should be accent colored
    let has_accent_connector = res.lines.iter().any(|l| {
        l.spans.iter().any(|s| {
            s.style.fg == Some(theme.status_running)
                && s.content.chars().any(|c| "│─┌┐└┘├┤┬┴┼".contains(c))
        })
    });
    assert!(has_accent_connector);
}

#[test]
fn no_running_node_gives_no_dot_no_accent() {
    let state = diamond_state();
    let theme = Theme::classic();
    let res = diagram_lines(
        &state,
        Some("d"),
        true,
        80,
        &theme,
        &DiagramAnim {
            running_node_id: None,
            tick: Some(5),
        },
    )
    .unwrap();
    // Animation dots are on connectors; box follow markers are in titles.
    let anim_dots: Vec<_> = find_char_cells(&res, '●')
        .into_iter()
        .filter(|(x, y, c)| {
            *c == theme.status_running
                && !line_strings(&res)[*y]
                    .chars()
                    .collect::<Vec<_>>()
                    .get(*x + 1)
                    .is_some_and(|ch| *ch == ' ')
        })
        .collect();
    assert!(!res.anim_drawn, "anim_drawn true, dots: {anim_dots:?}");
    assert!(anim_dots.is_empty(), "dots: {anim_dots:?}");
}

#[test]
fn ensemble_member_maps_to_ensemble_box() {
    let mut state = blank_state();
    state.effective_nodes = vec![
        node("start", 0),
        node("m0", 1),
        node("m1", 2),
        node("join", 3),
        node("end", 4),
    ];
    state.ensembles = vec![EnsembleLiveInfo {
        ensemble_id: "ens".to_string(),
        name: "Rev".to_string(),
        join_node_id: "join".to_string(),
        members: vec![
            EnsembleMemberLiveInfo {
                node_id: "m0".to_string(),
                label: "r1".to_string(),
                status: Some(GraphRunStatus::Running),
            },
            EnsembleMemberLiveInfo {
                node_id: "m1".to_string(),
                label: "r2".to_string(),
                status: Some(GraphRunStatus::Pass),
            },
        ],
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
    }];
    state.effective_edges = vec![
        edge("e1", "start", "m0", GraphEdgeCondition::Pass),
        edge("e2", "start", "m1", GraphEdgeCondition::Pass),
        edge("e3", "m0", "join", GraphEdgeCondition::Pass),
        edge("e4", "m1", "join", GraphEdgeCondition::Pass),
        edge("e5", "join", "end", GraphEdgeCondition::Pass),
    ];
    let theme = Theme::classic();
    let res = diagram_lines(
        &state,
        Some("m0"),
        true,
        80,
        &theme,
        &DiagramAnim {
            running_node_id: Some("m0"),
            tick: Some(0),
        },
    )
    .unwrap();
    assert!(res.anim_drawn);
    let rows = line_strings(&res);
    // ensemble box should be pulsed/highlighted - find its top border
    let ens_title = rows
        .iter()
        .position(|r| r.contains("Rev [2 models]"))
        .unwrap();
    let top_idx = ens_title - 1;
    let expected = theme.status_running; // tick 0, (0/8)%2==0
    let top_fg = res.lines[top_idx]
        .spans
        .iter()
        .find(|s| s.content.contains('┌'))
        .and_then(|s| s.style.fg);
    assert_eq!(
        top_fg,
        Some(expected),
        "top border fg wrong, rows: {:?}",
        rows
    );
    assert!(!find_char_cells(&res, '●').is_empty(), "rows: {:?}", rows);
}
