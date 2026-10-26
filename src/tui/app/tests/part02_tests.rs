//! Part 02 of `tests` (split from `src/tui/app/mod.rs`, pure move).
use super::*;

// ── AgentEntry::id tests ────────────────────────────────────

#[test]
fn agent_entry_id_for_agent() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let entry = AgentEntry::Agent(crate::domain::models::Agent {
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
    });
    assert_eq!(entry.id(&app), "bg-1");
}

#[test]
fn agent_entry_id_for_corrupt() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let entry = AgentEntry::Corrupt(crate::domain::models::CorruptAgent {
        id: "corrupt-1".to_string(),
        enabled: false,
        error: "corrupt row".to_string(),
    });
    assert_eq!(entry.id(&app), "corrupt-1");
}

#[test]
fn agent_entry_id_for_group_out_of_bounds() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let entry = AgentEntry::Group(99);
    assert_eq!(entry.id(&app), "?");
}

#[test]
fn agent_entry_id_for_interactive_out_of_bounds() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let entry = AgentEntry::Interactive(99);
    assert_eq!(entry.id(&app), "?");
}

#[test]
fn agent_entry_id_for_terminal_out_of_bounds() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let entry = AgentEntry::Terminal(99);
    assert_eq!(entry.id(&app), "?");
}

#[test]
fn agent_entry_id_for_orphaned_out_of_bounds() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let entry = AgentEntry::Orphaned(99);
    assert_eq!(entry.id(&app), "?");
}

// ── Navigation edge cases ───────────────────────────────────

#[test]
fn select_next_empty_agents_stays_put() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.agents.clear();
    app.selected = 0;
    app.sidebar_layer = SidebarLayer::Live;
    app.select_next();
    // Should not panic, stays at 0
    assert_eq!(app.selected, 0);
}

#[test]
fn select_prev_empty_agents_stays_put() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.agents.clear();
    app.selected = 0;
    app.sidebar_layer = SidebarLayer::Live;
    app.select_prev();
    assert_eq!(app.selected, 0);
}

#[test]
fn select_agent_at_out_of_bounds_does_nothing() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.agents.clear();
    let prev = app.selected;
    app.select_agent_at(999);
    assert_eq!(app.selected, prev);
}

#[test]
fn scroll_log_down_and_up() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.log_scroll = 10;
    app.scroll_log_down();
    assert_eq!(app.log_scroll, 13);
    app.scroll_log_up();
    assert_eq!(app.log_scroll, 10);
}

#[test]
fn scroll_log_up_at_zero_stays_zero() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.log_scroll = 0;
    app.scroll_log_up();
    assert_eq!(app.log_scroll, 0);
}

// ── sidebar_graphs: no status filtering ──────────────────────

#[test]
fn sidebar_graphs_includes_every_status() {
    use crate::domain::graphs::GraphStatus;
    let db = test_db();
    db.insert_graph(&make_graph("l1", "Running", GraphStatus::Running))
        .unwrap();
    db.insert_graph(&make_graph("l2", "Draft", GraphStatus::Draft))
        .unwrap();
    db.insert_graph(&make_graph("l3", "Paused", GraphStatus::Paused))
        .unwrap();
    db.insert_graph(&make_graph("l4", "Completed", GraphStatus::Completed))
        .unwrap();
    db.insert_graph(&make_graph("l5", "Failed", GraphStatus::Failed))
        .unwrap();

    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let listed: Vec<&str> = app
        .sidebar_graphs()
        .iter()
        .map(|lp| lp.id.as_str())
        .collect();
    assert!(
        listed.contains(&"l4"),
        "a completed graph must stay in the sidebar list"
    );
    assert!(
        listed.contains(&"l5"),
        "a failed graph must stay in the sidebar list"
    );
    assert!(listed.contains(&"l1"));
    assert!(listed.contains(&"l2"));
    assert!(listed.contains(&"l3"));
    assert_eq!(listed.len(), 5, "no graph is dropped by status");
}

// ── Playground state tests ───────────────────────────────────

#[test]
fn activate_deactivate_playground() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(!app.playground_active);

    app.playground_query = "test query".to_string();
    app.playground_selected = 5;
    app.playground_active = true;
    app.activate_playground();
    assert!(app.playground_active);
    assert!(app.playground_query.is_empty());
    assert_eq!(app.playground_selected, 0);

    app.deactivate_playground();
    assert!(!app.playground_active);
    assert!(app.playground_query.is_empty());
}

