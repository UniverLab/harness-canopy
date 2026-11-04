//! MCP handler tools split out of `daemon::handler` (preflight audit tool and reviewer helpers).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_models_probes::clamp_probe_timeout;
use crate::daemon::handler_preflight_report::{
    full_preflight_response, no_target_response, PreflightStaticReport, ProbeOutcomeReport,
};
use crate::daemon::handler_preflight_sections::{
    commit_rights_holders, dirty_workdir_value, effort_warnings_for, fetch_graph_details_or_error,
    interactive_only_warnings_for, min_pass_error, orphan_join_error, output_reference_error,
    probe_value, spawn_requested_reviewer, spec_content_warnings, spec_quorum_warnings,
    split_probe_targets, stale_name_warnings, structural_terminals_and_warnings,
    unsafe_template_error,
};
use crate::daemon::handler_run_exec::validate_graph_ensembles_for_run;
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::or_early_return;
use rmcp::model::CallToolResult;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_preflight, vis = "pub(crate)")]
impl TaskTriggerHandler {
    pub(crate) async fn graph_preflight_impl(
        &self,
        params: GraphPreflightParams,
    ) -> Result<CallToolResult, McpError> {
        let details = or_early_return!(fetch_graph_details_or_error(&self.db, &params.graph_id));
        let dirty_workdir_warning = dirty_workdir_value(&details.lp.workdir).await;
        let (terminal_list, mut graph_warnings) =
            or_early_return!(structural_terminals_and_warnings(&self.db, &details));
        graph_warnings.extend(or_early_return!(spec_quorum_warnings(
            &self.db,
            &details.specs
        )));
        let stale_name_warnings = or_early_return!(stale_name_warnings(&self.db, &details));
        // Templates stored before write-time template validation that carry a
        // placeholder nothing can bind are reported as warnings (FR3) — the
        // graph was writable then, so this stays advisory, never a refusal.
        match crate::daemon::handler_templates::unbindable_template_warnings(&self.db, &details) {
            Ok(warnings) => graph_warnings.extend(warnings),
            Err(e) => return Ok(error_result(&e)),
        }

        if let Some(message) = output_reference_error(&details.graph_nodes) {
            return Ok(error_result(&message));
        }

        // CM38 FR4: an unknown `{{...}}` marker in a check command is a
        // static authoring defect — the engine fails the node before running
        // it (see `render_check_command`). Report it before any reviewer
        // spawn or probe quota is spent, across the graph's own nodes and
        // every spec's nodes.
        if let Some(message) = crate::graph_engine::check_command_marker_error(&details) {
            return Ok(error_result(&format!("Preflight: {message}")));
        }

        // Ensemble-owned wiring must be complete before any platform probes
        // spend quota. This is the same structural check graph_control performs.
        if let Err(e) = validate_graph_ensembles_for_run(&self.db, &details.lp.id, None) {
            return Ok(error_result(&format!("Preflight: {e}")));
        }

        if let Some(message) =
            or_early_return!(min_pass_error(&self.db, &details.lp.id, &details.specs))
        {
            return Ok(error_result(&message));
        }
        if let Some(message) =
            or_early_return!(orphan_join_error(&self.db, &details.lp.id, &details.specs))
        {
            return Ok(error_result(&message));
        }
        let commit_rights_holders = or_early_return!(commit_rights_holders(&self.db, &details));
        // Computed before the no-target early return so a graph with no
        // agent nodes still reports its blank specs.
        let spec_warnings = spec_content_warnings(&details.specs);

        // CM49: crew-bound nodes/ensembles/hooks contribute their crew's
        // member pairs as probe targets; a crew missing locally is reported
        // under `missing_crews` and counts into `would_fail`.
        let (graph_targets, missing_crews, crew_bindings) =
            crate::daemon::probe::graph_targets_with_crews(&self.db, &details);

        if let Some(message) = or_early_return!(unsafe_template_error(&graph_targets)) {
            return Ok(error_result(&message));
        }
        let reviewer_out = spawn_requested_reviewer(self, &details, &params.reviewer).await;
        let static_report = PreflightStaticReport {
            spec_warnings,
            graph_warnings,
            stale_name_warnings,
            missing_crews,
            crew_bindings,
            dirty_workdir_warning,
            review: reviewer_out,
            terminals: terminal_list,
            commit_rights_holders,
        };
        if graph_targets.is_empty() && static_report.missing_crews.is_empty() {
            return Ok(no_target_response(&params.graph_id, &static_report));
        }

        let Some(home) = dirs::home_dir() else {
            return Err(internal_error("No home directory"));
        };
        let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));

        let timeout_secs = clamp_probe_timeout(params.timeout_seconds);
        let (probe_pairs, skipped_unavailable, roster_would_fail) =
            split_probe_targets(&self.db, &details, &graph_targets);
        let targets: Vec<crate::daemon::probe::ProbeTarget> = probe_pairs
            .iter()
            .map(|(target, _)| target.clone())
            .collect();
        let reports = crate::daemon::probe::probe_targets(
            &config,
            &targets,
            Some(&details.lp.workdir),
            std::time::Duration::from_secs(timeout_secs),
        )
        .await;
        let probes: Vec<serde_json::Value> = reports
            .iter()
            .zip(probe_pairs.iter())
            .map(|(report, (target, used_by))| probe_value(report, target, used_by, &config))
            .collect();
        let effort_warnings = effort_warnings_for(&graph_targets, &config);
        let interactive_only_warnings = interactive_only_warnings_for(&graph_targets, &config);
        let outcome = ProbeOutcomeReport {
            reports,
            roster_would_fail,
            probes,
            skipped_unavailable,
            effort_warnings,
            interactive_only_warnings,
        };
        Ok(full_preflight_response(
            &params.graph_id,
            timeout_secs,
            &outcome,
            &static_report,
        ))
    }
}
