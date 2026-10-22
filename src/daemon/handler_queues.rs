//! MCP handler tools split out of `daemon::handler` (queue tools and shared queue helpers).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::{build_id_result, build_json_result, internal_error};
use crate::daemon::handler_specs_helpers::spec_summary_json;
use crate::daemon::handler_validate::{
    resolve_prefix_or_error, validate_non_empty, validate_spec_exists, validate_spec_unbindable,
};
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::GraphSpecStatus;
use crate::domain::queues::{Queue, QueueDetails, QueueMember};
use rmcp::model::{CallToolResult, Content};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_queues, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "queue_create",
        description = "Create a queue: an ordered list of existing specs, decoupled from any one graph."
    )]
    pub(crate) async fn queue_create(
        &self,
        Parameters(params): Parameters<QueueCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_queue_create(&params.name).await
    }

    #[tool(
        name = "queue_add_spec",
        description = "Add an existing spec to a queue; a grouped add lands right after the group's last pending member."
    )]
    pub(crate) async fn queue_add_spec(
        &self,
        Parameters(params): Parameters<QueueAddSpecParams>,
    ) -> Result<CallToolResult, McpError> {
        let queue_id = match resolve_prefix_or_error(
            &self.db,
            params.queue_id.trim(),
            Database::resolve_queue_id_by_prefix,
            "queue",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            |db, id| db.resolve_spec_id_by_prefix(id, None),
            "spec",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        self.do_queue_add_spec(&queue_id, &spec_id, params.group.as_deref())
            .await
    }

    #[tool(
        name = "queue_list",
        description = "List a queue's ordered members, or every queue (summary only) if queue_id is omitted."
    )]
    pub(crate) async fn queue_list(
        &self,
        Parameters(params): Parameters<QueueListParams>,
    ) -> Result<CallToolResult, McpError> {
        let resolved_queue_id = match &params.queue_id {
            Some(qid) => {
                match resolve_prefix_or_error(
                    &self.db,
                    qid.trim(),
                    Database::resolve_queue_id_by_prefix,
                    "queue",
                ) {
                    Ok(id) => Some(id),
                    Err(e) => return Ok(e),
                }
            }
            None => None,
        };
        self.do_queue_list(
            resolved_queue_id.as_deref(),
            params.include_descriptions.unwrap_or(false),
        )
        .await
    }

    #[tool(
        name = "queue_remove_spec",
        description = "Remove a spec from a queue."
    )]
    pub(crate) async fn queue_remove_spec(
        &self,
        Parameters(params): Parameters<QueueRemoveSpecParams>,
    ) -> Result<CallToolResult, McpError> {
        let queue_id = match resolve_prefix_or_error(
            &self.db,
            params.queue_id.trim(),
            Database::resolve_queue_id_by_prefix,
            "queue",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            |db, id| db.resolve_spec_id_by_prefix(id, None),
            "spec",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        self.do_queue_remove_spec(&queue_id, &spec_id).await
    }

    #[tool(
        name = "queue_reorder",
        description = "Reorder a queue. `spec_ids` must list every queue member exactly once, in the desired order — a total replacement, not a partial swap."
    )]
    pub(crate) async fn queue_reorder(
        &self,
        Parameters(params): Parameters<QueueReorderParams>,
    ) -> Result<CallToolResult, McpError> {
        let queue_id = match resolve_prefix_or_error(
            &self.db,
            params.queue_id.trim(),
            Database::resolve_queue_id_by_prefix,
            "queue",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let mut resolved_spec_ids = Vec::with_capacity(params.spec_ids.len());
        for sid in &params.spec_ids {
            match resolve_prefix_or_error(
                &self.db,
                sid.trim(),
                |db, id| db.resolve_spec_id_by_prefix(id, None),
                "spec",
            ) {
                Ok(id) => resolved_spec_ids.push(id),
                Err(e) => return Ok(e),
            }
        }
        self.do_queue_reorder(&queue_id, &resolved_spec_ids).await
    }

    #[tool(
        name = "queue_set_group",
        description = "Set or clear the context group of an existing queue member; each group's pending members stay contiguous."
    )]
    pub(crate) async fn queue_set_group(
        &self,
        Parameters(params): Parameters<QueueSetGroupParams>,
    ) -> Result<CallToolResult, McpError> {
        let queue_id = match resolve_prefix_or_error(
            &self.db,
            params.queue_id.trim(),
            Database::resolve_queue_id_by_prefix,
            "queue",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            |db, id| db.resolve_spec_id_by_prefix(id, None),
            "spec",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        self.do_queue_set_group(&queue_id, &spec_id, params.group.as_deref())
            .await
    }
}

