use super::*;

impl Database {
    pub fn insert_graph_node(&self, node: &GraphNode) -> Result<()> {
        validate_single_target(node.spec_id.as_deref(), node.graph_id.as_deref())?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &node.id,
                &node.spec_id,
                &node.graph_id,
                &node.name,
                node.kind.as_str(),
                serde_json::to_string(&node.config)?,
                node.position,
                node.created_at.timestamp(),
            ],
        )?;
        Ok(())
    }

    pub fn list_graph_nodes(&self, spec_id: &str) -> Result<Vec<GraphNode>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, name, kind, config, position, created_at
             FROM graph_nodes WHERE spec_id = ?1 ORDER BY position ASC",
        )?;
        let rows = stmt.query_map(params![spec_id], map_graph_node_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Nodes belonging to a graph's top-level graph (as opposed to any one
    /// spec's graph).
    pub fn list_graph_nodes_for_graph(&self, graph_id: &str) -> Result<Vec<GraphNode>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, name, kind, config, position, created_at
             FROM graph_nodes WHERE graph_id = ?1 ORDER BY position ASC",
        )?;
        let rows = stmt.query_map(params![graph_id], map_graph_node_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Every graph node in the database, across every graph and spec — unlike
    /// [`list_graph_nodes`]/[`list_graph_nodes_for_graph`], which scope to one
    /// graph. Used by the `graph_audit_node_configs` MCP tool to find nodes
    /// already carrying a config key their kind will never read (e.g. a
    /// `prompt` key on an agent node — see
    /// `daemon::handler::validate_node_config`), which write-time validation
    /// alone can't catch for nodes created before it existed.
    pub fn list_all_graph_nodes(&self) -> Result<Vec<GraphNode>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, name, kind, config, position, created_at
             FROM graph_nodes ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], map_graph_node_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_graph_node(&self, node_id: &str) -> Result<Option<GraphNode>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, name, kind, config, position, created_at
             FROM graph_nodes WHERE id = ?1",
        )?;
        stmt.query_row(params![node_id], map_graph_node_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn update_graph_node_details(
        &self,
        node_id: &str,
        name: Option<&str>,
        kind: Option<GraphNodeKind>,
        config: Option<&Value>,
        position: Option<i64>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_nodes
             SET name = COALESCE(?1, name),
                 kind = COALESCE(?2, kind),
                 config = COALESCE(?3, config),
                 position = COALESCE(?4, position)
             WHERE id = ?5",
            params![
                name,
                kind.map(|value| value.as_str()),
                config.map(serde_json::to_string).transpose()?,
                position,
                node_id,
            ],
        )?;
        Ok(rows > 0)
    }

    pub fn insert_graph_edge(&self, edge: &GraphEdge) -> Result<()> {
        validate_single_target(edge.spec_id.as_deref(), edge.graph_id.as_deref())?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_edges (id, spec_id, graph_id, from_node, to_node, condition, route)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &edge.id,
                &edge.spec_id,
                &edge.graph_id,
                &edge.from_node,
                &edge.to_node,
                edge.condition.as_str(),
                edge.condition.route_label(),
            ],
        )?;
        Ok(())
    }

    /// Insert a node together with any edges wiring it, in one transaction, so
    /// a copy (`graph_copy_node`) can never leave a node half-wired. Every edge
    /// and the node must target the same single graph (spec or graph).
    pub fn insert_node_with_edges(&self, node: &GraphNode, edges: &[GraphEdge]) -> Result<()> {
        validate_single_target(node.spec_id.as_deref(), node.graph_id.as_deref())?;
        for edge in edges {
            validate_single_target(edge.spec_id.as_deref(), edge.graph_id.as_deref())?;
        }
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &node.id,
                &node.spec_id,
                &node.graph_id,
                &node.name,
                node.kind.as_str(),
                serde_json::to_string(&node.config)?,
                node.position,
                node.created_at.timestamp(),
            ],
        )?;
        for edge in edges {
            tx.execute(
                "INSERT INTO graph_edges (id, spec_id, graph_id, from_node, to_node, condition, route)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    &edge.id,
                    &edge.spec_id,
                    &edge.graph_id,
                    &edge.from_node,
                    &edge.to_node,
                    edge.condition.as_str(),
                    edge.condition.route_label(),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_graph_edges(&self, spec_id: &str) -> Result<Vec<GraphEdge>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, from_node, to_node, condition, route
             FROM graph_edges WHERE spec_id = ?1 ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map(params![spec_id], map_graph_edge_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Edges belonging to a graph's top-level graph (as opposed to any one
    /// spec's graph).
    pub fn list_graph_edges_for_graph(&self, graph_id: &str) -> Result<Vec<GraphEdge>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, from_node, to_node, condition, route
             FROM graph_edges WHERE graph_id = ?1 ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map(params![graph_id], map_graph_edge_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_graph_edge(&self, edge_id: &str) -> Result<Option<GraphEdge>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, from_node, to_node, condition, route
             FROM graph_edges WHERE id = ?1",
        )?;
        stmt.query_row(params![edge_id], map_graph_edge_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn update_graph_edge_condition(
        &self,
        edge_id: &str,
        condition: &GraphEdgeCondition,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_edges
             SET condition = ?1,
                 route = ?2
             WHERE id = ?3",
            params![condition.as_str(), condition.route_label(), edge_id],
        )?;
        Ok(rows > 0)
    }

    /// Repoint an existing edge at a new target node — used to rewire a
    /// router route to a different destination without dropping and
    /// re-creating the edge (which would lose its id).
    pub fn update_graph_edge_target(&self, edge_id: &str, to_node: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_edges SET to_node = ?1 WHERE id = ?2",
            params![to_node, edge_id],
        )?;
        Ok(rows > 0)
    }

    /// Drop a single edge — used when a router's routes editor removes a
    /// declared route that already had an edge wired to it, so the edge
    /// never outlives the route it named (see
    /// [`crate::domain::graphs::validate_router_edges_declared`]).
    pub fn delete_graph_edge(&self, edge_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute("DELETE FROM graph_edges WHERE id = ?1", params![edge_id])?;
        Ok(rows > 0)
    }
}
