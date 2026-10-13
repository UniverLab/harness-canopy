//! MCP handler tools split out of `daemon::handler` (preflight audit tool and reviewer helpers).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_models_probes::{clamp_probe_timeout, display_for_slug};
use crate::daemon::handler_read::orphan_join_warning_text;
use crate::daemon::handler_run_exec::validate_graph_ensembles_for_run;
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use rmcp::model::{CallToolResult, Content};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

#[tool_router(router = tool_router_preflight, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_preflight",
        description = "Probe every distinct platform+model pair a graph's agent nodes, ensemble members, and on_completed hook reference — before spending a real graph_run on a harness that's installed and configured but can't actually produce a response. A platform used by several nodes is probed once, not once per node; the result names every node/hook that references a failing pair. Verdict is based on response content, not exit code (see agent_probe): a pair the harness silently answers with a substituted model, or one whose CLI has no way to select a model explicitly, is never reported reachable — `would_fail` counts confirmed failures and `unknown` counts pairs that couldn't be validated, kept separate so an unvalidated pair is never mistaken for a passing one. A pair whose harness answered with its own argument-parser usage/error text (canopy's invocation was malformed, not the platform) is reported misconfigured rather than broken, with the same node/ensemble naming and the exact argv canopy ran, redacted of secrets. Spends real tokens/quota per distinct pair — call this explicitly before graph_run, never automatically. When 'reviewer' names a platform+model pair, additionally spawns a background design-review agent (see 'review' in the output); collect it with subagent_collect. The review never blocks or fails the probe results."
    )]
    pub(crate) async fn graph_preflight(
        &self,
        Parameters(params): Parameters<GraphPreflightParams>,
    ) -> Result<CallToolResult, McpError> {
        let details = match self.db.get_graph_details(&params.graph_id) {
            Ok(Some(details)) => details,
            Ok(None) => {
                return Ok(error_result(&format!(
                    "Graph '{}' not found.",
                    params.graph_id
                )))
            }
            Err(e) => return Err(internal_error(e.to_string())),
        };

        let dirty_workdir_warning: Option<serde_json::Value> = {
            let (dirty, _paths) =
                crate::graph_engine::capture_workdir_dirty_with_paths(&details.lp.workdir).await;
            dirty.filter(|n| *n > 0).map(|count| {
                let plural = if count == 1 { "y" } else { "ies" };
                serde_json::json!({
                    "count": count,
                    "warning": format!(
                        "Workdir '{}' has {} uncommitted git entr{}.",
                        details.lp.workdir, count, plural
                    ),
                })
            })
        };

        // Graph-level structural validation (CB8) — same validate_graph
        // used by graph_import, before spending quota on platform probes.
        let (terminal_list, graph_warnings): (Vec<String>, Vec<serde_json::Value>) = {
            let router_labels: Vec<Vec<String>> = details
                .graph_nodes
                .iter()
                .map(|n| {
                    if n.kind == crate::domain::graphs::GraphNodeKind::Router {
                        n.config
                            .get("routes")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|entry| {
                                        entry
                                            .get("label")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    }
                })
                .collect();
            let node_views: Vec<crate::domain::validation::GraphNodeView> = details
                .graph_nodes
                .iter()
                .enumerate()
                .map(|(idx, n)| crate::domain::validation::GraphNodeView {
                    id: &n.id,
                    kind: n.kind,
                    route_labels: &router_labels[idx],
                })
                .collect();
            let edge_views: Vec<crate::domain::validation::GraphEdgeView> = details
                .graph_edges
                .iter()
                .map(|e| crate::domain::validation::GraphEdgeView {
                    from: &e.from_node,
                    to: &e.to_node,
                    condition: &e.condition,
                })
                .collect();
            let validation_report =
                match crate::domain::validation::validate_graph(&node_views, &edge_views) {
                    Ok(report) => report,
                    Err(e) => {
                        // The validator names nodes by id; a live graph's ids are
                        // opaque UUIDs, so enrich with the display name where we can
                        // (same treatment graph_import gives its message).
                        let mut enriched = e;
                        for n in &details.graph_nodes {
                            if enriched.contains(n.id.as_str()) {
                                enriched = enriched
                                    .replace(n.id.as_str(), &format!("{} ({})", n.name, n.id));
                            }
                        }
                        return Ok(error_result(&enriched));
                    }
                };
            // Terminal exits are informational: a state with no outgoing edge
            // ends the run there (the engine treats a missing edge as an
            // ending). Name nodes the way the operator sees them — by name,
            // with the id in parentheses, matching the error text's shape.
            let terminal_list: Vec<String> = validation_report
                .terminals
                .iter()
                .map(|t| {
                    let display = details
                        .graph_nodes
                        .iter()
                        .find(|n| n.id == t.node_id)
                        .map(|n| format!("{} ({})", n.name, n.id))
                        .unwrap_or_else(|| t.node_id.clone());
                    format!("{} ends on '{}'", display, t.state)
                })
                .collect();
            // CM19: non-terminal agent/check/gate nodes whose `fail` status
            // has nowhere to go. A warning, never a refusal — the graph may be
            // exactly what its author meant. Deliberate terminals are already
            // excluded upstream (they land in `terminals`, not here).
            let graph_warnings: Vec<serde_json::Value> = validation_report
                .fail_dead_ends
                .iter()
                .map(|d| {
                    let node = details.graph_nodes.iter().find(|n| n.id == d.node_id);
                    let name = node.map(|n| n.name.clone()).unwrap_or_default();
                    let kind = node.map(|n| n.kind.as_str()).unwrap_or("node");
                    let display = node
                        .map(|n| format!("{} ({})", n.name, n.id))
                        .unwrap_or_else(|| d.node_id.clone());
                    let infra_clause = if d.has_error_path {
                        " Its 'error' edge still routes infrastructure failures, but a genuine 'fail' verdict is not."
                    } else {
                        " It has no 'error' edge either, so an infrastructure failure would dead-end here as well."
                    };
                    serde_json::json!({
                        "node_id": d.node_id,
                        "node_name": name,
                        "kind": kind,
                        "missing_edge": "fail",
                        "has_error_path": d.has_error_path,
                        "warning": format!(
                            "Node {display} [{kind}] has no outgoing 'fail' edge. If it returns a fail verdict the spec terminates at this node with \"no outgoing edge for that status\" and the run stays blocked until a human intervenes.{infra_clause} Add a 'fail' or 'always' edge with graph_add_edge if this failure should route somewhere.",
                        ),
                    })
                })
                .collect();
            (terminal_list, graph_warnings)
        };

        // CB68 FR2: warn (never rewrite — FR4) about every node, ensemble and
        // hook whose prompt/command still names a removed 2.x `loop_<tool>` or
        // `canopy loop`, so a graph built before 3.0.0 doesn't silently produce
        // an agent that calls a tool that no longer exists.
        let stale_name_warnings: Vec<serde_json::Value> = {
            let mut out = Vec::new();
            let all_nodes = details
                .graph_nodes
                .iter()
                .chain(details.specs.iter().flat_map(|s| s.nodes.iter()));
            for node in all_nodes {
                for field in ["prompt_template", "command"] {
                    if let Some(text) = node.config.get(field).and_then(|v| v.as_str()) {
                        for hit in crate::domain::validation::find_stale_2x_names(text) {
                            out.push(serde_json::json!({
                                "kind": "node",
                                "id": node.id,
                                "name": node.name,
                                "old_name": hit.old,
                                "new_name": hit.new,
                                "warning": format!(
                                    "Node '{}' {} mentions '{}', which no longer exists since 3.0.0 — use '{}'.",
                                    node.name, field, hit.old, hit.new
                                ),
                            }));
                        }
                    }
                }
            }
            for (event, hook_list) in &details.lp.hooks {
                for hook in hook_list {
                    for text in [hook.prompt.as_deref(), hook.command.as_deref()]
                        .into_iter()
                        .flatten()
                    {
                        for hit in crate::domain::validation::find_stale_2x_names(text) {
                            out.push(serde_json::json!({
                                "kind": "hook",
                                "event": event.as_str(),
                                "old_name": hit.old,
                                "new_name": hit.new,
                                "warning": format!(
                                    "A {} hook mentions '{}', which no longer exists since 3.0.0 — use '{}'.",
                                    event.as_str(), hit.old, hit.new
                                ),
                            }));
                        }
                    }
                }
            }
            let mut ensembles = self
                .db
                .list_ensembles_for_graph(&details.lp.id)
                .map_err(internal_error)?;
            for spec in &details.specs {
                ensembles.extend(
                    self.db
                        .list_ensembles_for_spec(&spec.spec.id)
                        .map_err(internal_error)?,
                );
            }
            for ens in &ensembles {
                for hit in
                    crate::domain::validation::find_stale_2x_names(&ens.ensemble.prompt_template)
                {
                    out.push(serde_json::json!({
                        "kind": "ensemble",
                        "id": ens.ensemble.id,
                        "name": ens.ensemble.name,
                        "old_name": hit.old,
                        "new_name": hit.new,
                        "warning": format!(
                            "Ensemble '{}' prompt mentions '{}', which no longer exists since 3.0.0 — use '{}'.",
                            ens.ensemble.name, hit.old, hit.new
                        ),
                    }));
                }
                for member in &ens.members {
                    let Some(prompt_override) = &member.prompt_override else {
                        continue;
                    };
                    for hit in crate::domain::validation::find_stale_2x_names(prompt_override) {
                        out.push(serde_json::json!({
                            "kind": "ensemble",
                            "id": ens.ensemble.id,
                            "name": ens.ensemble.name,
                            "old_name": hit.old,
                            "new_name": hit.new,
                            "warning": format!(
                                "Ensemble '{}' member prompt override mentions '{}', which no longer exists since 3.0.0 — use '{}'.",
                                ens.ensemble.name, hit.old, hit.new
                            ),
                        }));
                    }
                }
            }
            out
        };

        // CM1: validate {{output:NodeName}} references
        {
            let node_names: Vec<String> =
                details.graph_nodes.iter().map(|n| n.name.clone()).collect();
            for node in &details.graph_nodes {
                if let Some(prompt) = node.config.get("prompt_template").and_then(|v| v.as_str()) {
                    let mut rest = prompt;
                    while let Some(start) = rest.find("{{output:") {
                        let after_prefix = &rest[start + 9..];
                        if let Some(end) = after_prefix.find("}}") {
                            let referenced_name = &after_prefix[..end];
                            if !node_names.iter().any(|n| n == referenced_name) {
                                return Ok(error_result(&format!(
                                    "Node '{}' references unknown node '{}' in {{{{output:{}}}}}.",
                                    node.name, referenced_name, referenced_name
                                )));
                            }
                            rest = &after_prefix[end + 2..];
                        } else {
                            break;
                        }
                    }
                }
            }
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
        // spend quota. This is the same structural check graph_run performs.
        if let Err(e) = validate_graph_ensembles_for_run(&self.db, &details.lp.id, None) {
            return Ok(error_result(&format!("Preflight: {e}")));
        }

        // ── CB40: report pre-existing ensembles with invalid min_pass (FR5) ──────────
        // These were stored before the write-time validation existed. Report them as
        // errors rather than spending probe quota on a graph that will fail on export.
        {
            let graph_ensembles = self
                .db
                .list_ensembles_for_graph(&details.lp.id)
                .map_err(internal_error)?;
            for ens in &graph_ensembles {
                if let Err(e) = crate::domain::validation::validate_ensemble_min_pass(
                    &ens.ensemble.name,
                    ens.ensemble.min_pass,
                    ens.members.len(),
                ) {
                    return Ok(error_result(&format!(
                        "Preflight: {e} Fix with graph_update_ensemble (ensemble_id '{}', min_pass ≤ {}).",
                        ens.ensemble.id,
                        ens.members.len()
                    )));
                }
            }
            for spec in &details.specs {
                let spec_ensembles = self
                    .db
                    .list_ensembles_for_spec(&spec.spec.id)
                    .map_err(internal_error)?;
                for ens in &spec_ensembles {
                    if let Err(e) = crate::domain::validation::validate_ensemble_min_pass(
                        &ens.ensemble.name,
                        ens.ensemble.min_pass,
                        ens.members.len(),
                    ) {
                        return Ok(error_result(&format!(
                            "Preflight: {e} Fix with graph_update_ensemble (ensemble_id '{}', min_pass ≤ {}).",
                            ens.ensemble.id,
                            ens.members.len()
                        )));
                    }
                }
            }
        }

        // ── CB52 FR4: refuse a graph with orphaned joins before spending ──
        // probe quota on it. A join node with no ensemble row means the next
        // dispatch through that point dies with "ambiguous outgoing edges" —
        // report the cause instead. List it with graph_get's
        // orphan_join_warnings and delete it with graph_delete_node, or
        // repair the graph manually (never auto-repaired, C1).
        {
            let mut orphans = self
                .db
                .list_orphan_join_nodes_for_graph(&details.lp.id)
                .map_err(internal_error)?;
            for spec in &details.specs {
                orphans.extend(
                    self.db
                        .list_orphan_join_nodes_for_spec(&spec.spec.id)
                        .map_err(internal_error)?,
                );
            }
            if !orphans.is_empty() {
                let parts: Vec<String> = orphans.iter().map(orphan_join_warning_text).collect();
                return Ok(error_result(&format!(
                    "Preflight: {}. List it with graph_get's orphan_join_warnings and delete it with graph_delete_node, or repair the graph manually.",
                    parts.join("; ")
                )));
            }
        }

        // CM28 / FR3: name whichever node or ensemble in this graph holds commit
        // rights, across the top-level graph and every spec's own graph. A node
        // owned by an ensemble is never listed on its own — only the ensemble is.
        let commit_rights_holders: Vec<serde_json::Value> = {
            fn holders_for(
                nodes: &[crate::domain::graphs::GraphNode],
                ensembles: &[crate::domain::graphs::EnsembleDetails],
            ) -> Vec<serde_json::Value> {
                let owned_ids: std::collections::HashSet<&str> = ensembles
                    .iter()
                    .flat_map(|d| {
                        d.members
                            .iter()
                            .map(|m| m.node_id.as_str())
                            .chain(std::iter::once(d.ensemble.join_node_id.as_str()))
                    })
                    .collect();
                let mut out = Vec::new();
                for node in nodes {
                    if owned_ids.contains(node.id.as_str()) {
                        continue;
                    }
                    if node.config.get("commit_rights").and_then(|v| v.as_bool()) == Some(true) {
                        out.push(
                            serde_json::json!({ "kind": "node", "id": node.id, "name": node.name }),
                        );
                    }
                }
                for details in ensembles {
                    if details.ensemble.commit_rights {
                        out.push(serde_json::json!({
                            "kind": "ensemble",
                            "id": details.ensemble.id,
                            "name": details.ensemble.name,
                        }));
                    }
                }
                out
            }
            let graph_ensembles_for_report = self
                .db
                .list_ensembles_for_graph(&details.lp.id)
                .map_err(internal_error)?;
            let mut holders = holders_for(&details.graph_nodes, &graph_ensembles_for_report);
            for spec in &details.specs {
                let spec_ensembles = self
                    .db
                    .list_ensembles_for_spec(&spec.spec.id)
                    .map_err(internal_error)?;
                holders.extend(holders_for(&spec.nodes, &spec_ensembles));
            }
            holders
        };

        // (CB22) Bound-spec content warnings: advisory only, never a tool
        // error and never spending probe quota. Computed before the
        // no-target early return so a graph with no agent nodes still reports
        // its blank specs.
        let spec_warnings: Vec<serde_json::Value> = details
            .specs
            .iter()
            .filter(|s| !crate::graph_engine::GraphEngine::spec_has_content(&s.spec))
            .map(|s| {
                let display = if s.spec.name.trim().is_empty() {
                    s.spec.id.clone()
                } else {
                    s.spec.name.clone()
                };
                serde_json::json!({
                    "spec_id": s.spec.id,
                    "warning": format!(
                        "Spec '{}' (id '{}') has no content: both name and description are empty. Give the spec a name or description before running.",
                        display, s.spec.id
                    ),
                })
            })
            .collect();

        let graph_targets = crate::daemon::probe::distinct_targets_for_graph(&details);

        // FR4/CM31: reject before spending any probe quota — this is a static
        // defect in the platform's template, not a runtime reachability
        // question, so there is nothing a probe could tell us that changes the
        // verdict.
        {
            let Some(home) = dirs::home_dir() else {
                return Err(internal_error("No home directory"));
            };
            let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
            let mut seen_platforms = std::collections::HashSet::new();
            for gt in &graph_targets {
                let platform = gt.target.platform.as_str();
                if !seen_platforms.insert(platform) {
                    continue;
                }
                let Some(cli) = config.get_cli(platform) else {
                    continue;
                };
                let Some(template) = cli.invocation_template.as_deref() else {
                    continue;
                };
                if let Some(msg) =
                    crate::domain::cli_strategy::CliStrategy::unsafe_optional_model_token(
                        template, platform,
                    )
                {
                    return Ok(error_result(&format!("Preflight: {msg}")));
                }
            }
        }

        // (CB25) Spawn the design reviewer BEFORE the no-target early return
        // so a review is available on both success shapes. `spawn_subagent`
        // returns an id immediately and never blocks, so the reviewer works
        // while the probes run below. Failures degrade to `not_spawned` and
        // never fail the preflight itself.
        let reviewer_out: Option<serde_json::Value> = match &params.reviewer {
            None => None,
            Some(r) => Some(
                spawn_preflight_reviewer(
                    self,
                    &details.lp.id,
                    details.lp.active_run_queue_id.as_deref(),
                    &details.lp.workdir,
                    &r.platform,
                    r.model.as_deref(),
                )
                .await,
            ),
        };
        if graph_targets.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&serde_json::json!({
                    "graph_id": params.graph_id,
                    "message": format!(
                        "Graph '{}' has no agent nodes, ensemble members, or on_completed hook to probe.",
                        params.graph_id
                    ),
                    "spec_warnings": spec_warnings,
                    "graph_warnings": graph_warnings,
                    "stale_name_warnings": stale_name_warnings,
                    "interactive_only_warnings": Vec::<serde_json::Value>::new(),
                    "skipped_unavailable": Vec::<serde_json::Value>::new(),
                    "dirty_workdir_warning": dirty_workdir_warning.clone(),
                    "review": reviewer_out,
                    "terminals": terminal_list,
                    "commit_rights_holders": commit_rights_holders,
                }))
                .unwrap_or_default(),
            )]));
        }

        let Some(home) = dirs::home_dir() else {
            return Err(internal_error("No home directory"));
        };
        let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));

        let timeout_secs = clamp_probe_timeout(params.timeout_seconds);
        // CM48: split targets by roster availability. An unavailable pair is
        // NOT probed (no quota spent on a pair the engine will skip anyway)
        // and is reported in `skipped_unavailable` instead.
        let now = chrono::Utc::now();
        let mut skipped_unavailable: Vec<serde_json::Value> = Vec::new();
        let mut probe_pairs: Vec<(crate::daemon::probe::ProbeTarget, Vec<String>)> = Vec::new();
        for gt in &graph_targets {
            let entries = self
                .db
                .roster_list_for_platform(&gt.target.platform)
                .unwrap_or_default();
            let avail = crate::domain::roster::effective_state(
                &entries,
                &gt.target.platform,
                gt.target.model.as_deref(),
                &now,
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
        // CM48: an ensemble that would block under the dispatch rules counts
        // as `would_fail` — the roster makes it unrunnable without editing it.
        let roster_would_fail = self.roster_blocked_ensemble_count(&details.lp.id, &now);
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
            .map(|(report, (target, used_by))| {
                let mut value = report.to_json();
                if let serde_json::Value::Object(map) = &mut value {
                    map.insert("used_by".to_string(), serde_json::json!(used_by));
                    map.insert(
                        "platform_display".to_string(),
                        serde_json::json!(display_for_slug(&config, &report.platform)),
                    );
                    if let Some(effort) = target.effort.as_deref() {
                        if let Some(reason) = crate::domain::cli_config::effort_rejection_reason(
                            config
                                .get_cli(&target.platform)
                                .and_then(|c| c.effort_declaration.as_ref()),
                            &target.platform,
                            effort,
                        ) {
                            map.insert("effort_not_applied".to_string(), serde_json::json!(reason));
                        }
                    }
                }
                value
            })
            .collect();

        let effort_warnings: Vec<serde_json::Value> = graph_targets
            .iter()
            .filter_map(|lt| {
                let effort = lt.target.effort.as_deref()?;
                let reason = crate::domain::cli_config::effort_rejection_reason(
                    config
                        .get_cli(&lt.target.platform)
                        .and_then(|c| c.effort_declaration.as_ref()),
                    &lt.target.platform,
                    effort,
                )?;
                Some(serde_json::json!({
                    "used_by": lt.used_by,
                    "platform": lt.target.platform,
                    "effort": effort,
                    "warning": reason,
                }))
            })
            .collect();

        // CM35: flag every existing node/member/hook that points at a platform
        // marked `headless = false` — the probe above already reports it
        // `interactive_only`, and this surfaces which parts of the graph use
        // it. Advisory only: preflight never fails on this.
        let interactive_only_warnings: Vec<serde_json::Value> = graph_targets
            .iter()
            .filter_map(|lt| {
                let cli = config.get_cli(&lt.target.platform)?;
                (!cli.headless).then(|| {
                    serde_json::json!({
                        "used_by": lt.used_by,
                        "platform": lt.target.platform,
                        "warning": crate::domain::cli_config::interactive_only_reason(
                            &lt.target.platform,
                        ),
                    })
                })
            })
            .collect();

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "graph_id": params.graph_id,
                "timeout_seconds": timeout_secs,
                "pairs_checked": reports.len(),
                "would_fail": crate::daemon::probe::would_fail_count(&reports) + roster_would_fail,
                "unknown": crate::daemon::probe::unknown_count(&reports),
                "probes": probes,
                "skipped_unavailable": skipped_unavailable,
                "effort_warnings": effort_warnings,
                "interactive_only_warnings": interactive_only_warnings,
                "spec_warnings": spec_warnings,
                "graph_warnings": graph_warnings,
                "stale_name_warnings": stale_name_warnings,
                "dirty_workdir_warning": dirty_workdir_warning,
                "review": reviewer_out,
                "terminals": terminal_list,
                "commit_rights_holders": commit_rights_holders,
            }))
            .unwrap_or_default(),
        )]))
    }

    /// CM48: how many of a graph's ensembles would block at dispatch because
    /// too few members are available right now. Read-only; never probes.
    pub(crate) fn roster_blocked_ensemble_count(
        &self,
        graph_id: &str,
        now: &chrono::DateTime<chrono::Utc>,
    ) -> usize {
        self.db
            .list_ensembles_for_graph(graph_id)
            .unwrap_or_default()
            .iter()
            .filter(|details| ensemble_would_block(&self.db, details, now))
            .count()
    }
}

