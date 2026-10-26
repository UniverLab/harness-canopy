//! Part 01 of `preview_key_tests` (split from `src/tui/event/home_preview.rs`, pure move).
use super::*;

#[test]
fn preview_esc_goes_home() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    handle_preview_key(&mut app, KeyCode::Esc, KeyModifiers::NONE).unwrap();
    assert!(matches!(app.focus, Focus::Home));
}

#[test]
fn preview_h_goes_home() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    handle_preview_key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE).unwrap();
    assert!(matches!(app.focus, Focus::Home));
}

#[test]
fn preview_f10_goes_home() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    handle_preview_key(&mut app, KeyCode::F(10), KeyModifiers::NONE).unwrap();
    assert!(matches!(app.focus, Focus::Home));
}

#[test]
fn preview_down_selects_next() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.sidebar_layer = SidebarLayer::Automation;
    app.agents = vec![
        AgentEntry::Agent(cron_agent("a1")),
        AgentEntry::Agent(cron_agent("a2")),
    ];
    app.selected = 0;
    handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();
    assert_eq!(app.selected, 1);
}

#[test]
fn preview_up_selects_prev() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.sidebar_layer = SidebarLayer::Automation;
    app.agents = vec![
        AgentEntry::Agent(cron_agent("a1")),
        AgentEntry::Agent(cron_agent("a2")),
    ];
    app.selected = 1;
    handle_preview_key(&mut app, KeyCode::Up, KeyModifiers::NONE).unwrap();
    assert_eq!(app.selected, 0);
}

#[test]
fn preview_j_selects_next() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.sidebar_layer = SidebarLayer::Automation;
    app.agents = vec![
        AgentEntry::Agent(cron_agent("a1")),
        AgentEntry::Agent(cron_agent("a2")),
    ];
    app.selected = 0;
    handle_preview_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE).unwrap();
    assert_eq!(app.selected, 1);
}

#[test]
fn preview_k_selects_prev() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.sidebar_layer = SidebarLayer::Automation;
    app.agents = vec![
        AgentEntry::Agent(cron_agent("a1")),
        AgentEntry::Agent(cron_agent("a2")),
    ];
    app.selected = 1;
    handle_preview_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE).unwrap();
    assert_eq!(app.selected, 0);
}

#[test]
fn preview_enter_focuses_agent() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(matches!(app.focus, Focus::Agent));
}

#[test]
fn preview_l_focuses_agent() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    handle_preview_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE).unwrap();
    assert!(matches!(app.focus, Focus::Agent));
}

#[test]
fn preview_f1_shows_legend() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.hub_open = false;
    handle_preview_key(&mut app, KeyCode::F(1), KeyModifiers::NONE).unwrap();
    assert!(app.hub_open);
}

#[test]
fn preview_n_opens_new_agent_dialog() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    handle_preview_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
    assert!(matches!(app.focus, Focus::NewAgentDialog));
}

#[test]
fn preview_delete_confirm_y_deletes() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.delete_project_confirm = true;
    handle_preview_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE).unwrap();
    assert!(!app.delete_project_confirm);
}

#[test]
fn preview_delete_confirm_n_cancels() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.delete_project_confirm = true;
    handle_preview_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
    assert!(!app.delete_project_confirm);
}

#[test]
fn preview_archive_graph_confirm_y_archives() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.archive_graph_confirm = true;
    handle_preview_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE).unwrap();
    assert!(!app.archive_graph_confirm);
}

#[test]
fn preview_archive_graph_confirm_n_cancels() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.archive_graph_confirm = true;
    handle_preview_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
    assert!(!app.archive_graph_confirm);
}

#[test]
fn preview_permanent_delete_graph_confirm_y_deletes() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.permanent_delete_graph_confirm = true;
    handle_preview_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE).unwrap();
    assert!(!app.permanent_delete_graph_confirm);
}

#[test]
fn preview_permanent_delete_graph_confirm_n_cancels() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.permanent_delete_graph_confirm = true;
    handle_preview_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
    assert!(!app.permanent_delete_graph_confirm);
}

