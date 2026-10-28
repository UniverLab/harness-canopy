//! Resolve which node run a `graph_complete_node`/`graph_report_blocker`
//! report belongs to (B12), moved out of `handler_run_exec` so the
//! env-identity fallback and prefix-correction live in one place.
//!
//! Identity comes from the engine, never the model: an agent process carries
//! its run's ids in `CANOPY_NODE_RUN_ID`/`CANOPY_NODE_ID` (exported at spawn,
//! forwarded as headers by the bridge), so the report tools accept omitted
//! ids and resolve them from the caller's [`GraphIdentity`]. A right-prefix /
//! wrong-tail id — the transcription error that lost 2.5 hours of review on
//! 2026-10-27 — is corrected to the environment's run instead of rejected.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::db::Database;
use crate::domain::graphs::{GraphNodeRun, GraphRunStatus, GraphStatus};
use crate::shared::sync_identity::{GraphIdentity, CANOPY_NODE_RUN_ID_ENV};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

/// A resolved report: the run it lands on, plus the id correction applied
/// to get there (`None` when the supplied/resolved ids already named it).
#[derive(Debug)]
pub(crate) struct ResolvedReport {
    pub(crate) run: GraphNodeRun,
    pub(crate) correction: Option<String>,
}

impl ResolvedReport {
    /// The sentence appended to a success message when a correction was
    /// applied (`""` otherwise), naming both the given and the used ids.
    pub(crate) fn correction_suffix(&self) -> String {
        match &self.correction {
            Some(correction) => format!(" Corrected {correction}."),
            None => String::new(),
        }
    }
}

/// Resolve the exact node run a `graph_complete_node`/`graph_report_blocker`
/// report belongs to (B12). A node can be retried, so more than one run can
/// exist for the same `node_id` over a spec's lifetime — matching on
/// `node_id` alone means a report arriving late from a killed/superseded
/// attempt (e.g. a timed-out or paused agent that ignores its own
/// termination and calls the tool anyway) would get silently applied to
/// whatever newer run is now active for that node. Requiring the exact
/// `run_id` and verifying it's both still `running` and actually for the
/// claimed `node_id` closes that gap: anything else is rejected outright
/// rather than guessed at.
///
/// Supplied ids are kept; an omitted id is taken from `identity` (the
/// caller's headers/env triple). Omitted with no identity is a malformed
/// call; a supplied id that names no run but shares its first 8 characters
/// with the identity's is corrected to the identity's run; any other
/// unknown id fails naming the run id the environment carries, so the agent
/// can retry with it. An explicit id that names a different, existing,
/// running run is honored as before — never silently redirected.
///
/// Returns `Err(McpError)` only for a genuine DB failure; a stale/malformed
/// report is a normal `Ok(Err(CallToolResult))` — the tool call succeeded at
/// the protocol level, it's just telling the caller its report didn't stick.
pub(crate) fn resolve_reported_run(
    db: &Database,
    run_id: Option<&str>,
    node_id: Option<&str>,
    identity: Option<&GraphIdentity>,
) -> Result<Result<ResolvedReport, CallToolResult>, McpError> {
    let supplied_run = run_id;
    let supplied_node = node_id;
    let Some(effective_run) = supplied_run.or(identity.map(|id| id.run_id.as_str())) else {
        return Ok(Err(missing_identity_result()));
    };
    let Some(effective_node) = supplied_node.or(identity.map(|id| id.node_id.as_str())) else {
        return Ok(Err(missing_identity_result()));
    };

    match lookup_verified_run(db, effective_run, effective_node)? {
        RunLookup::Found(run) => Ok(Ok(ResolvedReport {
            run: *run,
            correction: None,
        })),
        RunLookup::Rejected(result) => Ok(Err(result)),
        RunLookup::UnknownRun { resolved_run_id } => {
            unknown_run_report(db, supplied_run, supplied_node, &resolved_run_id, identity)
        }
    }
}

/// One prefix-resolve + fetch + verify pass over concrete ids: either the
/// run the report lands on, an unknown run row (correctable/annotatable by
/// the caller), or a rejection that keeps today's errors verbatim.
enum RunLookup {
    Found(Box<GraphNodeRun>),
    UnknownRun { resolved_run_id: String },
    Rejected(CallToolResult),
}

