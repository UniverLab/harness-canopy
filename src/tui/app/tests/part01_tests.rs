//! Part 01 of `tests` (split from `src/tui/app/mod.rs`, pure move).
use super::*;

#[test]
fn reconcile_bridge_sessions_reaps_dead_pid_but_leaves_live_bridge_active() {
    let db = test_db();
    db.insert_interactive_session(
        "dead-bridge",
        "standalone",
        "bridge",
        "/tmp",
        Some("canopy bridge"),
        Some(999_999_999),
        "bridge",
        None,
    )
    .unwrap();
    db.insert_interactive_session(
        "live-bridge",
        "standalone",
        "bridge",
        "/tmp",
        Some("canopy bridge"),
        Some(std::process::id() as i64),
        "bridge",
        None,
    )
    .unwrap();

    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.reconcile_bridge_sessions();

    let still_active = db.get_active_sessions_by_type("bridge").unwrap();
    assert_eq!(still_active.len(), 1);
    assert_eq!(still_active[0].id, "live-bridge");
}

#[test]
fn test_yolo_mode_preservation_in_session_relaunch() {
    let session = InteractiveSession {
        id: "test-session".to_string(),
        name: "test-session".to_string(),
        cli: "opencode".to_string(),
        working_dir: "/tmp".to_string(),
        args: Some("--tui --yolo".to_string()),
        started_at: "2023-01-01T00:00:00Z".to_string(),
        status: "active".to_string(),
        session_type: "interactive".to_string(),
        pid: None,
        boot_id: None,
        harness_conversation_id: None,
    };

    assert!(
        build_resumed_session_args(session.args.as_deref(), None, None, None, Some("--yolo"))
            .as_deref()
            .is_some_and(|args| args.contains("--yolo"))
    );
}

#[test]
fn test_yolo_flag_not_duplicated_when_falling_back_to_original_args() {
    let session = InteractiveSession {
        id: "test-session".to_string(),
        name: "test-session".to_string(),
        cli: "opencode".to_string(),
        working_dir: "/tmp".to_string(),
        args: Some("--tui --yolo".to_string()),
        started_at: "2023-01-01T00:00:00Z".to_string(),
        status: "active".to_string(),
        session_type: "interactive".to_string(),
        pid: None,
        boot_id: None,
        harness_conversation_id: None,
    };

    let args =
        build_resumed_session_args(session.args.as_deref(), None, None, None, Some("--yolo"))
            .unwrap();
    assert_eq!(args.matches("--yolo").count(), 1);
}

#[test]
fn test_original_resume_args_preserved_over_reconstructed_args() {
    let session = InteractiveSession {
        id: "test-session".to_string(),
        name: "test-session".to_string(),
        cli: "opencode".to_string(),
        working_dir: "/tmp".to_string(),
        args: Some("--session abc123 --yolo".to_string()),
        started_at: "2023-01-01T00:00:00Z".to_string(),
        status: "active".to_string(),
        session_type: "interactive".to_string(),
        pid: None,
        boot_id: None,
        harness_conversation_id: None,
    };

    let args = build_resumed_session_args(
        session.args.as_deref(),
        Some("--chat"),
        Some("-c"),
        Some("--session"),
        Some("--yolo"),
    )
    .unwrap();
    assert!(args.contains("--session abc123"));
    assert!(args.contains("--yolo"));
    assert!(!args.contains("-c"));
}

#[test]
fn test_rebuilds_resume_args_for_fresh_session() {
    let session = InteractiveSession {
        id: "test-session".to_string(),
        name: "test-session".to_string(),
        cli: "copilot".to_string(),
        working_dir: "/tmp".to_string(),
        args: None,
        started_at: "2023-01-01T00:00:00Z".to_string(),
        status: "active".to_string(),
        session_type: "interactive".to_string(),
        pid: None,
        boot_id: None,
        harness_conversation_id: None,
    };

    let args = build_resumed_session_args(
        session.args.as_deref(),
        None,
        Some("--continue"),
        None,
        Some("--yolo"),
    )
    .unwrap();
    assert!(args.contains("--continue"));
    assert!(!args.contains("--yolo"));
}

