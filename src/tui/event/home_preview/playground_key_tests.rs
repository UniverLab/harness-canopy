use super::*;
use crate::db::Database;
use crate::domain::models::{Agent, Cli, Trigger};
use crate::tui::app::types::App;
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
        log_path: "/tmp/test-playground.log".to_string(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    }
}

fn app_with_agents() -> App {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
    app.selected = 0;
    app
}

#[test]
fn playground_inactive_returns_false() {
    let mut app = app_with_agents();
    app.playground_active = false;
    assert!(!handle_playground_key(
        &mut app,
        KeyCode::Char('a'),
        KeyModifiers::NONE
    ));
}

#[test]
fn playground_active_returns_true_for_any_key() {
    let mut app = app_with_agents();
    app.playground_active = true;
    assert!(handle_playground_key(
        &mut app,
        KeyCode::Char('a'),
        KeyModifiers::NONE
    ));
}

#[test]
fn playground_f10_deactivates() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_selected = 2;
    handle_playground_key(&mut app, KeyCode::F(10), KeyModifiers::NONE);
    assert!(!app.playground_active);
    assert!(matches!(app.focus, Focus::Preview));
}

#[test]
fn playground_detail_mode_esc_closes_detail() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_detail_mode = true;
    app.playground_scroll = 5;
    handle_playground_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.playground_detail_mode);
    assert_eq!(app.playground_scroll, 0);
    assert!(app.playground_active, "should stay active");
}

#[test]
fn playground_detail_mode_f10_closes_detail() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_detail_mode = true;
    handle_playground_key(&mut app, KeyCode::F(10), KeyModifiers::NONE);
    assert!(!app.playground_detail_mode);
}

#[test]
fn playground_up_navigates_results() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_selected = 2;
    app.playground_results = (0..4)
        .map(|i| crate::rag::vector_store::SearchResult {
            id: format!("id{i}"),
            file_path: format!("r{i}"),
            content: format!("r{i}"),
            created_at: 0,
            distance: None,
        })
        .collect();
    handle_playground_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.playground_selected, 1);
}

#[test]
fn playground_up_at_zero_stays() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_selected = 0;
    app.playground_results = vec![crate::rag::vector_store::SearchResult {
        id: "id0".into(),
        file_path: "r0".into(),
        content: "r0".into(),
        created_at: 0,
        distance: None,
    }];
    handle_playground_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.playground_selected, 0);
}

#[test]
fn playground_down_navigates_results() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_selected = 0;
    app.playground_results = (0..3)
        .map(|i| crate::rag::vector_store::SearchResult {
            id: format!("id{i}"),
            file_path: format!("r{i}"),
            content: format!("r{i}"),
            created_at: 0,
            distance: None,
        })
        .collect();
    handle_playground_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.playground_selected, 1);
}

#[test]
fn playground_down_at_end_stays() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_results = vec![crate::rag::vector_store::SearchResult {
        id: "id0".into(),
        file_path: "r0".into(),
        content: "r0".into(),
        created_at: 0,
        distance: None,
    }];
    app.playground_selected = 0;
    handle_playground_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.playground_selected, 0);
}

#[test]
fn playground_backspace_removes_char() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_query = "hello".into();
    handle_playground_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(app.playground_query, "hell");
    assert!(!app.playground_search_pending);
}

#[test]
fn playground_backspace_empty_clears_results() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_query.clear();
    app.playground_results = vec![crate::rag::vector_store::SearchResult {
        id: "id1".into(),
        file_path: "r1".into(),
        content: "r1".into(),
        created_at: 0,
        distance: None,
    }];
    app.playground_selected = 2;
    handle_playground_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert!(app.playground_results.is_empty());
    assert_eq!(app.playground_selected, 0);
}

#[test]
fn playground_char_appends_to_query() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_query.clear();
    handle_playground_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(app.playground_query, "x");
    assert!(app.playground_search_pending);
}

#[test]
fn playground_ctrl_char_ignored() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_query.clear();
    handle_playground_key(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert!(app.playground_query.is_empty());
}

#[test]
fn playground_detail_shift_up_deactivates() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_detail_mode = true;
    app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
    app.selected = 0;
    app.focus = Focus::Agent;
    handle_playground_key(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
    assert!(!app.playground_active);
}

#[test]
fn playground_detail_shift_down_deactivates() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_detail_mode = true;
    app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
    app.selected = 0;
    app.focus = Focus::Agent;
    handle_playground_key(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
    assert!(!app.playground_active);
}

#[test]
fn playground_non_detail_shift_up_deactivates() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_detail_mode = false;
    app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
    app.selected = 0;
    app.focus = Focus::Agent;
    handle_playground_key(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
    assert!(!app.playground_active);
}

#[test]
fn playground_detail_scroll_down() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_detail_mode = true;
    app.playground_scroll = 0;
    handle_playground_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.playground_scroll, 3);
}

#[test]
fn playground_detail_scroll_up() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_detail_mode = true;
    app.playground_scroll = 5;
    handle_playground_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.playground_scroll, 2);
}

#[test]
fn playground_detail_scroll_up_saturates() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_detail_mode = true;
    app.playground_scroll = 1;
    handle_playground_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.playground_scroll, 0);
}

#[test]
fn playground_enter_triggers_search_when_query_changed() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_query = "test".into();
    app.playground_last_executed_query = "other".into();
    handle_playground_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.playground_search_pending);
}

#[test]
fn playground_enter_opens_detail_when_results_match() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_results = vec![crate::rag::vector_store::SearchResult {
        id: "id1".into(),
        file_path: "r1".into(),
        content: "r1".into(),
        created_at: 0,
        distance: None,
    }];
    app.playground_query = "r1".into();
    app.playground_last_executed_query = "r1".into();
    handle_playground_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.playground_detail_mode);
}

#[test]
fn playground_ctrl_t_opens_rag_transfer() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_results = vec![crate::rag::vector_store::SearchResult {
        id: "id1".into(),
        file_path: "r1".into(),
        content: "r1".into(),
        created_at: 0,
        distance: None,
    }];
    handle_playground_key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert!(app.rag_transfer_modal.is_some());
}

#[test]
fn playground_detail_ctrl_t_opens_rag_transfer() {
    let mut app = app_with_agents();
    app.playground_active = true;
    app.playground_detail_mode = true;
    app.playground_results = vec![crate::rag::vector_store::SearchResult {
        id: "id1".into(),
        file_path: "r1".into(),
        content: "r1".into(),
        created_at: 0,
        distance: None,
    }];
    handle_playground_key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert!(app.rag_transfer_modal.is_some());
}
