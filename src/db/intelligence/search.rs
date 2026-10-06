use super::*;

impl Database {
    /// Write-time duplicate candidates for `node`, strongest signal first.
    ///
    /// 1. **Shared citations** — same-kind nodes citing the same `file:line`
    ///    or the same node ids. Deterministic and precise for this corpus.
    /// 2. **Lexical ranking** — same-kind nodes scoring at or above
    ///    [`DUPLICATE_SIMILARITY_THRESHOLD`] against the node's title plus
    ///    the first ~100 chars of its body.
    ///
    /// Candidates are a warning, never a rejection. Best-effort: any
    /// internal lookup failure yields fewer candidates, not a failed write.
    /// Returns every match found (uncapped, deduped by id) — CB45's
    /// reporting cap is applied by the caller (the MCP handler), not here,
    /// so this method's result stays the authoritative "how many total".
    pub(super) fn detect_duplicate_candidates(
        &self,
        node: &IntelligenceNodeRecord,
    ) -> Vec<DuplicateCandidate> {
        let mut seen: HashSet<String> = HashSet::from([node.id.clone()]);
        let mut candidates: Vec<DuplicateCandidate> = Vec::new();

        // Signal 1: shared citations. This includes both file:line citations
        // and references to other knowledge-node ids.
        let mut shared_references = extract_citations(&node.body);
        shared_references.extend(extract_node_id_references(&node.body));
        for citation in shared_references {
            self.push_citation_duplicates(&citation, node, &mut seen, &mut candidates);
        }

        // Signal 2: lexical ranking over title + body head, same kind only.
        // search_intelligence_nodes is shared with intelligence_search and
        // out of scope to change; its hits carry full bodies because
        // lexical_duplicate_score needs the full text to score against, not
        // just an excerpt — that body is used here, then excerpted only for
        // the candidates that actually clear the threshold.
        let body_head: String = node.body.chars().take(100).collect();
        let query = format!("{} {body_head}", node.title);
        if let Ok(results) = self.search_intelligence_nodes(&query, Some(&node.kind), 10) {
            let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
            for hit in results.results {
                if !seen.insert(hit.id.clone()) {
                    continue;
                }
                if lexical_duplicate_score(&terms, &hit) >= DUPLICATE_SIMILARITY_THRESHOLD {
                    candidates.push(DuplicateCandidate {
                        id: hit.id.clone(),
                        kind: hit.kind.clone(),
                        title: hit.title.clone(),
                        project_hash: hit.project_hash.clone(),
                        excerpt: duplicate_excerpt(&hit.body),
                    });
                }
            }
        }

        candidates
    }

    /// Add the same-kind nodes citing `citation` to `candidates`, skipping any
    /// already seen. Best-effort: a lookup failure adds nothing.
    fn push_citation_duplicates(
        &self,
        citation: &str,
        node: &IntelligenceNodeRecord,
        seen: &mut HashSet<String>,
        candidates: &mut Vec<DuplicateCandidate>,
    ) {
        let Ok(neighbours) = self.list_nodes_citing(citation, &node.kind, &node.id) else {
            return;
        };
        for neighbour in neighbours {
            if seen.insert(neighbour.id.clone()) {
                candidates.push(neighbour);
            }
        }
    }