#[test]
fn preview_f4_on_graph_opens_archive_confirm_not_permanent_delete() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.sidebar_layer = SidebarLayer::Automation;
    app.automation_kind = crate::tui::app::AutomationKind::Graph;
    app.graph_view_archived = false;
    handle_preview_key(&mut app, KeyCode::F(4), KeyModifiers::NONE).unwrap();
    assert!(app.archive_graph_confirm);
    assert!(!app.permanent_delete_graph_confirm);
}

#[test]
fn preview_f4_in_archived_view_opens_permanent_delete_confirm() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.sidebar_layer = SidebarLayer::Automation;
    app.automation_kind = crate::tui::app::AutomationKind::Graph;
    app.graph_view_archived = true;
    handle_preview_key(&mut app, KeyCode::F(4), KeyModifiers::NONE).unwrap();
    assert!(app.permanent_delete_graph_confirm);
    assert!(!app.archive_graph_confirm);
}

#[test]
fn preview_shift_a_toggles_graph_archive_view() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.sidebar_layer = SidebarLayer::Automation;
    app.automation_kind = crate::tui::app::AutomationKind::Graph;
    assert!(!app.graph_view_archived);

    handle_preview_key(&mut app, KeyCode::Char('A'), KeyModifiers::NONE).unwrap();
    assert!(app.graph_view_archived);

    handle_preview_key(&mut app, KeyCode::Char('A'), KeyModifiers::NONE).unwrap();
    assert!(!app.graph_view_archived);
}

#[test]
fn preview_d_toggles_enable() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
    app.selected = 0;
    let _ = handle_preview_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
    // The toggle call may fail on test agents, but the key is consumed
}

#[test]
fn preview_r_reruns_selected() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
    app.selected = 0;
    let _ = handle_preview_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
}

#[test]
fn preview_e_opens_edit_dialog() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
    app.selected = 0;
    let _ = handle_preview_key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
}

#[test]
fn preview_r_on_a_completed_graph_dispatches_run() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Completed);
    handle_preview_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE).unwrap();
    assert!(app.graph_action_pending);
    assert!(app.graph_action_rx.is_some());
}

#[test]
fn preview_r_on_a_running_graph_is_not_bound() {
    // Run is not a valid action for a running graph (decision 3) — 'r'
    // must be a no-op, not a dispatch the daemon then refuses.
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
    handle_preview_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE).unwrap();
    assert!(!app.graph_action_pending);
    assert!(app.graph_action_rx.is_none());
}

#[test]
fn preview_p_on_a_running_graph_dispatches_pause() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
    handle_preview_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE).unwrap();
    assert!(app.graph_action_pending);
}

#[test]
fn preview_p_on_a_non_running_graph_is_not_bound() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Draft);
    handle_preview_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE).unwrap();
    assert!(!app.graph_action_pending);
}

#[test]
fn preview_c_and_shift_c_on_a_paused_graph_dispatch_continue() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Paused);
    handle_preview_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE).unwrap();
    assert!(app.graph_action_pending);

    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Paused);
    handle_preview_key(&mut app, KeyCode::Char('C'), KeyModifiers::SHIFT).unwrap();
    assert!(app.graph_action_pending);
}

#[test]
fn preview_continue_keys_on_a_non_paused_graph_are_not_bound() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
    handle_preview_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE).unwrap();
    assert!(!app.graph_action_pending);
    handle_preview_key(&mut app, KeyCode::Char('C'), KeyModifiers::SHIFT).unwrap();
    assert!(!app.graph_action_pending);
}

#[test]
fn preview_x_on_a_completed_graph_opens_reset_confirm() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Completed);
    handle_preview_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE).unwrap();
    assert!(app.graph_reset_confirm);
}

#[test]
fn preview_x_on_a_running_graph_is_not_bound() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
    handle_preview_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE).unwrap();
    assert!(!app.graph_reset_confirm);
}

#[test]
fn preview_t_closes_open_tail_dialog() {
    let mut app = app_with_open_tail();
    handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::NONE).unwrap();
    assert!(!app.node_tail_dialog_active());
}

