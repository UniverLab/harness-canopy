//! CM50: `graph_crew` — the read-only view over every graph's crew
//! assignments, so "which graphs use this harness or model?" never needs a
//! hand-written SQLite query again.
//!
//! Built entirely on the existing accessors ([`Database::list_graphs`],
//! [`Database::list_all_graph_nodes`], [`Database::list_all_ensembles`],
//! [`Database::list_all_specs_for_conversion`],
//! [`Database::roster_list`]) — one pass each, never one query per graph —
//! so this module adds no SQL of its own. Prompt text is never read into a
//! row: only identity, pair, timeout and the CM49/CM48 metadata travel.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};

use super::Database;
use crate::db::roster_entries::RosterEntry;
use crate::domain::graphs::{
    group_graph_crew_rows, summarize_graph_crew_rows, CrewRowKind, Ensemble, EnsembleDetails,
    Graph, GraphCompletionHook, GraphCrewGroup, GraphCrewRow, GraphCrewSummary, GraphHookEvent,
    GraphNode, GraphNodeKind,
};
use crate::domain::roster::effective_state;

/// The filters [`Database::graph_crew`] accepts. `graph_id` narrows to one
/// graph; `platform` is an exact match, `model` a case-insensitive
/// substring; `include_archived` widens the graph set past the archive.
#[derive(Debug, Default, Clone, Copy)]
pub struct GraphCrewDbParams<'a> {
    /// Full id of the one graph to report on.
    pub graph_id: Option<&'a str>,
    /// Exact platform match (e.g. `claude`, `opencode-go`).
    pub platform: Option<&'a str>,
    /// Case-insensitive substring match against the model (e.g. `opus`).
    pub model: Option<&'a str>,
    /// Include archived graphs. Defaults to `false`.
    pub include_archived: bool,
}

impl GraphCrewDbParams<'_> {
    /// Whether the caller narrowed the result in any way — a narrowed query
    /// omits graphs that ended up with no matching row, while an unnarrowed
    /// one still shows them with an empty `rows` list.
    fn narrows(&self) -> bool {
        self.platform.is_some() || self.model.is_some()
    }

    /// Does `row` survive the caller's pair filters?
    fn matches(&self, row: &GraphCrewRow) -> bool {
        if let Some(platform) = self.platform {
            if row.platform != platform {
                return false;
            }
        }
        if let Some(model) = self.model {
            if !row.model.to_lowercase().contains(&model.to_lowercase()) {
                return false;
            }
        }
        true
    }
}

impl Database {
    /// Every agent node (outside an ensemble), ensemble member and
    /// agent-type hook, grouped per graph — the `graph_crew` result shape.
    ///
    /// Rows carry no prompt text. With `graph_id` the result is that graph
    /// alone (an unknown id is an error, not an empty answer, and an
    /// archived id still resolves); without it, every non-archived graph —
    /// or every graph when `include_archived` — with the pair filters
    /// applied per row, and graphs left with no matching row dropped
    /// whenever a filter was given.
    pub fn graph_crew(&self, params: GraphCrewDbParams<'_>) -> Result<Vec<GraphCrewGroup>> {
        // A `graph_id` must resolve even when the graph is archived: naming
        // it is an explicit request, not an accidental peek past the filter.
        let graphs =
            self.list_graphs(None, params.include_archived || params.graph_id.is_some())?;
        let selected = select_graphs(&graphs, params.graph_id)?;
        let spec_graph = self.spec_graph_map()?;
        let nodes = self.list_all_graph_nodes()?;
        let ensembles = self.list_all_ensembles()?;
        let roster = self.roster_by_platform()?;
        let now = Utc::now();

        let member_nodes = member_node_ids(&ensembles);
        let mut rows = Vec::new();
        for graph in &selected {
            collect_node_rows(
                graph,
                &nodes,
                &spec_graph,
                &member_nodes,
                &params,
                &mut rows,
            );
            collect_member_rows(graph, &ensembles, &spec_graph, &params, &mut rows);
            collect_hook_rows(graph, &params, &mut rows);
        }
        apply_availability(&mut rows, &roster, &now);

        let mut groups = group_graph_crew_rows(&selected, rows);
        if params.narrows() {
            groups.retain(|group| !group.rows.is_empty());
        }
        Ok(groups)
    }

    /// `group_by: "pair"` mode: one row per distinct `(platform, model)`
    /// pair with its graph/ensemble/node/hook counts and graph names,
    /// derived from exactly the rows [`Database::graph_crew`] would return
    /// for the same filters.
    pub fn graph_crew_summary(
        &self,
        params: GraphCrewDbParams<'_>,
    ) -> Result<Vec<GraphCrewSummary>> {
        let rows: Vec<GraphCrewRow> = self
            .graph_crew(params)?
            .into_iter()
            .flat_map(|group| group.rows)
            .collect();
        Ok(summarize_graph_crew_rows(&rows))
    }

