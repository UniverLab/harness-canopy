//! Database queries supporting gamification mission checks.

use anyhow::Result;

use crate::application::ports::StateRepository;
use crate::db::Database;

/// `daemon_state` key holding the comma-separated set of project hashes whose
/// description or tags were changed (The Gardener mission).
pub const GARDENER_PROJECTS_KEY: &str = "gamification:gardener_projects";

impl Database {
    pub fn count_intelligence_nodes(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM intelligence_nodes", [], |r| r.get(0))?;
        Ok(count)
    }

    pub fn count_cross_project_intelligence_links(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM intelligence_edges e
             JOIN intelligence_nodes fn ON fn.id = e.from_node_id
             JOIN intelligence_nodes tn ON tn.id = e.to_node_id
             WHERE fn.project_hash IS NOT NULL
               AND tn.project_hash IS NOT NULL
               AND fn.project_hash != tn.project_hash",
            [],
            |r| r.get(0),
        )?;
        Ok(count)
    }

    pub fn count_graph_node_runs(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM graph_runs", [], |r| r.get(0))?;
        Ok(count)
    }

    pub fn count_completed_graphs(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM graphs WHERE status = 'completed'",
            [],
            |r| r.get(0),
        )?;
        Ok(count)
    }

    pub fn max_graph_nodes_in_any_graph(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let max: i64 = conn.query_row(
            "SELECT COALESCE(MAX(node_count), 0) FROM (
                SELECT ws.graph_id, COUNT(wn.id) AS node_count
                FROM graph_specs ws
                JOIN graph_nodes wn ON wn.spec_id = ws.id
                GROUP BY ws.graph_id
             )",
            [],
            |r| r.get(0),
        )?;
        Ok(max.max(0) as usize)
    }

    /// True when at least one completed node run belongs to a member of a
    /// `parallel` ensemble. The `parallelizable` spec flag alone does not
    /// count — the engine never acts on it.
    pub fn has_parallel_graph_run(&self) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM graph_runs wr
             JOIN ensemble_members em ON em.node_id = wr.node_id
             JOIN ensembles e ON e.id = em.ensemble_id
             WHERE e.kind = 'parallel' AND wr.status = 'success'",
            [],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn max_seed_session_bindings(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let max: i64 = conn.query_row(
            "SELECT COALESCE(MAX(cnt), 0) FROM (
                SELECT COUNT(*) AS cnt FROM seed_sessions GROUP BY seed_id
             )",
            [],
            |r| r.get(0),
        )?;
        Ok(max)
    }

    /// Record one Gardener project: add `project_hash` to the deduplicated
    /// comma-separated set at `GARDENER_PROJECTS_KEY`. Re-adding the same
    /// project does not grow the set.
    pub fn record_gardener_project(&self, project_hash: &str) -> Result<()> {
        let current = self.get_state(GARDENER_PROJECTS_KEY)?.unwrap_or_default();
        let mut known: Vec<&str> = current
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if !known.contains(&project_hash) {
            known.push(project_hash);
            self.set_state(GARDENER_PROJECTS_KEY, &known.join(","))?;
        }
        Ok(())
    }

    /// How many distinct projects have had their description or tags changed.
    pub fn count_gardener_projects(&self) -> Result<u64> {
        let current = self.get_state(GARDENER_PROJECTS_KEY)?.unwrap_or_default();
        Ok(current
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .count() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_db() -> Database {
        let dir = tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    #[test]
    fn count_intelligence_nodes_empty() {
        let db = test_db();
        let count = db.count_intelligence_nodes().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn count_cross_project_intelligence_links_empty() {
        let db = test_db();
        let count = db.count_cross_project_intelligence_links().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn count_graph_node_runs_empty() {
        let db = test_db();
        let count = db.count_graph_node_runs().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn count_completed_graphs_empty() {
        let db = test_db();
        let count = db.count_completed_graphs().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn max_graph_nodes_in_any_graph_empty() {
        let db = test_db();
        let max = db.max_graph_nodes_in_any_graph().unwrap();
        assert_eq!(max, 0);
    }

    #[test]
    fn max_seed_session_bindings_empty() {
        let db = test_db();
        let max = db.max_seed_session_bindings().unwrap();
        assert_eq!(max, 0);
    }

    #[test]
    fn count_intelligence_nodes_with_data() {
        let db = test_db();
        let conn = db.conn.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        conn.execute(
            "INSERT INTO intelligence_nodes (id, kind, title, body, created_at, updated_at) VALUES (?1, 'fact', 'Test', 'Body', ?2, ?2)",
            rusqlite::params!["node1", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO intelligence_nodes (id, kind, title, body, created_at, updated_at) VALUES (?1, 'pattern', 'Test2', 'Body2', ?2, ?2)",
            rusqlite::params!["node2", now],
        )
        .unwrap();
        drop(conn);
        assert_eq!(db.count_intelligence_nodes().unwrap(), 2);
    }

    #[test]
    fn count_cross_project_intelligence_links_with_data() {
        let db = test_db();
        let conn = db.conn.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        conn.execute(
            "INSERT INTO intelligence_nodes (id, kind, title, body, project_hash, created_at, updated_at) VALUES (?1, 'fact', 'N1', 'B1', ?2, ?3, ?3)",
            rusqlite::params!["n1", "hash_a", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO intelligence_nodes (id, kind, title, body, project_hash, created_at, updated_at) VALUES (?1, 'fact', 'N2', 'B2', ?2, ?3, ?3)",
            rusqlite::params!["n2", "hash_b", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO intelligence_edges (from_node_id, to_node_id, relation, created_at) VALUES (?1, ?2, 'relates_to', ?3)",
            rusqlite::params!["n1", "n2", now],
        )
        .unwrap();
        drop(conn);
        assert_eq!(db.count_cross_project_intelligence_links().unwrap(), 1);
    }

    #[test]
    fn count_cross_project_same_project_not_counted() {
        let db = test_db();
        let conn = db.conn.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        conn.execute(
            "INSERT INTO intelligence_nodes (id, kind, title, body, project_hash, created_at, updated_at) VALUES (?1, 'fact', 'N1', 'B1', ?2, ?3, ?3)",
            rusqlite::params!["n1", "hash_a", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO intelligence_nodes (id, kind, title, body, project_hash, created_at, updated_at) VALUES (?1, 'fact', 'N2', 'B2', ?2, ?3, ?3)",
            rusqlite::params!["n2", "hash_a", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO intelligence_edges (from_node_id, to_node_id, relation, created_at) VALUES (?1, ?2, 'relates_to', ?3)",
            rusqlite::params!["n1", "n2", now],
        )
        .unwrap();
        drop(conn);
        assert_eq!(db.count_cross_project_intelligence_links().unwrap(), 0);
    }

    #[test]
    fn count_graph_node_runs_with_data() {
        let db = test_db();
        let conn = db.conn.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // Insert a graph
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES (?1, 'test', '/tmp', 'draft', ?2)",
            rusqlite::params!["loop1", now],
        )
        .unwrap();
        // Insert a spec
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES (?1, ?2, 'spec1', 0, 0, 'pending')",
            rusqlite::params!["spec1", "loop1"],
        )
        .unwrap();
        // Insert a node (spec_id non-null, graph_id null)
        conn.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at) VALUES (?1, ?2, NULL, 'node1', 'agent', '{}', 0, ?3)",
            rusqlite::params!["n1", "spec1", now],
        )
        .unwrap();
        // Insert a graph_run
        conn.execute(
            "INSERT INTO graph_runs (id, graph_id, spec_id, node_id, status, started_at, iteration) VALUES (?1, ?2, ?3, ?4, 'success', ?5, 1)",
            rusqlite::params!["run1", "loop1", "spec1", "n1", now],
        )
        .unwrap();
        drop(conn);
        assert_eq!(db.count_graph_node_runs().unwrap(), 1);
    }

    #[test]
    fn count_completed_graphs_with_data() {
        let db = test_db();
        let conn = db.conn.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES (?1, 'completed', '/tmp', 'completed', ?2)",
            rusqlite::params!["loop1", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES (?1, 'running', '/tmp', 'running', ?2)",
            rusqlite::params!["loop2", now],
        )
        .unwrap();
        drop(conn);
        assert_eq!(db.count_completed_graphs().unwrap(), 1);
    }

    #[test]
    fn max_graph_nodes_in_any_graph_with_data() {
        let db = test_db();
        let conn = db.conn.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // Graph with 1 spec, 2 nodes
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES (?1, 'loop1', '/tmp', 'draft', ?2)",
            rusqlite::params!["loop1", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES (?1, ?2, 'spec1', 0, 0, 'pending')",
            rusqlite::params!["spec1", "loop1"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at) VALUES (?1, ?2, NULL, 'n1', 'agent', '{}', 0, ?3)",
            rusqlite::params!["n1", "spec1", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at) VALUES (?1, ?2, NULL, 'n2', 'check', '{}', 1, ?3)",
            rusqlite::params!["n2", "spec1", now],
        )
        .unwrap();
        drop(conn);
        assert_eq!(db.max_graph_nodes_in_any_graph().unwrap(), 2);
    }

    #[test]
    fn has_parallel_graph_run_false_when_non_parallel() {
        let db = test_db();
        let conn = db.conn.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES (?1, 'loop1', '/tmp', 'draft', ?2)",
            rusqlite::params!["loop1", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES (?1, ?2, 'spec1', 0, 0, 'pending')",
            rusqlite::params!["spec1", "loop1"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at) VALUES (?1, ?2, NULL, 'n1', 'agent', '{}', 0, ?3)",
            rusqlite::params!["n1", "spec1", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_runs (id, graph_id, spec_id, node_id, status, started_at, iteration) VALUES (?1, ?2, ?3, ?4, 'success', ?5, 1)",
            rusqlite::params!["run1", "loop1", "spec1", "n1", now],
        )
        .unwrap();
        drop(conn);
        assert!(!db.has_parallel_graph_run().unwrap());
    }

    #[test]
    fn has_parallel_graph_run_false_for_parallelizable_spec() {
        let db = test_db();
        let conn = db.conn.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES (?1, 'loop1', '/tmp', 'draft', ?2)",
            rusqlite::params!["loop1", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES (?1, ?2, 'spec1', 0, 1, 'pending')",
            rusqlite::params!["spec1", "loop1"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at) VALUES (?1, ?2, NULL, 'n1', 'agent', '{}', 0, ?3)",
            rusqlite::params!["n1", "spec1", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_runs (id, graph_id, spec_id, node_id, status, started_at, iteration) VALUES (?1, ?2, ?3, ?4, 'success', ?5, 1)",
            rusqlite::params!["run1", "loop1", "spec1", "n1", now],
        )
        .unwrap();
        drop(conn);
        // The `parallelizable` flag alone never counted: the node is in no ensemble.
        assert!(!db.has_parallel_graph_run().unwrap());
    }

    /// Seeds one graph with a join node, one member node, one ensemble of
    /// `kind`, and one successful run of the member node.
    fn seed_ensemble_run(db: &Database, kind: &str) {
        let conn = db.conn.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES (?1, 'loop1', '/tmp', 'draft', ?2)",
            rusqlite::params!["loop1", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES (?1, ?2, 'spec1', 0, 0, 'pending')",
            rusqlite::params!["spec1", "loop1"],
        )
        .unwrap();
        for (id, node_kind, position) in [("j1", "join", 0), ("m1", "agent", 1)] {
            conn.execute(
                "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at) VALUES (?1, ?2, NULL, ?3, ?4, '{}', ?5, ?6)",
                rusqlite::params![id, "spec1", id, node_kind, position, now],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO ensembles (id, spec_id, graph_id, name, prompt_template, join_node_id, entry_from_node, entry_condition, min_pass, timeout_minutes, on_pass_to, on_fail_to, created_at, kind) VALUES (?1, ?2, NULL, 'ens', 'tmpl', 'j1', 'm1', 'always', 1, 15, 'j1', NULL, ?3, ?4)",
            rusqlite::params!["ens1", "spec1", now, kind],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ensemble_members (ensemble_id, node_id, position, platform) VALUES (?1, ?2, 0, 'opencode')",
            rusqlite::params!["ens1", "m1"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_runs (id, graph_id, spec_id, node_id, status, started_at, iteration) VALUES (?1, ?2, ?3, ?4, 'success', ?5, 1)",
            rusqlite::params!["run1", "loop1", "spec1", "m1", now],
        )
        .unwrap();
    }

    #[test]
    fn has_parallel_graph_run_true_for_parallel_ensemble_member() {
        let db = test_db();
        seed_ensemble_run(&db, "parallel");
        assert!(db.has_parallel_graph_run().unwrap());
    }

    #[test]
    fn has_parallel_graph_run_false_for_cascade_ensemble_member() {
        let db = test_db();
        seed_ensemble_run(&db, "cascade");
        assert!(!db.has_parallel_graph_run().unwrap());
    }

    #[test]
    fn max_seed_session_bindings_with_data() {
        let db = test_db();
        // Need an interactive_sessions row for the FK
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO interactive_sessions (id, name, cli, working_dir, started_at) VALUES (?1, 's1', 'opencode', '/tmp', '2024-01-01T00:00:00Z')",
            rusqlite::params!["sess1"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO interactive_sessions (id, name, cli, working_dir, started_at) VALUES (?1, 's2', 'opencode', '/tmp', '2024-01-01T00:00:00Z')",
            rusqlite::params!["sess2"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO seed_sessions (session_id, seed_id, bound_at) VALUES (?1, 'seed1', '2024-01-01T00:00:00Z')",
            rusqlite::params!["sess1"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO seed_sessions (session_id, seed_id, bound_at) VALUES (?1, 'seed1', '2024-01-01T00:00:00Z')",
            rusqlite::params!["sess2"],
        )
        .unwrap();
        drop(conn);
        assert_eq!(db.max_seed_session_bindings().unwrap(), 2);
    }
}
