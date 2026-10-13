use super::*;
use chrono::Utc;

pub(crate) struct HookFireCtx<'a> {
    pub(crate) lp: &'a Graph,
    pub(crate) hook: &'a GraphCompletionHook,
    pub(crate) event: &'a GraphHookEvent,
    pub(crate) ctx: &'a HookContext<'a>,
    pub(crate) run_id: &'a str,
    pub(crate) idx: usize,
}

impl GraphEngine {
    /// Fire all hooks registered for `event`, in declaration order. Each
    /// hook gets its own `GraphCompletionHookRun` row (visible via
    /// `graph_get`/`canopy graph info`), and a failed hook is recorded and
    /// never stops the remaining hooks of that event from running. Never
    /// returns an `Err` — a malformed hook config or a failed process must
    /// never propagate past the caller.
    pub(crate) async fn fire_hooks(
        &self,
        lp: &Graph,
        event: GraphHookEvent,
        ctx: &HookContext<'_>,
    ) {
        // CM45: read the graph's hooks as stored RIGHT NOW. `lp` is the run
        // context's single `&Graph`, captured when the run was claimed/resumed;
        // a `graph_update hooks=` made mid-run must be the edit that fires. Read
        // through the same accessor every other graph read uses
        // (`Database::get_graph`), never from `lp.hooks`.
        let Ok(Some(fresh)) = self.db.get_graph(&lp.id) else {
            return;
        };
        let Some(hooks) = fresh.hooks.get(&event) else {
            return;
        };
        if hooks.is_empty() {
            return;
        }
        for (idx, hook) in hooks.iter().enumerate() {
            let Some(run_id) = self.record_hook_run_start(lp, &event, hook, idx) else {
                continue;
            };
            if self.refuse_graph_hook_at_depth(lp, hook, &run_id) {
                continue;
            }
            let fire = HookFireCtx {
                lp,
                hook,
                event: &event,
                ctx,
                run_id: &run_id,
                idx,
            };
            let execution = self.execute_hook_by_kind(&fire).await;
            let _ = self.db.update_graph_completion_hook_run_result(
                &run_id,
                execution.status,
                Some(&execution.output),
                Some(&execution.summary),
                Some(chrono::Utc::now()),
            );
            self.publish_hook_outcome(&fire, &execution);
            self.report_hook_failure(&fire, &execution);
        }
    }

    pub(crate) fn hook_failure(output: serde_json::Value, summary: String) -> HookExecution {
        HookExecution {
            status: GraphRunStatus::Fail,
            output,
            summary,
        }
    }

