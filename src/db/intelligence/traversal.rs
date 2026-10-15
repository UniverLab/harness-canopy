use super::*;

impl Database {
    pub fn walk_intelligence_graph(
        &self,
        node_id: &str,
        depth: usize,
    ) -> Result<Option<IntelligenceGraphWalk>> {
        let Some(root) = self.get_intelligence_node(node_id)? else {
            return Ok(None);
        };

        let mut visited_nodes: HashSet<String> = HashSet::from([root.id.clone()]);
        let mut collected_nodes: HashMap<String, IntelligenceNodeRecord> =
            HashMap::from([(root.id.clone(), root.clone())]);
        let mut collected_edges: Vec<IntelligenceEdgeRecord> = Vec::new();
        let mut frontier = VecDeque::from([root.id.clone()]);

        for _ in 0..depth {
            let current_level: Vec<String> = frontier.drain(..).collect();
            if current_level.is_empty() {
                break;
            }

            let mut next_frontier = VecDeque::new();
            for current in current_level {
                self.visit_intelligence_neighbors(
                    &current,
                    &mut collected_edges,
                    &mut collected_nodes,
                    &mut visited_nodes,
                    &mut next_frontier,
                )?;
            }
            frontier = next_frontier;
        }

        Ok(Some(IntelligenceGraphWalk {
            root,
            nodes: collected_nodes.into_values().collect(),
            edges: collected_edges,
        }))
    }

    /// Collect the edges and unvisited neighbours of `current` for one step of
    /// [`Database::walk_intelligence_graph`].
    fn visit_intelligence_neighbors(
        &self,
        current: &str,
        collected_edges: &mut Vec<IntelligenceEdgeRecord>,
        collected_nodes: &mut HashMap<String, IntelligenceNodeRecord>,
        visited_nodes: &mut HashSet<String>,
        next_frontier: &mut VecDeque<String>,
    ) -> Result<()> {
        for edge in self.list_recent_intelligence_edges(current)? {
            if collected_edges
                .iter()
                .all(|existing| existing.id != edge.id)
            {
                collected_edges.push(edge.clone());
            }

            for neighbor in [edge.from_node_id.clone(), edge.to_node_id.clone()] {
                if !visited_nodes.insert(neighbor.clone()) {
                    continue;
                }
                let Some(node) = self.get_intelligence_node(&neighbor)? else {
                    continue;
                };
                collected_nodes.insert(neighbor.clone(), node);
                next_frontier.push_back(neighbor);
            }
        }
        Ok(())
    }