/// CM48: an ensemble blocks when it has no available member, or is `parallel`
/// with fewer than `min_pass` members available — the same rule the engine
/// applies at dispatch.
fn ensemble_would_block(
    db: &crate::db::Database,
    details: &crate::domain::graphs::EnsembleDetails,
    now: &chrono::DateTime<chrono::Utc>,
) -> bool {
    let mut available = 0usize;
    for member in &details.members {
        let entries = db
            .roster_list_for_platform(&member.platform)
            .unwrap_or_default();
        let avail = crate::domain::roster::effective_state(
            &entries,
            &member.platform,
            member.model.as_deref(),
            now,
        );
        if avail.available {
            available += 1;
        }
    }
    available == 0
        || (details.ensemble.kind == crate::domain::graphs::EnsembleKind::Parallel
            && (available as i64) < details.ensemble.min_pass)
}

/// (CB25) Build the design-review prompt for a `graph_preflight` reviewer.
/// The full audit checklist lives in the `graph-reviewer` skill — never
/// duplicated here. The fallback checklist below is used ONLY when the
/// skill cannot be resolved, and names the areas, not the checks.
pub(crate) fn build_preflight_reviewer_prompt(
    graph_id: &str,
    queue_id: Option<&str>,
    skill_section: &str,
) -> String {
    let mut prompt = String::from(
        "You are auditing a canopy graph's DESIGN, not its reachability. Audit ONLY: do not modify the graph, the specs, or any file (no graph_* mutation tools, no file writes).",
    );
    prompt.push_str(&format!(" Target: graph_id='{graph_id}'"));
    if let Some(qid) = queue_id {
        prompt.push_str(&format!(" queue_id='{qid}'."));
    } else {
        prompt.push('.');
    }
    prompt.push_str(
        " Read the graph with graph_get (and queue_list for the queue) through the MCP surface.",
    );
    prompt.push_str(
        " Load the 'graph-reviewer' skill with skill_get; if the skill text is appended below under '## Skill: graph-reviewer', that IS its content — do not fetch again.",
    );
    prompt.push_str(skill_section);
    prompt.push_str(
        " Fallback areas (only if the skill is missing): graph (single entry, no unreachable node, pass+fail on every agent node, paired ops balanced); specs (decided, one repo each, seven sections, one language, no unsourced factual claims); model economy (expensive model placement, cost of one rejection, resume, commit rights, context groups, pinned skills); prompts (finish the turn, continue from work on disk).",
    );
    prompt.push_str(
        " Order findings by severity, name the SINGLE item to fix first, and close with an advisory verdict. Your verdict does not block the run — the caller decides.",
    );
    prompt
}

