use super::*;

impl Database {
    /// CB43: platform+model pairs this installation has actually run since
    /// `since`, across every run table (graph node runs, hook runs,
    /// background agent runs, subagent runs). One lock acquisition; each
    /// branch is bounded by its `started_at` predicate. Ordered by most
    /// recent use, capped at `limit`. Rows with no recorded pair
    /// (`executed_platform IS NULL` — every run predating CB43) are omitted,
    /// never guessed at. Never reads a node's current config.
    pub fn list_recent_platform_model_usage(
        &self,
        since: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<RecentModelUsage>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        let since_epoch = since.timestamp();
        let mut grouped: HashMap<(String, Option<String>), Vec<RecentUsageSample>> = HashMap::new();

        // graph_runs + graph_completion_hook_runs store epoch INTEGERs.
        for table in ["graph_runs", "graph_completion_hook_runs"] {
            let sql = format!(
                "SELECT executed_platform, executed_model, status, started_at FROM {table} \
                 WHERE executed_platform IS NOT NULL AND started_at >= ?1"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![since_epoch], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (platform, model, status, started_at) = row?;
                let started_at = from_timestamp(started_at)?;
                grouped
                    .entry((platform, model))
                    .or_default()
                    .push(RecentUsageSample { started_at, status });
            }
        }

        // runs + subagent_runs store RFC3339 TEXT timestamps.
        Self::collect_rfc3339_usage_samples(
            &conn,
            "SELECT executed_platform, executed_model, status, started_at FROM runs \
             WHERE executed_platform IS NOT NULL",
            since,
            &mut grouped,
        )?;

        // CB43: `subagent_runs.platform`/`model` are the pair resolved at
        // dispatch (see schema comment); `platform` is NOT NULL by schema so
        // every row in the window counts. NULL model = the CLI's default.
        Self::collect_rfc3339_usage_samples(
            &conn,
            "SELECT platform, model, status, started_at FROM subagent_runs",
            since,
            &mut grouped,
        )?;

        let mut out: Vec<RecentModelUsage> = grouped
            .into_iter()
            .map(|((platform, model), mut samples)| {
                samples.sort_by_key(|sample| std::cmp::Reverse(sample.started_at));
                let last = &samples[0];
                RecentModelUsage {
                    last_run: last.started_at,
                    count: samples.len() as i64,
                    last_outcome: last.status.clone(),
                    platform,
                    model,
                }
            })
            .collect();
        out.sort_by_key(|usage| std::cmp::Reverse(usage.last_run));
        out.truncate(limit.max(0) as usize);
        Ok(out)
    }