    pub(crate) fn hook_kind_label(hook: &GraphCompletionHook) -> &'static str {
        if hook.is_interactive() {
            return "interactive";
        }
        if hook.is_command() {
            return "command";
        }
        if hook.is_graph() {
            return "graph";
        }
        "agent"
    }

    pub(crate) fn hook_outcome_text(execution: &HookExecution) -> String {
        if execution.status == GraphRunStatus::Pass {
            return "passed".to_string();
        }
        match execution
            .output
            .get("exit_code")
            .and_then(serde_json::Value::as_i64)
        {
            Some(code) => format!("failed: exit {code}"),
            None => format!("failed: {}", execution.summary),
        }
    }

    pub(crate) fn record_hook_run_start(
        &self,
        lp: &Graph,
        event: &GraphHookEvent,
        hook: &GraphCompletionHook,
        idx: usize,
    ) -> Option<String> {
        let run_id = uuid::Uuid::new_v4().to_string();
        let (executed_platform, executed_model) =
            executed_pair_for_platform_model(hook.platform.as_deref(), hook.model.as_deref());
        if let Err(error) = self
            .db
            .insert_graph_completion_hook_run(&GraphCompletionHookRun {
                id: run_id.clone(),
                graph_id: lp.id.clone(),
                event: *event,
                hook_index: idx as i64,
                status: GraphRunStatus::Running,
                output: None,
                summary: None,
                started_at: chrono::Utc::now(),
                completed_at: None,
                pid: None,
                boot_id: None,
                executed_platform,
                executed_model,
                hook_config: Some(hook.clone()),
            })
        {
            tracing::warn!(
                "Graph '{}' failed to record {} hook run (index {idx}): {:#}",
                lp.name,
                event.as_str(),
                error
            );
            return None;
        }
        Some(run_id)
    }

    pub(crate) fn refuse_graph_hook_at_depth(
        &self,
        lp: &Graph,
        hook: &GraphCompletionHook,
        run_id: &str,
    ) -> bool {
        if !hook.is_graph() || !self.is_hook_launched_graph(&lp.id) {
            return false;
        }
        let _ = self.db.update_graph_completion_hook_run_result(
            run_id,
            GraphRunStatus::Fail,
            Some(&serde_json::json!({
                "error": "graph hook refused: this graph was itself launched by a hook (depth cap is 1)",
                "target_graph_id": hook.target_graph_id,
            })),
            Some(&format!(
                "Graph hook refused: '{}' is at depth 1. Depth cap is 1.",
                lp.name
            )),
            Some(chrono::Utc::now()),
        );
        true
    }

    pub(crate) async fn execute_hook_by_kind(&self, fire: &HookFireCtx<'_>) -> HookExecution {
        if fire.hook.is_interactive() {
            return self
                .execute_interactive_hook(fire.lp, fire.hook, fire.event, fire.ctx)
                .await;
        }
        if fire.hook.is_command() {
            return self.execute_command_hook(fire).await;
        }
        if fire.hook.is_graph() {
            return self
                .execute_graph_hook(fire.lp, fire.hook, fire.event, fire.ctx)
                .await;
        }
        self.execute_agent_hook(fire).await
    }

    pub(crate) async fn execute_command_hook(&self, fire: &HookFireCtx<'_>) -> HookExecution {
        let raw_command = fire.hook.command.as_deref().unwrap();
        let command = match render_hook_command(fire.event, fire.ctx, raw_command) {
            Ok(command) => command,
            Err(error) => {
                return Self::hook_failure(
                    serde_json::json!({
                        "command": raw_command,
                        "error": error.to_string(),
                    }),
                    format!("{} hook command is invalid: {error}", fire.event.as_str()),
                );
            }
        };
        let timeout_seconds = fire.hook.timeout_minutes.unwrap_or(30) * 60;
        match execute_shell_command(
            &self.db,
            fire.run_id,
            &command,
            fire.ctx.workdir,
            timeout_seconds,
            &hook_env_vars(fire.event, fire.ctx),
        )
        .await
        {
            Ok(r) => HookExecution {
                status: r.status,
                output: r.output,
                summary: r.summary,
            },
            Err(e) => Self::hook_failure(
                serde_json::json!({
                    "command": command,
                    "error": e.to_string(),
                }),
                format!(
                    "{} hook command failed to execute: {e}",
                    fire.event.as_str()
                ),
            ),
        }
    }

    pub(crate) async fn execute_agent_hook(&self, fire: &HookFireCtx<'_>) -> HookExecution {
        // CM48: an agent hook whose pair is declared unavailable never spawns;
        // its run is recorded as failed with the availability message and it
        // is not retried.
        if let Some(platform) = fire.hook.platform.as_deref() {
            let avail = self.availability_for(platform, fire.hook.model.as_deref(), &Utc::now());
            if !avail.available {
                let reason = avail.reason.as_deref().unwrap_or("no reason");
                return Self::hook_failure(
                    serde_json::json!({
                        "platform": fire.hook.platform,
                        "model": fire.hook.model,
                        "error": "unavailable",
                        "reason": reason,
                    }),
                    node_unavailable_blocker(
                        fire.event.as_str(),
                        platform,
                        fire.hook.model.as_deref(),
                        reason,
                    ),
                );
            }
        }
        let cli = match Cli::resolve(fire.hook.platform.as_deref()) {
            Ok(cli) => cli,
            Err(error) => {
                return Self::hook_failure(
                    serde_json::json!({
                        "platform": fire.hook.platform,
                        "error": error,
                    }),
                    format!(
                        "{} hook has an invalid platform '{}': {error}",
                        fire.event.as_str(),
                        fire.hook.platform.as_deref().unwrap_or("")
                    ),
                );
            }
        };
        let prompt = match render_hook_prompt(
            fire.event,
            fire.ctx,
            fire.hook.prompt.as_deref().unwrap_or(""),
        ) {
            Ok(prompt) => prompt,
            Err(error) => {
                return Self::hook_failure(
                    serde_json::json!({
                        "platform": fire.hook.platform,
                        "error": error.to_string(),
                    }),
                    format!("{} hook prompt is invalid: {error}", fire.event.as_str()),
                );
            }
        };
        let mut strategy = cli.strategy();
        if prompt.len() > ARGV_SAFETY_THRESHOLD && !strategy.prompt_via_stdin {
            *strategy = strategy.with_stdin_forced();
        }
        run_completion_hook_process(
            &self.db,
            fire.run_id,
            &cli,
            &strategy,
            &prompt,
            fire.hook.model.as_deref(),
            fire.hook.effort.as_deref(),
            fire.ctx.workdir,
            fire.hook.timeout_minutes.unwrap_or(30),
        )
        .await
    }

    pub(crate) fn publish_hook_outcome(&self, fire: &HookFireCtx<'_>, execution: &HookExecution) {
        let kind_label = Self::hook_kind_label(fire.hook);
        let outcome_text = Self::hook_outcome_text(execution);
        let ok = execution.status == GraphRunStatus::Pass;
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: &fire.lp.id,
                graph_name: &fire.lp.name,
                workdir: fire.ctx.workdir,
                spec_id: None,
                node_id: None,
                run_id: Some(fire.run_id),
                kind: activity::GraphEventKind::HookFired,
                message: format!(
                    "Hook {}[{}] ({kind_label}) {outcome_text}.",
                    fire.event.as_str(),
                    fire.idx
                ),
                payload: Some(serde_json::json!({
                    "event": fire.event.as_str(),
                    "mode": kind_label,
                    "ok": ok,
                    "error": if ok { Value::Null } else { Value::String(execution.summary.clone()) },
                })),
            },
        );
    }

    pub(crate) fn report_hook_failure(&self, fire: &HookFireCtx<'_>, execution: &HookExecution) {
        if execution.status == GraphRunStatus::Pass {
            return;
        }
        tracing::warn!(
            "Graph '{}' {} hook (index {}) failed: {}",
            fire.lp.name,
            fire.event.as_str(),
            fire.idx,
            execution.summary
        );
        self.notification_service
            .notify_graph_completion_hook_failed(&fire.lp.name, &execution.summary);
    }

    pub(crate) fn is_hook_launched_graph(&self, graph_id: &str) -> bool {
        self.db.is_graph_hook_launched(graph_id).unwrap_or(false)
    }

    /// Fire `lp`'s `on_completed` hook (N2), if configured — a no-op
    /// otherwise. Runs through the same spawn path as a graph agent node
    /// ([`run_agent_process`]/[`spawn_and_wait_cli_process`]), records the
    /// firing in `graph_completion_hook_runs` (visible via `graph_get`/`canopy
    /// graph info`), and on failure logs a WARN plus a "post-completion hook
    /// failed" notification. Never returns an `Err` — a malformed hook
    /// config or a failed process must never propagate past the run that
    /// already finished successfully.
    pub(crate) async fn fire_completion_hook(
        &self,
        lp: &crate::domain::graphs::Graph,
        workdir: &str,
        completed_specs: &[(String, String)],
    ) {
        let ctx = HookContext {
            graph_name: &lp.name,
            workdir,
            completed_specs,
            spec_name: None,
            spec_id: None,
            blocker: None,
            node_name: None,
        };
        self.fire_hooks(lp, GraphHookEvent::OnCompleted, &ctx).await;
    }
}
