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
        log_path: "/tmp/test-home.log".to_string(),
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
fn home_f10_shows_quit_confirm() {
    let mut app = app_with_agents();
    app.focus = Focus::Home;
    handle_home_key(&mut app, KeyCode::F(10), KeyModifiers::NONE).unwrap();
    assert!(app.quit_confirm);
    assert!(app.running);
}

#[test]
fn home_quit_confirm_y_exits() {
    let mut app = app_with_agents();
    app.focus = Focus::Home;
    app.quit_confirm = true;
    handle_home_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE).unwrap();
    assert!(!app.running);
}

#[test]
fn home_quit_confirm_enter_exits() {
    let mut app = app_with_agents();
    app.focus = Focus::Home;
    app.quit_confirm = true;
    handle_home_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(!app.running);
}

#[test]
fn home_quit_confirm_other_key_cancels() {
    let mut app = app_with_agents();
    app.focus = Focus::Home;
    app.quit_confirm = true;
    handle_home_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
    assert!(!app.quit_confirm);
    assert!(app.running);
}

#[test]
fn home_down_moves_to_preview() {
    let mut app = app_with_agents();
    app.focus = Focus::Home;
    handle_home_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();
    assert!(matches!(app.focus, Focus::Preview));
}

#[test]
fn home_up_moves_to_preview() {
    let mut app = app_with_agents();
    app.focus = Focus::Home;
    handle_home_key(&mut app, KeyCode::Up, KeyModifiers::NONE).unwrap();
    assert!(matches!(app.focus, Focus::Preview));
}

#[test]
fn home_j_moves_to_preview() {
    let mut app = app_with_agents();
    app.focus = Focus::Home;
    handle_home_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE).unwrap();
    assert!(matches!(app.focus, Focus::Preview));
}

#[test]
fn home_k_moves_to_preview() {
    let mut app = app_with_agents();
    app.focus = Focus::Home;
    handle_home_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE).unwrap();
    assert!(matches!(app.focus, Focus::Preview));
}

#[test]
fn home_enter_moves_to_preview() {
    let mut app = app_with_agents();
    app.focus = Focus::Home;
    handle_home_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(matches!(app.focus, Focus::Preview));
}

#[test]
fn home_n_opens_new_agent_dialog() {
    let mut app = app_with_agents();
    app.focus = Focus::Home;
    handle_home_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE).unwrap();
    assert!(matches!(app.focus, Focus::NewAgentDialog));
}

#[test]
fn home_f1_shows_legend() {
    let mut app = app_with_agents();
    app.focus = Focus::Home;
    app.hub_open = false;
    handle_home_key(&mut app, KeyCode::F(1), KeyModifiers::NONE).unwrap();
    assert!(app.hub_open);
}

#[test]
fn home_esc_shows_quit_confirm() {
    let mut app = app_with_agents();
    app.focus = Focus::Home;
    handle_home_key(&mut app, KeyCode::Esc, KeyModifiers::NONE).unwrap();
    assert!(app.quit_confirm);
}
