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
    /// FR5: this dispatch was launched by a cron tick of a spec-less graph
    /// (see `GraphEngine::trigger_prompt_idea`), so its idle rule applies: a
    /// check that ends the run with no outgoing Fail edge and no agent node
    /// having run records `completed`/`outcome: idle` instead of `failed`.
    /// Only the scheduler's tick path sets this — a manual `graph_control`
    /// run of the same graph keeps failing exactly as today.
    pub(crate) tick: bool,
    /// Daemon-initiated trigger source for per-graph run logging. `None` for
    /// manual launches (run/continue) which must never append a log block.
    pub(crate) daemon_trigger: Option<crate::graph_engine::graph_run_log::GraphRunTrigger>,
    /// Watch-fire context for this dispatch: the triggering path and event.
    /// `Some` only on the watchers' watch-fire path (or a manual replay of
    /// it); cron ticks and manual runs leave this `None` so legacy
    /// `{{FILE_PATH}}`/`{{EVENT_TYPE}}` markers stay literally substituted
    /// the way the legacy watcher left them.
    pub(crate) watch_fire: Option<WatchFireCtx>,
}

/// The file-system event that fired a watch dispatch: the changed path and
/// the event kind, exactly as the legacy watcher passed them to
/// `execute_agent_with_context` and `render_send_prompt`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WatchFireCtx {
    pub(crate) file_path: String,
    pub(crate) event_type: String,
}

/// What a node render needs to reproduce the legacy agent firing: the
/// daemon trigger (manual when `None`) plus the watch-fire context when
/// this dispatch is a watch fire.
#[derive(Debug, Clone, Default)]
pub(crate) struct FireCtx {
    pub(crate) trigger: Option<crate::graph_engine::graph_run_log::GraphRunTrigger>,
    pub(crate) file_path: Option<String>,
    pub(crate) event_type: Option<String>,
}

impl FireCtx {
    pub(crate) fn is_manual(&self) -> bool {
        self.trigger.is_none()
    }

    /// The fire context for a dispatch from its daemon trigger plus the
    /// watchers' watch-fire context (`None` on every other path, so legacy
    /// markers stay literally substituted the way the legacy paths left
    /// them).
    pub(crate) fn from_dispatch(
        trigger: Option<crate::graph_engine::graph_run_log::GraphRunTrigger>,
        watch: Option<&WatchFireCtx>,
    ) -> Self {
        Self {
            trigger,
            file_path: watch.map(|fire| fire.file_path.clone()),
            event_type: watch.map(|fire| fire.event_type.clone()),
        }
    }
}

/// The fire context for a dispatch: build it from the dispatch's daemon
/// trigger and watch-fire context. Lives here (not on `DispatchCtx`) so
/// `run_spec` stays within its line budget.
pub(crate) fn dispatch_fire_ctx(ctx: &crate::graph_engine::DispatchCtx<'_>) -> FireCtx {
    FireCtx::from_dispatch(ctx.daemon_trigger, ctx.watch_fire.as_ref())
}

impl DispatchMode {
    pub(crate) fn fresh() -> Self {
        Self::default()
    }
    pub(crate) fn resume() -> Self {
        Self {
            is_resume: true,
            ..Self::default()
        }
    }
    pub(crate) fn fresh_from(directed: DirectedStart) -> Self {
        Self {
            directed: Some(directed),
            ..Self::default()
        }
    }
    pub(crate) fn resume_from(directed: DirectedStart) -> Self {
        Self {
            is_resume: true,
            directed: Some(directed),
            ..Self::default()
        }
    }
    /// A fresh dispatch launched by a cron tick (never a resume).
    pub(crate) fn tick() -> Self {
        Self {
            tick: true,
            daemon_trigger: Some(crate::graph_engine::graph_run_log::GraphRunTrigger::Cron),
            ..Self::default()
        }
    }
    pub(crate) fn with_daemon_trigger(
        mut self,
        trigger: crate::graph_engine::graph_run_log::GraphRunTrigger,
    ) -> Self {
        self.daemon_trigger = Some(trigger);
        self
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
    pub(crate) entry: String,
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
        return ensemble_target(details, nodes, edges);
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
        entry,
    })
}

