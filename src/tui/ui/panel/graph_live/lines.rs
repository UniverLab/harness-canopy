//! Node and edge line construction for the live graph view: box drawing,
//! ensemble collapsing, and the DFS walk over the collapsed tree. Split
//! out of `graph_live.rs` to keep every production file within the
//! file-size gate.

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::domain::graphs::{GraphEdgeCondition, GraphNode, GraphNodeKind, GraphRunStatus};
use crate::tui::app::graph_live_state::{EnsembleLiveInfo, GraphLiveState};
use crate::tui::ui::theme::Theme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Border/text style and marker glyph for a node box — bold accent with a
/// solid marker while auto-following (the "pulsing current node" cue),
/// plain accent with a `›` marker for a manually-picked node, dim/plain
/// otherwise.
fn node_style(is_highlighted: bool, follow: bool, theme: &Theme) -> (Style, Style, &'static str) {
    if is_highlighted && follow {
        (
            Style::default()
                .fg(theme.header_color)
                .add_modifier(Modifier::BOLD),
            Style::default()
                .fg(theme.header_color)
                .add_modifier(Modifier::BOLD),
            "●",
        )
    } else if is_highlighted {
        (
            Style::default().fg(theme.header_color),
            Style::default()
                .fg(theme.header_color)
                .add_modifier(Modifier::BOLD),
            "›",
        )
    } else {
        (
            Style::default().fg(theme.border_color),
            Style::default().fg(theme.text_primary),
            " ",
        )
    }
}

fn depth_prefix(depth: usize) -> String {
    "   ".repeat(depth.min(3))
}

fn edge_condition_priority(condition: &GraphEdgeCondition) -> (u8, Option<String>) {
    match condition {
        GraphEdgeCondition::Pass => (0, None),
        GraphEdgeCondition::Fail => (1, None),
        GraphEdgeCondition::Always => (2, None),
        GraphEdgeCondition::Route(label) => (3, Some(label.clone())),
        GraphEdgeCondition::Error => (4, None),
    }
}

/// Truncates `content` to at most `max_width` display columns (per
/// `unicode-width`), appending `…` when it doesn't fit. Unlike
/// `truncate_str`, this measures render columns rather than
/// `chars().count()`, so a wide character (CJK, emoji) is never counted as
/// one column when it occupies two — the miscount that let a wide label
/// desync a box border.
fn truncate_str_width(s: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let budget = max_width - 1; // reserve 1 column for the ellipsis
    let mut out = String::new();
    let mut width = 0usize;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > budget {
            break;
        }
        out.push(ch);
        width += cw;
    }
    out.push('…');
    out
}

/// Fits `content` into a box row that already has `leading` display columns
/// of fixed prefix before it (a marker+space, or a two-space indent):
/// truncates `content` by display width to leave room for one mandatory
/// blank column before the closing border, then right-pads with spaces and
/// appends that blank column. The result's display width is always
/// `inner - leading`, so `leading + width(fit_row(...)) == inner` for every
/// row built this way — this is what keeps member rows exactly as wide as
/// the title and border rows (previously short by 2 columns because the
/// padding math didn't add up to `inner`).
fn fit_row(content: &str, inner: usize, leading: usize) -> String {
    let budget = inner.saturating_sub(leading + 1);
    let truncated = truncate_str_width(content, budget);
    let pad = budget.saturating_sub(UnicodeWidthStr::width(truncated.as_str()));
    format!("{truncated}{} ", " ".repeat(pad))
}

pub(super) fn node_box_lines(
    node: &GraphNode,
    is_highlighted: bool,
    follow: bool,
    inner: usize,
    theme: &Theme,
    depth: usize,
) -> Vec<Line<'static>> {
    let prefix = depth_prefix(depth);
    let (border_style, text_style, marker) = node_style(is_highlighted, follow, theme);
    let kind_tag = format!("[{}]", node.kind.display_str());
    let kind_tag_width = UnicodeWidthStr::width(kind_tag.as_str());
    let kind_tag_style = if node.kind == GraphNodeKind::Router {
        Style::default()
            .fg(theme.kind_router)
            .add_modifier(Modifier::BOLD)
    } else {
        text_style
    };
    // Budget reserves: marker+space (2, "leading"), kind_tag_width, and one
    // mandatory trailing blank column before the border.
    let name_budget = inner.saturating_sub(2 + kind_tag_width + 1);
    let name_display = truncate_str_width(&node.name, name_budget);
    let pad = name_budget.saturating_sub(UnicodeWidthStr::width(name_display.as_str()));

    vec![
        Line::from(Span::styled(
            format!("{prefix}┌{}┐", "─".repeat(inner)),
            border_style,
        )),
        Line::from(vec![
            Span::styled(
                format!("{prefix}│{marker} {name_display}{}", " ".repeat(pad)),
                text_style,
            ),
            Span::styled(kind_tag, kind_tag_style),
            Span::styled(" │", text_style),
        ]),
        Line::from(Span::styled(
            format!("{prefix}└{}┘", "─".repeat(inner)),
            border_style,
        )),
    ]
}