// ── Playground search edge cases ─────────────────────────────

#[test]
fn poll_playground_search_no_rx_returns_early() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.playground_search_rx = None;
    // Should not panic
    app.poll_playground_search();
}

#[test]
fn poll_playground_search_disconnected_cleans_up() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let (tx, rx) = std::sync::mpsc::channel();
    app.playground_search_rx = Some(rx);
    app.playground_search_pending = true;
    drop(tx);
    app.poll_playground_search();
    assert!(app.playground_search_rx.is_none());
    assert!(!app.playground_search_pending);
}

#[test]
fn duplicate_selected_graph_node_copies_config_and_opens_editor() {
    use crate::domain::graphs::{
        GraphNode, GraphNodeKind, GraphSpec, GraphSpecStatus, GraphStatus,
    };
    use crate::tui::app::types::Focus;

    let db = test_db();
    db.insert_graph(&make_graph("graph-1", "Graph", GraphStatus::Draft))
        .unwrap();
    db.insert_graph_spec(&GraphSpec {
        id: "spec-a".to_string(),
        graph_id: Some("graph-1".to_string()),
        name: "spec-a".to_string(),
        description: None,
        position: 0,
        parallelizable: false,
        status: GraphSpecStatus::Pending,
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
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "impl".to_string(),
        spec_id: Some("spec-a".to_string()),
        graph_id: None,
        name: "implement".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude", "prompt_template": "do it"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.selected_graph_id = Some("graph-1".to_string());
    app.graph_details = db.get_graph_details("graph-1").unwrap();
    app.graph_selected_spec = 0;
    app.graph_selected_node = 0;
    assert_eq!(app.selected_graph_node().unwrap().id, "impl");

    app.duplicate_selected_graph_node().unwrap();

    // The editor opens pre-filled on the new copy.
    assert!(matches!(app.focus, Focus::GraphEditorDialog));
    let dialog = app.graph_editor_dialog.as_ref().unwrap();
    assert!(
        dialog.node_name.ends_with("(copy)"),
        "expected a copy name, got '{}'",
        dialog.node_name
    );
    assert_ne!(dialog.node_id, "impl", "the copy must have a fresh id");

    // A second node now exists on the spec with the source's config.
    let nodes = db.list_graph_nodes("spec-a").unwrap();
    assert_eq!(nodes.len(), 2);
    let copy = nodes.iter().find(|n| n.id != "impl").unwrap();
    assert_eq!(copy.name, "implement (copy)");
    assert_eq!(copy.config["platform"], "claude");
    assert_eq!(copy.config["prompt_template"], "do it");
}

// ── Additional navigation and state tests ───────────────────

#[test]
fn sidebar_graphs_empty_when_no_graphs() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(app.sidebar_graphs().is_empty());
}

#[test]
fn live_indices_empty_when_no_agents() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(app.live_indices().is_empty());
}

#[test]
fn automation_agent_indices_empty_when_no_agents() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(app.automation_agent_indices().is_empty());
}

#[test]
fn step_sidebar_tab_forward_wraps() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.sidebar_layer = SidebarLayer::Knowledge;
    app.step_sidebar_tab(true);
    assert_eq!(app.sidebar_layer, SidebarLayer::Live);
}

#[test]
fn step_sidebar_tab_backward_wraps() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.sidebar_layer = SidebarLayer::Live;
    app.step_sidebar_tab(false);
    assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);
}

#[test]
fn step_sidebar_tab_restores_non_edge_selection_on_return() {
    // Functional requirement 6: stepping away from a layer and back must
    // not disturb what was selected there, unlike a fresh jump (F2,
    // click), which deliberately always lands on the tab's edge item.
    // `Live` and Automation's agent sub-list share `selected` as their
    // index space, so this specifically exercises the case where
    // leaving one for the other would otherwise clobber it.
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.agents = vec![
        AgentEntry::Group(0),
        AgentEntry::Group(1),
        AgentEntry::Agent(bg_agent("bg-1")),
    ];
    app.sidebar_layer = SidebarLayer::Live;
    app.selected = 1; // the second (non-edge) Live item

    app.step_sidebar_tab(true);
    assert_eq!(app.sidebar_layer, SidebarLayer::Automation);
    assert_eq!(
        app.selected, 2,
        "a fresh jump into Automation lands on its edge item"
    );

    app.step_sidebar_tab(false);
    assert_eq!(app.sidebar_layer, SidebarLayer::Live);
    assert_eq!(
        app.selected, 1,
        "returning to Live must restore the remembered selection, not reset to its edge"
    );
}

