//! Section helpers for `graph_preflight_impl`, split out so the orchestrator
//! stays within the size gate. Behaviour unchanged: every helper performs
//! the same reads and builds the same JSON the inline blocks did.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_models_probes::display_for_slug;
use crate::daemon::handler_preflight_review::spawn_preflight_reviewer;
use crate::daemon::handler_read::orphan_join_warning_text;
use crate::daemon::helpers::EarlyReturn;
use crate::daemon::params::*;
use crate::daemon::probe::{GraphProbeTarget, ProbeReport, ProbeTarget};
use crate::db::Database;
use crate::domain::graphs::{EnsembleDetails, GraphDetails, GraphNode, GraphSpecDetails};
use crate::domain::validation::GraphValidationReport;

/// Load the graph's details or fail the preflight when it is missing.
pub(crate) fn fetch_graph_details_or_error(
    db: &Database,
    graph_id: &str,
) -> Result<GraphDetails, EarlyReturn> {
    match db.get_graph_details(graph_id) {
        Ok(Some(details)) => Ok(details),
        Ok(None) => Err(EarlyReturn::tool_error(&format!(
            "Graph '{graph_id}' not found."
        ))),
        Err(e) => Err(internal_error(e.to_string()).into()),
    }
}

/// Dirty-workdir warning value, or `None` for a clean workdir.
pub(crate) async fn dirty_workdir_value(workdir: &str) -> Option<serde_json::Value> {
    let (dirty, _paths) = crate::graph_engine::capture_workdir_dirty_with_paths(workdir).await;
    dirty.filter(|n| *n > 0).map(|count| {
        let plural = if count == 1 { "y" } else { "ies" };
        serde_json::json!({
            "count": count,
            "warning": format!(
                "Workdir '{workdir}' has {count} uncommitted git entr{plural}."
            ),
        })
    })
}

/// Graph-level structural validation (CB8) — same validate_graph
/// used by graph_edit, before spending quota on platform probes.
/// Returns the terminal list and the advisory graph warnings.
pub(crate) fn structural_terminals_and_warnings(
    db: &Database,
    details: &GraphDetails,
) -> Result<(Vec<String>, Vec<serde_json::Value>), EarlyReturn> {
    let report = structural_validation_report(db, details)?;
    Ok(terminal_and_fail_warnings(&report, &details.graph_nodes))
}

fn structural_validation_report(
    db: &Database,
    details: &GraphDetails,
) -> Result<GraphValidationReport, EarlyReturn> {
    let router_labels = router_labels_for(&details.graph_nodes);
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
    let graph_ensembles = db
        .list_ensembles_for_graph(&details.lp.id)
        .map_err(internal_error)?;
    let quorum_views: Vec<crate::domain::validation::EnsembleQuorumView> = graph_ensembles
        .iter()
        .map(|d| crate::domain::validation::EnsembleQuorumView {
            ensemble_id: &d.ensemble.id,
            ensemble_name: &d.ensemble.name,
            join_node_id: &d.ensemble.join_node_id,
            on_fail_to: d.ensemble.on_fail_to.as_deref(),
        })
        .collect();
    match crate::domain::validation::validate_graph_with_ensembles(
        &node_views,
        &edge_views,
        &quorum_views,
    ) {
        Ok(report) => Ok(report),
        Err(e) => {
            // The validator names nodes by id; a live graph's ids are
            // opaque UUIDs, so enrich with the display name where we can
            // (same treatment graph_edit gives its message).
            let mut enriched = e;
            for n in &details.graph_nodes {
                if enriched.contains(n.id.as_str()) {
                    enriched = enriched.replace(n.id.as_str(), &format!("{} ({})", n.name, n.id));
                }
            }
            Err(EarlyReturn::tool_error(&enriched))
        }
    }
}