impl TaskTriggerHandler {
    // ---- Queue tools (Q1) --------------------------------------------------
    //
    // A "queue" is an ordered list of existing specs, decoupled from any one
    // graph.

    pub(crate) async fn do_queue_create(&self, name: &str) -> Result<CallToolResult, McpError> {
        let name = name.trim();
        if let Err(e) = validate_non_empty(name, "Queue name") {
            return Ok(error_result(&e));
        }

        let queue = Queue {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            created_at: chrono::Utc::now(),
        };
        self.db.insert_queue(&queue).map_err(internal_error)?;

        Ok(build_id_result(&queue.id, "queue_id"))
    }

    pub(crate) async fn do_queue_add_spec(
        &self,
        queue_id: &str,
        spec_id: &str,
        group: Option<&str>,
    ) -> Result<CallToolResult, McpError> {
        let queue_id = queue_id.trim();
        if let Err(e) = validate_queue_exists(&self.db, queue_id) {
            return Ok(error_result(&e));
        }
        let spec_id = spec_id.trim();
        let spec = match validate_spec_exists(&self.db, spec_id) {
            Ok(spec) => spec,
            Err(e) => return Ok(error_result(&e)),
        };
        // SPECCODE: a queue is graph-agnostic, so an untyped spec is refused
        // at add time only when some browsing graph demands spec types
        // (`require_spec_type`) — it might run the queue next.
        if spec.spec_type.is_none() {
            let wants_types = self
                .db
                .list_graphs(None, false)
                .map_err(internal_error)?
                .iter()
                .any(|graph| graph.require_spec_type);
            if wants_types {
                return Ok(error_result(&format!(
                    "Spec '{}' has no type: a graph with require_spec_type may run this queue. Set a Conventional Commits type via spec_update first.",
                    spec.name
                )));
            }
        }
        let already_member = self
            .db
            .queue_has_member(queue_id, spec_id)
            .map_err(internal_error)?;
        if already_member {
            return Ok(error_result(&format!(
                "Spec '{spec_id}' is already in queue '{queue_id}'."
            )));
        }

        // An all-whitespace or empty `group` is treated as ungrouped.
        let group = group.map(str::trim).filter(|g| !g.is_empty());
        let new_member_is_pending = spec.status == GraphSpecStatus::Pending;

        self.db
            .add_queue_member_grouped(queue_id, spec_id, group, new_member_is_pending)
            .map_err(internal_error)?;

        queue_order_result(&self.db, queue_id)
    }

    pub(crate) async fn do_queue_set_group(
        &self,
        queue_id: &str,
        spec_id: &str,
        group: Option<&str>,
    ) -> Result<CallToolResult, McpError> {
        let queue_id = queue_id.trim();
        if let Err(e) = validate_queue_exists(&self.db, queue_id) {
            return Ok(error_result(&e));
        }
        let spec_id = spec_id.trim();
        let is_member = self
            .db
            .queue_has_member(queue_id, spec_id)
            .map_err(internal_error)?;
        if !is_member {
            return Ok(error_result(&format!(
                "Spec '{spec_id}' is not a member of queue '{queue_id}'."
            )));
        }

        // An all-whitespace or empty `group` is treated as clearing it.
        let group = group.map(str::trim).filter(|g| !g.is_empty());

        self.db
            .set_queue_member_group(queue_id, spec_id, group)
            .map_err(internal_error)?;

        queue_order_result(&self.db, queue_id)
    }

