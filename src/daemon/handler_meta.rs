//! MCP handler tools split out of `daemon::handler` (meta tools (get_tools, projects, RAG, subagents)).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::application::ports::StateRepository;
use crate::daemon::handler::{OptionalExtension, TaskTriggerHandler};
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_helpers::resolve_effective_project_hash;
use crate::daemon::handler_validate::validate_non_empty;
use crate::daemon::helpers::{data_dir, error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use axum::http::request::Parts;
use rmcp::model::{CallToolResult, Content};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

#[tool_router(router = tool_router_meta, vis = "pub(crate)")]
impl TaskTriggerHandler {
    /// Returns the recommended tools and step-by-step protocol for a given action scope.
    /// Use this at session start, before file writes, before test runs, and on session close.
    #[tool(
        name = "get_tools",
        description = "Return the right tools and action protocol for the requested scope. \
        Scopes: session_start (what to do first), file_write (conflict check before modifying), \
        test_run (broadcast before/after running), close_session (wrap up + summary), \
        multi_agent (full sync toolkit). Call this before acting — it tells you exactly what to use."
    )]
    pub(crate) async fn get_tools(
        &self,
        Parameters(params): Parameters<GetToolsParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        let scope = params.scope.trim().to_lowercase();
        if !matches!(
            scope.as_str(),
            "session_start" | "file_write" | "test_run" | "close_session" | "multi_agent"
        ) {
            return Ok(error_result(
                "Invalid scope. Must be one of: session_start, file_write, test_run, close_session, multi_agent.",
            ));
        }

        if self.resolve_sync_agent_id(parts.as_ref()).is_ok() {
            self.reject_if_nursery(parts.as_ref())?;
        }

        let mut out = if scope == "file_write" {
            let mut result = build_get_tools_response(&scope);
            if let Some(obj) = result.as_object_mut() {
                obj.insert(
                    "path".to_string(),
                    serde_json::json!(params.path.as_deref().unwrap_or("(not specified)")),
                );
            }
            result
        } else {
            build_get_tools_response(&scope)
        };

        if scope == "session_start" {
            if let Some(obj) = out.as_object_mut() {
                if let Ok(session_id) = self.resolve_sync_agent_id(parts.as_ref()) {
                    obj.insert("session_id".to_string(), serde_json::json!(session_id));
                    obj.insert(
                        "session_hint".to_string(),
                        serde_json::json!(format!(
                            "Your interactive session id is '{}'. It is the exact string an interactive graph hook's 'target_session_id' accepts. Call session_list to see every live session.",
                            session_id
                        )),
                    );
                }
            }
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }

    /// Search registered projects by name or description.
    #[tool(
        name = "project_search",
        description = "Search projects by name/description, returns workdir_hash and metadata."
    )]
    pub(crate) async fn project_search(
        &self,
        Parameters(params): Parameters<ProjectSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let projects = self
            .db
            .search_projects(&params.query)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if projects.is_empty() {
            return Ok(success_result("No projects found matching the query."));
        }

        let out: Vec<serde_json::Value> = projects
            .iter()
            .map(|p| {
                serde_json::json!({
                    "workdir_hash": p.hash,
                    "name": p.name,
                    "path": p.path,
                    "description": p.description,
                    "tags": p.tags,
                })
            })
            .collect();

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }

    /// Update project metadata (description, tags).
    #[tool(
        name = "project_update",
        description = "Update project metadata. Agents can enrich description and tags discovered during a session."
    )]
    pub(crate) async fn project_update(
        &self,
        Parameters(params): Parameters<ProjectUpdateParams>,
    ) -> Result<CallToolResult, McpError> {
        let updated = self
            .db
            .update_project_meta(
                &params.project_hash,
                params.description.as_deref(),
                params.tags.as_deref(),
            )
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if updated {
            // The Gardener mission counts distinct projects whose description
            // or tags changed — same one-shot flag pattern as identity_evolved.
            if params.description.is_some() || params.tags.is_some() {
                let _ = self.db.record_gardener_project(&params.project_hash);
            }
            Ok(success_result(&format!(
                "Project '{}' metadata updated.",
                params.project_hash
            )))
        } else {
            Ok(error_result(&format!(
                "Project '{}' not found.",
                params.project_hash
            )))
        }
    }

    /// Point a project at a new workdir after its directory was renamed or
    /// moved, keeping its sessions and history instead of orphaning them.
    #[tool(
        name = "project_remap",
        description = "Remap a project whose directory was renamed or moved to its new path, \
        re-keying its sessions/graphs/history instead of losing them. MOVE if nothing is \
        registered at the new path, MERGE (folding into it, removing the stale row) if a \
        project already exists there. Refuses a new_path that doesn't exist unless force=true. \
        Set dry_run=true to preview without changing anything."
    )]
    pub(crate) async fn project_remap(
        &self,
        Parameters(params): Parameters<ProjectRemapParams>,
    ) -> Result<CallToolResult, McpError> {
        let new_path = std::path::Path::new(&params.new_path);
        let resolved =
            match crate::db::project::resolve_remap_path(new_path, params.force.unwrap_or(false)) {
                Ok(resolved) => resolved,
                Err(e) => return Ok(error_result(&e.to_string())),
            };

        let outcome = if params.dry_run.unwrap_or(false) {
            self.db.remap_preview(&params.project_hash, &resolved)
        } else {
            self.db.remap_project(&params.project_hash, &resolved)
        };

        match outcome {
            Ok(outcome) => {
                let kind = match outcome.kind {
                    crate::domain::project::RemapKind::Move => "move",
                    crate::domain::project::RemapKind::Merge => "merge",
                };
                let out = serde_json::json!({
                    "kind": kind,
                    "dry_run": params.dry_run.unwrap_or(false),
                    "old_hash": outcome.old_hash,
                    "new_hash": outcome.new_hash,
                    "new_path": outcome.new_path,
                    "rows_moved": outcome.counts.total(),
                    "counts": {
                        "interactive_sessions": outcome.counts.interactive_sessions,
                        "terminal_sessions": outcome.counts.terminal_sessions,
                        "graphs": outcome.counts.graphs,
                        "graph_specs": outcome.counts.graph_specs,
                        "sync_messages": outcome.counts.sync_messages,
                        "sync_locks": outcome.counts.sync_locks,
                        "last_prompts": outcome.counts.last_prompts,
                        "scheduled_sends": outcome.counts.scheduled_sends,
                        "failed_scheduled_sends": outcome.counts.failed_scheduled_sends,
                        "agents": outcome.counts.agents,
                        "intelligence_nodes": outcome.counts.intelligence_nodes,
                    },
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&out).unwrap_or_default(),
                )]))
            }
            Err(e) => Ok(error_result(&e.to_string())),
        }
    }

    /// Explicitly register a project directory (no marker file required).
    #[tool(
        name = "project_register",
        description = "Explicitly register a project directory in the registry. \
        Unlike automatic registration (which requires a `.canopy-project` marker \
        file in the directory), this call registers any existing directory."
    )]
    pub(crate) async fn project_register(
        &self,
        Parameters(params): Parameters<ProjectRegisterParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(e) = validate_non_empty(&params.path, "path") {
            return Ok(error_result(&e));
        }
        let raw = std::path::Path::new(&params.path);
        let canonical = match std::fs::canonicalize(raw) {
            Ok(canonical) => canonical,
            Err(e) => {
                return Ok(error_result(&format!(
                    "Path '{}' does not exist ({e})",
                    params.path
                )))
            }
        };
        match self.db.register_project_explicit(&canonical) {
            Ok(project) => {
                let out = serde_json::json!({
                    "workdir_hash": project.hash,
                    "name": project.name,
                    "path": project.path,
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&out).unwrap_or_default(),
                )]))
            }
            Err(e) => Ok(error_result(&e.to_string())),
        }
    }

    /// Full-text search over personal RAG chunks (LanceDB vector search). Rate-limited: 10/min per agent.
    #[tool(
        name = "rag_search",
        description = "Search indexed personal content (markdown and PDF files) using semantic \
        vector search. Default limit: 5. Rate-limited to 10 calls/min per agent. \
        Usage examples: 'search RAG for X', 'find documents about Y', \
        'consult RAG about Z', 'what does the RAG say about W'."
    )]
    pub(crate) async fn rag_search(
        &self,
        Parameters(params): Parameters<RagSearchParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;
        if let Some(result) = self.check_rag_rate_limit(&params).await {
            return Ok(result);
        }

        let limit = params.limit.unwrap_or(5).min(20);
        let data_dir_path = data_dir().map_err(internal_error)?;
        let config = crate::domain::canopy_config::CanopyConfig::load(&data_dir_path);

        let model = config.embeddings_model.trim();
        if model.is_empty() {
            return Ok(success_result("No embeddings model configured."));
        }

        let dimensions = match crate::rag::embedding_client::model_dimensions(model) {
            Ok(d) => d,
            Err(e) => return Ok(error_result(&format!("Unknown model dimensions: {e}"))),
        };

        // B22: go through the shared ingestion cache instead of building a
        // throwaway client — queries reuse the already-loaded model (no
        // reload per search) and the persisted model-loaded status flips to
        // "ready" for the CLI/TUI, exactly like an indexing pass does.
        let embedding_client = match self.ingestion.get_embedding_client(&config).await {
            Ok(client) => client,
            Err(e) => return Ok(error_result(&format!("Embedding client error: {e}"))),
        };

        let query = params.query.clone();
        let query_vec = tokio::task::spawn_blocking(move || embedding_client.embed(&query))
            .await
            .map_err(|e| internal_error(e.to_string()))?
            .map_err(|e| internal_error(e.to_string()))?;

        let store = crate::rag::vector_store::VectorStore::new(
            dimensions,
            Some(config.rag_vector_cache_entries),
        )
        .await
        .map_err(|e| internal_error(e.to_string()))?;

        let results = store
            .search_similar(&query_vec, limit)
            .await
            .map_err(|e| internal_error(e.to_string()))?;

        if results.is_empty() {
            return Ok(success_result("No results found."));
        }

        self.record_rag_search_missions(&results);

        let out: Vec<serde_json::Value> = results.iter().map(rag_result_json).collect();

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }

    /// Compact the RAG vector store now: merge fragments and prune old
    /// versions. Runs on the daemon's own store handle (never a second,
    /// independent connection) and refuses politely while another
    /// maintenance pass is in flight.
    #[tool(
        name = "rag_compact",
        description = "Compact the RAG vector store and prune old versions"
    )]
    pub(crate) async fn rag_compact(&self) -> Result<CallToolResult, McpError> {
        let report = self.ingestion.compact_now_report().await;
        Ok(CallToolResult::success(vec![Content::text(report)]))
    }

    #[tool(
        name = "subagent_spawn",
        description = "Launch an ephemeral subagent. By default returns an ID \
         immediately (async); poll subagent_collect for the result. Pass \
         `blocking: true` to wait for the subagent to finish and return its \
         result directly — the shape is identical to what subagent_collect \
         would return, and no collect call is needed. Blocking respects \
         `timeout_minutes` and returns a timeout as a result, not as an error. \
         No agent, schedule, or permanent state is registered."
    )]
    pub(crate) async fn subagent_spawn(
        &self,
        Parameters(params): Parameters<SubagentSpawnParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        // Block spawning mid seed-creation interview — but only for a synced
        // agent caller. The `canopy subagent` CLI reaches this tool through
        // the daemon with no sync identity, and `reject_if_nursery` would
        // otherwise hard-fail it on identity resolution alone.
        if self.resolve_sync_agent_id(parts.as_ref()).is_ok() {
            self.reject_if_nursery(parts.as_ref())?;
        }

        let platform = params.cli.as_deref().unwrap_or("opencode");
        let workdir = params
            .workdir
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                self.resolve_sync_agent_id(parts.as_ref())
                    .ok()
                    .and_then(|agent_id| resolve_effective_project_hash(&self.db, None, &agent_id))
            })
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .and_then(|p| p.to_str().map(String::from))
            })
            .unwrap_or_else(|| "/tmp".to_string());

        let mcp_servers = params.mcp_servers.unwrap_or_default();
        let timeout_minutes = params.timeout_minutes.unwrap_or(15);
        let ttl_minutes = params.ttl_minutes.unwrap_or(60);
        let blocking = params.blocking.unwrap_or(false);

        let db = Arc::clone(&self.db);
        let prompt = params.prompt.clone();
        let model = params.model.clone();
        let effort = params.effort.clone();
        let workdir_clone = workdir.clone();

        // CM18: blocking stays opt-in; absent/false is today's async path,
        // unchanged. The blocking wait holds no DB guard across its await
        // (see `spawn_subagent_blocking`), so other MCP work progresses.
        if blocking {
            match crate::daemon::subagent::spawn_subagent_blocking(
                &db,
                platform,
                &prompt,
                model.as_deref(),
                &workdir_clone,
                &mcp_servers,
                timeout_minutes,
                ttl_minutes,
                effort.as_deref(),
            )
            .await
            {
                Ok(blocking_result) => {
                    // Identical shape to `subagent_collect` (guideline), plus
                    // the wait accounting FR3 requires on timeouts.
                    let result = blocking_result.result;
                    let mut json = serde_json::json!({
                        "id": result.id,
                        "status": result.status,
                        "exit_code": result.exit_code,
                        "stdout": result.stdout,
                        "stderr": result.stderr,
                        "mcp_surface": result.mcp_surface,
                        "platform": result.platform,
                        "model": result.model,
                        "waited_secs": blocking_result.waited_secs,
                        "waited_minutes": timeout_minutes,
                        "timed_out": blocking_result.timed_out,
                    });
                    if let Some(reason) = blocking_result.effort_not_applied {
                        json["effort_not_applied"] = serde_json::json!(reason);
                    }
                    Ok(CallToolResult::success(vec![Content::text(
                        serde_json::to_string_pretty(&json).unwrap_or_default(),
                    )]))
                }
                Err(e) => Ok(error_result(&e.to_string())),
            }
        } else {
            match crate::daemon::subagent::spawn_subagent(
                &db,
                platform,
                &prompt,
                model.as_deref(),
                &workdir_clone,
                &mcp_servers,
                timeout_minutes,
                ttl_minutes,
                effort.as_deref(),
            )
            .await
            {
                Ok((id, effort_not_applied)) => {
                    let mut result = serde_json::json!({
                        "id": id,
                        "status": "running",
                        "handle": id,
                    });
                    if let Some(reason) = effort_not_applied {
                        result["effort_not_applied"] = serde_json::json!(reason);
                    }
                    Ok(CallToolResult::success(vec![Content::text(
                        serde_json::to_string_pretty(&result).unwrap_or_default(),
                    )]))
                }
                Err(e) => Ok(error_result(&e.to_string())),
            }
        }
    }

    #[tool(
        name = "subagent_collect",
        description = "Collect the result of an ephemeral subagent. \
         Returns the result and marks it for cleanup. Returns status='running' \
         if the subagent hasn't finished yet."
    )]
    pub(crate) async fn subagent_collect(
        &self,
        Parameters(params): Parameters<SubagentCollectParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = Arc::clone(&self.db);
        match crate::daemon::subagent::collect_subagent(&db, &params.id) {
            Ok(Some(result)) => {
                let json = serde_json::json!({
                    "id": result.id,
                    "status": result.status,
                    "exit_code": result.exit_code,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "mcp_surface": result.mcp_surface,
                    "platform": result.platform,
                    "model": result.model,
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&json).unwrap_or_default(),
                )]))
            }
            Ok(None) => {
                // CM18: a blocking spawn delivers its result inline and
                // deletes the row, leaving a tombstone — report that it was
                // already delivered rather than that it is missing (FR4).
                if db.is_delivered_tombstone(&params.id).unwrap_or(false) {
                    return Ok(error_result(
                        &crate::daemon::subagent::already_delivered_message(&params.id),
                    ));
                }
                Ok(error_result(&format!(
                    "Subagent run '{}' not found",
                    params.id
                )))
            }
            Err(e) => Ok(error_result(&e.to_string())),
        }
    }
}

