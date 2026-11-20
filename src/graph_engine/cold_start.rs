//! Cold-start prompt rendering for agent nodes (split from `agent_exec`
//! so that file stays under the 800-line gate).
use super::*;

/// One cold-start prompt render: every input the two render paths read.
/// A struct (not eleven loose arguments) keeps the call within the
/// argument-count lint.
pub(crate) struct ColdStartRender<'a> {
    pub(crate) db: &'a Arc<Database>,
    pub(crate) lp: &'a crate::domain::graphs::Graph,
    pub(crate) spec: &'a GraphSpec,
    pub(crate) node: &'a GraphNode,
    pub(crate) template: &'a str,
    pub(crate) previous_output: Option<&'a Value>,
    pub(crate) workdir: &'a str,
    pub(crate) run_id: &'a str,
    pub(crate) node_outputs: &'a HashMap<String, Value>,
    pub(crate) all_node_names: &'a [String],
    pub(crate) fire: &'a FireCtx,
}

impl ColdStartRender<'_> {
    /// Migrated legacy agents render their verbatim prompt in two passes
    /// (legacy substitution, then the three router bindings) instead of the
    /// graph's normal render — the headless CLI must see the same prompt the
    /// legacy path produced.
    pub(crate) fn prompt(self) -> Result<String, NodeExecution> {
        if let Some(agent_id) = legacy_agent_id(self.node) {
            return render_legacy_agent_prompt(
                self.lp,
                self.node,
                &agent_id,
                self.template,
                self.fire,
            )
            .map_err(|e| invalid_template_execution(self.node, &e));
        }
        // CM47: the spec's working-memory notes travel with every cold start,
        // before `[REPORTING]`.
        let spec_notes_section = build_spec_notes_section(self.db, self.lp, self.spec, self.node);
        render_agent_prompt(
            self.lp,
            self.spec,
            self.node,
            self.template,
            self.previous_output,
            self.workdir,
            self.run_id,
            self.node_outputs,
            self.all_node_names,
            &spec_notes_section,
        )
        .map_err(|e| invalid_template_execution(self.node, &e))
    }
}

/// The node execution for a prompt template that could not be rendered.
pub(crate) fn invalid_template_execution(node: &GraphNode, error: &anyhow::Error) -> NodeExecution {
    NodeExecution {
        status: GraphRunStatus::Fail,
        output: serde_json::json!({
            "failure_kind": "invalid_template",
            "error": error.to_string(),
        }),
        summary: format!(
            "Agent node '{}' prompt template is invalid: {error}",
            node.name
        ),
    }
}

/// CM47: build the `[SPEC NOTES]` section for a cold-start prompt. Fetches
/// the spec's notes, resolves the caller's owner (ensemble when the node is
/// a member, else the node — the same `daemon::handler_notes` ownership the
/// write path enforces) and the Designer, then composes the section. A
/// notes-fetch failure degrades to notes-free (instructions-only) rather
/// than failing the node — notes are working memory, not a gate.
pub(crate) fn build_spec_notes_section(
    db: &Arc<Database>,
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
) -> String {
    let notes = db.spec_notes_get(&spec.id).unwrap_or_default();
    let views: Vec<SpecNoteView<'_>> = notes
        .iter()
        .map(|note| SpecNoteView {
            owner_kind: note.owner_kind.as_str(),
            owner_id: note.owner_id.as_str(),
            owner_name: note.owner_name.as_str(),
            body: note.body.as_str(),
            updated_at: note.updated_at.as_str(),
        })
        .collect();
    let caller = crate::daemon::handler_notes::resolve_owner(db, &node.id)
        .ok()
        .flatten()
        .map(|owner| (owner.kind, owner.id));
    let designer = designer_owner_for_prompt(db, lp, spec);
    let caller_ref = caller.as_ref().map(|(kind, id)| (*kind, id.as_str()));
    let designer_ref = designer
        .as_ref()
        .map(|(kind, id)| (kind.as_str(), id.as_str()));
    compose_spec_notes_section(&views, caller_ref, designer_ref)
}
