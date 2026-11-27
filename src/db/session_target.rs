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

/// Resolved live interactive session: stable UUID id and mutable display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSessionTarget {
    pub id: String,
    pub name: String,
}

/// Structured target-resolution failure, so callers can emit deterministic text
/// and preserve machine-readable details (e.g. matching ids).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveTargetError {
    Ambiguous {
        name: String,
        matches: Vec<(String, String)>,
    },
    Missing {
        name: String,
        live_names: Vec<String>,
    },
    Conflict {
        id: String,
        name: String,
        by_id: Box<ResolvedSessionTarget>,
        by_name: Box<ResolvedSessionTarget>,
    },
    Message(String),
}

impl ResolveTargetError {
    pub fn message(&self) -> String {
        match self {
            ResolveTargetError::Ambiguous { name, matches } => {
                let entries = matches
                    .iter()
                    .map(|(id, session_name)| format!("{session_name} ({id})"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("Session name '{name}' is ambiguous; matches: {entries}.")
            }
            ResolveTargetError::Missing { name, live_names } => {
                let available = if live_names.is_empty() {
                    "none".to_string()
                } else {
                    live_names.join(", ")
                };
                format!(
                    "Session name '{name}' does not exist or is no longer live. Live interactive session names: {available}."
                )
            }
            ResolveTargetError::Conflict {
                id,
                name,
                by_id,
                by_name,
            } => format!(
                "Target session id '{id}' and name '{name}' resolve to different sessions ('{} ({})' vs '{} ({})').",
                by_id.name, by_id.id, by_name.name, by_name.id
            ),
            ResolveTargetError::Message(msg) => msg.clone(),
        }
    }

    pub fn matching_ids(&self) -> Option<Vec<String>> {
        match self {
            ResolveTargetError::Ambiguous { matches, .. } => {
                Some(matches.iter().map(|(id, _)| id.clone()).collect())
            }
            _ => None,
        }
    }
}

impl std::fmt::Display for ResolveTargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl Database {
    /// Resolve a configured session *name* to exactly one live hookable
    /// session. Ambiguity is reported, never collapsed to one match.
    pub fn resolve_target_session_name(&self, name: &str) -> Result<SessionTarget> {
        let candidates = self.list_hookable_sessions(i64::MAX)?;
        let mut matching: Vec<(String, String)> = candidates
            .into_iter()
            .filter(|session| session.name == name)
            .map(|session| (session.id, session.name))
            .collect();
        matching.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(match matching.len() {
            0 => SessionTarget::Missing,
            1 => {
                let (id, name) = matching.into_iter().next().unwrap_or_default();
                SessionTarget::Found {
                    id,
                    name: Some(name),
                }
            }
            _ => SessionTarget::Ambiguous(matching.into_iter().map(|(id, _)| id).collect()),
        })
    }

    /// Resolve a configured session *id*, using the same hookable rule as
    /// `scheduled_prompts` (a bridge sidecar is never a target).
    pub fn resolve_target_session_id(&self, id: &str) -> Result<SessionTarget> {
        Ok(match self.get_hookable_session(id)? {
            Some(session) => SessionTarget::Found {
                id: session.id,
                name: Some(session.name),
            },
            None => SessionTarget::Missing,
        })
    }

    /// Resolve a target by optional id and/or name with deterministic errors.
    ///
    /// - `id` only: hookable-session lookup by id, then fallback to name lookup.
    /// - `name` only: name lookup.
    /// - both: resolve both and refuse if they resolve to different session ids.
    pub fn resolve_target_session(
        &self,
        id: Option<&str>,
        name: Option<&str>,
    ) -> Result<ResolvedSessionTarget, ResolveTargetError> {
        let id = id.map(str::trim).filter(|s| !s.is_empty());
        let name = name.map(str::trim).filter(|s| !s.is_empty());
        match (id, name) {
            (Some(id), Some(name)) => {
                let by_id = self.resolve_target_session_single(Some(id), None)?;
                let by_name = self.resolve_target_session_single(None, Some(name))?;
                if by_id.id != by_name.id {
                    return Err(ResolveTargetError::Conflict {
                        id: id.to_string(),
                        name: name.to_string(),
                        by_id: Box::new(by_id),
                        by_name: Box::new(by_name),
                    });
                }
                Ok(by_id)
            }
            (Some(id), None) => self.resolve_target_session_single(Some(id), None),
            (None, Some(name)) => self.resolve_target_session_single(None, Some(name)),
            (None, None) => Err(ResolveTargetError::Message(
                "Provide either `target_session_id` or `target_session_name`.".into(),
            )),
        }
    }

    fn resolve_target_session_single(
        &self,
        id: Option<&str>,
        name: Option<&str>,
    ) -> Result<ResolvedSessionTarget, ResolveTargetError> {
        if let Some(name) = name {
            return self.resolve_live_session_by_name(name);
        }
        let id = id.ok_or_else(|| {
            ResolveTargetError::Message(
                "Provide either `target_session_id` or `target_session_name`.".into(),
            )
        })?;
        if let Some(session) = self.get_hookable_session(id).map_err(|e| db_err(&e))? {
            return Ok(ResolvedSessionTarget {
                id: session.id,
                name: session.name,
            });
        }
        self.resolve_live_session_by_name(id)
    }

    fn resolve_live_session_by_name(
        &self,
        name: &str,
    ) -> Result<ResolvedSessionTarget, ResolveTargetError> {
        let candidates = self
            .list_hookable_sessions(i64::MAX)
            .map_err(|e| db_err(&e))?;
        let mut matching: Vec<(String, String)> = candidates
            .iter()
            .filter(|session| session.name == name)
            .map(|session| (session.id.clone(), session.name.clone()))
            .collect();
        matching.sort_by(|a, b| a.0.cmp(&b.0));
        match matching.len() {
            0 => {
                let mut live_names: Vec<String> =
                    candidates.iter().map(|s| s.name.clone()).collect();
                live_names.sort();
                live_names.dedup();
                Err(ResolveTargetError::Missing {
                    name: name.to_string(),
                    live_names,
                })
            }
            1 => {
                let (id, session_name) = matching.remove(0);
                Ok(ResolvedSessionTarget {
                    id,
                    name: session_name,
                })
            }
            _ => Err(ResolveTargetError::Ambiguous {
                name: name.to_string(),
                matches: matching,
            }),
        }
    }
}

fn db_err(e: &anyhow::Error) -> ResolveTargetError {
    ResolveTargetError::Message(e.to_string())
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
                name: Some("orch".to_string()),
            }
        );
    }

    #[test]
    fn resolve_target_session_by_name() {
        let db = test_db();
        add_session(&db, "s1", "orch", "interactive");
        let resolved = db.resolve_target_session(None, Some("orch")).unwrap();
        assert_eq!(resolved.id, "s1");
        assert_eq!(resolved.name, "orch");
    }

    #[test]
    fn resolve_target_session_id_falls_back_to_name() {
        let db = test_db();
        add_session(&db, "s1", "orch", "interactive");
        let resolved = db.resolve_target_session(Some("orch"), None).unwrap();
        assert_eq!(resolved.id, "s1");
    }

    #[test]
    fn resolve_target_session_ambiguous_lists_sorted_matches() {
        let db = test_db();
        add_session(&db, "s2", "orch", "interactive");
        add_session(&db, "s1", "orch", "interactive");
        let err = db.resolve_target_session(None, Some("orch")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"), "{msg}");
        assert!(msg.contains("orch (s1), orch (s2)"), "{msg}");
    }

    #[test]
    fn resolve_target_session_unknown_lists_sorted_live_names() {
        let db = test_db();
        add_session(&db, "s1", "zeta", "interactive");
        add_session(&db, "s2", "alpha", "interactive");
        let err = db
            .resolve_target_session(None, Some("missing"))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("does not exist or is no longer live"), "{msg}");
        assert!(
            msg.contains("Live interactive session names: alpha, zeta"),
            "{msg}"
        );
    }

    #[test]
    fn resolve_target_session_unknown_with_no_live_sessions() {
        let db = test_db();
        let err = db
            .resolve_target_session(None, Some("missing"))
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Live interactive session names: none"),
            "{msg}"
        );
    }

    #[test]
    fn resolve_target_session_rejects_conflicting_id_and_name() {
        let db = test_db();
        add_session(&db, "s1", "alpha", "interactive");
        add_session(&db, "s2", "beta", "interactive");
        let err = db
            .resolve_target_session(Some("s1"), Some("beta"))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("resolve to different sessions"), "{msg}");
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
                preserve_args: true,
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