#[test]
fn step_sidebar_tab_falls_back_to_edge_when_remembered_selection_is_gone() {
    // If the remembered index no longer belongs to the layer (e.g. the
    // entry was removed while away), stepping back must not restore a
    // stale/invalid index — it should behave like a fresh jump instead.
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.agents = vec![
        AgentEntry::Group(0),
        AgentEntry::Group(1),
        AgentEntry::Agent(bg_agent("bg-1")),
    ];
    app.sidebar_layer = SidebarLayer::Live;
    app.selected = 1;

    app.step_sidebar_tab(true);
    assert_eq!(app.sidebar_layer, SidebarLayer::Automation);

    // The remembered Live index (1) is no longer a Live entry.
    app.agents[1] = AgentEntry::Agent(bg_agent("bg-2"));

    app.step_sidebar_tab(false);
    assert_eq!(app.sidebar_layer, SidebarLayer::Live);
    assert_eq!(
        app.selected, 0,
        "an invalidated memory falls back to the edge item"
    );
}

#[test]
fn reset_log_scroll_sets_to_zero() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.log_scroll = 50;
    app.sidebar_scroll_offset = 10;
    app.reset_log_scroll();
    assert_eq!(app.log_scroll, 0);
    assert_eq!(app.sidebar_scroll_offset, 0);
}

#[test]
fn cycle_sidebar_layer_skips_empty_tabs() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.sidebar_layer = SidebarLayer::Live;
    // No agents, no projects, no graphs → cycle may stay or move
    app.cycle_sidebar_layer();
    // Should not panic
}

#[test]
fn selected_project_out_of_bounds() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(app.selected_project().is_none());
}

#[test]
fn selected_graph_none_when_no_graphs() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(app.selected_graph().is_none());
}

#[test]
fn selected_graph_spec_none_when_no_details() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(app.selected_graph_spec().is_none());
}

#[test]
fn selected_graph_node_none_when_no_details() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(app.selected_graph_node().is_none());
}

#[test]
fn visible_graphs_returns_all_graphs() {
    use crate::domain::graphs::GraphStatus;
    let db = test_db();
    db.insert_graph(&make_graph("l1", "A", GraphStatus::Running))
        .unwrap();
    db.insert_graph(&make_graph("l2", "B", GraphStatus::Completed))
        .unwrap();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert_eq!(app.visible_graphs().len(), 2);
}

#[test]
fn selected_agent_none_when_empty() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(app.selected_agent().is_none());
}

#[test]
fn selected_id_empty_when_no_agent() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert_eq!(app.selected_id(), "—");
}

#[test]
fn focused_agent_name_empty_when_no_agent() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(app.focused_agent_name().is_empty());
}

#[test]
fn graph_live_highlighted_node_id_none_when_no_live_state() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(app.graph_live_highlighted_node_id().is_none());
}

#[test]
fn graph_live_reset_follow_clears_selection() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.graph_live_follow = false;
    app.graph_live_selected_node = Some("some-node".to_string());
    app.graph_live_reset_follow();
    assert!(app.graph_live_follow);
    assert!(app.graph_live_selected_node.is_none());
}

#[test]
fn graph_live_highlighted_node_run_info_default_when_no_live_state() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let info = app.graph_live_highlighted_node_run_info();
    assert!(info.status.is_none());
}

#[test]
fn graph_spec_strip_move_selection_cycles_through_queue_and_wraps() {
    let mut app = app_with_spec_queue(&["s1", "s2", "s3"]);
    assert!(app.graph_spec_strip_selected.is_none());

    app.graph_spec_strip_move_selection(true);
    assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s1"));
    app.graph_spec_strip_move_selection(true);
    assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s2"));
    app.graph_spec_strip_move_selection(true);
    assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s3"));
    // Wraps back to the first spec.
    app.graph_spec_strip_move_selection(true);
    assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s1"));

    // Backward wraps the other way.
    app.graph_spec_strip_move_selection(false);
    assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s3"));

    // Selecting a spec by hand (the mouse click path) and then moving by
    // keyboard from that point lands on the same spec a second keyboard
    // move would — click and keyboard share one selection.
    app.graph_spec_strip_select("s2".to_string());
    assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s2"));
    app.graph_spec_strip_move_selection(true);
    assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s3"));
}