    /// `spec_id -> graph_id` for every bound spec, in one query — the map
    /// that lets spec-scoped nodes and ensembles be attributed to their
    /// graph without a lookup each.
    fn spec_graph_map(&self) -> Result<HashMap<String, String>> {
        let specs = self.list_all_specs_for_conversion()?;
        Ok(specs
            .into_iter()
            .filter_map(|spec| spec.graph_id.map(|graph_id| (spec.id, graph_id)))
            .collect())
    }

    /// Every roster entry, bucketed by platform — one query feeding
    /// [`effective_state`] for every row.
    fn roster_by_platform(&self) -> Result<HashMap<String, Vec<RosterEntry>>> {
        let mut by_platform: HashMap<String, Vec<RosterEntry>> = HashMap::new();
        for entry in self.roster_list(None)? {
            by_platform
                .entry(entry.platform.clone())
                .or_default()
                .push(entry);
        }
        Ok(by_platform)
    }
}

/// The graphs this query reports on: one graph when `graph_id` is given
/// (an unknown id fails loudly — silently returning nothing would read as
/// "this graph runs nothing"), otherwise every selected graph in
/// `list_graphs` order.
fn select_graphs<'a>(graphs: &'a [Graph], graph_id: Option<&str>) -> Result<Vec<&'a Graph>> {
    let Some(query) = graph_id else {
        return Ok(graphs.iter().collect());
    };
    match graphs.iter().find(|graph| graph.id == query) {
        Some(graph) => Ok(vec![graph]),
        None => Err(anyhow!(
            "No graph with id '{query}' (searched {} graph(s), archived included).",
            graphs.len()
        )),
    }
}

/// Every node id that belongs to an ensemble — those nodes are reported as
/// ensemble members, never as standalone agent-node rows.
fn member_node_ids(ensembles: &[EnsembleDetails]) -> HashSet<String> {
    ensembles
        .iter()
        .flat_map(|details| details.members.iter())
        .map(|member| member.node_id.clone())
        .collect()
}

/// The graph a spec-scoped node belongs to: its own `graph_id`, or its
/// spec's graph.
fn graph_of_node<'a>(
    node: &'a GraphNode,
    spec_graph: &'a HashMap<String, String>,
) -> Option<&'a str> {
    node.graph_id.as_deref().or_else(|| {
        node.spec_id
            .as_deref()
            .and_then(|id| spec_graph.get(id).map(|graph_id| graph_id.as_str()))
    })
}

/// The graph an ensemble belongs to: its own `graph_id`, or its spec's.
fn graph_of_ensemble<'a>(
    details: &'a EnsembleDetails,
    spec_graph: &'a HashMap<String, String>,
) -> Option<&'a str> {
    details.ensemble.graph_id.as_deref().or_else(|| {
        details
            .ensemble
            .spec_id
            .as_deref()
            .and_then(|id| spec_graph.get(id).map(|graph_id| graph_id.as_str()))
    })
}

/// A config's platform, accepting the legacy `cli` alias exactly like the
/// engine's dispatch paths do. `None` for a crew-bound or unconfigured node.
fn config_platform(config: &serde_json::Value) -> Option<String> {
    ["platform", "cli"]
        .iter()
        .filter_map(|key| config.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_string)
}

/// A config's string field, or the empty string.
fn config_string(config: &serde_json::Value, key: &str) -> String {
    config
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A config's integer field (tolerating a numeric string), if any.
fn config_i64(config: &serde_json::Value, key: &str) -> Option<i64> {
    let value = config.get(key)?;
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|raw| raw.parse().ok()))
}

/// The graph header every row of this graph repeats (grouping input only —
/// `GraphCrewRow`'s graph fields never serialize).
fn row_shell(graph: &Graph) -> GraphCrewRow {
    GraphCrewRow {
        graph_id: graph.id.clone(),
        graph_name: graph.name.clone(),
        graph_status: graph.status.as_str().to_string(),
        graph_workdir: graph.workdir.clone(),
        kind: CrewRowKind::Node,
        node_id: None,
        ensemble_id: None,
        position: None,
        name: String::new(),
        platform: String::new(),
        model: String::new(),
        timeout_minutes: None,
        ensemble_kind: None,
        ensemble_min_pass: None,
        ensemble_commit_rights: None,
        crew: None,
        crew_version_applied: None,
        availability: None,
        availability_reason: None,
        availability_until: None,
    }
}

