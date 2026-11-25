//! Two-dimensional ranked diagram layout for the live graph view.
//!
//! Lays out collapsed nodes (and ensembles) in horizontal rank bands joined
//! by orthogonal box-drawing connectors. Returns `None` when the graph or
//! panel falls outside the diagram's bounds so the caller can fall back to the
//! vertical list in `lines.rs`.

use std::collections::{HashMap, HashSet, VecDeque};

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::lines::{
    collapse_edges, collapse_nodes, ensemble_box_lines, ensemble_index, find_entry_key, fit_row,
    is_collapsed_highlighted, node_box_lines, Collapsed, GraphLinesResult,
};
use crate::domain::graphs::GraphEdgeCondition;
use crate::tui::app::graph_live_state::GraphLiveState;
use crate::tui::ui::theme::Theme;

const BOX_WIDTH: usize = 24;
const BOX_INNER: usize = 22;
const COL_GAP: usize = 2;
const CONNECTOR_ROWS: usize = 2;
const MAX_DIAGRAM_BOXES: usize = 40;
const MIN_DIAGRAM_WIDTH: u16 = 30;

const DIR_UP: u8 = 1 << 0;
const DIR_DOWN: u8 = 1 << 1;
const DIR_LEFT: u8 = 1 << 2;
const DIR_RIGHT: u8 = 1 << 3;

struct DfsAnalysis {
    non_back_edges: HashMap<String, Vec<String>>,
    back_edges_by_target: HashMap<String, Vec<String>>,
    dfs_order: HashMap<String, usize>,
}

struct PlacedBox {
    key: String,
    x: usize,
    y: usize,
    lines: Vec<Line<'static>>,
}

struct RankLayout {
    placed: Vec<PlacedBox>,
    total_height: usize,
    highlighted_offset: Option<u16>,
    highlighted_span: Option<u16>,
}

pub(super) fn diagram_lines(
    state: &GraphLiveState,
    highlighted_node_id: Option<&str>,
    follow: bool,
    area_width: u16,
    theme: &Theme,
) -> Option<GraphLinesResult<'static>> {
    if area_width < MIN_DIAGRAM_WIDTH {
        return None;
    }

    let index = ensemble_index(state);
    let (collapsed_nodes, collapsed_pos) = collapse_nodes(state, &index);
    if collapsed_nodes.is_empty() || collapsed_nodes.len() > MAX_DIAGRAM_BOXES {
        return None;
    }

    let collapsed_edges = collapse_edges(state, &index, &collapsed_nodes);
    let entry_key = find_entry_key(&collapsed_nodes, &collapsed_edges, &collapsed_pos);
    let analysis = analyze_dfs(
        &entry_key,
        &collapsed_nodes,
        &collapsed_edges,
        &collapsed_pos,
    );
    let ranks = compute_ranks(&collapsed_nodes, &analysis)?;

    let layout = layout_ranks(
        &ranks,
        &collapsed_nodes,
        &analysis.back_edges_by_target,
        highlighted_node_id,
        follow,
        area_width as usize,
        theme,
    )?;

    let grid = draw_connectors(
        &layout.placed,
        &analysis.non_back_edges,
        area_width as usize,
        layout.total_height,
    );
    let lines = compose_canvas(layout.placed, &grid, layout.total_height, theme);

    Some(GraphLinesResult {
        lines,
        highlighted_offset: layout.highlighted_offset,
        highlighted_span: layout.highlighted_span,
    })
}

fn analyze_dfs(
    entry_key: &str,
    nodes: &HashMap<String, Collapsed<'_>>,
    edges: &HashMap<String, Vec<(String, GraphEdgeCondition)>>,
    positions: &HashMap<String, i64>,
) -> DfsAnalysis {
    let mut roots: Vec<String> = Vec::with_capacity(nodes.len());
    roots.push(entry_key.to_string());
    let mut rest: Vec<String> = nodes
        .keys()
        .filter(|k| k.as_str() != entry_key)
        .cloned()
        .collect();
    rest.sort_by(|a, b| {
        let pa = positions.get(a.as_str()).copied().unwrap_or(i64::MAX);
        let pb = positions.get(b.as_str()).copied().unwrap_or(i64::MAX);
        pa.cmp(&pb).then_with(|| a.cmp(b))
    });
    roots.extend(rest);

    let mut visited = HashSet::new();
    let mut on_stack = HashSet::new();
    let mut analysis = DfsAnalysis {
        non_back_edges: HashMap::new(),
        back_edges_by_target: HashMap::new(),
        dfs_order: HashMap::new(),
    };

    for root in &roots {
        if !visited.contains(root.as_str()) {
            dfs_walk(root, edges, &mut visited, &mut on_stack, &mut analysis);
        }
    }
    analysis
}

