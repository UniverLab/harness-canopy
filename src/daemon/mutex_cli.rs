//! CLI handlers for `canopy mutex` subcommands.
//!
//! `list` is read-only: it opens the local database directly, exactly like
//! `canopy agent show` (see `agent_cli.rs`), and never requires the daemon.

use anyhow::Result;
use clap::Subcommand;

use crate::db::mutex_store::MutexState;
use crate::db::Database;
use crate::domain::db_paths::database_path;

#[derive(Subcommand, Debug)]
pub(crate) enum MutexAction {
    /// List granted mutex leases and queued waiters (FIFO).
    List {
        /// Print the raw JSON shape instead of human-readable lines.
        #[arg(long)]
        json: bool,
    },
}

pub async fn handle_mutex_action(action: MutexAction) -> Result<()> {
    match action {
        MutexAction::List { json } => {
            let data_dir = crate::ensure_data_dir()?;
            let db = Database::new_safe(&database_path(&data_dir), &data_dir)?;
            let state = db.list_mutex_state()?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "leases": state.leases,
                        "waiters": state.waiters,
                    }))?
                );
                return Ok(());
            }
            for line in render_mutex_state(&state) {
                println!("{line}");
            }
            Ok(())
        }
    }
}

/// Human-readable lines for `mutex list`: leases first, then FIFO waiters.
fn render_mutex_state(state: &MutexState) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push("Leases:".to_string());
    if state.leases.is_empty() {
        lines.push("  (none)".to_string());
    }
    for lease in &state.leases {
        lines.push(format!(
            "  {}: graph {} node '{}' (run {}, since {})",
            lease.resource, lease.graph_id, lease.node_name, lease.holder_run_id, lease.acquired_at,
        ));
    }
    lines.push("Waiters (FIFO):".to_string());
    if state.waiters.is_empty() {
        lines.push("  (none)".to_string());
    }
    for waiter in &state.waiters {
        lines.push(format!(
            "  graph {} node '{}' (run {}) needs [{}] since {}",
            waiter.graph_id,
            waiter.node_name,
            waiter.run_id,
            waiter.resources.join(", "),
            waiter.requested_at,
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::mutex_store::{MutexLease, MutexWaiter};

    #[test]
    fn render_mutex_state_names_holders_and_waiters() {
        let state = MutexState {
            leases: vec![MutexLease {
                resource: "cpu".to_string(),
                holder_run_id: "run-1".to_string(),
                graph_id: "graph-1".to_string(),
                node_name: "gates".to_string(),
                acquired_at: "2026-10-27T00:00:00Z".to_string(),
            }],
            waiters: vec![MutexWaiter {
                run_id: "run-2".to_string(),
                graph_id: "graph-2".to_string(),
                node_name: "other gates".to_string(),
                resources: vec!["cpu".to_string()],
                requested_at: "2026-10-27T00:01:00Z".to_string(),
            }],
        };
        let lines = render_mutex_state(&state);
        assert!(lines.iter().any(|line| line.contains("cpu")
            && line.contains("graph-1")
            && line.contains("gates")));
        assert!(lines.iter().any(|line| line.contains("graph-2")
            && line.contains("other gates")
            && line.contains("cpu")));
    }

    #[test]
    fn render_mutex_state_reports_empty() {
        let lines = render_mutex_state(&MutexState::default());
        assert_eq!(
            lines.iter().filter(|line| line.contains("(none)")).count(),
            2
        );
    }
}
