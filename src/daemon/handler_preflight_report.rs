//! Preflight success-shape builders: the static report both shapes share
//! plus the no-target and full responses. Split out so the sections
//! module stays within the size gate. Behaviour unchanged.

use crate::daemon::probe::ProbeReport;
use rmcp::model::{CallToolResult, ContentBlock};

/// Everything the static (quota-free) half of preflight computed, shared
/// by both success shapes.
pub(crate) struct PreflightStaticReport {
    pub spec_warnings: Vec<serde_json::Value>,
    pub graph_warnings: Vec<serde_json::Value>,
    pub stale_name_warnings: Vec<serde_json::Value>,
    pub missing_crews: Vec<String>,
    pub crew_bindings: Vec<serde_json::Value>,
    pub dirty_workdir_warning: Option<serde_json::Value>,
    pub review: Option<serde_json::Value>,
    pub terminals: Vec<String>,
    pub commit_rights_holders: Vec<serde_json::Value>,
}

/// Everything the probing half of preflight computed.
pub(crate) struct ProbeOutcomeReport {
    pub reports: Vec<ProbeReport>,
    pub roster_would_fail: usize,
    pub probes: Vec<serde_json::Value>,
    pub skipped_unavailable: Vec<serde_json::Value>,
    pub effort_warnings: Vec<serde_json::Value>,
    pub interactive_only_warnings: Vec<serde_json::Value>,
}

pub(crate) fn no_target_response(graph_id: &str, report: &PreflightStaticReport) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(
        serde_json::to_string_pretty(&serde_json::json!({
            "graph_id": graph_id,
            "message": format!(
                "Graph '{graph_id}' has no agent nodes, ensemble members, or on_completed hook to probe."
            ),
            "spec_warnings": report.spec_warnings,
            "graph_warnings": report.graph_warnings,
            "stale_name_warnings": report.stale_name_warnings,
            "interactive_only_warnings": Vec::<serde_json::Value>::new(),
            "skipped_unavailable": Vec::<serde_json::Value>::new(),
            "missing_crews": report.missing_crews,
            "crew_bindings": report.crew_bindings,
            "would_fail": report.missing_crews.len(),
            "dirty_workdir_warning": report.dirty_workdir_warning,
            "review": report.review,
            "terminals": report.terminals,
            "commit_rights_holders": report.commit_rights_holders,
        }))
        .unwrap_or_default(),
    )])
}

pub(crate) fn full_preflight_response(
    graph_id: &str,
    timeout_secs: u64,
    outcome: &ProbeOutcomeReport,
    report: &PreflightStaticReport,
) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(
        serde_json::to_string_pretty(&serde_json::json!({
            "graph_id": graph_id,
            "timeout_seconds": timeout_secs,
            "pairs_checked": outcome.reports.len(),
            "would_fail": crate::daemon::probe::would_fail_count(&outcome.reports)
                + outcome.roster_would_fail
                + report.missing_crews.len(),
            "unknown": crate::daemon::probe::unknown_count(&outcome.reports),
            "probes": outcome.probes,
            "skipped_unavailable": outcome.skipped_unavailable,
            "missing_crews": report.missing_crews,
            "crew_bindings": report.crew_bindings,
            "effort_warnings": outcome.effort_warnings,
            "interactive_only_warnings": outcome.interactive_only_warnings,
            "spec_warnings": report.spec_warnings,
            "graph_warnings": report.graph_warnings,
            "stale_name_warnings": report.stale_name_warnings,
            "dirty_workdir_warning": report.dirty_workdir_warning,
            "review": report.review,
            "terminals": report.terminals,
            "commit_rights_holders": report.commit_rights_holders,
        }))
        .unwrap_or_default(),
    )])
}