#[test]
fn test_appends_resume_args_to_original_interactive_command() {
    let session = InteractiveSession {
        id: "test-session".to_string(),
        name: "test-session".to_string(),
        cli: "kiro".to_string(),
        working_dir: "/tmp".to_string(),
        args: Some("chat --trust-all-tools".to_string()),
        started_at: "2023-01-01T00:00:00Z".to_string(),
        status: "active".to_string(),
        session_type: "interactive".to_string(),
        pid: None,
        boot_id: None,
        harness_conversation_id: None,
    };

    let args = build_resumed_session_args(
        session.args.as_deref(),
        Some("chat"),
        Some("--resume-picker"),
        None,
        Some("--trust-all-tools"),
    )
    .unwrap();
    assert!(args.contains("chat"));
    assert!(args.contains("--resume-picker"));
    assert_eq!(args.matches("--trust-all-tools").count(), 1);
}

#[test]
fn sidebar_graphs_orders_by_recency_across_all_statuses() {
    use crate::domain::graphs::GraphStatus;

    let db = test_db();
    let base = chrono::Utc::now() - chrono::Duration::hours(10);

    let mut draft = make_graph("l-draft", "Draft Graph", GraphStatus::Draft);
    draft.created_at = base;
    db.insert_graph(&draft).unwrap();

    let mut failed = make_graph("l-failed", "Failed Graph", GraphStatus::Failed);
    failed.created_at = base + chrono::Duration::minutes(10);
    db.insert_graph(&failed).unwrap();

    let mut done = make_graph("l-done", "Done Graph", GraphStatus::Completed);
    done.created_at = base + chrono::Duration::minutes(20);
    db.insert_graph(&done).unwrap();

    let mut paused = make_graph("l-paused", "Paused Graph", GraphStatus::Paused);
    paused.created_at = base + chrono::Duration::minutes(30);
    db.insert_graph(&paused).unwrap();

    let mut running = make_graph("l-running", "Running Graph", GraphStatus::Running);
    running.created_at = base + chrono::Duration::minutes(40);
    db.insert_graph(&running).unwrap();

    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");

    let ids: Vec<&str> = app
        .sidebar_graphs()
        .iter()
        .map(|lp| lp.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["l-running", "l-paused", "l-done", "l-failed", "l-draft"],
        "every graph must be listed regardless of status, ordered by last \
             activity (created_at, since none of these have run) most recent first"
    );
}

#[test]
fn refresh_backlog_specs_filters_by_selected_project_workdir() {
    let db = test_db();
    db.upsert_project(&make_project("hash0", "/tmp/proj0"))
        .unwrap();
    db.upsert_project(&make_project("hash1", "/tmp/proj1"))
        .unwrap();
    db.insert_graph_spec(&make_backlog_spec("spec-a", "Spec A", Some("/tmp/proj0")))
        .unwrap();
    db.insert_graph_spec(&make_backlog_spec("spec-b", "Spec B", Some("/tmp/proj1")))
        .unwrap();
    db.insert_graph_spec(&make_backlog_spec("spec-c", "Spec C", None))
        .unwrap();

    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    // Projects are ranked by recency with second-resolution timestamps, so
    // two upserts may land in either order: select by hash, never by index.
    let index_of = |app: &App, hash: &str| {
        app.projects
            .iter()
            .position(|p| p.hash == hash)
            .expect("project listed")
    };

    app.selected_project = index_of(&app, "hash0");
    app.refresh_backlog_specs().expect("refresh backlog");
    assert_eq!(
        app.backlog_specs
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>(),
        vec!["Spec A".to_string()],
        "backlog should be tag-filtered to the selected project's workdir"
    );

    app.selected_project = index_of(&app, "hash1");
    app.refresh_backlog_specs().expect("refresh backlog");
    assert_eq!(
        app.backlog_specs
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>(),
        vec!["Spec B".to_string()]
    );
}

#[test]
fn entering_a_project_defaults_to_overview_and_history_lazily_loads() {
    let db = test_db();
    db.upsert_project(&make_project("hash0", "/tmp/proj0"))
        .unwrap();

    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert!(app.project_focus.is_none(), "starts on Preview, not Focus");

    app.enter_project_focus(ProjectTab::Overview);
    assert_eq!(app.project_focus, Some(ProjectTab::Overview));

    app.open_project_tab(ProjectTab::History);
    assert_eq!(app.project_focus, Some(ProjectTab::History));
    assert!(
        app.project_history_cache.contains_key("hash0"),
        "History tab lazily loads persisted data on first show"
    );

    app.exit_project_focus();
    assert!(app.project_focus.is_none());
}

