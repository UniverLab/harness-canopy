use super::*;

impl Database {
    /// Recompute derived `contains` (parent → child) edges from registry paths.
    ///
    /// Deletes all existing `contains` edges between project nodes, then links
    /// each project to its deepest container (component-wise path prefix via
    /// `Path::starts_with` — never a string prefix). Pure recompute: hand
    /// edits never survive, and only the direct parent is stored (transitivity
    /// is recovered via the depth parameter at read time). Returns edges inserted.
    pub fn rebuild_containment_edges(&self) -> Result<usize> {
        {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            conn.execute(
                "DELETE FROM intelligence_edges WHERE relation = 'contains'
                 AND from_node_id IN (SELECT id FROM intelligence_nodes WHERE kind = 'project')
                 AND to_node_id IN (SELECT id FROM intelligence_nodes WHERE kind = 'project')",
                [],
            )?;
        }

        let projects = self.list_projects()?;
        let mut pairs: Vec<(String, String)> = Vec::new();
        for child in &projects {
            if let Some(parent) = Self::deepest_containing_parent(child, &projects) {
                pairs.push((parent.hash.clone(), child.hash.clone()));
            }
        }

        let mut inserted = 0usize;
        for (parent_hash, child_hash) in pairs {
            let from_node = self.find_project_node(&parent_hash)?;
            let to_node = self.find_project_node(&child_hash)?;
            let (Some(from_node), Some(to_node)) = (from_node, to_node) else {
                continue;
            };
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM intelligence_edges WHERE from_node_id = ?1 AND to_node_id = ?2 AND relation = 'contains')",
                rusqlite::params![from_node.id, to_node.id],
                |row| row.get::<_, i64>(0),
            )? != 0;
            if exists {
                continue;
            }
            let now = Utc::now().timestamp();
            conn.execute(
                "INSERT INTO intelligence_edges (from_node_id, to_node_id, relation, weight, created_at)
                 VALUES (?1, ?2, 'contains', 1.0, ?3)",
                rusqlite::params![from_node.id, to_node.id, now],
            )?;
            inserted += 1;
        }
        Ok(inserted)
    }

    /// The deepest registered project whose path is a component-wise ancestor
    /// of `child` (`Path::starts_with` — never a string prefix), or `None`.
    /// The strict `>` keeps the first-deepest tie-break.
    fn deepest_containing_parent<'a>(
        child: &crate::domain::project::Project,
        projects: &'a [crate::domain::project::Project],
    ) -> Option<&'a crate::domain::project::Project> {
        let child_path = std::path::Path::new(&child.path);
        let mut best: Option<&crate::domain::project::Project> = None;
        let mut best_depth = 0usize;
        for parent in projects {
            if parent.hash == child.hash {
                continue;
            }
            let parent_path = std::path::Path::new(&parent.path);
            if !child_path.starts_with(parent_path) {
                continue;
            }
            let depth = parent_path.components().count();
            if depth > best_depth {
                best_depth = depth;
                best = Some(parent);
            }
        }
        best
    }

    /// Retype the two backlog-note nodes (`d8c3230b`, `508e398c`) from
    /// `kind='project'` to `kind='fact'`. Skips absent or already-fixed nodes.
    /// Refuses (Err) if a target looks like a real registry node
    /// (`metadata.source == "registry"`) — retyping that would corrupt the graph.
    pub fn retype_backlog_project_nodes(&self) -> Result<usize> {
        let mut retyped = 0usize;
        for prefix in ["d8c3230b", "508e398c"] {
            let Some(id) = self.resolve_node_id_by_prefix(prefix)? else {
                tracing::warn!("cm9 retype: no node matching prefix '{prefix}', skipping");
                continue;
            };
            let Some(node) = self.get_intelligence_node(&id)? else {
                continue;
            };
            if node.kind != "project" {
                continue;
            }
            if Self::node_is_registry_project(&node) {
                anyhow::bail!(
                    "refusing to retype node '{id}': it is a registry project node, not a backlog note"
                );
            }
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            conn.execute(
                "UPDATE intelligence_nodes SET kind = 'fact' WHERE id = ?1",
                rusqlite::params![id],
            )?;
            retyped += 1;
        }
        Ok(retyped)
    }

    /// Seed the four known cross-project `depends_on` edges (best-effort).
    /// Each side resolves by `path.contains(substr)` against registered
    /// projects (falling back to graph/spec workdirs for ticket-tagged
    /// from-sides); missing or ambiguous sides are skipped with a warning and
    /// never fail. `link_projects` dedup makes this idempotent.
    pub fn seed_cm9_project_dependencies(&self) -> Result<usize> {
        // (from_substrs, from_ticket, to_substr): CB3, CX1 x2, CX2, AD11.
        let pairs: &[(&[&str], &str, &str)] = &[
            (&["harness-canopy"], "CB3", "canopy-registry"),
            (&["cx1"], "CX1", "ghscaff"),
            (&["cx1"], "CX1", "demostage"),
            (&["cx2"], "CX2", "univerlab"),
            (&["astro-denoise"], "AD11", "harness-canopy"),
        ];
        let mut created = 0usize;
        for (from_substrs, ticket, to_substr) in pairs {
            let from_hash = self.resolve_seed_project(from_substrs, ticket);
            let to_hash = self.resolve_seed_project(&[to_substr], "");
            match (from_hash, to_hash) {
                (Some(from), Some(to)) => {
                    let before = self.list_related_projects(&from, 1000)?.len();
                    self.link_projects(&from, &to, "depends_on", None)?;
                    let after = self.list_related_projects(&from, 1000)?.len();
                    if after > before {
                        created += 1;
                    }
                }
                (from, to) => {
                    tracing::warn!(
                        "cm9 seed: skipping {ticket} edge (from={from:?}, to={to:?}): side not registered or ambiguous"
                    );
                }
            }
        }
        Ok(created)
    }

    /// Resolve one seed endpoint: first registered project whose path contains
    /// any of `substrs` (exactly one match required); else the workdir of a
    /// graph/spec whose name mentions `ticket`.
    fn resolve_seed_project(&self, substrs: &[&str], ticket: &str) -> Option<String> {
        let projects = self.list_projects().ok()?;
        let mut matches: Vec<String> = Vec::new();
        for project in &projects {
            if substrs.iter().any(|s| project.path.contains(s)) {
                matches.push(project.hash.clone());
            }
        }
        if matches.len() == 1 {
            return Some(matches.into_iter().next().unwrap());
        }
        if !ticket.is_empty() {
            // Fall back to graph/spec workdirs tagged with the ticket id.
            let workdir: Option<String> = {
                let conn = self.conn.lock().ok()?;
                let mut stmt = conn
                    .prepare(
                        "SELECT workdir FROM graph_specs WHERE name LIKE ?1 AND workdir IS NOT NULL LIMIT 1",
                    )
                    .ok()?;
                let pattern = format!("%{ticket}%");
                stmt.query_row(rusqlite::params![pattern], |row| row.get(0))
                    .ok()
            };
            if let Some(workdir) = workdir {
                let hit = projects.into_iter().find(|p| p.path == workdir);
                if let Some(project) = hit {
                    return Some(project.hash);
                }
            }
        }
        None
    }
}