fn ensemble_target(
    details: &EnsembleDetails,
    nodes: &[GraphNode],
    edges: &[GraphEdge],
) -> Result<DirectedTarget> {
    let mut start_node_ids: Vec<String> =
        details.members.iter().map(|m| m.node_id.clone()).collect();
    start_node_ids.push(details.ensemble.join_node_id.clone());
    let entry = find_entry_node(nodes, edges, "directed")?;
    let skipped_nodes = skipped_node_names(nodes, edges, &start_node_ids, &entry);
    Ok(DirectedTarget {
        cursor: SpecCursor::Ensemble(details.ensemble.id.clone()),
        label: format!("ensemble '{}'", details.ensemble.name),
        start_node_ids,
        skipped_nodes,
        entry,
    })
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
    // Forward reachability FROM the start: anything the run reaches after the
    // start is downstream, never skipped. Cycles and bypass edges would
    // otherwise leak downstream nodes in — reachable from the entry AND able
    // to reach a start node via a back edge. DAGs are unaffected (nothing is
    // both strictly upstream and downstream there).
    let mut downstream: HashSet<String> = HashSet::new();
    let mut forward: VecDeque<String> = start_node_ids.iter().cloned().collect();
    while let Some(id) = forward.pop_front() {
        for edge in edges.iter().filter(|e| e.from_node == id) {
            if downstream.insert(edge.to_node.clone()) {
                forward.push_back(edge.to_node.clone());
            }
        }
    }
    nodes
        .iter()
        .filter(|n| {
            !start.contains(n.id.as_str())
                && !downstream.contains(&n.id)
                && can_reach_start.contains(&n.id)
                && from_entry.contains(&n.id)
        })
        .map(|n| n.name.clone())
        .collect()
}

/// Node ids upstream of a directed cursor's start nodes: every node that can
/// reach one of the start nodes (walking edges backwards) *and* is reachable
/// from the entry node on a path that never passes through a start node,
/// excluding the start nodes themselves. A node the entry reaches only by
/// passing through the start node — an Auditor behind a fail back-edge to
/// the Implementer — is not upstream, whatever back-edges lead from it.
pub(crate) fn directed_upstream_node_ids(
    edges: &[GraphEdge],
    start_node_ids: &[String],
    entry: &str,
) -> HashSet<String> {
    let start: HashSet<&str> = start_node_ids.iter().map(String::as_str).collect();
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
    can_reach_start
        .into_iter()
        .filter(|id| !start.contains(id.as_str()) && from_entry.contains(id))
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
        // A deliver prompt renders with the router decision vocabulary —
        // no `{{output:...}}` can survive write-time validation, so this
        // only matters for template-shape audits, never for output gating.
        GraphNodeKind::Deliver => {
            if let Some(t) = node.config.get("prompt_template").and_then(Value::as_str) {
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
    entry: &str,
    runs: &[GraphNodeRun],
) -> Vec<(String, String)> {
    let upstream_ids = directed_upstream_node_ids(edges, start_node_ids, entry);
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
        let upstream_ids =
            directed_upstream_node_ids(&edges, &target.start_node_ids, &target.entry);
        check_upstream_prerequisites(
            &nodes,
            &edges,
            &upstream_ids,
            &target.start_node_ids,
            &runs,
            &ensembles,
        )
        .map_err(|e| e.to_string())?;
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
        // FR4: when the column was cleared (a pause reconciles it away), the
        // plan reports the head this run's first `spec_started` recorded —
        // the same baseline the dispatch will reuse. A failed lookup reports
        // none, exactly as before.
        let start_head = spec.spec_start_head.clone().or_else(|| {
            self.db
                .spec_started_head(graph_id, spec_id)
                .ok()
                .flatten()
                .and_then(|(head, _)| head)
        });
        Ok(DirectedPlan {
            spec_name: spec.name,
            start_label: target.label,
            skipped_nodes: target.skipped_nodes,
            reused_outputs: reused_output_records(
                &nodes,
                &edges,
                &target.start_node_ids,
                &target.entry,
                &runs,
            ),
            start_head,
        })
    }
}

#[cfg(test)]
mod tests;