fn lookup_verified_run(db: &Database, run_id: &str, node_id: &str) -> Result<RunLookup, McpError> {
    let resolved_run_id = match db
        .resolve_run_id_by_prefix(run_id)
        .map_err(|e| e.to_string())
    {
        Ok(Some(id)) => id,
        Ok(None) => run_id.to_string(),
        Err(e) => return Ok(RunLookup::Rejected(error_result(&e))),
    };
    let resolved_node_id = match db
        .resolve_graph_node_id_by_prefix(node_id)
        .map_err(|e| e.to_string())
    {
        Ok(Some(id)) => id,
        Ok(None) => node_id.to_string(),
        Err(e) => return Ok(RunLookup::Rejected(error_result(&e))),
    };
    let run = db.get_graph_run(&resolved_run_id).map_err(internal_error)?;
    let Some(run) = run else {
        return Ok(RunLookup::UnknownRun { resolved_run_id });
    };
    if run.node_id != resolved_node_id {
        return Ok(RunLookup::Rejected(error_result(&format!(
            "Run '{resolved_run_id}' belongs to node '{}', not '{resolved_node_id}'.",
            run.node_id
        ))));
    }
    if run.status != GraphRunStatus::Running {
        return Ok(RunLookup::Rejected(error_result(&format!(
            "Run '{resolved_run_id}' for node '{resolved_node_id}' is no longer active (status: {}); this report \
             is stale — the run was already finalized (timed out, paused, reset, superseded by \
             a retry, or already reported) — and was rejected.",
            run.status.as_str()
        ))));
    }
    Ok(RunLookup::Found(Box::new(run)))
}

/// The report's ids name no run row. With nothing supplied, both ids already
/// came from the environment, so this is today's error verbatim. With at
/// least one id supplied and every supplied id sharing its first 8 bytes
/// with the identity's, the report is applied to the environment's run (the
/// identity's ids are verified the same way — a finalized env run still
/// rejects as stale). Any other mismatch keeps failing, naming the run id
/// the environment carries so the agent can retry with it.
fn unknown_run_report(
    db: &Database,
    supplied_run: Option<&str>,
    supplied_node: Option<&str>,
    resolved_run_id: &str,
    identity: Option<&GraphIdentity>,
) -> Result<Result<ResolvedReport, CallToolResult>, McpError> {
    let unknown = || format!("No graph run found with id '{resolved_run_id}'.");
    let Some(identity) = identity else {
        return Ok(Err(error_result(&unknown())));
    };
    if supplied_run.is_none() && supplied_node.is_none() {
        return Ok(Err(error_result(&unknown())));
    }
    if shares_prefix8_opt(supplied_run, &identity.run_id)
        && shares_prefix8_opt(supplied_node, &identity.node_id)
    {
        let correction = correction_text(
            supplied_run,
            &identity.run_id,
            supplied_node,
            &identity.node_id,
        );
        return match lookup_verified_run(db, &identity.run_id, &identity.node_id)? {
            RunLookup::Found(run) => Ok(Ok(ResolvedReport {
                run: *run,
                correction,
            })),
            RunLookup::Rejected(result) => Ok(Err(result)),
            RunLookup::UnknownRun { resolved_run_id } => Ok(Err(error_result(&format!(
                "No graph run found with id '{resolved_run_id}'."
            )))),
        };
    }
    Ok(Err(error_result(&format!(
        "{} The run id this process carries in {CANOPY_NODE_RUN_ID_ENV} is '{}'.",
        unknown(),
        identity.run_id
    ))))
}

/// An omitted id with no identity to supply it from: the call carries no
/// run to report against at all.
fn missing_identity_result() -> CallToolResult {
    error_result(&format!(
        "an id was omitted but this process carries no graph identity \
         ({CANOPY_NODE_RUN_ID_ENV} is unset); pass run_id and node_id explicitly."
    ))
}

/// Whether an optionally supplied id matches the identity's id on its first
/// 8 bytes. An omitted id was already taken from the identity, so it
/// trivially matches; an input shorter than 8 bytes never does — it cannot
/// carry an 8-character prefix.
fn shares_prefix8_opt(supplied: Option<&str>, identity_id: &str) -> bool {
    supplied.is_none_or(|value| {
        value.len() >= 8
            && identity_id.len() >= 8
            && value.as_bytes()[..8] == identity_id.as_bytes()[..8]
    })
}

