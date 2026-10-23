use super::*;
use std::collections::VecDeque;

/// A directed run: run `spec_id` starting at the operator-named node.
/// `node_arg` is the raw string the operator passed — a node id, an
/// unambiguous node-id prefix, a node name, or an ensemble name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectedStart {
    pub(crate) spec_id: String,
    pub(crate) node_arg: String,
}

/// What a dispatch carries: whether it resumes an in-flight attempt and,
/// when it is a directed run, which spec/node it starts at.
#[derive(Debug, Clone, Default)]
pub(crate) struct DispatchMode {
    pub(crate) is_resume: bool,
    pub(crate) directed: Option<DirectedStart>,
}

impl DispatchMode {
    pub(crate) fn fresh() -> Self {
        Self {
            is_resume: false,
            directed: None,
        }
    }
    pub(crate) fn resume() -> Self {
        Self {
            is_resume: true,
            directed: None,
        }
    }
    pub(crate) fn fresh_from(directed: DirectedStart) -> Self {
        Self {
            is_resume: false,
            directed: Some(directed),
        }
    }
    pub(crate) fn resume_from(directed: DirectedStart) -> Self {
        Self {
            is_resume: true,
            directed: Some(directed),
        }
    }
}

/// The resolved directed start plus the data the response/activity report.
#[derive(Debug)]
pub(crate) struct DirectedTarget {
    pub(crate) cursor: SpecCursor,
    pub(crate) label: String,
    /// The node ids the cursor starts on: one for a node, members + join for an ensemble.
    pub(crate) start_node_ids: Vec<String>,
    pub(crate) skipped_nodes: Vec<String>,
}

/// The synchronous plan a handler reports to the operator.
#[derive(Debug)]
pub(crate) struct DirectedPlan {
    pub(crate) spec_name: String,
    pub(crate) start_label: String,
    pub(crate) skipped_nodes: Vec<String>,
    /// `(node name, run id)` for every output seeded, most recent per node.
    pub(crate) reused_outputs: Vec<(String, String)>,
    pub(crate) start_head: Option<String>,
}

/// Resolve the operator's `node_arg` to a cursor within one spec's effective graph.
/// Ordered rules: exact ensemble name; then exact node id, exact node name, unique id
/// prefix; a join or member is refused naming its ensemble; anything else is a plain
/// node. A node that is an ensemble's fan-out source (`entry_from_node`) is a plain
/// node — routing fans out to the ensemble after it runs, so it is never reported as
/// skipped and never replaced by its successor.
pub(crate) fn resolve_directed_target(
    node_arg: &str,
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    ensembles: &[EnsembleDetails],
) -> Result<DirectedTarget> {
    let arg = node_arg.trim();
    if let Some(details) = ensembles.iter().find(|d| d.ensemble.name == arg) {
        return Ok(ensemble_target(details, nodes, edges));
    }
    let node = resolve_node(arg, nodes)
        .ok_or_else(|| anyhow!("{}", unknown_target_message(arg, nodes, ensembles)))?;
    if let Some(details) = ensembles.iter().find(|d| {
        d.ensemble.join_node_id == node.id || d.members.iter().any(|m| m.node_id == node.id)
    }) {
        bail!(
            "Node '{}' belongs to ensemble '{}'. Target the ensemble by its name ('{}') \
             or its fan-out node instead.",
            node.name,
            details.ensemble.name,
            details.ensemble.name
        );
    }
    // A node that is an ensemble's fan-out source (`entry_from_node`) is a
    // plain node — routing (routing.rs) fans out to the ensemble after it
    // runs, so it is the start id, never reported as skipped, never replaced
    // by its successor. Do NOT redirect to the ensemble here.
    let entry = find_entry_node(nodes, edges, "directed")?;
    let start_node_ids = vec![node.id.clone()];
    Ok(DirectedTarget {
        cursor: SpecCursor::Node(node.id.clone()),
        label: format!("node '{}'", node.name),
        skipped_nodes: skipped_node_names(nodes, edges, &start_node_ids, &entry),
        start_node_ids,
    })
}