#[test]
fn graph_spec_strip_move_selection_never_touches_graph_follow() {
    let mut app = app_with_spec_queue(&["s1", "s2"]);
    assert!(app.graph_live_follow);
    app.graph_spec_strip_move_selection(true);
    assert!(
        app.graph_live_follow,
        "selecting a spec in the strip must not disturb the graph's own follow state"
    );
}

#[test]
fn graph_spec_strip_select_ignores_unknown_spec_id() {
    let mut app = app_with_spec_queue(&["s1", "s2"]);
    app.graph_spec_strip_select("does-not-exist".to_string());
    assert!(app.graph_spec_strip_selected.is_none());
}

#[test]
fn graph_spec_strip_select_focuses_the_strip() {
    let mut app = app_with_spec_queue(&["s1", "s2"]);
    assert_eq!(app.graph_live_focus, GraphLiveFocus::Graph);
    app.graph_spec_strip_select("s1".to_string());
    assert_eq!(app.graph_live_focus, GraphLiveFocus::SpecStrip);
}

#[test]
fn graph_live_toggle_focus_toggles_between_graph_and_spec_strip() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert_eq!(app.graph_live_focus, GraphLiveFocus::Graph);
    app.graph_live_toggle_focus();
    assert_eq!(app.graph_live_focus, GraphLiveFocus::SpecStrip);
    app.graph_live_toggle_focus();
    assert_eq!(app.graph_live_focus, GraphLiveFocus::Graph);
}

#[test]
fn cancel_graph_editor_dialog() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.graph_editor_dialog = Some(crate::tui::app::types::GraphEditorDialog::new(
        "n1".into(),
        "node1".into(),
        "title".into(),
        "help".into(),
        "buffer".into(),
        crate::tui::app::types::GraphEditorMode::AgentPrompt,
    ));
    app.cancel_graph_editor_dialog();
    assert!(app.graph_editor_dialog.is_none());
    assert!(matches!(app.focus, Focus::Preview));
}

#[test]
fn toggle_rag_pause_flips() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(!app.rag_paused);
    app.toggle_rag_pause();
    assert!(app.rag_paused);
    app.toggle_rag_pause();
    assert!(!app.rag_paused);
}

#[test]
fn normalize_selected_knowledge_clamps_to_first_filtered() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.project_knowledge = vec![
        crate::db::intelligence::IntelligenceNodeRecord {
            id: "n1".into(),
            kind: "fact".into(),
            status: "noted".into(),
            title: "Alpha".into(),
            body: "body".into(),
            metadata: None,
            project_hash: None,
            session_id: None,
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        },
        crate::db::intelligence::IntelligenceNodeRecord {
            id: "n2".into(),
            kind: "fact".into(),
            status: "noted".into(),
            title: "Beta".into(),
            body: "body".into(),
            metadata: None,
            project_hash: None,
            session_id: None,
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        },
    ];
    app.selected_knowledge = 5; // out of range
    app.knowledge_filter = "alpha".to_string();
    app.normalize_selected_knowledge();
    assert_eq!(app.selected_knowledge, 0);
}

#[test]
fn enter_exit_knowledge_filter_mode() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(!app.knowledge_filter_mode);
    app.enter_knowledge_filter_mode();
    assert!(app.knowledge_filter_mode);
    app.exit_knowledge_filter_mode();
    assert!(!app.knowledge_filter_mode);
}

#[test]
fn dismiss_copied_recent_does_not_clear() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.show_copied = true;
    app.copied_at = std::time::Instant::now();
    app.dismiss_copied();
    assert!(app.show_copied);
}

#[test]
fn dismiss_copied_old_enough_clears() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.show_copied = true;
    app.copied_at = std::time::Instant::now() - std::time::Duration::from_secs(5);
    app.dismiss_copied();
    assert!(!app.show_copied);
}

#[test]
fn open_graph_edges_dialog_lists_the_nodes_outgoing_plain_edges() {
    let db = test_db();
    seed_plain_edge_graph(&db);
    let data_dir = tempdir().expect("create data dir");
    let mut app = app_on_plain_node(&db, data_dir.path(), 0);

    app.open_graph_edges_dialog().expect("open edges dialog");

    let dialog = app.graph_editor_dialog.as_ref().expect("dialog opens");
    assert!(matches!(
        dialog.mode,
        crate::tui::app::types::GraphEditorMode::Edges
    ));
    assert_eq!(dialog.edge_rows.len(), 1);
    assert_eq!(dialog.edge_rows[0].to_node, "node_b");
    assert!(
        !dialog.edge_targets.iter().any(|(id, _)| id == "node_a"),
        "candidate targets exclude the node itself"
    );
}

