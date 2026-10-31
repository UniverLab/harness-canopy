//! Coalescing support for `send_to_session` agents (CM53).
//!
//! An agent whose trigger fires repeatedly while no TUI is open must not pile
//! up one waiting message per fire. The executor looks for an existing
//! undelivered agent-trigger send for the same agent and target; if one is
//! there it replaces that row's prompt and fire time instead of inserting a
//! second.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::params;

use crate::db::scheduled_prompts::{ScheduledPrompt, ScheduledPromptProvenance};
use crate::db::Database;

impl Database {
    /// The first still-pending `agent_trigger` send this agent already has
    /// queued for `target_session_id`, if any. Filtered in Rust (no SQL LIKE)
    /// so it matches the same provenance shape the executor writes.
    pub fn find_pending_agent_trigger_send(
        &self,
        agent_id: &str,
        target_session_id: &str,
    ) -> Result<Option<ScheduledPrompt>> {
        let pending = self.list_pending_scheduled_prompts_for_session(target_session_id)?;
        Ok(pending.into_iter().find(|send| {
            send.provenance.as_ref().is_some_and(|p| {
                p.kind == "agent_trigger" && p.agent_id.as_deref() == Some(agent_id)
            })
        }))
    }

    /// Replace a pending send's prompt and fire time in place, leaving its id
    /// and target. Returns false when the row vanished (a TUI delivered and
    /// deleted it between the find and this update) so the caller can fall
    /// back to a fresh insert.
    pub fn replace_scheduled_prompt(
        &self,
        id: &str,
        prompt: &str,
        fire_at: DateTime<Utc>,
        provenance: &ScheduledPromptProvenance,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let provenance_json = ScheduledPromptProvenance::to_json(Some(provenance))?;
        let rows = conn.execute(
            "UPDATE scheduled_prompts
             SET prompt = ?2, fire_at = ?3, provenance = ?4, builder_state = NULL
             WHERE id = ?1",
            params![id, prompt, fire_at.timestamp(), provenance_json],
        )?;
        Ok(rows > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::scheduled_prompts::ScheduledPromptProvenance;

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn insert_send(db: &Database, id: &str, agent_id: &str, prompt: &str) {
        let provenance = ScheduledPromptProvenance::agent_trigger(agent_id, "cron");
        db.insert_scheduled_prompt(
            id,
            prompt,
            "sess-1",
            Some("/tmp"),
            Utc::now(),
            None,
            Some(&provenance),
        )
        .unwrap();
    }

    #[test]
    fn find_returns_only_same_agent_pending_send() {
        let db = test_db();
        insert_send(&db, "send-a", "agent-a", "hello from a");
        insert_send(&db, "send-b", "agent-b", "hello from b");
        let found = db
            .find_pending_agent_trigger_send("agent-a", "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(found.id, "send-a");
        assert_eq!(found.prompt, "hello from a");
    }

    #[test]
    fn find_skips_non_agent_trigger_provenance() {
        let db = test_db();
        db.insert_scheduled_prompt(
            "send-hook",
            "hook message",
            "sess-1",
            Some("/tmp"),
            Utc::now(),
            None,
            Some(&ScheduledPromptProvenance::hook("graph-1", "on_passed")),
        )
        .unwrap();
        assert!(db
            .find_pending_agent_trigger_send("agent-a", "sess-1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn replace_updates_prompt_and_fire_at_in_place() {
        let db = test_db();
        insert_send(&db, "send-a", "agent-a", "first");
        let later = Utc::now() + chrono::Duration::minutes(5);
        let provenance = ScheduledPromptProvenance::agent_trigger("agent-a", "watch");
        assert!(db
            .replace_scheduled_prompt("send-a", "second", later, &provenance)
            .unwrap());

        let pending = db
            .list_pending_scheduled_prompts_for_session("sess-1")
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "send-a");
        assert_eq!(pending[0].prompt, "second");
        assert_eq!(pending[0].fire_at.timestamp(), later.timestamp());
        assert_eq!(
            pending[0].provenance.as_ref().unwrap().trigger.as_deref(),
            Some("watch")
        );
    }

    #[test]
    fn replace_returns_false_when_row_is_gone() {
        let db = test_db();
        let provenance = ScheduledPromptProvenance::agent_trigger("agent-a", "cron");
        assert!(!db
            .replace_scheduled_prompt("missing", "x", Utc::now(), &provenance)
            .unwrap());
    }
}