impl TaskTriggerHandler {
    pub(crate) async fn check_rag_rate_limit(
        &self,
        params: &RagSearchParams,
    ) -> Option<CallToolResult> {
        let limiter_key = params
            .agent_id
            .clone()
            .unwrap_or_else(|| "global".to_owned());
        let mut limiters = self.rag_limiters.lock().await;
        let limiter = limiters
            .entry(limiter_key)
            .or_insert_with(|| crate::rag::rate_limiter::RateLimiter::new(10));

        limiter
            .check()
            .err()
            .map(|retry_after| error_result(&format!("rate_limited: retry_after={retry_after}s")))
    }

    /// Flag one-shot RAG missions for the TUI to pick up via shared state,
    /// mirroring the `gamification:identity_evolved` mechanism.
    pub(crate) fn record_rag_search_missions(
        &self,
        results: &[crate::rag::vector_store::SearchResult],
    ) {
        if results.iter().any(|r| r.distance.is_some_and(|d| d < 0.2)) {
            let _ = self.db.set_state("gamification:deep_rag_search", "1");
        }

        let month_ago = chrono::Utc::now().timestamp() - 30 * 24 * 3600;
        if results.iter().any(|r| r.created_at < month_ago) {
            let _ = self.db.set_state("gamification:digital_archeologist", "1");
        }
    }
}