#[test]
fn preview_esc_closes_open_tail_dialog() {
    let mut app = app_with_open_tail();
    handle_preview_key(&mut app, KeyCode::Esc, KeyModifiers::NONE).unwrap();
    assert!(!app.node_tail_dialog_active());
}

#[test]
fn preview_t_with_no_highlighted_node_opens_the_pane() {
    // CT28: `t` toggles the pane in both modes. With nothing highlighted it
    // still opens (showing the waiting line), superseding CT3 decision 3
    // which refused non-running nodes.
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
    handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::NONE).unwrap();
    assert!(app.node_tail_dialog_active());
    handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::NONE).unwrap();
    assert!(!app.node_tail_dialog_active());
}

#[test]
fn preview_ctrl_t_still_opens_context_transfer_not_tail() {
    // Guard against the plain-`t` arm swallowing Ctrl+T: with no live
    // state the tail cannot open, and the Ctrl+T arm must run instead
    // (no-op here without an interactive agent, but crucially not a
    // tail dialog).
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
    handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL).unwrap();
    assert!(!app.node_tail_dialog_active());
}

#[test]
fn preview_page_up_down_scroll_open_tail_pane() {
    // CT28: Up/Down keep their graph meaning while the pane is open;
    // PageUp/PageDown scroll the pane body by half its height.
    let mut app = app_with_open_tail();
    app.node_tail_pane_body_height = 20;
    let before = app.node_tail_dialog.as_ref().unwrap().scroll;
    handle_preview_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE).unwrap();
    assert_eq!(
        app.node_tail_dialog.as_ref().unwrap().scroll,
        before + 10,
        "PageUp should scroll the pane body back by half height"
    );
    handle_preview_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE).unwrap();
    assert_eq!(
        app.node_tail_dialog.as_ref().unwrap().scroll,
        before,
        "PageDown should scroll forward by half height"
    );
}

#[test]
fn follow_mode_node_transition_switches_pane_run_and_resets_scroll() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
    seed_live_graph(&app.db, &["n1", "n2"]);
    insert_live_run(
        &app.db,
        "run-n1",
        "n1",
        crate::domain::graphs::GraphRunStatus::Running,
        Utc::now(),
    );
    insert_live_run(
        &app.db,
        "run-n2",
        "n2",
        crate::domain::graphs::GraphRunStatus::Running,
        Utc::now(),
    );
    let mut state = running_live_state();
    state.current_spec_id = Some("s1".to_string());
    state.current_node_id = Some("n1".to_string());
    app.graph_live_state = Some(state);
    app.graph_live_follow = true;

    app.open_node_tail_pane();
    assert_eq!(
        app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
        Some("run-n1")
    );
    app.node_tail_scroll_by(-7);
    assert_eq!(app.node_tail_dialog.as_ref().unwrap().scroll, 7);

    // The engine moves to the next node: auto-follow retargets the pane.
    app.graph_live_state.as_mut().unwrap().current_node_id = Some("n2".to_string());
    app.poll_node_tail_dialog();
    let dialog = app.node_tail_dialog.as_ref().unwrap();
    assert_eq!(
        dialog.run_id.as_deref(),
        Some("run-n2"),
        "the pane must follow the run to the next node"
    );
    assert_eq!(
        dialog.scroll, 0,
        "a node transition returns the pane to the live edge"
    );
}

#[test]
fn manual_enter_toggles_pane_on_selected_node() {
    // CT28 req 6: Enter in manual mode toggles the pane on the selected
    // node (follow mode keeps CT24's enter-manual behaviour — covered by
    // manual_down_moves_selection_and_pane_follows).
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
    seed_live_graph(&app.db, &["n1"]);
    insert_live_run(
        &app.db,
        "run-n1",
        "n1",
        crate::domain::graphs::GraphRunStatus::Running,
        Utc::now(),
    );
    let mut state = running_live_state();
    state.current_spec_id = Some("s1".to_string());
    state.current_node_id = Some("n1".to_string());
    app.graph_live_state = Some(state);
    app.graph_live_follow = false;
    app.graph_live_selected_node = Some("n1".to_string());

    handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(app.node_tail_dialog_active(), "Enter must open the pane");
    assert_eq!(
        app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
        Some("run-n1"),
        "the pane must bind to the selected node's run"
    );

    handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(!app.node_tail_dialog_active(), "Enter must close the pane");
}