/// Collapsed box for an ensemble (F1) — folds its N member nodes plus the
/// join into ONE box ("name [N models]") with a live status tag per member,
/// instead of drawing N+1 separate boxes and a fan-out of near-identical
/// edges. Expand-on-inspect isn't a separate view: the member/join ids are
/// still real entries in `state.effective_nodes`, so navigating directly to
/// one (e.g. via a future picker) still resolves correctly — this box is
/// purely a collapsed *rendering*, not a different graph.
pub(super) fn ensemble_box_lines(
    ensemble: &EnsembleLiveInfo,
    is_highlighted: bool,
    follow: bool,
    inner: usize,
    theme: &Theme,
    depth: usize,
) -> Vec<Line<'static>> {
    let prefix = depth_prefix(depth);
    let (border_style, text_style, marker) = node_style(is_highlighted, follow, theme);
    let title = format!("{} [{} models]", ensemble.name, ensemble.members.len());

    let mut lines = vec![
        Line::from(Span::styled(
            format!("{prefix}┌{}┐", "─".repeat(inner)),
            border_style,
        )),
        Line::from(Span::styled(
            format!("{prefix}│{marker} {}│", fit_row(&title, inner, 2)),
            text_style,
        )),
    ];
    for member in &ensemble.members {
        let (tag, color) = ensemble_member_status_tag(member.status, theme);
        let label = format!("{} {tag}", member.label);
        lines.push(Line::from(Span::styled(
            format!("{prefix}│  {}│", fit_row(&label, inner, 2)),
            Style::default().fg(color),
        )));
    }
    lines.push(Line::from(Span::styled(
        format!("{prefix}└{}┘", "─".repeat(inner)),
        border_style,
    )));
    lines
}

fn ensemble_member_status_tag(
    status: Option<GraphRunStatus>,
    theme: &Theme,
) -> (&'static str, Color) {
    match status {
        Some(GraphRunStatus::Pass) => ("[pass]", theme.status_ok),
        Some(GraphRunStatus::Fail) => ("[fail]", theme.status_fail),
        Some(GraphRunStatus::Interrupted) => ("[interrupted]", theme.status_fail),
        Some(GraphRunStatus::Error) => ("[error]", theme.status_interrupted),
        Some(GraphRunStatus::Running) => ("[running]", theme.status_running),
        None => ("[pending]", theme.dim_text),
    }
}

/// Node boxes in DFS tree order starting from the entry node (the node
/// with no incoming edges, or position 0 if every node has one). Each node's
/// children appear immediately after its edge lines, indented one level
/// deeper. Cycles render as a back-reference line with `↩` instead of a
/// second box. An ensemble (F1) renders as one collapsed box at the depth
/// of its first member; other members and the join are skipped.
pub(super) struct GraphLinesResult<'a> {
    pub(super) lines: Vec<Line<'a>>,
    pub(super) highlighted_offset: Option<u16>,
}

/// A collapsed box entry: either a plain node or a whole ensemble.
enum Collapsed<'a> {
    Node(&'a GraphNode),
    Ensemble(&'a EnsembleLiveInfo),
}

/// Indexes over the state's ensembles, used to collapse members into one box.
struct EnsembleIndex<'a> {
    join_ids: std::collections::HashSet<&'a str>,
    by_member: HashMap<&'a str, &'a EnsembleLiveInfo>,
    by_join: HashMap<&'a str, &'a EnsembleLiveInfo>,
}