#[test]
fn cycle_project_tab_wraps_through_all_four_tabs() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.enter_project_focus(ProjectTab::Overview);

    app.cycle_project_tab(true);
    assert_eq!(app.project_focus, Some(ProjectTab::Backlog));
    app.cycle_project_tab(true);
    assert_eq!(app.project_focus, Some(ProjectTab::Knowledge));
    app.cycle_project_tab(true);
    assert_eq!(app.project_focus, Some(ProjectTab::History));
    app.cycle_project_tab(true);
    assert_eq!(app.project_focus, Some(ProjectTab::Overview));

    app.cycle_project_tab(false);
    assert_eq!(app.project_focus, Some(ProjectTab::History));
}

#[test]
fn select_prev_on_first_live_item_does_not_select_automation() {
    // Regression: standing on the first Live item and pressing "up"
    // used to fall through to the last Automation entry (the old
    // flat-ring `cross_layer` behavior, back when background agents
    // lived in Live before tabs existed). Arrows must now stay inside
    // the active tab — with a populated Automation tab present, "up"
    // must not land there.
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
    app.automation_kind = AutomationKind::Agent;
    app.selected = 0;

    app.select_prev();

    assert_eq!(app.sidebar_layer, SidebarLayer::Live);
    assert_ne!(
        app.selected, 2,
        "must not land on the Automation agent entry"
    );
}

#[test]
fn navigate_live_wraps_at_both_ends() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.agents = vec![AgentEntry::Group(0), AgentEntry::Group(1)];
    app.sidebar_layer = SidebarLayer::Live;
    app.selected = 0;

    // Backward off the first item wraps to the last (no RAG activity to
    // divert to).
    app.select_prev();
    assert_eq!(app.sidebar_layer, SidebarLayer::Live);
    assert_eq!(app.selected, 1);

    // Forward off the last item wraps back to the first.
    app.select_next();
    assert_eq!(app.sidebar_layer, SidebarLayer::Live);
    assert_eq!(app.selected, 0);
}

#[test]
fn navigate_automation_wraps_at_both_ends() {
    use crate::domain::graphs::GraphStatus;

    let db = test_db();
    db.insert_graph(&make_graph(
        "l-active",
        "Active Graph",
        GraphStatus::Running,
    ))
    .unwrap();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.agents = vec![AgentEntry::Agent(bg_agent("bg-1"))];
    app.sidebar_layer = SidebarLayer::Automation;
    app.automation_kind = AutomationKind::Agent;
    app.selected = 0;

    // Backward off the first entry (the agent) wraps to the last (the
    // graph) instead of leaving Automation.
    app.select_prev();
    assert_eq!(app.sidebar_layer, SidebarLayer::Automation);
    assert_eq!(app.automation_kind, AutomationKind::Graph);
    assert_eq!(app.selected_graph_id.as_deref(), Some("l-active"));

    // Forward off the last entry (the graph) wraps back to the first
    // (the agent).
    app.select_next();
    assert_eq!(app.sidebar_layer, SidebarLayer::Automation);
    assert_eq!(app.automation_kind, AutomationKind::Agent);
    assert_eq!(app.selected, 0);
}

#[test]
fn navigate_projects_wraps_at_both_ends() {
    let db = test_db();
    db.upsert_project(&make_project("hash0", "/tmp/proj0"))
        .unwrap();
    db.upsert_project(&make_project("hash1", "/tmp/proj1"))
        .unwrap();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.sidebar_layer = SidebarLayer::Knowledge;
    app.selected_project = 0;

    // Backward off the first project wraps to the last.
    app.select_prev();
    assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);
    assert_eq!(app.selected_project, 1);

    // Forward off the last project wraps back to the first.
    app.select_next();
    assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);
    assert_eq!(app.selected_project, 0);
}

