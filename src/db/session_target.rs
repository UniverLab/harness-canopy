//! Shared resolution of an interactive-hook / agent-delivery target session.
//!
//! Both the interactive graph hook (CH3, `src/graph_engine/hooks_interactive.rs`)
//! and the `send_to_session` agent executor resolve a configured target the same
//! way; this module is that one implementation. A name must match exactly one
//! live hookable session — never the first of several, never a redirect to
//! another session.

use anyhow::Result;

use crate::db::Database;

/// Outcome of resolving a configured target session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTarget {
    /// Exactly one live session matched.
    Found { id: String, name: Option<String> },
    /// No live session matched.
    Missing,
    /// More than one live session shares the name; ids of every match.
    Ambiguous(Vec<String>),
}

impl Database {
    /// Resolve a configured session *name* to exactly one live hookable
    /// session. Ambiguity is reported, never collapsed to one match.
    pub fn resolve_target_session_name(&self, name: &str) -> Result<SessionTarget> {
        let candidates = self.list_hookable_sessions(i64::MAX)?;
        let matching: Vec<String> = candidates
            .into_iter()
            .filter(|session| session.name == name)
            .map(|session| session.id)
            .collect();
        Ok(match matching.len() {
            0 => SessionTarget::Missing,
            1 => SessionTarget::Found {
                id: matching.into_iter().next().unwrap_or_default(),
                name: Some(name.to_string()),
            },
            _ => SessionTarget::Ambiguous(matching),
        })
    }

    /// Resolve a configured session *id*, using the same hookable rule as
    /// `scheduled_send_create` (a bridge sidecar is never a target).
    pub fn resolve_target_session_id(&self, id: &str) -> Result<SessionTarget> {
        Ok(match self.get_hookable_session(id)? {
            Some(session) => SessionTarget::Found {
                id: session.id,
                name: None,
            },
            None => SessionTarget::Missing,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn add_session(db: &Database, id: &str, name: &str, session_type: &str) {
        db.insert_interactive_session(id, name, "opencode", "/tmp", None, None, session_type, None)
            .unwrap();
    }

    #[test]
    fn name_matches_exactly_one_live_session() {
        let db = test_db();
        add_session(&db, "s1", "orch", "interactive");
        assert_eq!(
            db.resolve_target_session_name("orch").unwrap(),
            SessionTarget::Found {
                id: "s1".to_string(),
                name: Some("orch".to_string()),
            }
        );
    }

    #[test]
    fn unknown_name_is_missing() {
        let db = test_db();
        add_session(&db, "s1", "orch", "interactive");
        assert_eq!(
            db.resolve_target_session_name("nobody").unwrap(),
            SessionTarget::Missing
        );
    }

    #[test]
    fn two_live_sessions_same_name_is_ambiguous() {
        let db = test_db();
        add_session(&db, "s1", "orch", "interactive");
        add_session(&db, "s2", "orch", "interactive");
        let mut ids = match db.resolve_target_session_name("orch").unwrap() {
            SessionTarget::Ambiguous(ids) => ids,
            other => panic!("expected Ambiguous, got {other:?}"),
        };
        ids.sort();
        assert_eq!(ids, vec!["s1".to_string(), "s2".to_string()]);
    }

    #[test]
    fn bridge_or_non_hookable_session_is_not_a_target() {
        let db = test_db();
        add_session(&db, "sidecar", "orch", "bridge");
        assert_eq!(
            db.resolve_target_session_name("orch").unwrap(),
            SessionTarget::Missing
        );
        assert_eq!(
            db.resolve_target_session_id("sidecar").unwrap(),
            SessionTarget::Missing
        );
    }

    #[test]
    fn id_resolves_to_found() {
        let db = test_db();
        add_session(&db, "s1", "orch", "interactive");
        assert_eq!(
            db.resolve_target_session_id("s1").unwrap(),
            SessionTarget::Found {
                id: "s1".to_string(),
                name: None,
            }
        );
    }

    /// FR7: a hook addressed to the old name resolves to the resumed session.
    /// The finished row is not a target, but the Resume step relaunches the
    /// source id — with no recorded harness id (FR1) — and the registration
    /// reactivates that same row in place (FR2), so the same name resolves
    /// again to the same id.
    #[test]
    fn hook_targeting_the_old_name_resolves_after_the_resume() {
        use crate::db::session::{RegistrationOutcome, SessionRegistration};

        let db = test_db();
        add_session(&db, "s1", "marasmius", "interactive");
        db.finish_interactive_session("s1", 0).unwrap();
        assert_eq!(
            db.resolve_target_session_name("marasmius").unwrap(),
            SessionTarget::Missing,
            "the finished session is not a hook target yet"
        );

        let outcome = db
            .register_interactive_session(&SessionRegistration {
                id: "s1",
                name: "marasmius",
                cli: "opencode",
                working_dir: "/tmp",
                args: None,
                pid: Some(99),
                session_type: "interactive",
                boot_id: Some("boot-new"),
                harness_conversation_id: None,
            })
            .unwrap();
        assert_eq!(
            outcome,
            RegistrationOutcome::Reactivated {
                id: "s1".to_string(),
                name: "marasmius".to_string(),
            },
            "the resume must reactivate the source row, not add one"
        );

        assert_eq!(
            db.resolve_target_session_name("marasmius").unwrap(),
            SessionTarget::Found {
                id: "s1".to_string(),
                name: Some("marasmius".to_string()),
            }
        );
    }
}
