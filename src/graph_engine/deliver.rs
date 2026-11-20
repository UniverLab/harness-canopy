//! A `deliver` node hands a rendered prompt to a live interactive session —
//! the node form of a background agent with `AgentAction::SendToSession`.
//! Deterministic and model-free: the prompt renders through the router
//! decision vocabulary, the target resolves exactly like an interactive
//! hook target (a missing or ambiguous target fails the node with nothing
//! enqueued, never a redirect), and the send is queued as a due-now
//! scheduled send with `deliver` provenance carrying the graph, node and
//! run ids. The daemon's delivery task (woken by the notify this enqueue
//! fires) lands it within seconds; a session that disappears later is
//! preserved by the existing dead-target path.
use super::*;
use crate::db::session_target::SessionTarget;

impl GraphEngine {
    /// Run one `deliver` node: resolve its single target, render its prompt
    /// template with the router decision bindings, and enqueue a due-now
    /// scheduled send. Passes as soon as the send is queued; any failure
    /// (unresolvable template, missing/ambiguous/dead target, enqueue
    /// error) is a `Fail` verdict with nothing enqueued.
    pub(crate) fn execute_deliver_node(
        &self,
        lp: &Graph,
        spec: &GraphSpec,
        node: &GraphNode,
        input: &DeliverInput<'_>,
    ) -> Result<NodeExecution> {
        let (target, target_name) = match self.resolve_deliver_target(node) {
            Ok(resolved) => resolved,
            Err(failure) => return Ok(failure),
        };
        let rendered =
            match Self::render_deliver_prompt(lp, spec, node, input.previous_output, input.fire) {
                Ok(prompt) => prompt,
                Err(failure) => return Ok(failure),
            };
        if let Err(failure) = self.verify_live_deliver_target(node, &target) {
            return Ok(failure);
        }
        let enqueue = DeliverEnqueue {
            workdir: input.workdir,
            target: &target,
            target_name,
            rendered: &rendered,
        };
        Ok(self.enqueue_deliver_send(lp, node, input.run_id, &enqueue))
    }