#[test]
fn rag_reachable_upward_from_live_first_item_returns_to_live() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.agents = vec![AgentEntry::Group(0), AgentEntry::Group(1)];
    app.sidebar_layer = SidebarLayer::Live;
    app.selected = 0;
    app.rag_info = crate::db::project::RagInfoSummary {
        total_chunks: 5,
        ..Default::default()
    };

    app.select_prev();
    assert!(app.agents_rag_focused);
    assert_eq!(
        app.sidebar_layer,
        SidebarLayer::Live,
        "entering RAG focus must not change the active tab"
    );

    app.select_next();
    assert!(!app.agents_rag_focused);
    assert_eq!(app.sidebar_layer, SidebarLayer::Live);
    assert_eq!(
        app.selected, 0,
        "leaving RAG lands on the first item of the tab it came from"
    );
}

#[test]
fn rag_reachable_upward_from_automation_first_item_returns_to_automation() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.agents = vec![AgentEntry::Agent(bg_agent("bg-1"))];
    app.sidebar_layer = SidebarLayer::Automation;
    app.automation_kind = AutomationKind::Agent;
    app.selected = 0;
    app.rag_info = crate::db::project::RagInfoSummary {
        total_chunks: 5,
        ..Default::default()
    };

    app.select_prev();
    assert!(app.agents_rag_focused);
    assert_eq!(app.sidebar_layer, SidebarLayer::Automation);

    app.select_next();
    assert!(!app.agents_rag_focused);
    assert_eq!(app.sidebar_layer, SidebarLayer::Automation);
    assert_eq!(app.automation_kind, AutomationKind::Agent);
    assert_eq!(app.selected, 0);
}

#[test]
fn rag_reachable_upward_from_knowledge_first_item_returns_to_knowledge() {
    let db = test_db();
    db.upsert_project(&make_project("hash0", "/tmp/proj0"))
        .unwrap();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.sidebar_layer = SidebarLayer::Knowledge;
    app.selected_project = 0;
    app.rag_info = crate::db::project::RagInfoSummary {
        total_chunks: 5,
        ..Default::default()
    };

    app.select_prev();
    assert!(app.agents_rag_focused);
    assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);

    app.select_next();
    assert!(!app.agents_rag_focused);
    assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);
    assert_eq!(app.selected_project, 0);
}

#[test]
fn switch_sidebar_tab_lands_on_an_empty_tab_instead_of_refusing() {
    // Unlike `cycle_sidebar_layer` (which skips empty layers so
    // keyboard cycling never lands somewhere with nothing to select), a
    // deliberate mouse click on a tab must always switch to it — even
    // Automation with nothing running, so its empty state is reachable.
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    assert_eq!(app.sidebar_layer, SidebarLayer::Live);

    app.switch_sidebar_tab(SidebarLayer::Automation);

    assert_eq!(app.sidebar_layer, SidebarLayer::Automation);
}

#[test]
fn switch_sidebar_tab_selects_first_item_when_the_tab_has_content() {
    let db = test_db();
    db.upsert_project(&make_project("hash0", "/tmp/proj0"))
        .unwrap();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");

    app.switch_sidebar_tab(SidebarLayer::Knowledge);

    assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);
    assert_eq!(app.selected_project, 0);
}

// ── Pure helper tests ────────────────────────────────────────

#[test]
fn calculate_log_hash_empty_string() {
    assert_eq!(calculate_log_hash(""), 0);
}

#[test]
fn calculate_log_hash_deterministic() {
    let h1 = calculate_log_hash("hello world");
    let h2 = calculate_log_hash("hello world");
    assert_eq!(h1, h2);
}

#[test]
fn calculate_log_hash_different_inputs() {
    let h1 = calculate_log_hash("hello");
    let h2 = calculate_log_hash("world");
    assert_ne!(h1, h2);
}

#[test]
fn log_contains_error_positive() {
    assert!(log_contains_error("ERROR: something broke"));
    assert!(log_contains_error("FAILED to connect"));
    assert!(log_contains_error("EXCEPTION thrown"));
    assert!(log_contains_error("PANIC in module"));
    assert!(log_contains_error("SEGFAULT detected"));
    assert!(log_contains_error("TIMED OUT after 30s"));
    assert!(log_contains_error("CONNECTION REFUSED"));
    assert!(log_contains_error("PERMISSION DENIED"));
    assert!(log_contains_error("HALTED unexpectedly"));
    assert!(log_contains_error("PROBLEMA detectado"));
    assert!(log_contains_error("FALLO en el sistema"));
    assert!(log_contains_error("FALLANDO test"));
}

