use super::*;

impl Database {
    // ── Intelligence V2: Project-Linked Knowledge ──

    /// Upsert the intelligence root node (`kind='project'`) for a registered
    /// project. Idempotent: node id is derived from the project hash.
    pub fn ensure_project_node(
        &self,
        project: &crate::domain::project::Project,
    ) -> Result<IntelligenceNodeRecord> {
        self.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some(format!("project:{}", project.hash)),
            kind: Some("project".to_string()),
            status: None,
            title: Some(project.name.clone()),
            body: Some(
                project
                    .description
                    .clone()
                    .unwrap_or_else(|| project.path.clone()),
            ),
            metadata: Some(Some(serde_json::json!({
                "source": "registry",
                "path": project.path,
            }))),
            project_hash: Some(Some(project.hash.clone())),
            session_id: None,
            body_replace: None,
            relations: None,
        })
        .map(|result| result.record)
    }

    /// Create missing `kind='project'` root nodes for already-registered
    /// projects. Runs at database open so graphs created before this code
    /// existed become linkable.
    pub fn backfill_project_nodes(&self) -> Result<usize> {
        let projects = self.list_projects()?;
        let mut created = 0;
        for project in &projects {
            if self.find_project_node(&project.hash)?.is_none() {
                self.ensure_project_node(project)?;
                created += 1;
            }
        }
        Ok(created)
    }

    /// List all indexed project nodes for the project picker.
    pub fn list_intelligence_projects(
        &self,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<IntelligenceNodeRecord>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        let sql = if query.is_some() {
            "SELECT id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at
             FROM intelligence_nodes
             WHERE kind = 'project'
               AND (instr(lower(title), ?1) > 0 OR instr(lower(body), ?1) > 0 OR instr(lower(coalesce(metadata, '')), ?1) > 0)
             ORDER BY updated_at DESC
             LIMIT ?2"
        } else {
            "SELECT id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at
             FROM intelligence_nodes
             WHERE kind = 'project'
             ORDER BY updated_at DESC
             LIMIT ?1"
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = if let Some(q) = query {
            stmt.query_map(
                rusqlite::params![q.to_lowercase(), limit as i64],
                Self::read_intelligence_node,
            )?
        } else {
            stmt.query_map(
                rusqlite::params![limit as i64],
                Self::read_intelligence_node,
            )?
        };
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    /// Create a relationship edge between two project nodes.
    /// If an edge with the same from/to/relation already exists, returns it.
    pub fn link_projects(
        &self,
        from_project_hash: &str,
        to_project_hash: &str,
        relation: &str,
        weight: Option<f64>,
    ) -> Result<IntelligenceEdgeRecord> {
        crate::domain::project::validate_project_relation(relation)?;
        if relation == "contains" {
            anyhow::bail!(
                "relation 'contains' is derived from registry paths — it cannot be hand-linked"
            );
        }
        let from_node = self
            .find_project_node(from_project_hash)?
            .ok_or_else(|| anyhow!("Project node not found for hash '{}'", from_project_hash))?;
        let to_node = self
            .find_project_node(to_project_hash)?
            .ok_or_else(|| anyhow!("Project node not found for hash '{}'", to_project_hash))?;

        let weight = weight.unwrap_or(1.0);
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        // Check for existing edge with the same from/to/relation.
        let mut stmt = conn.prepare(
            "SELECT id, from_node_id, to_node_id, relation, weight, created_at
             FROM intelligence_edges
             WHERE from_node_id = ?1 AND to_node_id = ?2 AND relation = ?3
             LIMIT 1",
        )?;
        let existing = stmt
            .query_row(
                rusqlite::params![from_node.id, to_node.id, relation],
                Self::read_intelligence_edge,
            )
            .ok();
        if let Some(edge) = existing {
            return Ok(edge);
        }

        let now = Utc::now().timestamp();
        drop(stmt);
        let mut stmt = conn.prepare(
            "INSERT INTO intelligence_edges (from_node_id, to_node_id, relation, weight, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        stmt.execute(rusqlite::params![
            from_node.id,
            to_node.id,
            relation,
            weight,
            now
        ])?;
        drop(stmt);

        let edge_id = conn.last_insert_rowid();
        Ok(IntelligenceEdgeRecord {
            id: edge_id,
            from_node_id: from_node.id,
            to_node_id: to_node.id,
            relation: relation.to_string(),
            weight,
            created_at: now,
        })
    }

    /// Find a project node by its hash (project_hash column or id match).
    pub(super) fn find_project_node(
        &self,
        project_hash: &str,
    ) -> Result<Option<IntelligenceNodeRecord>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at
             FROM intelligence_nodes
             WHERE kind = 'project'
               AND (project_hash = ?1 OR id = ?1)
             LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![project_hash])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::read_intelligence_node(row)?))
        } else {
            Ok(None)
        }
    }

    /// Whether a node's `metadata` marks it as a registry-sourced project node.
    pub(super) fn node_is_registry_project(node: &IntelligenceNodeRecord) -> bool {
        let Some(meta) = node.metadata.as_deref() else {
            return false;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(meta) else {
            return false;
        };
        value.get("source").and_then(|s| s.as_str()) == Some("registry")
    }

    /// Get facts and patterns linked to a specific project.
    pub fn list_project_knowledge(
        &self,
        project_hash: &str,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<IntelligenceNodeRecord>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        let sql = match kind {
            Some(_k) => "SELECT id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at
                        FROM intelligence_nodes
                        WHERE project_hash = ?1 AND kind = ?2
                        ORDER BY updated_at DESC
                        LIMIT ?3",
            None => "SELECT id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at
                     FROM intelligence_nodes
                     WHERE project_hash = ?1 AND kind IN ('fact', 'pattern')
                     ORDER BY updated_at DESC
                     LIMIT ?2",
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = match kind {
            Some(_) => stmt.query_map(
                rusqlite::params![project_hash, kind.unwrap(), limit as i64],
                Self::read_intelligence_node,
            )?,
            None => stmt.query_map(
                rusqlite::params![project_hash, limit as i64],
                Self::read_intelligence_node,
            )?,
        };
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    /// Return the newest knowledge write for a project, independent of the
    /// capped list used to render the Knowledge face. Reads a dedicated
    /// millisecond-resolution column, not the public `updated_at` (seconds) —
    /// the panel ticks every 50-200ms (see `tui/event/mod.rs::tick_duration`),
    /// so two writes inside the same second must stay distinguishable.
    pub fn max_project_knowledge_updated_at(&self, project_hash: &str) -> Result<Option<i64>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.query_row(
            "SELECT MAX(content_touched_at) FROM intelligence_nodes
             WHERE project_hash = ?1 AND kind IN ('fact', 'pattern')",
            rusqlite::params![project_hash],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// List projects related to a given project via edges.
    /// Returns (related_node, edge) where edge is reoriented so
    /// from_node_id always equals the queried project's node id.
    pub fn list_related_projects(
        &self,
        project_hash: &str,
        limit: usize,
    ) -> Result<Vec<(IntelligenceNodeRecord, IntelligenceEdgeRecord)>> {
        let Some(project_node) = self.find_project_node(project_hash)? else {
            return Ok(Vec::new());
        };

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT
                 n.id, n.kind, n.status, n.title, n.body, n.metadata, n.project_hash, n.session_id, n.created_at, n.updated_at,
                 e.id, e.from_node_id, e.to_node_id, e.relation, e.weight, e.created_at
             FROM intelligence_edges e
             JOIN intelligence_nodes n ON (
                 (e.from_node_id = ?1 AND n.id = e.to_node_id) OR
                 (e.to_node_id = ?1 AND n.id = e.from_node_id)
             )
             WHERE n.kind = 'project'
             ORDER BY e.weight DESC, e.created_at DESC
             LIMIT ?2",
        )?;
        let project_id = project_node.id.clone();
        let rows = stmt.query_map(
            rusqlite::params![project_node.id, limit as i64],
            move |row| -> rusqlite::Result<_> {
                let node = Self::read_intelligence_node(row)?;
                let raw_from: String = row.get(11)?;
                let raw_to: String = row.get(12)?;
                // Reorient so from_node_id is always the queried project.
                let (oriented_from, oriented_to) = if raw_from == project_id {
                    (raw_from, raw_to)
                } else {
                    (raw_to, raw_from)
                };
                let edge = IntelligenceEdgeRecord {
                    id: row.get(10)?,
                    from_node_id: oriented_from,
                    to_node_id: oriented_to,
                    relation: row.get(13)?,
                    weight: row.get(14)?,
                    created_at: row.get(15)?,
                };
                Ok((node, edge))
            },
        )?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }
}