    /// Resolve the node's single target: exactly one of
    /// `target_session_name`/`target_session_id` (blank counts as unset —
    /// write-time validation enforces the rule, this re-checks the values
    /// actually stored). A name resolves to exactly one live hookable
    /// session; ambiguity is reported, never collapsed to one match.
    fn resolve_deliver_target(
        &self,
        node: &GraphNode,
    ) -> Result<(String, Option<String>), NodeExecution> {
        let target_id = node
            .config
            .get("target_session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let target_name = node
            .config
            .get("target_session_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(id) = target_id {
            return Ok((id.to_string(), None));
        }
        if let Some(name) = target_name {
            return match self.db.resolve_target_session_name(name) {
                Err(error) => Err(deliver_failure(
                    serde_json::json!({
                        "target_session_name": name,
                        "error": error.to_string(),
                    }),
                    format!(
                        "Deliver node '{}' could not resolve target session name '{name}': {error}; message not enqueued.",
                        node.name
                    ),
                )),
                Ok(SessionTarget::Missing) => Err(deliver_failure(
                    serde_json::json!({
                        "target_session_name": name,
                        "error": "session does not exist or is no longer live",
                    }),
                    format!(
                        "Deliver node '{}' target session name '{name}' does not exist or is no longer live; message not enqueued.",
                        node.name
                    ),
                )),
                Ok(SessionTarget::Found { id, name }) => Ok((id, name)),
                Ok(SessionTarget::Ambiguous(many)) => Err(deliver_failure(
                    serde_json::json!({
                        "target_session_name": name,
                        "error": "multiple live sessions share this name",
                        "matching_ids": many,
                    }),
                    format!(
                        "Deliver node '{}' target session name '{name}' matches {} live sessions ({}); message not enqueued.",
                        node.name,
                        many.len(),
                        many.join(", ")
                    ),
                )),
            };
        }
        Err(deliver_failure(
            serde_json::json!({
                "error": "deliver node has no target session id or name",
            }),
            format!(
                "Deliver node '{}' has no target session id or name; message not enqueued.",
                node.name
            ),
        ))
    }

    /// Render the node's prompt template exactly like
    /// [`render_decision_state`]: `previous_output` is the previous node's
    /// output as bounded pretty-printed JSON (`"(none)"` when absent),
    /// `spec_content` is the spec description or name, and unbound markers
    /// are refused against [`ROUTER_DECISION_BINDINGS`].
    ///
    /// A migrated legacy agent's deliver node (`legacy_agent_id` set) takes
    /// the two-pass legacy render instead: the agent prompt verbatim through
    /// legacy substitution then the three router bindings, plus the watch
    /// block on non-manual watch fires.
    fn render_deliver_prompt(
        lp: &Graph,
        spec: &GraphSpec,
        node: &GraphNode,
        previous_output: Option<&Value>,
        fire: &FireCtx,
    ) -> Result<String, NodeExecution> {
        let template = node
            .config
            .get("prompt_template")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("{{previous_output}}");
        if let Some(agent_id) = legacy_agent_id(node) {
            if let Err(error) = refuse_unbindable_legacy_template(&node.name, template) {
                return Err(NodeExecution {
                    status: GraphRunStatus::Fail,
                    output: serde_json::json!({
                        "error": error.to_string(),
                    }),
                    summary: format!("Deliver node '{}' prompt is invalid: {error}", node.name),
                });
            }
            return Ok(render_legacy_migrated_prompt(
                template,
                &agent_id,
                &lp.log_path(),
                fire,
                legacy_watch_append(node),
            ));
        }
        if let Err(error) =
            refuse_unbindable_template(&node.name, template, ROUTER_DECISION_BINDINGS, None)
        {
            return Err(NodeExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "error": error.to_string(),
                }),
                summary: format!("Deliver node '{}' prompt is invalid: {error}", node.name),
            });
        }
        let previous_output_json = previous_output
            .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
            .unwrap_or_else(|| "(none)".to_string());
        let bounded = bound_previous_feedback(previous_output_json);
        let spec_content = spec.description.as_deref().unwrap_or(&spec.name);
        Ok(crate::domain::prompts::render_template(
            template,
            |raw| match raw {
                "previous_feedback" => Some(bounded.clone()),
                "previous_output" => Some(bounded.clone()),
                "spec_content" => Some(spec_content.to_string()),
                "spec_name" => Some(spec.name.clone()),
                _ => None,
            },
        ))
    }

    /// Verify an id-targeted session is still live, mirroring the
    /// interactive hook's live check: a dead target fails the node with
    /// nothing enqueued, never a redirect.
    fn verify_live_deliver_target(
        &self,
        node: &GraphNode,
        target: &str,
    ) -> Result<(), NodeExecution> {
        let live = match self.db.get_active_sessions() {
            Ok(sessions) => sessions,
            Err(error) => {
                return Err(deliver_failure(
                    serde_json::json!({
                        "target_session_id": target,
                        "error": error.to_string(),
                    }),
                    format!(
                        "Deliver node '{}' could not verify target session '{target}': {error}; message not enqueued.",
                        node.name
                    ),
                ));
            }
        };
        if live.iter().any(|session| session.id == target) {
            return Ok(());
        }
        Err(deliver_failure(
            serde_json::json!({
                "target_session_id": target,
                "error": "session does not exist or is no longer live",
            }),
            format!(
                "Deliver node '{}' target session '{target}' does not exist or is no longer live; message not enqueued.",
                node.name
            ),
        ))
    }

    /// Enqueue the rendered prompt as a due-now scheduled send with
    /// `deliver` provenance, wake the delivery task, and pass — mirroring
    /// the interactive hook's enqueue output so runs stay observable.
    fn enqueue_deliver_send(
        &self,
        lp: &Graph,
        node: &GraphNode,
        run_id: &str,
        enqueue: &DeliverEnqueue<'_>,
    ) -> NodeExecution {
        let builder_state = match serde_json::to_string(
            &crate::tui::PersistedBuilderState::for_instruction_prompt(enqueue.rendered),
        ) {
            Ok(json) => json,
            Err(error) => {
                return deliver_failure(
                    serde_json::json!({
                        "target_session_id": enqueue.target,
                        "error": error.to_string(),
                    }),
                    format!(
                        "Deliver node '{}' could not encode builder state for session '{}': {error}; message not enqueued.",
                        node.name, enqueue.target
                    ),
                );
            }
        };
        let provenance = ScheduledPromptProvenance::deliver(&lp.id, &node.id, run_id);
        let send_id = uuid::Uuid::new_v4().to_string();
        match self.db.insert_scheduled_prompt(
            &send_id,
            enqueue.rendered,
            enqueue.target,
            Some(enqueue.workdir),
            chrono::Utc::now(),
            Some(&builder_state),
            Some(&provenance),
        ) {
            Ok(()) => {
                self.scheduled_prompt_notify.notify_one();
                let mut output = serde_json::json!({
                    "scheduled_prompt_id": send_id,
                    "target_session_id": enqueue.target,
                    "provenance": provenance,
                });
                if let Some(name) = enqueue.target_name.clone() {
                    output["target_session_name"] = serde_json::Value::String(name);
                }
                NodeExecution {
                    status: GraphRunStatus::Pass,
                    output,
                    summary: format!(
                        "Deliver node '{}' enqueued a message for live session '{}'; the daemon delivers it within seconds.",
                        node.name, enqueue.target
                    ),
                }
            }
            Err(error) => deliver_failure(
                serde_json::json!({
                    "target_session_id": enqueue.target,
                    "error": error.to_string(),
                }),
                format!(
                    "Deliver node '{}' could not enqueue a message for session '{}': {error}; message not enqueued.",
                    node.name, enqueue.target
                ),
            ),
        }
    }
}

/// One deliver execution's per-run inputs: the previous node's output, the
/// run id, the run workdir, and the legacy fire context. A struct (not four
/// loose arguments) keeps the call within the argument-count lint.
pub(crate) struct DeliverInput<'a> {
    pub(crate) previous_output: Option<&'a Value>,
    pub(crate) run_id: &'a str,
    pub(crate) workdir: &'a str,
    pub(crate) fire: &'a FireCtx,
}

/// What [`GraphEngine::enqueue_deliver_send`] queues: the resolved target
/// plus the rendered prompt. A struct (not four loose arguments) keeps the
/// call within the argument-count lint.
struct DeliverEnqueue<'a> {
    workdir: &'a str,
    target: &'a str,
    target_name: Option<String>,
    rendered: &'a str,
}

/// One `Fail` verdict for every deliver failure path: carries the
/// structured output, enqueues nothing.
fn deliver_failure(output: Value, summary: String) -> NodeExecution {
    NodeExecution {
        status: GraphRunStatus::Fail,
        output,
        summary,
    }
}
