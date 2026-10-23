use std::collections::HashMap;

use anyhow::Result;

use crate::db::Database;
use crate::domain::project::Project;

use super::history::parse_rfc3339_timestamp;
use super::row_to_project;

/// A registered project together with the timestamp of its most recent
/// activity, ordered newest-first by [`Database::list_projects_by_activity`].
#[derive(Debug, Clone)]
pub struct ProjectActivity {
    pub project: Project,
    pub last_activity: i64,
}

impl Database {
    /// Registered projects ordered by most recent activity — the newest of
    /// the project's own `created_at`/`indexed_at`, the newest `updated_at`
    /// among its intelligence nodes, and the newest entry of its project
    /// history (the same graphs and past interactive/terminal sessions the
    /// project History tab shows — see [`Database::list_project_history`]).
    /// Ties break by name ascending so the order is stable between refreshes.
    ///
    /// Each source table is read exactly once for the whole list — never one
    /// query per project.
    pub fn list_projects_by_activity(&self) -> Result<Vec<ProjectActivity>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;

        let mut projects: Vec<Project> = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT hash,path,name,description,tags,indexed_at,created_at
                 FROM projects",
            )?;
            let rows = stmt.query_map([], row_to_project)?;
            for project in rows.flatten() {
                projects.push(project);
            }
        }

        let node_activity = int_max_by(
            &conn,
            "SELECT project_hash, MAX(updated_at) FROM intelligence_nodes
             WHERE project_hash IS NOT NULL GROUP BY project_hash",
        )?;
        let graph_activity = int_max_by(
            &conn,
            "SELECT workdir, MAX(COALESCE(completed_at, created_at)) FROM graphs
             WHERE status IN ('completed', 'failed') GROUP BY workdir",
        )?;
        // Session timestamps are RFC3339 text, so SQLite's `MAX()` would
        // compare them as strings; the maximum is folded per directory here
        // instead — still one query for the whole table.
        let interactive_activity = text_max_by(
            &conn,
            "SELECT working_dir, COALESCE(exited_at, started_at) FROM interactive_sessions
             WHERE status != 'active'",
        )?;
        let terminal_activity = text_max_by(
            &conn,
            "SELECT working_dir, COALESCE(last_active, created_at) FROM terminal_sessions
             WHERE status != 'idle'",
        )?;

        let mut ranked: Vec<ProjectActivity> = projects
            .into_iter()
            .map(|project| {
                let last_activity = project
                    .created_at
                    .max(project.indexed_at.unwrap_or(0))
                    .max(node_activity.get(&project.hash).copied().unwrap_or(0))
                    .max(graph_activity.get(&project.path).copied().unwrap_or(0))
                    .max(
                        interactive_activity
                            .get(&project.path)
                            .copied()
                            .unwrap_or(0),
                    )
                    .max(terminal_activity.get(&project.path).copied().unwrap_or(0));
                ProjectActivity {
                    project,
                    last_activity,
                }
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.last_activity
                .cmp(&a.last_activity)
                .then_with(|| a.project.name.cmp(&b.project.name))
        });
        Ok(ranked)
    }
}

/// `(key, MAX(integer column))` rows folded into a map.
fn int_max_by(conn: &rusqlite::Connection, sql: &str) -> Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (key, value) = row?;
        map.insert(key, value);
    }
    Ok(map)
}

/// `(key, timestamp as RFC3339 text)` rows folded into a per-key maximum.
fn text_max_by(conn: &rusqlite::Connection, sql: &str) -> Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut map: HashMap<String, i64> = HashMap::new();
    for row in rows {
        let (key, value) = row?;
        let at = parse_rfc3339_timestamp(&value);
        let entry = map.entry(key).or_insert(at);
        *entry = (*entry).max(at);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::intelligence::IntelligenceNodeInput;

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn project(hash: &str, name: &str, path: &str, created_at: i64) -> Project {
        Project {
            hash: hash.to_string(),
            path: path.to_string(),
            name: name.to_string(),
            description: None,
            tags: None,
            indexed_at: None,
            created_at,
        }
    }

    /// Registration also writes the project's `kind='project'` root node, whose
    /// `updated_at` is "now" — age it back to the registration instant so a
    /// freshly written test row doesn't read as brand-new activity.
    fn age_root_node(db: &Database, hash: &str, at: i64) {
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "UPDATE intelligence_nodes SET updated_at = ?2 WHERE id = ?1",
            rusqlite::params![format!("project:{hash}"), at],
        )
        .unwrap();
    }

    fn note(db: &Database, project_hash: &str) {
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: None,
            kind: Some("fact".to_string()),
            status: None,
            title: Some("a fact".to_string()),
            body: Some("body".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some(project_hash.to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();
    }

    #[test]
    fn old_project_with_a_recent_intelligence_node_ranks_first() {
        let db = test_db();
        let now = chrono::Utc::now().timestamp();
        let ten_days_ago = now - 10 * 86_400;
        let yesterday = now - 86_400;
        db.upsert_project(&project("hash-old", "Old", "/proj/old", ten_days_ago))
            .unwrap();
        db.upsert_project(&project("hash-new", "New", "/proj/new", yesterday))
            .unwrap();
        age_root_node(&db, "hash-old", ten_days_ago);
        age_root_node(&db, "hash-new", yesterday);
        // "Old" was registered ten days ago but written to just now; by
        // creation order alone it would come second.
        note(&db, "hash-old");

        let ranked = db.list_projects_by_activity().unwrap();

        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].project.hash, "hash-old");
        assert_eq!(ranked[1].project.hash, "hash-new");
        assert!(ranked[0].last_activity > ranked[1].last_activity);
    }

    #[test]
    fn old_project_with_recent_session_history_ranks_first() {
        let db = test_db();
        let now = chrono::Utc::now().timestamp();
        let ten_days_ago = now - 10 * 86_400;
        let yesterday = now - 86_400;
        db.upsert_project(&project("hash-sess", "Session", "/proj/sess", ten_days_ago))
            .unwrap();
        db.upsert_project(&project("hash-idle", "Idle", "/proj/idle", yesterday))
            .unwrap();
        age_root_node(&db, "hash-sess", ten_days_ago);
        age_root_node(&db, "hash-idle", yesterday);
        db.insert_interactive_session(
            "sess-1",
            "sess-1",
            "opencode",
            "/proj/sess",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        // A live session is excluded from the history the same way the
        // project History tab excludes it; finishing it is what makes it
        // count as activity.
        db.finish_interactive_session("sess-1", 0).unwrap();

        let ranked = db.list_projects_by_activity().unwrap();

        assert_eq!(ranked[0].project.hash, "hash-sess");
        assert!(ranked[0].last_activity >= now);
    }

    #[test]
    fn equal_activity_breaks_ties_by_name() {
        let db = test_db();
        let created = chrono::Utc::now().timestamp() - 100 * 86_400;
        db.upsert_project(&project("hash-b", "Beta", "/proj/beta", created))
            .unwrap();
        db.upsert_project(&project("hash-a", "Alpha", "/proj/alpha", created))
            .unwrap();
        age_root_node(&db, "hash-a", created);
        age_root_node(&db, "hash-b", created);

        let ranked = db.list_projects_by_activity().unwrap();

        assert_eq!(ranked[0].project.name, "Alpha");
        assert_eq!(ranked[1].project.name, "Beta");
        assert_eq!(ranked[0].last_activity, created);
    }
}
