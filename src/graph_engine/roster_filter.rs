//! CM48: engine dispatch-time roster filtering.
//!
//! Availability is resolved at the moment of dispatch (never at run start), so
//! a pair declared unavailable mid-run is skipped from its next dispatch on.
//! The decision is a pure function of the roster rows the engine reads here;
//! the ensemble/node/hook dispatch sites only carry out its verdict. Nothing
//! here ever writes the roster.

use super::*;
use crate::db::roster_entries::RosterEntry;
use crate::domain::graphs::Ensemble;
use crate::domain::roster::{effective_state, Availability};
use chrono::{DateTime, Utc};

/// One member or node whose pair is effectively unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnavailablePair {
    pub platform: String,
    pub model: Option<String>,
    pub reason: Option<String>,
    pub until: Option<String>,
}

impl UnavailablePair {
    fn label(&self) -> String {
        format!(
            "{}/{}",
            self.platform,
            self.model.as_deref().unwrap_or("(default)")
        )
    }

    /// `<platform>/<model> — <reason>[ until <until>]`, the fragment the
    /// blocker messages join with `, `.
    pub(crate) fn describe(&self) -> String {
        let mut text = format!(
            "{} — {}",
            self.label(),
            self.reason.as_deref().unwrap_or("no reason")
        );
        if let Some(until) = &self.until {
            text.push_str(&format!(" until {until}"));
        }
        text
    }
}

/// What the engine should do with one ensemble dispatch after consulting the
/// roster.
pub(crate) enum EnsembleRoster {
    /// Dispatch these members (the available subset; all of them when the
    /// roster is empty). Boxed so this variant stays close in size to the
    /// blocker message.
    Ready(Box<EnsembleDetails>),
    /// Do not dispatch; block the spec with this message.
    Blocked(String),
}

impl GraphEngine {
    /// Query the roster for one platform. An empty list on a query failure is
    /// the safe default: everything is available.
    pub(crate) fn roster_for_platform(&self, platform: &str) -> Vec<RosterEntry> {
        self.db
            .roster_list_for_platform(platform)
            .unwrap_or_default()
    }

    /// Effective availability of one platform/model pair at `now`.
    pub(crate) fn availability_for(
        &self,
        platform: &str,
        model: Option<&str>,
        now: &DateTime<Utc>,
    ) -> Availability {
        let entries = self.roster_for_platform(platform);
        effective_state(&entries, platform, model, now)
    }

    /// Partition an ensemble's members into those available now and those not,
    /// querying each distinct platform at most once.
    pub(crate) fn partition_ensemble_members(
        &self,
        details: &EnsembleDetails,
    ) -> (Vec<EnsembleMember>, Vec<UnavailablePair>) {
        let now = Utc::now();
        let mut cache: HashMap<String, Vec<RosterEntry>> = HashMap::new();
        let mut available = Vec::new();
        let mut unavailable = Vec::new();
        for member in &details.members {
            let entries = cache
                .entry(member.platform.clone())
                .or_insert_with(|| self.roster_for_platform(&member.platform));
            let avail = effective_state(entries, &member.platform, member.model.as_deref(), &now);
            if avail.available {
                available.push(member.clone());
            } else {
                unavailable.push(UnavailablePair {
                    platform: member.platform.clone(),
                    model: member.model.clone(),
                    reason: avail.reason,
                    until: avail.until,
                });
            }
        }
        (available, unavailable)
    }

    /// Resolve one ensemble dispatch against the roster: ready with the
    /// available members, or blocked with the required message.
    pub(crate) fn resolve_ensemble_roster(&self, details: &EnsembleDetails) -> EnsembleRoster {
        let (available, unavailable) = self.partition_ensemble_members(details);
        match ensemble_roster_decision(
            &details.ensemble,
            details.members.len(),
            available,
            &unavailable,
        ) {
            EnsembleRosterDecision::Ready(members) => {
                EnsembleRoster::Ready(Box::new(EnsembleDetails {
                    ensemble: details.ensemble.clone(),
                    members,
                }))
            }
            EnsembleRosterDecision::Blocked(blocker) => EnsembleRoster::Blocked(blocker),
        }
    }

    /// Blocker for a single agent node whose pair is unavailable, or `None`
    /// when it may run. Non-agent nodes carry no platform and are never
    /// blocked by the roster.
    pub(crate) fn node_availability_blocker(&self, node: &GraphNode) -> Option<String> {
        if node.kind != GraphNodeKind::Agent {
            return None;
        }
        let platform = node
            .config
            .get("platform")
            .or_else(|| node.config.get("cli"))
            .and_then(Value::as_str)?;
        let model = node.config.get("model").and_then(Value::as_str);
        let avail = self.availability_for(platform, model, &Utc::now());
        if avail.available {
            return None;
        }
        Some(node_unavailable_blocker(
            &node.name,
            platform,
            model,
            avail.reason.as_deref().unwrap_or("no reason"),
        ))
    }
}

