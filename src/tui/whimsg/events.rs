//! Real events the header can announce, each carrying its subject (an agent or
//! graph name) and, where a template needs it, a detail (a spec name or node
//! id). `diff_graphs` turns two consecutive graph snapshots into the events
//! that happened between them — no daemon API, just the data the TUI already
//! refreshes.

use std::collections::HashMap;

use crate::domain::graphs::GraphStatus;

/// What happened. The kind picks the template list and the kaomoji intent
/// (failed/blocked render as errors, everything else as success).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventKind {
    AgentSpawned,
    AgentDone,
    AgentFailed,
    SpecCompleted,
    GraphCompleted,
    GraphBlocked,
    GraphFailed,
    /// CT40 wires hook delivery; the template exists so the list is complete.
    #[allow(dead_code)]
    HookMessage,
    MissionUnlocked,
}

impl EventKind {
    /// Failed/blocked events render as errors; everything else as success.
    pub(crate) fn is_error(self) -> bool {
        matches!(
            self,
            Self::AgentFailed | Self::GraphBlocked | Self::GraphFailed
        )
    }
}

/// One thing that happened, with the real names to put in its message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhimEvent {
    pub kind: EventKind,
    /// The agent or graph name (the mission title for a mission unlock).
    pub subject: String,
    /// A spec name or node id when the template needs one.
    pub detail: Option<String>,
}

impl WhimEvent {
    pub fn new(kind: EventKind, subject: impl Into<String>) -> Self {
        Self {
            kind,
            subject: subject.into(),
            detail: None,
        }
    }

    pub fn with_detail(
        kind: EventKind,
        subject: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            subject: subject.into(),
            detail: Some(detail.into()),
        }
    }
}

/// The header-relevant slice of one graph's state, captured on each refresh so
/// the next refresh can diff against it.
#[derive(Clone, Debug)]
pub struct GraphSnapshot {
    pub name: String,
    pub status: GraphStatus,
    pub blocked: bool,
    /// The node the graph last ran, for a blocked/failed `{node}` template.
    pub node: Option<String>,
    /// Completed spec names (queue specs included).
    pub done_specs: Vec<String>,
}

/// Events implied by moving from `prev` to `curr`, for graphs present in both
/// (a graph appearing for the first time is primed silently, never announced).
/// Ids are sorted so the order is deterministic.
pub fn diff_graphs(
    prev: &HashMap<String, GraphSnapshot>,
    curr: &HashMap<String, GraphSnapshot>,
) -> Vec<WhimEvent> {
    let mut ids: Vec<&String> = prev.keys().filter(|id| curr.contains_key(*id)).collect();
    ids.sort();
    let mut events = Vec::new();
    for id in ids {
        let (Some(p), Some(c)) = (prev.get(id), curr.get(id)) else {
            continue;
        };
        if p.status != GraphStatus::Completed && c.status == GraphStatus::Completed {
            events.push(WhimEvent::new(EventKind::GraphCompleted, c.name.clone()));
        }
        if p.status != GraphStatus::Failed && c.status == GraphStatus::Failed {
            events.push(graph_node_event(EventKind::GraphFailed, c));
        }
        if !p.blocked && c.blocked {
            events.push(graph_node_event(EventKind::GraphBlocked, c));
        }
        for spec in &c.done_specs {
            if !p.done_specs.contains(spec) {
                events.push(WhimEvent::with_detail(
                    EventKind::SpecCompleted,
                    c.name.clone(),
                    spec.clone(),
                ));
            }
        }
    }
    events
}

fn graph_node_event(kind: EventKind, snap: &GraphSnapshot) -> WhimEvent {
    match &snap.node {
        Some(node) => WhimEvent::with_detail(kind, snap.name.clone(), node.clone()),
        None => WhimEvent::new(kind, snap.name.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(name: &str, status: GraphStatus, blocked: bool) -> GraphSnapshot {
        GraphSnapshot {
            name: name.to_string(),
            status,
            blocked,
            node: None,
            done_specs: Vec::new(),
        }
    }

    fn map(snaps: Vec<(&str, GraphSnapshot)>) -> HashMap<String, GraphSnapshot> {
        snaps
            .into_iter()
            .map(|(id, snap)| (id.to_string(), snap))
            .collect()
    }

    #[test]
    fn new_graph_is_primed_silently() {
        let prev = map(vec![]);
        let curr = map(vec![("g1", snap("grove", GraphStatus::Running, false))]);
        assert!(diff_graphs(&prev, &curr).is_empty());
    }

    #[test]
    fn completion_transition_fires_once() {
        let prev = map(vec![("g1", snap("grove", GraphStatus::Running, false))]);
        let curr = map(vec![("g1", snap("grove", GraphStatus::Completed, false))]);
        let events = diff_graphs(&prev, &curr);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::GraphCompleted);
        assert_eq!(events[0].subject, "grove");

        // Stable state fires nothing.
        assert!(diff_graphs(&curr, &curr).is_empty());
    }

    #[test]
    fn failure_transition_carries_the_node() {
        let mut running = snap("grove", GraphStatus::Running, false);
        running.node = Some("node7".to_string());
        let mut failed = snap("grove", GraphStatus::Failed, false);
        failed.node = Some("node7".to_string());
        let events = diff_graphs(&map(vec![("g1", running)]), &map(vec![("g1", failed)]));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::GraphFailed);
        assert_eq!(events[0].detail.as_deref(), Some("node7"));
    }

    #[test]
    fn blocked_transition_fires_once() {
        let prev = map(vec![("g1", snap("grove", GraphStatus::Paused, false))]);
        let mut blocked = snap("grove", GraphStatus::Paused, true);
        blocked.node = Some("node3".to_string());
        let curr = map(vec![("g1", blocked)]);
        let events = diff_graphs(&prev, &curr);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::GraphBlocked);
        assert_eq!(events[0].detail.as_deref(), Some("node3"));
        assert!(diff_graphs(&curr, &curr).is_empty());
    }

    #[test]
    fn new_done_specs_fire_spec_completed() {
        let mut prev = snap("grove", GraphStatus::Running, false);
        prev.done_specs = vec!["CT31".to_string()];
        let mut curr = snap("grove", GraphStatus::Running, false);
        curr.done_specs = vec![
            "CT31".to_string(),
            "CT32 minimal prompt protocol".to_string(),
        ];
        let events = diff_graphs(&map(vec![("g1", prev)]), &map(vec![("g1", curr)]));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::SpecCompleted);
        assert_eq!(events[0].subject, "grove");
        assert_eq!(
            events[0].detail.as_deref(),
            Some("CT32 minimal prompt protocol")
        );
    }
}