/// The applied correction, one `'<kind> '<given>' -> '<used>'` pair per
/// supplied id that actually differed from the identity's: `run_id 'x' ->
/// 'y', node_id 'a' -> 'b'`. An id that already equalled the environment's
/// was not corrected, so it must not be reported as if it had been — and a
/// correction naming nothing (both ids already right, unreachable in
/// practice) stays `None`.
fn correction_text(
    supplied_run: Option<&str>,
    identity_run: &str,
    supplied_node: Option<&str>,
    identity_node: &str,
) -> Option<String> {
    let mut pairs = Vec::new();
    if let Some(supplied) = supplied_run.filter(|supplied| *supplied != identity_run) {
        pairs.push(format!("run_id '{supplied}' -> '{identity_run}'"));
    }
    if let Some(supplied) = supplied_node.filter(|supplied| *supplied != identity_node) {
        pairs.push(format!("node_id '{supplied}' -> '{identity_node}'"));
    }
    if pairs.is_empty() {
        None
    } else {
        Some(pairs.join(", "))
    }
}

impl TaskTriggerHandler {
    /// Shared body of `graph_complete_node` and the deprecated
    /// `loop_complete_node` alias (CB68) — identical params, identical
    /// behaviour; only the alias's response gets a `deprecated` note.
    pub(crate) async fn complete_node_impl(
        &self,
        params: GraphCompleteNodeParams,
        identity: Option<GraphIdentity>,
    ) -> Result<CallToolResult, McpError> {
        let status = match params.status.trim() {
            "pass" => Some(GraphRunStatus::Pass),
            "fail" => Some(GraphRunStatus::Fail),
            _ => None,
        };
        let Some(status) = status else {
            return Ok(error_result(
                "graph_complete_node status must be pass or fail.",
            ));
        };
        let report = match resolve_reported_run(
            &self.db,
            params.run_id.as_deref(),
            params.node_id.as_deref(),
            identity.as_ref(),
        )? {
            Ok(report) => report,
            Err(result) => return Ok(result),
        };

        self.db
            .update_graph_run_result(
                &report.run.id,
                status,
                Some(&serde_json::json!({
                    "reported_output": params.output,
                    "summary": params.summary,
                })),
                Some(chrono::Utc::now()),
            )
            .map_err(internal_error)?;

        Ok(success_result(&format!(
            "Graph node result recorded.{}",
            report.correction_suffix()
        )))
    }

    pub(crate) async fn report_blocker_impl(
        &self,
        params: GraphReportBlockerParams,
        identity: Option<GraphIdentity>,
    ) -> Result<CallToolResult, McpError> {
        let report = match resolve_reported_run(
            &self.db,
            params.run_id.as_deref(),
            params.node_id.as_deref(),
            identity.as_ref(),
        )? {
            Ok(report) => report,
            Err(result) => return Ok(result),
        };
        let run = &report.run;

        self.db
            .update_graph_run_result(
                &run.id,
                GraphRunStatus::Fail,
                Some(&serde_json::json!({
                    "blocker": params.description,
                })),
                Some(chrono::Utc::now()),
            )
            .map_err(internal_error)?;
        self.db
            .update_graph_status(&run.graph_id, GraphStatus::Paused, None, None)
            .map_err(internal_error)?;
        self.graph_engine
            .notify_blocked(&run.graph_id, &params.description)
            .map_err(internal_error)?;
        // Resolve the human-readable node name for {{node}} before firing,
        // falling back to the raw node id when the node row is gone.
        let node_name = self
            .db
            .get_graph_node(&run.node_id)
            .ok()
            .flatten()
            .map(|node| node.name)
            .unwrap_or_else(|| run.node_id.clone());
        self.graph_engine
            .fire_on_blocked_hooks(&run.graph_id, &params.description, Some(&node_name))
            .await;

        Ok(success_result(&format!(
            "Graph blocker recorded and graph paused.{}",
            report.correction_suffix()
        )))
    }
}