    pub(crate) async fn do_queue_list(
        &self,
        queue_id: Option<&str>,
        include_descriptions: bool,
    ) -> Result<CallToolResult, McpError> {
        let queue_id = queue_id.map(str::trim).filter(|s| !s.is_empty());

        let body = match queue_id {
            Some(queue_id) => {
                let details = match self.db.get_queue_details(queue_id) {
                    Ok(Some(details)) => details,
                    Ok(None) => return Ok(error_result(&format!("Queue '{queue_id}' not found."))),
                    Err(e) => return Err(internal_error(e.to_string())),
                };
                let total = details.members.len();
                let cap = 200usize;
                let truncated = total > cap;
                let omitted = total.saturating_sub(cap);
                let visible_members = &details.members[..details.members.len().min(cap)];
                let visible_groups: std::collections::HashMap<_, _> = details
                    .member_groups
                    .iter()
                    .filter(|(id, _)| visible_members.iter().any(|m| &m.id == *id))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let trimmed_details = QueueDetails {
                    queue: details.queue,
                    members: visible_members.to_vec(),
                    member_groups: visible_groups,
                };
                let mut qobj = queue_details_json(&trimmed_details, include_descriptions);
                qobj["total_members"] = serde_json::json!(total);
                if truncated {
                    qobj["truncated"] = serde_json::json!(true);
                    qobj["omitted"] = serde_json::json!(omitted);
                }
                serde_json::json!({ "queue": qobj })
            }
            None => {
                let queues = self.db.list_queues().map_err(internal_error)?;
                serde_json::json!({
                    "queues": queues
                        .iter()
                        .map(|queue| serde_json::json!({
                            "id": queue.id,
                            "name": queue.name,
                        }))
                        .collect::<Vec<_>>(),
                })
            }
        };

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        )]))
    }

    pub(crate) async fn do_queue_remove_spec(
        &self,
        queue_id: &str,
        spec_id: &str,
    ) -> Result<CallToolResult, McpError> {
        let queue_id = queue_id.trim();
        if let Err(e) = validate_queue_exists(&self.db, queue_id) {
            return Ok(error_result(&e));
        }
        let spec_id = spec_id.trim();
        if let Err(e) = validate_queue_member_removable(&self.db, queue_id, spec_id) {
            return Ok(error_result(&e));
        }
        let removed = self
            .db
            .remove_queue_member(queue_id, spec_id)
            .map_err(internal_error)?;
        if !removed {
            return Ok(error_result(&format!(
                "Queue '{queue_id}' has no spec '{spec_id}'."
            )));
        }

        Ok(success_result(&format!(
            "Spec '{spec_id}' removed from queue '{queue_id}'."
        )))
    }

    pub(crate) async fn do_graph_remove_spec(
        &self,
        graph_id: &str,
        spec_id: &str,
    ) -> Result<CallToolResult, McpError> {
        let spec_id = spec_id.trim();
        let spec = match validate_spec_exists(&self.db, spec_id) {
            Ok(s) => s,
            Err(e) => return Ok(error_result(&e)),
        };
        if let Some(ref bound_graph) = spec.graph_id {
            if bound_graph != graph_id {
                return Ok(error_result(&format!(
                    "Spec '{spec_id}' is bound to graph '{bound_graph}', not '{graph_id}'."
                )));
            }
        } else {
            return Ok(error_result(&format!(
                "Spec '{spec_id}' is not bound to any graph; nothing to unbind."
            )));
        }
        if let Err(e) = validate_spec_unbindable(&self.db, &spec) {
            return Ok(error_result(&e));
        }
        self.db.unbind_graph_spec(spec_id).map_err(internal_error)?;
        Ok(success_result(&format!(
            "Spec '{spec_id}' unbound from graph '{graph_id}'. It is now a standalone spec."
        )))
    }

    pub(crate) async fn do_queue_reorder(
        &self,
        queue_id: &str,
        spec_ids: &[String],
    ) -> Result<CallToolResult, McpError> {
        let queue_id = queue_id.trim();
        if let Err(e) = validate_queue_exists(&self.db, queue_id) {
            return Ok(error_result(&e));
        }
        let current = self
            .db
            .list_queue_member_spec_ids(queue_id)
            .map_err(internal_error)?;
        if let Err(e) = validate_queue_reorder(&current, spec_ids) {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_queue_reorder_locking(&self.db, &current, spec_ids) {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_reorder_groups_intact(&self.db, queue_id, spec_ids) {
            return Ok(error_result(&e));
        }

        self.db
            .reorder_queue_members(queue_id, spec_ids)
            .map_err(internal_error)?;

        queue_order_result(&self.db, queue_id)
    }
}

pub(crate) fn validate_queue_exists(db: &Database, queue_id: &str) -> Result<Queue, String> {
    db.get_queue(queue_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Queue '{queue_id}' not found."))
}

/// Refuse to start a queue run when one of the queue's specs is already
/// `running` under a different graph. A queue spec's own `graph_id` stays
/// `None` (queue membership never binds it), so ownership is read off the
/// spec's most recent `graph_runs` row instead — the graph that most recently
/// touched the spec is the only one that could have set it `running`.
///
/// This is a start-time check, not a lock: two `graph_run` calls issued in
/// the same instant, before either has run a single node, can still race.
pub(crate) fn validate_queue_not_consumed(
    db: &Database,
    queue_id: &str,
    requesting_graph_id: &str,
) -> Result<(), String> {
    for spec_id in db
        .list_queue_member_spec_ids(queue_id)
        .map_err(|e| e.to_string())?
    {
        let Some(spec) = db.get_graph_spec(&spec_id).map_err(|e| e.to_string())? else {
            continue;
        };
        if spec.status != GraphSpecStatus::Running {
            continue;
        }

        let runs = db
            .list_graph_runs_for_spec(&spec_id)
            .map_err(|e| e.to_string())?;
        let owner_graph_id = runs.last().map(|run| run.graph_id.clone());
        if owner_graph_id.as_deref() != Some(requesting_graph_id) {
            let owner = owner_graph_id.unwrap_or_else(|| "another graph".to_string());
            return Err(format!(
                "Queue '{queue_id}' spec '{spec_id}' is already running under graph '{owner}'; wait for it to finish, or pause that graph, before starting a new run against this queue."
            ));
        }
    }
    Ok(())
}

/// Validate that `spec_ids` is a total permutation of `current`: same
/// length, same set, no duplicates, no unknown ids. This rejects any partial
/// reorder (a subset, or a list with an unrecognized id) so the operation is
/// always "here is the whole new order," never a swap of two entries applied
/// on top of unknown existing state.
pub(crate) fn validate_queue_reorder(
    current: &[String],
    spec_ids: &[String],
) -> Result<(), String> {
    if spec_ids.len() != current.len() {
        return Err(format!(
            "Reorder must list all {} queue spec(s) exactly once; got {}.",
            current.len(),
            spec_ids.len()
        ));
    }

    let mut seen = std::collections::HashSet::new();
    for id in spec_ids {
        if !seen.insert(id.as_str()) {
            return Err(format!("Reorder lists spec '{id}' more than once."));
        }
        if !current.iter().any(|existing| existing == id) {
            return Err(format!("Queue has no spec '{id}'."));
        }
    }
    Ok(())
}

/// Live queues (R6): refuse to remove a queue member that is currently
/// `running` — the tool-layer half of the same lock `queue_reorder` enforces
/// via [`validate_queue_reorder_locking`]. A spec that isn't a queue member at
/// all, or isn't found, is left for [`Database::remove_queue_member`]'s own
/// "no such spec" error — this only ever blocks a positive `running` match.
pub(crate) fn validate_queue_member_removable(
    db: &Database,
    queue_id: &str,
    spec_id: &str,
) -> Result<(), String> {
    if let Some(spec) = db.get_graph_spec(spec_id).map_err(|e| e.to_string())? {
        if spec.status == GraphSpecStatus::Running {
            return Err(format!(
                "Spec '{spec_id}' is currently running and cannot be removed from queue '{queue_id}'; wait for it to finish, or pause the graph, first."
            ));
        }
    }
    Ok(())
}

/// Live queues (R6): the currently running spec and every already-executed
/// one (`completed`/`failed`/`skipped`) are immutable in the queue's order —
/// only `pending` members may move. Call after [`validate_queue_reorder`] has
/// already confirmed `spec_ids` is a total permutation of `current`: under
/// that guarantee, a locked member "doesn't move" iff it sits at the same
/// index in both slices, since moving it necessarily displaces whatever now
/// occupies its old slot.
pub(crate) fn validate_queue_reorder_locking(
    db: &Database,
    current: &[String],
    spec_ids: &[String],
) -> Result<(), String> {
    for (index, spec_id) in current.iter().enumerate() {
        let status = db
            .get_graph_spec(spec_id)
            .map_err(|e| e.to_string())?
            .map(|spec| spec.status)
            .unwrap_or(GraphSpecStatus::Pending);
        if status == GraphSpecStatus::Pending {
            continue;
        }
        if spec_ids.get(index) != Some(spec_id) {
            return Err(format!(
                "Spec '{spec_id}' is {} and cannot be moved by a reorder; only pending members may be reordered.",
                status.as_str()
            ));
        }
    }
    Ok(())
}

/// Refuse a reorder that would split a context group's pending members. Built
/// from the CURRENT membership's groups + statuses, applied to the requested
/// order, so it works before anything is written. Runs after
/// [`validate_queue_reorder`] confirmed `spec_ids` is a total permutation.
pub(crate) fn validate_reorder_groups_intact(
    db: &Database,
    queue_id: &str,
    spec_ids: &[String],
) -> Result<(), String> {
    let members = db
        .queue_members_in_order(queue_id)
        .map_err(|e| e.to_string())?;
    let by_id: std::collections::HashMap<&str, &QueueMember> = members
        .iter()
        .map(|member| (member.spec_id.as_str(), member))
        .collect();
    let proposed: Vec<QueueMember> = spec_ids
        .iter()
        .map(|spec_id| {
            let current = by_id.get(spec_id.as_str());
            QueueMember {
                spec_id: spec_id.clone(),
                group: current.and_then(|member| member.group.clone()),
                locked: current.is_some_and(|member| member.locked),
            }
        })
        .collect();
    let Some(split) = crate::domain::queues::split_group(&proposed) else {
        return Ok(());
    };
    let positions = split
        .positions
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "Reorder would split group '{}': its pending members would sit at positions {}. Nothing was changed.",
        split.group, positions
    ))
}

/// The queue's resulting order, with groups, as the tool result of a write
/// that moved a member (CM44 FR6).
fn queue_order_result(db: &Database, queue_id: &str) -> Result<CallToolResult, McpError> {
    let details = db
        .get_queue_details(queue_id)
        .map_err(internal_error)?
        .ok_or_else(|| internal_error(format!("Queue '{queue_id}' not found.")))?;
    Ok(build_json_result(&serde_json::json!({
        "queue": queue_details_json(&details, false),
    })))
}

pub(crate) fn queue_details_json(
    details: &QueueDetails,
    include_descriptions: bool,
) -> serde_json::Value {
    serde_json::json!({
        "id": details.queue.id,
        "name": details.queue.name,
        "members": details
            .members
            .iter()
            .enumerate()
            .map(|(index, spec)| {
                let mut value = spec_summary_json(spec, include_descriptions);
                value["queue_position"] = serde_json::json!(index + 1);
                if let Some(Some(group)) = details.member_groups.get(&spec.id) {
                    value["group"] = serde_json::json!(group);
                }
                value
            })
            .collect::<Vec<_>>(),
    })
}
