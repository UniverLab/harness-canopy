//! The `sandbox` resource tool: list, inspect, land and discard the worktrees
//! sandboxed graph runs leave behind (CB42).
//!
//! Every action is an adapter over [`crate::daemon::sandbox_ops`], which is
//! the same code `canopy sandbox` runs — `land` merges through
//! `domain::sandbox::land_sandbox` and `discard` through `discard_sandbox`,
//! so both surfaces share the refusals (never force, never auto-resolve a
//! conflict) and the `sandbox_runs` row update (`merged` / `discarded`).

use crate::daemon::actions::{action_params, ActionObject, ActionSpec, ToolSpec};
use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::daemon::sandbox_ops;
use crate::db::sandbox::SandboxRun;
use crate::domain::sandbox::SandboxInfo;
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

static SANDBOX: ToolSpec = ToolSpec {
    name: "sandbox",
    actions: &[
        ActionSpec {
            name: "list",
            required: &[],
            optional: &["graph_id", "status"],
        },
        ActionSpec {
            name: "get",
            required: &["id"],
            optional: &[],
        },
        ActionSpec {
            name: "land",
            required: &["id"],
            optional: &[],
        },
        ActionSpec {
            name: "discard",
            required: &["id"],
            optional: &["force"],
        },
    ],
};

#[tool_router(router = tool_router_sandbox, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "sandbox",
        description = "List, inspect, land and discard the sandbox worktrees a sandboxed graph run leaves behind. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `list` (required: none; optional: graph_id, status) — every sandbox run with its path, graph, branch, base branch, original workdir, status, whether its branch holds commits that exist nowhere else, how far behind its base it is, and whether its worktree is still registered. \
        `get` (required: id; optional: none) — one sandbox by full id or unambiguous id prefix, same fields as `list`. \
        `land` (required: id; optional: none) — merge the sandbox's branch onto its base branch, mark it `merged` and remove the worktree; refuses rather than forcing when the base checkout is on another branch, has uncommitted changes, or the merge conflicts (naming the conflicting paths), leaving the sandbox untouched. \
        `discard` (required: id; optional: force) — remove the sandbox's worktree and delete its branch, marking it `discarded`; refuses when the branch holds commits that exist nowhere else unless force: true is passed in the same call."
    )]
    pub(crate) async fn sandbox(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = SANDBOX.validate(&params)?;
        match action {
            "list" => self.sandbox_list_impl(action_params(&params)?),
            "get" => self.sandbox_get_impl(&action_params(&params)?),
            "land" => self.sandbox_land_impl(action_params(&params)?).await,
            "discard" => self.sandbox_discard_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }
}

impl TaskTriggerHandler {
    /// `sandbox` `list`: every stored row, optionally narrowed to one graph
    /// (graph-owned rows only) and one status, each enriched with live git
    /// facts. Untracked leftovers with no row are a CLI-only view — this
    /// surface never shows a sandbox nothing can then be done with.
    fn sandbox_list_impl(&self, params: SandboxListParams) -> Result<CallToolResult, McpError> {
        let graph_id = trimmed(params.graph_id);
        let status = trimmed(params.status);
        let rows = self
            .db
            .list_all_sandbox_runs()
            .map_err(|e| internal_error(&e))?;
        let infos: Vec<SandboxInfo> = rows
            .iter()
            .filter(|row| match &graph_id {
                Some(graph_id) => row.owner_type == "graph" && &row.owner_id == graph_id,
                None => true,
            })
            .filter(|row| match &status {
                Some(status) => &row.status == status,
                None => true,
            })
            .map(sandbox_ops::enrich)
            .collect();
        json_result(&infos)
    }

    /// `sandbox` `get`: one row by id or unambiguous prefix.
    fn sandbox_get_impl(&self, params: &SandboxIdParams) -> Result<CallToolResult, McpError> {
        let Some(row) = self.find_sandbox_row(&params.id)? else {
            return Ok(not_found_result(&params.id));
        };
        json_result(&sandbox_ops::enrich(&row))
    }

    /// `sandbox` `land`: the CLI's own land path, recording `merged`; a
    /// refusal comes back as a tool error result and the row is untouched.
    async fn sandbox_land_impl(&self, params: SandboxIdParams) -> Result<CallToolResult, McpError> {
        let Some(row) = self.find_sandbox_row(&params.id)? else {
            return Ok(not_found_result(&params.id));
        };
        let outcome = sandbox_ops::land_and_record(&self.db, &row)
            .await
            .map_err(|e| internal_error(&e))?;
        if let Some(refusal) = sandbox_ops::land_refusal(&row, &outcome) {
            return Ok(error_result(&refusal));
        }
        json_result(&landed_json(&row))
    }

    /// `sandbox` `discard`: the CLI's own discard path, recording
    /// `discarded`; unique commits are refused unless `force` is true.
    async fn sandbox_discard_impl(
        &self,
        params: SandboxDiscardParams,
    ) -> Result<CallToolResult, McpError> {
        let Some(row) = self.find_sandbox_row(&params.id)? else {
            return Ok(not_found_result(&params.id));
        };
        let force = params.force.unwrap_or(false);
        let outcome = sandbox_ops::discard_and_record(&self.db, &row, force)
            .await
            .map_err(|e| internal_error(&e))?;
        if outcome == crate::domain::sandbox::DiscardOutcome::RefusedUniqueCommits {
            return Ok(error_result(&sandbox_ops::discard_refusal(
                &row,
                "force: true",
            )));
        }
        json_result(&discarded_json(&row))
    }

    /// One stored row by id or unambiguous prefix, `Ok(None)` when nothing
    /// matches. Every action that names a sandbox resolves through here, so an
    /// unknown id is the same tool-level refusal on all of them — never an
    /// internal error, which would read as "canopy is broken" for a typo.
    fn find_sandbox_row(&self, sandbox_id: &str) -> Result<Option<SandboxRun>, McpError> {
        sandbox_ops::find_row(&self.db, sandbox_id).map_err(|e| internal_error(&e))
    }
}

/// The refusal result for an id that matches no sandbox, in the CLI's words.
fn not_found_result(sandbox_id: &str) -> CallToolResult {
    error_result(&sandbox_ops::not_found_message(sandbox_id))
}

/// An optional filter value, trimmed, blank read as "no filter".
fn trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn landed_json(row: &SandboxRun) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "status": "merged",
        "branch": row.sandbox_branch,
        "base_branch": row.base_branch,
    })
}

fn discarded_json(row: &SandboxRun) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "status": "discarded",
        "branch": row.sandbox_branch,
    })
}

fn json_result<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let body = serde_json::to_string_pretty(value)
        .map_err(|e| McpError::internal_error(format!("serializing result: {e}"), None))?;
    Ok(success_result(&body))
}

fn internal_error(error: &anyhow::Error) -> McpError {
    McpError::internal_error(error.to_string(), None)
}