/// Result shape of [`ensemble_roster_decision`], split out so the rule itself
/// is a table-driven pure function.
pub(crate) enum EnsembleRosterDecision {
    Ready(Vec<EnsembleMember>),
    Blocked(String),
}

/// Requirement 5's blocking rules, in priority order: every member
/// unavailable blocks in any kind; only a `parallel` ensemble also blocks when
/// fewer than `min_pass` members are available.
pub(crate) fn ensemble_roster_decision(
    ensemble: &Ensemble,
    total: usize,
    available: Vec<EnsembleMember>,
    unavailable: &[UnavailablePair],
) -> EnsembleRosterDecision {
    if available.is_empty() {
        return EnsembleRosterDecision::Blocked(ensemble_no_member_blocker(
            &ensemble.name,
            unavailable,
        ));
    }
    if ensemble.kind == EnsembleKind::Parallel && (available.len() as i64) < ensemble.min_pass {
        return EnsembleRosterDecision::Blocked(ensemble_min_pass_blocker(
            &ensemble.name,
            available.len(),
            total,
            ensemble.min_pass,
            unavailable,
        ));
    }
    EnsembleRosterDecision::Ready(available)
}

fn unavailable_list(unavailable: &[UnavailablePair]) -> String {
    unavailable
        .iter()
        .map(UnavailablePair::describe)
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn ensemble_no_member_blocker(name: &str, unavailable: &[UnavailablePair]) -> String {
    format!(
        "ensemble '{name}': no available member (unavailable: {})",
        unavailable_list(unavailable)
    )
}

pub(crate) fn ensemble_min_pass_blocker(
    name: &str,
    available: usize,
    total: usize,
    min_pass: i64,
    unavailable: &[UnavailablePair],
) -> String {
    format!(
        "ensemble '{name}': {available} of {total} members available, min_pass {min_pass} (unavailable: {})",
        unavailable_list(unavailable)
    )
}

pub(crate) fn node_unavailable_blocker(
    name: &str,
    platform: &str,
    model: Option<&str>,
    reason: &str,
) -> String {
    format!(
        "node '{name}': {platform}/{} unavailable — {reason}",
        model.unwrap_or("(default)")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graphs::EnsembleKind;

    fn member(node_id: &str, platform: &str, model: Option<&str>) -> EnsembleMember {
        EnsembleMember {
            ensemble_id: "ens".to_string(),
            node_id: node_id.to_string(),
            position: 0,
            platform: platform.to_string(),
            model: model.map(str::to_string),
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }
    }

    fn pair(platform: &str, model: Option<&str>) -> UnavailablePair {
        UnavailablePair {
            platform: platform.to_string(),
            model: model.map(str::to_string),
            reason: Some("quota".to_string()),
            until: Some("2026-07-01T00:00:00Z".to_string()),
        }
    }

    fn ensemble(kind: EnsembleKind, min_pass: i64) -> Ensemble {
        Ensemble {
            id: "ens".to_string(),
            spec_id: Some("spec".to_string()),
            graph_id: None,
            name: "Implementer".to_string(),
            prompt_template: "{{spec_content}}".to_string(),
            join_node_id: "join".to_string(),
            entry_from_node: "entry".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: "entry".to_string(),
            on_fail_to: None,
            kind,
            round_robin_index: Some(0),
            commit_rights: false,
            created_at: chrono::Utc::now(),
        }
    }

    fn engine_with_db() -> (tempfile::TempDir, Arc<Database>, GraphEngine) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::new(&dir.path().join("t.db")).unwrap());
        let engine = GraphEngine::new(
            Arc::clone(&db),
            Arc::new(crate::application::notification_service::DefaultNotificationService),
        );
        (dir, db, engine)
    }

    fn mark_unavailable(db: &Database, platform: &str) {
        db.roster_upsert(crate::db::roster_entries::NewRosterEntry {
            platform,
            model_pattern: None,
            state: crate::db::roster_entries::RosterState::Unavailable,
            reason: Some("quota"),
            until: None,
        })
        .unwrap();
    }

    /// DB-backed: the exact method `run_spec_ensemble_step` calls — an
    /// unavailable member is dropped from the dispatch set.
    #[test]
    fn resolve_ensemble_roster_skips_unavailable_member() {
        let (_dir, db, engine) = engine_with_db();
        mark_unavailable(&db, "opencode-go");
        let details = EnsembleDetails {
            ensemble: ensemble(EnsembleKind::Parallel, 1),
            members: vec![
                member("m1", "claude", None),
                member("m2", "opencode-go", Some("opencode-go/claude")),
            ],
        };
        match engine.resolve_ensemble_roster(&details) {
            EnsembleRoster::Ready(ready) => {
                assert_eq!(ready.members.len(), 1);
                assert_eq!(ready.members[0].node_id, "m1");
            }
            EnsembleRoster::Blocked(message) => panic!("expected a dispatch, got block: {message}"),
        }
    }

    /// DB-backed: all members unavailable blocks before dispatch.
    #[test]
    fn resolve_ensemble_roster_blocks_when_every_member_unavailable() {
        let (_dir, db, engine) = engine_with_db();
        mark_unavailable(&db, "claude");
        let details = EnsembleDetails {
            ensemble: ensemble(EnsembleKind::RoundRobin, 1),
            members: vec![member("m1", "claude", None), member("m2", "claude", None)],
        };
        match engine.resolve_ensemble_roster(&details) {
            EnsembleRoster::Blocked(message) => {
                assert!(message.contains("no available member"), "{message}");
            }
            EnsembleRoster::Ready(_) => panic!("every member unavailable must block"),
        }
    }

    /// DB-backed: the exact method `run_spec_node_step` calls.
    #[test]
    fn node_availability_blocker_blocks_unavailable_agent_node() {
        let (_dir, db, engine) = engine_with_db();
        mark_unavailable(&db, "opencode-go");
        let node = GraphNode {
            id: "n1".to_string(),
            spec_id: None,
            graph_id: None,
            name: "Implementer".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": "opencode-go",
                "model": "opencode-go/claude",
            }),
            position: 0,
            created_at: chrono::Utc::now(),
        };
        assert_eq!(
            engine.node_availability_blocker(&node),
            Some(
                "node 'Implementer': opencode-go/opencode-go/claude unavailable — quota"
                    .to_string()
            )
        );
    }

    #[test]
    fn node_availability_blocker_none_for_available_node() {
        let (_dir, _db, engine) = engine_with_db();
        let node = GraphNode {
            id: "n1".to_string(),
            spec_id: None,
            graph_id: None,
            name: "Implementer".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({ "platform": "claude" }),
            position: 0,
            created_at: chrono::Utc::now(),
        };
        assert_eq!(engine.node_availability_blocker(&node), None);
    }

    #[test]
    fn parallel_below_min_pass_blocks_with_message() {
        let decision = ensemble_roster_decision(
            &ensemble(EnsembleKind::Parallel, 2),
            3,
            vec![member("m1", "claude", None)],
            &[pair("opencode-go", Some("opencode-go/claude"))],
        );
        match decision {
            EnsembleRosterDecision::Blocked(message) => {
                assert_eq!(
                    message,
                    "ensemble 'Implementer': 1 of 3 members available, min_pass 2 (unavailable: opencode-go/opencode-go/claude — quota until 2026-07-01T00:00:00Z)"
                );
            }
            EnsembleRosterDecision::Ready(_) => panic!("must block below min_pass"),
        }
    }

    #[test]
    fn every_member_unavailable_blocks_in_any_kind() {
        for kind in [
            EnsembleKind::Parallel,
            EnsembleKind::Cascade,
            EnsembleKind::RoundRobin,
        ] {
            let decision = ensemble_roster_decision(
                &ensemble(kind, 1),
                2,
                Vec::new(),
                &[pair("opencode-go", None)],
            );
            match decision {
                EnsembleRosterDecision::Blocked(message) => {
                    assert_eq!(
                        message,
                        "ensemble 'Implementer': no available member (unavailable: opencode-go/(default) — quota until 2026-07-01T00:00:00Z)"
                    );
                }
                EnsembleRosterDecision::Ready(_) => panic!("no available member must block"),
            }
        }
    }

    #[test]
    fn round_robin_does_not_block_on_min_pass() {
        // min_pass 2, only 1 available: parallel would block, round-robin must not.
        let decision = ensemble_roster_decision(
            &ensemble(EnsembleKind::RoundRobin, 2),
            2,
            vec![member("m1", "claude", None)],
            &[pair("opencode-go", None)],
        );
        assert!(matches!(decision, EnsembleRosterDecision::Ready(_)));
    }

    #[test]
    fn empty_roster_dispatches_every_member() {
        let decision = ensemble_roster_decision(
            &ensemble(EnsembleKind::Parallel, 2),
            2,
            vec![member("m1", "claude", None), member("m2", "claude", None)],
            &[],
        );
        match decision {
            EnsembleRosterDecision::Ready(members) => assert_eq!(members.len(), 2),
            EnsembleRosterDecision::Blocked(_) => panic!("empty roster must dispatch all"),
        }
    }

    #[test]
    fn node_blocker_names_platform_model_and_reason() {
        assert_eq!(
            node_unavailable_blocker(
                "Implementer",
                "opencode-go",
                Some("opencode-go/claude"),
                "quota"
            ),
            "node 'Implementer': opencode-go/opencode-go/claude unavailable — quota"
        );
    }
}
