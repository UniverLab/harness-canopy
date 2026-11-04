//! Part 02 of `tests` (split from `src/tui/ui/sidebar.rs`, pure move).
use super::*;

#[test]
fn draw_sidebar_renders_without_panic() {
    use crate::db::Database;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(Database::new(&path).unwrap());
    let data_dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .unwrap();
    let backend = TestBackend::new(33, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            draw_sidebar(frame, frame.area(), &mut app, &Theme::classic());
        })
        .unwrap();
}

#[test]
fn draw_sidebar_modern_theme_no_panic() {
    use crate::db::Database;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(Database::new(&path).unwrap());
    let data_dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .unwrap();
    let backend = TestBackend::new(33, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            draw_sidebar(frame, frame.area(), &mut app, &Theme::modern());
        })
        .unwrap();
}

#[test]
fn rag_status_line_shows_downloading_not_ready_even_when_loaded_flag_is_stale() {
    use crate::db::Database;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(Database::new(&path).unwrap());
    let data_dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .unwrap();
    app.rag_embeddings_model = "text-embedding-3-small".to_string();
    app.rag_model_loaded = true;
    app.rag_acquisition_state =
        Some(crate::rag::status::AcquisitionState::Downloading { started_at: 0 });

    let theme = Theme::classic();
    let line = rag_status_line(&app, &theme);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("downloading"), "got: {text:?}");
}

#[test]
fn rag_status_line_shows_download_failed() {
    use crate::db::Database;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(Database::new(&path).unwrap());
    let data_dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .unwrap();
    app.rag_embeddings_model = "text-embedding-3-small".to_string();
    app.rag_acquisition_state = Some(crate::rag::status::AcquisitionState::Failed {
        reason: "boom".to_string(),
    });

    let theme = Theme::classic();
    let line = rag_status_line(&app, &theme);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("download failed"), "got: {text:?}");
}

#[test]
fn split_shares_space_when_both_fit() {
    let area = Rect::new(0, 0, 30, 20);
    match split_brain_or_graph(area, true) {
        BrainOrGraphLayout::Both { graph, brain } => {
            assert!(graph.height >= GRAPH_MIN_HEIGHT, "graph below its minimum");
            assert!(brain.height >= 3, "brain below its minimum");
            assert_eq!(
                graph.height + brain.height,
                area.height,
                "the two sub-areas should cover the whole leftover area"
            );
            assert_eq!(
                brain.y,
                graph.y + graph.height,
                "brain should sit below the graph"
            );
        }
        other => panic!("expected Both, got {other:?}"),
    }
}

#[test]
fn split_gives_graph_alone_when_only_it_fits() {
    // Tall enough for the graph (>=4) but not for graph + brain (>=7).
    let area = Rect::new(0, 0, 30, 5);
    match split_brain_or_graph(area, true) {
        BrainOrGraphLayout::GraphOnly(graph) => assert_eq!(graph, area),
        other => panic!("expected GraphOnly, got {other:?}"),
    }
}

#[test]
fn split_gives_brain_alone_when_graph_is_empty() {
    // Below the graph's own minimum, but that's moot since it has nothing to show.
    let area = Rect::new(0, 0, 30, 3);
    match split_brain_or_graph(area, false) {
        BrainOrGraphLayout::BrainOnly(brain) => assert_eq!(brain, area),
        other => panic!("expected BrainOnly, got {other:?}"),
    }
}

#[test]
fn split_yields_neither_below_both_minimums() {
    let area = Rect::new(0, 0, 30, 2);
    assert_eq!(
        split_brain_or_graph(area, true),
        BrainOrGraphLayout::Neither
    );
}

#[test]
fn split_yields_neither_on_zero_height() {
    let area = Rect::new(0, 0, 30, 0);
    assert_eq!(
        split_brain_or_graph(area, true),
        BrainOrGraphLayout::Neither
    );
    assert_eq!(
        split_brain_or_graph(area, false),
        BrainOrGraphLayout::Neither
    );
}

// ── C26: graph_has_content / graph_edge_row_budget ────────────────

#[test]
fn graph_has_content_regression_defect_1_trees_but_no_edges() {
    // 39 singleton trees, 0 edges: `project_graph_trees` is never empty
    // (defect 1's false-positive signal), but with no edges there's
    // nothing to draw, so content must read false and the split must
    // hand the whole area to the brain.
    assert!(!graph_has_content(0, true));

    let area = Rect::new(0, 0, 30, 20);
    match split_brain_or_graph(area, graph_has_content(0, true)) {
        BrainOrGraphLayout::BrainOnly(brain) => assert_eq!(brain, area),
        other => panic!("expected BrainOnly, got {other:?}"),
    }
}

#[test]
fn graph_has_content_true_with_at_least_one_edge_on_knowledge_tab() {
    assert!(graph_has_content(1, true));
}

#[test]
fn graph_has_content_gated_to_knowledge_layer() {
    // Same edge count, same area — only the tab differs.
    let edge_count = 3;
    assert!(
        graph_has_content(edge_count, true),
        "Knowledge should show it"
    );
    assert!(
        !graph_has_content(edge_count, false),
        "Live/Automation should not show it"
    );
}

#[test]
fn graph_edge_row_budget_regression_defect_2() {
    // GRAPH_MIN_HEIGHT is the *outer* height the split hands the panel;
    // render_titled_panel strips Borders::ALL's 2 border rows before
    // draw_project_graph ever sees the area. The old code subtracted
    // another 2 from that already-inner height, computing 0 for a
    // panel that has exactly one edge to show.
    let inner_height = GRAPH_MIN_HEIGHT - 2;
    assert_eq!(graph_edge_row_budget(inner_height, 1), 1);
}