#[test]
fn retarget_focused_graph_edge_changes_only_the_destination() {
    let db = test_db();
    seed_plain_edge_graph(&db);
    let data_dir = tempdir().expect("create data dir");
    let mut app = app_on_plain_node(&db, data_dir.path(), 0);
    app.open_graph_edges_dialog().expect("open edges dialog");

    app.retarget_focused_graph_edge(true).expect("retarget");

    let dialog = app.graph_editor_dialog.as_ref().expect("dialog still open");
    assert!(dialog.parse_error.is_none(), "{:?}", dialog.parse_error);
    assert_eq!(dialog.edge_rows[0].to_node, "node_c");
    assert_eq!(
        dialog.edge_rows[0].condition,
        crate::domain::graphs::GraphEdgeCondition::Pass,
        "retargeting never touches the edge's condition"
    );

    let edge = db.get_graph_edge("e_ab").unwrap().unwrap();
    assert_eq!(edge.to_node, "node_c");
    assert_eq!(edge.from_node, "node_a");
}

#[test]
fn delete_focused_graph_edge_removes_it_from_the_db_and_the_dialog() {
    let db = test_db();
    seed_plain_edge_graph(&db);
    let data_dir = tempdir().expect("create data dir");
    let mut app = app_on_plain_node(&db, data_dir.path(), 0);
    app.open_graph_edges_dialog().expect("open edges dialog");

    app.delete_focused_graph_edge().expect("delete edge");

    let dialog = app.graph_editor_dialog.as_ref().expect("dialog still open");
    assert!(dialog.edge_rows.is_empty());
    assert!(db.get_graph_edge("e_ab").unwrap().is_none());
}

#[test]
fn retarget_focused_graph_edge_surfaces_the_running_graph_rejection() {
    let db = test_db();
    seed_plain_edge_graph(&db);
    db.update_graph_status(
        "plp1",
        crate::domain::graphs::GraphStatus::Running,
        None,
        None,
    )
    .unwrap();
    let data_dir = tempdir().expect("create data dir");
    let mut app = app_on_plain_node(&db, data_dir.path(), 0);
    app.open_graph_edges_dialog().expect("open edges dialog");

    app.retarget_focused_graph_edge(true)
        .expect("call returns Ok");

    let dialog = app.graph_editor_dialog.as_ref().expect("dialog still open");
    let error = dialog.parse_error.as_deref().unwrap_or_default();
    assert!(error.contains("running"), "{error}");
    // Target unchanged in the db.
    assert_eq!(
        db.get_graph_edge("e_ab").unwrap().unwrap().to_node,
        "node_b"
    );
}

#[test]
fn open_graph_editor_dialog_prefills_router_routes_fallback_and_existing_wiring() {
    let db = test_db();
    seed_router_graph(&db);
    let data_dir = tempdir().expect("create data dir");
    let mut app = app_on_router_node(&db, data_dir.path());

    app.open_graph_editor_dialog().expect("open editor");

    let dialog = app.graph_editor_dialog.as_ref().expect("dialog opens");
    assert!(matches!(
        dialog.mode,
        crate::tui::app::types::GraphEditorMode::RouterRoutes
    ));
    assert_eq!(dialog.router_routes.len(), 4);
    assert_eq!(dialog.router_fallback, "escalation");

    let billing = dialog
        .router_routes
        .iter()
        .find(|r| r.label == "billing")
        .expect("billing route present");
    assert_eq!(billing.description, "billing desc");
    assert_eq!(billing.target_node_id.as_deref(), Some("billing_agent"));

    let technical = dialog
        .router_routes
        .iter()
        .find(|r| r.label == "technical")
        .expect("technical route present");
    assert_eq!(
        technical.target_node_id, None,
        "not yet wired in the fixture"
    );

    // Candidate targets exclude the router itself.
    assert!(!dialog.router_targets.iter().any(|(id, _)| id == "router"));
    assert!(dialog
        .router_targets
        .iter()
        .any(|(id, _)| id == "technical_agent"));
}

