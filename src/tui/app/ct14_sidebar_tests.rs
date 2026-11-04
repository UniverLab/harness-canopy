use super::App;
use crate::db::Database;
use crate::domain::graphs::GraphStatus;
use crate::tui::app::types::{
    AgentEntry, AgentSectionFocus, AutomationKind, ProjectTab, SidebarLayer,
};
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

fn test_db() -> Arc<Database> {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(Database::new(&path).expect("create test db"))
}

fn make_project(hash: &str, path: &str) -> crate::domain::project::Project {
    crate::domain::project::Project {
        hash: hash.to_string(),
        path: path.to_string(),
        name: hash.to_string(),
        description: None,
        tags: None,
        indexed_at: None,
        created_at: 0,
    }
}

fn make_graph(id: &str, name: &str) -> crate::domain::graphs::Graph {
    crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: id.to_string(),
        name: name.to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status: GraphStatus::Running,
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
    }
}

fn bg_agent(id: &str) -> crate::domain::models::Agent {
    crate::domain::models::Agent {
        id: id.to_string(),
        prompt: String::new(),
        trigger: None,
        cli: crate::domain::models::Cli::new("claude"),
        model: None,
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: chrono::Utc::now(),
        log_path: format!("/tmp/{id}.log"),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    }
}

fn history_entry(name: &str) -> crate::db::project::ProjectHistoryEntry {
    crate::db::project::ProjectHistoryEntry {
        kind: crate::db::project::ProjectHistoryKind::Graph,
        name: name.to_string(),
        status: "done".to_string(),
        at: 0,
    }
}

#[test]
fn ct14_shift_arrows_cycle_after_knowledge_round_trip() {
    // FOCUS bug #1: `project_focus` must not survive leaving Knowledge —
    // a surviving focus traps Shift+←/→ in the project-tab keymap.
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
    app.agents = vec![AgentEntry::Group(0), AgentEntry::Group(1)];
    app.sidebar_layer = SidebarLayer::Knowledge;
    app.selected_project = 0;

    app.enter_project_focus(ProjectTab::Overview);
    assert_eq!(app.project_focus, Some(ProjectTab::Overview));

    // Leave Knowledge via the F2 ring walk.
    app.cycle_sidebar_layer();
    assert!(
        app.project_focus.is_none(),
        "leaving Knowledge must clear the deep project focus"
    );
    assert_ne!(
        app.sidebar_layer,
        SidebarLayer::Knowledge,
        "F2 must land on another tab"
    );

    // Shift+←/→ steps the sidebar tab ring (not the project tabs).
    let before = app.sidebar_layer;
    app.step_sidebar_tab(true);
    assert!(
        app.project_focus.is_none(),
        "tab stepping must not re-enter project focus"
    );
    assert_ne!(
        app.sidebar_layer, before,
        "Shift+→ must advance the sidebar tab ring"
    );
    app.step_sidebar_tab(false);
    assert_eq!(app.sidebar_layer, before, "Shift+← must step the ring back");
}

#[test]
fn ct14_automation_kind_flips_when_its_list_empties_and_graphs_still_reachable() {
    // SELECTION bug #2: `automation_kind` pointing at an emptied sub-list
    // must flip to the non-empty side so graphs stay reachable.
    let db = test_db();
    db.insert_graph(&make_graph("graph-1", "Graph 1")).unwrap();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.refresh_graphs().expect("refresh graphs");
    app.agents = vec![AgentEntry::Agent(bg_agent("bg-1"))];
    app.automation_kind = AutomationKind::Agent;

    // The last background agent disappears while one graph exists.
    app.agents
        .retain(|a| !matches!(a, AgentEntry::Agent(_) | AgentEntry::Corrupt(_)));
    app.normalize_automation_kind();
    assert_eq!(app.automation_kind, AutomationKind::Graph);
    assert_eq!(app.selected_graph_id.as_deref(), Some("graph-1"));
    assert!(
        app.enter_layer(SidebarLayer::Automation, true),
        "graphs section must stay reachable"
    );

    // Symmetric subcase: graphs vanish while agents remain.
    app.graphs.clear();
    app.archived_graphs.clear();
    app.selected_graph_id = Some("graph-1".to_string());
    app.automation_kind = AutomationKind::Graph;
    app.agents = vec![AgentEntry::Agent(bg_agent("bg-2"))];
    app.selected = 99;
    app.normalize_automation_kind();
    assert_eq!(app.automation_kind, AutomationKind::Agent);
    assert!(
        app.automation_agent_indices().contains(&app.selected),
        "agent cursor must name a live row"
    );
    assert!(
        app.enter_layer(SidebarLayer::Automation, true),
        "agents side must stay reachable"
    );
}