#[test]
fn manual_down_moves_selection_and_pane_follows() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
    seed_live_graph(&app.db, &["n1", "n2"]);
    insert_live_run(
        &app.db,
        "run-n1",
        "n1",
        crate::domain::graphs::GraphRunStatus::Running,
        Utc::now(),
    );
    insert_live_run(
        &app.db,
        "run-n2",
        "n2",
        crate::domain::graphs::GraphRunStatus::Running,
        Utc::now(),
    );
    let mut state = running_live_state();
    state.current_spec_id = Some("s1".to_string());
    state.current_node_id = Some("n1".to_string());
    app.graph_live_state = Some(state);
    app.graph_live_follow = true;

    // Enter -> manual mode, seeded at the current node n1.
    handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(!app.graph_live_follow);
    app.open_node_tail_pane();
    assert_eq!(
        app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
        Some("run-n1")
    );

    // Down moves the manual selection; the pane follows on the next poll.
    handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();
    assert_eq!(app.graph_live_selected_node.as_deref(), Some("n2"));
    app.poll_node_tail_dialog();
    assert_eq!(
        app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
        Some("run-n2"),
        "in manual mode the pane shows the selected node's run"
    );
}

#[test]
fn ensemble_dot_cycles_through_all_three_members() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
    seed_live_graph(&app.db, &["m1", "m2", "m3", "j1"]);
    let base = Utc::now();
    insert_live_run(
        &app.db,
        "run-m1",
        "m1",
        crate::domain::graphs::GraphRunStatus::Running,
        base,
    );
    insert_live_run(
        &app.db,
        "run-m2",
        "m2",
        crate::domain::graphs::GraphRunStatus::Pass,
        base + chrono::Duration::seconds(9),
    );
    insert_live_run(
        &app.db,
        "run-m3",
        "m3",
        crate::domain::graphs::GraphRunStatus::Pass,
        base + chrono::Duration::seconds(2),
    );

    let mut state = running_live_state();
    state.current_spec_id = Some("s1".to_string());
    state.current_node_id = Some("m1".to_string());
    state.effective_nodes = ["m1", "m2", "m3", "j1"]
        .iter()
        .enumerate()
        .map(|(i, id)| crate::domain::graphs::GraphNode {
            id: id.to_string(),
            spec_id: Some("s1".to_string()),
            graph_id: None,
            name: format!("Node {id}"),
            kind: crate::domain::graphs::GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: i as i64,
            created_at: Utc::now(),
        })
        .collect();
    state.effective_edges = ["m1", "m2", "m3"]
        .iter()
        .map(|m| crate::domain::graphs::GraphEdge {
            id: format!("e-{m}"),
            spec_id: None,
            graph_id: Some("lp1".to_string()),
            from_node: (*m).to_string(),
            to_node: "j1".to_string(),
            condition: crate::domain::graphs::GraphEdgeCondition::Always,
        })
        .collect();
    state.ensembles = vec![crate::tui::app::graph_live_state::EnsembleLiveInfo {
        ensemble_id: "e1".to_string(),
        name: "Ensemble".to_string(),
        join_node_id: "j1".to_string(),
        members: ["m1", "m2", "m3"]
            .iter()
            .map(
                |m| crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
                    node_id: (*m).to_string(),
                    label: (*m).to_string(),
                    status: None,
                },
            )
            .collect(),
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
    }];
    app.graph_live_state = Some(state);
    app.graph_live_follow = true;

    app.open_node_tail_pane();
    assert_eq!(
        app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
        Some("run-m1"),
        "the running member is shown first"
    );

    handle_preview_key(&mut app, KeyCode::Char('.'), KeyModifiers::NONE).unwrap();
    app.poll_node_tail_dialog();
    assert_eq!(
        app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
        Some("run-m2"),
        "`.` steps to the newest finished member"
    );

    handle_preview_key(&mut app, KeyCode::Char('.'), KeyModifiers::NONE).unwrap();
    app.poll_node_tail_dialog();
    assert_eq!(
        app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
        Some("run-m3"),
        "`.` steps to the older finished member"
    );

    handle_preview_key(&mut app, KeyCode::Char('.'), KeyModifiers::NONE).unwrap();
    app.poll_node_tail_dialog();
    assert_eq!(
        app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
        Some("run-m1"),
        "`.` wraps back to the running member"
    );
}

