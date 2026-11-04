//! Tests for the `send_to_session` executor path (CM53).

use crate::application::notification_service::DefaultNotificationService;
use crate::application::ports::{AgentRepository, RunRepository};
use crate::db::Database;
use crate::domain::models::{Agent, AgentAction, Cli, RunStatus, Trigger};
use crate::executor::Executor;
use chrono::Utc;
use std::path::Path;
use std::sync::Arc;

fn test_db() -> Database {
    let dir = tempfile::tempdir().unwrap();
    Database::new(&dir.path().join("test.db")).unwrap()
}

fn add_session(db: &Database, id: &str, name: &str) {
    db.insert_interactive_session(
        id,
        name,
        "opencode",
        "/tmp/proj",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
}

fn send_agent(id: &str, name: &str, log_path: &Path) -> Agent {
    Agent {
        id: id.to_string(),
        prompt: "report status".to_string(),
        trigger: Some(Trigger::Cron {
            schedule_expr: "0 * * * *".to_string(),
        }),
        cli: Cli(String::new()),
        model: None,
        effort: None,
        working_dir: Some("/tmp/proj".to_string()),
        enabled: true,
        enable_at: None,
        created_at: Utc::now(),
        log_path: log_path.to_string_lossy().to_string(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: AgentAction::send_to_session(Some(name.to_string()), None).unwrap(),
    }
}

fn executor(db: &Arc<Database>) -> Executor {
    Executor::new(db.clone(), Arc::new(DefaultNotificationService))
}

fn pending(db: &Database, session_id: &str) -> Vec<crate::db::scheduled_prompts::ScheduledPrompt> {
    db.list_pending_scheduled_prompts_for_session(session_id)
        .unwrap()
}

#[tokio::test]
async fn cron_fire_enqueues_one_send_with_agent_trigger_provenance() {
    let db = Arc::new(test_db());
    add_session(&db, "orch-id", "orch");
    let log_dir = tempfile::tempdir().unwrap();
    let agent = send_agent("nudger", "orch", &log_dir.path().join("a.log"));
    db.upsert_agent(&agent).unwrap();
    let exec = executor(&db);

    let before = Utc::now();
    let code = exec.execute_agent(&agent, false).await.unwrap();
    assert_eq!(code, 0);

    let sends = pending(&db, "orch-id");
    assert_eq!(sends.len(), 1);
    let prov = sends[0].provenance.as_ref().unwrap();
    assert_eq!(prov.kind, "agent_trigger");
    assert_eq!(prov.agent_id.as_deref(), Some("nudger"));
    assert_eq!(prov.trigger.as_deref(), Some("cron"));
    assert!(sends[0].fire_at <= before + chrono::Duration::seconds(2));
    assert_eq!(sends[0].workdir.as_deref(), Some("/tmp/proj"));
}

#[tokio::test]
async fn watch_fire_carries_changed_path_and_event() {
    let db = Arc::new(test_db());
    add_session(&db, "orch-id", "orch");
    let log_dir = tempfile::tempdir().unwrap();
    let mut agent = send_agent("watcher", "orch", &log_dir.path().join("w.log"));
    db.upsert_agent(&agent).unwrap();
    agent.trigger = Some(Trigger::Watch {
        path: "/tmp/x".to_string(),
        events: vec![crate::domain::models::WatchEvent::Modify],
        debounce_seconds: 2,
        recursive: false,
    });
    let exec = executor(&db);

    let code = exec
        .execute_agent_with_context(&agent, "/tmp/x/spec.md", "modify")
        .await
        .unwrap();
    assert_eq!(code, 0);

    let sends = pending(&db, "orch-id");
    assert_eq!(sends.len(), 1);
    assert!(
        sends[0]
            .prompt
            .ends_with("Changed path: /tmp/x/spec.md\nEvent: modify"),
        "prompt must end with the watch block, got:\n{}",
        sends[0].prompt
    );
    assert_eq!(
        sends[0].provenance.as_ref().unwrap().trigger.as_deref(),
        Some("watch")
    );
}

#[tokio::test]
async fn second_fire_coalesces_into_first_undelivered_send() {
    let db = Arc::new(test_db());
    add_session(&db, "orch-id", "orch");
    let log_dir = tempfile::tempdir().unwrap();
    let mut agent = send_agent("nudger", "orch", &log_dir.path().join("a.log"));
    db.upsert_agent(&agent).unwrap();
    agent.prompt = "first prompt".to_string();
    let exec = executor(&db);

    exec.execute_agent(&agent, false).await.unwrap();
    let first = pending(&db, "orch-id");
    assert_eq!(first.len(), 1);
    let first_id = first[0].id.clone();

    let mut second = agent.clone();
    second.prompt = "second prompt".to_string();
    exec.execute_agent(&second, false).await.unwrap();

    let sends = pending(&db, "orch-id");
    assert_eq!(sends.len(), 1, "a second fire must coalesce, not pile up");
    assert_eq!(sends[0].id, first_id);
    assert_eq!(sends[0].prompt, "second prompt");

    let latest = db.list_runs("nudger", 1).unwrap();
    assert!(
        latest[0]
            .summary
            .as_deref()
            .unwrap_or("")
            .contains("Coalesced"),
        "run summary must say it coalesced, got: {:?}",
        latest[0].summary
    );
}

#[tokio::test]
async fn missing_name_fails_loudly_and_enqueues_nothing() {
    let db = Arc::new(test_db());
    add_session(&db, "orch-id", "orch");
    let log_dir = tempfile::tempdir().unwrap();
    let agent = send_agent("nudger", "nobody", &log_dir.path().join("a.log"));
    db.upsert_agent(&agent).unwrap();
    let exec = executor(&db);

    let code = exec.execute_agent(&agent, false).await.unwrap();
    assert_eq!(code, 1);
    assert!(pending(&db, "orch-id").is_empty());

    let latest = db.list_runs("nudger", 1).unwrap();
    assert_eq!(latest[0].status, RunStatus::Error);
    let summary = latest[0].summary.clone().unwrap_or_default();
    assert!(summary.contains("nobody"), "summary: {summary}");
    assert!(
        summary.contains("no live session has this name"),
        "summary: {summary}"
    );
    assert!(
        summary.contains("`agent_edit update`"),
        "summary: {summary}"
    );

    assert_eq!(
        db.get_agent("nudger").unwrap().unwrap().last_run_ok,
        Some(false)
    );
}

#[tokio::test]
async fn ambiguous_name_fails_and_names_both_ids() {
    let db = Arc::new(test_db());
    add_session(&db, "orch-1", "orch");
    add_session(&db, "orch-2", "orch");
    let log_dir = tempfile::tempdir().unwrap();
    let agent = send_agent("nudger", "orch", &log_dir.path().join("a.log"));
    db.upsert_agent(&agent).unwrap();
    let exec = executor(&db);

    let code = exec.execute_agent(&agent, false).await.unwrap();
    assert_eq!(code, 1);
    assert!(pending(&db, "orch-1").is_empty());
    assert!(pending(&db, "orch-2").is_empty());

    let latest = db.list_runs("nudger", 1).unwrap();
    assert_eq!(latest[0].status, RunStatus::Error);
    let summary = latest[0].summary.clone().unwrap_or_default();
    assert!(summary.contains("orch-1"), "summary: {summary}");
    assert!(summary.contains("orch-2"), "summary: {summary}");
}

#[tokio::test]
async fn send_to_session_never_spawns_a_process() {
    let db = Arc::new(test_db());
    add_session(&db, "orch-id", "orch");
    let log_dir = tempfile::tempdir().unwrap();
    let log_path = log_dir.path().join("a.log");
    let agent = send_agent("nudger", "orch", &log_path);
    db.upsert_agent(&agent).unwrap();
    // An empty CLI name would fail binary resolution if a process were spawned.
    assert_eq!(agent.cli.as_str(), "");
    let exec = executor(&db);

    let code = exec.execute_agent(&agent, false).await.unwrap();
    assert_eq!(code, 0);

    let latest = db.list_runs("nudger", 1).unwrap();
    assert_eq!(latest[0].status, RunStatus::Success);

    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        !log.contains("Failed to resolve") && !log.contains("build command"),
        "send path must not spawn a process; log:\n{log}"
    );

    // Cron fires do not touch trigger_count (same contract as headless cron).
    assert_eq!(db.get_agent("nudger").unwrap().unwrap().trigger_count, 0);
    assert_eq!(
        db.get_agent("nudger").unwrap().unwrap().last_run_ok,
        Some(true)
    );
}

#[tokio::test]
async fn agent_run_manual_send_delivers_once() {
    let db = Arc::new(test_db());
    add_session(&db, "orch-id", "orch");
    let log_dir = tempfile::tempdir().unwrap();
    let agent = send_agent("nudger", "orch", &log_dir.path().join("a.log"));
    db.upsert_agent(&agent).unwrap();
    let exec = executor(&db);

    let code = exec.execute_agent(&agent, true).await.unwrap();
    assert_eq!(code, 0);
    assert_eq!(pending(&db, "orch-id").len(), 1);
    assert_eq!(db.list_runs("nudger", 1).unwrap().len(), 1);
}

#[tokio::test]
async fn disabled_send_agent_is_skipped() {
    let db = Arc::new(test_db());
    add_session(&db, "orch-id", "orch");
    let log_dir = tempfile::tempdir().unwrap();
    let mut agent = send_agent("nudger", "orch", &log_dir.path().join("a.log"));
    db.upsert_agent(&agent).unwrap();
    agent.enabled = false;
    let exec = executor(&db);

    let code = exec.execute_agent(&agent, false).await.unwrap();
    assert_eq!(code, -1);
    assert!(pending(&db, "orch-id").is_empty());
}
