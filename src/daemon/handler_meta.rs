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
use rmcp::model::{CallToolResult, ContentBlock};
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
        Scopes: session_start (what to do first), file_write (read before modifying), \
        test_run (run and report), close_session (wrap up + summary), \
        multi_agent (session coordination). Call this before acting — it tells you exactly what to use."
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

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }

    /// Search registered projects by name or description.
    pub(crate) async fn project_search_impl(
        &self,
        params: ProjectSearchParams,
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

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }

    /// Update project metadata (description, tags).
    pub(crate) async fn project_update_impl(
        &self,
        params: ProjectUpdateParams,
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
    pub(crate) async fn project_remap_impl(
        &self,
        params: ProjectRemapParams,
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
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&out).unwrap_or_default(),
                )]))
            }
            Err(e) => Ok(error_result(&e.to_string())),
        }
    }

    /// Explicitly register a project directory (no marker file required).
    pub(crate) async fn project_register_impl(
        &self,
        params: ProjectRegisterParams,
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
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&out).unwrap_or_default(),
                )]))
            }
            Err(e) => Ok(error_result(&e.to_string())),
        }
    }

    /// Full-text search over personal RAG chunks (LanceDB vector search). Rate-limited: 10/min per agent.
    pub(crate) async fn rag_search_impl(
        &self,
        params: RagSearchParams,
        parts: Option<&Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts)?;
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

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }

    /// Compact the RAG vector store now: merge fragments and prune old
    /// versions. Runs on the daemon's own store handle (never a second,
    /// independent connection) and refuses politely while another
    /// maintenance pass is in flight.
    pub(crate) async fn rag_compact_impl(&self) -> Result<CallToolResult, McpError> {
        let report = self.ingestion.compact_now_report().await;
        Ok(CallToolResult::success(vec![ContentBlock::text(report)]))
    }

    pub(crate) async fn subagent_spawn_impl(
        &self,
        params: SubagentSpawnParams,
        parts: Option<&Parts>,
    ) -> Result<CallToolResult, McpError> {
        // Block spawning mid seed-creation interview — but only for a synced
        // agent caller. The `canopy subagent` CLI reaches this tool through
        // the daemon with no sync identity, and `reject_if_nursery` would
        // otherwise hard-fail it on identity resolution alone.
        if self.resolve_sync_agent_id(parts).is_ok() {
            self.reject_if_nursery(parts)?;
        }

        let platform = params.cli.as_deref().unwrap_or("opencode");
        let workdir = params
            .workdir
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                self.resolve_sync_agent_id(parts)
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
                    // Identical shape to `subagent` `collect` (guideline), plus
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
                    Ok(CallToolResult::success(vec![ContentBlock::text(
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
                    Ok(CallToolResult::success(vec![ContentBlock::text(
                        serde_json::to_string_pretty(&result).unwrap_or_default(),
                    )]))
                }
                Err(e) => Ok(error_result(&e.to_string())),
            }
        }
    }

    pub(crate) async fn subagent_collect_impl(
        &self,
        params: SubagentCollectParams,
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
                Ok(CallToolResult::success(vec![ContentBlock::text(
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

/// The 32-tool map every `get_tools` scope serves: one line per tool,
/// each naming the tool, its actions, and what it is for. Alphabetical by
/// tool name, matching the canonical listing. The sync protocol is retired,
/// so every scope carries the same map — the scopes differ only in their
/// protocol steps.
fn tool_map_entries() -> Vec<&'static str> {
    vec![
        "agent — list, get, logs, status; read background agents, logs, and daemon health",
        "agent_edit — add, watch, update, remove, enable, disable, schedule, run; manage background agents",
        "agent_report — report a run's execution status (the only way to report a run)",
        "config — get, list, set, reset; read and change canopy settings",
        "crew — list, get, create, update, delete, assignments; manage crews and read harness/model assignments",
        "get_tools — this map: the right tools and protocol per scope",
        "graph — list, get, export, validate, audit, events, node_runs, node_run; read graphs, specs, runs, events",
        "graph_complete_node — complete an active graph node",
        "graph_control — run, pause, continue, reset, preflight, schedule_continue, schedule_autorun; run and manage graph execution",
        "graph_edge — add, update, delete; wire graph nodes",
        "graph_edit — create, update, archive, restore, import, install, attach_spec, detach_spec, update_spec; author graphs and specs",
        "graph_ensemble — add, update, delete, copy; author multi-member ensembles",
        "graph_node — add, update, delete, copy; author graph nodes",
        "graph_report_blocker — report a blocker for an active graph node",
        "harness — models, probe, probe_recent, refresh_registry, roster_list, roster_set, roster_clear, roster_check; inspect catalogs, probe liveness, manage the roster",
        "identity — get, evolve, list_seeds, create_seed, remove_seed; read and evolve the agent identity, manage seed identities",
        "intelligence_admin — delete_node, delete_relation, walk, list_projects, link_projects; prune the knowledge graph and link projects",
        "intelligence_get_context — curated project context for session start",
        "intelligence_search — full-text search across intelligence nodes",
        "intelligence_upsert — persist a fact, pattern, or session summary",
        "node — list, get, install, remove; manage shareable node documents",
        "project — register, remap, search, update; manage the project registry",
        "queue — create, list, add_spec, remove_spec, reorder, set_group; author ordered spec queues",
        "rag — search, compact; semantic search over indexed content and vector-store maintenance",
        "sandbox — list, get, land, discard; deal with the worktrees sandboxed graph runs leave behind",
        "scheduled_send — list, cancel; list and cancel pending scheduled sends",
        "scheduled_send_create — schedule a prompt for future delivery to a session",
        "session_list — list live interactive sessions",
        "skill — list, get; discover and fetch skills",
        "spec — list, get, section, notes; read specs, sections, and notes",
        "spec_edit — create, update, delete, set_status, note_write; author and transition specs",
        "subagent — spawn, collect; launch ephemeral subagents and collect their results",
    ]
}

pub(crate) fn build_get_tools_response(scope: &str) -> serde_json::Value {
    let mut out = match scope {
        "session_start" => serde_json::json!({
            "scope": "session_start",
            "risk": "low",
            "protocol": [
                "1. Call intelligence_get_context(scope=\"light\") to load workspace brief.",
                "2. If this workdir is new here, call project register to register it.",
                "3. Use intelligence_search to find prior art before starting.",
                "4. Respond to the user with context in hand."
            ],
        }),
        "file_write" => serde_json::json!({
            "scope": "file_write",
            "risk": "high",
            "protocol": [
                "1. Read the file (or the surrounding code) before modifying it.",
                "2. Modify the file.",
                "3. Run the smallest check that covers the change."
            ],
        }),
        "test_run" => serde_json::json!({
            "scope": "test_run",
            "risk": "medium",
            "protocol": [
                "1. Note which suite or command you are running.",
                "2. Run the tests.",
                "3. On failure, fix or report the failure before moving on."
            ],
        }),
        "close_session" => serde_json::json!({
            "scope": "close_session",
            "risk": "low",
            "protocol": [
                "1. Call intelligence_upsert(kind=\"fact\", title=\"Session summary: <mission>\", body=\"<what was done>\", \
                   metadata={\"type\": \"session_summary\", workdir, summary, ...}) — store the session summary in project context.",
                "2. Do NOT manually call any close/shutdown tool — daemon handles it."
            ],
        }),
        "multi_agent" => serde_json::json!({
            "scope": "multi_agent",
            "risk": "varies",
            "protocol": [
                "1. Call session_list to see every live session before addressing one.",
                "2. Use scheduled_send_create to wake another session (or your future self) instead of polling.",
                "3. Persist shared findings with intelligence_upsert so other agents find them via intelligence_search."
            ],
        }),
        _ => unreachable!(),
    };
    if let Some(obj) = out.as_object_mut() {
        obj.insert(
            "tools".to_string(),
            serde_json::Value::Array(
                tool_map_entries()
                    .into_iter()
                    .map(|entry| serde_json::Value::String(entry.to_string()))
                    .collect(),
            ),
        );
    }
    out
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
