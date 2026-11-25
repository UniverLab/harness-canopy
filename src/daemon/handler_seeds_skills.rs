//! Skill MCP handler tools split out of `daemon::handler`, plus the
//! redaction helpers for stored node-run input/output.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::params::*;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::ErrorData as McpError;
use std::sync::Arc;

impl TaskTriggerHandler {
    pub(crate) async fn skill_list_impl(&self) -> Result<CallToolResult, McpError> {
        let store = Arc::clone(&self.dynamic_skills);
        let entries = tokio::task::spawn_blocking(move || store.list())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&entries).unwrap_or_default(),
        )]))
    }

    pub(crate) async fn skill_get_impl(
        &self,
        params: SkillGetParams,
    ) -> Result<CallToolResult, McpError> {
        let store = Arc::clone(&self.dynamic_skills);
        let content = tokio::task::spawn_blocking(move || store.get(&params.name))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
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
/// `output` before it crosses the MCP boundary via `graph`.
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