pub(crate) fn build_get_tools_response(scope: &str) -> serde_json::Value {
    match scope {
        "session_start" => serde_json::json!({
            "scope": "session_start",
            "risk": "low",
            "protocol": [
                "1. Call intelligence_get_context(scope=\"light\") to load workspace brief.",
                "2. Check active_missions in sync context — align your work with open missions.",
                "3. If starting a new thread of work, call sync_declare_intent to register your mission.",
                "4. Respond to the user with context in hand."
            ],
            "tools": [
                "intelligence_get_context — pull session history, facts, patterns",
                "sync_get_context — check active missions and workspace vibe",
                "sync_declare_intent — announce your mission (impact: low/high/breaking)"
            ]
        }),
        "file_write" => serde_json::json!({
            "scope": "file_write",
            "risk": "high",
            "protocol": [
                "1. Call sync_get_context(workdir=...) — check for conflicting missions on this path.",
                "2. If no conflict, call sync_declare_intent(impact=\"high\", mission=\"...\").",
                "3. Modify the file.",
                "4. Call sync_report_status(status=\"stable\", message=\"Changes complete: ...\")."
            ],
            "tools": [
                "sync_get_context — check active missions, detect conflicts",
                "sync_declare_intent — announce what you're changing and why",
                "sync_report_status — report stable/unstable/testing after the change"
            ]
        }),
        "test_run" => serde_json::json!({
            "scope": "test_run",
            "risk": "medium",
            "protocol": [
                "1. Call sync_broadcast(kind=\"info\", message=\"Running tests: <suite/command>\").",
                "2. Run the tests.",
                "3. Call sync_broadcast(kind=\"info\", message=\"Tests complete: PASSED/FAILED — <summary>\").",
                "4. If failed, call sync_report_status(status=\"unstable\", message=\"Test failure: ...\")."
            ],
            "tools": [
                "sync_broadcast — announce test start and result to peer agents",
                "sync_report_status — mark workspace unstable if tests fail"
            ]
        }),
        "close_session" => serde_json::json!({
            "scope": "close_session",
            "risk": "low",
            "protocol": [
                "1. Call intelligence_upsert(kind=\"fact\", title=\"Session summary: <mission>\", body=\"<what was done>\", \
                   metadata={\"type\": \"session_summary\", workdir, summary, ...}) — store the session summary in project context.",
                "2. Call sync_report_status(status=\"stable\", message=\"Mission complete: <summary>\") \
                   — the daemon closes your mission automatically on exit.",
                "3. Do NOT manually call any close/shutdown tool — daemon handles it."
            ],
            "tools": [
                "intelligence_upsert — persist the session summary as a 'fact' node with metadata.type = \"session_summary\"",
                "sync_report_status — leave a clean 'stable' marker for the next agent"
            ]
        }),
        "multi_agent" => serde_json::json!({
            "scope": "multi_agent",
            "risk": "varies",
            "protocol": [
                "Follow the action-risk table: low=execute and broadcast if notable, high=declare+execute+report, breaking=same as high with impact=breaking.",
                "Always non-blocking — act on last-known state, never wait for responses.",
                "Communicate intent not implementation — missions explain what and why, not how."
            ],
            "tools": [
                "sync_get_context — check active missions and workspace vibe (call first)",
                "sync_declare_intent — announce mission (impact: low/high/breaking)",
                "sync_broadcast — send info/query/answer messages to peers",
                "sync_report_status — report stable/unstable/testing after actions",
                "intelligence_get_context(scope=\"full\") — deep project-context pull for architecture work",
                "intelligence_upsert — persist facts, patterns, session summaries",
                "intelligence_search — find prior art or decisions in project context"
            ]
        }),
        _ => unreachable!(),
    }
}

