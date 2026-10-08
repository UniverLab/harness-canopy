//! MCP handler tools split out of `daemon::handler` (sync and project-context tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::{missing_sync_identity_error, OptionalExtension, TaskTriggerHandler};
use crate::daemon::handler_helpers::{map_action_result, resolve_effective_project_hash};
use crate::daemon::handler_validate::validate_non_empty;
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::intelligence::IntelligenceNodeRecord;
use crate::db::Database;
use crate::domain::sync::MessageKind;
use crate::domain::sync::MissionImpact;
use crate::domain::sync::WorkspaceStatus;
use crate::shared::sync_identity::header_str;
use crate::shared::sync_identity::CANOPY_AGENT_ID_ENV;
use crate::shared::sync_identity::CANOPY_AGENT_ID_HEADER;
use crate::shared::sync_identity::CANOPY_CLIENT_NAME_ENV;
use crate::shared::sync_identity::CANOPY_CLIENT_NAME_HEADER;
use axum::http::request::Parts;
use rmcp::model::{CallToolResult, Content};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_sync, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "sync_declare_intent",
        description = "Declare a high-level mission for the current workdir. Non-blocking. \
         Use this to announce major work before you start changing things. \
         The actor identity is resolved automatically from the current Canopy session."
    )]
    pub(crate) async fn sync_declare_intent(
        &self,
        Parameters(params): Parameters<SyncDeclareIntentParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;
        let Some(impact) = MissionImpact::from_str(&params.impact) else {
            return Ok(error_result(
                "Invalid impact. Must be: low, high, breaking.",
            ));
        };
        let agent_id = self.resolve_sync_agent_id(parts.as_ref())?;
        let client_name = self.resolve_sync_client_name(parts.as_ref());

        Ok(map_action_result(
            self.sync_manager
                .declare_intent(
                    &params.workdir,
                    &agent_id,
                    client_name.as_deref(),
                    &params.mission,
                    impact,
                    &params.description,
                )
                .await,
            "Intent declared.",
        ))
    }

    #[tool(
        name = "sync_report_status",
        description = "Report the current workspace status for your mission. Non-blocking. \
         status: stable | unstable | testing. The actor identity is resolved \
         automatically from the current Canopy session."
    )]
    pub(crate) async fn sync_report_status(
        &self,
        Parameters(params): Parameters<SyncReportStatusParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;
        let Some(status) = WorkspaceStatus::from_str(&params.status) else {
            return Ok(error_result(
                "Invalid status. Must be: stable, unstable, testing.",
            ));
        };
        let agent_id = self.resolve_sync_agent_id(parts.as_ref())?;
        let client_name = self.resolve_sync_client_name(parts.as_ref());

        Ok(map_action_result(
            self.sync_manager
                .report_status(
                    &params.workdir,
                    &agent_id,
                    client_name.as_deref(),
                    status,
                    &params.message,
                )
                .await,
            "Status reported.",
        ))
    }

    /// Broadcast a message to the workdir sync channel (non-blocking).
    #[tool(
        name = "sync_broadcast",
        description = "Broadcast a message to the workdir sync channel. Non-blocking. \
         kind: info | query | answer. The actor identity is resolved \
         automatically from the current Canopy session."
    )]
    pub(crate) async fn sync_broadcast(
        &self,
        Parameters(params): Parameters<SyncBroadcastParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;
        let Some(kind) = MessageKind::from_str(&params.kind) else {
            return Ok(error_result("Invalid kind. Must be: info, query, answer."));
        };
        if !kind.is_chatter() {
            return Ok(error_result(
                "sync_broadcast only accepts: info, query, answer.",
            ));
        }

        let payload = params
            .metadata
            .as_ref()
            .map(|metadata| metadata.to_string());
        let agent_id = self.resolve_sync_agent_id(parts.as_ref())?;
        let client_name = self.resolve_sync_client_name(parts.as_ref());
        Ok(map_action_result(
            self.sync_manager
                .broadcast(
                    &params.workdir,
                    &agent_id,
                    client_name.as_deref(),
                    kind,
                    &params.message,
                    payload.as_deref(),
                )
                .await,
            "Message broadcast.",
        ))
    }

    #[tool(
        name = "sync_get_context",
        description = "Get active missions, recent sync chatter, and current workspace vibe for a workdir."
    )]
    pub(crate) async fn sync_get_context(
        &self,
        Parameters(params): Parameters<SyncGetContextParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;
        let limit = params.limit.unwrap_or(10);

        let context = self
            .sync_manager
            .get_context(&params.workdir, limit)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let intents_json: Vec<serde_json::Value> = context
            .active_intents
            .iter()
            .map(|intent| {
                serde_json::json!({
                    "agent_id": intent.agent_id,
                    "agent_name": intent.agent_name,
                    "mission": intent.mission,
                    "impact": intent.impact.as_str(),
                    "description": intent.description,
                    "status": intent.status.as_str(),
                    "since": intent.since,
                })
            })
            .collect();

        let chatter_json: Vec<serde_json::Value> = context
            .recent_chatter
            .iter()
            .map(|message| {
                serde_json::json!({
                    "agent_id": message.agent_id,
                    "agent_name": message.agent_name,
                    "kind": message.kind.as_str(),
                    "message": message.message,
                    "ts": message.created_at,
                })
            })
            .collect();

        let summary = if context.active_intents.is_empty() {
            "No active missions.".to_owned()
        } else {
            let parts: Vec<String> = context
                .active_intents
                .iter()
                .map(|intent| format!("{}: {}", intent.agent_name, intent.mission))
                .collect();
            parts.join("; ")
        };

        let out = serde_json::json!({
            "active_intents": intents_json,
            "recent_chatter": chatter_json,
            "vibe": context.vibe.as_str(),
            "summary": summary,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "intelligence_list_projects",
        description = "List all indexed projects for the project picker. \
         Returns project nodes with their hash, name, and description. \
         Supports optional query filtering by name/body."
    )]
    pub(crate) async fn intelligence_list_projects(
        &self,
        Parameters(params): Parameters<IntelligenceListProjectsParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = params.limit.unwrap_or(20).min(100);
        let projects = self
            .db
            .list_intelligence_projects(params.query.as_deref(), limit)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let projects_json: Vec<serde_json::Value> = projects
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "title": p.title,
                    "body": p.body,
                    "project_hash": p.project_hash,
                    "metadata": p.metadata.as_ref().and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok()),
                    "updated_at": p.updated_at,
                })
            })
            .collect();

        let out = serde_json::json!({
            "count": projects_json.len(),
            "projects": projects_json,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "intelligence_link_projects",
        description = "Create a typed relationship between two indexed projects. \
         Relation must be one of: depends_on, complements, extends, publishes \
         (relates_to is accepted as legacy). 'contains' is derived from registry \
         paths and rejected here. Both projects must exist as kind='project' \
         nodes in the intelligence graph."
    )]
    pub(crate) async fn intelligence_link_projects(
        &self,
        Parameters(params): Parameters<IntelligenceLinkProjectsParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(e) = validate_non_empty(&params.from_project_hash, "from_project_hash") {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_non_empty(&params.to_project_hash, "to_project_hash") {
            return Ok(error_result(&e));
        }

        let relation = params.relation.as_deref().unwrap_or("relates_to");
        if let Err(e) = validate_non_empty(relation, "Relation") {
            return Ok(error_result(&e));
        }
        if let Err(e) = crate::domain::project::validate_project_relation(relation) {
            return Ok(error_result(&e.to_string()));
        }
        if relation == "contains" {
            return Ok(error_result(
                "relation 'contains' is derived from registry paths — it cannot be hand-linked",
            ));
        }

        let edge = self
            .db
            .link_projects(
                &params.from_project_hash,
                &params.to_project_hash,
                relation,
                params.weight,
            )
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;

        let out = serde_json::json!({
            "edge_id": edge.id,
            "from_node_id": edge.from_node_id,
            "to_node_id": edge.to_node_id,
            "relation": edge.relation,
            "weight": edge.weight,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }
}

impl TaskTriggerHandler {
    pub(crate) fn resolve_sync_agent_id(&self, parts: Option<&Parts>) -> Result<String, McpError> {
        if let Some(parts) = parts {
            if let Some(agent_id) = header_str(parts, CANOPY_AGENT_ID_HEADER) {
                return Ok(agent_id.to_string());
            }
        }
        if let Ok(agent_id) = std::env::var(CANOPY_AGENT_ID_ENV) {
            let trimmed = agent_id.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
        Err(missing_sync_identity_error())
    }

    pub(crate) fn resolve_sync_client_name(&self, parts: Option<&Parts>) -> Option<String> {
        if let Some(parts) = parts {
            if let Some(client_name) = header_str(parts, CANOPY_CLIENT_NAME_HEADER) {
                return Some(client_name.to_string());
            }
        }
        std::env::var(CANOPY_CLIENT_NAME_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    pub(crate) fn reject_if_nursery(&self, parts: Option<&Parts>) -> Result<(), McpError> {
        let agent_id = self.resolve_sync_agent_id(parts)?;
        match self.db.get_session_type(&agent_id) {
            Ok(Some(st)) if st == "nursery" => {
                Err(McpError::invalid_params(
                    "This tool is not available during a seed creation session (nursery). Complete the seed identity interview first.".to_string(),
                    None,
                ))
            }
            Ok(_) => Ok(()),
            Err(e) => Err(McpError::internal_error(e.to_string(), None)),
        }
    }

    /// Resolve the project hash a delete tool should scope itself to: an
    /// explicit `provided` value always wins (the same override the read
    /// tools allow), otherwise it's auto-detected from the caller's session
    /// workdir. Returns `None` if neither is available.
    pub(crate) fn effective_project_hash_for_delete(
        &self,
        parts: Option<&Parts>,
        provided: Option<&str>,
    ) -> Option<String> {
        if let Some(project_hash) = provided {
            return Some(project_hash.to_string());
        }
        let agent_id = self.resolve_sync_agent_id(parts).ok()?;
        resolve_effective_project_hash(&self.db, None, &agent_id)
    }

    /// `Some(error result)` if `node` belongs to a project other than
    /// `effective_project_hash`, `None` if deletion may proceed. Nodes
    /// without a `project_hash` aren't project-scoped, so they're always in
    /// scope.
    pub(crate) fn check_intelligence_delete_scope(
        &self,
        node: &crate::db::intelligence::IntelligenceNodeRecord,
        effective_project_hash: Option<&str>,
    ) -> Option<CallToolResult> {
        let node_project_hash = node.project_hash.as_deref()?;
        if effective_project_hash == Some(node_project_hash) {
            return None;
        }
        Some(error_result(&format!(
            "Intelligence node '{}' belongs to project '{}', which is outside the caller's \
             active project scope. Pass project_hash explicitly to operate on it anyway.",
            node.id, node_project_hash
        )))
    }

    /// Fetch knowledge for the context endpoint, traversing the project graph.
    /// Returns project-scoped plus generic knowledge with traversal info.
    /// Each tagged row is (record, via_project_hash, via_relation): root rows
    /// carry `via_relation: None` (direct); reached rows carry the edge
    /// relation that reached them. Bounded: per-project cap + total cap of 60.
    /// The root project keeps the historic single-project limit so a project
    /// with no edges reads exactly as before (modulo additive via_* fields).
    pub(crate) fn fetch_context_knowledge(
        &self,
        project_hash: Option<&str>,
        scope: &str,
        depth: usize,
    ) -> Result<ContextKnowledge, String> {
        if let Some(ph) = project_hash {
            self.fetch_project_context_knowledge(ph, scope, depth)
        } else {
            self.fetch_generic_context_knowledge(scope)
        }
    }

    /// Scoped fetch: traversed project knowledge plus generic sessions.
    /// Preserves traversal order, root vs related per-project limits, the
    /// 60-item total cap, depth-cap metadata and database call order.
    pub(crate) fn fetch_project_context_knowledge(
        &self,
        project_hash: &str,
        scope: &str,
        depth: usize,
    ) -> Result<ContextKnowledge, String> {
        const TOTAL_CAP: usize = 60;
        let root_limit = if scope == "full" { 50 } else { 20 };
        let per_project_limit = if scope == "full" { 20 } else { 8 };
        let traversal = self
            .db
            .traverse_project_scope(project_hash, depth)
            .map_err(|e| e.to_string())?;
        let mut tagged = Vec::new();
        for tp in &traversal.projects {
            let limit = if tp.via_relation.is_none() {
                root_limit
            } else {
                per_project_limit
            };
            if !Self::append_traversed_project_knowledge(
                &self.db,
                &mut tagged,
                &tp.hash,
                tp.via_relation.as_deref(),
                limit,
                TOTAL_CAP,
            )? {
                break;
            }
        }
        let traversal_info = serde_json::json!({
            "depth": depth.min(crate::db::intelligence::MAX_TRAVERSAL_DEPTH),
            "projects_reached": traversal.projects.len(),
            "total_cap": TOTAL_CAP,
        });
        let generic: Vec<crate::db::intelligence::IntelligenceNodeRecord> = self
            .db
            .list_operational_sessions(if scope == "full" { 20 } else { 5 })
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|r| r.into_intelligence_node_record())
            .collect();
        Ok(ContextKnowledge {
            generic,
            project: tagged,
            traversal: traversal_info,
        })
    }

    /// Unscoped fallback: generic knowledge only, with empty project rows
    /// and traversal info — the same shape the scoped path returns.
    pub(crate) fn fetch_generic_context_knowledge(
        &self,
        scope: &str,
    ) -> Result<ContextKnowledge, String> {
        let knowledge_limit = if scope == "full" { 20 } else { 5 };
        let generic = self
            .db
            .list_intelligence_nodes(None, knowledge_limit)
            .map_err(|e| e.to_string())?;
        Ok(ContextKnowledge {
            generic,
            project: Vec::new(),
            traversal: serde_json::json!({}),
        })
    }

    /// Appends one traversed project's knowledge rows to `tagged` in
    /// traversal order, honoring the per-project limit, and reports whether
    /// the total cap still has room — `false` stops the caller's traversal.
    /// The list query runs even at the cap (exactly as it always has), so
    /// the number of database calls per traversal is unchanged.
    pub(crate) fn append_traversed_project_knowledge(
        db: &Database,
        tagged: &mut TaggedProjectKnowledge,
        via_hash: &str,
        via_relation: Option<&str>,
        limit: usize,
        total_cap: usize,
    ) -> Result<bool, String> {
        let rows = db
            .list_project_knowledge(via_hash, None, limit)
            .map_err(|e| e.to_string())?;
        for row in rows {
            if tagged.len() >= total_cap {
                return Ok(false);
            }
            tagged.push((row, via_hash.to_string(), via_relation.map(str::to_string)));
        }
        Ok(tagged.len() < total_cap)
    }
}

pub(crate) type TaggedProjectKnowledge = Vec<(
    crate::db::intelligence::IntelligenceNodeRecord,
    String,
    Option<String>,
)>;

pub(crate) struct ContextKnowledge {
    pub(crate) generic: Vec<crate::db::intelligence::IntelligenceNodeRecord>,
    pub(crate) project: TaggedProjectKnowledge,
    pub(crate) traversal: serde_json::Value,
}

pub(crate) fn inject_project_context(
    out: &mut serde_json::Value,
    project_hash: Option<&str>,
    project_knowledge: &[(IntelligenceNodeRecord, String, Option<String>)],
    related_projects: &[(
        IntelligenceNodeRecord,
        crate::db::intelligence::IntelligenceEdgeRecord,
    )],
    traversal: &serde_json::Value,
    dependents: &[crate::db::intelligence::IntelligenceProjectDependencyRecord],
) {
    if let Some(ph) = project_hash {
        if let Some(obj) = out.as_object_mut() {
            obj.insert("project_hash".to_string(), serde_json::json!(ph));
        }
    }

    if !project_knowledge.is_empty() {
        let pk_json: Vec<serde_json::Value> = project_knowledge
            .iter()
            .map(|(n, via_project, via_relation)| {
                // `contains` hops read as upward inheritance, never a silent merge.
                let via_relation = match via_relation.as_deref() {
                    None => serde_json::Value::Null,
                    Some("contains") => serde_json::json!("inherited:contains"),
                    Some(other) => serde_json::json!(other),
                };
                serde_json::json!({
                    "id": n.id,
                    "kind": n.kind,
                    "title": n.title,
                    "body": n.body,
                    "project_hash": n.project_hash,
                    "via_project": via_project,
                    "via_relation": via_relation,
                })
            })
            .collect();
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "project_knowledge".to_string(),
                serde_json::Value::Array(pk_json),
            );
            if let Some(summary) = obj.get("summary").and_then(|s| s.as_str()) {
                let new_summary = format!(
                    "{} (+ {} project fact(s)/pattern(s))",
                    summary,
                    project_knowledge.len()
                );
                obj.insert(
                    "summary".to_string(),
                    serde_json::Value::String(new_summary),
                );
            }
        }
    }

    if let Some(obj) = out.as_object_mut() {
        if project_hash.is_some()
            && traversal.is_object()
            && !traversal.as_object().map(|o| o.is_empty()).unwrap_or(true)
        {
            obj.insert("traversal".to_string(), traversal.clone());
        }
    }

    if !dependents.is_empty() {
        let target = project_hash.unwrap_or_default();
        let dep_json: Vec<serde_json::Value> = dependents
            .iter()
            .map(|d| {
                serde_json::json!({
                    "from_node_id": d.from_node_id,
                    "from_title": d.from_title,
                    "from_project_hash": d.from_project_hash,
                    "relation": d.relation,
                })
            })
            .collect();
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "dependency_impact".to_string(),
                serde_json::json!({
                    "target": target,
                    "dependents": dep_json,
                }),
            );
        }
    }

    if !related_projects.is_empty() {
        let rp_json: Vec<serde_json::Value> = related_projects
            .iter()
            .map(|(node, edge)| {
                serde_json::json!({
                    "project": {
                        "id": node.id,
                        "title": node.title,
                        "project_hash": node.project_hash,
                    },
                    "relation": edge.relation,
                    "weight": edge.weight,
                })
            })
            .collect();
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "related_projects".to_string(),
                serde_json::Value::Array(rp_json),
            );
        }
    }
}