fn ensemble_target(
    details: &EnsembleDetails,
    nodes: &[GraphNode],
    edges: &[GraphEdge],
) -> DirectedTarget {
    let mut start_node_ids: Vec<String> =
        details.members.iter().map(|m| m.node_id.clone()).collect();
    start_node_ids.push(details.ensemble.join_node_id.clone());
    let entry = find_entry_node(nodes, edges, "directed").unwrap_or_default();
    let skipped_nodes = skipped_node_names(nodes, edges, &start_node_ids, &entry);
    DirectedTarget {
        cursor: SpecCursor::Ensemble(details.ensemble.id.clone()),
        label: format!("ensemble '{}'", details.ensemble.name),
        start_node_ids,
        skipped_nodes,
    }
}

/// Exact id, then exact name, then a unique id prefix — all scoped to `nodes`.
/// Returns `None` when nothing matches or a prefix is ambiguous.
fn resolve_node<'a>(arg: &str, nodes: &'a [GraphNode]) -> Option<&'a GraphNode> {
    if let Some(node) = nodes.iter().find(|n| n.id == arg) {
        return Some(node);
    }
    if let Some(node) = nodes.iter().find(|n| n.name == arg) {
        return Some(node);
    }
    let prefixed: Vec<&GraphNode> = nodes.iter().filter(|n| n.id.starts_with(arg)).collect();
    match prefixed.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

fn unknown_target_message(arg: &str, nodes: &[GraphNode], ensembles: &[EnsembleDetails]) -> String {
    let node_names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    let ensemble_names: Vec<&str> = ensembles.iter().map(|e| e.ensemble.name.as_str()).collect();
    format!(
        "No node or ensemble matches '{arg}'. Valid nodes: [{}]. Ensembles: [{}].",
        node_names.join(", "),
        ensemble_names.join(", ")
    )
}

/// Node names on some path from `entry` to the start cursor, excluding the start
/// cursor's own nodes — the "nodes skipped between Start and the start node".
/// A node qualifies only if it can *reach* a start node (walking edges
/// backwards) *and* is reachable from `entry`, so dead-end side branches that
/// the run would never have visited on its way to the start node are not
/// reported as skipped.
pub(crate) fn skipped_node_names(
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    start_node_ids: &[String],
    entry: &str,
) -> Vec<String> {
    let start: HashSet<&str> = start_node_ids.iter().map(String::as_str).collect();
    // Backward reachability: every node with a path to some start node.
    let mut can_reach_start: HashSet<String> = HashSet::new();
    let mut back: VecDeque<String> = start_node_ids.iter().cloned().collect();
    while let Some(id) = back.pop_front() {
        if !can_reach_start.insert(id.clone()) {
            continue;
        }
        for edge in edges.iter().filter(|e| e.to_node == id) {
            if !can_reach_start.contains(&edge.from_node) {
                back.push_back(edge.from_node.clone());
            }
        }
    }
    // Forward reachability from the entry, never expanding through a start
    // node — a path "entry →…→ start" cannot pass through the start.
    let mut from_entry: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    if !start.contains(entry) {
        queue.push_back(entry.to_string());
    }
    while let Some(id) = queue.pop_front() {
        if !from_entry.insert(id.clone()) {
            continue;
        }
        if start.contains(id.as_str()) {
            continue; // never expand through the start
        }
        for edge in edges.iter().filter(|e| e.from_node == id) {
            if !from_entry.contains(&edge.to_node) {
                queue.push_back(edge.to_node.clone());
            }
        }
    }
    nodes
        .iter()
        .filter(|n| {
            !start.contains(n.id.as_str())
                && can_reach_start.contains(&n.id)
                && from_entry.contains(&n.id)
        })
        .map(|n| n.name.clone())
        .collect()
}

/// The `{{output:X}}` names a template references (unescaped markers only).
fn output_refs(template: &str) -> Vec<String> {
    crate::domain::prompts::placeholder_names(template)
        .into_iter()
        .filter_map(|name| name.strip_prefix("output:").map(str::to_string))
        .collect()
}

