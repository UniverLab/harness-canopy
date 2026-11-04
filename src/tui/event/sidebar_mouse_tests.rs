use super::*;
use crate::db::Database;
use crate::domain::models::{Agent, Cli, Trigger};
use chrono::Utc;
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

fn test_db() -> Arc<Database> {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(Database::new(&path).expect("create test db"))
}

fn cron_agent(id: &str) -> Agent {
    Agent {
        id: id.to_string(),
        prompt: "prompt".to_string(),
        trigger: Some(Trigger::Cron {
            schedule_expr: "0 9 * * *".to_string(),
        }),
        cli: Cli::new("claude"),
        model: None,
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: Utc::now(),
        log_path: "/tmp/test-sidebar-mouse.log".to_string(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    }
}

/// Builds an App with `count` background agents and a `sidebar_click_map`
/// matching the row layout `draw_agent_list` produces (3-row cards, 1-row
/// gap): agent `i` occupies rows `[i*4, i*4+3)`.
fn app_with_agents(count: usize) -> App {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.agents = (0..count)
        .map(|i| AgentEntry::Agent(cron_agent(&format!("agent-{i}"))))
        .collect();
    app.sidebar_visible = true;
    app.sidebar_click_map = (0..count)
        .map(|i| (i, (i * 4) as u16, (i * 4 + 3) as u16))
        .collect();
    app.selected = 0;
    app.focus = Focus::Preview;
    app
}

#[test]
fn moved_updates_hovered_row_without_changing_selection() {
    let mut app = app_with_agents(3);
    assert_eq!(app.hovered_row, None);

    let mouse = MouseEvent {
        kind: MouseEventKind::Moved,
        column: 5,
        row: 5, // falls in agent #1's rows [4, 7)
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_sidebar_mouse(&mut app, &mouse);

    assert!(consumed);
    assert_eq!(app.hovered_row, Some(1));
    assert_eq!(app.selected, 0, "hover must not change selection");
}

#[test]
fn left_click_selects_agent_under_cursor() {
    let mut app = app_with_agents(3);

    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: 9, // falls in agent #2's rows [8, 11)
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_sidebar_mouse(&mut app, &mouse);

    assert!(consumed);
    assert_eq!(app.selected, 2);
    assert!(matches!(app.focus, Focus::Preview));
}

#[test]
fn left_click_on_already_selected_agent_enters_it() {
    let mut app = app_with_agents(3);
    app.selected = 2;

    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: 9, // agent #2 again
        modifiers: KeyModifiers::NONE,
    };
    handle_sidebar_mouse(&mut app, &mouse);

    assert_eq!(app.selected, 2);
    assert!(matches!(app.focus, Focus::Agent));
}

#[test]
fn scroll_down_increments_offset_within_bounds() {
    // 20 agents, 8 rows visible per the last render.
    let mut app = app_with_agents(20);
    app.sidebar_visible_capacity = 8;
    assert_eq!(app.sidebar_scroll_offset, 0);

    let mouse = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    handle_sidebar_mouse(&mut app, &mouse);

    assert_eq!(app.sidebar_scroll_offset, 1);
}

#[test]
fn clamp_sidebar_scroll_never_exceeds_max_offset() {
    // total=20, max_visible=8 → max_offset=12.
    assert_eq!(clamp_sidebar_scroll(0, 20, 8, -1), 1);
    assert_eq!(clamp_sidebar_scroll(12, 20, 8, -1), 12);
    assert_eq!(clamp_sidebar_scroll(1, 20, 8, 1), 0);
    assert_eq!(clamp_sidebar_scroll(0, 20, 8, 1), 0);
}

#[test]
fn right_click_behaves_like_f2() {
    let mut via_key = app_with_agents(3);
    via_key.focus = Focus::Agent;
    let key_handled = handle_global_key(&mut via_key, KeyCode::F(2), KeyModifiers::NONE);

    let mut via_click = app_with_agents(3);
    via_click.focus = Focus::Agent;
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    let click_handled = handle_sidebar_mouse(&mut via_click, &mouse);

    assert!(key_handled);
    assert!(click_handled);
    assert!(via_key.sidebar_layer == via_click.sidebar_layer);
    assert!(matches!(via_key.focus, Focus::Preview));
    assert!(matches!(via_click.focus, Focus::Preview));
}

#[test]
fn f2_cycles_sidebar_layer_skipping_empty_layers() {
    let mut app = app_with_agents(3);
    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Live);

    assert!(handle_global_key(
        &mut app,
        KeyCode::F(2),
        KeyModifiers::NONE
    ));
    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Automation);

    // Live and Knowledge are both empty, so a second press has nowhere
    // else to land and stays on Automation.
    assert!(handle_global_key(
        &mut app,
        KeyCode::F(2),
        KeyModifiers::NONE
    ));
    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Automation);
}

