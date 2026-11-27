//! Live session rename: validate, resolve, persist, update hub, emit event.

use crate::db::session_target::ResolvedSessionTarget;
use crate::db::Database;
use crate::domain::validation_session::validate_session_name;
use crate::sessions::{HubEvent, SessionHub};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRenameResult {
    pub id: String,
    pub old_name: String,
    pub new_name: String,
}

/// Rename a live interactive session by id or name.
///
/// Resolves `target` with the same rules as scheduled_prompts/session targeting,
/// validates the new name, refuses duplicates held by another live session,
/// persists to the DB, updates the live hub session, and emits SessionRenamed.
pub fn rename_live_session(
    db: &Database,
    hub: Option<&SessionHub>,
    target: &str,
    new_name: &str,
) -> Result<SessionRenameResult, String> {
    let new_name = new_name.trim();
    validate_session_name(new_name)?;
    let resolved = resolve_target(db, target)?;
    if resolved.name == new_name {
        return Ok(SessionRenameResult {
            id: resolved.id,
            old_name: resolved.name,
            new_name: new_name.to_string(),
        });
    }
    ensure_name_available(db, &resolved.id, new_name)?;
    db.rename_interactive_session(&resolved.id, new_name)
        .map_err(|e| e.to_string())?;
    if let Some(hub) = hub {
        if let Some(session) = hub.get(&resolved.id) {
            session.set_name(new_name.to_string());
            hub.emit(HubEvent::SessionRenamed {
                id: resolved.id.clone(),
                name: new_name.to_string(),
            });
        }
    }
    Ok(SessionRenameResult {
        id: resolved.id,
        old_name: resolved.name,
        new_name: new_name.to_string(),
    })
}

fn resolve_target(db: &Database, target: &str) -> Result<ResolvedSessionTarget, String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("Target session id or name must not be empty.".to_string());
    }
    db.resolve_target_session(Some(target), None)
        .map_err(|e| e.to_string())
}

fn ensure_name_available(db: &Database, target_id: &str, new_name: &str) -> Result<(), String> {
    let sessions = db
        .list_hookable_sessions(i64::MAX)
        .map_err(|e| e.to_string())?;
    if sessions
        .iter()
        .any(|s| s.id != target_id && s.name == new_name)
    {
        return Err(format!(
            "Session name '{}' is already held by another live session.",
            new_name
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::scheduled_prompts::ScheduledPromptProvenance;

    fn test_db(dir: &tempfile::TempDir) -> Database {
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn add_session(db: &Database, id: &str, name: &str) {
        db.insert_interactive_session(
            id,
            name,
            "opencode",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
    }

    #[test]
    fn renames_by_name_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        add_session(&db, "s1", "alpha");
        let result = rename_live_session(&db, None, "alpha", "beta").unwrap();
        assert_eq!(result.old_name, "alpha");
        assert_eq!(result.new_name, "beta");
        let session = db.get_hookable_session("s1").unwrap().unwrap();
        assert_eq!(session.name, "beta");
    }

    #[test]
    fn rejects_duplicate_name() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        add_session(&db, "s1", "alpha");
        add_session(&db, "s2", "beta");
        let err = rename_live_session(&db, None, "s1", "beta").unwrap_err();
        assert!(
            err.contains("already held by another live session"),
            "{err}"
        );
    }

    #[test]
    fn rename_persists_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = test_db(&dir);
            add_session(&db, "s1", "alpha");
            rename_live_session(&db, None, "s1", "beta").unwrap();
        }
        let db = test_db(&dir);
        let session = db.get_hookable_session("s1").unwrap().unwrap();
        assert_eq!(session.name, "beta");
    }

    #[test]
    fn rename_while_scheduled_send_pending_still_targets_same_id() {
        let dir = tempfile::tempdir().unwrap();
        let db = test_db(&dir);
        add_session(&db, "s1", "alpha");
        let fire_at = chrono::Utc::now() + chrono::Duration::seconds(60);
        db.insert_scheduled_prompt(
            "ss-1",
            "hello",
            "s1",
            Some("/tmp"),
            fire_at,
            None,
            Some(&ScheduledPromptProvenance::agent("s1")),
        )
        .unwrap();
        rename_live_session(&db, None, "s1", "beta").unwrap();
        let pending = db.list_pending_scheduled_prompts_for_session("s1").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].target_session_id, "s1");
        let session = db.get_hookable_session("s1").unwrap().unwrap();
        assert_eq!(session.name, "beta");
    }
}
