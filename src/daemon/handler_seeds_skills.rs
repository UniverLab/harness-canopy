//! MCP handler tools split out of `daemon::handler` (identity/seed/skill tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::application::ports::StateRepository;
use crate::daemon::handler::{OptionalExtension, TaskTriggerHandler};
use crate::daemon::handler_helpers::load_bound_seed_identity;
use crate::daemon::helpers::success_result;
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use axum::http::request::Parts;
use rmcp::model::{CallToolResult, Content};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

#[tool_router(router = tool_router_seeds_skills, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "get_identity",
        description = "Read the agent's own structured identity contract (identity.toml). \
         Returns the full TOML content including name, directives, and traits."
    )]
    pub(crate) async fn get_identity(
        &self,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;
        let agent_id = self.resolve_sync_agent_id(parts.as_ref())?;
        let (_, identity) = load_bound_seed_identity(&self.db, parts.as_ref(), &agent_id)
            .map_err(|e| McpError::invalid_params(e, None))?;
        let toml_str = identity
            .to_toml()
            .map_err(|e| McpError::internal_error(e, None))?;
        Ok(CallToolResult::success(vec![Content::text(toml_str)]))
    }

    #[tool(
        name = "evolve_identity",
        description = "Suggest refinements to the agent's directives or traits based on \
         the current session's learnings. The Daemon validates updates against the schema \
         and the 4KB size cap, then overwrites the global identity.toml."
    )]
    pub(crate) async fn evolve_identity(
        &self,
        Parameters(params): Parameters<EvolveIdentityParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;
        let agent_id = self.resolve_sync_agent_id(parts.as_ref())?;
        let (seed_id, mut identity) = load_bound_seed_identity(&self.db, parts.as_ref(), &agent_id)
            .map_err(|e| McpError::invalid_params(e, None))?;
        identity
            .evolve(params.new_directives, params.new_traits)
            .map_err(|e| McpError::invalid_params(e, None))?;
        crate::domain::seeds::save_seed(&seed_id, &identity)
            .map_err(|e| McpError::internal_error(e, None))?;
        let _ = self.db.set_state("gamification:identity_evolved", "1");
        Ok(success_result("Identity evolved and saved successfully."))
    }

    #[tool(
        name = "list_seeds",
        description = "List all existing seed identities. Returns seed IDs and their names."
    )]
    pub(crate) async fn list_seeds(&self) -> Result<CallToolResult, McpError> {
        let seed_ids =
            crate::domain::seeds::list_seeds().map_err(|e| McpError::internal_error(e, None))?;
        let mut seeds_info = Vec::new();
        for seed_id in &seed_ids {
            if let Ok(identity) = crate::domain::seeds::load_seed(seed_id) {
                seeds_info.push(serde_json::json!({
                    "id": seed_id,
                    "name": identity.name,
                }));
            }
        }
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&seeds_info).unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "create_seed",
        description = "Create a new seed identity. Validates name uniqueness, structure, and 4KB size cap."
    )]
    pub(crate) async fn create_seed(
        &self,
        Parameters(params): Parameters<CreateSeedParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;
        let seed_id = crate::domain::nursery::slugify(&params.name);
        let identity = crate::domain::seeds::SeedIdentity {
            name: params.name,
            created_at: chrono::Utc::now(),
            directives: params.directives.unwrap_or_default(),
            traits: params.traits.unwrap_or_default(),
        };
        crate::domain::seeds::save_seed(&seed_id, &identity)
            .map_err(|e| McpError::invalid_params(e, None))?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Seed '{seed_id}' created successfully."
        ))]))
    }

    #[tool(
        name = "remove_seed",
        description = "Remove a seed identity completely by ID."
    )]
    pub(crate) async fn remove_seed(
        &self,
        Parameters(params): Parameters<RemoveSeedParams>,
    ) -> Result<CallToolResult, McpError> {
        crate::domain::seeds::remove_seed(&params.seed_id)
            .map_err(|e| McpError::invalid_params(e, None))?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Seed '{}' removed successfully.",
            params.seed_id
        ))]))
    }

    #[tool(
        name = "skill_list",
        description = "List skills available to this agent from the dynamic skill store — the \
         union of skills already fetched into ~/.canopy/skills/ and the catalogs of every \
         git source configured under [skills] in ~/.canopy/config.toml. Each entry reports \
         name, one-line description, source URL, and whether it is already installed \
         (fetched locally, served instantly) or merely available (fetch it on demand with \
         skill_get, which clones it into the store transparently). Call this first to \
         discover what's available before deciding which skill_get to call — this system \
         works across every harness, not just one platform's local skills folder."
    )]
    pub(crate) async fn skill_list(&self) -> Result<CallToolResult, McpError> {
        let store = Arc::clone(&self.dynamic_skills);
        let entries = tokio::task::spawn_blocking(move || store.list())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&entries).unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "skill_get",
        description = "Fetch a skill's full instructions by name from the dynamic skill \
         store. If the skill isn't in the local store yet, clones it on demand from its \
         configured git source; if it is present but its TTL has expired, transparently \
         checks the source for updates and refreshes the local copy first (network failures \
         during that check are non-fatal — the last-known-good copy is served). Returns the \
         SKILL.md/INSTRUCTIONS.md instructions plus any reference files (inlined when small, \
         otherwise listed by store path). Call skill_list first to find valid skill names."
    )]
    pub(crate) async fn skill_get(
        &self,
        Parameters(params): Parameters<SkillGetParams>,
    ) -> Result<CallToolResult, McpError> {
        let store = Arc::clone(&self.dynamic_skills);
        let content = tokio::task::spawn_blocking(move || store.get(&params.name))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&content).unwrap_or_default(),
        )]))
    }
}

