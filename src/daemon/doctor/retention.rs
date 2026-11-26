use super::*;

/// Row counts for the tables the retention pass manages, with the active
/// retention settings — so growth without bound reads as numbers, not a
/// feeling.
pub(crate) fn report_retention_counts(db: &Database, canopy_dir: &Path) {
    let config = crate::domain::canopy_config::CanopyConfig::load(canopy_dir);
    match db.table_row_counts() {
        Ok(counts) => println!(
            " Retention: graph_runs: {}, interactive_sessions: {}, terminal_sessions: {} \
             (retention.graph_runs_days: {}, clean.retention_days: {})",
            counts.graph_runs,
            counts.interactive_sessions,
            counts.terminal_sessions,
            config.retention.graph_runs_days,
            config.clean.retention_days
        ),
        Err(e) => println!(" \x1b[33m⚠\x1b[0m Could not read retention row counts: {e}"),
    }
}