fn dfs_walk(
    key: &str,
    edges: &HashMap<String, Vec<(String, GraphEdgeCondition)>>,
    visited: &mut HashSet<String>,
    on_stack: &mut HashSet<String>,
    analysis: &mut DfsAnalysis,
) {
    visited.insert(key.to_string());
    on_stack.insert(key.to_string());
    let next_idx = analysis.dfs_order.len();
    analysis.dfs_order.insert(key.to_string(), next_idx);

    if let Some(adj) = edges.get(key) {
        for (target, _) in adj {
            if on_stack.contains(target.as_str()) {
                let back_list = analysis
                    .back_edges_by_target
                    .entry(target.clone())
                    .or_default();
                if !back_list.iter().any(|s| s == key) {
                    back_list.push(key.to_string());
                }
            } else {
                let fwd_list = analysis.non_back_edges.entry(key.to_string()).or_default();
                if !fwd_list.iter().any(|t| t == target) {
                    fwd_list.push(target.clone());
                }
                if !visited.contains(target.as_str()) {
                    dfs_walk(target, edges, visited, on_stack, analysis);
                }
            }
        }
    }

    on_stack.remove(key);
}

fn compute_ranks(
    nodes: &HashMap<String, Collapsed<'_>>,
    analysis: &DfsAnalysis,
) -> Option<Vec<Vec<String>>> {
    let mut in_degree: HashMap<&str, usize> = nodes.keys().map(|k| (k.as_str(), 0)).collect();
    for targets in analysis.non_back_edges.values() {
        for target in targets {
            if let Some(deg) = in_degree.get_mut(target.as_str()) {
                *deg += 1;
            }
        }
    }

    let mut rank0: Vec<String> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(k, _)| (*k).to_string())
        .collect();
    rank0.sort_by_key(|k| {
        analysis
            .dfs_order
            .get(k.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });

    let mut rank: HashMap<String, usize> = HashMap::with_capacity(nodes.len());
    let mut topo_queue: VecDeque<String> = VecDeque::new();
    for r in &rank0 {
        rank.insert(r.clone(), 0);
        topo_queue.push_back(r.clone());
    }

    while let Some(u) = topo_queue.pop_front() {
        let u_rank = rank[&u];
        let Some(targets) = analysis.non_back_edges.get(&u) else {
            continue;
        };
        for v in targets {
            let entry = rank.entry(v.clone()).or_insert(0);
            *entry = (*entry).max(u_rank + 1);
            let Some(deg) = in_degree.get_mut(v.as_str()) else {
                continue;
            };
            *deg -= 1;
            if *deg == 0 {
                topo_queue.push_back(v.clone());
            }
        }
    }

    for (u, targets) in &analysis.non_back_edges {
        let u_rank = rank.get(u.as_str())?;
        for v in targets {
            let v_rank = rank.get(v.as_str())?;
            if *v_rank != u_rank + 1 {
                return None;
            }
        }
    }

    let max_rank = rank.values().copied().max().unwrap_or(0);
    let mut ranks: Vec<Vec<String>> = vec![Vec::new(); max_rank + 1];
    let mut bfs_visited = HashSet::new();
    let mut bfs_queue: VecDeque<String> = VecDeque::new();
    for r in rank0 {
        if bfs_visited.insert(r.clone()) {
            bfs_queue.push_back(r);
        }
    }
    while let Some(u) = bfs_queue.pop_front() {
        ranks[rank[&u]].push(u.clone());
        if let Some(targets) = analysis.non_back_edges.get(&u) {
            for v in targets {
                if bfs_visited.insert(v.clone()) {
                    bfs_queue.push_back(v.clone());
                }
            }
        }
    }

    Some(ranks)
}

