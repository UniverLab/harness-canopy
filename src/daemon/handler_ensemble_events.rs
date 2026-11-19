//! Live ensemble-change narration: the `ensemble_changed` graph event.
//!
//! Split out of `handler_ensemble_update` so the kind/crew change path and
//! the generic config-change path each own one narration helper. A kind or
//! crew change accepted on a running graph writes one `ensemble_changed`
//! event (carrying the old/new kind and crew) plus one activity-log line;
//! on an idle graph — or when neither field actually changed — it is a
//! no-op, exactly like `record_config_change_if_running`.
use crate::db::Database;
use crate::domain::activity::{self, GraphEvent, GraphEventKind};
use crate::domain::graphs::{Ensemble, GraphStatus};

/// Record one `ensemble_changed` event for a kind/crew change accepted
/// while the owning graph runs. No-op unless the graph resolves and is
/// `running` — an edit on an idle graph is not "a change made while the
/// graph runs", which is what this narrates.
pub(crate) fn record_ensemble_change_if_running(
    db: &Database,
    ensemble: &Ensemble,
    old_kind: &str,
    new_kind: &str,
    old_crew: Option<&str>,
    new_crew: Option<&str>,
) {
    let owning_graph_id = ensemble.graph_id.clone().or_else(|| {
        ensemble.spec_id.as_deref().and_then(|sid| {
            db.get_graph_spec(sid)
                .ok()
                .flatten()
                .and_then(|spec| spec.graph_id)
        })
    });
    let Some(graph_id) = owning_graph_id else {
        return;
    };
    let Ok(Some(graph)) = db.get_graph(&graph_id) else {
        return;
    };
    if graph.status != GraphStatus::Running {
        return;
    }
    let message = match (old_crew, new_crew) {
        (Some(old), Some(new)) if old != new => format!(
            "Ensemble '{}' changed crew from '{old}' to '{new}' (kind {}).",
            ensemble.name, new_kind,
        ),
        (None, Some(new)) => format!(
            "Ensemble '{}' bound to crew '{new}' (kind {}).",
            ensemble.name, new_kind,
        ),
        (Some(old), None) => format!(
            "Ensemble '{}' unbound from crew '{old}' (kind {}).",
            ensemble.name, new_kind,
        ),
        _ => format!(
            "Ensemble '{}' changed kind from {old_kind} to {new_kind}.",
            ensemble.name,
        ),
    };
    activity::record(
        db,
        GraphEvent {
            graph_id: &graph_id,
            graph_name: &graph.name,
            workdir: &graph.workdir,
            spec_id: ensemble.spec_id.as_deref(),
            node_id: None,
            run_id: None,
            kind: GraphEventKind::EnsembleChanged,
            message,
            payload: Some(serde_json::json!({
                "old_kind": old_kind,
                "new_kind": new_kind,
                "old_crew": old_crew,
                "new_crew": new_crew,
            })),
        },
    );
}