/// Every template string a node contributes at execution time: for an agent,
/// its effective prompt — explicit `prompt_template`, a `prompt_preset` body,
/// or the default fallback, exactly as [`resolve_node_prompt_template`] will
/// resolve it at spawn — a check's `command`, and — when it is a member — the
/// owning ensemble's shared `prompt_template` and the member's own
/// `prompt_override`.
fn node_templates(
    node: &GraphNode,
    ensembles: &[EnsembleDetails],
    prompts_dir: &std::path::Path,
) -> Vec<String> {
    let mut templates: Vec<String> = Vec::new();
    match node.kind {
        GraphNodeKind::Agent => {
            templates.push(resolve_node_prompt_template(node, prompts_dir));
        }
        GraphNodeKind::Check => {
            if let Some(t) = node.config.get("command").and_then(Value::as_str) {
                templates.push(t.to_string());
            }
        }
        GraphNodeKind::Gate | GraphNodeKind::Join | GraphNodeKind::Router => {}
    }
    if let Some(details) = ensembles
        .iter()
        .find(|d| d.members.iter().any(|m| m.node_id == node.id))
    {
        templates.push(details.ensemble.prompt_template.clone());
        for member in details.members.iter().filter(|m| m.node_id == node.id) {
            if let Some(override_text) = member.prompt_override.clone() {
                templates.push(override_text);
            }
        }
    }
    templates
}