/// Borrowed state the recursive DFS walk needs.
struct DfsCtx<'a> {
    nodes: &'a HashMap<String, Collapsed<'a>>,
    edges: &'a HashMap<String, Vec<(String, GraphEdgeCondition)>>,
    state: &'a GraphLiveState,
    follow: bool,
    inner: usize,
    theme: &'a Theme,
    highlighted_node_id: Option<&'a str>,
}

pub(super) fn graph_lines(
    state: &GraphLiveState,
    highlighted_node_id: Option<&str>,
    follow: bool,
    area_width: u16,
    theme: &Theme,
) -> GraphLinesResult<'static> {
    let box_width = (area_width as usize).saturating_sub(4).clamp(22, 48);
    let inner = box_width.saturating_sub(2);

    let index = ensemble_index(state);
    let (collapsed_nodes, collapsed_pos) = collapse_nodes(state, &index);
    if collapsed_nodes.is_empty() {
        return GraphLinesResult {
            lines: Vec::new(),
            highlighted_offset: None,
        };
    }

    let collapsed_edges = collapse_edges(state, &index, &collapsed_nodes);
    let entry_key = find_entry_key(&collapsed_nodes, &collapsed_edges, &collapsed_pos);
    let ctx = DfsCtx {
        nodes: &collapsed_nodes,
        edges: &collapsed_edges,
        state,
        follow,
        inner,
        theme,
        highlighted_node_id,
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut highlighted_offset: Option<u16> = None;
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Start DFS from entry
    dfs_visit(
        &entry_key,
        0,
        &ctx,
        &mut lines,
        &mut highlighted_offset,
        &mut visited,
    );
    // Visit any disconnected components not reached from entry
    let mut remaining: Vec<String> = collapsed_nodes
        .keys()
        .filter(|k| !visited.contains(k.as_str()))
        .cloned()
        .collect();
    remaining.sort_by_key(|k| collapsed_pos.get(k.as_str()).copied().unwrap_or(i64::MAX));
    for rk in &remaining {
        dfs_visit(
            rk,
            0,
            &ctx,
            &mut lines,
            &mut highlighted_offset,
            &mut visited,
        );
    }
    // Trim trailing blank lines
    while lines.last().is_some_and(|l| l.width() == 0) {
        lines.pop();
    }
    GraphLinesResult {
        lines,
        highlighted_offset,
    }
}

/// Index the state's ensembles by join node and by member node.
fn ensemble_index(state: &GraphLiveState) -> EnsembleIndex<'_> {
    EnsembleIndex {
        join_ids: state
            .ensembles
            .iter()
            .map(|e| e.join_node_id.as_str())
            .collect(),
        by_member: state
            .ensembles
            .iter()
            .flat_map(|e| e.members.iter().map(move |m| (m.node_id.as_str(), e)))
            .collect(),
        by_join: state
            .ensembles
            .iter()
            .map(|e| (e.join_node_id.as_str(), e))
            .collect(),
    }
}

/// Collapse ensemble members into one box each, recording the first position.
fn collapse_nodes<'a>(
    state: &'a GraphLiveState,
    index: &EnsembleIndex<'a>,
) -> (HashMap<String, Collapsed<'a>>, HashMap<String, i64>) {
    let mut collapsed_nodes: HashMap<String, Collapsed<'a>> = HashMap::new();
    // Position for ordering fallback entry selection.
    let mut collapsed_pos: HashMap<String, i64> = HashMap::new();
    let mut seen_ens: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for node in &state.effective_nodes {
        if index.join_ids.contains(node.id.as_str()) {
            continue;
        }
        if let Some(ens) = index.by_member.get(node.id.as_str()) {
            if seen_ens.insert(ens.ensemble_id.as_str()) {
                // position is the first member's position
                collapsed_nodes.insert(ens.ensemble_id.clone(), Collapsed::Ensemble(ens));
                collapsed_pos.insert(ens.ensemble_id.clone(), node.position);
            }
        } else {
            collapsed_nodes.insert(node.id.clone(), Collapsed::Node(node));
            collapsed_pos.insert(node.id.clone(), node.position);
        }
    }
    (collapsed_nodes, collapsed_pos)
}