#[test]
fn ct14_agent_section_focus_moves_off_empty_section() {
    // SELECTION bug #3 (C32 follow-on): a zero-row section must not keep
    // the focus that `fair_section_heights` turns into a floor.
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");

    app.agents = vec![AgentEntry::Terminal(0)];
    app.agent_section_focus = AgentSectionFocus::Terminal;
    app.agents = vec![AgentEntry::Interactive(0)];
    app.normalize_agent_section_focus();
    assert_eq!(app.agent_section_focus, AgentSectionFocus::Interactive);

    // Subcase: only Groups remain.
    app.split_groups.push(crate::domain::models::SplitGroup {
        id: "g0".to_string(),
        orientation: crate::domain::models::SplitOrientation::Horizontal,
        session_a: "a".to_string(),
        session_b: "b".to_string(),
        created_at: chrono::Utc::now(),
    });
    app.agents = vec![AgentEntry::Group(0)];
    app.agent_section_focus = AgentSectionFocus::Terminal;
    app.normalize_agent_section_focus();
    assert_eq!(app.agent_section_focus, AgentSectionFocus::Groups);
}

#[test]
fn ct14_selected_and_scroll_clamped_after_shrink() {
    // SELECTION bugs #4/#5/#7: every stored cursor is corrected at the
    // moment of use instead of failing silently.
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");

    // `selected` past the end of a shrunk agent list.
    app.agents = vec![
        AgentEntry::Group(0),
        AgentEntry::Group(1),
        AgentEntry::Group(2),
        AgentEntry::Group(3),
        AgentEntry::Group(4),
    ];
    app.sidebar_layer = SidebarLayer::Live;
    app.selected = 4;
    app.agents.truncate(3);
    app.clamp_sidebar_selection();
    assert_eq!(app.selected, 2);

    // `selected_project` past the end of a shrunk project list.
    app.projects = vec![
        make_project("h0", "/tmp/a"),
        make_project("h1", "/tmp/b"),
        make_project("h2", "/tmp/c"),
    ];
    app.selected_project = 2;
    app.projects.truncate(1);
    app.clamp_sidebar_selection();
    assert_eq!(app.selected_project, 0);

    // History cursor belongs to the previous project's longer list.
    app.projects = vec![make_project("h0", "/tmp/a"), make_project("h1", "/tmp/b")];
    app.project_history_cache.insert(
        "h0".to_string(),
        (0..6).map(|i| history_entry(&format!("e{i}"))).collect(),
    );
    app.project_history_cache
        .insert("h1".to_string(), vec![history_entry("only")]);
    app.selected_project = 0;
    app.selected_project_history = 5;
    app.project_focus = Some(ProjectTab::History);
    app.selected_project = 1;
    app.clamp_sidebar_selection();
    assert_eq!(app.selected_project_history, 0);

    // Scroll offset stranded past the new tab total.
    app.project_focus = None;
    app.sidebar_layer = SidebarLayer::Live;
    app.sidebar_visible_capacity = 5;
    app.sidebar_scroll_offset = 10;
    app.agents = vec![AgentEntry::Group(0), AgentEntry::Group(1)];
    app.selected = 0;
    app.clamp_sidebar_selection();
    assert_eq!(app.sidebar_scroll_offset, 0);

    // Arrow navigation stays within the new bounds.
    let live: Vec<usize> = app.live_indices();
    app.select_next();
    assert!(
        live.contains(&app.selected),
        "select_next must land on an existing row"
    );
    app.select_prev();
    assert!(
        live.contains(&app.selected),
        "select_prev must land on an existing row"
    );
}

