use crate::db::Database;
use crate::tui::agent::{AgentStatus, InteractiveAgent};
use crate::tui::app::agents::SessionTarget;
use crate::tui::app::types::{AgentEntry, App};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

struct FailListAgentsGuard;
impl Drop for FailListAgentsGuard {
    fn drop(&mut self) {
        crate::db::agent::set_fail_list_agents(false);
    }
}

fn test_app() -> (App, tempfile::TempDir) {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(Database::new(&path).expect("create test db"));
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        db,
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    (app, data_dir)
}

fn spawn_terminal(name: &str) -> InteractiveAgent {
    InteractiveAgent::spawn_terminal(
        "cat",
        "/tmp",
        80,
        24,
        Some(name),
        &[],
        ratatui::style::Color::White,
    )
    .expect("spawn terminal")
}

fn draw_frame(app: &mut App) {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, app))
        .expect("draw");
}

fn assert_selected_in_range(app: &App) {
    if app.agents.is_empty() {
        assert_eq!(app.selected, 0);
    } else {
        assert!(
            app.selected < app.agents.len(),
            "selected {} out of {}",
            app.selected,
            app.agents.len()
        );
    }
}

fn assert_no_stale_session_indices(app: &App) {
    for entry in &app.agents {
        match entry {
            AgentEntry::Interactive(idx) => assert!(
                *idx < app.interactive_agents.len(),
                "stale Interactive({idx}) against {} sessions",
                app.interactive_agents.len()
            ),
            AgentEntry::Terminal(idx) => assert!(
                *idx < app.terminal_agents.len(),
                "stale Terminal({idx}) against {} sessions",
                app.terminal_agents.len()
            ),
            AgentEntry::Orphaned(idx) => assert!(
                *idx < app.orphaned_sessions.len(),
                "stale Orphaned({idx}) against {} sessions",
                app.orphaned_sessions.len()
            ),
            _ => {}
        }
    }
}

#[test]
fn refresh_agents_failure_rebuilds_session_entries_from_current_lists() {
    let (mut app, _dir) = test_app();
    let agent = spawn_terminal("gone");
    let id = agent.id.clone();
    app.interactive_agents.push(agent);
    app.agents = vec![AgentEntry::Interactive(0)];
    app.selected = 0;
    // The list shrinks before the failing refresh (the close path).
    app.interactive_agents.clear();

    crate::db::agent::set_fail_list_agents(true);
    let _guard = FailListAgentsGuard;
    let result = app.refresh_agents();
    drop(_guard);

    assert!(result.is_err(), "the injected failure must surface");
    assert!(
        app.agents.is_empty(),
        "a failed refresh must still drop the removed session's entry ({} left)",
        app.agents.len()
    );
    assert_no_stale_session_indices(&app);
    assert_selected_in_range(&app);
    crate::sessions::client::test_client().hub.kill(&id);
}

#[test]
fn close_last_interactive_while_list_agents_fails_draws_without_panic() {
    let (mut app, _dir) = test_app();
    let agent = spawn_terminal("last");
    let id = agent.id.clone();
    app.interactive_agents.push(agent);
    app.agents = vec![AgentEntry::Interactive(0)];
    app.selected = 0;

    crate::db::agent::set_fail_list_agents(true);
    let _guard = FailListAgentsGuard;
    let _ = app.close_session_target(SessionTarget::Interactive(0), 0);
    app.finish_session_mutation();
    drop(_guard);

    // The failed refresh must still rebuild the sidebar entries from the
    // (now empty) session lists instead of keeping the stale Interactive(0).
    assert!(
        app.agents.is_empty(),
        "a failed refresh must still drop the closed session's entry ({} left)",
        app.agents.len()
    );
    assert_no_stale_session_indices(&app);
    draw_frame(&mut app);
    assert_selected_in_range(&app);
    crate::sessions::client::test_client().hub.kill(&id);
}

#[test]
fn close_middle_interactive_rebuilds_sidebar_indices() {
    let (mut app, _dir) = test_app();
    let a = spawn_terminal("a");
    let b = spawn_terminal("b");
    let c = spawn_terminal("c");
    let ids = [a.id.clone(), b.id.clone(), c.id.clone()];
    app.interactive_agents.extend([a, b, c]);
    app.agents = vec![
        AgentEntry::Interactive(0),
        AgentEntry::Interactive(1),
        AgentEntry::Interactive(2),
    ];
    app.selected = 1;

    let _ = app.close_session_target(SessionTarget::Interactive(1), 0);
    app.finish_session_mutation();

    // Without the reindex the sidebar still holds Interactive(2) against a
    // two-element list — the next draw indexes one past the end.
    let stale: Vec<usize> = app
        .agents
        .iter()
        .filter_map(|entry| match entry {
            AgentEntry::Interactive(idx) => Some(*idx),
            _ => None,
        })
        .filter(|idx| *idx >= app.interactive_agents.len())
        .collect();
    assert!(
        stale.is_empty(),
        "stale sidebar indices after close: {stale:?}"
    );
    assert_no_stale_session_indices(&app);
    draw_frame(&mut app);
    assert_selected_in_range(&app);
    assert_eq!(app.interactive_agents.len(), 2);
    assert_eq!(
        app.agents
            .iter()
            .filter(|entry| matches!(entry, AgentEntry::Interactive(_)))
            .count(),
        2,
        "sidebar must hold exactly the two surviving sessions"
    );
    for id in ids {
        crate::sessions::client::test_client().hub.kill(&id);
    }
}