/// One row per agent node in `graph` that is not an ensemble member.
fn collect_node_rows(
    graph: &Graph,
    nodes: &[GraphNode],
    spec_graph: &HashMap<String, String>,
    member_nodes: &HashSet<String>,
    params: &GraphCrewDbParams<'_>,
    rows: &mut Vec<GraphCrewRow>,
) {
    for node in nodes {
        if node.kind != GraphNodeKind::Agent || member_nodes.contains(&node.id) {
            continue;
        }
        if graph_of_node(node, spec_graph) != Some(graph.id.as_str()) {
            continue;
        }
        let crew = config_string(&node.config, "crew");
        let row = GraphCrewRow {
            kind: CrewRowKind::Node,
            node_id: Some(node.id.clone()),
            name: node.name.clone(),
            platform: config_platform(&node.config).unwrap_or_default(),
            model: config_string(&node.config, "model"),
            timeout_minutes: config_i64(&node.config, "timeout_minutes"),
            crew: (!crew.is_empty()).then_some(crew),
            ..row_shell(graph)
        };
        if params.matches(&row) {
            rows.push(row);
        }
    }
}

/// One row per ensemble member in `graph`, carrying the ensemble's own
/// kind/min_pass/commit_rights and (CM49) its crew binding.
fn collect_member_rows(
    graph: &Graph,
    ensembles: &[EnsembleDetails],
    spec_graph: &HashMap<String, String>,
    params: &GraphCrewDbParams<'_>,
    rows: &mut Vec<GraphCrewRow>,
) {
    for details in ensembles {
        if graph_of_ensemble(details, spec_graph) != Some(graph.id.as_str()) {
            continue;
        }
        let ensemble: &Ensemble = &details.ensemble;
        for member in &details.members {
            let row = GraphCrewRow {
                kind: CrewRowKind::EnsembleMember,
                node_id: Some(member.node_id.clone()),
                ensemble_id: Some(ensemble.id.clone()),
                position: Some(member.position),
                name: ensemble.name.clone(),
                platform: member.platform.clone(),
                // No model means the platform's default — reported as the
                // empty string rather than a guess.
                model: member.model.clone().unwrap_or_default(),
                // A member's own timeout; absent, the ensemble's shared
                // agent timeout is what actually applies to it.
                timeout_minutes: Some(member.timeout_minutes.unwrap_or(ensemble.timeout_minutes)),
                ensemble_kind: Some(ensemble.kind.as_str().to_string()),
                ensemble_min_pass: Some(ensemble.min_pass),
                ensemble_commit_rights: Some(ensemble.commit_rights),
                crew: ensemble.crew_name.clone(),
                crew_version_applied: ensemble.crew_version_applied,
                ..row_shell(graph)
            };
            if params.matches(&row) {
                rows.push(row);
            }
        }
    }
}

/// One row per agent-type hook in `graph`, named by its event.
fn collect_hook_rows(graph: &Graph, params: &GraphCrewDbParams<'_>, rows: &mut Vec<GraphCrewRow>) {
    for (event, hooks) in &graph.hooks {
        for hook in hooks {
            if !hook.is_agent() {
                continue;
            }
            let row = hook_row(graph, *event, hook);
            if params.matches(&row) {
                rows.push(row);
            }
        }
    }
}

/// The single hook row: event name, the hook's own pair/timeout/crew.
fn hook_row(graph: &Graph, event: GraphHookEvent, hook: &GraphCompletionHook) -> GraphCrewRow {
    GraphCrewRow {
        kind: CrewRowKind::Hook,
        name: event.as_str().to_string(),
        platform: hook.platform.clone().unwrap_or_default(),
        model: hook.model.clone().unwrap_or_default(),
        timeout_minutes: hook.timeout_minutes.map(|minutes| minutes as i64),
        crew: hook.crew.clone(),
        ..row_shell(graph)
    }
}

/// Stamp CM48 availability onto every row that has a platform to ask about,
/// from the roster snapshot taken once for the whole query. A crew-bound row
/// has no pair yet, so it stays `None` rather than claiming availability.
fn apply_availability(
    rows: &mut [GraphCrewRow],
    roster: &HashMap<String, Vec<RosterEntry>>,
    now: &DateTime<Utc>,
) {
    for row in rows.iter_mut() {
        if row.platform.is_empty() {
            continue;
        }
        let entries: Vec<RosterEntry> = roster.get(&row.platform).cloned().unwrap_or_default();
        let model = (!row.model.is_empty()).then_some(row.model.as_str());
        let availability = effective_state(&entries, &row.platform, model, now);
        row.availability = Some(if availability.available {
            "available".to_string()
        } else {
            "unavailable".to_string()
        });
        if !availability.available {
            row.availability_reason = availability.reason;
            row.availability_until = availability.until;
        }
    }
}