#[test]
fn graph_edge_row_budget_truncates_to_available_rows() {
    assert_eq!(graph_edge_row_budget(2, 5), 2);
}

#[test]
fn graph_edge_row_budget_draws_all_edges_when_rows_have_slack() {
    assert_eq!(graph_edge_row_budget(10, 3), 3);
}

#[test]
fn knowledge_tab_does_not_draw_graph_in_sidebar() {
    let text = render_sidebar_text_with(
        2,
        34,
        40,
        &Theme::classic(),
        SidebarLayer::Knowledge,
        |app| {
            app.project_graph_edges
                .push(crate::tui::app::types::ProjectGraphEdge {
                    from_name: "project0".to_string(),
                    to_name: "project1".to_string(),
                    from_hash: "hash0".to_string(),
                    to_hash: "hash1".to_string(),
                    relation: "depends_on".to_string(),
                });
        },
    );
    assert!(
        !text.contains("project graph"),
        "project graph belongs to the right-panel Knowledge face: {text}"
    );
}

#[test]
fn live_tab_never_shows_the_graph_panel_even_with_edges() {
    let text = render_sidebar_text_with(2, 34, 40, &Theme::classic(), SidebarLayer::Live, |app| {
        app.project_graph_edges
            .push(crate::tui::app::types::ProjectGraphEdge {
                from_name: "project0".to_string(),
                to_name: "project1".to_string(),
                from_hash: "hash0".to_string(),
                to_hash: "hash1".to_string(),
                relation: "depends_on".to_string(),
            });
    });
    assert!(
        !text.contains("project graph"),
        "graph panel must not render on Live: {text}"
    );
}

#[test]
fn registered_projects_with_zero_relations_do_not_claim_graph_height() {
    // Spec CB17: registered projects with zero relations make
    // `refresh_project_graph` fill `trees` with one singleton per project
    // while leaving `edges` empty. The old `!trees.is_empty()` height-claim
    // check fired on that, stealing GRAPH_MIN_HEIGHT rows to show only
    // "No relationships yet". The fixed criterion is the same one the
    // drawer uses — it requires an edge — so with none the panel claims
    // nothing and the strip goes to Brian's Brain.
    //
    // Guard: `knowledge_tab_with_an_edge_draws_the_graph_panel` renders
    // with this exact (project_count, width, height, tab) and asserts the
    // panel *does* appear once an edge is present. The only difference
    // here is the relation count, so a pass proves the criterion — not a
    // lack of room — is what withholds the rows.
    let text = render_sidebar_text_with(
        2,
        34,
        40,
        &Theme::classic(),
        SidebarLayer::Knowledge,
        |app| {
            assert!(
                !app.project_graph_trees.is_empty(),
                "precondition: singleton trees should be present"
            );
            assert!(
                app.project_graph_edges.is_empty(),
                "precondition: no relations were registered"
            );
        },
    );
    assert!(
        !text.contains("project graph"),
        "graph panel must not claim height with zero relations: {text}"
    );
    assert!(
        !text.contains("No relationships yet"),
        "empty message must not be drawn when panel has no content: {text}"
    );
}

#[test]
fn ct14_fair_section_floor_tracks_cursor_not_stored_focus() {
    // CT14 render invariant (C32 follow-on): the guaranteed floor follows
    // the section holding the cursor, not the stored focus alone — so a
    // zero-row focused section cannot steal the floor on short screens.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(crate::db::Database::new(&path).unwrap());
    let data_dir = tempfile::tempdir().unwrap();
    let mut app = crate::tui::app::App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .unwrap();

    // Cursor sits on a terminal row while stored focus claims Interactive.
    app.agents = vec![
        AgentEntry::Interactive(0),
        AgentEntry::Interactive(1),
        AgentEntry::Terminal(0),
    ];
    app.selected = 2;
    app.agent_section_focus = AgentSectionFocus::Interactive;
    assert_eq!(
        super::live_section_floor_index(&app, &[0, 1], &[2]),
        Some(1),
        "floor must track the terminal cursor, not the stored Interactive focus"
    );

    // Automation mirror: cursor on a background agent while the stored
    // kind claims Graphs (whose list is empty here).
    app.agents = vec![AgentEntry::Agent(crate::domain::models::Agent {
        id: "bg-1".to_string(),
        prompt: String::new(),
        trigger: None,
        cli: crate::domain::models::Cli::new("claude"),
        model: None,
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: chrono::Utc::now(),
        log_path: "/tmp/bg-1.log".to_string(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    })];
    app.selected = 0;
    app.selected_graph_id = None;
    app.automation_kind = AutomationKind::Graph;
    assert_eq!(
        super::automation_section_floor_index(&app, &[0]),
        Some(0),
        "floor must track the agent cursor, not the stored Graph kind"
    );
}

#[test]
fn sidebar_shows_the_live_device_count_only_when_nonzero() {
    let text = render_sidebar_text_with(0, 40, 24, &Theme::classic(), SidebarLayer::Live, |app| {
        app.hub.remote.view.live_devices = vec!["a".to_string(), "b".to_string()];
    });
    assert!(text.contains("⇄ 2"), "two live devices: {text}");

    let text = render_sidebar_text(0, 40, 24);
    assert!(!text.contains('⇄'), "nothing when none live: {text}");
}