    pub fn list_cross_project_dependencies(
        &self,
        limit: usize,
    ) -> Result<Vec<IntelligenceProjectDependencyRecord>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT
                e.from_node_id,
                from_node.title,
                from_node.project_hash,
                e.to_node_id,
                to_node.title,
                to_node.project_hash,
                e.relation,
                e.weight,
                e.created_at
             FROM intelligence_edges e
             JOIN intelligence_nodes from_node ON from_node.id = e.from_node_id
             JOIN intelligence_nodes to_node ON to_node.id = e.to_node_id
             WHERE from_node.kind = 'project'
               AND to_node.kind = 'project'
               AND (
                   instr(lower(e.relation), 'depend') > 0 OR
                   instr(lower(e.relation), 'require') > 0 OR
                   instr(lower(e.relation), 'block') > 0
               )
             ORDER BY e.created_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![limit as i64],
            |row| -> rusqlite::Result<IntelligenceProjectDependencyRecord> {
                Ok(IntelligenceProjectDependencyRecord {
                    from_node_id: row.get(0)?,
                    from_title: row.get(1)?,
                    from_project_hash: row.get(2)?,
                    to_node_id: row.get(3)?,
                    to_title: row.get(4)?,
                    to_project_hash: row.get(5)?,
                    relation: row.get(6)?,
                    weight: row.get(7)?,
                    created_at: row.get(8)?,
                })
            },
        )?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    /// BFS project-scope traversal from `root_hash` up to `depth` hops.
    ///
    /// Follows outbound edges for `depends_on`, `extends`, `publishes`,
    /// `relates_to`; `complements` in both directions; `contains` in both
    /// directions (reverse = inherited-upward rule). Inbound `depends_on`
    /// is never followed (see [`Database::list_project_dependents`]).
    /// Depth is clamped to `0..=MAX_TRAVERSAL_DEPTH`.
    pub fn traverse_project_scope(&self, root_hash: &str, depth: usize) -> Result<TraversalScope> {
        let depth = depth.min(MAX_TRAVERSAL_DEPTH);
        let mut projects = vec![TraversedProject {
            hash: root_hash.to_string(),
            via_relation: None,
            depth: 0,
        }];
        if depth == 0 {
            return Ok(TraversalScope {
                projects,
                reached: 0,
            });
        }
        let Some(root_node) = self.find_project_node(root_hash)? else {
            return Ok(TraversalScope {
                projects,
                reached: 0,
            });
        };

        // Load project↔project edges once; BFS in Rust over node ids.
        let edges: Vec<IntelligenceEdgeRecord> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT e.id, e.from_node_id, e.to_node_id, e.relation, e.weight, e.created_at
                 FROM intelligence_edges e
                 JOIN intelligence_nodes a ON a.id = e.from_node_id
                 JOIN intelligence_nodes b ON b.id = e.to_node_id
                 WHERE a.kind = 'project' AND b.kind = 'project'",
            )?;
            let rows: Vec<IntelligenceEdgeRecord> = stmt
                .query_map([], Self::read_intelligence_edge)?
                .filter_map(|r| r.ok())
                .collect();
            rows
        };
        // Node id → project hash for reached nodes.
        let node_hash: std::collections::HashMap<String, String> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, project_hash FROM intelligence_nodes WHERE kind = 'project'",
            )?;
            let mut map = std::collections::HashMap::new();
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?;
            for row in rows.filter_map(|r| r.ok()) {
                if let Some(hash) = row.1 {
                    map.insert(row.0, hash);
                }
            }
            map
        };

        let mut visited: HashSet<String> = HashSet::from([root_node.id.clone()]);
        // (node_id, depth) frontier; root depth = 0.
        let mut frontier: Vec<(String, usize)> = vec![(root_node.id, 0)];
        for _ in 0..depth {
            let mut next_frontier: Vec<(String, usize)> = Vec::new();
            for (current, cur_depth) in &frontier {
                next_frontier.extend(Self::expand_scope_frontier(
                    current,
                    *cur_depth,
                    &edges,
                    &node_hash,
                    &mut visited,
                    &mut projects,
                ));
            }
            if next_frontier.is_empty() {
                break;
            }
            frontier = next_frontier;
        }
        let reached = projects.len().saturating_sub(1);
        Ok(TraversalScope { projects, reached })
    }

    /// The neighbour `current` reaches through `edge`, when the relation is
    /// followed from that direction: outbound for every relation, reverse only
    /// for symmetric `complements` and upward `contains`.
    fn scope_neighbor<'a>(
        edge: &'a IntelligenceEdgeRecord,
        current: &str,
    ) -> Option<(&'a str, &'a str)> {
        if edge.from_node_id == current {
            // Outbound: all relations followed.
            Some((edge.to_node_id.as_str(), edge.relation.as_str()))
        } else if edge.to_node_id == current
            && (edge.relation == "complements" || edge.relation == "contains")
        {
            // Reverse: only symmetric complements + upward contains.
            Some((edge.from_node_id.as_str(), edge.relation.as_str()))
        } else {
            None
        }
    }

    /// One BFS expansion step: visit every edge of `current`, recording newly
    /// reached projects and returning the next frontier.
    fn expand_scope_frontier(
        current: &str,
        cur_depth: usize,
        edges: &[IntelligenceEdgeRecord],
        node_hash: &HashMap<String, String>,
        visited: &mut HashSet<String>,
        projects: &mut Vec<TraversedProject>,
    ) -> Vec<(String, usize)> {
        let mut next_frontier: Vec<(String, usize)> = Vec::new();
        for edge in edges {
            let Some((neighbor_id, relation)) = Self::scope_neighbor(edge, current) else {
                continue;
            };
            if !visited.insert(neighbor_id.to_string()) {
                continue;
            }
            if let Some(hash) = node_hash.get(neighbor_id) {
                projects.push(TraversedProject {
                    hash: hash.clone(),
                    via_relation: Some(relation.to_string()),
                    depth: cur_depth + 1,
                });
            }
            next_frontier.push((neighbor_id.to_string(), cur_depth + 1));
        }
        next_frontier
    }

    /// Impact warning source: projects that declare `depends_on` ON `hash`.
    /// Inbound dependents never pull context — they only warn.
    pub fn list_project_dependents(
        &self,
        hash: &str,
    ) -> Result<Vec<IntelligenceProjectDependencyRecord>> {
        let Some(node) = self.find_project_node(hash)? else {
            return Ok(Vec::new());
        };
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT
                e.from_node_id,
                from_node.title,
                from_node.project_hash,
                e.to_node_id,
                to_node.title,
                to_node.project_hash,
                e.relation,
                e.weight,
                e.created_at
             FROM intelligence_edges e
             JOIN intelligence_nodes from_node ON from_node.id = e.from_node_id
             JOIN intelligence_nodes to_node ON to_node.id = e.to_node_id
             WHERE e.to_node_id = ?1 AND e.relation = 'depends_on'
             ORDER BY e.created_at DESC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![node.id],
            |row| -> rusqlite::Result<IntelligenceProjectDependencyRecord> {
                Ok(IntelligenceProjectDependencyRecord {
                    from_node_id: row.get(0)?,
                    from_title: row.get(1)?,
                    from_project_hash: row.get(2)?,
                    to_node_id: row.get(3)?,
                    to_title: row.get(4)?,
                    to_project_hash: row.get(5)?,
                    relation: row.get(6)?,
                    weight: row.get(7)?,
                    created_at: row.get(8)?,
                })
            },
        )?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }
}