    /// Collect RFC3339-timestamped rows (the `runs` / `subagent_runs` shape)
    /// into `grouped`, dropping unparseable rows and rows older than `since`.
    pub(super) fn collect_rfc3339_usage_samples(
        conn: &rusqlite::Connection,
        sql: &str,
        since: DateTime<Utc>,
        grouped: &mut HashMap<(String, Option<String>), Vec<RecentUsageSample>>,
    ) -> Result<()> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (platform, model, status, started_at): (String, Option<String>, String, String) =
                row?;
            let Ok(started_at) =
                chrono::DateTime::parse_from_rfc3339(&started_at).map(|dt| dt.with_timezone(&Utc))
            else {
                continue;
            };
            if started_at < since {
                continue;
            }
            grouped
                .entry((platform, model))
                .or_default()
                .push(RecentUsageSample { started_at, status });
        }
        Ok(())
    }

    /// Return every recent run event with the identity that produced it.
    /// This is the installation-wide source data for `agent_probe_recent`;
    /// callers group it by platform and model while retaining attribution.
    pub fn list_recent_platform_model_usage_sources(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<RecentPairSource>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let since_epoch = since.timestamp();
        let mut out = Vec::new();
        Self::collect_graph_node_usage_sources(&conn, since_epoch, &mut out)?;
        Self::collect_completion_hook_usage_sources(&conn, since_epoch, &mut out)?;
        Self::collect_background_agent_usage_sources(&conn, since, &mut out)?;
        Self::collect_subagent_usage_sources(&conn, since, &mut out)?;
        Ok(out)
    }

    /// `graph_runs` joined to its graph/node identity (epoch timestamps).
    pub(super) fn collect_graph_node_usage_sources(
        conn: &rusqlite::Connection,
        since_epoch: i64,
        out: &mut Vec<RecentPairSource>,
    ) -> Result<()> {
        let mut stmt = conn.prepare(
            "SELECT gr.executed_platform, gr.executed_model, gr.status, gr.started_at,
                    g.id, g.name, gn.id, gn.name
             FROM graph_runs gr
             JOIN graphs g ON g.id = gr.graph_id
             JOIN graph_nodes gn ON gn.id = gr.node_id
             WHERE gr.executed_platform IS NOT NULL AND gr.started_at >= ?1",
        )?;
        let rows = stmt.query_map(params![since_epoch], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        for row in rows {
            let (platform, model, status, started_at, graph_id, graph_name, node_id, node_name) =
                row?;
            out.push(RecentPairSource {
                platform,
                model,
                status,
                started_at: from_timestamp(started_at)?,
                source: RecentPairSourceKind::GraphNode {
                    graph_id,
                    graph_name,
                    node_id,
                    node_name,
                },
            });
        }
        Ok(())
    }

    /// `graph_completion_hook_runs` joined to its graph identity.
    pub(super) fn collect_completion_hook_usage_sources(
        conn: &rusqlite::Connection,
        since_epoch: i64,
        out: &mut Vec<RecentPairSource>,
    ) -> Result<()> {
        let mut stmt = conn.prepare(
            "SELECT h.executed_platform, h.executed_model, h.status, h.started_at,
                    g.id, g.name, h.event, h.hook_index
             FROM graph_completion_hook_runs h
             JOIN graphs g ON g.id = h.graph_id
             WHERE h.executed_platform IS NOT NULL AND h.started_at >= ?1",
        )?;
        let rows = stmt.query_map(params![since_epoch], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?;
        for row in rows {
            let (platform, model, status, started_at, graph_id, graph_name, event, hook_index) =
                row?;
            out.push(RecentPairSource {
                platform,
                model,
                status,
                started_at: from_timestamp(started_at)?,
                source: RecentPairSourceKind::CompletionHook {
                    graph_id,
                    graph_name,
                    event,
                    hook_index,
                },
            });
        }
        Ok(())
    }

    /// `runs` joined to the background agent that produced the prompt
    /// (RFC3339 timestamps).
    pub(super) fn collect_background_agent_usage_sources(
        conn: &rusqlite::Connection,
        since: DateTime<Utc>,
        out: &mut Vec<RecentPairSource>,
    ) -> Result<()> {
        let mut stmt = conn.prepare(
            "SELECT r.executed_platform, r.executed_model, r.status, r.started_at,
                    r.background_agent_id, a.prompt
             FROM runs r JOIN agents a ON a.id = r.background_agent_id
             WHERE r.executed_platform IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        for row in rows {
            let (platform, model, status, started_at, agent_id, prompt) = row?;
            let Ok(started_at) =
                DateTime::parse_from_rfc3339(&started_at).map(|dt| dt.with_timezone(&Utc))
            else {
                continue;
            };
            if started_at < since {
                continue;
            }
            out.push(RecentPairSource {
                platform,
                model,
                status,
                started_at,
                source: RecentPairSourceKind::BackgroundAgent { agent_id, prompt },
            });
        }
        Ok(())
    }

    /// `subagent_runs` (RFC3339 timestamps).
    pub(super) fn collect_subagent_usage_sources(
        conn: &rusqlite::Connection,
        since: DateTime<Utc>,
        out: &mut Vec<RecentPairSource>,
    ) -> Result<()> {
        let mut stmt =
            conn.prepare("SELECT platform, model, status, started_at, workdir FROM subagent_runs")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (platform, model, status, started_at, workdir) = row?;
            let Ok(started_at) =
                DateTime::parse_from_rfc3339(&started_at).map(|dt| dt.with_timezone(&Utc))
            else {
                continue;
            };
            if started_at < since {
                continue;
            }
            out.push(RecentPairSource {
                platform,
                model,
                status,
                started_at,
                source: RecentPairSourceKind::SubagentSpawn { workdir },
            });
        }
        Ok(())
    }

    /// Most recent `graph_runs.started_at` per graph, across every graph in a
    /// single query — the sidebar's "last activity" signal. Unlike
    /// [`Self::list_graph_specs`] (a graph's own bound specs, empty for a
    /// queue-driven run whose specs live on the queue instead), this reads
    /// `graph_runs.graph_id`, which is set on every insert regardless of how
    /// the spec was bound (see [`Self::list_graph_runs_for_graph`]), so it
    /// reflects real execution for every graph kind. A graph absent from the
    /// returned map has never recorded a run.
    pub fn list_graph_last_run_times(&self) -> Result<HashMap<String, DateTime<Utc>>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt =
            conn.prepare("SELECT graph_id, MAX(started_at) FROM graph_runs GROUP BY graph_id")?;
        let rows = stmt.query_map(params![], |row| {
            let graph_id: String = row.get(0)?;
            let started_at: i64 = row.get(1)?;
            Ok((graph_id, started_at))
        })?;
        let mut result = HashMap::new();
        for row in rows {
            let (graph_id, started_at) = row?;
            result.insert(graph_id, from_timestamp(started_at)?);
        }
        Ok(result)
    }
}