#[test]
fn graph_reset_confirm_y_dispatches_reset_and_closes_the_modal() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
    app.graph_reset_confirm = true;
    handle_preview_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE).unwrap();
    assert!(!app.graph_reset_confirm);
    assert!(app.graph_action_pending);
}

#[test]
fn graph_reset_confirm_n_cancels_without_dispatching() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
    app.graph_reset_confirm = true;
    handle_preview_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
    assert!(!app.graph_reset_confirm);
    assert!(!app.graph_action_pending);
}

#[test]
fn preview_a_opens_autorun_dialog_regardless_of_status() {
    for status in [
        crate::domain::graphs::GraphStatus::Draft,
        crate::domain::graphs::GraphStatus::Running,
        crate::domain::graphs::GraphStatus::Paused,
        crate::domain::graphs::GraphStatus::Completed,
        crate::domain::graphs::GraphStatus::Failed,
    ] {
        let mut app = app_on_graph(status);
        handle_preview_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE).unwrap();
        assert!(app.graph_autorun_dialog.is_some(), "status {status:?}");
    }
}

#[test]
fn graph_autorun_dialog_quota_message_typing_backspace_and_submit() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
    handle_preview_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE).unwrap();
    // Tab switches into the free-text quota-message mode (the picker is
    // the default mode on open).
    handle_preview_key(&mut app, KeyCode::Tab, KeyModifiers::NONE).unwrap();

    handle_preview_key(&mut app, KeyCode::Char('1'), KeyModifiers::NONE).unwrap();
    handle_preview_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE).unwrap();
    handle_preview_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE).unwrap();
    assert_eq!(app.graph_autorun_dialog.as_ref().unwrap().quota_input, "1");

    handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(app.graph_autorun_dialog.is_none());
    assert!(app.graph_action_pending);
}

#[test]
fn graph_autorun_dialog_picker_mode_default_seed_is_rejected_on_submit() {
    // The picker seeds to "now" absent a pending autorun (see
    // `GraphAutorunDialog::new`), which is already past by the time
    // submit runs a moment later — the dialog must stay open with an
    // inline error rather than dispatch (requirement 7).
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
    handle_preview_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE).unwrap();
    handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(app.graph_autorun_dialog.is_some());
    assert!(!app.graph_action_pending);
    assert!(app.graph_autorun_dialog.as_ref().unwrap().error.is_some());
}

#[test]
fn graph_autorun_dialog_picker_mode_future_time_submits() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
    handle_preview_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE).unwrap();
    // Field 0 (year) is focused on open; bump it a year forward so the
    // picked time is unambiguously in the future.
    handle_preview_key(&mut app, KeyCode::Up, KeyModifiers::NONE).unwrap();
    handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(app.graph_autorun_dialog.is_none());
    assert!(app.graph_action_pending);
}

#[test]
fn graph_autorun_dialog_esc_closes_without_dispatching() {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
    handle_preview_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE).unwrap();
    handle_preview_key(&mut app, KeyCode::Esc, KeyModifiers::NONE).unwrap();
    assert!(app.graph_autorun_dialog.is_none());
    assert!(!app.graph_action_pending);
}