    pub fn search_intelligence_nodes(
        &self,
        query: &str,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<IntelligenceSearchResult> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();

        // Always report how many nodes would be considered for this kind filter.
        let examined_count = self.count_intelligence_nodes_for_search(kind)?;

        if terms.is_empty() {
            return Ok(IntelligenceSearchResult {
                results: Vec::new(),
                examined_count,
            });
        }

        // Ranking follows the spec guideline: order primarily by how many
        // distinct query terms match anywhere, then break ties by *where* they
        // match (a title hit outweighs a body hit), then by recency. The WHERE
        // clause keeps OR semantics — any term matching any field is enough, so
        // extra words degrade a node's rank but never drop it from the results.
        // (SQL text is built before taking the mutex; the guard lives only in
        // the query scope below so the later bitácora search — which locks
        // again — cannot deadlock.)
        let mut match_count_parts: Vec<String> = Vec::with_capacity(terms.len());
        let mut score_parts: Vec<String> = Vec::with_capacity(terms.len() * 5);
        let mut or_clauses: Vec<String> = Vec::with_capacity(terms.len() * 5);
        for i in 0..terms.len() {
            let p = i + 2;
            score_parts.push(format!(
                "(CASE WHEN instr(lower(title), ?{p}) > 0 THEN 3 ELSE 0 END)"
            ));
            score_parts.push(format!(
                "(CASE WHEN instr(lower(body), ?{p}) > 0 THEN 1 ELSE 0 END)"
            ));
            score_parts.push(format!(
                "(CASE WHEN instr(lower(coalesce(metadata, '')), ?{p}) > 0 THEN 1 ELSE 0 END)"
            ));
            score_parts.push(format!(
                "(CASE WHEN instr(lower(id), ?{p}) > 0 THEN 1 ELSE 0 END)"
            ));
            score_parts.push(format!(
                "(CASE WHEN instr(lower(kind), ?{p}) > 0 THEN 1 ELSE 0 END)"
            ));

            let term_fields: Vec<String> =
                ["id", "kind", "title", "body", "coalesce(metadata, '')"]
                    .into_iter()
                    .map(|field| format!("instr(lower({field}), ?{p}) > 0"))
                    .collect();
            match_count_parts.push(format!(
                "(CASE WHEN {} THEN 1 ELSE 0 END)",
                term_fields.join(" OR ")
            ));
            or_clauses.extend(term_fields);
        }

        let match_count_expr = match_count_parts.join(" + ");
        let score_expr = score_parts.join(" + ");
        let or_clause = or_clauses.join(" OR ");
        let limit_placeholder = terms.len() + 2;
        let sql = format!(
            "SELECT id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at, \
             ({match_count_expr}) AS match_count, ({score_expr}) AS score \
             FROM intelligence_nodes \
             WHERE (?1 IS NULL OR kind = ?1) AND ({or_clause}) \
             ORDER BY match_count DESC, score DESC, updated_at DESC \
             LIMIT ?{limit_placeholder}"
        );

        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(terms.len() + 2);
        params.push(Box::new(kind.map(str::to_string)));
        for term in &terms {
            params.push(Box::new(term.clone()));
        }
        params.push(Box::new(limit as i64));
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(Box::as_ref).collect();

        // Guard + statement + row collection share one scope so the mutex is
        // released before the bitácora search below.
        let mut results: Vec<IntelligenceNodeRecord> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(param_refs.as_slice(), Self::read_intelligence_node)?;
            rows.filter_map(|row| row.ok()).collect()
        };
        // Bitácora hits ride along with Knowledge search results as
        // structural `kind = "activity"` records — same surface, separate
        // table. No summarising or interpretation on the way in or out.
        if kind.is_none() || kind == Some("activity") {
            let remaining = limit.saturating_sub(results.len());
            if remaining > 0 {
                let activity_hits = self
                    .search_activity_log_entries(query, None, remaining)
                    .unwrap_or_default();
                results.extend(
                    activity_hits
                        .iter()
                        .map(Self::activity_entry_to_intelligence_record),
                );
            }
            results.truncate(limit);
        }
        Ok(IntelligenceSearchResult {
            results,
            examined_count,
        })
    }

    fn count_intelligence_nodes_for_search(&self, kind: Option<&str>) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn
            .prepare("SELECT COUNT(*) FROM intelligence_nodes WHERE (?1 IS NULL OR kind = ?1)")?;
        Ok(stmt.query_row(rusqlite::params![kind], |row| row.get(0))?)
    }

    pub fn list_recent_intelligence_edges(
        &self,
        node_id: &str,
    ) -> Result<Vec<IntelligenceEdgeRecord>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, from_node_id, to_node_id, relation, weight, created_at
             FROM intelligence_edges
             WHERE from_node_id = ?1 OR to_node_id = ?1
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![node_id], Self::read_intelligence_edge)?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    /// Scoped variant of [`Database::search_intelligence_nodes`]: same ranking
    /// SQL, plus `AND project_hash IN (...)`. Empty `hashes` yields an empty
    /// result (with the examined count). The unscoped original is untouched.
    pub fn search_intelligence_nodes_scoped(
        &self,
        query: &str,
        kind: Option<&str>,
        limit: usize,
        hashes: &[String],
    ) -> Result<IntelligenceSearchResult> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let examined_count = self.count_intelligence_nodes_for_search(kind)?;

        if terms.is_empty() || hashes.is_empty() {
            return Ok(IntelligenceSearchResult {
                results: Vec::new(),
                examined_count,
            });
        }

        // (SQL text is built before taking the mutex; the guard lives only in
        // the query scope below so the later bitácora lookups — which lock
        // again — cannot deadlock.)
        let mut match_count_parts: Vec<String> = Vec::with_capacity(terms.len());
        let mut score_parts: Vec<String> = Vec::with_capacity(terms.len() * 5);
        let mut or_clauses: Vec<String> = Vec::with_capacity(terms.len() * 5);
        for i in 0..terms.len() {
            let p = i + 2;
            score_parts.push(format!(
                "(CASE WHEN instr(lower(title), ?{p}) > 0 THEN 3 ELSE 0 END)"
            ));
            score_parts.push(format!(
                "(CASE WHEN instr(lower(body), ?{p}) > 0 THEN 1 ELSE 0 END)"
            ));
            score_parts.push(format!(
                "(CASE WHEN instr(lower(coalesce(metadata, '')), ?{p}) > 0 THEN 1 ELSE 0 END)"
            ));
            score_parts.push(format!(
                "(CASE WHEN instr(lower(id), ?{p}) > 0 THEN 1 ELSE 0 END)"
            ));
            score_parts.push(format!(
                "(CASE WHEN instr(lower(kind), ?{p}) > 0 THEN 1 ELSE 0 END)"
            ));

            let term_fields: Vec<String> =
                ["id", "kind", "title", "body", "coalesce(metadata, '')"]
                    .into_iter()
                    .map(|field| format!("instr(lower({field}), ?{p}) > 0"))
                    .collect();
            match_count_parts.push(format!(
                "(CASE WHEN {} THEN 1 ELSE 0 END)",
                term_fields.join(" OR ")
            ));
            or_clauses.extend(term_fields);
        }

        let match_count_expr = match_count_parts.join(" + ");
        let score_expr = score_parts.join(" + ");
        let or_clause = or_clauses.join(" OR ");
        let limit_placeholder = terms.len() + 2;
        // project_hash placeholders shift with the term count.
        let hash_start = terms.len() + 3;
        let hash_placeholders: Vec<String> = (0..hashes.len())
            .map(|i| format!("?{}", hash_start + i))
            .collect();
        let hash_clause = hash_placeholders.join(", ");
        let sql = format!(
            "SELECT id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at, \
             ({match_count_expr}) AS match_count, ({score_expr}) AS score \
             FROM intelligence_nodes \
             WHERE (?1 IS NULL OR kind = ?1) AND project_hash IN ({hash_clause}) AND ({or_clause}) \
             ORDER BY match_count DESC, score DESC, updated_at DESC \
             LIMIT ?{limit_placeholder}"
        );

        // Placeholder index order: ?1 kind, ?2.. terms, ?{limit} limit,
        // then hash IN-list placeholders (rusqlite binds by index).
        let mut params: Vec<Box<dyn rusqlite::ToSql>> =
            Vec::with_capacity(terms.len() + hashes.len() + 2);
        params.push(Box::new(kind.map(str::to_string)));
        for term in &terms {
            params.push(Box::new(term.clone()));
        }
        params.push(Box::new(limit as i64));
        for hash in hashes {
            params.push(Box::new(hash.clone()));
        }
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(Box::as_ref).collect();

        // Guard + statement + row collection share one scope so the mutex is
        // released before the bitácora lookups below (each takes the lock
        // itself — holding both would deadlock the mutex). Workdir
        // resolution via `get_project` also locks, so it runs after the
        // guard is gone.
        let mut results: Vec<IntelligenceNodeRecord> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(param_refs.as_slice(), Self::read_intelligence_node)?;
            rows.filter_map(|row| row.ok()).collect()
        };
        // Scoped bitácora hits: activity entries whose workdir matches one
        // of the scoped projects' paths.
        if kind.is_none() || kind == Some("activity") {
            for hash in hashes {
                let remaining = limit.saturating_sub(results.len());
                if remaining == 0 {
                    break;
                }
                let workdir = self.get_project(hash).ok().flatten().map(|p| p.path);
                if let Some(workdir) = workdir {
                    let activity_hits = self
                        .search_activity_log_entries(query, Some(&workdir), remaining)
                        .unwrap_or_default();
                    results.extend(
                        activity_hits
                            .iter()
                            .map(Self::activity_entry_to_intelligence_record),
                    );
                }
            }
            results.truncate(limit);
        }
        Ok(IntelligenceSearchResult {
            results,
            examined_count,
        })
    }

    // ── Operational sessions (CM8) ──

    pub fn upsert_operational_session(
        &self,
        input: OperationalSessionInput,
    ) -> Result<(OperationalSessionRecord, bool)> {
        let node_id = input.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let now = Utc::now().timestamp();
        let metadata = input.metadata.map(|value| value.to_string());
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let was_created: bool = {
            let mut stmt =
                conn.prepare("SELECT 1 FROM operational_sessions WHERE id = ?1 LIMIT 1")?;
            let mut rows = stmt.query(rusqlite::params![&node_id])?;
            rows.next()?.is_none()
        };

        conn.execute(
            "INSERT INTO operational_sessions (
                id, title, body, metadata, project_hash, session_id, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                body = excluded.body,
                metadata = excluded.metadata,
                project_hash = excluded.project_hash,
                session_id = excluded.session_id,
                updated_at = excluded.updated_at",
            rusqlite::params![
                node_id,
                input.title,
                input.body,
                metadata,
                input.project_hash,
                input.session_id,
                now,
                now
            ],
        )?;

        drop(conn);
        let record = self
            .get_operational_session(&node_id)?
            .ok_or_else(|| anyhow!("Failed to load operational session '{}'", node_id))?;
        Ok((record, was_created))
    }

    pub fn get_operational_session(&self, id: &str) -> Result<Option<OperationalSessionRecord>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, title, body, metadata, project_hash, session_id, created_at, updated_at
             FROM operational_sessions WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::read_operational_session(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn list_operational_sessions(&self, limit: usize) -> Result<Vec<OperationalSessionRecord>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, title, body, metadata, project_hash, session_id, created_at, updated_at
             FROM operational_sessions
             ORDER BY updated_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![limit as i64],
            Self::read_operational_session,
        )?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    #[allow(dead_code)]
    pub fn list_operational_sessions_by_project(
        &self,
        workdir: &str,
        limit: usize,
    ) -> Result<Vec<OperationalSessionRecord>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, title, body, metadata, project_hash, session_id, created_at, updated_at
             FROM operational_sessions
             WHERE project_hash = ?1 OR json_extract(metadata, '$.workdir') = ?1
             ORDER BY updated_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![workdir, limit as i64],
            Self::read_operational_session,
        )?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    pub fn search_operational_sessions(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<OperationalSessionRecord>> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        let mut match_count_parts: Vec<String> = Vec::with_capacity(terms.len());
        let mut score_parts: Vec<String> = Vec::with_capacity(terms.len() * 4);
        let mut or_clauses: Vec<String> = Vec::with_capacity(terms.len() * 4);
        for i in 0..terms.len() {
            let p = i + 1;
            score_parts.push(format!(
                "(CASE WHEN instr(lower(title), ?{p}) > 0 THEN 3 ELSE 0 END)"
            ));
            score_parts.push(format!(
                "(CASE WHEN instr(lower(body), ?{p}) > 0 THEN 1 ELSE 0 END)"
            ));
            score_parts.push(format!(
                "(CASE WHEN instr(lower(coalesce(metadata, '')), ?{p}) > 0 THEN 1 ELSE 0 END)"
            ));
            score_parts.push(format!(
                "(CASE WHEN instr(lower(id), ?{p}) > 0 THEN 1 ELSE 0 END)"
            ));

            let term_fields: Vec<String> = ["id", "title", "body", "coalesce(metadata, '')"]
                .into_iter()
                .map(|field| format!("instr(lower({field}), ?{p}) > 0"))
                .collect();
            match_count_parts.push(format!(
                "(CASE WHEN {} THEN 1 ELSE 0 END)",
                term_fields.join(" OR ")
            ));
            or_clauses.extend(term_fields);
        }

        let match_count_expr = match_count_parts.join(" + ");
        let score_expr = score_parts.join(" + ");
        let or_clause = or_clauses.join(" OR ");
        let limit_placeholder = terms.len() + 1;
        let sql = format!(
            "SELECT id, title, body, metadata, project_hash, session_id, created_at, updated_at, \
             ({match_count_expr}) AS match_count, ({score_expr}) AS score \
             FROM operational_sessions \
             WHERE ({or_clause}) \
             ORDER BY match_count DESC, score DESC, updated_at DESC \
             LIMIT ?{limit_placeholder}"
        );

        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(terms.len() + 1);
        for term in &terms {
            params.push(Box::new(term.clone()));
        }
        params.push(Box::new(limit as i64));
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(Box::as_ref).collect();

        let rows = stmt.query_map(param_refs.as_slice(), Self::read_operational_session)?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    pub fn prune_operational_sessions(&self, max_age_days: i64) -> Result<u64> {
        let cutoff = Utc::now().timestamp() - (max_age_days * 86400);
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let count = conn.execute(
            "DELETE FROM operational_sessions WHERE updated_at < ?1",
            [cutoff],
        )?;
        Ok(count as u64)
    }

    fn read_operational_session(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<OperationalSessionRecord> {
        Ok(OperationalSessionRecord {
            id: row.get(0)?,
            title: row.get(1)?,
            body: row.get(2)?,
            metadata: row.get(3)?,
            project_hash: row.get(4)?,
            session_id: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }
}
