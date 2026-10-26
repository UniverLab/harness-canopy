//! Part 02 of `tests` (split from `src/tui/ui/panel/graph_live.rs`, pure move).
use super::*;

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
