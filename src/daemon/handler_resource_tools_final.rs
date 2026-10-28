//! The eight final-wave resource tools: one `#[tool]` method per resource,
//! each dispatching on `action` through the shared [`ToolSpec`] contract in
//! [`crate::daemon::actions`]. Every action validates first (one refusal
//! listing the whole action table), then deserializes the same JSON — minus
//! the dispatch key — into the existing per-operation params struct and
//! calls the `<tool>_<action>` `_impl` method. No behaviour lives here; the
//! logic is the old tools' own.
//!
//! `scheduled_send_create` stays a standalone tool (creating a send is not
//! listing or cancelling one) and keeps its own `#[tool]` in
//! `handler_scheduled_sends`.

use crate::daemon::actions::{action_params, ActionObject, ActionSpec, ToolSpec};
use crate::daemon::handler::{OptionalExtension, TaskTriggerHandler};
use crate::daemon::params_extract::Parameters;
use axum::http::request::Parts;
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

// ── Action tables ────────────────────────────────────────────────────

static NODE: ToolSpec = ToolSpec {
    name: "node",
    actions: &[
        ActionSpec {
            name: "list",
            required: &[],
            optional: &[],
        },
        ActionSpec {
            name: "get",
            required: &["name"],
            optional: &[],
        },
        ActionSpec {
            name: "install",
            required: &["ref"],
            optional: &["overwrite_presets", "accept"],
        },
        ActionSpec {
            name: "remove",
            required: &["name"],
            optional: &[],
        },
    ],
};

static INTELLIGENCE_ADMIN: ToolSpec = ToolSpec {
    name: "intelligence_admin",
    actions: &[
        ActionSpec {
            name: "delete_node",
            required: &["node_id"],
            optional: &["project_hash"],
        },
        ActionSpec {
            name: "delete_relation",
            required: &["edge_id"],
            optional: &["project_hash"],
        },
        ActionSpec {
            name: "walk",
            required: &["node_id"],
            optional: &["depth", "compact"],
        },
        ActionSpec {
            name: "list_projects",
            required: &[],
            optional: &["query", "limit"],
        },
        ActionSpec {
            name: "link_projects",
            required: &["from_project_hash", "to_project_hash"],
            optional: &["relation", "weight"],
        },
    ],
};

static PROJECT: ToolSpec = ToolSpec {
    name: "project",
    actions: &[
        ActionSpec {
            name: "register",
            required: &["path"],
            optional: &[],
        },
        ActionSpec {
            name: "remap",
            required: &["project_hash", "new_path"],
            optional: &["dry_run", "force"],
        },
        ActionSpec {
            name: "search",
            required: &["query"],
            optional: &[],
        },
        ActionSpec {
            name: "update",
            required: &["project_hash"],
            optional: &["description", "tags"],
        },
    ],
};

static SCHEDULED_SEND: ToolSpec = ToolSpec {
    name: "scheduled_send",
    actions: &[
        ActionSpec {
            name: "list",
            required: &[],
            optional: &["session_id"],
        },
        ActionSpec {
            name: "cancel",
            required: &["id"],
            optional: &[],
        },
    ],
};

static SKILL: ToolSpec = ToolSpec {
    name: "skill",
    actions: &[
        ActionSpec {
            name: "list",
            required: &[],
            optional: &[],
        },
        ActionSpec {
            name: "get",
            required: &["name"],
            optional: &[],
        },
    ],
};

static SUBAGENT: ToolSpec = ToolSpec {
    name: "subagent",
    actions: &[
        ActionSpec {
            name: "spawn",
            required: &["prompt"],
            optional: &[
                "cli",
                "workdir",
                "model",
                "effort",
                "mcp_servers",
                "timeout_minutes",
                "ttl_minutes",
                "blocking",
            ],
        },
        ActionSpec {
            name: "collect",
            required: &["id"],
            optional: &[],
        },
    ],
};

static RAG: ToolSpec = ToolSpec {
    name: "rag",
    actions: &[
        ActionSpec {
            name: "search",
            required: &["query"],
            optional: &["agent_id", "limit"],
        },
        ActionSpec {
            name: "compact",
            required: &[],
            optional: &[],
        },
    ],
};