/// Refuse the directed run when any node reachable from the start cursor holds a
/// `{{output:X}}` whose `X` has no recorded output for this spec. Names every
/// missing node and every node that references it.
pub(crate) fn check_reachable_outputs(
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    ensembles: &[EnsembleDetails],
    start_node_ids: &[String],
    node_outputs: &HashMap<String, Value>,
    prompts_dir: &std::path::Path,
) -> Result<()> {
    let mut reachable: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = start_node_ids.iter().cloned().collect();
    while let Some(id) = queue.pop_front() {
        if !reachable.insert(id.clone()) {
            continue;
        }
        for edge in edges.iter().filter(|e| e.from_node == id) {
            queue.push_back(edge.to_node.clone());
        }
    }
    let mut missing: Vec<(String, String)> = Vec::new(); // (missing node, referencing node)
    for node in nodes.iter().filter(|n| reachable.contains(&n.id)) {
        for template in node_templates(node, ensembles, prompts_dir) {
            for referenced in output_refs(&template) {
                if !node_outputs.contains_key(&referenced)
                    && !missing
                        .iter()
                        .any(|(m, r)| m == &referenced && r == &node.name)
                {
                    missing.push((referenced, node.name.clone()));
                }
            }
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let detail = missing
        .iter()
        .map(|(missing_node, referrer)| {
            format!("'{missing_node}' (referenced by node '{referrer}')")
        })
        .collect::<Vec<_>>()
        .join("; ");
    bail!(
        "Directed run refused: reachable prompt(s)/check(s) reference node output(s) with no \
         recorded output for this spec: {detail}. Run the missing node(s) first, or start at an \
         earlier node."
    )
}

/// `(node name, run id)` for every output the directed run actually seeds:
/// only nodes upstream of the resolved cursor (excluding the cursor's own
/// nodes and everything downstream), most recent Pass/Fail run per node, in
/// node order. The cursor's own prior output is never reported as reused.
pub(crate) fn reused_output_records(
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    start_node_ids: &[String],
    runs: &[GraphNodeRun],
) -> Vec<(String, String)> {
    let upstream_ids = directed_upstream_node_ids(edges, start_node_ids);
    let mut latest: HashMap<String, String> = HashMap::new();
    for run in runs {
        if (run.status == GraphRunStatus::Pass || run.status == GraphRunStatus::Fail)
            && run.output.is_some()
            && upstream_ids.contains(&run.node_id)
        {
            if let Some(node) = nodes.iter().find(|n| n.id == run.node_id) {
                latest.insert(node.name.clone(), run.id.clone());
            }
        }
    }
    nodes
        .iter()
        .filter(|n| upstream_ids.contains(&n.id))
        .filter_map(|n| latest.get(&n.name).map(|id| (n.name.clone(), id.clone())))
        .collect()
}

impl GraphEngine {
    /// Synchronous validation + report for a directed run, called from the MCP
    /// handlers before any dispatch. Loads the same effective graph `run_spec`
    /// will (spec-level nodes when the spec has its own, else the graph-level
    /// fallback), resolves the target, and refuses a missing reachable output.
    pub(crate) fn plan_directed_run(
        &self,
        graph_id: &str,
        spec_id: &str,
        node_arg: &str,
    ) -> std::result::Result<DirectedPlan, String> {
        let spec = self
            .db
            .get_graph_spec(spec_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Spec '{spec_id}' not found."))?;
        let details = self
            .db
            .get_graph_spec_details(spec_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Spec '{spec_id}' not found."))?;
        let (nodes, edges, ensembles) = if !details.nodes.is_empty() {
            (
                details.nodes,
                details.edges,
                self.db
                    .list_ensembles_for_spec(spec_id)
                    .map_err(|e| e.to_string())?,
            )
        } else {
            (
                self.db
                    .list_graph_nodes_for_graph(graph_id)
                    .map_err(|e| e.to_string())?,
                self.db
                    .list_graph_edges_for_graph(graph_id)
                    .map_err(|e| e.to_string())?,
                self.db
                    .list_ensembles_for_graph(graph_id)
                    .map_err(|e| e.to_string())?,
            )
        };
        let runs = self
            .db
            .list_graph_runs_for_spec(spec_id)
            .map_err(|e| e.to_string())?;
        let target = resolve_directed_target(node_arg, &nodes, &edges, &ensembles)
            .map_err(|e| e.to_string())?;
        // Validate unsafe upstream prerequisites synchronously, before dispatch.
        let upstream_ids = directed_upstream_node_ids(&edges, &target.start_node_ids);
        check_upstream_prerequisites(&nodes, &upstream_ids, &runs).map_err(|e| e.to_string())?;
        let node_outputs = upstream_node_outputs(&nodes, &upstream_ids, &runs);
        // The same prompts dir the agent spawn path resolves presets from, so
        // a preset body's `{{output:X}}` is validated exactly as it will bind.
        let prompts_dir =
            crate::domain::prompts::prompts_dir(&crate::domain::prompts::canopy_dir());
        check_reachable_outputs(
            &nodes,
            &edges,
            &ensembles,
            &target.start_node_ids,
            &node_outputs,
            &prompts_dir,
        )
        .map_err(|e| e.to_string())?;
        Ok(DirectedPlan {
            spec_name: spec.name,
            start_label: target.label,
            skipped_nodes: target.skipped_nodes,
            reused_outputs: reused_output_records(&nodes, &edges, &target.start_node_ids, &runs),
            start_head: spec.spec_start_head,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graphs::{Ensemble, EnsembleKind};

    fn node(id: &str, name: &str, position: i64, config: Value) -> GraphNode {
        GraphNode {
            id: id.to_string(),
            spec_id: None,
            graph_id: Some("g".to_string()),
            name: name.to_string(),
            kind: GraphNodeKind::Agent,
            config,
            position,
            created_at: chrono::Utc::now(),
        }
    }

    fn edge(from: &str, to: &str, condition: GraphEdgeCondition) -> GraphEdge {
        GraphEdge {
            id: format!("e-{from}-{to}"),
            spec_id: None,
            graph_id: Some("g".to_string()),
            from_node: from.to_string(),
            to_node: to.to_string(),
            condition,
        }
    }

    fn run(
        node_id: &str,
        status: GraphRunStatus,
        output: Option<Value>,
        input: Option<Value>,
    ) -> GraphNodeRun {
        GraphNodeRun {
            id: format!("run-{node_id}"),
            graph_id: "g".to_string(),
            spec_id: "spec-1".to_string(),
            node_id: node_id.to_string(),
            status,
            input,
            output,
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        }
    }

    fn spec(id: &str) -> GraphSpec {
        GraphSpec {
            id: id.to_string(),
            graph_id: Some("g".to_string()),
            name: "S".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
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
        }
    }

    fn ensemble_details(
        id: &str,
        name: &str,
        entry: &str,
        members: &[&str],
        join: &str,
    ) -> EnsembleDetails {
        EnsembleDetails {
            ensemble: Ensemble {
                id: id.to_string(),
                spec_id: None,
                graph_id: Some("g".to_string()),
                name: name.to_string(),
                prompt_template: "do the thing {{output:A}}".to_string(),
                join_node_id: join.to_string(),
                entry_from_node: entry.to_string(),
                entry_condition: GraphEdgeCondition::Always,
                min_pass: members.len() as i64,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: 10,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: "done".to_string(),
                on_fail_to: None,
                kind: EnsembleKind::Parallel,
                round_robin_index: None,
                commit_rights: false,
                created_at: chrono::Utc::now(),

                crew_id: None,
                crew_name: None,
                crew_version_applied: None,
            },
            members: members
                .iter()
                .enumerate()
                .map(|(position, node_id)| EnsembleMember {
                    ensemble_id: id.to_string(),
                    node_id: node_id.to_string(),
                    position: position as i64,
                    platform: "opencode".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                })
                .collect(),
        }
    }

    fn abc_graph() -> (Vec<GraphNode>, Vec<GraphEdge>) {
        let nodes = vec![
            node("A", "A", 0, Value::Null),
            node("B", "B", 1, Value::Null),
            node("C", "C", 2, Value::Null),
        ];
        let edges = vec![
            edge("A", "B", GraphEdgeCondition::Pass),
            edge("B", "C", GraphEdgeCondition::Pass),
        ];
        (nodes, edges)
    }

    #[test]
    fn directed_start_reuses_earlier_outputs_and_restarts_iterations() {
        let s = spec("spec-1");
        let (nodes, edges) = abc_graph();
        let runs = vec![run(
            "A",
            GraphRunStatus::Pass,
            Some(serde_json::json!({"a": "out"})),
            None,
        )];
        let directed = DirectedStart {
            spec_id: s.id.clone(),
            node_arg: "B".to_string(),
        };
        let (cursor, node_outputs, _previous, iterations) =
            resolve_spec_start(&nodes, &edges, &s, &runs, &[], false, Some(&directed)).unwrap();
        assert_eq!(cursor, SpecCursor::Node("B".to_string()));
        assert_eq!(
            node_outputs.get("A"),
            Some(&serde_json::json!({"a": "out"}))
        );
        assert!(iterations.is_empty());
    }

    #[test]
    fn directed_start_at_ensemble_resolves_to_ensemble_cursor() {
        let s = spec("spec-1");
        let nodes = vec![
            node("A", "A", 0, Value::Null),
            node("m1", "M1", 1, Value::Null),
            node("m2", "M2", 2, Value::Null),
            node("j", "Join", 3, Value::Null),
        ];
        let edges = vec![
            edge("A", "m1", GraphEdgeCondition::Always),
            edge("A", "m2", GraphEdgeCondition::Always),
            edge("m1", "j", GraphEdgeCondition::Always),
            edge("m2", "j", GraphEdgeCondition::Always),
        ];
        let ensembles = vec![ensemble_details("ens", "ens-name", "A", &["m1", "m2"], "j")];

        let by_name = resolve_directed_target("ens-name", &nodes, &edges, &ensembles).unwrap();
        assert_eq!(by_name.cursor, SpecCursor::Ensemble("ens".to_string()));

        let by_entry = resolve_directed_target("A", &nodes, &edges, &ensembles).unwrap();
        assert_eq!(by_entry.cursor, SpecCursor::Node("A".to_string()));
        assert!(
            !by_entry.skipped_nodes.contains(&"A".to_string()),
            "the named node must not be listed as skipped: {:?}",
            by_entry.skipped_nodes
        );

        let runs = vec![];
        let directed = DirectedStart {
            spec_id: s.id.clone(),
            node_arg: "ens-name".to_string(),
        };
        let (cursor, _, _, _) = resolve_spec_start(
            &nodes,
            &edges,
            &s,
            &runs,
            &ensembles,
            false,
            Some(&directed),
        )
        .unwrap();
        assert_eq!(cursor, SpecCursor::Ensemble("ens".to_string()));
    }

    #[test]
    fn directed_start_refuses_join_and_member() {
        let nodes = vec![
            node("A", "A", 0, Value::Null),
            node("m1", "M1", 1, Value::Null),
            node("m2", "M2", 2, Value::Null),
            node("j", "Join", 3, Value::Null),
        ];
        let edges = vec![edge("A", "m1", GraphEdgeCondition::Always)];
        let ensembles = vec![ensemble_details("ens", "ens-name", "A", &["m1", "m2"], "j")];

        let join_err = resolve_directed_target("j", &nodes, &edges, &ensembles).unwrap_err();
        assert!(join_err.to_string().contains("ens-name"));
        let member_err = resolve_directed_target("M1", &nodes, &edges, &ensembles).unwrap_err();
        assert!(member_err.to_string().contains("ens-name"));
    }

    #[test]
    fn directed_start_refuses_missing_output_reference() {
        let nodes = vec![
            node("A", "A", 0, Value::Null),
            node(
                "B",
                "B",
                1,
                serde_json::json!({ "prompt_template": "{{output:A}} plus work" }),
            ),
        ];
        let edges = vec![edge("A", "B", GraphEdgeCondition::Pass)];
        let node_outputs = HashMap::new();
        let err = check_reachable_outputs(
            &nodes,
            &edges,
            &[],
            &["B".to_string()],
            &node_outputs,
            std::path::Path::new("/nonexistent-prompts"),
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("'A'"), "message: {message}");
        assert!(message.contains("'B'"), "message: {message}");
    }

    #[test]
    fn directed_refusal_sees_preset_body_references() {
        // A preset file is what `resolve_prompt_preset` reads at spawn time —
        // its body's `{{output:A}}` must be validated exactly like an explicit
        // `prompt_template`'s, or a directed run would silently hand the agent
        // "(not yet executed)" for a reference the operator believed reused.
        let dir = tempfile::tempdir().unwrap();
        let prompts = dir.path().join("prompts");
        std::fs::create_dir_all(&prompts).unwrap();
        std::fs::write(
            prompts.join("custom.md"),
            "reuse the earlier output: {{output:A}}",
        )
        .unwrap();
        let nodes = vec![
            node("A", "A", 0, Value::Null),
            node(
                "B",
                "B",
                1,
                serde_json::json!({ "prompt_preset": "custom" }),
            ),
        ];
        let edges = vec![edge("A", "B", GraphEdgeCondition::Pass)];

        let err = check_reachable_outputs(
            &nodes,
            &edges,
            &[],
            &["B".to_string()],
            &HashMap::new(),
            &prompts,
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("'A'"), "message: {message}");
        assert!(message.contains("'B'"), "message: {message}");

        let mut node_outputs = HashMap::new();
        node_outputs.insert("A".to_string(), serde_json::json!({ "a": 1 }));
        check_reachable_outputs(
            &nodes,
            &edges,
            &[],
            &["B".to_string()],
            &node_outputs,
            &prompts,
        )
        .unwrap();
    }

    #[test]
    fn directed_start_lists_skipped_nodes() {
        let (nodes, edges) = abc_graph();
        let skipped = skipped_node_names(&nodes, &edges, &["C".to_string()], "A");
        assert_eq!(skipped, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn directed_skipped_nodes_exclude_dead_end_side_branches() {
        // A → B → C(start), plus a side branch A → E → F that never reaches
        // C: E and F are reachable from the entry but are NOT between Start
        // and the start node, so they must not be reported as skipped.
        let nodes = vec![
            node("A", "A", 0, Value::Null),
            node("B", "B", 1, Value::Null),
            node("C", "C", 2, Value::Null),
            node("E", "E", 3, Value::Null),
            node("F", "F", 4, Value::Null),
        ];
        let edges = vec![
            edge("A", "B", GraphEdgeCondition::Pass),
            edge("B", "C", GraphEdgeCondition::Pass),
            edge("A", "E", GraphEdgeCondition::Pass),
            edge("E", "F", GraphEdgeCondition::Pass),
        ];
        let skipped = skipped_node_names(&nodes, &edges, &["C".to_string()], "A");
        assert_eq!(skipped, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn directed_output_refs_ignore_escaped_markers() {
        assert!(output_refs("\\{{output:A}}").is_empty());
        assert_eq!(output_refs("{{output:A}}"), vec!["A".to_string()]);
    }

    #[test]
    fn plain_mode_has_no_directed_start() {
        assert!(DispatchMode::fresh().directed.is_none());
        assert!(DispatchMode::resume().directed.is_none());
    }
}