#[test]
fn log_contains_error_negative() {
    assert!(!log_contains_error("everything is fine"));
    assert!(!log_contains_error("SUCCESS all done"));
    assert!(!log_contains_error(""));
}

#[test]
fn log_contains_success_positive() {
    assert!(log_contains_success("SUCCESS"));
    assert!(log_contains_success("ALL TESTS PASSED"));
    assert!(log_contains_success("BUILD SUCCEEDED"));
    assert!(log_contains_success("FINISHED task"));
    assert!(log_contains_success("COMPLETED"));
    assert!(log_contains_success("DONE."));
    assert!(log_contains_success("STABILIZED"));
    assert!(log_contains_success("READY to deploy"));
    assert!(log_contains_success("CONVERGED"));
    assert!(log_contains_success("DEPLOYED to prod"));
    assert!(log_contains_success("EXCELENTE resultado"));
    assert!(log_contains_success("COMPLETADO"));
    assert!(log_contains_success("HECHO"));
    assert!(log_contains_success("LISTO"));
    assert!(log_contains_success("TERMINADO"));
}

#[test]
fn log_contains_success_negative() {
    assert!(!log_contains_success("ERROR: failed"));
    assert!(!log_contains_success("running tests..."));
    assert!(!log_contains_success(""));
}

#[test]
fn log_contains_spawn_positive() {
    assert!(log_contains_spawn("SPAWNING agent"));
    assert!(log_contains_spawn("STARTING UP server"));
    assert!(log_contains_spawn("BOOTSTRAPPING cluster"));
    assert!(log_contains_spawn("INITIALIZING module"));
}

#[test]
fn log_contains_spawn_negative() {
    assert!(!log_contains_spawn("agent stopped"));
    assert!(!log_contains_spawn("DONE"));
    assert!(!log_contains_spawn(""));
}

#[test]
fn lerp_f32_midpoint() {
    assert!((lerp_f32(0.0, 10.0, 0.5) - 5.0).abs() < f32::EPSILON);
}

#[test]
fn lerp_f32_endpoints() {
    assert!((lerp_f32(0.0, 10.0, 0.0) - 0.0).abs() < f32::EPSILON);
    assert!((lerp_f32(0.0, 10.0, 1.0) - 10.0).abs() < f32::EPSILON);
}

#[test]
fn lerp_f32_negative() {
    assert!((lerp_f32(10.0, 0.0, 0.5) - 5.0).abs() < f32::EPSILON);
}

#[test]
fn lerp_u64_midpoint() {
    assert_eq!(lerp_u64(0, 100, 0.5), 50);
}

#[test]
fn lerp_u64_endpoints() {
    assert_eq!(lerp_u64(0, 100, 0.0), 0);
    assert_eq!(lerp_u64(0, 100, 1.0), 100);
}

#[test]
fn blend_optional_f32_both_present() {
    assert_eq!(blend_optional_f32(Some(0.0), Some(10.0), 0.5), Some(5.0));
}

#[test]
fn blend_optional_f32_first_none() {
    assert_eq!(blend_optional_f32(None, Some(10.0), 0.5), Some(10.0));
}

#[test]
fn blend_optional_f32_second_none() {
    assert_eq!(blend_optional_f32(Some(0.0), None, 0.5), None);
}

#[test]
fn blend_optional_f32_both_none() {
    assert_eq!(blend_optional_f32(None, None, 0.5), None);
}

#[test]
fn blend_optional_f64_both_present() {
    let result = blend_optional_f64(Some(0.0), Some(10.0), 0.5);
    assert!((result.unwrap() - 5.0).abs() < 0.01);
}

#[test]
fn blend_optional_f64_first_none() {
    assert_eq!(blend_optional_f64(None, Some(10.0), 0.5), Some(10.0));
}

#[test]
fn adaptive_change_score_identical() {
    let s = SystemSample {
        cpu_usage: 50.0,
        mem_pct: 60.0,
        load: 1.0,
        cpu_temp: 40.0,
        gpu_usage: 30.0,
        gpu_temp: 50.0,
    };
    assert!((adaptive_change_score(s, s) - 0.0).abs() < f32::EPSILON);
}

