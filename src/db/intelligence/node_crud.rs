use super::*;

impl Database {
    /// Resolve a node id prefix (e.g. "3a476c63") to exactly one node.
    /// - Ok(Some(full_id)) if exactly one node matches
    /// - Ok(None) if no nodes match
    /// - Err with candidate listing if multiple nodes match (ambiguous)
    ///
    /// Search is global (no project_hash filter) so a prefix that lives in a
    /// different project hash still resolves — the corpus is split across two
    /// hashes and callers cite ids by prefix regardless of project.
    pub fn resolve_node_id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        if prefix.is_empty() {
            return Ok(None);
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Self::resolve_node_id_by_prefix_on(&conn, prefix)
    }

    pub fn upsert_intelligence_node(
        &self,
        mut input: IntelligenceNodeInput,
    ) -> Result<UpsertResult> {
        Self::validate_intelligence_node_input(&input)?;

        let node_id = input
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let now = Utc::now().timestamp();
        let content_touched_at = Utc::now().timestamp_millis();
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        // Every write below runs inside one transaction: the node insert or
        // update, the edge replacement and the edge inserts. Any `Err` before
        // `tx.commit()` drops the transaction uncommitted and rolls the whole
        // upsert back, so a failing edge can never leave an orphan node or a
        // partial edge list behind.
        let tx = conn.transaction()?;
        // Resolve relation target prefixes INSIDE the transaction so the
        // existence check and the node/edge writes are one atomic unit: a
        // missing target fails here — before any row is written — and the `?`
        // drops the transaction uncommitted (rollback). The helper takes a
        // `&Connection` (`&Transaction` derefs to `&Connection`) and never locks
        // `self.conn`, so there is no reentrant-mutex deadlock and no TOCTOU
        // window between resolution and the write.
        if let Some(ref rels) = input.relations {
            input.relations = Some(Self::resolve_relation_target_ids(&tx, rels)?);
        }
        // Determine creation vs update before writing. For auto-generated
        // ids this is always creation. Done inside the same transaction as
        // the write so the check and the write cannot be split by another
        // thread's insert. The existing row is read here too so an update
        // that omits a field preserves it without a second lock acquisition.
        let existing: Option<IntelligenceNodeRecord> = {
            let mut stmt = tx.prepare(
                "SELECT id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at
                 FROM intelligence_nodes WHERE id = ?1 LIMIT 1",
            )?;
            let mut rows = stmt.query(rusqlite::params![&node_id])?;
            match rows.next()? {
                Some(row) => Some(Self::read_intelligence_node(row)?),
                None => None,
            }
        };
        let was_created = existing.is_none();

        let mut changed_fields = match existing {
            None => Self::create_intelligence_node(&tx, &node_id, input, now, content_touched_at)?,
            Some(prev) => {
                let finals = Self::resolve_updated_fields(prev, &input, &node_id)?;
                Self::ensure_supersedes_available(
                    &tx,
                    &node_id,
                    &finals.status,
                    input.relations.as_deref(),
                )?;
                Self::apply_node_update(&tx, &node_id, &finals, now, content_touched_at)?;
                let mut changed_fields = finals.changed_fields;
                if let Some(rels) = input.relations.take() {
                    changed_fields.push("relations".to_string());
                    tx.execute(
                        "DELETE FROM intelligence_edges WHERE from_node_id = ?1",
                        rusqlite::params![&node_id],
                    )?;
                    Self::insert_intelligence_relations(&tx, &node_id, rels, now)?;
                }
                changed_fields
            }
        };
        changed_fields.push("updated_at".to_string());

        tx.commit()?;
        drop(conn);
        let record = self
            .get_intelligence_node(&node_id)?
            .ok_or_else(|| anyhow!("Failed to load intelligence node '{node_id}'"))?;
        // Write-time signals, computed after the write lands: duplicate
        // candidates (shared citations, then lexical ranking) and references
        // to existing nodes that were mentioned but not declared as edges.
        let duplicates = self.detect_duplicate_candidates(&record);
        let undeclared_references = self.extract_undeclared_references(&record)?;
        Ok(UpsertResult {
            record,
            created: was_created,
            duplicates,
            undeclared_references,
            changed_fields,
        })
    }