#[test]
fn save_router_routes_dialog_rejects_an_invalid_fallback_with_a_readable_message() {
    let db = test_db();
    seed_router_graph(&db);
    let data_dir = tempdir().expect("create data dir");
    let mut app = app_on_router_node(&db, data_dir.path());
    app.open_graph_editor_dialog().expect("open editor");

    app.graph_editor_dialog.as_mut().unwrap().router_fallback = "not-a-route".to_string();

    let result = app.save_graph_editor_dialog();
    assert!(result.is_err(), "invalid fallback must not save");

    let dialog = app
        .graph_editor_dialog
        .as_ref()
        .expect("dialog stays open on validation failure");
    let message = dialog.parse_error.as_deref().unwrap_or_default();
    assert!(
        message.contains("not-a-route"),
        "error should be a readable, specific message: {message:?}"
    );

    // Nothing was written.
    let node = db.get_graph_node("router").unwrap().unwrap();
    assert_eq!(node.config["fallback"], "escalation");
}

#[test]
fn save_router_routes_dialog_rejects_partial_wiring_before_writing_anything() {
    let db = test_db();
    seed_router_graph(&db);
    let data_dir = tempdir().expect("create data dir");
    let mut app = app_on_router_node(&db, data_dir.path());
    app.open_graph_editor_dialog().expect("open editor");

    // "billing" is already wired (from the fixture); leaving every other
    // route unwired is a rejected half-wired state, not a valid save.
    let result = app.save_graph_editor_dialog();
    assert!(result.is_err());
    assert!(app
        .graph_editor_dialog
        .as_ref()
        .unwrap()
        .parse_error
        .is_some());

    // The pre-existing edge is untouched.
    let edges = db.list_graph_edges("rs1").unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].to_node, "billing_agent");
}

#[test]
fn save_router_routes_dialog_persists_config_and_wires_every_route() {
    let db = test_db();
    seed_router_graph(&db);
    let data_dir = tempdir().expect("create data dir");
    let mut app = app_on_router_node(&db, data_dir.path());
    app.open_graph_editor_dialog().expect("open editor");

    {
        let dialog = app.graph_editor_dialog.as_mut().unwrap();
        for route in dialog.router_routes.iter_mut() {
            route.target_node_id = Some(match route.label.as_str() {
                "billing" => "billing_agent".to_string(),
                "technical" => "technical_agent".to_string(),
                "sales" => "sales_agent".to_string(),
                "escalation" => "billing_agent".to_string(),
                other => panic!("unexpected route {other}"),
            });
        }
    }

    app.save_graph_editor_dialog().expect("save succeeds");

    assert!(app.graph_editor_dialog.is_none(), "dialog closes on save");

    let node = db.get_graph_node("router").unwrap().unwrap();
    assert_eq!(node.config["fallback"], "escalation");
    assert_eq!(node.config["routes"].as_array().unwrap().len(), 4);

    let edges = db.list_graph_edges("rs1").unwrap();
    assert_eq!(edges.len(), 4, "every route now has exactly one edge");
    let by_route: HashMap<&str, &str> = edges
        .iter()
        .map(|e| {
            (
                e.condition.route_label().expect("route edge"),
                e.to_node.as_str(),
            )
        })
        .collect();
    assert_eq!(by_route["billing"], "billing_agent");
    assert_eq!(by_route["technical"], "technical_agent");
    assert_eq!(by_route["sales"], "sales_agent");
    assert_eq!(by_route["escalation"], "billing_agent");
}

#[test]
fn save_router_routes_dialog_drops_the_edge_of_a_removed_route() {
    let db = test_db();
    seed_router_graph(&db);
    let data_dir = tempdir().expect("create data dir");
    let mut app = app_on_router_node(&db, data_dir.path());
    app.open_graph_editor_dialog().expect("open editor");

    {
        let dialog = app.graph_editor_dialog.as_mut().unwrap();
        // Drop "billing" — the one route the fixture already wired.
        let idx = dialog
            .router_routes
            .iter()
            .position(|r| r.label == "billing")
            .unwrap();
        dialog.router_route_index = idx;
        dialog.router_remove_route();
        // The remaining 3 routes must all be wired for this to be a
        // valid, non-partial save.
        for route in dialog.router_routes.iter_mut() {
            route.target_node_id = Some("technical_agent".to_string());
        }
        dialog.router_fallback = "escalation".to_string();
    }

    app.save_graph_editor_dialog().expect("save succeeds");

    let edges = db.list_graph_edges("rs1").unwrap();
    assert_eq!(edges.len(), 3);
    assert!(
        !edges
            .iter()
            .any(|e| e.condition.route_label() == Some("billing")),
        "the removed route's edge must not survive the save"
    );
}

