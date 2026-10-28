//! CM50: the `crew assignments` result types — what runs each node, without any
//! prompt text ever travelling through them.
//!
//! The surface answers one question: "which graphs use this harness or
//! model?". Every row carries identity (`kind`, `node_id`/`ensemble_id` +
//! `position`, name), the pair (`platform`/`model`), the timeout, and the
//! optional CM49 crew / CM48 availability metadata — and nothing else. The
//! plan pins the whole result for 46 graphs under 64 KB, which is exactly
//! what a type with no prompt field guarantees by construction.

use serde::{Deserialize, Serialize};

use super::Graph;

/// One row in a `crew assignments` result: an agent node outside an ensemble, an
/// ensemble member, or an agent-type hook.
///
/// The `graph_*` fields identify which graph the row belongs to and are
/// `skip_serializing`: the wire shape groups rows per graph (see
/// [`GraphCrewGroup`]), so they are grouping input, never output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphCrewRow {
    /// Owning graph id — grouping input, never serialized.
    #[serde(skip_serializing)]
    pub graph_id: String,
    /// Owning graph name — grouping input, never serialized.
    #[serde(skip_serializing)]
    pub graph_name: String,
    /// Owning graph status (`draft`, `running`, …) — grouping input, never
    /// serialized.
    #[serde(skip_serializing)]
    pub graph_status: String,
    /// Owning graph workdir — grouping input, never serialized.
    #[serde(skip_serializing)]
    pub graph_workdir: String,
    /// Which sort of crew assignment this row represents.
    pub kind: CrewRowKind,
    /// The agent node (or member node) this row is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// The ensemble this member belongs to (`kind == ensemble_member`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ensemble_id: Option<String>,
    /// The member's position within its ensemble.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    /// Node name, ensemble name, or hook event name (`on_completed`, …).
    pub name: String,
    /// CLI platform, or empty when the row is bound to a crew instead.
    pub platform: String,
    /// Model, or empty when the row takes the platform's default.
    pub model: String,
    /// Effective agent timeout in minutes, when one is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<i64>,
    /// The ensemble's strategy (`parallel`, `cascade`, `round_robin`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ensemble_kind: Option<String>,
    /// Members required for the ensemble's join to report `pass`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ensemble_min_pass: Option<i64>,
    /// Whether this ensemble is its graph's designated committer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ensemble_commit_rights: Option<bool>,
    /// CM49: the crew this row is bound to, when it names one instead of a
    /// literal platform/model pair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// CM49: the crew version materialized into the row's members.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew_version_applied: Option<i64>,
    /// CM48: `"available"` or `"unavailable"` — `None` when the row has no
    /// platform to ask the roster about (e.g. a crew-bound row).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<String>,
    /// Why the pair is unavailable (CM48), when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_reason: Option<String>,
    /// When the unavailability expires (CM48), if it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_until: Option<String>,
}

/// What kind of crew assignment a [`GraphCrewRow`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrewRowKind {
    /// An agent node that is not an ensemble member.
    Node,
    /// One member of a literal (or crew-materialized) ensemble.
    EnsembleMember,
    /// An agent-type hook on one of the graph's hook events.
    Hook,
}

impl CrewRowKind {
    /// The public kind string — matches the serialized form exactly.
    pub fn as_str(self) -> &'static str {
        match self {
            CrewRowKind::Node => "node",
            CrewRowKind::EnsembleMember => "ensemble_member",
            CrewRowKind::Hook => "hook",
        }
    }
}

/// One graph's slice of a `crew assignments` result — the exact wire shape: the
/// graph's identity plus one row per agent node, ensemble member and agent
/// hook it runs. `rows` is empty for a graph with no crew assignments of
/// its own (e.g. only `check`/`gate` nodes) when no filter was asked for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphCrewGroup {
    /// Graph id.
    pub id: String,
    /// Graph name.
    pub name: String,
    /// Graph status (`draft`, `running`, …).
    pub status: String,
    /// Graph workdir.
    pub workdir: String,
    /// Every crew assignment in this graph, nodes first, then ensemble
    /// members, then hooks.
    pub rows: Vec<GraphCrewRow>,
}