fn terminal_and_fail_warnings(
    report: &GraphValidationReport,
    nodes: &[GraphNode],
) -> (Vec<String>, Vec<serde_json::Value>) {
    // Terminal exits are informational: a state with no outgoing edge
    // ends the run there (the engine treats a missing edge as an
    // ending). Name nodes the way the operator sees them — by name,
    // with the id in parentheses, matching the error text's shape.
    let terminal_list: Vec<String> = report
        .terminals
        .iter()
        .map(|t| {
            let display = nodes
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
    let mut graph_warnings: Vec<serde_json::Value> = report
        .fail_dead_ends
        .iter()
        .map(|d| {
            let node = nodes.iter().find(|n| n.id == d.node_id);
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
                    "Node {display} [{kind}] has no outgoing 'fail' edge. If it returns a fail verdict the spec terminates at this node with \"no outgoing edge for that status\" and the run stays blocked until a human intervenes.{infra_clause} Add a 'fail' or 'always' edge with graph_edge add if this failure should route somewhere.",
                ),
            })
        })
        .collect();
    // FR1: an ensemble whose quorum has no fail exit ends its spec on
    // failure. Same advisory list as CM19 — never a refusal.
    graph_warnings.extend(quorum_warning_values(&report.quorum_fail_dead_ends, nodes));
    (terminal_list, graph_warnings)
}

/// FR2: the same quorum warning for ensembles owned by a spec's own
/// graph. Failures here are skipped, not refused: the structural check
/// above already errored on a broken graph.
pub(crate) fn spec_quorum_warnings(
    db: &Database,
    specs: &[GraphSpecDetails],
) -> Result<Vec<serde_json::Value>, EarlyReturn> {
    let mut graph_warnings = Vec::new();
    for spec in specs {
        let spec_ensembles = db
            .list_ensembles_for_spec(&spec.spec.id)
            .map_err(internal_error)?;
        if spec_ensembles.is_empty() {
            continue;
        }
        let router_labels = router_labels_for(&spec.nodes);
        let node_views: Vec<crate::domain::validation::GraphNodeView> = spec
            .nodes
            .iter()
            .enumerate()
            .map(|(idx, n)| crate::domain::validation::GraphNodeView {
                id: &n.id,
                kind: n.kind,
                route_labels: &router_labels[idx],
            })
            .collect();
        let edge_views: Vec<crate::domain::validation::GraphEdgeView> = spec
            .edges
            .iter()
            .map(|e| crate::domain::validation::GraphEdgeView {
                from: &e.from_node,
                to: &e.to_node,
                condition: &e.condition,
            })
            .collect();
        let quorum_views: Vec<crate::domain::validation::EnsembleQuorumView> = spec_ensembles
            .iter()
            .map(|d| crate::domain::validation::EnsembleQuorumView {
                ensemble_id: &d.ensemble.id,
                ensemble_name: &d.ensemble.name,
                join_node_id: &d.ensemble.join_node_id,
                on_fail_to: d.ensemble.on_fail_to.as_deref(),
            })
            .collect();
        if let Ok(report) = crate::domain::validation::validate_graph_with_ensembles(
            &node_views,
            &edge_views,
            &quorum_views,
        ) {
            graph_warnings.extend(quorum_warning_values(
                &report.quorum_fail_dead_ends,
                &spec.nodes,
            ));
        }
    }
    Ok(graph_warnings)
}

// CB68 FR2: warn (never rewrite — FR4) about every node, ensemble and
// hook whose prompt/command still names a removed 2.x `loop_<tool>` or
// `canopy loop`, so a graph built before 3.0.0 doesn't silently produce
// an agent that calls a tool that no longer exists.
pub(crate) fn stale_name_warnings(
    db: &Database,
    details: &GraphDetails,
) -> Result<Vec<serde_json::Value>, EarlyReturn> {
    let mut out = stale_node_warnings(&details.graph_nodes);
    for spec in &details.specs {
        out.extend(stale_node_warnings(&spec.nodes));
    }
    for (event, hook_list) in &details.lp.hooks {
        for hook in hook_list {
            push_stale_hook_hits(
                &mut out,
                event.as_str(),
                hook.prompt.as_deref(),
                hook.command.as_deref(),
            );
        }
    }
    let mut ensembles = db
        .list_ensembles_for_graph(&details.lp.id)
        .map_err(internal_error)?;
    for spec in &details.specs {
        ensembles.extend(
            db.list_ensembles_for_spec(&spec.spec.id)
                .map_err(internal_error)?,
        );
    }
    for ens in &ensembles {
        push_stale_ensemble_hits(
            &mut out,
            &ens.ensemble.id,
            &ens.ensemble.name,
            &ens.ensemble.prompt_template,
        );
        for member in &ens.members {
            let Some(prompt_override) = &member.prompt_override else {
                continue;
            };
            push_stale_member_hits(
                &mut out,
                &ens.ensemble.id,
                &ens.ensemble.name,
                prompt_override,
            );
        }
    }
    Ok(out)
}