/// `NodeConfig` mode is the one TUI path that hand-writes a node's raw
/// config JSON (a `check` node here — agent nodes go through the
/// structured `AgentPrompt` mode instead), so it's the one that can
/// reintroduce the exact incident shape (a config key the engine will
/// never read) if left unvalidated. Saving must be rejected the same
/// way `graph_add_node`/`graph_update_node` reject it, and the bad config
/// must never reach the DB.
#[test]
fn save_graph_editor_dialog_rejects_unknown_config_key_in_node_config_mode() {
    use crate::domain::graphs::{Graph, GraphNode, GraphNodeKind, GraphSpec};

    let db = test_db();
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "clp1".to_string(),
        name: "check graph".to_string(),
        description: None,
        workdir: "/tmp/check-test".to_string(),
        status: GraphStatus::Draft,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    })
    .unwrap();
    db.insert_graph_spec(&GraphSpec {
        id: "cs1".to_string(),
        graph_id: Some("clp1".to_string()),
        name: "spec one".to_string(),
        description: None,
        position: 1,
        parallelizable: false,
        status: GraphSpecStatus::Pending,
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
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "check1".to_string(),
        spec_id: Some("cs1".to_string()),
        graph_id: None,
        name: "Gate".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 0,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.refresh_graphs().expect("refresh graphs");
    app.graph_selected_spec = 0;
    app.graph_selected_node = 0;
    assert_eq!(
        app.selected_graph_node().map(|n| n.id.as_str()),
        Some("check1"),
        "fixture invariant: check node must be selected"
    );

    app.open_graph_editor_dialog().expect("open editor");
    assert!(matches!(
        app.graph_editor_dialog.as_ref().unwrap().mode,
        crate::tui::app::types::GraphEditorMode::NodeConfig
    ));
    app.graph_editor_dialog.as_mut().unwrap().buffer =
        serde_json::json!({"command": "true", "unexpected_field": true}).to_string();

    let result = app.save_graph_editor_dialog();
    assert!(result.is_err(), "an unrecognized config key must not save");

    let dialog = app
        .graph_editor_dialog
        .as_ref()
        .expect("dialog reopens with the error visible");
    assert!(
        dialog
            .parse_error
            .as_deref()
            .unwrap_or_default()
            .contains("unexpected_field"),
        "{:?}",
        dialog.parse_error
    );

    let stored = db.get_graph_node("check1").unwrap().unwrap();
    assert_eq!(
        stored.config,
        serde_json::json!({"command": "true"}),
        "the invalid config must never reach the DB"
    );
}

#[test]
fn graph_live_view_scroll_step_clamps_at_zero() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.graph_live_view_scroll = 0;
    app.graph_live_view_total_lines = 50;
    app.last_panel_inner = (80, 20);
    app.graph_live_view_scroll_step(-10);
    assert_eq!(app.graph_live_view_scroll, 0, "scroll must not go below 0");
}

#[test]
fn graph_live_view_scroll_step_clamps_at_max() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.graph_live_view_total_lines = 50;
    app.last_panel_inner = (80, 20);
    app.graph_live_view_scroll = 30;
    app.graph_live_view_scroll_step(10);
    assert_eq!(
        app.graph_live_view_scroll, 30,
        "scroll must not exceed total_lines - height (30)"
    );
    app.graph_live_view_scroll = 10;
    app.graph_live_view_scroll_step(5);
    assert_eq!(app.graph_live_view_scroll, 15);
    app.graph_live_view_scroll_step(100);
    assert_eq!(app.graph_live_view_scroll, 30);
}

#[test]
fn navigate_child_follows_pass_edge() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let state = graph_state(
        &[("a", "A"), ("b", "B"), ("c", "C")],
        &[
            ("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass),
            ("a", "c", crate::domain::graphs::GraphEdgeCondition::Fail),
        ],
        Some("a"),
    );
    app.graph_live_state = Some(state);
    app.graph_live_follow = true; // starts auto-following at a
                                  // child should follow pass edge to b
    app.graph_live_navigate_child();
    assert_eq!(app.graph_live_highlighted_node_id(), Some("b"));
    assert!(!app.graph_live_follow);
}