#[test]
fn graph_autorun_dialog_picker_arrows_and_digit_keep_editing() {
    use crate::tui::app::dialog::GraphAutorunMode;
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Failed);
    handle_preview_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE).unwrap();
    assert_eq!(
        app.graph_autorun_dialog.as_ref().unwrap().mode,
        GraphAutorunMode::Picker
    );

    // Move focus across fields, nudge the value, then type a digit: the
    // picker must stay open in Picker mode without dispatching.
    handle_preview_key(&mut app, KeyCode::Right, KeyModifiers::NONE).unwrap();
    handle_preview_key(&mut app, KeyCode::Left, KeyModifiers::NONE).unwrap();
    handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();
    handle_preview_key(&mut app, KeyCode::Up, KeyModifiers::NONE).unwrap();
    handle_preview_key(&mut app, KeyCode::Char('7'), KeyModifiers::NONE).unwrap();

    assert!(app.graph_autorun_dialog.is_some());
    assert_eq!(
        app.graph_autorun_dialog.as_ref().unwrap().mode,
        GraphAutorunMode::Picker
    );
    assert!(!app.graph_action_pending);
}

#[test]
fn entering_a_graph_lets_plain_arrows_navigate_it() {
    // CT23 FR2/FR3: before Enter, plain arrows must not touch the
    // graph's internal navigation; after Enter, they must.
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
    app.graph_live_state = Some(crate::tui::app::graph_live_state::GraphLiveState {
        graph_id: "lp1".to_string(),
        graph_name: "Nightly review".to_string(),
        graph_status: crate::domain::graphs::GraphStatus::Running,
        workdir: "/tmp".to_string(),
        trigger_type: "manual".to_string(),
        schedule_expr: None,
        watch_path: None,
        autorun_at: None,
        spec_queue: Vec::new(),
        done_count: 0,
        total_count: 0,
        current_spec_id: None,
        effective_nodes: vec![
            crate::domain::graphs::GraphNode {
                id: "n1".to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                name: "Implement".to_string(),
                kind: crate::domain::graphs::GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            crate::domain::graphs::GraphNode {
                id: "n2".to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                name: "Review".to_string(),
                kind: crate::domain::graphs::GraphNodeKind::Check,
                config: serde_json::json!({}),
                position: 1,
                created_at: Utc::now(),
            },
        ],
        effective_edges: vec![crate::domain::graphs::GraphEdge {
            id: "e1".to_string(),
            spec_id: None,
            graph_id: Some("lp1".to_string()),
            from_node: "n1".to_string(),
            to_node: "n2".to_string(),
            condition: crate::domain::graphs::GraphEdgeCondition::Pass,
        }],
        ensembles: Vec::new(),
        router_taken_routes: std::collections::HashMap::new(),
        current_node_id: Some("n1".to_string()),
        current_node_status: None,
        current_node_started_at: None,
        current_node_iteration: None,
    });

    assert!(app.graph_live_follow);
    handle_preview_key(&mut app, KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert!(
        app.graph_live_follow,
        "plain Right before Enter must not move the graph or leave auto-follow"
    );

    handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(!app.graph_live_follow, "Enter must switch to manual mode");

    handle_preview_key(&mut app, KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert_eq!(
        app.graph_live_selected_node.as_deref(),
        Some("n2"),
        "plain Right after entering must move to the child node"
    );
}

#[test]
fn esc_leaves_manual_navigation_back_to_auto() {
    // CT23 FR3/FR5: Esc must leave manual navigation, returning to auto.
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
    app.graph_live_state = Some(running_live_state());
    handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(!app.graph_live_follow, "Enter must switch to manual mode");

    handle_preview_key(&mut app, KeyCode::Esc, KeyModifiers::NONE).unwrap();
    assert!(
        app.graph_live_follow,
        "Esc must return the graph view to auto mode"
    );
}

#[test]
fn enter_on_completed_graph_seeds_manual_and_stays_manual() {
    // CT24 FR1/FR2: a finished graph has no engine current node, so Enter
    // must seed a manual selection (entry node) and the next refresh tick
    // must keep manual mode as long as that node still exists — never
    // flip back just because the graph is not running.
    let db = test_db();
    seed_completed_graph_with_nodes(&db);
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.focus = Focus::Preview;
    app.sidebar_layer = SidebarLayer::Automation;
    app.automation_kind = crate::tui::app::AutomationKind::Graph;
    app.graphs = vec![graph_with_status(
        crate::domain::graphs::GraphStatus::Completed,
    )];
    app.selected_graph_id = Some("lp1".to_string());
    app.graph_live_state = Some(crate::tui::app::graph_live_state::GraphLiveState {
        graph_id: "lp1".to_string(),
        graph_name: "Nightly review".to_string(),
        graph_status: crate::domain::graphs::GraphStatus::Completed,
        workdir: "/tmp".to_string(),
        trigger_type: "manual".to_string(),
        schedule_expr: None,
        watch_path: None,
        autorun_at: None,
        spec_queue: Vec::new(),
        done_count: 0,
        total_count: 0,
        current_spec_id: None,
        effective_nodes: vec![
            crate::domain::graphs::GraphNode {
                id: "n1".to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                name: "Implement".to_string(),
                kind: crate::domain::graphs::GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            crate::domain::graphs::GraphNode {
                id: "n2".to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                name: "Review".to_string(),
                kind: crate::domain::graphs::GraphNodeKind::Check,
                config: serde_json::json!({}),
                position: 1,
                created_at: Utc::now(),
            },
        ],
        effective_edges: vec![crate::domain::graphs::GraphEdge {
            id: "e1".to_string(),
            spec_id: None,
            graph_id: Some("lp1".to_string()),
            from_node: "n1".to_string(),
            to_node: "n2".to_string(),
            condition: crate::domain::graphs::GraphEdgeCondition::Pass,
        }],
        ensembles: Vec::new(),
        router_taken_routes: std::collections::HashMap::new(),
        current_node_id: None,
        current_node_status: None,
        current_node_started_at: None,
        current_node_iteration: None,
    });
    app.graph_live_follow = true;
    app.graph_live_selected_node = None;

    handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(!app.graph_live_follow, "Enter must switch to manual mode");
    assert!(
        app.graph_live_selected_node.is_some(),
        "a seed must exist on a finished graph"
    );

    app.refresh_graphs_selection();
    assert!(
        !app.graph_live_follow,
        "refresh must keep manual mode when the seed still exists"
    );

    let before = app.graph_live_selected_node.clone();
    handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();
    assert_ne!(
        app.graph_live_selected_node, before,
        "plain Down must move inside the graph"
    );
    assert_eq!(
        app.selected_graph_id.as_deref(),
        Some("lp1"),
        "plain Down must not move the sidebar to the next graph"
    );
}

#[test]
fn enter_on_a_paused_graph_seeds_the_entry_node() {
    // CT24 FR1 (c): with no runs to consult, Enter must seed the graph's
    // entry node (first effective node) — not stay in follow with a None
    // selection.
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Paused);
    app.graph_live_state = Some(crate::tui::app::graph_live_state::GraphLiveState {
        graph_id: "lp1".to_string(),
        graph_name: "Nightly review".to_string(),
        graph_status: crate::domain::graphs::GraphStatus::Paused,
        workdir: "/tmp".to_string(),
        trigger_type: "manual".to_string(),
        schedule_expr: None,
        watch_path: None,
        autorun_at: None,
        spec_queue: Vec::new(),
        done_count: 0,
        total_count: 0,
        current_spec_id: None,
        effective_nodes: vec![
            crate::domain::graphs::GraphNode {
                id: "n1".to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                name: "Implement".to_string(),
                kind: crate::domain::graphs::GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            crate::domain::graphs::GraphNode {
                id: "n2".to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                name: "Review".to_string(),
                kind: crate::domain::graphs::GraphNodeKind::Check,
                config: serde_json::json!({}),
                position: 1,
                created_at: Utc::now(),
            },
        ],
        effective_edges: vec![crate::domain::graphs::GraphEdge {
            id: "e1".to_string(),
            spec_id: None,
            graph_id: Some("lp1".to_string()),
            from_node: "n1".to_string(),
            to_node: "n2".to_string(),
            condition: crate::domain::graphs::GraphEdgeCondition::Pass,
        }],
        ensembles: Vec::new(),
        router_taken_routes: std::collections::HashMap::new(),
        current_node_id: None,
        current_node_status: None,
        current_node_started_at: None,
        current_node_iteration: None,
    });
    app.graph_live_follow = true;
    app.graph_live_selected_node = None;

    handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(!app.graph_live_follow, "Enter must switch to manual mode");
    assert_eq!(
        app.graph_live_selected_node.as_deref(),
        Some("n1"),
        "the entry node must be selected when there is no run to seed from"
    );
}

#[test]
fn shift_down_moves_between_graphs_without_entering_either() {
    // CT23 FR1/FR5: Shift+Down must move the sidebar selection to the
    // next graph, not navigate inside the first one, and the
    // destination must land in auto mode (not entered).
    let mut app = app_on_two_graphs();

    handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::SHIFT).unwrap();

    assert_eq!(app.selected_graph_id.as_deref(), Some("g2"));
    assert!(
        app.graph_live_follow,
        "landing on the next graph must not be in manual/entered mode"
    );
}

#[test]
fn shift_down_reaches_next_graph_even_while_entered() {
    // CT23 FR1: once a graph is entered (manual navigation active),
    // Shift+Down must still move to the next graph — the previous bug
    // captured it as internal sibling navigation instead.
    let mut app = app_on_two_graphs();
    app.graph_live_state = Some(running_live_state());

    handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(!app.graph_live_follow, "Enter must switch to manual mode");

    handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::SHIFT).unwrap();

    assert_eq!(app.selected_graph_id.as_deref(), Some("g2"));
}

#[test]
fn preview_ctrl_t_opens_context_transfer_for_interactive_selection() {
    let mut app = app_with_agents();
    app.interactive_agents = vec![spawn_cat_agent("interactive-1")];
    app.agents = vec![AgentEntry::Interactive(0)];
    app.selected = 0;
    app.focus = Focus::Preview;

    handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL).unwrap();

    assert!(matches!(app.focus, Focus::ContextTransfer));
    app.interactive_agents[0].kill();
}

#[test]
fn preview_ctrl_t_does_nothing_for_non_session_selection() {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;

    handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL).unwrap();

    assert!(matches!(app.focus, Focus::Preview));
}