/// Group flat [`GraphCrewRow`]s into the per-graph wire shape: exactly one
/// group per graph in `graphs` order, each seeded even when no row of its
/// own survives (an unfiltered answer still lists a graph that runs
/// nothing), with every row attached to its graph's group.
///
/// A row naming a graph outside `graphs` still opens a new group rather than
/// being dropped.
pub fn group_graph_crew_rows(graphs: &[&Graph], rows: Vec<GraphCrewRow>) -> Vec<GraphCrewGroup> {
    let mut groups: Vec<GraphCrewGroup> = graphs
        .iter()
        .map(|graph| GraphCrewGroup {
            id: graph.id.clone(),
            name: graph.name.clone(),
            status: graph.status.as_str().to_string(),
            workdir: graph.workdir.clone(),
            rows: Vec::new(),
        })
        .collect();
    for row in rows {
        let position = groups.iter().position(|group| group.id == row.graph_id);
        match position {
            Some(index) => groups[index].rows.push(row),
            None => groups.push(GraphCrewGroup {
                id: row.graph_id.clone(),
                name: row.graph_name.clone(),
                status: row.graph_status.clone(),
                workdir: row.graph_workdir.clone(),
                rows: vec![row],
            }),
        }
    }
    groups
}

/// A summary row for `crew assignments { group_by: "pair" }`: everything that
/// uses one distinct `platform`/`model` pair, counted by kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphCrewSummary {
    /// Exact platform of the pair.
    pub platform: String,
    /// Model of the pair (empty = the platform's default model).
    pub model: String,
    /// How many graphs use the pair at least once.
    pub graph_count: usize,
    /// How many ensembles have at least one member on the pair.
    pub ensemble_count: usize,
    /// How many agent nodes run on the pair.
    pub node_count: usize,
    /// How many agent hooks fire on the pair.
    pub hook_count: usize,
    /// The graphs that use the pair, first-seen order, duplicates dropped.
    pub graph_names: Vec<String>,
}

/// Count flat rows into one [`GraphCrewSummary`] per distinct
/// `(platform, model)` pair. Rows with no platform (crew-bound rows, whose
/// pair is only decided at dispatch) are skipped: they are not a
/// `platform/model` pair yet.
pub fn summarize_graph_crew_rows(rows: &[GraphCrewRow]) -> Vec<GraphCrewSummary> {
    let mut summaries: Vec<GraphCrewSummary> = Vec::new();
    for row in rows.iter().filter(|r| !r.platform.is_empty()) {
        let found = summaries
            .iter()
            .position(|s| s.platform == row.platform && s.model == row.model);
        let index = match found {
            Some(index) => index,
            None => {
                summaries.push(GraphCrewSummary {
                    platform: row.platform.clone(),
                    model: row.model.clone(),
                    graph_count: 0,
                    ensemble_count: 0,
                    node_count: 0,
                    hook_count: 0,
                    graph_names: Vec::new(),
                });
                summaries.len() - 1
            }
        };
        let summary = &mut summaries[index];
        if !summary.graph_names.contains(&row.graph_name) {
            summary.graph_names.push(row.graph_name.clone());
        }
        match row.kind {
            CrewRowKind::Node => summary.node_count += 1,
            CrewRowKind::Hook => summary.hook_count += 1,
            // `graph_count` and `ensemble_count` are distinct-entity counts,
            // so both are computed in the second pass below instead of being
            // bumped once per row here.
            CrewRowKind::EnsembleMember => {}
        }
    }
    for summary in summaries.iter_mut() {
        summary.ensemble_count =
            distinct_ensembles_on_pair(rows, &summary.platform, &summary.model);
        summary.graph_count = distinct_graphs_on_pair(rows, &summary.platform, &summary.model);
    }
    summaries.sort_by(|a, b| (&a.platform, &a.model).cmp(&(&b.platform, &b.model)));
    summaries
}

/// How many distinct graphs use this pair at least once.
fn distinct_graphs_on_pair(rows: &[GraphCrewRow], platform: &str, model: &str) -> usize {
    let mut seen: Vec<&str> = Vec::new();
    for row in rows
        .iter()
        .filter(|r| !r.platform.is_empty() && r.platform == platform && r.model == model)
    {
        if !seen.contains(&row.graph_id.as_str()) {
            seen.push(&row.graph_id);
        }
    }
    seen.len()
}

/// How many distinct ensembles have at least one member on this pair.
fn distinct_ensembles_on_pair(rows: &[GraphCrewRow], platform: &str, model: &str) -> usize {
    let mut seen: Vec<&str> = Vec::new();
    for row in rows.iter().filter(|r| {
        r.kind == CrewRowKind::EnsembleMember && r.platform == platform && r.model == model
    }) {
        let ensemble = row.ensemble_id.as_deref().unwrap_or_default();
        if !seen.contains(&ensemble) {
            seen.push(ensemble);
        }
    }
    seen.len()
}