/// Secret-shaped substrings a node run's stored input/output can carry
/// through whatever CLI it wrapped (an API key echoed into a shell command,
/// a leaked token in stderr, a pasted private key). Compiled once and
/// applied uniformly to every string leaf in the run's JSON — see
/// [`redact_sensitive_value`] — rather than to specific fields like `stdout`
/// or `command`, since sensitive content can land in any of them alike and a
/// per-field allowlist would miss the next field name that carries it.
pub(crate) static SECRET_PATTERNS: std::sync::LazyLock<Vec<regex::Regex>> =
    std::sync::LazyLock::new(|| {
        [
        // Anthropic/OpenAI-style API keys: sk-..., sk-ant-...
        r"sk-[A-Za-z0-9_-]{16,}",
        // GitHub personal/app/oauth/refresh tokens.
        r"gh[pousr]_[A-Za-z0-9]{20,}",
        // AWS access key IDs.
        r"AKIA[0-9A-Z]{16}",
        // Bearer tokens in an Authorization header or similar.
        r"(?i)bearer\s+[A-Za-z0-9._-]{16,}",
        // key/value assignments: api_key=..., "password": "...", token: ...
        r#"(?i)(api[_-]?key|secret|password|access[_-]?token|token)["']?\s*[:=]\s*["']?[A-Za-z0-9._\-/+]{8,}"#,
        // PEM private key blocks.
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
    ]
    .iter()
    .map(|pattern| regex::Regex::new(pattern).expect("valid redaction regex"))
    .collect()
    });

pub(crate) fn redact_secrets(text: &str) -> String {
    let mut result = text.to_string();
    for pattern in SECRET_PATTERNS.iter() {
        result = pattern.replace_all(&result, "[REDACTED]").into_owned();
    }
    result
}

/// Recursively redacts every string leaf of a node run's stored `input`/
/// `output` before it crosses the MCP boundary via `graph_node_run_get`.
pub(crate) fn redact_sensitive_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => serde_json::Value::String(redact_secrets(s)),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(redact_sensitive_value).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), redact_sensitive_value(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(crate) fn inject_seed_identity(
    out: &mut serde_json::Value,
    db: &crate::db::Database,
    parts: Option<&axum::http::request::Parts>,
    agent_id: Option<&str>,
) {
    let Some(agent_id) = agent_id else {
        return;
    };
    if let Ok((_, identity)) = load_bound_seed_identity(db, parts, agent_id) {
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "seed_identity".to_string(),
                serde_json::json!({
                    "name": identity.name,
                    "directives": identity.directives.general,
                    "traits": serde_json::json!({
                        "tone": identity.traits.tone,
                        "focus": identity.traits.focus,
                    }),
                    "prompt_injection": identity.prompt_injection(),
                }),
            );
        }
    }
}
