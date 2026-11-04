//! Formatting of an agent's delivery action (CM53) for the `agent get`,
//! `agent list` and `agent status` surfaces.

use crate::domain::models::Agent;

/// A human description of how an agent fires.
pub(crate) struct ActionDescription {
    /// "headless" or "send to session".
    pub label: &'static str,
    /// "target: session name 'x'" / "target: session id 'x'"; `None` for
    /// headless agents.
    pub target: Option<String>,
}

/// Append the `agent status` section naming every send-to-session agent and
/// its target, so status shows the delivery action too (CM53).
pub(crate) fn append_send_session_section(status: &mut String, agents: &[Agent]) {
    let send: Vec<&Agent> = agents.iter().filter(|a| a.is_send_to_session()).collect();
    if send.is_empty() {
        return;
    }
    status.push_str(&format!("\nSend-to-session agents ({}):", send.len()));
    for agent in send {
        let description = describe_action(agent);
        status.push_str(&format!(
            "\n  - {} -> {}",
            agent.id,
            description.target.unwrap_or_default()
        ));
    }
}

pub(crate) fn describe_action(agent: &Agent) -> ActionDescription {
    if let Some(name) = agent.action.target_session_name() {
        return ActionDescription {
            label: "send to session",
            target: Some(format!("target: session name '{name}'")),
        };
    }
    if let Some(id) = agent.action.target_session_id() {
        return ActionDescription {
            label: "send to session",
            target: Some(format!("target: session id '{id}'")),
        };
    }
    ActionDescription {
        label: "headless",
        target: None,
    }
}