#[test]
fn close_two_interactive_in_one_tick_draws_without_panic() {
    let (mut app, _dir) = test_app();
    let a = spawn_terminal("a");
    let b = spawn_terminal("b");
    let c = spawn_terminal("c");
    let ids = [a.id.clone(), b.id.clone(), c.id.clone()];
    app.interactive_agents.extend([a, b, c]);
    app.agents = vec![
        AgentEntry::Interactive(0),
        AgentEntry::Interactive(1),
        AgentEntry::Interactive(2),
    ];
    app.selected = 2;

    // Remove two before refresh/draw (same tick mutation burst).
    let _ = app.close_session_target(SessionTarget::Interactive(2), 0);
    let _ = app.close_session_target(SessionTarget::Interactive(1), 0);
    app.finish_session_mutation();
    assert_no_stale_session_indices(&app);
    draw_frame(&mut app);
    assert_selected_in_range(&app);
    assert_eq!(app.interactive_agents.len(), 1);
    for id in ids {
        crate::sessions::client::test_client().hub.kill(&id);
    }
}

#[test]
fn orphaned_dismiss_with_stale_index_draws_without_panic() {
    let (mut app, _dir) = test_app();
    app.orphaned_sessions.clear();
    app.agents = vec![AgentEntry::Orphaned(5)];
    app.selected = 0;

    app.dismiss_selected_orphaned_session();
    assert_no_stale_session_indices(&app);
    draw_frame(&mut app);
    assert_selected_in_range(&app);
    assert!(app.orphaned_sessions.is_empty());
}

#[test]
fn hub_session_removed_last_interactive_same_tick_draws_dropped() {
    let (mut app, _dir) = test_app();
    let agent = spawn_terminal("drop-me");
    let id = agent.id.clone();
    app.interactive_agents.push(agent);
    let _ = app.refresh_agents();
    app.selected = app
        .agents
        .iter()
        .position(|e| matches!(e, AgentEntry::Interactive(_)))
        .unwrap_or(0);

    app.handle_hub_frame(crate::sessions::protocol::HostFrame::SessionRemoved { id: id.clone() });
    assert_no_stale_session_indices(&app);
    draw_frame(&mut app);
    assert_selected_in_range(&app);
    assert!(app.interactive_agents.is_empty());
    crate::sessions::client::test_client().hub.kill(&id);
}

#[test]
fn hub_session_removed_last_interactive_same_tick_draws_retained_failed() {
    let (mut app, _dir) = test_app();
    let mut agent = spawn_terminal("keep-me");
    let id = agent.id.clone();
    agent.status = AgentStatus::Exited(1);
    agent.exit_detail = Some("launch failed".to_string());
    app.interactive_agents.push(agent);
    let _ = app.refresh_agents();
    app.selected = app
        .agents
        .iter()
        .position(|e| matches!(e, AgentEntry::Interactive(_)))
        .unwrap_or(0);

    app.handle_hub_frame(crate::sessions::protocol::HostFrame::SessionRemoved { id: id.clone() });
    assert_no_stale_session_indices(&app);
    draw_frame(&mut app);
    assert_selected_in_range(&app);
    assert_eq!(
        app.interactive_agents.len(),
        1,
        "failed session should be retained"
    );
    crate::sessions::client::test_client().hub.kill(&id);
}

#[test]
fn nonzero_exit_at_launch_removed_same_tick_draws_without_panic() {
    let (mut app, _dir) = test_app();
    let agent = spawn_terminal("fast-fail");
    let id = agent.id.clone();
    // Leave status running / no exit_detail so poll() yields Exited(nonzero)
    // with no detail (retain false) when the removal is processed.
    app.interactive_agents.push(agent);
    let _ = app.refresh_agents();
    app.selected = 0;

    app.handle_hub_frame(crate::sessions::protocol::HostFrame::Exited {
        id: id.clone(),
        code: 2,
    });
    app.handle_hub_frame(crate::sessions::protocol::HostFrame::SessionRemoved { id: id.clone() });
    assert_no_stale_session_indices(&app);
    draw_frame(&mut app);
    assert_selected_in_range(&app);
    assert!(
        app.interactive_agents.is_empty(),
        "nonzero exit at launch without detail should be removed, not retained"
    );
    crate::sessions::client::test_client().hub.kill(&id);
}

#[test]
fn panic_report_contains_location() {
    let text = crate::tui::panic_hook::format_panic_report(
        "src/tui/ui/sidebar/agent_cards.rs:277:48",
        "boom",
        "bt",
    );
    assert!(
        text.contains("location: src/tui/ui/sidebar/agent_cards.rs:277:48"),
        "{text}"
    );
}