/// Rewrite edges onto collapsed keys, dropping ensemble-internal edges and
/// duplicates, then sorting each adjacency by condition priority.
fn collapse_edges<'a>(
    state: &'a GraphLiveState,
    index: &EnsembleIndex<'a>,
    collapsed_nodes: &HashMap<String, Collapsed<'a>>,
) -> HashMap<String, Vec<(String, GraphEdgeCondition)>> {
    let mut collapsed_edges: HashMap<String, Vec<(String, GraphEdgeCondition)>> = HashMap::new();
    for edge in &state.effective_edges {
        let from_raw = edge.from_node.as_str();
        let to_raw = edge.to_node.as_str();
        // internal ensemble member -> join edge
        if let Some(ens) = index.by_member.get(from_raw) {
            if ens.join_node_id.as_str() == to_raw {
                continue;
            }
        }
        let from_key = collapse_key(from_raw, index, collapsed_nodes);
        let to_key = collapse_key(to_raw, index, collapsed_nodes);
        if let (Some(fk), Some(tk)) = (from_key, to_key) {
            if fk == tk {
                // self-graph: keep as edge but DFS will treat it as back-ref
            }
            let entry = collapsed_edges.entry(fk).or_default();
            if !entry
                .iter()
                .any(|(ek, ec)| ek == &tk && ec == &edge.condition)
            {
                entry.push((tk, edge.condition.clone()));
            }
        }
    }
    // Sort each adjacency by pass > fail > always > route(alpha) > error
    for edges in collapsed_edges.values_mut() {
        edges.sort_by(|a, b| {
            let (pa, la) = edge_condition_priority(&a.1);
            let (pb, lb) = edge_condition_priority(&b.1);
            pa.cmp(&pb).then_with(|| la.cmp(&lb))
        });
    }
    collapsed_edges
}

/// Map a raw node id onto the collapsed key that represents it.
fn collapse_key(
    raw: &str,
    index: &EnsembleIndex,
    collapsed_nodes: &HashMap<String, Collapsed>,
) -> Option<String> {
    if let Some(ens) = index.by_join.get(raw) {
        return Some(ens.ensemble_id.clone());
    }
    if let Some(ens) = index.by_member.get(raw) {
        return Some(ens.ensemble_id.clone());
    }
    if collapsed_nodes.contains_key(raw) {
        return Some(raw.to_string());
    }
    None
}

/// The DFS entry node: the one with no incoming collapsed edge, or the one
/// with the smallest position when every node has one.
fn find_entry_key(
    nodes: &HashMap<String, Collapsed>,
    edges: &HashMap<String, Vec<(String, GraphEdgeCondition)>>,
    positions: &HashMap<String, i64>,
) -> String {
    let mut incoming: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for tos in edges.values() {
        for (tk, _) in tos {
            incoming.insert(tk.as_str());
        }
    }
    nodes
        .keys()
        .find(|k| !incoming.contains(k.as_str()))
        .cloned()
        .or_else(|| {
            // fallback to smallest position
            nodes
                .keys()
                .min_by_key(|k| positions.get(k.as_str()).copied().unwrap_or(i64::MAX))
                .cloned()
        })
        .unwrap()
}

/// Recursive DFS walk over the collapsed tree.
fn dfs_visit(
    key: &str,
    depth: usize,
    ctx: &DfsCtx,
    lines: &mut Vec<Line<'static>>,
    highlighted_offset: &mut Option<u16>,
    visited: &mut std::collections::HashSet<String>,
) {
    if visited.contains(key) {
        return;
    }
    visited.insert(key.to_string());
    let depth_clamped = depth.min(3);
    let is_hl = dfs_is_highlighted(ctx, key);
    if is_hl && highlighted_offset.is_none() {
        *highlighted_offset = Some(lines.len() as u16);
    }
    let Some(entry) = ctx.nodes.get(key) else {
        return;
    };
    dfs_push_box(lines, ctx, entry, depth_clamped, is_hl);
    let Some(edges) = ctx.edges.get(key) else {
        // leaf node still needs spacing before next sibling at same depth
        // Only add blank if not last overall? DFS already pushes blanks per node with edges;
        // for leaves, add a blank line to separate siblings visually unless at end.
        // We'll add a blank line for consistency with old rendering.
        lines.push(Line::from(""));
        return;
    };
    dfs_edge_lines(lines, ctx, key, depth_clamped, edges, visited);
    lines.push(Line::from(""));
    // Recurse into unvisited children in priority order
    dfs_recurse_children(lines, ctx, depth, edges, highlighted_offset, visited);
}