    /// Validate the cheap, caller-facing invariants before any write.
    fn validate_intelligence_node_input(input: &IntelligenceNodeInput) -> Result<()> {
        // 1. Provided values are validated first: cheapest checks, most
        // likely mistakes. `None` means "not mentioned" and skips validation.
        if let Some(ref k) = input.kind {
            validate_knowledge_kind(k)?;
        }

        // 2. A provided status must be a known value. `None` resolves below
        // (create → "noted", update → preserve existing).
        if let Some(ref s) = input.status {
            validate_node_status(s)?;
        }

        // 3. New edges must use the knowledge-edge vocabulary. Existing
        // stored edges are NOT validated retroactively: rejecting them on
        // read would break every graph walk over the old corpus.
        if let Some(ref rels) = input.relations {
            for rel in rels {
                validate_knowledge_edge_relation(&rel.relation)?;
            }
        }

        // 4. `body` and `body_replace` are mutually exclusive: one names the
        // whole new text, the other names a fragment of the old text.
        if input.body.is_some() && input.body_replace.is_some() {
            anyhow::bail!(
                "body and body_replace are mutually exclusive: send either the full new body or a fragment replacement, not both"
            );
        }
        if let Some(ref br) = input.body_replace {
            if br.fragment.is_empty() {
                anyhow::bail!("body_replace fragment must not be empty");
            }
        }
        Ok(())
    }

    /// Resolve the stored values an update keeps or replaces, and record which
    /// fields changed.
    fn resolve_updated_fields(
        prev: IntelligenceNodeRecord,
        input: &IntelligenceNodeInput,
        node_id: &str,
    ) -> Result<IntelligenceNodeFinals> {
        let mut changed_fields: Vec<String> = Vec::new();

        let final_kind = match input.kind.as_ref() {
            Some(k) => {
                if k != &prev.kind {
                    changed_fields.push("kind".to_string());
                }
                k.clone()
            }
            None => prev.kind,
        };
        let final_status = match input.status.as_ref() {
            Some(s) => {
                if s != &prev.status {
                    changed_fields.push("status".to_string());
                }
                s.clone()
            }
            None => prev.status,
        };
        let final_title = match input.title.as_ref() {
            Some(t) => {
                if t != &prev.title {
                    changed_fields.push("title".to_string());
                }
                t.clone()
            }
            None => prev.title,
        };
        let final_body = match (input.body.as_ref(), input.body_replace.as_ref()) {
            (Some(b), None) => {
                if b != &prev.body {
                    changed_fields.push("body".to_string());
                }
                b.clone()
            }
            (None, Some(br)) => {
                // Literal match only — never a regular expression. Zero
                // matches or more than one match fails the whole write,
                // changing nothing, instead of guessing.
                let count = prev.body.matches(br.fragment.as_str()).count();
                if count == 0 {
                    anyhow::bail!(
                        "body_replace fragment not found: no occurrence of the given fragment in node '{node_id}', nothing was changed"
                    );
                }
                if count > 1 {
                    anyhow::bail!(
                        "body_replace fragment is ambiguous: matches {count} times in node '{node_id}', nothing was changed"
                    );
                }
                let replaced = prev
                    .body
                    .replacen(br.fragment.as_str(), br.replacement.as_str(), 1);
                if replaced != prev.body {
                    changed_fields.push("body".to_string());
                }
                replaced
            }
            (None, None) => prev.body,
            // Rejected during validation above; unreachable here.
            (Some(_), Some(_)) => {
                anyhow::bail!(
                    "body and body_replace are mutually exclusive: send either the full new body or a fragment replacement, not both"
                );
            }
        };
        // Doubly-optional: `None` = not mentioned (keep), `Some(None)` =
        // clear, `Some(Some(v))` = set.
        let final_metadata = match input.metadata.as_ref() {
            Some(inner) => {
                let as_str = inner.as_ref().map(|value| value.to_string());
                if as_str != prev.metadata {
                    changed_fields.push("metadata".to_string());
                }
                as_str
            }
            None => prev.metadata,
        };
        let final_project_hash = match input.project_hash.as_ref() {
            Some(inner) => {
                if inner != &prev.project_hash {
                    changed_fields.push("project_hash".to_string());
                }
                inner.clone()
            }
            None => prev.project_hash,
        };
        let final_session_id = match input.session_id.as_ref() {
            Some(inner) => {
                if inner != &prev.session_id {
                    changed_fields.push("session_id".to_string());
                }
                inner.clone()
            }
            None => prev.session_id,
        };

        Ok(IntelligenceNodeFinals {
            kind: final_kind,
            status: final_status,
            title: final_title,
            body: final_body,
            metadata: final_metadata,
            project_hash: final_project_hash,
            session_id: final_session_id,
            changed_fields,
        })
    }