/// (CB25) Resolve the `graph-reviewer` skill for the preflight reviewer
/// prompt. Success inlines the instructions under the pinned-skill
/// delimiter; anything else degrades to the unresolved note and the
/// reviewer still spawns. Blocking (git/network), so the caller runs it
/// via `spawn_blocking`.
pub(crate) fn resolve_reviewer_skill_section(store: &crate::dynamic_skills::SkillStore) -> String {
    match store.get("graph-reviewer") {
        Ok(content) => format!("\n\n## Skill: graph-reviewer\n{}", content.instructions),
        Err(_) => "\n\n## Skill: graph-reviewer\n_Could not resolve pinned skill 'graph-reviewer' — continuing without it._"
            .to_string(),
    }
}

/// (CB25) Spawn the `graph_preflight` design reviewer. NEVER returns Err:
/// every failure degrades to a `not_spawned` note so the reviewer can
/// never fail the preflight itself.
pub(crate) async fn spawn_preflight_reviewer(
    handler: &TaskTriggerHandler,
    graph_id: &str,
    queue_id: Option<&str>,
    workdir: &str,
    platform: &str,
    model: Option<&str>,
) -> serde_json::Value {
    let pair = format!("{platform}/{}", model.unwrap_or("default"));
    // `Cli::strategy()` panics for a platform missing from the canopy
    // config (and `find_platform` errors for names outside the registry),
    // so pre-validate here: an unconfigured reviewer platform degrades to
    // `not_spawned` instead of panicking the preflight itself. Mirrors
    // `Cli::strategy`'s home resolution (`CANOPY_HOME_OVERRIDE` first).
    let home = std::env::var_os("CANOPY_HOME_OVERRIDE")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir);
    let configured =
        home.map(|h| crate::domain::canopy_config::CanopyConfig::load(&h.join(".canopy")));
    if configured
        .as_ref()
        .and_then(|c| c.get_cli(platform))
        .is_none()
    {
        return serde_json::json!({
            "status": "not_spawned",
            "platform": platform,
            "model": model,
            "pair": pair,
            "note": format!("Reviewer platform '{platform}' is not configured in canopy; probes above are unaffected."),
        });
    }
    let store = Arc::clone(&handler.dynamic_skills);
    let skill_section =
        match tokio::task::spawn_blocking(move || resolve_reviewer_skill_section(&store)).await {
            Ok(section) => section,
            Err(_) => resolve_reviewer_skill_section(&handler.dynamic_skills),
        };
    let prompt = build_preflight_reviewer_prompt(graph_id, queue_id, &skill_section);
    // Run the spawn in its own task so a panic inside `spawn_subagent`
    // (e.g. `fetch_registry`'s `reqwest::blocking` client, which cannot
    // drop its runtime in an async context under `debug_assertions`)
    // is captured as a `JoinError` and degrades to `not_spawned`
    // instead of taking down the preflight. Owned clones keep the
    // future `'static`; the monitor task it leaves behind still joins
    // the shared runtime, so a successful spawn is unaffected.
    let db = Arc::clone(&handler.db);
    let platform_owned = platform.to_string();
    let model_owned = model.map(str::to_string);
    let workdir_owned = workdir.to_string();
    let spawned = tokio::spawn(async move {
        crate::daemon::subagent::spawn_subagent(
            &db,
            &platform_owned,
            &prompt,
            model_owned.as_deref(),
            &workdir_owned,
            &["canopy".to_string()],
            15,
            60,
            None,
        )
        .await
    })
    .await;
    match spawned {
        Ok(Ok((id, _))) => serde_json::json!({
            "status": "running",
            "id": id,
            "platform": platform,
            "model": model,
            "pair": pair,
            "collect_with": "subagent_collect",
            "note": format!("Cost: ran on '{pair}'. Advisory only — does not affect probe results."),
        }),
        Ok(Err(e)) => serde_json::json!({
            "status": "not_spawned",
            "platform": platform,
            "model": model,
            "pair": pair,
            "note": format!("Reviewer could not start ({e}); probes above are unaffected."),
        }),
        Err(join_err) => serde_json::json!({
            "status": "not_spawned",
            "platform": platform,
            "model": model,
            "pair": pair,
            "note": format!("Reviewer could not start ({join_err}); probes above are unaffected."),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::roster_entries::{NewRosterEntry, RosterState};
    use crate::db::Database;
    use crate::domain::graphs::{
        Ensemble, EnsembleDetails, EnsembleKind, EnsembleMember, GraphEdgeCondition,
    };

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

    fn details(kind: EnsembleKind, min_pass: i64, members: Vec<EnsembleMember>) -> EnsembleDetails {
        EnsembleDetails {
            ensemble: Ensemble {
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
            },
            members,
        }
    }

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("t.db")).unwrap()
    }

    fn mark_unavailable(db: &Database, platform: &str) {
        db.roster_upsert(NewRosterEntry {
            platform,
            model_pattern: None,
            state: RosterState::Unavailable,
            reason: Some("quota"),
            until: None,
        })
        .unwrap();
    }

    /// CM48: an ensemble with no available member counts as `would_fail`.
    #[test]
    fn ensemble_with_no_available_member_would_block() {
        let db = test_db();
        mark_unavailable(&db, "opencode-go");
        let d = details(
            EnsembleKind::RoundRobin,
            1,
            vec![
                member("m1", "opencode-go", None),
                member("m2", "opencode-go", None),
            ],
        );
        assert!(ensemble_would_block(&db, &d, &chrono::Utc::now()));
    }

    /// CM48: a `parallel` ensemble below `min_pass` counts as `would_fail`,
    /// while the same member set does not block a sequential kind.
    #[test]
    fn parallel_below_min_pass_would_block_but_sequential_does_not() {
        let db = test_db();
        mark_unavailable(&db, "opencode-go");
        let members = vec![
            member("m1", "claude", None),
            member("m2", "opencode-go", None),
        ];
        let now = chrono::Utc::now();
        assert!(ensemble_would_block(
            &db,
            &details(EnsembleKind::Parallel, 2, members.clone()),
            &now
        ));
        assert!(!ensemble_would_block(
            &db,
            &details(EnsembleKind::RoundRobin, 2, members),
            &now
        ));
    }

    /// CM48: an empty roster leaves every ensemble runnable.
    #[test]
    fn empty_roster_never_would_block() {
        let db = test_db();
        let d = details(
            EnsembleKind::Parallel,
            2,
            vec![member("m1", "claude", None), member("m2", "claude", None)],
        );
        assert!(!ensemble_would_block(&db, &d, &chrono::Utc::now()));
    }
}
