//! Target discovery for `agent_probe`/`graph_preflight`: which
//! platform+model pairs a graph or recent-run history references, and the
//! aggregate counts the MCP tools report. Split out of `probe.rs` to keep
//! every production file within the file-size gate.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

use super::{probe_target, ProbeOutcome, ProbeReport, ProbeTarget};
use crate::domain::canopy_config::CanopyConfig;
use crate::domain::graphs::{GraphDetails, GraphNodeKind};

/// Count of reports that are *known* to fail a real run — every outcome
/// except `Reachable` and `Unknown`. Deliberately excludes `Unknown`: a pair
/// whose validity could not be determined is not a confirmed failure, and
/// folding it in here would make `would_fail` claim more certainty than the
/// probe actually has. Shared by `agent_probe` and `graph_preflight` so the
/// two tools can never disagree about what counts as "would fail".
pub(crate) fn would_fail_count(reports: &[ProbeReport]) -> usize {
    reports
        .iter()
        .filter(|r| !r.outcome.reachable() && r.outcome != ProbeOutcome::Unknown)
        .count()
}

/// Count of reports whose validity could not be determined at all (see
/// [`ProbeOutcome::Unknown`]). Reported alongside `would_fail` rather than
/// folded into it — an unknown pair is not a confirmed pass (excluded from
/// `would_fail`'s complement) and not a confirmed failure (excluded from
/// `would_fail` itself), so it needs its own count to stay honest about
/// which pairs were actually validated.
pub(crate) fn unknown_count(reports: &[ProbeReport]) -> usize {
    reports
        .iter()
        .filter(|r| r.outcome == ProbeOutcome::Unknown)
        .count()
}

/// Probe every target concurrently — a single slow/hanging harness must not
/// serialize the rest of the sweep. Results are returned in the same order
/// as `targets`.
pub(crate) async fn probe_targets(
    config: &CanopyConfig,
    targets: &[ProbeTarget],
    workdir: Option<&str>,
    timeout: Duration,
) -> Vec<ProbeReport> {
    let futures = targets
        .iter()
        .map(|target| probe_target(config, target, workdir, timeout));
    futures::future::join_all(futures).await
}

/// One distinct platform+model pair referenced somewhere in a graph, plus the
/// human-readable list of nodes/hooks that reference it — so a caller who
/// sees a pair fail knows which node(s) that affects, without probing the
/// same pair twice.
#[derive(Debug, Clone)]
pub(crate) struct GraphProbeTarget {
    pub target: ProbeTarget,
    pub used_by: Vec<String>,
}

/// Walk a graph's agent nodes (top-level graph and every spec's graph —
/// ensemble members are themselves ordinary [`GraphNodeKind::Agent`] rows, so
/// no separate ensemble query is needed) plus every hook in every event, and
/// return the distinct platform+model pairs they reference. A platform used
/// by five nodes appears once, with all five names attached.
pub(crate) fn distinct_targets_for_graph(details: &GraphDetails) -> Vec<GraphProbeTarget> {
    let mut by_key: HashMap<(String, Option<String>, Option<String>), usize> = HashMap::new();
    let mut result: Vec<GraphProbeTarget> = Vec::new();

    let mut record =
        |platform: Option<&str>, model: Option<&str>, effort: Option<&str>, label: String| {
            let Some(platform) = platform else { return };
            let key = (
                platform.to_string(),
                model.map(str::to_string),
                effort.map(str::to_string),
            );
            if let Some(&idx) = by_key.get(&key) {
                result[idx].used_by.push(label);
            } else {
                by_key.insert(key.clone(), result.len());
                result.push(GraphProbeTarget {
                    target: ProbeTarget {
                        platform: key.0,
                        model: key.1,
                        effort: key.2,
                    },
                    used_by: vec![label],
                });
            }
        };

    for (event, hooks) in &details.lp.hooks {
        for (idx, hook) in hooks.iter().enumerate() {
            // Command hooks have no platform — they are not agent targets.
            record(
                hook.platform.as_deref(),
                hook.model.as_deref(),
                hook.effort.as_deref(),
                format!("{} hook {}", event.as_str(), idx),
            );
        }
    }

    let all_nodes = details
        .graph_nodes
        .iter()
        .chain(details.specs.iter().flat_map(|spec| spec.nodes.iter()));
    for node in all_nodes {
        if node.kind != GraphNodeKind::Agent {
            continue;
        }
        let platform = node
            .config
            .get("platform")
            .or_else(|| node.config.get("cli"))
            .and_then(Value::as_str);
        let model = node.config.get("model").and_then(Value::as_str);
        let effort = node.config.get("effort").and_then(Value::as_str);
        record(platform, model, effort, format!("node: {}", node.name));
    }

    result
}

#[derive(Debug, Clone)]
pub(crate) struct RecentProbeTarget {
    pub target: ProbeTarget,
    pub used_by: Vec<String>,
    pub last_run: chrono::DateTime<chrono::Utc>,
    pub run_count: i64,
    pub last_outcome: String,
}

fn recent_source_label(source: &crate::db::graphs::RecentPairSourceKind) -> String {
    use crate::db::graphs::RecentPairSourceKind::*;
    match source {
        GraphNode {
            graph_id,
            graph_name,
            node_id,
            node_name,
        } => format!("graph '{graph_name}' ({graph_id}) — node '{node_name}' ({node_id})"),
        CompletionHook {
            graph_id,
            graph_name,
            event,
            hook_index,
        } => format!("graph '{graph_name}' ({graph_id}) — {event} hook {hook_index}"),
        BackgroundAgent { agent_id, prompt } => {
            let preview: String = prompt.chars().take(80).collect();
            let suffix = if preview.chars().count() < prompt.chars().count() {
                "..."
            } else {
                ""
            };
            format!("agent {agent_id} (prompt: \"{preview}{suffix}\")")
        }
        SubagentSpawn { workdir } => format!("subagent spawn in workdir '{workdir}'"),
    }
}

pub(crate) fn group_recent_usage(
    rows: &[crate::db::graphs::RecentPairSource],
) -> Vec<RecentProbeTarget> {
    let mut indexes: HashMap<(String, Option<String>), usize> = HashMap::new();
    let mut grouped = Vec::new();
    for row in rows {
        let key = (row.platform.clone(), row.model.clone());
        let index = match indexes.get(&key) {
            Some(index) => *index,
            None => {
                let index = grouped.len();
                indexes.insert(key.clone(), index);
                grouped.push(RecentProbeTarget {
                    target: ProbeTarget {
                        platform: key.0,
                        model: key.1,
                        effort: None,
                    },
                    used_by: Vec::new(),
                    last_run: row.started_at,
                    run_count: 0,
                    last_outcome: row.status.clone(),
                });
                index
            }
        };
        let target = &mut grouped[index];
        target.run_count += 1;
        if row.started_at > target.last_run {
            target.last_run = row.started_at;
            target.last_outcome = row.status.clone();
        }
        let label = recent_source_label(&row.source);
        if !target.used_by.contains(&label) {
            target.used_by.push(label);
        }
    }
    grouped.sort_by_key(|target| std::cmp::Reverse(target.last_run));
    grouped
}
