//! Structured origin of a scheduled send, split out of
//! `db::scheduled_prompts` so that module stays within the size gate.
//! Behaviour unchanged: the JSON shape is append-only — `node_id`/`run_id`
//! default and skip when absent, so rows written before the deliver node
//! existed still deserialize.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// Structured origin of a scheduled send, stored as JSON in the nullable
/// `provenance` column. Kept out of `prompt` so the delivered text stays
/// exactly what the promptbuilder would submit while the recipient can still
/// tell which graph, event, or agent sent it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledPromptProvenance {
    /// Provenance kind: `"hook"`, `"agent"`, `"agent_trigger"`,
    /// `"spec_review"`, or `"deliver"`.
    pub kind: String,
    /// Id of the graph whose hook or deliver node enqueued the send. Empty
    /// for `"agent"`.
    pub graph_id: String,
    /// Hook event that produced it (e.g. `"on_failed"`). Empty for `"agent"`
    /// and `"deliver"`.
    pub event: String,
    /// Id of the calling session that scheduled an agent send.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Id of the background agent whose trigger enqueued the send (CM53).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// The trigger that fired, `"cron"` or `"watch"` (CM53).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// The session *name* an interactive hook was aimed at (CM27), when the
    /// hook was configured with `target_session_name` rather than a literal
    /// id — the id it actually resolved to and delivered to is the row's own
    /// `target_session_id` column, not duplicated here. `None` for id-hooks
    /// and for agent-scheduled sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_session_name: Option<String>,
    /// Id of the deliver node that enqueued the send. `None` for every
    /// other provenance kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Id of the node run that enqueued the send. `None` for every other
    /// provenance kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

impl ScheduledPromptProvenance {
    /// Provenance for a message enqueued by a graph hook.
    pub fn hook(graph_id: &str, event: &str) -> Self {
        Self {
            kind: "hook".to_string(),
            graph_id: graph_id.to_string(),
            event: event.to_string(),
            session_id: None,
            agent_id: None,
            trigger: None,
            target_session_name: None,
            node_id: None,
            run_id: None,
        }
    }

    /// Provenance for a message enqueued by a name-targeted interactive
    /// hook (CM27) — same as `hook`, plus the name it was aimed at.
    pub fn hook_with_target_name(graph_id: &str, event: &str, target_session_name: &str) -> Self {
        Self {
            kind: "hook".to_string(),
            graph_id: graph_id.to_string(),
            event: event.to_string(),
            session_id: None,
            agent_id: None,
            trigger: None,
            target_session_name: Some(target_session_name.to_string()),
            node_id: None,
            run_id: None,
        }
    }

    /// Provenance for a message an agent scheduled via the MCP surface.
    pub fn agent(session_id: &str) -> Self {
        Self {
            kind: "agent".to_string(),
            graph_id: String::new(),
            event: String::new(),
            session_id: Some(session_id.to_string()),
            agent_id: None,
            trigger: None,
            target_session_name: None,
            node_id: None,
            run_id: None,
        }
    }

    /// Provenance for a message a background agent's cron/watch trigger
    /// delivered (CM53). `trigger` is `"cron"` or `"watch"`.
    pub fn agent_trigger(agent_id: &str, trigger: &str) -> Self {
        Self {
            kind: "agent_trigger".to_string(),
            graph_id: String::new(),
            event: String::new(),
            session_id: None,
            agent_id: Some(agent_id.to_string()),
            trigger: Some(trigger.to_string()),
            target_session_name: None,
            node_id: None,
            run_id: None,
        }
    }

    /// Provenance for a batched spec-review drain delivered to the session
    /// that created the reviewed specs.
    pub fn spec_review() -> Self {
        Self {
            kind: "spec_review".to_string(),
            graph_id: String::new(),
            event: String::new(),
            session_id: None,
            agent_id: None,
            trigger: None,
            target_session_name: None,
            node_id: None,
            run_id: None,
        }
    }

    /// Provenance for a message enqueued by a deliver node — the node form
    /// of a `SendToSession` agent fire. Carries the graph, node and run ids
    /// so a pending send traces back to the exact node run that queued it.
    pub fn deliver(graph_id: &str, node_id: &str, run_id: &str) -> Self {
        Self {
            kind: "deliver".to_string(),
            graph_id: graph_id.to_string(),
            event: String::new(),
            session_id: None,
            agent_id: None,
            trigger: None,
            target_session_name: None,
            node_id: Some(node_id.to_string()),
            run_id: Some(run_id.to_string()),
        }
    }

    pub(crate) fn from_json(raw: Option<String>) -> Result<Option<Self>> {
        raw.map(|json| serde_json::from_str(&json).map_err(|e| anyhow!("{e}")))
            .transpose()
    }

    pub(crate) fn to_json(value: Option<&Self>) -> Result<Option<String>> {
        value
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| anyhow!("{e}"))
    }
}