    /// `superseded` requires a `supersedes` edge. On update the edge may
    /// already be stored: only a write that replaces the edge list must carry
    /// the edge itself; a write that leaves relations untouched is checked
    /// against the stored edges.
    fn ensure_supersedes_available(
        tx: &rusqlite::Transaction,
        node_id: &str,
        final_status: &str,
        relations: Option<&[IntelligenceRelationInput]>,
    ) -> Result<()> {
        if final_status != "superseded" {
            return Ok(());
        }
        let has_supersedes = match relations {
            Some(rels) => rels.iter().any(|r| r.relation == "supersedes"),
            None => {
                let mut stmt = tx.prepare(
                    "SELECT COUNT(*) FROM intelligence_edges WHERE from_node_id = ?1 AND relation = 'supersedes'",
                )?;
                let count: i64 = stmt.query_row(rusqlite::params![node_id], |row| row.get(0))?;
                count > 0
            }
        };
        if !has_supersedes {
            anyhow::bail!(
                "status 'superseded' requires a 'supersedes' edge pointing to the replacing node"
            );
        }
        Ok(())
    }

    /// Execute the dynamic `UPDATE intelligence_nodes` for an update, setting
    /// only the columns that changed plus `updated_at` / `content_touched_at`.
    fn apply_node_update(
        tx: &rusqlite::Transaction,
        node_id: &str,
        finals: &IntelligenceNodeFinals,
        now: i64,
        content_touched_at: i64,
    ) -> Result<()> {
        // Each entry is (SET clause, boxed value) so the statement text and
        // the parameter list are built together.
        let mut set_clauses: Vec<&str> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if finals.changed_fields.iter().any(|f| f == "kind") {
            set_clauses.push("kind = ?");
            values.push(Box::new(finals.kind.clone()));
        }
        if finals.changed_fields.iter().any(|f| f == "status") {
            set_clauses.push("status = ?");
            values.push(Box::new(finals.status.clone()));
        }
        if finals.changed_fields.iter().any(|f| f == "title") {
            set_clauses.push("title = ?");
            values.push(Box::new(finals.title.clone()));
        }
        if finals.changed_fields.iter().any(|f| f == "body") {
            set_clauses.push("body = ?");
            values.push(Box::new(finals.body.clone()));
        }
        if finals.changed_fields.iter().any(|f| f == "metadata") {
            set_clauses.push("metadata = ?");
            values.push(Box::new(finals.metadata.clone()));
        }
        if finals.changed_fields.iter().any(|f| f == "project_hash") {
            set_clauses.push("project_hash = ?");
            values.push(Box::new(finals.project_hash.clone()));
        }
        if finals.changed_fields.iter().any(|f| f == "session_id") {
            set_clauses.push("session_id = ?");
            values.push(Box::new(finals.session_id.clone()));
        }
        set_clauses.push("updated_at = ?");
        values.push(Box::new(now));
        set_clauses.push("content_touched_at = ?");
        values.push(Box::new(content_touched_at));
        let sql = format!(
            "UPDATE intelligence_nodes SET {} WHERE id = ?",
            set_clauses.join(", ")
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v.as_ref()).collect();
        // The WHERE id is the trailing parameter after every SET value.
        let mut all: Vec<&dyn rusqlite::types::ToSql> = params;
        all.push(&node_id);
        tx.execute(&sql, all.as_slice())?;
        Ok(())
    }