static IDENTITY: ToolSpec = ToolSpec {
    name: "identity",
    actions: &[
        ActionSpec {
            name: "get",
            required: &[],
            optional: &[],
        },
        ActionSpec {
            name: "evolve",
            required: &[],
            optional: &["new_directives", "new_traits"],
        },
        ActionSpec {
            name: "list_seeds",
            required: &[],
            optional: &[],
        },
        ActionSpec {
            name: "create_seed",
            required: &["name"],
            optional: &["directives", "traits"],
        },
        ActionSpec {
            name: "remove_seed",
            required: &["seed_id"],
            optional: &[],
        },
    ],
};

// ── The eight resource tools ─────────────────────────────────────────

#[tool_router(router = tool_router_resource_tools_final, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "node",
        description = "List, inspect, install, and remove shareable node documents. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `list` (required: none; optional: none) — list every node document available to graph_node add and graph_ensemble add, installed and builtin. \
        `get` (required: name; optional: none) — return one node document with its kind, source, and full config. \
        `install` (required: ref; optional: overwrite_presets, accept) — install a node document from the catalogue by ref (`<slug>`, `<owner>/<repo>/<slug>`, each with an optional `@<ref>`, or `draft:<id>` for a one-use editor draft); without accept: true it writes nothing and returns {needs_review: true, review} for the agent to show its user first, with accept: true it installs and echoes the review. \
        `remove` (required: name; optional: none) — remove an installed node document; a builtin with no installed copy is refused."
    )]
    pub(crate) async fn node(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = NODE.validate(&params)?;
        match action {
            "list" => self.node_list_impl().await,
            "get" => self.node_get_impl(action_params(&params)?).await,
            "install" => self.node_install_impl(action_params(&params)?).await,
            "remove" => self.node_remove_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "intelligence_admin",
        description = "Delete intelligence nodes and relations, walk the knowledge graph, and manage project links. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `delete_node` (required: node_id; optional: project_hash) — delete an intelligence node and every relation touching it, in one transaction (hard delete); errors if the node does not exist. \
        `delete_relation` (required: edge_id; optional: project_hash) — delete a single relation by ID, leaving both endpoint nodes intact; errors if it does not exist. \
        `walk` (required: node_id; optional: depth, compact) — walk the project-context graph from a node up to depth; compact mode (default) omits bodies, and the root is returned in `root` only. \
        `list_projects` (required: none; optional: query, limit) — list all indexed projects for the project picker. \
        `link_projects` (required: from_project_hash, to_project_hash; optional: relation, weight) — create a typed relationship between two indexed projects."
    )]
    pub(crate) async fn intelligence_admin(
        &self,
        Parameters(params): Parameters<ActionObject>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = INTELLIGENCE_ADMIN.validate(&params)?;
        match action {
            "delete_node" => {
                self.intelligence_admin_delete_node_impl(action_params(&params)?, parts.as_ref())
                    .await
            }
            "delete_relation" => {
                self.intelligence_admin_delete_relation_impl(
                    action_params(&params)?,
                    parts.as_ref(),
                )
                .await
            }
            "walk" => {
                self.intelligence_admin_walk_impl(action_params(&params)?)
                    .await
            }
            "list_projects" => {
                self.intelligence_admin_list_projects_impl(action_params(&params)?)
                    .await
            }
            "link_projects" => {
                self.intelligence_admin_link_projects_impl(action_params(&params)?)
                    .await
            }
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "project",
        description = "Register, remap, search, and update project registry entries. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `register` (required: path; optional: none) — explicitly register a project directory in the registry (no marker file required). \
        `remap` (required: project_hash, new_path; optional: dry_run, force) — remap a renamed or moved project to its new path, re-keying its sessions and history instead of losing them; dry_run previews without changing anything. \
        `search` (required: query; optional: none) — search projects by name/description. \
        `update` (required: project_hash; optional: description, tags) — update project metadata."
    )]
    pub(crate) async fn project(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = PROJECT.validate(&params)?;
        match action {
            "register" => self.project_register_impl(action_params(&params)?).await,
            "remap" => self.project_remap_impl(action_params(&params)?).await,
            "search" => self.project_search_impl(action_params(&params)?).await,
            "update" => self.project_update_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "scheduled_send",
        description = "List and cancel pending scheduled sends (`scheduled_send_create` stays its own tool). Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `list` (required: none; optional: session_id) — list pending (not yet fired) scheduled sends for a session, soonest first; omit session_id for your own session. \
        `cancel` (required: id; optional: none) — cancel a pending scheduled send by id before it fires."
    )]
    pub(crate) async fn scheduled_send(
        &self,
        Parameters(params): Parameters<ActionObject>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = SCHEDULED_SEND.validate(&params)?;
        match action {
            "list" => {
                self.scheduled_send_list_impl(action_params(&params)?, parts.as_ref())
                    .await
            }
            "cancel" => {
                self.scheduled_send_cancel_impl(action_params(&params)?)
                    .await
            }
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "skill",
        description = "Discover and fetch skills from the dynamic skill store. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `list` (required: none; optional: none) — list skills available to this agent, each with name, description, source, and whether it is installed or merely available. \
        `get` (required: name; optional: none) — fetch a skill's full instructions by name, cloning or refreshing it on demand."
    )]
    pub(crate) async fn skill(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = SKILL.validate(&params)?;
        match action {
            "list" => self.skill_list_impl().await,
            "get" => self.skill_get_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "subagent",
        description = "Launch ephemeral subagents and collect their results. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `spawn` (required: prompt; optional: cli, workdir, model, effort, mcp_servers, timeout_minutes, ttl_minutes, blocking) — launch an ephemeral subagent; async by default (poll `collect`), or pass blocking: true to wait and return the result directly. \
        `collect` (required: id; optional: none) — collect a subagent result and mark it for cleanup; status is 'running' if unfinished."
    )]
    pub(crate) async fn subagent(
        &self,
        Parameters(params): Parameters<ActionObject>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = SUBAGENT.validate(&params)?;
        match action {
            "spawn" => {
                self.subagent_spawn_impl(action_params(&params)?, parts.as_ref())
                    .await
            }
            "collect" => self.subagent_collect_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "rag",
        description = "Search indexed personal content and maintain the vector store. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `search` (required: query; optional: agent_id, limit) — semantic vector search over indexed personal content; rate-limited to 10 calls/min per agent. \
        `compact` (required: none; optional: none) — compact the vector store and prune old versions."
    )]
    pub(crate) async fn rag(
        &self,
        Parameters(params): Parameters<ActionObject>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = RAG.validate(&params)?;
        match action {
            "search" => {
                self.rag_search_impl(action_params(&params)?, parts.as_ref())
                    .await
            }
            "compact" => self.rag_compact_impl().await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "identity",
        description = "Read and evolve the agent's identity, and manage seed identities. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `get` (required: none; optional: none) — read the agent's own structured identity contract. \
        `evolve` (required: none; optional: new_directives, new_traits) — suggest refinements to directives or traits from the current session's learnings. \
        `list_seeds` (required: none; optional: none) — list all seed identities. \
        `create_seed` (required: name; optional: directives, traits) — create a new seed identity. \
        `remove_seed` (required: seed_id; optional: none) — remove a seed identity by ID."
    )]
    pub(crate) async fn identity(
        &self,
        Parameters(params): Parameters<ActionObject>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = IDENTITY.validate(&params)?;
        match action {
            "get" => self.identity_get_impl(parts.as_ref()).await,
            "evolve" => {
                self.identity_evolve_impl(action_params(&params)?, parts.as_ref())
                    .await
            }
            "list_seeds" => self.identity_list_seeds_impl().await,
            "create_seed" => {
                self.identity_create_seed_impl(action_params(&params)?, parts.as_ref())
                    .await
            }
            "remove_seed" => {
                self.identity_remove_seed_impl(action_params(&params)?)
                    .await
            }
            _ => unreachable!("validated action"),
        }
    }
}