fn layout_ranks(
    ranks: &[Vec<String>],
    nodes: &HashMap<String, Collapsed<'_>>,
    back_edges_by_target: &HashMap<String, Vec<String>>,
    highlighted_node_id: Option<&str>,
    follow: bool,
    area_width: usize,
    theme: &Theme,
) -> Option<RankLayout> {
    let mut placed = Vec::new();
    let mut cur_y = 0usize;
    let mut highlighted_offset = None;
    let mut highlighted_span = None;

    for (rank_idx, rank_keys) in ranks.iter().enumerate() {
        let count = rank_keys.len();
        let group_width = count * BOX_WIDTH + count.saturating_sub(1) * COL_GAP;
        if group_width > area_width {
            return None;
        }
        let left_offset = (area_width - group_width) / 2;
        let mut rank_height = 0usize;

        for (i, key) in rank_keys.iter().enumerate() {
            let entry = nodes.get(key.as_str())?;
            let is_hl = is_collapsed_highlighted(nodes, highlighted_node_id, key);
            let lines = build_box_lines(
                entry,
                nodes,
                back_edges_by_target.get(key.as_str()),
                is_hl,
                follow,
                theme,
            );
            let box_h = lines.len();
            rank_height = rank_height.max(box_h);
            if is_hl && highlighted_offset.is_none() {
                highlighted_offset = Some(cur_y as u16);
                highlighted_span = Some(box_h as u16);
            }
            placed.push(PlacedBox {
                key: key.clone(),
                x: left_offset + i * (BOX_WIDTH + COL_GAP),
                y: cur_y,
                lines,
            });
        }

        cur_y += rank_height;
        if rank_idx + 1 < ranks.len() {
            cur_y += CONNECTOR_ROWS;
        }
    }

    Some(RankLayout {
        placed,
        total_height: cur_y,
        highlighted_offset,
        highlighted_span,
    })
}

fn build_box_lines(
    entry: &Collapsed<'_>,
    nodes: &HashMap<String, Collapsed<'_>>,
    back_sources: Option<&Vec<String>>,
    is_highlighted: bool,
    follow: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = match entry {
        Collapsed::Node(node) => node_box_lines(node, is_highlighted, follow, BOX_INNER, theme, 0),
        Collapsed::Ensemble(ens) => {
            ensemble_box_lines(ens, is_highlighted, follow, BOX_INNER, theme, 0)
        }
    };

    if let Some(sources) = back_sources {
        let mut extra = Vec::with_capacity(sources.len());
        for src_key in sources {
            let src_name = match nodes.get(src_key.as_str()) {
                Some(Collapsed::Node(n)) => n.name.as_str(),
                Some(Collapsed::Ensemble(e)) => e.name.as_str(),
                None => src_key.as_str(),
            };
            let content = fit_row(&format!("↺ {src_name}"), BOX_INNER, 2);
            extra.push(Line::from(Span::styled(
                format!("│  {content}│"),
                Style::default().fg(theme.dim_text),
            )));
        }
        lines.splice(2..2, extra);
    }

    lines
}

fn draw_connectors(
    placed: &[PlacedBox],
    non_back_edges: &HashMap<String, Vec<String>>,
    width: usize,
    total_height: usize,
) -> Vec<Vec<u8>> {
    let mut grid = vec![vec![0u8; width]; total_height];
    let by_key: HashMap<&str, &PlacedBox> = placed.iter().map(|b| (b.key.as_str(), b)).collect();

    let mut rank_bottoms: HashMap<usize, usize> = HashMap::new();
    for b in placed {
        let bottom = b.y + b.lines.len();
        let entry = rank_bottoms.entry(b.y).or_insert(0);
        *entry = (*entry).max(bottom);
    }

    for a in placed {
        let Some(targets) = non_back_edges.get(a.key.as_str()) else {
            continue;
        };
        let Some(&rank_bottom) = rank_bottoms.get(&a.y) else {
            continue;
        };
        let gap1_y = rank_bottom;
        let gap2_y = rank_bottom + 1;
        if gap2_y >= total_height {
            continue;
        }
        let ax = a.x + BOX_WIDTH / 2;
        let a_below_y = a.y + a.lines.len();

        for target_key in targets {
            let Some(b) = by_key.get(target_key.as_str()) else {
                continue;
            };
            let bx = b.x + BOX_WIDTH / 2;

            for row in &mut grid[a_below_y..=gap1_y] {
                row[ax] |= DIR_UP | DIR_DOWN;
            }
            if ax == bx {
                grid[gap2_y][ax] |= DIR_UP | DIR_DOWN;
            } else {
                let (min_x, max_x) = if ax < bx { (ax, bx) } else { (bx, ax) };
                grid[gap2_y][ax] |= DIR_UP;
                grid[gap2_y][bx] |= DIR_DOWN;
                grid[gap2_y][min_x] |= DIR_RIGHT;
                grid[gap2_y][max_x] |= DIR_LEFT;
                for cell in &mut grid[gap2_y][(min_x + 1)..max_x] {
                    *cell |= DIR_LEFT | DIR_RIGHT;
                }
            }
        }
    }

    grid
}

