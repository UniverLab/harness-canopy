//! CM48: `graph_preflight`'s roster split — which of a graph's probe targets
//! are off-limits right now, and which ensembles the roster would stop.
//!
//! Split out of `handler_preflight.rs` (which grew past the file-size gate)
//! and sharing the engine's own dispatch decision
//! ([`crate::graph_engine::ensemble_would_block`]), so the preflight's verdict
//! and the engine's dispatch come from one availability decision. An
//! unavailable pair is never probed — no quota is spent on a pair the engine
//! will skip — and is reported in `skipped_unavailable` instead; an ensemble
//! that would block at dispatch counts as `would_fail`. Nothing here ever
//! writes the roster: only the MCP/CLI surfaces do.

use crate::daemon::probe::{GraphProbeTarget, ProbeTarget};
use crate::db::Database;
use crate::domain::roster::effective_state;
use chrono::{DateTime, Utc};

/// Split a graph's probe targets by roster availability at `now`:
///
/// - the pairs to probe — available or unlisted (an empty roster probes
///   everything, exactly as before), each with its `used_by` names;
/// - the pairs declared effectively unavailable, each with the reason, the
///   expiry and the nodes/members that reference it — never probed.
pub(crate) fn split_targets_by_availability(
    db: &Database,
    graph_targets: &[GraphProbeTarget],
    now: &DateTime<Utc>,
) -> (Vec<(ProbeTarget, Vec<String>)>, Vec<serde_json::Value>) {
    let mut probe_pairs: Vec<(ProbeTarget, Vec<String>)> = Vec::new();
    let mut skipped_unavailable: Vec<serde_json::Value> = Vec::new();
    for gt in graph_targets {
        let entries = db
            .roster_list_for_platform(&gt.target.platform)
            .unwrap_or_default();
        let avail = effective_state(
            &entries,
            &gt.target.platform,
            gt.target.model.as_deref(),
            now,
        );
        if avail.available {
            probe_pairs.push((gt.target.clone(), gt.used_by.clone()));
        } else {
            skipped_unavailable.push(serde_json::json!({
                "platform": gt.target.platform,
                "model": gt.target.model,
                "reason": avail.reason,
                "until": avail.until,
                "used_by": gt.used_by,
            }));
        }
    }
    (probe_pairs, skipped_unavailable)
}