#[test]
fn adaptive_change_score_max_change() {
    let prev = SystemSample {
        cpu_usage: 0.0,
        mem_pct: 0.0,
        load: 0.0,
        cpu_temp: 0.0,
        gpu_usage: 0.0,
        gpu_temp: 0.0,
    };
    let next = SystemSample {
        cpu_usage: 100.0,
        mem_pct: 100.0,
        load: 2.0,
        cpu_temp: 20.0,
        gpu_usage: 100.0,
        gpu_temp: 20.0,
    };
    let score = adaptive_change_score(prev, next);
    assert!(score > 0.8);
    assert!(score <= 1.0);
}

#[test]
fn adaptive_poll_interval_ms_fast_when_high_change() {
    let ms = adaptive_poll_interval_ms(1.0);
    assert!(ms <= 1000);
}

#[test]
fn adaptive_poll_interval_ms_slow_when_no_change() {
    let ms = adaptive_poll_interval_ms(0.0);
    assert!(ms >= 2500);
}

#[test]
fn adaptive_poll_interval_ms_clamped() {
    let ms_low = adaptive_poll_interval_ms(-1.0);
    let ms_high = adaptive_poll_interval_ms(2.0);
    assert!(ms_low >= 500);
    assert!(ms_high <= 3000);
}

#[test]
fn sample_from_basic_conversion() {
    let info = crate::system::SystemInfo {
        cpu_usage: 42.5,
        memory_used: 4_000_000_000,
        memory_total: 8_000_000_000,
        load_average: Some(1.5),
        cpu_temperature: Some(55.0),
        gpu_info: Some(crate::system::GpuInfo {
            name: "RTX 4090".to_string(),
            vendor: "NVIDIA".to_string(),
            usage: Some(70.0),
            temperature: Some(65.0),
            vram_used: Some(8_000_000_000),
            vram_total: Some(24_000_000_000),
            power_watts: Some(300.0),
            power_limit_watts: Some(450.0),
        }),
        ..crate::system::SystemInfo::default()
    };
    let sample = sample_from(&info);
    assert!((sample.cpu_usage - 42.5).abs() < f32::EPSILON);
    assert!((sample.mem_pct - 50.0).abs() < 0.1);
    assert!((sample.load - 1.5).abs() < f32::EPSILON);
    assert!((sample.cpu_temp - 55.0).abs() < f32::EPSILON);
    assert!((sample.gpu_usage - 70.0).abs() < f32::EPSILON);
    assert!((sample.gpu_temp - 65.0).abs() < f32::EPSILON);
}

#[test]
fn sample_from_zero_memory_total() {
    let info = crate::system::SystemInfo {
        memory_used: 1000,
        memory_total: 0,
        ..crate::system::SystemInfo::default()
    };
    let sample = sample_from(&info);
    assert!((sample.mem_pct).abs() < f32::EPSILON);
}

#[test]
fn sample_from_no_gpu() {
    let info = crate::system::SystemInfo {
        gpu_info: None,
        ..crate::system::SystemInfo::default()
    };
    let sample = sample_from(&info);
    assert!((sample.gpu_usage).abs() < f32::EPSILON);
    assert!((sample.gpu_temp).abs() < f32::EPSILON);
}

#[test]
fn sidebar_tab_index_returns_correct_index() {
    assert_eq!(App::sidebar_tab_index(SidebarLayer::Live), 0);
    assert_eq!(App::sidebar_tab_index(SidebarLayer::Automation), 1);
    assert_eq!(App::sidebar_tab_index(SidebarLayer::Knowledge), 2);
}

#[test]
fn update_prompt_config_on_object() {
    let config = serde_json::json!({"platform": "claude", "model": "sonnet"});
    let result = App::update_prompt_config(&config, "new prompt here");
    assert_eq!(result["prompt_template"], "new prompt here");
    assert_eq!(result["platform"], "claude");
    assert_eq!(result["model"], "sonnet");
}

#[test]
fn update_prompt_config_on_non_object() {
    let config = serde_json::json!("just a string");
    let result = App::update_prompt_config(&config, "prompt text");
    assert_eq!(result["prompt_template"], "prompt text");
}

#[test]
fn update_prompt_config_preserves_existing_prompt_template() {
    let config = serde_json::json!({"prompt_template": "old prompt"});
    let result = App::update_prompt_config(&config, "replaced");
    assert_eq!(result["prompt_template"], "replaced");
}