#[test]
fn ct14_tab_memory_dead_slot_cleared_on_failed_restore() {
    // SELECTION bug #6: a dead `SidebarStepMemory` slot must be cleared on
    // failed restore, not re-probed on every revisit.
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.agents = vec![AgentEntry::Group(0)];

    app.sidebar_step_memory.live_selected = Some(99);
    assert!(!app.restore_remembered_sidebar_selection(SidebarLayer::Live));
    assert_eq!(app.sidebar_step_memory.live_selected, None);

    app.sidebar_step_memory.automation_kind = Some(AutomationKind::Graph);
    app.sidebar_step_memory.automation_graph_id = Some("ghost".to_string());
    assert!(!app.restore_remembered_sidebar_selection(SidebarLayer::Automation));
    assert_eq!(app.sidebar_step_memory.automation_graph_id, None);

    app.sidebar_step_memory.knowledge_selected = Some(7);
    assert!(!app.restore_remembered_sidebar_selection(SidebarLayer::Knowledge));
    assert_eq!(app.sidebar_step_memory.knowledge_selected, None);
}

#[test]
fn ct14_long_scripted_sequence_leaves_navigation_working() {
    // The spec's scripted reproduction: start Live → enter Knowledge →
    // move within it → leave → F2 cycle → Shift+←/→ both ways → enter
    // Automation → navigate both ways → shrink graphs between ticks →
    // navigate again. Navigation must work throughout.
    let db = test_db();
    db.upsert_project(&make_project("hash0", "/tmp/proj0"))
        .unwrap();
    db.insert_graph(&make_graph("graph-1", "Graph 1")).unwrap();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.refresh_graphs().expect("refresh graphs");
    app.agents = vec![
        AgentEntry::Group(0),
        AgentEntry::Group(1),
        AgentEntry::Agent(bg_agent("bg-1")),
    ];
    app.sidebar_layer = SidebarLayer::Live;
    app.selected = 0;

    // Enter Knowledge, move within the project tabs, leave via Esc.
    app.sidebar_layer = SidebarLayer::Knowledge;
    app.selected_project = 0;
    app.enter_project_focus(ProjectTab::Overview);
    app.navigate_project_tab_list(true);
    app.cycle_project_tab(true);
    assert_eq!(app.project_focus, Some(ProjectTab::Backlog));
    app.navigate_project_tab_list(true);
    app.exit_project_focus();
    assert!(app.project_focus.is_none());

    // Switch sections a few times (F2) and cycle tabs (Shift+←/→).
    app.cycle_sidebar_layer();
    app.cycle_sidebar_layer();
    let ring_pos = app.sidebar_layer;
    app.step_sidebar_tab(true);
    app.step_sidebar_tab(false);
    assert_eq!(app.sidebar_layer, ring_pos);
    assert!(app.project_focus.is_none());

    // Reach the graphs section and move within Automation.
    app.switch_sidebar_tab(SidebarLayer::Automation);
    assert_eq!(app.sidebar_layer, SidebarLayer::Automation);
    app.select_next();
    app.select_prev();

    // Shrink the graphs list between ticks, then navigate again.
    app.graphs.clear();
    app.archived_graphs.clear();
    app.refresh_graphs_selection();
    app.normalize_automation_kind();
    app.normalize_agent_section_focus();
    app.clamp_sidebar_selection();
    app.select_next();
    app.select_prev();

    // Every cursor still names something that exists.
    match app.sidebar_layer {
        SidebarLayer::Live => {
            assert!(app.live_indices().contains(&app.selected));
        }
        SidebarLayer::Automation => match app.automation_kind {
            AutomationKind::Agent => {
                assert!(app.automation_agent_indices().contains(&app.selected));
            }
            AutomationKind::Graph => {
                let id = app.selected_graph_id.clone().expect("graph cursor set");
                assert!(app.sidebar_graphs().iter().any(|lp| lp.id == id));
            }
        },
        SidebarLayer::Knowledge => {
            assert!(app.selected_project < app.projects.len());
        }
    }

    // Shift+←/→ still cycles the tab ring.
    let before = app.sidebar_layer;
    app.step_sidebar_tab(true);
    assert_ne!(app.sidebar_layer, before);
    assert!(app.project_focus.is_none());
}
