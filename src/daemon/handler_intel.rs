//! MCP handler tools split out of `daemon::handler` (intelligence tools and JSON builders).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::{
    OptionalExtension, TaskTriggerHandler, INTELLIGENCE_DUPLICATE_CANDIDATES_CAP,
};
use crate::daemon::handler_helpers::resolve_effective_project_hash;
use crate::daemon::handler_seeds_skills::inject_seed_identity;
use crate::daemon::handler_sync::inject_project_context;
use crate::daemon::handler_validate::validate_non_empty;
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use axum::http::request::Parts;
use rmcp::model::{CallToolResult, Content};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_intel, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "intelligence_get_context",
        description = "Return project context at the requested scope: light or full. \
         When project_hash is provided (or auto-detected from session workdir), \
         returns project-scoped facts and patterns instead of generic knowledge."
    )]
    pub(crate) async fn intelligence_get_context(
        &self,
        Parameters(params): Parameters<IntelligenceGetContextParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;
        let scope = params.scope.trim().to_lowercase();
        let (session_limit, _knowledge_limit, sync_limit, dependency_limit) = match scope.as_str() {
            "light" => (2, 5, 8, 0),
            "full" => (10, 20, 25, 30),
            _ => return Ok(error_result("Invalid scope. Must be: light or full.")),
        };

        // Auto-detect project_hash from session workdir if not provided.
        let resolved_agent_id = self.resolve_sync_agent_id(parts.as_ref()).ok();
        let effective_project_hash = resolved_agent_id.as_deref().and_then(|agent_id| {
            resolve_effective_project_hash(&self.db, params.project_hash.as_deref(), agent_id)
        });
        let depth = params
            .depth
            .unwrap_or(1)
            .min(crate::db::intelligence::MAX_TRAVERSAL_DEPTH);

        let sessions: Vec<crate::db::intelligence::IntelligenceNodeRecord> = self
            .db
            .list_operational_sessions(session_limit)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .into_iter()
            .map(|r| r.into_intelligence_node_record())
            .collect();

        let knowledge = self
            .fetch_context_knowledge(effective_project_hash.as_deref(), &scope, depth)
            .map_err(|e| McpError::internal_error(e, None))?;
        let (knowledge, project_knowledge, traversal) =
            (knowledge.generic, knowledge.project, knowledge.traversal);

        let sync_messages = self
            .db
            .list_recent_sync_messages(sync_limit)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        // Bitácora: recent activity for the resolved project (or across all
        // workdirs when no project resolved), readable without the TUI.
        let activity_workdir: Option<String> = effective_project_hash
            .as_deref()
            .and_then(|hash| self.db.get_project(hash).ok().flatten().map(|p| p.path));
        let activity_entries: Vec<crate::db::activity_log::ActivityLogEntry> =
            match activity_workdir {
                Some(ref workdir) => self
                    .db
                    .list_activity_log_entries(workdir, sync_limit)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?,
                None => self
                    .db
                    .list_recent_activity_log_entries(sync_limit)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?,
            };
        let cross_project_dependencies = if scope == "full" {
            self.db
                .list_cross_project_dependencies(dependency_limit)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
        } else {
            Vec::new()
        };

        let related_projects = if scope == "full" {
            if let Some(ref ph) = effective_project_hash {
                self.db
                    .list_related_projects(ph, 10)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // Impact warning (full scope only — keeps light cheap): which projects
        // declare a dependency on the targeted project. Inbound dependents
        // never pull context; they only warn.
        let dependents = if scope == "full" {
            if let Some(ref ph) = effective_project_hash {
                self.db
                    .list_project_dependents(ph)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let summary_prefix = if scope == "light" {
            "Light context"
        } else {
            "Full context"
        };

        let mut out = serde_json::json!({
            "scope": scope,
            "summary": format!(
                "{}: {} session node(s), {} knowledge node(s), {} sync message(s).",
                summary_prefix,
                sessions.len(),
                knowledge.len(),
                sync_messages.len()
            ),
            "sessions": sessions.iter().map(intelligence_node_json).collect::<Vec<_>>(),
            "knowledge": knowledge.iter().map(intelligence_node_json).collect::<Vec<_>>(),
            "sync_messages": sync_messages.iter().map(sync_message_json).collect::<Vec<_>>(),
            "activity": activity_entries.iter().map(activity_log_entry_json).collect::<Vec<_>>(),
            "cross_project_dependencies": cross_project_dependencies,
        });

        inject_project_context(
            &mut out,
            effective_project_hash.as_deref(),
            &project_knowledge,
            &related_projects,
            &traversal,
            &dependents,
        );
        inject_seed_identity(
            &mut out,
            &self.db,
            parts.as_ref(),
            resolved_agent_id.as_deref(),
        );

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "intelligence_upsert",
        description = "Create or update an intelligence node and optional relations. \
         project_hash is auto-detected from the session workdir if not provided. \
         Relation targets accept the same node IDs or unique ID prefixes as node_data.id."
    )]
    pub(crate) async fn intelligence_upsert(
        &self,
        Parameters(params): Parameters<IntelligenceUpsertParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;
        // Track an explicit id that resolved to nothing: the caller almost
        // certainly mistyped or truncated the id of a node they meant to
        // update, so a bare `created: true` (which also fires for the normal
        // auto-generated-id path) is not a loud enough signal — see CB13.
        let mut unresolved_explicit_id: Option<String> = None;
        let resolved_id = match &params.node_data.id {
            Some(raw_id) if !raw_id.trim().is_empty() => {
                let raw_id = raw_id.trim();
                match self.db.resolve_node_id_by_prefix(raw_id) {
                    Ok(Some(full)) => Some(full),
                    Ok(None) => {
                        unresolved_explicit_id = Some(raw_id.to_string());
                        Some(raw_id.to_string())
                    }
                    Err(e) => return Ok(error_result(&e.to_string())),
                }
            }
            _ => params.node_data.id.clone(),
        };

        // Whether this write will update an existing node. An omitted
        // project_hash leaves the stored value untouched on update, but is
        // still auto-detected from the session workdir on create.
        let updating_existing = resolved_id
            .as_deref()
            .and_then(|id| self.db.get_intelligence_node(id).ok())
            .flatten()
            .is_some();
        let project_hash: Option<Option<String>> = match params.node_data.project_hash {
            Some(explicit) => Some(explicit),
            None if updating_existing => None,
            None => {
                let detected = self
                    .resolve_sync_agent_id(parts.as_ref())
                    .ok()
                    .and_then(|agent_id| self.db.get_session_workdir(&agent_id).ok())
                    .flatten()
                    .map(|workdir| crate::domain::project::workdir_hash(&workdir));
                Some(detected)
            }
        };

        let node = crate::db::intelligence::IntelligenceNodeInput {
            id: resolved_id,
            kind: params.node_data.kind,
            status: params.node_data.status,
            title: params.node_data.title,
            body: params.node_data.body,
            body_replace: params.node_data.body_replace.map(|br| {
                crate::db::intelligence::BodyReplace {
                    fragment: br.fragment,
                    replacement: br.replacement,
                }
            }),
            metadata: params.node_data.metadata,
            project_hash,
            session_id: params.node_data.session_id,
            relations: params.node_data.relations.map(|relations| {
                relations
                    .into_iter()
                    .map(
                        |relation| crate::db::intelligence::IntelligenceRelationInput {
                            to_node_id: relation.to_node_id,
                            relation: relation.relation,
                            weight: relation.weight,
                        },
                    )
                    .collect()
            }),
        };

        let result = self
            .db
            .upsert_intelligence_node(node)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let crate::db::intelligence::UpsertResult {
            record,
            created,
            duplicates: duplicate_candidates,
            undeclared_references,
            changed_fields,
        } = result;

        let mut out = serde_json::json!({
            "node": intelligence_node_json(&record),
            "created": created,
            "changed_fields": changed_fields,
            "duplicate_detection_note":
                crate::db::intelligence::LEXICAL_DUPLICATE_LIMITATION,
        });
        if created {
            if let Some(bad_id) = unresolved_explicit_id {
                out["warning"] = serde_json::json!(format!(
                    "Created a NEW node with id '{}'. No existing node matched that id or \
                     prefix, so nothing was updated. If you meant to update an existing node, \
                     the id you passed is wrong — look it up with intelligence_search first.",
                    bad_id
                ));
            }
        }
        if !duplicate_candidates.is_empty() {
            let total_duplicates = duplicate_candidates.len();
            let visible_duplicates = &duplicate_candidates
                [..total_duplicates.min(INTELLIGENCE_DUPLICATE_CANDIDATES_CAP)];
            out["duplicate_candidates"] = serde_json::json!(visible_duplicates
                .iter()
                .map(intelligence_duplicate_candidate_json)
                .collect::<Vec<_>>());
            out["duplicate_warning"] = serde_json::json!(
                "Potential duplicate(s) found; review duplicate_candidates before recording another node."
            );
            let duplicates_omitted = total_duplicates.saturating_sub(visible_duplicates.len());
            if duplicates_omitted > 0 {
                out["duplicate_candidates_truncated"] = serde_json::json!(true);
                out["duplicate_candidates_omitted"] = serde_json::json!(duplicates_omitted);
            }
        }
        if !undeclared_references.is_empty() {
            out["undeclared_references"] = serde_json::json!(undeclared_references);
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "intelligence_search",
        description = "Search intelligence nodes by free text and optional kind. \
         Pass project_hash to scope the search to that project plus the projects \
         it reaches via outbound project-graph edges (depth, default 1, max 5); \
         otherwise the project is auto-detected from the session workdir. \
         Scoped hits carry via_relation; unscoped search is unchanged."
    )]
    pub(crate) async fn intelligence_search(
        &self,
        Parameters(params): Parameters<IntelligenceSearchParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;
        let limit = params.limit.unwrap_or(10).min(50);
        let depth = params
            .depth
            .unwrap_or(1)
            .min(crate::db::intelligence::MAX_TRAVERSAL_DEPTH);

        // Scoped when an explicit hash is given, else auto-detect (same
        // precedence as the other read tools).
        let resolved_agent_id = self.resolve_sync_agent_id(parts.as_ref()).ok();
        let scoped_hash = resolved_agent_id.as_deref().and_then(|agent_id| {
            resolve_effective_project_hash(&self.db, params.project_hash.as_deref(), agent_id)
        });

        if let Some(ph) = scoped_hash {
            let traversal = self
                .db
                .traverse_project_scope(&ph, depth)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            let hashes: Vec<String> = traversal.projects.iter().map(|p| p.hash.clone()).collect();
            let via_by_hash: std::collections::HashMap<&str, Option<&str>> = traversal
                .projects
                .iter()
                .map(|p| (p.hash.as_str(), p.via_relation.as_deref()))
                .collect();
            let search_result = self
                .db
                .search_intelligence_nodes_scoped(
                    &params.query,
                    params.kind.as_deref(),
                    limit,
                    &hashes,
                )
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

            let results: Vec<serde_json::Value> = search_result
                .results
                .iter()
                .map(|node| {
                    let mut value = intelligence_node_json(node);
                    let via_relation = node
                        .project_hash
                        .as_deref()
                        .and_then(|h| via_by_hash.get(h).copied().flatten());
                    let via_relation = match via_relation {
                        None => serde_json::Value::Null,
                        Some("contains") => serde_json::json!("inherited:contains"),
                        Some(other) => serde_json::json!(other),
                    };
                    if let Some(obj) = value.as_object_mut() {
                        obj.insert("via_relation".to_string(), via_relation);
                    }
                    value
                })
                .collect();
            let out = serde_json::json!({
                "query": params.query,
                "kind": params.kind,
                "project_hash": ph,
                "depth": depth,
                "projects_reached": traversal.projects.len(),
                "traversal": {
                    "depth": depth,
                    "projects_reached": traversal.projects.len(),
                },
                "count": results.len(),
                "examined_count": search_result.examined_count,
                "results": results,
            });
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&out).unwrap_or_default(),
            )]));
        }

        let search_result = self
            .db
            .search_intelligence_nodes(&params.query, params.kind.as_deref(), limit)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let out = serde_json::json!({
            "query": params.query,
            "kind": params.kind,
            "count": search_result.results.len(),
            "examined_count": search_result.examined_count,
            "projects_reached": 1,
            "results": search_result.results.iter().map(intelligence_node_json).collect::<Vec<_>>(),
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "intelligence_graph_walk",
        description = "Walk the project-context graph from a node up to the requested depth. \
         Default is compact mode (nodes omit body/metadata; pass compact: false for full bodies). \
         The root is returned in `root` only, never duplicated in `nodes`."
    )]
    pub(crate) async fn intelligence_graph_walk(
        &self,
        Parameters(params): Parameters<IntelligenceGraphWalkParams>,
    ) -> Result<CallToolResult, McpError> {
        let depth = params.depth.unwrap_or(2).min(8);
        let compact = params.compact.unwrap_or(true);
        let effective_id = match self.db.resolve_node_id_by_prefix(&params.node_id) {
            Ok(Some(full)) => full,
            Ok(None) => {
                return Ok(error_result(&format!(
                    "Intelligence node '{}' not found.",
                    params.node_id
                )))
            }
            Err(e) => return Ok(error_result(&e.to_string())),
        };
        let graph = match self.db.walk_intelligence_graph(&effective_id, depth) {
            Ok(Some(g)) => g,
            Ok(None) => {
                return Ok(error_result(&format!(
                    "Intelligence node '{}' not found.",
                    params.node_id
                )))
            }
            Err(e) => return Err(McpError::internal_error(e.to_string(), None)),
        };

        // The database layer returns whole nodes including the root inside
        // `nodes`; the projection happens here at the MCP boundary, where
        // the budget lives, and the root is returned in `root` only.
        let project_node = |node: &crate::db::intelligence::IntelligenceNodeRecord| {
            if compact {
                intelligence_node_compact_json(node)
            } else {
                intelligence_node_json(node)
            }
        };
        let nodes: Vec<serde_json::Value> = graph
            .nodes
            .iter()
            .filter(|n| n.id != graph.root.id)
            .map(project_node)
            .collect();
        let edges: Vec<serde_json::Value> =
            graph.edges.iter().map(intelligence_edge_json).collect();
        let node_count = nodes.len();
        let edge_count = edges.len();

        let mut out = serde_json::json!({
            "root": project_node(&graph.root),
            "nodes": nodes,
            "edges": edges,
            "node_count": node_count,
            "edge_count": edge_count,
            "compact": compact,
        });
        if compact {
            out["bodies_omitted"] = serde_json::json!(true);
            out["hint"] = serde_json::json!(
                "Node bodies/metadata omitted in compact mode (default); pass {\"compact\": false} for full bodies."
            );
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "intelligence_delete_node",
        description = "Delete an intelligence node and every relation touching it, in one \
         transaction (hard delete — the graph is meant to be able to forget). project_hash is \
         auto-detected from the session workdir like the read tools; pass it explicitly to \
         delete a node belonging to a different project. Returns a clear error, not a silent \
         success, if the node does not exist."
    )]
    pub(crate) async fn intelligence_delete_node(
        &self,
        Parameters(params): Parameters<IntelligenceDeleteNodeParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;
        if let Err(e) = validate_non_empty(&params.node_id, "node_id") {
            return Ok(error_result(&e));
        }

        let effective_project_hash =
            self.effective_project_hash_for_delete(parts.as_ref(), params.project_hash.as_deref());

        let effective_id = match self.db.resolve_node_id_by_prefix(&params.node_id) {
            Ok(Some(full)) => full,
            Ok(None) => {
                return Ok(error_result(&format!(
                    "Intelligence node '{}' not found.",
                    params.node_id
                )))
            }
            Err(e) => return Ok(error_result(&e.to_string())),
        };

        let node = match self.db.get_intelligence_node(&effective_id) {
            Ok(Some(node)) => node,
            Ok(None) => {
                return Ok(error_result(&format!(
                    "Intelligence node '{}' not found.",
                    params.node_id
                )))
            }
            Err(e) => return Err(McpError::internal_error(e.to_string(), None)),
        };

        if let Some(scope_error) =
            self.check_intelligence_delete_scope(&node, effective_project_hash.as_deref())
        {
            return Ok(scope_error);
        }

        let relations_removed = match self.db.delete_intelligence_node(&effective_id) {
            Ok(Some(count)) => count,
            Ok(None) => {
                return Ok(error_result(&format!(
                    "Intelligence node '{}' not found.",
                    params.node_id
                )))
            }
            Err(e) => return Err(McpError::internal_error(e.to_string(), None)),
        };

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "deleted_node_id": effective_id,
                "relations_removed": relations_removed,
            }))
            .unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "intelligence_delete_relation",
        description = "Delete a single relation (edge) by ID, leaving both endpoint nodes \
         intact. project_hash is auto-detected from the session workdir like the read tools; \
         pass it explicitly to delete a relation touching a different project's nodes. Returns \
         a clear error, not a silent success, if the relation does not exist."
    )]
    pub(crate) async fn intelligence_delete_relation(
        &self,
        Parameters(params): Parameters<IntelligenceDeleteRelationParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;

        let effective_project_hash =
            self.effective_project_hash_for_delete(parts.as_ref(), params.project_hash.as_deref());

        let edge = match self.db.get_intelligence_edge(params.edge_id) {
            Ok(Some(edge)) => edge,
            Ok(None) => {
                return Ok(error_result(&format!(
                    "Intelligence relation '{}' not found.",
                    params.edge_id
                )))
            }
            Err(e) => return Err(McpError::internal_error(e.to_string(), None)),
        };

        for node_id in [&edge.from_node_id, &edge.to_node_id] {
            if let Ok(Some(node)) = self.db.get_intelligence_node(node_id) {
                if let Some(scope_error) =
                    self.check_intelligence_delete_scope(&node, effective_project_hash.as_deref())
                {
                    return Ok(scope_error);
                }
            }
        }

        let removed = self
            .db
            .delete_intelligence_edge(params.edge_id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if !removed {
            return Ok(error_result(&format!(
                "Intelligence relation '{}' not found.",
                params.edge_id
            )));
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "deleted_edge_id": params.edge_id,
            }))
            .unwrap_or_default(),
        )]))
    }
}