/// Whether this entry is the highlighted node (or holds it as a member/join).
fn dfs_is_highlighted(ctx: &DfsCtx, key: &str) -> bool {
    if let Some(Collapsed::Ensemble(ens)) = ctx.nodes.get(key) {
        return ens
            .members
            .iter()
            .any(|m| Some(m.node_id.as_str()) == ctx.highlighted_node_id)
            || ctx.highlighted_node_id == Some(ens.join_node_id.as_str());
    }
    ctx.highlighted_node_id == Some(key)
}

/// Render the node or ensemble box for an entry.
fn dfs_push_box(
    lines: &mut Vec<Line<'static>>,
    ctx: &DfsCtx,
    entry: &Collapsed,
    depth_clamped: usize,
    is_hl: bool,
) {
    match entry {
        Collapsed::Node(node) => lines.extend(node_box_lines(
            node,
            is_hl,
            ctx.follow,
            ctx.inner,
            ctx.theme,
            depth_clamped,
        )),
        Collapsed::Ensemble(ens) => lines.extend(ensemble_box_lines(
            ens,
            is_hl,
            ctx.follow,
            ctx.inner,
            ctx.theme,
            depth_clamped,
        )),
    }
}

/// Render the edge lines hanging off one entry.
fn dfs_edge_lines(
    lines: &mut Vec<Line<'static>>,
    ctx: &DfsCtx,
    key: &str,
    depth_clamped: usize,
    edges: &[(String, GraphEdgeCondition)],
    visited: &std::collections::HashSet<String>,
) {
    let prefix = depth_prefix(depth_clamped);
    // Determine taken route for router nodes (only regular nodes)
    let taken_route = if let Some(Collapsed::Node(node)) = ctx.nodes.get(key) {
        ctx.state
            .router_taken_routes
            .get(node.id.as_str())
            .map(String::as_str)
    } else {
        None
    };
    for (i, (to_key, cond)) in edges.iter().enumerate() {
        let branch = if i == edges.len() - 1 { "└" } else { "├" };
        let is_back = visited.contains(to_key.as_str());
        let target_label = dfs_target_label(ctx, to_key);
        let suffix = if is_back { "  ↩" } else { "" };
        lines.push(dfs_edge_line(
            ctx,
            &prefix,
            branch,
            cond,
            &target_label,
            suffix,
            taken_route,
        ));
    }
}

/// Label to show for an edge target — the name, or the collapsed node key.
fn dfs_target_label(ctx: &DfsCtx, to_key: &str) -> String {
    match ctx.nodes.get(to_key) {
        Some(Collapsed::Ensemble(ens)) => {
            format!("{} [{} models]", ens.name, ens.members.len())
        }
        Some(Collapsed::Node(n)) => n.name.clone(),
        None => to_key.to_string(),
    }
}

/// Render one edge line — the route form (with the taken marker) or plain form.
fn dfs_edge_line(
    ctx: &DfsCtx,
    prefix: &str,
    branch: &str,
    cond: &GraphEdgeCondition,
    target_label: &str,
    suffix: &str,
    taken_route: Option<&str>,
) -> Line<'static> {
    match cond.route_label() {
        Some(route) => {
            let taken = taken_route == Some(route);
            let (marker, style) = if taken {
                (
                    "✓",
                    Style::default()
                        .fg(ctx.theme.status_ok)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (" ", Style::default().fg(ctx.theme.dim_text))
            };
            Line::from(Span::styled(
                format!("{prefix}   {branch}─{marker} {route} → {target_label}{suffix}"),
                style,
            ))
        }
        None => {
            let label = format!("{target_label}{suffix}");
            Line::from(Span::styled(
                format!("{prefix}   {}─ {} → {}", branch, cond.as_str(), label),
                Style::default().fg(ctx.theme.dim_text),
            ))
        }
    }
}

/// Recurse into the unvisited children of an entry, in adjacency order.
fn dfs_recurse_children(
    lines: &mut Vec<Line<'static>>,
    ctx: &DfsCtx,
    depth: usize,
    edges: &[(String, GraphEdgeCondition)],
    highlighted_offset: &mut Option<u16>,
    visited: &mut std::collections::HashSet<String>,
) {
    for (to_key, _) in edges {
        if visited.contains(to_key.as_str()) {
            continue;
        }
        dfs_visit(to_key, depth + 1, ctx, lines, highlighted_offset, visited);
    }
}