#[test]
fn f2_cycles_through_all_three_tabs_when_all_have_content() {
    use crate::domain::models::{SplitGroup, SplitOrientation};

    // 3 background agents (Automation) from `app_with_agents`, plus a
    // split group (Live) and a project (Knowledge) so every tab has
    // something to land on and the ring wraps all the way around.
    let mut app = app_with_agents(3);
    app.agents.push(AgentEntry::Group(0));
    app.split_groups.push(SplitGroup {
        id: "g1".to_string(),
        orientation: SplitOrientation::Horizontal,
        session_a: "a".to_string(),
        session_b: "b".to_string(),
        created_at: Utc::now(),
    });
    app.projects
        .push(crate::domain::project::Project::new("/tmp/proj"));

    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Live);

    assert!(handle_global_key(
        &mut app,
        KeyCode::F(2),
        KeyModifiers::NONE
    ));
    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Automation);

    assert!(handle_global_key(
        &mut app,
        KeyCode::F(2),
        KeyModifiers::NONE
    ));
    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Knowledge);

    assert!(handle_global_key(
        &mut app,
        KeyCode::F(2),
        KeyModifiers::NONE
    ));
    assert_eq!(
        app.sidebar_layer,
        crate::tui::app::SidebarLayer::Live,
        "the ring wraps back to Live"
    );
}

#[test]
fn shift_arrows_step_one_sidebar_tab_each_way_and_wrap() {
    let mut app = app_with_agents(3);
    app.focus = Focus::Home;
    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Live);

    assert!(handle_global_key(
        &mut app,
        KeyCode::Right,
        KeyModifiers::SHIFT
    ));
    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Automation);

    // Knowledge is empty here — unlike F2, the arrow still lands on it.
    assert!(handle_global_key(
        &mut app,
        KeyCode::Right,
        KeyModifiers::SHIFT
    ));
    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Knowledge);
    assert!(
        matches!(app.focus, Focus::Home),
        "stepping from sidebar level must stay at sidebar level"
    );
    assert!(
        app.project_focus.is_none(),
        "arriving on Knowledge must not open its project tabs"
    );

    assert!(handle_global_key(
        &mut app,
        KeyCode::Right,
        KeyModifiers::SHIFT
    ));
    assert_eq!(
        app.sidebar_layer,
        crate::tui::app::SidebarLayer::Live,
        "stepping right off the end wraps to the first tab"
    );

    assert!(handle_global_key(
        &mut app,
        KeyCode::Left,
        KeyModifiers::SHIFT
    ));
    assert_eq!(
        app.sidebar_layer,
        crate::tui::app::SidebarLayer::Knowledge,
        "stepping left off the start wraps to the last tab"
    );
    assert!(
        matches!(app.focus, Focus::Home),
        "stepping from sidebar level must stay at sidebar level"
    );
    assert!(
        app.project_focus.is_none(),
        "arriving on Knowledge must not open its project tabs"
    );
}

#[test]
fn unshifted_arrows_do_not_switch_sidebar_tabs() {
    // Plain ←/→ belongs to the graph expand/collapse handler; only the
    // shifted pair is ours.
    let mut app = app_with_agents(3);
    app.focus = Focus::Home;
    assert!(!handle_global_key(
        &mut app,
        KeyCode::Right,
        KeyModifiers::NONE
    ));
    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Live);
}

