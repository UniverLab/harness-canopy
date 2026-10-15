//! Tests for the CM53 `agents.action` column: decode, round-trip, refusal,
//! corruption quarantine, and migration idempotency.

use super::*;
use crate::domain::models::{Agent, AgentAction, Cli, Trigger};
use chrono::Utc;

fn test_db() -> Database {
    let dir = tempfile::tempdir().unwrap();
    Database::new(&dir.path().join("test.db")).unwrap()
}

fn base_agent(id: &str) -> Agent {
    Agent {
        id: id.to_string(),
        prompt: "p".to_string(),
        trigger: Some(Trigger::Cron {
            schedule_expr: "0 9 * * *".to_string(),
        }),
        cli: Cli::new("opencode"),
        model: None,
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: Utc::now(),
        log_path: format!("/tmp/{id}.log"),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: AgentAction::Headless,
    }
}

#[test]
fn old_row_without_action_column_decodes_as_headless() {
    let db = test_db();
    {
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO agents (id, prompt, trigger_type, cli, enabled, created_at, log_path, timeout_minutes, trigger_count)
             VALUES ('legacy', 'legacy prompt', 'cron', 'opencode', 1, ?1, '/tmp/legacy.log', 15, 0)",
            rusqlite::params![Utc::now().to_rfc3339()],
        )
        .unwrap();
    }
    let agent = db.get_agent("legacy").unwrap().unwrap();
    assert_eq!(agent.action, AgentAction::Headless);
    assert_eq!(agent.prompt, "legacy prompt");
    assert_eq!(agent.cli.as_str(), "opencode");
}

#[test]
fn send_to_session_round_trips_name_target() {
    let db = test_db();
    let mut agent = base_agent("by-name");
    agent.action = AgentAction::send_to_session(Some("orch".to_string()), None).unwrap();
    db.upsert_agent(&agent).unwrap();

    let read = db.get_agent("by-name").unwrap().unwrap();
    assert_eq!(
        read.action,
        AgentAction::SendToSession {
            target_session_name: Some("orch".to_string()),
            target_session_id: None,
        }
    );
    assert_eq!(read.action.target_session_name(), Some("orch"));
    assert!(read.action.target_session_id().is_none());
}

#[test]
fn send_to_session_round_trips_id_target() {
    let db = test_db();
    let mut agent = base_agent("by-id");
    agent.action = AgentAction::send_to_session(None, Some("sess-1".to_string())).unwrap();
    db.upsert_agent(&agent).unwrap();

    let read = db.get_agent("by-id").unwrap().unwrap();
    assert_eq!(read.action.target_session_id(), Some("sess-1"));
    assert!(read.action.target_session_name().is_none());
}

#[test]
fn upsert_refuses_send_agent_with_both_targets() {
    let db = test_db();
    let mut agent = base_agent("bad");
    agent.action = AgentAction::SendToSession {
        target_session_name: Some("a".to_string()),
        target_session_id: Some("b".to_string()),
    };
    assert!(db.upsert_agent(&agent).is_err());
    assert!(db.get_agent("bad").unwrap().is_none());
}

#[test]
fn row_with_unknown_action_is_listed_corrupt() {
    let db = test_db();
    db.upsert_agent(&base_agent("weird")).unwrap();
    {
        let conn = db.conn.lock().unwrap();
        conn.execute("UPDATE agents SET action = 'bogus' WHERE id = 'weird'", [])
            .unwrap();
    }
    let corrupt = db.list_corrupt_agents().unwrap();
    assert!(
        corrupt.iter().any(|c| c.id == "weird"),
        "corrupt: {corrupt:?}"
    );
    assert!(corrupt
        .iter()
        .find(|c| c.id == "weird")
        .unwrap()
        .error
        .contains("bogus"));
}

#[test]
fn schema_v11_migration_is_idempotent() {
    assert_eq!(super::SCHEMA_VERSION, 11);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let first = Database::new(&path).unwrap();
    drop(first);
    // Opening again re-runs the guards; it must not fail on existing columns.
    let second = Database::new(&path).unwrap();
    assert!(second.list_agents().unwrap().is_empty());
}
