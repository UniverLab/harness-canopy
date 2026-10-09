use super::*;

/// Inputs for launching a graph hook target.
pub(crate) struct GraphHookLaunch<'a> {
    pub(crate) lp: &'a Graph,
    pub(crate) target_id: String,
    pub(crate) target_name: String,
    pub(crate) queue_id: Option<&'a str>,
    pub(crate) workdir_override: Option<String>,
    pub(crate) rendered_idea: Option<String>,
    pub(crate) event: &'a GraphHookEvent,
}

impl GraphEngine {
    pub(crate) async fn execute_graph_hook(
        &self,
        lp: &Graph,
        hook: &GraphCompletionHook,
        event: &GraphHookEvent,
        ctx: &HookContext<'_>,
    ) -> HookExecution {
        let raw_target = hook.target_graph_id.as_deref().unwrap_or("");
        let target_id = match self.resolve_hook_target_graph_id(raw_target) {
            Ok(id) => id,
            Err(failure) => return failure,
        };
        let target_lp = match self.load_hook_target_graph(&target_id, raw_target) {
            Ok(lp) => lp,
            Err(failure) => return failure,
        };
        if let Err(failure) = Self::validate_hook_target_state(&target_lp) {
            return failure;
        }
        let queue_id = hook.queue_id.as_deref().filter(|s| !s.trim().is_empty());
        let idea = hook.idea.as_deref().filter(|s| !s.trim().is_empty());
        if let Err(failure) = Self::validate_hook_queue_idea(queue_id, idea) {
            return failure;
        }
        let rendered_idea = match Self::render_hook_idea_or_fail(event, ctx, idea) {
            Ok(rendered) => rendered,
            Err(failure) => return failure,
        };
        let workdir_override = hook
            .workdir_override
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);
        self.launch_hook_target(GraphHookLaunch {
            lp,
            target_id,
            target_name: target_lp.name,
            queue_id,
            workdir_override,
            rendered_idea,
            event,
        })
    }

    pub(crate) fn hook_target_not_found(raw_target: &str) -> HookExecution {
        HookExecution {
            status: GraphRunStatus::Fail,
            output: serde_json::json!({
                "error": format!("target graph '{raw_target}' not found"),
            }),
            summary: format!("Graph hook failed: target graph '{raw_target}' not found."),
        }
    }

    pub(crate) fn resolve_hook_target_graph_id(
        &self,
        raw_target: &str,
    ) -> Result<String, HookExecution> {
        match Database::resolve_graph_id_by_prefix(&self.db, raw_target) {
            Ok(Some(id)) => Ok(id),
            Ok(None) => Err(Self::hook_target_not_found(raw_target)),
            Err(e) => Err(HookExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({ "error": e.to_string() }),
                summary: format!("Graph hook failed to resolve target: {e}"),
            }),
        }
    }

    pub(crate) fn load_hook_target_graph(
        &self,
        target_id: &str,
        raw_target: &str,
    ) -> Result<Graph, HookExecution> {
        match self.db.get_graph(target_id) {
            Ok(Some(lp)) => Ok(lp),
            Ok(None) => Err(Self::hook_target_not_found(raw_target)),
            Err(e) => Err(HookExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({ "error": e.to_string() }),
                summary: format!("Graph hook failed to read target: {e}"),
            }),
        }
    }

    pub(crate) fn validate_hook_target_state(target_lp: &Graph) -> Result<(), HookExecution> {
        if target_lp.archived {
            return Err(HookExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "error": format!("target graph '{}' is archived", target_lp.name),
                }),
                summary: format!(
                    "Graph hook failed: target graph '{}' is archived.",
                    target_lp.name
                ),
            });
        }
        if target_lp.status == GraphStatus::Running {
            return Err(HookExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "error": format!("target graph '{}' is already running", target_lp.name),
                }),
                summary: format!(
                    "Graph hook failed: target graph '{}' is already running.",
                    target_lp.name
                ),
            });
        }
        Ok(())
    }

    pub(crate) fn validate_hook_queue_idea(
        queue_id: Option<&str>,
        idea: Option<&str>,
    ) -> Result<(), HookExecution> {
        if queue_id.is_some() && idea.is_some() {
            return Err(HookExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "error": "queue_id and idea are mutually exclusive",
                }),
                summary: "Graph hook failed: queue_id and idea are mutually exclusive.".to_string(),
            });
        }
        Ok(())
    }

    pub(crate) fn render_hook_idea_or_fail(
        event: &GraphHookEvent,
        ctx: &HookContext<'_>,
        idea: Option<&str>,
    ) -> Result<Option<String>, HookExecution> {
        let Some(idea_template) = idea else {
            return Ok(None);
        };
        match render_hook_idea(event, ctx, idea_template) {
            Ok(rendered) => Ok(Some(rendered)),
            Err(e) => Err(HookExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({ "error": e.to_string() }),
                summary: format!("Graph hook idea template is invalid: {e}"),
            }),
        }
    }

    pub(crate) fn launch_hook_target(&self, launch: GraphHookLaunch<'_>) -> HookExecution {
        match self.launch_graph_from_hook(
            launch.target_id.clone(),
            launch.queue_id.map(str::to_string),
            launch.workdir_override,
            launch.rendered_idea.clone(),
        ) {
            Ok(()) => {
                let _ = self.db.record_hook_launch_provenance(
                    &launch.target_id,
                    &launch.lp.id,
                    launch.event.as_str(),
                );
                HookExecution {
                    status: GraphRunStatus::Pass,
                    output: serde_json::json!({
                        "launched_graph_id": launch.target_id,
                        "queue_id": launch.queue_id,
                        "idea": launch.rendered_idea,
                    }),
                    summary: format!(
                        "Graph hook launched '{}' in background.",
                        launch.target_name
                    ),
                }
            }
            Err(message) => HookExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "error": message,
                    "target_graph_id": launch.target_id,
                    "queue_id": launch.queue_id,
                    "idea": launch.rendered_idea,
                }),
                summary: message,
            },
        }
    }

    /// Launch a graph from a hook context (CH4). Fire-and-forget: spawns a
    /// background task that calls [`Self::run_graph`]. The launched graph is
    /// marked as hook-launched so its own graph hooks are refused (depth
    /// cap = 1).
    pub(crate) fn launch_graph_from_hook(
        &self,
        graph_id: String,
        queue_id: Option<String>,
        workdir_override: Option<String>,
        idea: Option<String>,
    ) -> Result<(), String> {
        // CB41: refuse here with the engine's own pre-claim gate — the same check
        // `run_graph_dispatch` runs before it claims the graph
        // (see `empty_launch_check`, ~graph_engine.rs:408). The in-process launch
        // would return `EmptySpecSetError(message)` for exactly this `message`;
        // propagating it is the whole point. The hook reports whether the launch
        // was accepted, never whether the launched graph's work succeeds.
        match self.empty_launch_check(&graph_id, queue_id.as_deref(), idea.as_deref()) {
            Ok(Some(message)) => return Err(message),
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        // Mark this graph as hook-launched before spawning, so when
        // run_graph_dispatch begins, it knows to enforce the depth cap.
        let _ = self.db.mark_graph_as_hook_launched(&graph_id);

        let db = Arc::clone(&self.db);
        let notification_service = Arc::clone(&self.notification_service);
        let ensemble_concurrency = Arc::clone(&self.ensemble_concurrency);
        let dynamic_skills = self.dynamic_skills.clone();
        let spec_attempt_limit = self.spec_attempt_limit;
        let shutdown = Arc::clone(&self.shutdown);

        tokio::spawn(async move {
            let engine = GraphEngine {
                db,
                notification_service,
                ensemble_concurrency,
                dynamic_skills,
                spec_attempt_limit,
                shutdown,
            };
            if let Err(error) = engine
                .run_graph(
                    graph_id.clone(),
                    queue_id,
                    workdir_override,
                    idea,
                    None,
                    None,
                )
                .await
            {
                if error.downcast_ref::<EmptySpecSetError>().is_some() {
                    tracing::error!(
                        "Hook-launched graph '{}' launch refused: {error:#}",
                        graph_id
                    );
                } else {
                    tracing::error!("Hook-launched graph '{}' failed: {error:#}", graph_id);
                    let _ = engine
                        .fail_graph(&graph_id, None, None, None, &error.to_string())
                        .await;
                }
            }
        });
        Ok(())
    }
}