    /// Insert the edges of a node's relation list.
    fn insert_intelligence_relations(
        tx: &rusqlite::Transaction,
        node_id: &str,
        relations: Vec<IntelligenceRelationInput>,
        now: i64,
    ) -> Result<()> {
        for relation in relations {
            tx.execute(
                "INSERT INTO intelligence_edges (
                    from_node_id, to_node_id, relation, weight, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    node_id,
                    relation.to_node_id,
                    relation.relation,
                    relation.weight.unwrap_or(1.0_f64),
                    now
                ],
            )?;
        }
        Ok(())
    }

    /// Create a brand-new intelligence node and its edges. Returns the fields
    /// the create reported as changed (without `updated_at`, which the caller
    /// appends for both paths).
    fn create_intelligence_node(
        tx: &rusqlite::Transaction,
        node_id: &str,
        input: IntelligenceNodeInput,
        now: i64,
        content_touched_at: i64,
    ) -> Result<Vec<String>> {
        let IntelligenceNodeInput {
            kind,
            status: input_status,
            title,
            body,
            body_replace,
            metadata,
            project_hash,
            session_id,
            relations,
            id: _,
        } = input;

        // ── Create path: kind, title and body are required. ──
        let kind = kind.ok_or_else(|| {
            anyhow!("create requires kind, title and body: no node with this id exists and one of them was omitted")
        })?;
        let title = title.ok_or_else(|| {
            anyhow!("create requires kind, title and body: no node with this id exists and one of them was omitted")
        })?;
        let body = body.ok_or_else(|| {
            anyhow!("create requires kind, title and body: no node with this id exists and one of them was omitted")
        })?;
        // A create cannot carry a fragment replacement: there is no
        // existing body to match against.
        if let Some(br) = body_replace {
            anyhow::bail!(
                "body_replace needs an existing node: no node with id '{node_id}' exists (fragment '{}')",
                br.fragment
            );
        }
        let status = input_status.unwrap_or_else(|| "noted".to_string());
        let metadata_str = metadata.flatten().map(|value| value.to_string());
        let project_hash = project_hash.flatten();
        let session_id = session_id.flatten();

        // `superseded` is meaningless without the pointer to what holds
        // instead: it requires a `supersedes` edge in the same write.
        if status == "superseded" {
            let has_supersedes = relations
                .as_ref()
                .map(|rels| rels.iter().any(|r| r.relation == "supersedes"))
                .unwrap_or(false);
            if !has_supersedes {
                anyhow::bail!(
                    "status 'superseded' requires a 'supersedes' edge pointing to the replacing node"
                );
            }
        }

        tx.execute(
            "INSERT INTO intelligence_nodes (
                id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at, content_touched_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                node_id,
                kind,
                status,
                title,
                body,
                metadata_str,
                project_hash,
                session_id,
                now,
                now,
                content_touched_at
            ],
        )?;

        let mut changed_fields = vec![
            "kind".to_string(),
            "status".to_string(),
            "title".to_string(),
            "body".to_string(),
        ];
        if metadata_str.is_some() {
            changed_fields.push("metadata".to_string());
        }
        if project_hash.is_some() {
            changed_fields.push("project_hash".to_string());
        }
        if session_id.is_some() {
            changed_fields.push("session_id".to_string());
        }
        let relations_changed = relations.is_some();
        if let Some(rels) = relations {
            Self::insert_intelligence_relations(tx, node_id, rels, now)?;
        }
        if relations_changed {
            changed_fields.push("relations".to_string());
        }
        Ok(changed_fields)
    }

    /// Same-kind nodes (excluding `exclude_id`) whose body mentions
    /// `citation`. Backs signal 1 of [`Database::detect_duplicate_candidates`].
    /// Selects only the fields a duplicate candidate reports plus a bounded
    /// head of the body (500 chars, well over `DUPLICATE_EXCERPT_MAX_CHARS`)
    /// to build the excerpt from — the WHERE clause still matches against
    /// the full `body` column, but the full column is never pulled into
    /// this process. CB45: this query previously selected the whole node,
    /// body and metadata included, purely to discard both past the id.
    pub(super) fn list_nodes_citing(
        &self,
        citation: &str,
        kind: &str,
        exclude_id: &str,
    ) -> Result<Vec<DuplicateCandidate>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, kind, title, project_hash, substr(body, 1, 500)
             FROM intelligence_nodes
             WHERE kind = ?1 AND id != ?2 AND instr(body, ?3) > 0
             LIMIT 10",
        )?;
        let rows = stmt.query_map(rusqlite::params![kind, exclude_id, citation], |row| {
            let body_head: String = row.get(4)?;
            Ok(DuplicateCandidate {
                id: row.get(0)?,
                kind: row.get(1)?,
                title: row.get(2)?,
                project_hash: row.get(3)?,
                excerpt: duplicate_excerpt(&body_head),
            })
        })?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    /// Ids of existing nodes mentioned in `node`'s body that are not
    /// declared as outgoing edges. Returns the extracted candidates so
    /// declaring them costs nothing. Empty when the node has no edges at
    /// all — a generic orphan warning fires constantly and gets ignored.
    fn extract_undeclared_references(&self, node: &IntelligenceNodeRecord) -> Result<Vec<String>> {
        let declared: HashSet<String> = self
            .list_recent_intelligence_edges(&node.id)?
            .into_iter()
            .filter(|edge| edge.from_node_id == node.id)
            .map(|e| e.to_node_id)
            .collect();
        let mut refs: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut reported: HashSet<String> = HashSet::new();
        for candidate in extract_node_id_references(&node.body) {
            if candidate == node.id || !seen.insert(candidate.clone()) {
                continue;
            }
            if declared.contains(&candidate) {
                continue;
            }
            // Only warn about references to nodes that actually exist —
            // anything else is prose, not a missing edge. Prefixes resolve
            // so short citations (`→ f9f487d9`) still match. Ambiguous or
            // absent prefixes are skipped: a warning must never fail a write.
            let Ok(Some(resolved)) = self.resolve_node_id_by_prefix(&candidate) else {
                continue;
            };
            if resolved == node.id || declared.contains(&resolved) {
                continue;
            }
            if reported.insert(resolved.clone()) {
                refs.push(resolved);
            }
        }
        Ok(refs)
    }

    pub fn get_intelligence_node(&self, id: &str) -> Result<Option<IntelligenceNodeRecord>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at
             FROM intelligence_nodes WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::read_intelligence_node(row)?))
        } else {
            Ok(None)
        }
    }

    /// Remove an intelligence node and every relation touching it, in one
    /// transaction.
    ///
    /// Hard delete, not a soft-delete/tombstone: the graph is meant to be
    /// able to forget bad or superseded knowledge, and a tombstone column
    /// would require every read path (search, graph walk, context assembly)
    /// to filter it out in the same change — a half-applied filter would
    /// hide deleted nodes from search while `intelligence_get_context` kept
    /// injecting them, which is worse than no deletion at all.
    ///
    /// Edges are not deleted by hand here: `intelligence_edges` declares
    /// `ON DELETE CASCADE` on both `from_node_id` and `to_node_id`, and
    /// `PRAGMA foreign_keys=ON` is set on every connection (see
    /// `Database::new`), so deleting the node row cascades through both
    /// directions automatically — a manual `DELETE FROM intelligence_edges`
    /// alongside it would be duplicated logic that can drift. The relation
    /// count returned to callers is captured before the delete since the
    /// cascade itself doesn't report how many rows it removed. Both
    /// `intelligence_edges` FK columns already have covering indexes
    /// (`idx_intelligence_edges_from` / `idx_intelligence_edges_to`), so
    /// this lookup and the cascade are both index-driven rather than a full
    /// table scan.
    ///
    /// Returns `Ok(None)` if no node with `id` exists, so callers can
    /// distinguish "already gone" from a successful delete rather than
    /// treating a no-op as success.
    pub fn delete_intelligence_node(&self, id: &str) -> Result<Option<usize>> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;

        let relations_removed: i64 = tx.query_row(
            "SELECT COUNT(*) FROM intelligence_edges WHERE from_node_id = ?1 OR to_node_id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )?;

        let rows_deleted = tx.execute(
            "DELETE FROM intelligence_nodes WHERE id = ?1",
            rusqlite::params![id],
        )?;

        if rows_deleted == 0 {
            return Ok(None);
        }

        tx.commit()?;
        Ok(Some(relations_removed as usize))
    }

    /// Fetch a single relation (edge) by ID.
    pub fn get_intelligence_edge(&self, edge_id: i64) -> Result<Option<IntelligenceEdgeRecord>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, from_node_id, to_node_id, relation, weight, created_at
             FROM intelligence_edges WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![edge_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::read_intelligence_edge(row)?))
        } else {
            Ok(None)
        }
    }

    /// Remove a single relation (edge) without touching either endpoint
    /// node. Returns `false` if no edge with `edge_id` exists.
    pub fn delete_intelligence_edge(&self, edge_id: i64) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows_deleted = conn.execute(
            "DELETE FROM intelligence_edges WHERE id = ?1",
            rusqlite::params![edge_id],
        )?;
        Ok(rows_deleted > 0)
    }

    pub fn list_intelligence_nodes(
        &self,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<IntelligenceNodeRecord>> {
        // The bitácora lives in its own table, distinguishable from curated
        // knowledge from the first commit — never in `intelligence_nodes`.
        if kind == Some("activity") {
            return Ok(self
                .list_recent_activity_log_entries(limit)?
                .iter()
                .map(Self::activity_entry_to_intelligence_record)
                .collect());
        }
        // The whole node read (guard + statement + row collection) runs in
        // one scope so the mutex guard is released before the bitácora read
        // below (which takes the lock itself — nesting would deadlock).
        let mut nodes: Vec<IntelligenceNodeRecord> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt = if kind.is_some() {
                conn.prepare(
                    "SELECT id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at
                     FROM intelligence_nodes
                     WHERE kind = ?1
                     ORDER BY updated_at DESC
                     LIMIT ?2",
                )?
            } else {
                conn.prepare(
                    "SELECT id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at
                     FROM intelligence_nodes
                     ORDER BY updated_at DESC
                     LIMIT ?1",
                )?
            };

            let rows = if let Some(kind) = kind {
                stmt.query_map(
                    rusqlite::params![kind, limit as i64],
                    Self::read_intelligence_node,
                )?
            } else {
                stmt.query_map(
                    rusqlite::params![limit as i64],
                    Self::read_intelligence_node,
                )?
            };
            rows.filter_map(|row| row.ok()).collect()
        };
        // Unfiltered listing also surfaces the bitácora alongside curated
        // knowledge — same surface, separate table.
        if kind.is_none() {
            // Fill only the remaining budget so activity entries are not
            // silently dropped when `limit` knowledge nodes already matched.
            let remaining = limit.saturating_sub(nodes.len());
            if remaining > 0 {
                nodes.extend(
                    self.list_recent_activity_log_entries(remaining)?
                        .iter()
                        .map(Self::activity_entry_to_intelligence_record),
                );
            }
            nodes.truncate(limit);
        }
        Ok(nodes)
    }

    pub(super) fn read_intelligence_node(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<IntelligenceNodeRecord> {
        Ok(IntelligenceNodeRecord {
            id: row.get(0)?,
            kind: row.get(1)?,
            status: row.get(2)?,
            title: row.get(3)?,
            body: row.get(4)?,
            metadata: row.get(5)?,
            project_hash: row.get(6)?,
            session_id: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }

    pub(super) fn read_intelligence_edge(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<IntelligenceEdgeRecord> {
        Ok(IntelligenceEdgeRecord {
            id: row.get(0)?,
            from_node_id: row.get(1)?,
            to_node_id: row.get(2)?,
            relation: row.get(3)?,
            weight: row.get(4)?,
            created_at: row.get(5)?,
        })
    }
}
