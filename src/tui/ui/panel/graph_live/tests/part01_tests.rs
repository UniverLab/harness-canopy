//! Part 01 of `tests` (split from `src/tui/ui/panel/graph_live.rs`, pure move).
use super::*;

#[test]
fn running_graph_renders_header_queue_graph_and_footer_with_current_node_highlighted() {
    let state = running_state();
    let node_info = NodeRunInfo {
        status: state.current_node_status,
        started_at: state.current_node_started_at,
        iteration: state.current_node_iteration,
        chosen_route: None,
        waiting_for: Vec::new(),
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
        waiting_for: Vec::new(),
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
        spec_code: None,
        spec_type: None,
        spec_scope: None,
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
            spec_code: None,
            spec_type: None,
            spec_scope: None,
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
            spec_code: None,
            spec_type: None,
            spec_scope: None,
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
            spec_code: None,
            spec_type: None,
            spec_scope: None,
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
        waiting_for: Vec::new(),
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
fn graph_canvas_sits_on_info_bg() {
    let state = running_state();
    let node_info = NodeRunInfo {
        status: state.current_node_status,
        started_at: state.current_node_started_at,
        iteration: state.current_node_iteration,
        chosen_route: None,
        waiting_for: Vec::new(),
    };
    let theme = Theme::classic();
    let backend = ratatui::backend::TestBackend::new(80, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
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
                    highlighted_node_id: state.current_node_id.as_deref(),
                    node_info: &node_info,
                    blocked: false,
                    now: Utc::now(),
                    theme: &theme,
                    time_format: "%H:%M",
                    selected_spec_id: None,
                    spec_scroll: 0,
                    scroll: 0,
                },
            );
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut tinted = 0;
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if buffer[(x, y)].bg == theme.info_bg {
                tinted += 1;
            }
        }
    }
    assert!(
        tinted >= 1,
        "graph canvas must paint cells on info_bg, found none"
    );
}
