use super::*;
use crate::db::session_target::SessionTarget;

pub(crate) struct InteractiveSend<'a> {
    pub(crate) target: &'a str,
    pub(crate) target_name: Option<&'a str>,
    pub(crate) rendered: &'a str,
    pub(crate) builder_json: &'a str,
    pub(crate) workdir: &'a str,
    pub(crate) graph_id: &'a str,
    pub(crate) event: &'a GraphHookEvent,
}

impl GraphEngine {
    /// Fire one interactive hook (CH3): render its prompt through the same
    /// event-placeholder path as agent hooks, verify the configured target
    /// session is live in the database, and enqueue one due-now scheduled
    /// send carrying the rendered prompt, a canonical promptbuilder-equivalent
    /// builder state, and structured hook provenance (graph id + event).
    ///
    /// Delivery itself runs in the daemon's scheduler
    /// (`scheduler::cron_scheduler::scheduled_sends`), so a send is delivered
    /// on the next tick whether or not a TUI is attached. A missing or
    /// non-live target fails the hook run naming the exact session
    /// id, without inserting anything and without touching the graph's status
    /// (the caller records the failure and continues to later hooks).
    pub(crate) async fn execute_interactive_hook(
        &self,
        lp: &Graph,
        hook: &GraphCompletionHook,
        event: &GraphHookEvent,
        ctx: &HookContext<'_>,
    ) -> HookExecution {
        let (target_owned, target_name_owned) = match self.resolve_interactive_target(hook, event) {
            Ok(resolved) => resolved,
            Err(failure) => return failure,
        };
        let target: &str = &target_owned;
        let rendered = match Self::render_interactive_hook_prompt(event, ctx, hook, target) {
            Ok(prompt) => prompt,
            Err(failure) => return failure,
        };
        if let Err(failure) = self.verify_live_hook_target(target, event) {
            return failure;
        }
        let builder_json = match Self::encode_hook_builder_state(&rendered, target, event) {
            Ok(json) => json,
            Err(failure) => return failure,
        };
        let send = InteractiveSend {
            target,
            target_name: target_name_owned.as_deref(),
            rendered: &rendered,
            builder_json: &builder_json,
            workdir: ctx.workdir,
            graph_id: &lp.id,
            event,
        };
        self.enqueue_interactive_send(&send)
    }