#[test]
fn preview_ctrl_t_opens_rag_transfer_not_context_transfer_while_playground_active() {
    let mut app = app_with_agents();
    app.interactive_agents = vec![spawn_cat_agent("interactive-2")];
    app.agents = vec![AgentEntry::Interactive(0)];
    app.selected = 0;
    app.focus = Focus::Preview;
    app.playground_active = true;

    handle_preview_key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL).unwrap();

    assert!(!matches!(app.focus, Focus::ContextTransfer));
    app.interactive_agents[0].kill();
}

#[test]
fn preview_down_navigates_despite_claimed_selected_child() {
    // The selected session negotiated Kitty (Codex shape); plain Down
    // must still move the preview selection, not reach the child.
    let mut app = app_with_two_claimed_interactive_sessions();

    handle_preview_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();

    assert_eq!(app.selected, 1);
    assert!(matches!(app.focus, Focus::Preview));
    app.interactive_agents[0].kill();
    app.interactive_agents[1].kill();
}

#[test]
fn preview_up_navigates_despite_claimed_selected_child() {
    // The selected session entered the alternate screen (vim shape);
    // plain Up must still move the preview selection.
    let mut app = app_with_two_claimed_interactive_sessions();
    app.selected = 1;

    handle_preview_key(&mut app, KeyCode::Up, KeyModifiers::NONE).unwrap();

    assert_eq!(app.selected, 0);
    assert!(matches!(app.focus, Focus::Preview));
    app.interactive_agents[0].kill();
    app.interactive_agents[1].kill();
}

#[test]
fn preview_enter_on_claimed_child_still_enters_focus() {
    // Entering focus by keyboard is unchanged: log scroll resets and
    // focus becomes Agent, exactly as for an unclaimed child.
    let mut app = app_with_two_claimed_interactive_sessions();
    app.log_scroll = 9;

    handle_preview_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();

    assert!(matches!(app.focus, Focus::Agent));
    assert_eq!(app.log_scroll, 0);
    app.interactive_agents[0].kill();
    app.interactive_agents[1].kill();
}
