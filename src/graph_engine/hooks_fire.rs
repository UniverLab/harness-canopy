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
        // CM49: a crew-bound hook records the pair its crew resolves to now,
        // so the run row names what actually ran.
        let resolved = hook
            .crew
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(|crew| self.db.resolve_crew_pair(crew, None).ok().flatten())
            .map(|(platform, model, _index)| (platform, model));
        let (platform, model) = match &resolved {
            Some((platform, model)) => (Some(platform.as_str()), model.as_deref()),
            None => (hook.platform.as_deref(), hook.model.as_deref()),
        };
        let (executed_platform, executed_model) = executed_pair_for_platform_model(platform, model);
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
        // CM49: a crew-bound hook resolves its (platform, model) from the crew
        // at fire time. A missing crew, or one with no available member, fails
        // the hook naming the crew (hooks report failure — they never pause
        // the graph, only the reason changes).
        let crew = fire
            .hook
            .crew
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let resolved: Option<(String, Option<String>)> = match crew {
            Some(crew) => match self.resolve_hook_crew(fire, crew) {
                Ok(pair) => Some(pair),
                Err(exec) => return exec,
            },
            None => None,
        };
        let platform = resolved
            .as_ref()
            .map(|(platform, _)| platform.as_str())
            .or(fire.hook.platform.as_deref());
        let model = resolved
            .as_ref()
            .and_then(|(_, model)| model.as_deref())
            .or(fire.hook.model.as_deref());
        // CM48: an agent hook whose pair is declared unavailable never spawns;
        // its run is recorded as failed with the availability message and it
        // is not retried.
        if let Some(platform) = platform {
            let avail = self.availability_for(platform, model, &Utc::now());
            if !avail.available {
                let reason = avail.reason.as_deref().unwrap_or("no reason");
                return Self::hook_unavailable_execution(fire, platform, model, reason);
            }
        }
        let cli = match Cli::resolve(platform) {
            Ok(cli) => cli,
            Err(error) => {
                return Self::hook_failure(
                    serde_json::json!({
                        "platform": platform,
                        "error": error,
                    }),
                    format!(
                        "{} hook has an invalid platform '{}': {error}",
                        fire.event.as_str(),
                        platform.unwrap_or("")
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
                        "platform": platform,
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
            model,
            fire.hook.effort.as_deref(),
            fire.ctx.workdir,
            fire.hook.timeout_minutes.unwrap_or(30),
        )
        .await
    }

    /// CM49: resolve a crew-bound hook's `(platform, model)` at fire time,
    /// failing the hook (naming the crew) when the crew is missing or has no
    /// available member.
    fn resolve_hook_crew(
        &self,
        fire: &HookFireCtx<'_>,
        crew: &str,
    ) -> Result<(String, Option<String>), HookExecution> {
        match self.db.resolve_crew_pair(crew, None) {
            Ok(Some((platform, model, _index))) => Ok((platform, model)),
            Ok(None) => {
                let owner = format!("{} hook", fire.event.as_str());
                let message = if self.db.crew_get(crew).ok().flatten().is_some() {
                    super::crew_resolve::crew_no_member_blocker(&owner, crew)
                } else {
                    super::crew_resolve::crew_missing_blocker(&owner, crew)
                };
                Err(Self::hook_failure(
                    serde_json::json!({ "crew": crew, "error": "crew_unavailable" }),
                    message,
                ))
            }
            Err(error) => Err(Self::hook_failure(
                serde_json::json!({ "crew": crew, "error": error.to_string() }),
                format!("{} hook crew lookup failed: {error}", fire.event.as_str()),
            )),
        }
    }

    /// CM48: the failed hook execution for a hook whose pair is declared
    /// unavailable.
    fn hook_unavailable_execution(
        fire: &HookFireCtx<'_>,
        platform: &str,
        model: Option<&str>,
        reason: &str,
    ) -> HookExecution {
        Self::hook_failure(
            serde_json::json!({
                "platform": platform,
                "model": model,
                "error": "unavailable",
                "reason": reason,
            }),
            node_unavailable_blocker(fire.event.as_str(), platform, model, reason),
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_graph() -> Graph {
        Graph {
            id: "g1".to_string(),
            name: "Graph".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: crate::domain::graphs::GraphStatus::Running,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            archived: false,
            paused_by_reconciliation: false,
            infra_node_id: None,
            allow_dirty_start: false,
        }
    }

    fn agent_hook(platform: &str) -> GraphCompletionHook {
        GraphCompletionHook {
            platform: Some(platform.to_string()),
            model: Some(format!("{platform}/claude")),
            effort: None,
            prompt: Some("hello".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(1),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,

            crew: None,
        }
    }

    fn engine_with_roster(platform: &str) -> (tempfile::TempDir, GraphEngine) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::new(&dir.path().join("t.db")).unwrap());
        db.roster_upsert(crate::db::roster_entries::NewRosterEntry {
            platform,
            model_pattern: None,
            state: crate::db::roster_entries::RosterState::Unavailable,
            reason: Some("quota"),
            until: None,
        })
        .unwrap();
        let engine = GraphEngine::new(
            Arc::clone(&db),
            Arc::new(crate::application::notification_service::DefaultNotificationService),
        );
        (dir, engine)
    }

    /// CM48: an agent hook whose pair is declared unavailable never spawns —
    /// its run is recorded as failed with the availability message. Remove the
    /// guard and this hook would fall through to `Cli::resolve` (failing with
    /// a different message, or actually spawning), so the exact summary is the
    /// assertion that pins the behaviour.
    #[tokio::test]
    async fn unavailable_agent_hook_is_recorded_failed_without_spawning() {
        let (_dir, engine) = engine_with_roster("opencode-go");
        let lp = test_graph();
        let hook = agent_hook("opencode-go");
        let ctx = HookContext {
            graph_name: &lp.name,
            workdir: &lp.workdir,
            completed_specs: &[],
            spec_name: None,
            spec_id: None,
            blocker: None,
            node_name: None,
        };
        let fire = HookFireCtx {
            lp: &lp,
            hook: &hook,
            event: &GraphHookEvent::OnCompleted,
            ctx: &ctx,
            run_id: "run1",
            idx: 0,
        };

        let execution = engine.execute_agent_hook(&fire).await;
        assert_eq!(execution.status, GraphRunStatus::Fail);
        assert_eq!(
            execution.output.get("error").and_then(Value::as_str),
            Some("unavailable")
        );
        assert_eq!(
            execution.summary,
            "node 'on_completed': opencode-go/opencode-go/claude unavailable — quota"
        );
    }
}