#[test]
fn update_prompt_config_empty_object() {
    let config = serde_json::json!({});
    let result = App::update_prompt_config(&config, "test");
    assert_eq!(result["prompt_template"], "test");
}

#[test]
fn terminal_selection_normalized_ordering() {
    let sel = crate::tui::app::types::TerminalSelection {
        agent: (true, 0),
        start: (5, 10),
        end: (2, 3),
        dragging: false,
    };
    let (s, e) = sel.normalized();
    assert_eq!(s, (2, 3));
    assert_eq!(e, (5, 10));
}

#[test]
fn terminal_selection_normalized_already_ordered() {
    let sel = crate::tui::app::types::TerminalSelection {
        agent: (false, 1),
        start: (1, 2),
        end: (3, 4),
        dragging: false,
    };
    let (s, e) = sel.normalized();
    assert_eq!(s, (1, 2));
    assert_eq!(e, (3, 4));
}

#[test]
fn terminal_selection_normalized_equal_endpoints() {
    let sel = crate::tui::app::types::TerminalSelection {
        agent: (true, 0),
        start: (2, 3),
        end: (2, 3),
        dragging: false,
    };
    let (s, e) = sel.normalized();
    assert_eq!(s, (2, 3));
    assert_eq!(e, (2, 3));
}

// ── GraphEditorDialog tests ──────────────────────────────────

#[test]
fn graph_editor_dialog_new_sets_cursor_at_end() {
    let dialog = crate::tui::app::types::GraphEditorDialog::new(
        "n1".into(),
        "node1".into(),
        "title".into(),
        "help".into(),
        "hello world".into(),
        crate::tui::app::types::GraphEditorMode::AgentPrompt,
    );
    assert_eq!(dialog.cursor, 11); // "hello world" has 11 chars
}

#[test]
fn graph_editor_dialog_char_len() {
    let mut dialog = crate::tui::app::types::GraphEditorDialog::new(
        "n1".into(),
        "node1".into(),
        "".into(),
        "".into(),
        "abc".into(),
        crate::tui::app::types::GraphEditorMode::NodeConfig,
    );
    assert_eq!(dialog.char_len(), 3);
    dialog.insert_str("de");
    assert_eq!(dialog.char_len(), 5);
}

#[test]
fn graph_editor_dialog_insert_char() {
    let mut dialog = crate::tui::app::types::GraphEditorDialog::new(
        "n1".into(),
        "node1".into(),
        "".into(),
        "".into(),
        "ac".into(),
        crate::tui::app::types::GraphEditorMode::AgentPrompt,
    );
    dialog.cursor = 1;
    dialog.insert_char('b');
    assert_eq!(dialog.buffer, "abc");
    assert_eq!(dialog.cursor, 2);
}

#[test]
fn graph_editor_dialog_backspace() {
    let mut dialog = crate::tui::app::types::GraphEditorDialog::new(
        "n1".into(),
        "node1".into(),
        "".into(),
        "".into(),
        "abc".into(),
        crate::tui::app::types::GraphEditorMode::AgentPrompt,
    );
    dialog.backspace();
    assert_eq!(dialog.buffer, "ab");
    assert_eq!(dialog.cursor, 2);
}

#[test]
fn graph_editor_dialog_backspace_at_zero() {
    let mut dialog = crate::tui::app::types::GraphEditorDialog::new(
        "n1".into(),
        "node1".into(),
        "".into(),
        "".into(),
        "abc".into(),
        crate::tui::app::types::GraphEditorMode::AgentPrompt,
    );
    dialog.cursor = 0;
    dialog.backspace();
    assert_eq!(dialog.buffer, "abc");
}

#[test]
fn graph_editor_dialog_move_left_right() {
    let mut dialog = crate::tui::app::types::GraphEditorDialog::new(
        "n1".into(),
        "node1".into(),
        "".into(),
        "".into(),
        "abc".into(),
        crate::tui::app::types::GraphEditorMode::AgentPrompt,
    );
    dialog.move_left();
    assert_eq!(dialog.cursor, 2);
    dialog.move_right();
    assert_eq!(dialog.cursor, 3);
    dialog.move_right(); // At end
    assert_eq!(dialog.cursor, 3);
}