/// How many of a graph's ensembles would block at dispatch because of the
/// roster right now. Covers the graph-level ensembles AND every bound spec's:
/// both dispatch through the same roster filter, so a spec-level ensemble the
/// roster stops must count toward `would_fail` exactly like a graph-level
/// one. Read-only; never probes and never writes the roster.
pub(crate) fn roster_blocked_ensemble_count(
    db: &Database,
    details: &crate::domain::graphs::GraphDetails,
    now: &DateTime<Utc>,
) -> usize {
    let graph_level = db
        .list_ensembles_for_graph(&details.lp.id)
        .unwrap_or_default();
    let spec_level = details
        .specs
        .iter()
        .flat_map(|spec| {
            db.list_ensembles_for_spec(&spec.spec.id)
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    graph_level
        .into_iter()
        .chain(spec_level)
        .filter(|ensemble| crate::graph_engine::ensemble_would_block(db, ensemble, now))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::roster_entries::{NewRosterEntry, RosterState};

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn target(platform: &str, model: Option<&str>) -> GraphProbeTarget {
        GraphProbeTarget {
            target: ProbeTarget {
                platform: platform.to_string(),
                model: model.map(str::to_string),
                effort: None,
            },
            used_by: vec![format!("{platform} node")],
        }
    }

    fn mark_unavailable(db: &Database, platform: &str, until: Option<&str>) {
        db.roster_upsert(NewRosterEntry {
            platform,
            model_pattern: None,
            state: RosterState::Unavailable,
            reason: Some("quota"),
            until,
        })
        .unwrap();
    }

    /// CM48: an unavailable pair is withheld from probing and reported in
    /// `skipped_unavailable` with its reason, expiry and referencing nodes —
    /// no quota is spent on a pair the engine will skip.
    #[test]
    fn split_withholds_unavailable_pair_and_reports_it() {
        let db = test_db();
        mark_unavailable(&db, "opencode-go", Some("2099-01-01T00:00:00Z"));
        let targets = vec![
            target("opencode-go", Some("opencode-go/claude")),
            target("claude", Some("opus")),
        ];
        let (probe_pairs, skipped) = split_targets_by_availability(&db, &targets, &now());
        assert_eq!(probe_pairs.len(), 1, "only the available pair is probed");
        assert_eq!(probe_pairs[0].0.platform, "claude");
        assert_eq!(probe_pairs[0].1, vec!["claude node".to_string()]);
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0]["platform"], "opencode-go");
        assert_eq!(skipped[0]["model"], "opencode-go/claude");
        assert_eq!(skipped[0]["reason"], "quota");
        assert_eq!(skipped[0]["until"], "2099-01-01T00:00:00Z");
        assert_eq!(skipped[0]["used_by"][0], "opencode-go node");
    }

    /// An expired `until` reads as available, so the pair is probed again.
    #[test]
    fn split_probes_a_pair_whose_unavailability_expired() {
        let db = test_db();
        mark_unavailable(&db, "opencode-go", Some("2026-05-01T00:00:00Z"));
        let targets = vec![target("opencode-go", Some("opencode-go/claude"))];
        let (probe_pairs, skipped) = split_targets_by_availability(&db, &targets, &now());
        assert!(skipped.is_empty(), "an expired entry must not skip probing");
        assert_eq!(probe_pairs.len(), 1);
    }

    /// An empty roster probes everything, exactly as before CM48.
    #[test]
    fn split_probes_everything_when_the_roster_is_empty() {
        let db = test_db();
        let targets = vec![target("claude", Some("opus")), target("opencode-go", None)];
        let (probe_pairs, skipped) = split_targets_by_availability(&db, &targets, &now());
        assert!(skipped.is_empty());
        assert_eq!(probe_pairs.len(), 2);
    }

    /// A model-specific override keeps the pair runnable even when the
    /// platform as a whole is out.
    #[test]
    fn split_respects_the_most_specific_entry() {
        let db = test_db();
        mark_unavailable(&db, "opencode-go", None);
        db.roster_upsert(NewRosterEntry {
            platform: "opencode-go",
            model_pattern: Some("opencode-go/claude"),
            state: RosterState::Available,
            reason: None,
            until: None,
        })
        .unwrap();
        let targets = vec![target("opencode-go", Some("opencode-go/claude"))];
        let (probe_pairs, skipped) = split_targets_by_availability(&db, &targets, &now());
        assert!(
            skipped.is_empty(),
            "the exact override makes the pair available"
        );
        assert_eq!(probe_pairs.len(), 1);
    }

    /// An ensemble the roster would stop counts once toward `would_fail`.
    #[test]
    fn blocked_ensemble_count_counts_only_unrunnable_ensembles() {
        use crate::domain::graphs::{Ensemble, EnsembleKind, GraphEdgeCondition};

        let db = test_db();
        db.insert_graph(&crate::domain::graphs::Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "g1".to_string(),
            name: "Graph".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: crate::domain::graphs::GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        })
        .unwrap();
        let now_ts = chrono::Utc::now();
        // A graph-level ensemble: spec_id NULL, graph_id set (the graphs
        // table requires exactly one of the two).
        let kickoff_node = crate::domain::graphs::GraphNode {
            id: "kickoff".to_string(),
            spec_id: None,
            graph_id: Some("g1".to_string()),
            name: "kickoff".to_string(),
            kind: crate::domain::graphs::GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: now_ts,
        };
        let join_node = crate::domain::graphs::GraphNode {
            id: "join1".to_string(),
            spec_id: None,
            graph_id: Some("g1".to_string()),
            name: "join".to_string(),
            kind: crate::domain::graphs::GraphNodeKind::Join,
            config: serde_json::json!({ "ensemble_id": "ens-dead" }),
            position: 50,
            created_at: now_ts,
        };
        let member_node = crate::domain::graphs::GraphNode {
            id: "m1".to_string(),
            spec_id: None,
            graph_id: Some("g1".to_string()),
            name: "member".to_string(),
            kind: crate::domain::graphs::GraphNodeKind::Agent,
            config: serde_json::json!({ "platform": "opencode-go" }),
            position: 2,
            created_at: now_ts,
        };
        let ensemble = Ensemble {
            mutex: Vec::new(),
            commit_rights: false,
            id: "ens-dead".to_string(),
            spec_id: None,
            graph_id: Some("g1".to_string()),
            name: "Dead Ensemble".to_string(),
            prompt_template: "tmpl".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 1,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: "join1".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: now_ts,

            crew_id: None,
            crew_name: None,
            crew_version_applied: None,
        };
        let member = crate::domain::graphs::EnsembleMember {
            ensemble_id: "ens-dead".to_string(),
            node_id: "m1".to_string(),
            position: 0,
            platform: "opencode-go".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        db.insert_ensemble_unit(
            &ensemble,
            &[member],
            &[kickoff_node, member_node],
            &join_node,
            &[],
        )
        .unwrap();

        // A spec-level ensemble bound to the same graph: it dispatches
        // through the same roster filter as the graph-level one, so a
        // roster that would stop it must count toward `would_fail` too.
        db.insert_graph_spec(&crate::domain::graphs::GraphSpec {
            id: "spec-1".to_string(),
            graph_id: Some("g1".to_string()),
            name: "Spec One".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: crate::domain::graphs::GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        })
        .unwrap();
        let spec_ensemble = Ensemble {
            mutex: Vec::new(),
            commit_rights: false,
            id: "ens-spec".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Spec Ensemble".to_string(),
            prompt_template: "tmpl".to_string(),
            join_node_id: "spec-join".to_string(),
            entry_from_node: "spec-kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 1,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: "spec-join".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: now_ts,

            crew_id: None,
            crew_name: None,
            crew_version_applied: None,
        };
        let spec_member = crate::domain::graphs::EnsembleMember {
            ensemble_id: "ens-spec".to_string(),
            node_id: "spec-m1".to_string(),
            position: 0,
            platform: "spec-out".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        let spec_kickoff = crate::domain::graphs::GraphNode {
            id: "spec-kickoff".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "spec kickoff".to_string(),
            kind: crate::domain::graphs::GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: now_ts,
        };
        let spec_member_node = crate::domain::graphs::GraphNode {
            id: "spec-m1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "spec member".to_string(),
            kind: crate::domain::graphs::GraphNodeKind::Agent,
            config: serde_json::json!({ "platform": "spec-out" }),
            position: 2,
            created_at: now_ts,
        };
        let spec_join_node = crate::domain::graphs::GraphNode {
            id: "spec-join".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "spec join".to_string(),
            kind: crate::domain::graphs::GraphNodeKind::Join,
            config: serde_json::json!({ "ensemble_id": "ens-spec" }),
            position: 50,
            created_at: now_ts,
        };
        db.insert_ensemble_unit(
            &spec_ensemble,
            &[spec_member],
            &[spec_kickoff, spec_member_node],
            &spec_join_node,
            &[],
        )
        .unwrap();

        let details = db.get_graph_details("g1").unwrap().unwrap();
        assert_eq!(
            roster_blocked_ensemble_count(&db, &details, &now()),
            0,
            "an empty roster blocks nothing"
        );
        mark_unavailable(&db, "opencode-go", None);
        assert_eq!(
            roster_blocked_ensemble_count(&db, &details, &now()),
            1,
            "the graph-level ensemble whose only member is unavailable counts as would_fail"
        );
        mark_unavailable(&db, "spec-out", None);
        assert_eq!(
            roster_blocked_ensemble_count(&db, &details, &now()),
            2,
            "a spec-level ensemble the roster would block counts too"
        );
    }
}
