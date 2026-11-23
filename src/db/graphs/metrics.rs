use super::*;

/// Narrow row for metrics aggregation: never loads `input`/`output`.
#[derive(Debug, Clone)]
pub struct GraphMetricsRunRow {
    pub graph_id: String,
    pub node_id: String,
    pub status: String,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub executed_platform: Option<String>,
    pub executed_model: Option<String>,
}

pub const GRAPH_METRICS_SQL: &str = "SELECT graph_id, node_id, status, started_at, completed_at, executed_platform, executed_model \
     FROM graph_runs WHERE started_at >= ?1 AND started_at < ?2";

impl Database {
    pub fn list_graph_metrics_rows(
        &self,
        since_ts: i64,
        until_ts: i64,
        graph_id: Option<&str>,
    ) -> Result<Vec<GraphMetricsRunRow>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<GraphMetricsRunRow> {
            Ok(GraphMetricsRunRow {
                graph_id: row.get(0)?,
                node_id: row.get(1)?,
                status: row.get(2)?,
                started_at: row.get(3)?,
                completed_at: row.get(4)?,
                executed_platform: row.get(5)?,
                executed_model: row.get(6)?,
            })
        };
        if let Some(gid) = graph_id {
            let sql = format!("{GRAPH_METRICS_SQL} AND graph_id = ?3");
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params![since_ts, until_ts, gid], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        } else {
            let mut stmt = conn.prepare(GRAPH_METRICS_SQL)?;
            let rows = stmt
                .query_map(params![since_ts, until_ts], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }
    }
}
