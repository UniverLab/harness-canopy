//! `send_to_session` delivery for background agents (CM53).
//!
//! When an agent's action is `SendToSession`, a fire delivers the rendered
//! prompt into a live interactive session by enqueuing one due-now scheduled
//! send — the same transport the interactive graph hook (CH3) and
//! `scheduled_send_create` (CM22) use. No CLI process is ever spawned on this
//! path; delivery itself stays TUI-driven, so a send enqueued while no TUI is
//! open simply waits.

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::{append_to_log, CliRunResult, ExecutionContext, Executor};
use crate::application::ports::RunRepository;
use crate::db::scheduled_sends::ScheduledSendProvenance;
use crate::db::session_target::SessionTarget;
use crate::domain::models::{Agent, RunStatus};
use crate::scheduler::substitute_variables;

/// A resolved delivery target: the live session id plus the label to name in
/// logs and summaries (the configured name, or the id when targeted by id).
struct ResolvedTarget {
    id: String,
    display: String,
}

/// A failed target resolution: what the operator configured and why it failed.
struct SendFailure {
    target: String,
    reason: String,
}

impl Executor {
    /// Deliver one fire of a `send_to_session` agent. Returns the run's exit
    /// code (0 delivered/coalesced, 1 target or enqueue failure, -1 skipped
    /// because another run is already active). Never spawns a process.
    pub(super) async fn deliver_to_session(
        &self,
        agent: &Agent,
        ctx: &ExecutionContext<'_>,
    ) -> Result<i32> {
        let Some(run_id) = self.start_run(agent, ctx.trigger_type)? else {
            return Ok(-1);
        };

        let now = Utc::now();
        let provenance =
            ScheduledSendProvenance::agent_trigger(&agent.id, provenance_trigger(agent));

        let (summary, result) = match self.resolve_send_target(agent) {
            Err(failure) => (
                failure_summary(&agent.id, &failure.target, &failure.reason),
                failed_result(),
            ),
            Ok(target) => {
                let prompt = render_send_prompt(agent, ctx);
                match self.deliver_or_coalesce(agent, &target, &prompt, now, &provenance) {
                    Ok(summary) => (summary, ok_result()),
                    Err(error) => (
                        failure_summary(&agent.id, &target.display, &error.to_string()),
                        failed_result(),
                    ),
                }
            }
        };

        let status = if result.success {
            RunStatus::Success
        } else {
            RunStatus::Error
        };
        self.db.update_run_status(&run_id, status, Some(&summary))?;
        append_to_log(
            &agent.log_path,
            &agent.id,
            &ctx.trigger_type,
            &now,
            result.exit_code,
            &[],
            summary.as_bytes(),
        )?;

        let is_watch = ctx.is_watch || agent.is_watch();
        self.finalize_run(agent, &run_id, &result, is_watch);
        self.notify_result(agent, &result, ctx.is_watch, ctx.is_manual);

        Ok(result.exit_code)
    }

    /// Resolve the configured target to exactly one live session. A missing or
    /// ambiguous target fails; it never redirects to another session.
    fn resolve_send_target(&self, agent: &Agent) -> Result<ResolvedTarget, SendFailure> {
        if let Some(name) = agent.action.target_session_name() {
            return match self.db.resolve_target_session_name(name) {
                Ok(SessionTarget::Found { id, .. }) => Ok(ResolvedTarget {
                    id,
                    display: name.to_string(),
                }),
                Ok(SessionTarget::Missing) => Err(SendFailure {
                    target: name.to_string(),
                    reason: "no live session has this name".to_string(),
                }),
                Ok(SessionTarget::Ambiguous(many)) => Err(SendFailure {
                    target: name.to_string(),
                    reason: format!(
                        "multiple live sessions share this name: {}",
                        many.join(", ")
                    ),
                }),
                Err(error) => Err(SendFailure {
                    target: name.to_string(),
                    reason: error.to_string(),
                }),
            };
        }
        if let Some(id) = agent.action.target_session_id() {
            return match self.db.resolve_target_session_id(id) {
                Ok(SessionTarget::Found { id, .. }) => Ok(ResolvedTarget {
                    display: id.clone(),
                    id,
                }),
                Ok(_) => Err(SendFailure {
                    target: id.to_string(),
                    reason: "no live session has this id".to_string(),
                }),
                Err(error) => Err(SendFailure {
                    target: id.to_string(),
                    reason: error.to_string(),
                }),
            };
        }
        Err(SendFailure {
            target: String::new(),
            reason: "no target session configured".to_string(),
        })
    }

    /// Coalesce into an existing undelivered send for this agent and target,
    /// or insert a fresh one. Returns the run summary naming the target.
    fn deliver_or_coalesce(
        &self,
        agent: &Agent,
        target: &ResolvedTarget,
        prompt: &str,
        now: DateTime<Utc>,
        provenance: &ScheduledSendProvenance,
    ) -> Result<String> {
        if let Some(existing) = self
            .db
            .find_pending_agent_trigger_send(&agent.id, &target.id)?
        {
            if self
                .db
                .replace_scheduled_send(&existing.id, prompt, now, provenance)?
            {
                return Ok(coalesced_summary(&existing.id, &target.display));
            }
            tracing::info!(
                "Pending send '{}' vanished before coalescing; inserting a new one",
                existing.id
            );
        }
        let send_id = format!("ss-{}", uuid::Uuid::new_v4());
        self.db.insert_scheduled_send(
            &send_id,
            prompt,
            &target.id,
            agent.working_dir.as_deref(),
            now,
            None,
            Some(provenance),
        )?;
        Ok(delivered_summary(&target.display, &target.id))
    }
}

/// The provenance trigger label for a send agent: `"watch"` or `"cron"`.
fn provenance_trigger(agent: &Agent) -> &'static str {
    if agent.is_watch() {
        "watch"
    } else {
        "cron"
    }
}

/// Render the prompt the session receives: the same variable substitution the
/// headless path uses, plus (for a watch fire) a short block naming the change.
fn render_send_prompt(agent: &Agent, ctx: &ExecutionContext<'_>) -> String {
    let mut prompt = substitute_variables(
        &agent.prompt,
        &agent.id,
        &agent.log_path,
        ctx.file_path,
        ctx.event_type,
    );
    if agent.is_watch() && !ctx.is_manual {
        prompt.push_str(&format!(
            "\n\n---\nTriggered by canopy watch agent '{}'.\nChanged path: {}\nEvent: {}",
            agent.id,
            ctx.file_path.unwrap_or(""),
            ctx.event_type.unwrap_or("")
        ));
    }
    prompt
}

fn ok_result() -> CliRunResult {
    CliRunResult {
        exit_code: 0,
        success: true,
    }
}

fn failed_result() -> CliRunResult {
    CliRunResult {
        exit_code: 1,
        success: false,
    }
}

fn delivered_summary(display: &str, id: &str) -> String {
    format!(
        "Delivered to session '{display}' ({id}): one scheduled send queued for now; \
         it is delivered when a TUI is open."
    )
}

fn coalesced_summary(send_id: &str, display: &str) -> String {
    format!(
        "Coalesced into pending send '{send_id}' for session '{display}': prompt and fire \
         time replaced; it will be delivered when a TUI is open."
    )
}

fn failure_summary(agent_id: &str, target: &str, reason: &str) -> String {
    format!(
        "Agent '{agent_id}' could not deliver to session '{target}': {reason}. \
         Open the session or change the target with agent_update."
    )
}