pub(crate) fn rag_result_json(r: &crate::rag::vector_store::SearchResult) -> serde_json::Value {
    serde_json::json!({
        "source": r.file_path,
        "content": r.content,
        "distance": r.distance,
    })
}

#[cfg(test)]
mod tests {
    use super::build_get_tools_response;

    /// Every `intelligence_upsert(kind="…")` named in any get_tools protocol
    /// string must be accepted by the same validation `intelligence_upsert`
    /// runs, so the protocol text and the intelligence schema can never drift
    /// apart again. Kinds are pulled from the serialized protocol JSON by regex.
    #[test]
    fn every_protocol_intelligence_kind_is_accepted_by_upsert_validation() {
        let scopes = [
            "session_start",
            "file_write",
            "test_run",
            "close_session",
            "multi_agent",
        ];
        // `serde_json::to_string` escapes the protocol string's `"` into `\"`,
        // so the text is `intelligence_upsert(kind=\"fact\"`; match the backslash
        // before each quote. Anchored on `intelligence_upsert(kind=` so a
        // `sync_broadcast(kind="info")` is never captured.
        let re = regex::Regex::new(r#"intelligence_upsert\(kind=\\"([a-z]+)"#)
            .expect("static regex is valid");
        let mut kinds: Vec<String> = Vec::new();
        for scope in scopes {
            let json = build_get_tools_response(scope);
            let serialized = serde_json::to_string(&json).expect("protocol serializes to JSON");
            for cap in re.captures_iter(&serialized) {
                kinds.push(cap[1].to_string());
            }
        }
        assert!(
            !kinds.is_empty(),
            "no intelligence_upsert(kind=\"…\") found in any get_tools protocol — \
             the drift regex matched nothing; check the regex escaping"
        );
        for kind in &kinds {
            assert!(
                crate::db::intelligence::validate_knowledge_kind(kind).is_ok(),
                "get_tools protocol names intelligence kind {kind:?}, which \
                 intelligence_upsert rejects — fix the protocol string (CB89)"
            );
        }
    }
}