#[test]
fn graph_editor_dialog_move_home_end() {
    let mut dialog = crate::tui::app::types::GraphEditorDialog::new(
        "n1".into(),
        "node1".into(),
        "".into(),
        "".into(),
        "hello".into(),
        crate::tui::app::types::GraphEditorMode::AgentPrompt,
    );
    dialog.move_home();
    assert_eq!(dialog.cursor, 0);
    dialog.move_end();
    assert_eq!(dialog.cursor, 5);
}

// ── Knowledge filter tests ──────────────────────────────────

#[test]
fn filtered_knowledge_indices_empty_filter_returns_all() {
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
            title: "Fact One".into(),
            body: "body one".into(),
            metadata: None,
            project_hash: None,
            session_id: None,
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        },
        crate::db::intelligence::IntelligenceNodeRecord {
            id: "n2".into(),
            kind: "pattern".into(),
            status: "noted".into(),
            title: "Pattern Two".into(),
            body: "body two".into(),
            metadata: None,
            project_hash: None,
            session_id: None,
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        },
    ];
    app.knowledge_filter.clear();
    let indices = app.filtered_knowledge_indices();
    assert_eq!(indices, vec![0, 1]);
}

#[test]
fn filtered_knowledge_indices_filter_matches_title() {
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
            title: "Rust Ownership".into(),
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
            title: "Python GIL".into(),
            body: "body".into(),
            metadata: None,
            project_hash: None,
            session_id: None,
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        },
    ];
    app.knowledge_filter = "rust".to_string();
    let indices = app.filtered_knowledge_indices();
    assert_eq!(indices, vec![0]);
}

#[test]
fn filtered_knowledge_indices_filter_matches_body() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.project_knowledge = vec![crate::db::intelligence::IntelligenceNodeRecord {
        id: "n1".into(),
        kind: "fact".into(),
        status: "noted".into(),
        title: "Title".into(),
        body: "contains the word pattern".into(),
        metadata: None,
        project_hash: None,
        session_id: None,
        created_at: chrono::Utc::now().timestamp(),
        updated_at: chrono::Utc::now().timestamp(),
    }];
    app.knowledge_filter = "pattern".to_string();
    let indices = app.filtered_knowledge_indices();
    assert_eq!(indices, vec![0]);
}

#[test]
fn filtered_knowledge_indices_filter_matches_kind() {
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
            title: "Title".into(),
            body: "body".into(),
            metadata: None,
            project_hash: None,
            session_id: None,
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        },
        crate::db::intelligence::IntelligenceNodeRecord {
            id: "n2".into(),
            kind: "pattern".into(),
            status: "noted".into(),
            title: "Title".into(),
            body: "body".into(),
            metadata: None,
            project_hash: None,
            session_id: None,
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        },
    ];
    app.knowledge_filter = "pattern".to_string();
    let indices = app.filtered_knowledge_indices();
    assert_eq!(indices, vec![1]);
}

#[test]
fn filtered_knowledge_indices_no_match() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.project_knowledge = vec![crate::db::intelligence::IntelligenceNodeRecord {
        id: "n1".into(),
        kind: "fact".into(),
        status: "noted".into(),
        title: "Title".into(),
        body: "body".into(),
        metadata: None,
        project_hash: None,
        session_id: None,
        created_at: chrono::Utc::now().timestamp(),
        updated_at: chrono::Utc::now().timestamp(),
    }];
    app.knowledge_filter = "zzz_not_found".to_string();
    let indices = app.filtered_knowledge_indices();
    assert!(indices.is_empty());
}

// ── ProjectTab tests ────────────────────────────────────────

#[test]
fn project_tab_labels() {
    assert_eq!(ProjectTab::Overview.label(), "Overview");
    assert_eq!(ProjectTab::Backlog.label(), "Backlog");
    assert_eq!(ProjectTab::Knowledge.label(), "Knowledge");
    assert_eq!(ProjectTab::History.label(), "History");
}

#[test]
fn project_tab_hotkeys() {
    assert_eq!(ProjectTab::Overview.hotkey(), 'o');
    assert_eq!(ProjectTab::Backlog.hotkey(), 'b');
    assert_eq!(ProjectTab::Knowledge.hotkey(), 'k');
    assert_eq!(ProjectTab::History.hotkey(), 'h');
}

#[test]
fn project_tab_all_has_four_entries() {
    assert_eq!(ProjectTab::ALL.len(), 4);
}
