//! CM25: the graph engine's own narration into the shared activity stream.
//! CM46: the same call now also writes a durable per-graph event row.
//!
//! One place that formats and writes an engine-authored entry, so every call
//! site in `graph_engine.rs` / `cron_scheduler.rs` looks the same. [`record`]
//! writes both surfaces — the `graph_events` history and the shared
//! `activity_log` feed — so they cannot drift.

use serde_json::Value;

use crate::db::Database;
use crate::domain::sync::MessageKind;

/// The kind of a durable graph event. The string values are the vocabulary
/// the `graph_events` CHECK constraint enforces (CM46).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphEventKind {
    GraphStarted,
    GraphPaused,
    GraphResumed,
    GraphCompleted,
    GraphFailed,
    GraphBlocked,
    SpecStarted,
    SpecCompleted,
    SpecFailed,
    SpecSkipped,
    NodeStarted,
    NodeFinished,
    HookFired,
    ConfigChanged,
    /// CM47's `spec_note_write` constructs this; the CHECK constraint and this
    /// vocabulary are fixed now so the two never drift.
    #[allow(dead_code)]
    Note,
}

impl GraphEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GraphStarted => "graph_started",
            Self::GraphPaused => "graph_paused",
            Self::GraphResumed => "graph_resumed",
            Self::GraphCompleted => "graph_completed",
            Self::GraphFailed => "graph_failed",
            Self::GraphBlocked => "graph_blocked",
            Self::SpecStarted => "spec_started",
            Self::SpecCompleted => "spec_completed",
            Self::SpecFailed => "spec_failed",
            Self::SpecSkipped => "spec_skipped",
            Self::NodeStarted => "node_started",
            Self::NodeFinished => "node_finished",
            Self::HookFired => "hook_fired",
            Self::ConfigChanged => "config_changed",
            Self::Note => "note",
        }
    }

    /// Whether this kind also narrates into the shared activity stream.
    ///
    /// CM25 established that stream's visible behaviour; `record` must not
    /// change it. Only the kinds that published there before CM46 still do —
    /// the CM46-only kinds (node starts, spec skips, config edits, notes) live
    /// in `graph_events` alone.
    pub fn emits_activity(self) -> bool {
        !matches!(
            self,
            Self::NodeStarted | Self::SpecSkipped | Self::ConfigChanged | Self::Note
        )
    }
}

/// One engine-authored event, borrow-heavy so a call site can name ids it
/// already has in scope without cloning.
pub struct GraphEvent<'a> {
    pub graph_id: &'a str,
    pub graph_name: &'a str,
    pub workdir: &'a str,
    pub spec_id: Option<&'a str>,
    pub node_id: Option<&'a str>,
    pub run_id: Option<&'a str>,
    pub kind: GraphEventKind,
    pub message: String,
    pub payload: Option<Value>,
}

/// The owned, DB-ready form of a [`GraphEvent`]: ids cloned, `kind` rendered
/// to its stored string, `payload` serialized to JSON.
pub struct GraphEventRecord {
    pub graph_id: String,
    pub spec_id: Option<String>,
    pub node_id: Option<String>,
    pub run_id: Option<String>,
    pub kind: String,
    pub message: String,
    pub payload: Option<String>,
}

impl From<GraphEvent<'_>> for GraphEventRecord {
    fn from(event: GraphEvent<'_>) -> Self {
        Self {
            graph_id: event.graph_id.to_string(),
            spec_id: event.spec_id.map(str::to_string),
            node_id: event.node_id.map(str::to_string),
            run_id: event.run_id.map(str::to_string),
            kind: event.kind.as_str().to_string(),
            message: event.message,
            payload: event
                .payload
                .and_then(|value| serde_json::to_string(&value).ok()),
        }
    }
}

/// Record one engine-authored event.
///
/// Writes the durable `graph_events` row and the shared activity-stream line.
/// Never fails the caller: either write failing is a WARN, not a propagated
/// error — a graph run must never be affected by this (NFR, CM25/CM46).
pub fn record(db: &Database, event: GraphEvent<'_>) {
    let activity_msg = event.message.clone();
    let graph_id = event.graph_id.to_string();
    let graph_name = event.graph_name.to_string();
    let workdir = event.workdir.to_string();
    let kind = event.kind;

    if let Err(error) = db.insert_graph_event(&GraphEventRecord::from(event)) {
        tracing::warn!(
            graph_id,
            kind = kind.as_str(),
            "graph_event record failed: {:#}",
            error
        );
    }
    // CM25: only the kinds that already narrated there do so now — the
    // CM46-only kinds stay out of the activity feed (its visible behaviour is
    // unchanged).
    if !kind.emits_activity() {
        return;
    }
    if let Err(error) = db.insert_activity_log_entry(
        &workdir,
        &format!("graph:{graph_name}"),
        Some(&graph_id),
        MessageKind::Info.as_str(),
        &activity_msg,
        None,
    ) {
        tracing::warn!(graph_id, graph_name, "activity publish failed: {:#}", error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    #[test]
    fn record_writes_both_surfaces_with_identical_message() {
        let db = test_db();
        record(
            &db,
            GraphEvent {
                graph_id: "g1",
                graph_name: "Graph One",
                workdir: "/tmp/cm46",
                spec_id: Some("s1"),
                node_id: None,
                run_id: None,
                kind: GraphEventKind::SpecStarted,
                message: "Spec 'one' started.".to_string(),
                payload: None,
            },
        );

        let events = db.list_graph_events("g1", None, None, None, 200).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "spec_started");
        assert_eq!(events[0].spec_id.as_deref(), Some("s1"));
        assert_eq!(events[0].message, "Spec 'one' started.");

        let activity = db.list_activity_log_entries("/tmp/cm46", 10).unwrap();
        assert_eq!(activity.len(), 1);
        assert_eq!(activity[0].message, "Spec 'one' started.");
        assert_eq!(activity[0].source, "graph:Graph One");
        assert_eq!(activity[0].source_id.as_deref(), Some("g1"));
    }
}