#[test]
fn shift_arrows_step_sidebar_tab_inside_a_focused_agent_without_a_split() {
    // Functional requirement 1: the sidebar tab strip must be reachable
    // from focus, not only from Home/Preview, as long as nothing else
    // (a split) already owns Shift+←/→ there. Shift+arrows are frame
    // navigation: they select the tab and land at sidebar level, like a
    // click — entering the tab's panel is Enter's job.
    let mut app = app_with_agents(3);
    app.focus = Focus::Agent;
    app.selected = 0;

    assert!(handle_global_key(
        &mut app,
        KeyCode::Right,
        KeyModifiers::SHIFT
    ));

    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Automation);
    assert!(
        matches!(app.focus, Focus::Preview),
        "stepping tabs from focus must land at sidebar level, like a click"
    );
}

#[test]
fn shift_arrows_defer_to_split_focus_when_a_split_is_active() {
    // With a split active, Shift+←/→ keeps meaning split-pane focus —
    // the established binding that would otherwise be shadowed by
    // widening the sidebar-tab-step scope into Focus::Agent.
    let mut app = app_with_agents(3);
    app.focus = Focus::Agent;
    app.active_split_id = Some("split-1".to_string());

    assert!(!handle_global_key(
        &mut app,
        KeyCode::Right,
        KeyModifiers::SHIFT
    ));
    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Live);
}

#[test]
fn clicking_a_sidebar_tab_switches_it() {
    let mut app = app_with_agents(3);
    app.sidebar_tab_click_map = vec![
        (crate::tui::app::SidebarLayer::Live, 0, 0, 11),
        (crate::tui::app::SidebarLayer::Automation, 0, 11, 22),
        (crate::tui::app::SidebarLayer::Knowledge, 0, 22, 33),
    ];
    app.focus = Focus::Agent;

    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 15, // inside the Automation cell [11, 22)
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_sidebar_mouse(&mut app, &mouse);

    assert!(consumed);
    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Automation);
    assert!(
        matches!(app.focus, Focus::Preview),
        "clicking a tab must back out of a deep Focus::Agent view"
    );
}

#[test]
fn clicking_an_empty_sidebar_tab_still_switches_to_it() {
    // Knowledge has no projects — a keyboard F2 cycle would skip it, but
    // a deliberate click on its label must still land there so the
    // empty state ("No registered projects") is reachable.
    let mut app = app_with_agents(3);
    app.sidebar_tab_click_map = vec![
        (crate::tui::app::SidebarLayer::Live, 0, 0, 11),
        (crate::tui::app::SidebarLayer::Automation, 0, 11, 22),
        (crate::tui::app::SidebarLayer::Knowledge, 0, 22, 33),
    ];

    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 25,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    handle_sidebar_mouse(&mut app, &mouse);

    assert_eq!(app.sidebar_layer, crate::tui::app::SidebarLayer::Knowledge);
}

#[test]
fn f6_no_longer_opens_a_panel_picker_in_preview_or_agent_focus() {
    // The F6 panel picker is retired: the pin lives in
    // F1 → Settings → TUI Appearance now. In both surfaces where F6
    // used to open the picker it must be a no-op — most visibly, the
    // global handler no longer claims the key.
    let mut preview = app_with_agents(1);
    assert!(matches!(preview.focus, Focus::Preview));
    assert!(
        !handle_global_key(&mut preview, KeyCode::F(6), KeyModifiers::NONE),
        "F6 must not be claimed by the global handler anymore"
    );
    handle_key(&mut preview, KeyCode::F(6), KeyModifiers::NONE).unwrap();
    assert!(!preview.hub_open, "F6 must not open the hub in Preview");
    assert!(
        matches!(preview.focus, Focus::Preview),
        "F6 must leave Preview untouched"
    );

    let mut agent = app_with_agents(1);
    agent.focus = Focus::Agent;
    assert!(
        !handle_global_key(&mut agent, KeyCode::F(6), KeyModifiers::NONE),
        "F6 must not be claimed by the global handler anymore"
    );
    handle_key(&mut agent, KeyCode::F(6), KeyModifiers::NONE).unwrap();
    assert!(!agent.hub_open, "F6 must not open the hub in Agent focus");
    assert!(
        matches!(agent.focus, Focus::Agent),
        "F6 must leave the focused session untouched"
    );
}