pub(crate) fn intelligence_node_json(
    node: &crate::db::intelligence::IntelligenceNodeRecord,
) -> serde_json::Value {
    serde_json::json!({
        "id": node.id,
        "kind": node.kind,
        "status": node.status,
        "title": node.title,
        "body": node.body,
        "metadata": node.metadata,
        "project_hash": node.project_hash,
        "session_id": node.session_id,
        "created_at": node.created_at,
        "updated_at": node.updated_at,
    })
}

/// Compact projection of an intelligence node for `intelligence_graph_walk`:
/// `id`, `kind`, `title`, `created_at`, `updated_at` and `project_hash`,
/// omitting `body` and `metadata`. Uses the same field names as the rows
/// `intelligence_search` emits where the fields overlap — no second compact
/// shape for the same entity. The database layer keeps returning whole
/// nodes; this projection happens at the MCP boundary, where the budget
/// lives.
pub(crate) fn intelligence_node_compact_json(
    node: &crate::db::intelligence::IntelligenceNodeRecord,
) -> serde_json::Value {
    serde_json::json!({
        "id": node.id,
        "kind": node.kind,
        "title": node.title,
        "project_hash": node.project_hash,
        "created_at": node.created_at,
        "updated_at": node.updated_at,
    })
}

/// JSON for one `intelligence_upsert` duplicate candidate — id, kind,
/// title, project_hash and a short excerpt, never the full body. CB45.
pub(crate) fn intelligence_duplicate_candidate_json(
    candidate: &crate::db::intelligence::DuplicateCandidate,
) -> serde_json::Value {
    serde_json::json!({
        "id": candidate.id,
        "kind": candidate.kind,
        "title": candidate.title,
        "project_hash": candidate.project_hash,
        "excerpt": candidate.excerpt,
    })
}

pub(crate) fn intelligence_edge_json(
    edge: &crate::db::intelligence::IntelligenceEdgeRecord,
) -> serde_json::Value {
    serde_json::json!({
        "id": edge.id,
        "from_node_id": edge.from_node_id,
        "to_node_id": edge.to_node_id,
        "relation": edge.relation,
        "weight": edge.weight,
        "created_at": edge.created_at,
    })
}

pub(crate) fn activity_log_entry_json(
    entry: &crate::db::activity_log::ActivityLogEntry,
) -> serde_json::Value {
    serde_json::json!({
        "id": entry.id,
        "workdir": entry.workdir,
        "source": entry.source,
        "source_id": entry.source_id,
        "kind": entry.kind,
        "message": entry.message,
        "payload": entry.payload,
        "created_at": entry.created_at,
    })
}

pub(crate) fn sync_message_json(message: &crate::domain::sync::SyncMessage) -> serde_json::Value {
    serde_json::json!({
        "id": message.id,
        "workdir": message.workdir,
        "agent_id": message.agent_id,
        "agent_name": message.agent_name,
        "kind": message.kind.as_str(),
        "message": message.message,
        "payload": message.payload,
        "created_at": message.created_at,
    })
}