    pub(crate) fn resolve_target_by_name(
        &self,
        name: &str,
        event: &GraphHookEvent,
    ) -> Result<(String, Option<String>), HookExecution> {
        match self.db.resolve_target_session_name(name) {
            Err(error) => Err(HookExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "target_session_name": name,
                    "error": error.to_string(),
                }),
                summary: format!(
                    "{} hook could not resolve target session name '{name}': {error}",
                    event.as_str()
                ),
            }),
            Ok(SessionTarget::Missing) => Err(HookExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "target_session_name": name,
                    "error": "session does not exist or is no longer live",
                }),
                summary: format!(
                    "{} hook target session name '{name}' does not exist or is no longer live; message not enqueued.",
                    event.as_str()
                ),
            }),
            Ok(SessionTarget::Found { id, name }) => Ok((id, name)),
            Ok(SessionTarget::Ambiguous(many)) => Err(HookExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "target_session_name": name,
                    "error": "multiple live sessions share this name",
                    "matching_ids": many,
                }),
                summary: format!(
                    "{} hook target session name '{name}' matches {} live sessions ({}); message not enqueued.",
                    event.as_str(),
                    many.len(),
                    many.join(", ")
                ),
            }),
        }
    }

    pub(crate) fn resolve_interactive_target(
        &self,
        hook: &GraphCompletionHook,
        event: &GraphHookEvent,
    ) -> Result<(String, Option<String>), HookExecution> {
        let target_id_cfg = hook
            .target_session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let target_name_cfg = hook
            .target_session_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(id) = target_id_cfg {
            return Ok((id.to_string(), None));
        }
        if let Some(name) = target_name_cfg {
            return self.resolve_target_by_name(name, event);
        }
        Err(HookExecution {
            status: GraphRunStatus::Fail,
            output: serde_json::json!({
                "error": "interactive hook has no target session id or name",
            }),
            summary: format!(
                "{} hook has no target session id or name; message not enqueued.",
                event.as_str()
            ),
        })
    }

    pub(crate) fn render_interactive_hook_prompt(
        event: &GraphHookEvent,
        ctx: &HookContext<'_>,
        hook: &GraphCompletionHook,
        target: &str,
    ) -> Result<String, HookExecution> {
        match render_hook_prompt(event, ctx, hook.prompt.as_deref().unwrap_or("")) {
            Ok(prompt) => Ok(prompt),
            Err(error) => Err(HookExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "target_session_id": target,
                    "error": error.to_string(),
                }),
                summary: format!("{} hook prompt is invalid: {error}", event.as_str()),
            }),
        }
    }

    pub(crate) fn verify_live_hook_target(
        &self,
        target: &str,
        event: &GraphHookEvent,
    ) -> Result<(), HookExecution> {
        let live = match self.db.get_active_sessions() {
            Ok(sessions) => sessions,
            Err(error) => {
                return Err(HookExecution {
                    status: GraphRunStatus::Fail,
                    output: serde_json::json!({
                        "target_session_id": target,
                        "error": error.to_string(),
                    }),
                    summary: format!(
                        "{} hook could not verify target session '{target}': {error}",
                        event.as_str()
                    ),
                });
            }
        };
        if live.iter().any(|session| session.id == target) {
            return Ok(());
        }
        Err(HookExecution {
            status: GraphRunStatus::Fail,
            output: serde_json::json!({
                "target_session_id": target,
                "error": "session does not exist or is no longer live",
            }),
            summary: format!(
                "{} hook target session '{target}' does not exist or is no longer live; message not enqueued.",
                event.as_str()
            ),
        })
    }

    pub(crate) fn encode_hook_builder_state(
        rendered: &str,
        target: &str,
        event: &GraphHookEvent,
    ) -> Result<String, HookExecution> {
        let builder_state = crate::tui::PersistedBuilderState::for_instruction_prompt(rendered);
        match serde_json::to_string(&builder_state) {
            Ok(json) => Ok(json),
            Err(error) => Err(HookExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "target_session_id": target,
                    "error": error.to_string(),
                }),
                summary: format!(
                    "{} hook could not encode builder state for session '{target}': {error}",
                    event.as_str()
                ),
            }),
        }
    }

    pub(crate) fn enqueue_interactive_send(&self, send: &InteractiveSend<'_>) -> HookExecution {
        let provenance = match send.target_name {
            Some(name) => ScheduledSendProvenance::hook_with_target_name(
                send.graph_id,
                send.event.as_str(),
                name,
            ),
            None => ScheduledSendProvenance::hook(send.graph_id, send.event.as_str()),
        };
        let send_id = uuid::Uuid::new_v4().to_string();
        match self.db.insert_scheduled_send(
            &send_id,
            send.rendered,
            send.target,
            Some(send.workdir),
            chrono::Utc::now(),
            Some(send.builder_json),
            Some(&provenance),
        ) {
            Ok(()) => {
                let mut output = serde_json::json!({
                    "scheduled_send_id": send_id,
                    "target_session_id": send.target,
                    "provenance": provenance,
                });
                if let Some(name) = send.target_name {
                    output["target_session_name"] = serde_json::Value::String(name.to_string());
                }
                HookExecution {
                    status: GraphRunStatus::Pass,
                    output,
                    summary: format!(
                        "{} hook enqueued a message for live session '{}'; it will be delivered when a TUI is running.",
                        send.event.as_str(),
                        send.target
                    ),
                }
            }
            Err(error) => HookExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "target_session_id": send.target,
                    "error": error.to_string(),
                }),
                summary: format!(
                    "{} hook could not enqueue a message for session '{}': {error}",
                    send.event.as_str(),
                    send.target
                ),
            },
        }
    }
}