fn compose_canvas(
    placed: Vec<PlacedBox>,
    grid: &[Vec<u8>],
    total_height: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut row_boxes: Vec<Vec<(usize, Vec<Span<'static>>)>> = vec![Vec::new(); total_height];
    for b in placed {
        for (dy, line) in b.lines.into_iter().enumerate() {
            row_boxes[b.y + dy].push((b.x, line.spans));
        }
    }

    let conn_style = Style::default().fg(theme.dim_text);
    let mut out = Vec::with_capacity(total_height);

    for y in 0..total_height {
        let mut items: Vec<(usize, usize, Vec<Span<'static>>)> = Vec::new();
        for (x, spans) in std::mem::take(&mut row_boxes[y]) {
            items.push((x, BOX_WIDTH, spans));
        }
        let mut x = 0usize;
        while x < grid[y].len() {
            if grid[y][x] == 0 {
                x += 1;
                continue;
            }
            let start = x;
            let mut text = String::new();
            while x < grid[y].len() && grid[y][x] != 0 {
                text.push(connector_char(grid[y][x]));
                x += 1;
            }
            items.push((start, x - start, vec![Span::styled(text, conn_style)]));
        }
        items.sort_by_key(|(col, _, _)| *col);

        let mut spans = Vec::new();
        let mut cur_col = 0usize;
        for (col, width, item_spans) in items {
            if col > cur_col {
                spans.push(Span::raw(" ".repeat(col - cur_col)));
            }
            spans.extend(item_spans);
            cur_col = col + width;
        }
        out.push(Line::from(spans));
    }

    out
}

fn connector_char(bits: u8) -> char {
    match bits {
        b if b == DIR_UP | DIR_DOWN => '│',
        b if b == DIR_LEFT | DIR_RIGHT => '─',
        b if b == DIR_DOWN | DIR_RIGHT => '┌',
        b if b == DIR_DOWN | DIR_LEFT => '┐',
        b if b == DIR_UP | DIR_RIGHT => '└',
        b if b == DIR_UP | DIR_LEFT => '┘',
        b if b == DIR_UP | DIR_DOWN | DIR_RIGHT => '├',
        b if b == DIR_UP | DIR_DOWN | DIR_LEFT => '┤',
        b if b == DIR_DOWN | DIR_LEFT | DIR_RIGHT => '┬',
        b if b == DIR_UP | DIR_LEFT | DIR_RIGHT => '┴',
        b if b == DIR_UP | DIR_DOWN | DIR_LEFT | DIR_RIGHT => '┼',
        _ => ' ',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graphs::{GraphEdge, GraphNode, GraphNodeKind, GraphRunStatus, GraphStatus};
    use crate::tui::app::graph_live_state::{EnsembleLiveInfo, EnsembleMemberLiveInfo};
    use chrono::Utc;
    use serde_json::json;

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

    fn line_strings(result: &GraphLinesResult<'_>) -> Vec<String> {
        result
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn linear_three_nodes_renders_column_with_vertical_connectors() {
        let mut state = blank_state();
        state.effective_nodes = vec![node("a", 0), node("b", 1), node("c", 2)];
        state.effective_edges = vec![
            edge("e1", "a", "b", GraphEdgeCondition::Pass),
            edge("e2", "b", "c", GraphEdgeCondition::Pass),
        ];
        let res = diagram_lines(&state, None, false, 60, &Theme::classic())
            .expect("linear-3 should fit in 60 cols");
        let rows = line_strings(&res);
        // 3 boxes * 3 rows + 2 gaps * 2 rows = 13 rows
        assert_eq!(rows.len(), 13);
        assert!(rows[1].contains("a"));
        assert!(rows[3].contains('│'));
        assert!(rows[4].contains('│'));
        assert!(rows[6].contains("b"));
        assert!(rows[8].contains('│'));
        assert!(rows[9].contains('│'));
        assert!(rows[11].contains("c"));
    }

    #[test]
    fn diamond_renders_side_by_side_and_fan_connectors() {
        let mut state = blank_state();
        state.effective_nodes = vec![node("a", 0), node("b", 1), node("c", 2), node("d", 3)];
        state.effective_edges = vec![
            edge("e1", "a", "b", GraphEdgeCondition::Pass),
            edge("e2", "a", "c", GraphEdgeCondition::Fail),
            edge("e3", "b", "d", GraphEdgeCondition::Pass),
            edge("e4", "c", "d", GraphEdgeCondition::Pass),
        ];
        let res = diagram_lines(&state, None, false, 80, &Theme::classic())
            .expect("diamond should fit in 80 cols");
        let rows = line_strings(&res);
        // Rank 0 (0..3), gap (3..5), Rank 1 (5..8), gap (8..10), Rank 2 (10..13)
        assert_eq!(rows.len(), 13);
        assert!(rows[3].contains('│'));
        assert!(rows[4].contains('┴'));
        assert!(rows[4].contains('┌'));
        assert!(rows[4].contains('┐'));
        // B and C side by side on row 6
        assert!(rows[6].contains("b") && rows[6].contains("c"));
        assert!(rows[8].contains('│'));
        assert!(rows[9].contains('┬'));
        assert!(rows[9].contains('└'));
        assert!(rows[9].contains('┘'));
        assert!(rows[11].contains("d"));
    }

    #[test]
    fn ensemble_between_two_nodes_renders_collapsed_box() {
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
                    status: Some(GraphRunStatus::Pass),
                },
                EnsembleMemberLiveInfo {
                    node_id: "m1".to_string(),
                    label: "r2".to_string(),
                    status: Some(GraphRunStatus::Running),
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
        let res = diagram_lines(&state, None, false, 60, &Theme::classic())
            .expect("ensemble between nodes should render");
        let rows = line_strings(&res);
        // start (3) + gap (2) + ensemble (5: top, title, 2 members, bottom) + gap (2) + end (3) = 15
        assert_eq!(rows.len(), 15);
        assert!(rows[6].contains("Rev [2 models]"));
        assert!(rows[7].contains("r1 [pass]"));
        assert!(rows[8].contains("r2 [running]"));
    }

    #[test]
    fn retry_cycle_adds_back_edge_row_without_extra_connector() {
        let mut state = blank_state();
        state.effective_nodes = vec![node("impl", 0), node("check", 1)];
        state.effective_edges = vec![
            edge("e1", "impl", "check", GraphEdgeCondition::Pass),
            edge("e2", "check", "impl", GraphEdgeCondition::Fail),
        ];
        let res = diagram_lines(&state, None, false, 60, &Theme::classic())
            .expect("retry cycle should render");
        let rows = line_strings(&res);
        // impl (4 rows: top, title, ↺ check, bottom) + gap (2) + check (3) = 9
        assert_eq!(rows.len(), 9);
        assert!(rows[2].contains("↺ check"));
        assert!(rows[4].contains('│'));
        assert!(rows[5].contains('│'));
    }

    #[test]
    fn skip_level_edge_returns_none() {
        let mut state = blank_state();
        state.effective_nodes = vec![node("a", 0), node("b", 1), node("c", 2)];
        state.effective_edges = vec![
            edge("e1", "a", "b", GraphEdgeCondition::Pass),
            edge("e2", "b", "c", GraphEdgeCondition::Pass),
            edge("e3", "a", "c", GraphEdgeCondition::Fail),
        ];
        assert!(diagram_lines(&state, None, false, 80, &Theme::classic()).is_none());
    }

    #[test]
    fn fallbacks_for_narrow_width_many_boxes_and_wide_rank() {
        let mut state = blank_state();
        state.effective_nodes = vec![node("a", 0)];
        assert!(diagram_lines(&state, None, false, 29, &Theme::classic()).is_none());

        let mut wide_rank = blank_state();
        wide_rank.effective_nodes = vec![node("a", 0), node("b", 1), node("c", 2)];
        wide_rank.effective_edges = vec![
            edge("e1", "a", "b", GraphEdgeCondition::Pass),
            edge("e2", "a", "c", GraphEdgeCondition::Fail),
        ];
        // Rank 1 needs 2*24 + 2 = 50 cols, so width 40 must fall back.
        assert!(diagram_lines(&wide_rank, None, false, 40, &Theme::classic()).is_none());

        let mut too_many = blank_state();
        too_many.effective_nodes = (0..41).map(|i| node(&format!("n{i}"), i)).collect();
        assert!(diagram_lines(&too_many, None, false, 1200, &Theme::classic()).is_none());
    }

    #[test]
    fn highlight_offset_and_span_match_placed_box() {
        let mut state = blank_state();
        state.effective_nodes = vec![node("a", 0), node("b", 1)];
        state.effective_edges = vec![
            edge("e1", "a", "b", GraphEdgeCondition::Pass),
            edge("e2", "b", "a", GraphEdgeCondition::Fail),
        ];
        let res_a = diagram_lines(&state, Some("a"), true, 60, &Theme::classic()).unwrap();
        assert_eq!(res_a.highlighted_offset, Some(0));
        assert_eq!(res_a.highlighted_span, Some(4));

        let res_b = diagram_lines(&state, Some("b"), false, 60, &Theme::classic()).unwrap();
        assert_eq!(res_b.highlighted_offset, Some(6));
        assert_eq!(res_b.highlighted_span, Some(3));
    }
}
