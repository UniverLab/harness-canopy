//! CM25: the graph engine's own narration into the shared activity stream.
//!
//! One place that formats and writes an engine-authored activity entry, so
//! every call site in `graph_engine.rs` / `cron_scheduler.rs` looks the same
//! and the message wording lives in one file.

use crate::db::Database;
use crate::domain::sync::MessageKind;

/// Publish one activity entry attributed to a running graph.
///
/// Never fails the caller: a broken activity insert is a WARN, not a
/// propagated error — the graph run must never be affected by this write
/// (NFR, CM25).
pub fn publish(
    db: &Database,
    workdir: &str,
    loop_id: &str,
    loop_name: &str,
    message: impl AsRef<str>,
) {
    let message = message.as_ref();
    if let Err(error) = db.insert_activity_log_entry(
        workdir,
        &format!("graph:{loop_name}"),
        Some(loop_id),
        MessageKind::Info.as_str(),
        message,
        None,
    ) {
        tracing::warn!(loop_id, loop_name, "activity publish failed: {:#}", error);
    }
}