fn stale_node_warnings(nodes: &[GraphNode]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for node in nodes {
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
    out
}

fn push_stale_hook_hits(
    out: &mut Vec<serde_json::Value>,
    event: &str,
    prompt: Option<&str>,
    command: Option<&str>,
) {
    for text in [prompt, command].into_iter().flatten() {
        for hit in crate::domain::validation::find_stale_2x_names(text) {
            out.push(serde_json::json!({
                "kind": "hook",
                "event": event,
                "old_name": hit.old,
                "new_name": hit.new,
                "warning": format!(
                    "A {event} hook mentions '{}', which no longer exists since 3.0.0 — use '{}'.",
                    hit.old, hit.new
                ),
            }));
        }
    }
}

fn push_stale_ensemble_hits(
    out: &mut Vec<serde_json::Value>,
    ensemble_id: &str,
    ensemble_name: &str,
    prompt_template: &str,
) {
    for hit in crate::domain::validation::find_stale_2x_names(prompt_template) {
        out.push(serde_json::json!({
            "kind": "ensemble",
            "id": ensemble_id,
            "name": ensemble_name,
            "old_name": hit.old,
            "new_name": hit.new,
            "warning": format!(
                "Ensemble '{ensemble_name}' prompt mentions '{}', which no longer exists since 3.0.0 — use '{}'.",
                hit.old, hit.new
            ),
        }));
    }
}

fn push_stale_member_hits(
    out: &mut Vec<serde_json::Value>,
    ensemble_id: &str,
    ensemble_name: &str,
    prompt_override: &str,
) {
    for hit in crate::domain::validation::find_stale_2x_names(prompt_override) {
        out.push(serde_json::json!({
            "kind": "ensemble",
            "id": ensemble_id,
            "name": ensemble_name,
            "old_name": hit.old,
            "new_name": hit.new,
            "warning": format!(
                "Ensemble '{ensemble_name}' member prompt override mentions '{}', which no longer exists since 3.0.0 — use '{}'.",
                hit.old, hit.new
            ),
        }));
    }
}

/// CM1: validate {{output:NodeName}} references. The scan is the
/// shared escape-aware one, so a literal `\{{output:X}}` — accepted at
/// write time since it binds nothing — is not mistaken for a reference.
/// Returns the refusal message when a node references an unknown node.
pub(crate) fn output_reference_error(graph_nodes: &[GraphNode]) -> Option<String> {
    let node_names: Vec<String> = graph_nodes.iter().map(|n| n.name.clone()).collect();
    for node in graph_nodes {
        if let Some(prompt) = node.config.get("prompt_template").and_then(|v| v.as_str()) {
            for referenced_name in crate::domain::prompts::placeholder_names(prompt)
                .iter()
                .filter_map(|name| name.strip_prefix("output:"))
            {
                if !node_names.iter().any(|n| n == referenced_name) {
                    return Some(format!(
                        "Node '{}' references unknown node '{}' in {{{{output:{}}}}}.",
                        node.name, referenced_name, referenced_name
                    ));
                }
            }
        }
    }
    None
}

/// CB40: report pre-existing ensembles with invalid min_pass (FR5).
/// These were stored before the write-time validation existed. Returns
/// the refusal message rather than spending probe quota on a graph
/// that will fail on export.
pub(crate) fn min_pass_error(
    db: &Database,
    graph_id: &str,
    specs: &[GraphSpecDetails],
) -> Result<Option<String>, EarlyReturn> {
    let graph_ensembles = db
        .list_ensembles_for_graph(graph_id)
        .map_err(internal_error)?;
    for ens in &graph_ensembles {
        if let Err(e) = crate::domain::validation::validate_ensemble_min_pass(
            &ens.ensemble.name,
            ens.ensemble.min_pass,
            ens.members.len(),
        ) {
            return Ok(Some(format!(
                "Preflight: {e} Fix with graph_ensemble update (ensemble_id '{}', min_pass ≤ {}).",
                ens.ensemble.id,
                ens.members.len()
            )));
        }
    }
    for spec in specs {
        let spec_ensembles = db
            .list_ensembles_for_spec(&spec.spec.id)
            .map_err(internal_error)?;
        for ens in &spec_ensembles {
            if let Err(e) = crate::domain::validation::validate_ensemble_min_pass(
                &ens.ensemble.name,
                ens.ensemble.min_pass,
                ens.members.len(),
            ) {
                return Ok(Some(format!(
                    "Preflight: {e} Fix with graph_ensemble update (ensemble_id '{}', min_pass ≤ {}).",
                    ens.ensemble.id,
                    ens.members.len()
                )));
            }
        }
    }
    Ok(None)
}

/// CB52 FR4: refuse a graph with orphaned joins before spending
/// probe quota on it. A join node with no ensemble row means the next
/// dispatch through that point dies with "ambiguous outgoing edges" —
/// report the cause instead. Never auto-repaired (C1).
pub(crate) fn orphan_join_error(
    db: &Database,
    graph_id: &str,
    specs: &[GraphSpecDetails],
) -> Result<Option<String>, EarlyReturn> {
    let mut orphans = db
        .list_orphan_join_nodes_for_graph(graph_id)
        .map_err(internal_error)?;
    for spec in specs {
        orphans.extend(
            db.list_orphan_join_nodes_for_spec(&spec.spec.id)
                .map_err(internal_error)?,
        );
    }
    if orphans.is_empty() {
        return Ok(None);
    }
    let parts: Vec<String> = orphans.iter().map(orphan_join_warning_text).collect();
    Ok(Some(format!(
        "Preflight: {}. List it with graph get's orphan_join_warnings and delete it with graph_node delete, or repair the graph manually.",
        parts.join("; ")
    )))
}

/// CM28 / FR3: name whichever node or ensemble in this graph holds commit
/// rights, across the top-level graph and every spec's own graph. A node
/// owned by an ensemble is never listed on its own — only the ensemble is.
pub(crate) fn commit_rights_holders(
    db: &Database,
    details: &GraphDetails,
) -> Result<Vec<serde_json::Value>, EarlyReturn> {
    let graph_ensembles_for_report = db
        .list_ensembles_for_graph(&details.lp.id)
        .map_err(internal_error)?;
    let mut holders = holders_for(&details.graph_nodes, &graph_ensembles_for_report);
    for spec in &details.specs {
        let spec_ensembles = db
            .list_ensembles_for_spec(&spec.spec.id)
            .map_err(internal_error)?;
        holders.extend(holders_for(&spec.nodes, &spec_ensembles));
    }
    Ok(holders)
}

fn holders_for(nodes: &[GraphNode], ensembles: &[EnsembleDetails]) -> Vec<serde_json::Value> {
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
            out.push(serde_json::json!({ "kind": "node", "id": node.id, "name": node.name }));
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

/// (CB22) Bound-spec content warnings: advisory only, never a tool
/// error and never spending probe quota.
pub(crate) fn spec_content_warnings(specs: &[GraphSpecDetails]) -> Vec<serde_json::Value> {
    specs
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
        .collect()
}

/// FR4/CM31: reject before spending any probe quota — this is a static
/// defect in the platform's template, not a runtime reachability
/// question, so there is nothing a probe could tell us that changes the
/// verdict.
pub(crate) fn unsafe_template_error(
    graph_targets: &[GraphProbeTarget],
) -> Result<Option<String>, EarlyReturn> {
    let Some(home) = dirs::home_dir() else {
        return Err(internal_error("No home directory").into());
    };
    let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
    let mut seen_platforms = std::collections::HashSet::new();
    for gt in graph_targets {
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
        if let Some(msg) = crate::domain::cli_strategy::CliStrategy::unsafe_optional_model_token(
            template, platform,
        ) {
            return Ok(Some(format!("Preflight: {msg}")));
        }
    }
    Ok(None)
}

/// (CB25) Spawn the design reviewer before the no-target early return
/// so a review is available on both success shapes. `spawn_subagent`
/// returns an id immediately and never blocks, so the reviewer works
/// while the probes run. Failures degrade to `not_spawned` and
/// never fail the preflight itself.
pub(crate) async fn spawn_requested_reviewer(
    handler: &TaskTriggerHandler,
    details: &GraphDetails,
    reviewer: &Option<GraphPreflightReviewer>,
) -> Option<serde_json::Value> {
    match reviewer {
        None => None,
        Some(r) => Some(
            spawn_preflight_reviewer(
                handler,
                &details.lp.id,
                details.lp.active_run_queue_id.as_deref(),
                &details.lp.workdir,
                &r.platform,
                r.model.as_deref(),
            )
            .await,
        ),
    }
}

/// CM48: split probe targets by availability. An unavailable pair is NOT
/// probed (no quota spent on a pair the engine will skip anyway) and is
/// reported in `skipped_unavailable` instead; an ensemble the roster
/// would stop at dispatch counts as `would_fail`.
pub(crate) type SplitProbeTargets = (
    Vec<(ProbeTarget, Vec<String>)>,
    Vec<serde_json::Value>,
    usize,
);

pub(crate) fn split_probe_targets(
    db: &Database,
    details: &GraphDetails,
    graph_targets: &[GraphProbeTarget],
) -> SplitProbeTargets {
    let now = chrono::Utc::now();
    let (probe_pairs, skipped_unavailable) =
        crate::daemon::handler_preflight_roster::split_targets_by_availability(
            db,
            graph_targets,
            &now,
        );
    let roster_would_fail =
        crate::daemon::handler_preflight_roster::roster_blocked_ensemble_count(db, details, &now);
    (probe_pairs, skipped_unavailable, roster_would_fail)
}

/// One probe report as JSON, annotated with its users, the platform
/// display name, and any effort rejection.
pub(crate) fn probe_value(
    report: &ProbeReport,
    target: &ProbeTarget,
    used_by: &[String],
    config: &crate::domain::canopy_config::CanopyConfig,
) -> serde_json::Value {
    let mut value = report.to_json();
    if let serde_json::Value::Object(map) = &mut value {
        map.insert("used_by".to_string(), serde_json::json!(used_by));
        map.insert(
            "platform_display".to_string(),
            serde_json::json!(display_for_slug(config, &report.platform)),
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
}

pub(crate) fn effort_warnings_for(
    graph_targets: &[GraphProbeTarget],
    config: &crate::domain::canopy_config::CanopyConfig,
) -> Vec<serde_json::Value> {
    graph_targets
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
        .collect()
}

/// CM35: flag every existing node/member/hook that points at a platform
/// marked `headless = false` — the probe above already reports it
/// `interactive_only`, and this surfaces which parts of the graph use
/// it. Advisory only: preflight never fails on this.
pub(crate) fn interactive_only_warnings_for(
    graph_targets: &[GraphProbeTarget],
    config: &crate::domain::canopy_config::CanopyConfig,
) -> Vec<serde_json::Value> {
    graph_targets
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
        .collect()
}

/// Declared route labels for each node, indexed the same as `nodes`; empty
/// for every non-router. Shared by the graph-level and per-spec structural
/// checks so both build identical [`GraphNodeView`]s.
///
/// [`GraphNodeView`]: crate::domain::validation::GraphNodeView
fn router_labels_for(nodes: &[crate::domain::graphs::GraphNode]) -> Vec<Vec<String>> {
    nodes
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
        .collect()
}

/// FR1 warning JSON for ensembles whose quorum has no fail exit. Fields match
/// the CM19 entries in `graph_warnings` plus the ensemble identity.
fn quorum_warning_values(
    dead_ends: &[crate::domain::validation::QuorumFailDeadEnd],
    nodes: &[crate::domain::graphs::GraphNode],
) -> Vec<serde_json::Value> {
    dead_ends
        .iter()
        .map(|d| {
            let node = nodes.iter().find(|n| n.id == d.node_id);
            let name = node.map(|n| n.name.clone()).unwrap_or_default();
            let kind = node.map(|n| n.kind.as_str()).unwrap_or("join");
            serde_json::json!({
                "node_id": d.node_id,
                "node_name": name,
                "kind": kind,
                "ensemble_id": d.ensemble_id,
                "ensemble_name": d.ensemble_name,
                "missing_edge": "fail",
                "warning": format!(
                    "Ensemble '{}' ({}) has no on_fail_to. When its quorum fails (for example, a member reports FAIL with partial work), the spec ends at the quorum and the graph fails instead of routing on. Set on_fail_to with graph_ensemble update if a failed quorum should route somewhere.",
                    d.ensemble_name, d.ensemble_id
                ),
            })
        })
        .collect()
}
