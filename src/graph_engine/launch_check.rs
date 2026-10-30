use super::*;

impl GraphEngine {
    /// (CB22) Whether `spec` carries usable content: a trimmed non-empty
    /// name or a trimmed non-empty description. A spec with neither is not
    /// executable — the engine rejects it at launch instead of spending
    /// agents on an empty `{{spec_content}}`.
    pub fn spec_has_content(spec: &GraphSpec) -> bool {
        if !spec.name.trim().is_empty() {
            return true;
        }
        if let Some(description) = spec.description.as_deref() {
            if !description.trim().is_empty() {
                return true;
            }
        }
        false
    }

    /// (CB22) Actionable error for a bound or queued spec with no content.
    /// Names the graph and the offending spec (falling back to its id when
    /// its name is blank) so the caller knows exactly which row to fix.
    pub(crate) fn blank_spec_message(graph_name: &str, spec: &GraphSpec) -> String {
        let display = if spec.name.trim().is_empty() {
            spec.id.clone()
        } else {
            spec.name.clone()
        };
        format!(
            "Graph '{}' has a spec '{}' (id '{}') with no content: both name and description \
             are empty. Give the spec a name or description before running.",
            graph_name, display, spec.id
        )
    }

    /// (B17) `Ok(Some(message))` if launching `graph_id` (optionally against
    /// `queue_id`) would find no effective spec to run — `message` is the
    /// actionable, human/LLM-readable error to surface. `Ok(None)` means the
    /// launch may proceed.
    ///
    /// Exposed (not just inlined in [`Self::run_graph_dispatch`]) so the
    /// synchronous `graph_control` MCP handler can hand this straight back to its
    /// caller instead of the caller only finding out via a log line once the
    /// fire-and-forget background dispatch fails — every other launch path
    /// (autorun, cron/watch triggers, `graph_control`) still gets the same
    /// check from `run_graph_dispatch` itself.
    ///
    /// Emptiness is defined per launch mode:
    /// - Bound specs (`queue_id` is `None`): every *non-terminal* bound spec
    ///   must have usable content (CB22 — trimmed non-empty name or
    ///   description); a runnable spec with both blank is rejected before
    ///   the graph is claimed. Blank-name bookkeeping rows (idea / legacy
    ///   placeholders) do not count as an effective bound set. Zero real
    ///   bound specs is an error unless the caller supplied an explicit
    ///   non-empty `idea` (the intentional spec-less API mode). A top-level
    ///   graph alone is not a spec and never substitutes for content.
    /// - A queue (`queue_id` is `Some`): the queue has no `pending` member *and*
    ///   no other non-terminal (`running`/`failed`) member left either — i.e.
    ///   `Database::queue_has_outstanding_members` (any non-completed/skipped member) is false. Unlike bound
    ///   specs, a queue is a shared queue another graph or a stale relaunch can
    ///   easily point at by mistake, so "every member already done" is
    ///   treated as an error here rather than a silent, do-nothing
    ///   completion (regression (b): a queue run where every member is
    ///   already completed). Only the queue's own selected (non-terminal)
    ///   members are inspected for blank content — unrelated graph-bound
    ///   specs, including blank ones, are ignored. Unaffected by the
    ///   spec-less carve-out above —
    ///   a queue run always needs actual queue members.
    pub fn empty_launch_check(
        &self,
        graph_id: &str,
        queue_id: Option<&str>,
        idea: Option<&str>,
    ) -> Result<Option<String>> {
        let Some(lp) = self.db.get_graph(graph_id)? else {
            // Not-found is handled by the caller (`run_graph_dispatch` bails
            // on it above this check runs; the MCP handler checks it before
            // calling this at all) — nothing to report here.
            return Ok(None);
        };

        if let Some(queue_id) = queue_id {
            if !self.db.queue_has_outstanding_members(queue_id)? {
                return Ok(Some(self.empty_spec_set_message(&lp, Some(queue_id))?));
            }
            // (CB22) Validate only the queue's own effective members: a
            // blank selected member is rejected, while unrelated graph-bound
            // specs are ignored entirely.
            for spec_id in self.db.list_queue_member_spec_ids(queue_id)? {
                let Some(spec) = self.db.get_graph_spec(&spec_id)? else {
                    continue;
                };
                if matches!(
                    spec.status,
                    GraphSpecStatus::Completed | GraphSpecStatus::Skipped
                ) {
                    continue;
                }
                if !Self::spec_has_content(&spec) {
                    return Ok(Some(Self::blank_spec_message(&lp.name, &spec)));
                }
            }
            return Ok(None);
        }

        let bound = self.db.list_graph_specs(graph_id)?;
        // (CB22) Reject non-terminal bound specs with no content before the
        // graph is claimed — the engine must never execute an empty
        // `{{spec_content}}`. Terminal rows are skipped here so a leftover
        // completed bookkeeping/legacy blank does not block a relaunch that
        // still has real work (and so this matches the queue path above).
        for spec in &bound {
            if matches!(
                spec.status,
                GraphSpecStatus::Completed | GraphSpecStatus::Skipped
            ) {
                continue;
            }
            if !Self::spec_has_content(spec) {
                return Ok(Some(Self::blank_spec_message(&lp.name, spec)));
            }
        }

        // Effective bound work, for the emptiness decision below:
        // - any remaining non-terminal row (already content-checked) is
        //   executable — including a live idea bookkeeping row still mid-run;
        // - terminal rows with a non-empty name are real user specs, so an
        //   all-done relaunch still takes the zero-exec completion path.
        // Blank-name bookkeeping left over from a prior idea/legacy run does
        // *not* count: the next no-idea dispatch must refuse before claim
        // rather than purge-then-fail after flipping the graph to Running.
        let has_effective_bound = bound.iter().any(|spec| {
            if matches!(
                spec.status,
                GraphSpecStatus::Completed | GraphSpecStatus::Skipped
            ) {
                !spec.name.trim().is_empty()
            } else {
                true
            }
        });
        if has_effective_bound {
            return Ok(None);
        }

        // Zero real bound specs: only an explicit non-empty `idea` may supply
        // `spec_content` — and even then only against a top-level graph to
        // execute it (an idea with nothing to feed is still a launch error).
        // A top-level graph alone, without bound specs or an idea, is not
        // executable (CB22).
        let has_idea = idea
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some();
        if has_idea && !self.db.list_graph_nodes_for_graph(graph_id)?.is_empty() {
            return Ok(None);
        }

        Ok(Some(self.empty_spec_set_message(&lp, None)?))
    }

    /// CM29: the spec `graph_control`/autorun would dispatch or resume next,
    /// without actually starting it — used only to exclude that spec from
    /// the dirty-start refusal's "previous spec" search (see
    /// `find_dirty_predecessor_specs`). Mirrors the picking logic in
    /// `run_graph_dispatch` exactly: for a queue, the currently-running
    /// member if any, else the next pending/interrupted one; for bound
    /// specs, the first one (by position) that isn't `Completed`/`Skipped`.
    /// `Ok(None)` means nothing is runnable — the dirty-start check then has
    /// no "next spec" to exclude, which is fine (an empty graph never
    /// matches a `failed`/`interrupted`/`skipped` predecessor query anyway).
    pub fn preview_next_spec_id(
        &self,
        graph_id: &str,
        queue_id: Option<&str>,
    ) -> Result<Option<String>> {
        if let Some(queue_id) = queue_id {
            if let Some(id) = self.db.queue_running_spec_id(queue_id)? {
                return Ok(Some(id));
            }
            return self.db.queue_next_pending_spec_id(queue_id);
        }
        Ok(self
            .db
            .list_graph_specs(graph_id)?
            .into_iter()
            .find(|s| {
                !matches!(
                    s.status,
                    GraphSpecStatus::Completed | GraphSpecStatus::Skipped
                )
            })
            .map(|s| s.id))
    }

    /// CB69/FR1: the display name for a spec in the dirty-start message —
    /// the name when non-blank, otherwise its id. A single implementation
    /// point keeps the two call sites (predecessor and next spec) from
    /// diverging.
    fn dirty_display_name(spec: &GraphSpec) -> String {
        let t = spec.name.trim();
        if t.is_empty() {
            spec.id.clone()
        } else {
            spec.name.clone()
        }
    }

    /// CB69/FR1: the parenthesised detail for the blocking predecessor in the
    /// refusal message, e.g. `spec 'A' (graph 'G1', failed 2026-…)`.
    /// Uses the raw `graph_id` (not a name lookup) to avoid an extra fallible
    /// DB call; "standalone" covers `None` backlog rows the predecessor query
    /// can return. End time falls back to `started_at` for interrupted rows
    /// that may lack `completed_at` after boot reconciliation.
    /// For a queue-driven run (`queue_driven`) the graph tag would wrongly
    /// read "(graph 'standalone', …)" — queue members are standalone rows —
    /// so only the spec name is rendered.
    fn dirty_predecessor_detail(predecessor: &GraphSpec, queue_driven: bool) -> String {
        let display = Self::dirty_display_name(predecessor);
        if queue_driven {
            return format!("spec '{display}'");
        }
        let graph = predecessor.graph_id.as_deref().unwrap_or("standalone");
        let status = predecessor.status.as_str();
        let time = predecessor
            .completed_at
            .map(|t| t.to_rfc3339())
            .or_else(|| predecessor.started_at.map(|t| t.to_rfc3339()))
            .unwrap_or_else(|| "unknown time".to_string());
        format!("spec '{display}' (graph '{graph}', {status} {time})")
    }

    /// FR1: whether two recorded dirty-path sets are the same set. Both ends
    /// come from the same `git status --porcelain` decoding (`spec_end_dirty_paths`)
    /// and are already de-duplicated and capped, so equal length plus full
    /// containment is set equality.
    fn same_dirty_paths(a: &[String], b: &[String]) -> bool {
        a.len() == b.len() && a.iter().all(|path| b.contains(path))
    }

    /// FR1: whether a recorded dirty-path set shares any entry with the paths
    /// currently dirty in the workdir. A record with no paths (never captured,
    /// or captured by an older version) overlaps nothing: it cannot be shown
    /// to be the same dirt, so it must not block a resuming spec.
    fn dirty_paths_overlap(recorded: &[String], current: &[String]) -> bool {
        recorded.iter().any(|path| current.contains(path))
    }

    /// FR1: the predecessor that actually blocks this launch, among the
    /// workdir's non-completing specs. `None` when the current spec is resuming
    /// and the paths currently dirty are exactly the ones its own latest run
    /// recorded (`own`) — that is its own work, not somebody else's. Otherwise
    /// the first (most recently touched) candidate whose recorded paths overlap
    /// `current`, so a stale record naming different files never blocks.
    fn blocking_dirty_predecessor<'a>(
        candidates: &'a [GraphSpec],
        own: &[String],
        resuming: bool,
        current: &[String],
    ) -> Option<&'a GraphSpec> {
        if resuming && Self::same_dirty_paths(own, current) {
            return None;
        }
        candidates.iter().find(|spec| {
            Self::dirty_paths_overlap(spec.spec_end_dirty_paths.as_deref().unwrap_or(&[]), current)
        })
    }

    /// the spec a *directed* `graph_control` (`start_node_id`) targets —
    /// the first spec a FRESH dispatch would actually execute, so the plan
    /// the operator gets back names the spec the run really starts at. Unlike
    /// [`Self::preview_next_spec_id`] this never returns a queue member that
    /// is still `running`: a fresh dispatch re-enters such a member only via
    /// the resumed-queue path (`graph_control`), never through the pick
    /// loop, so planning a directed run at it would report a spec the run
    /// never executes. Bound specs are the same first non-terminal pick as
    /// [`Self::preview_next_spec_id`].
    pub fn preview_directed_run_spec_id(
        &self,
        graph_id: &str,
        queue_id: Option<&str>,
    ) -> Result<Option<String>> {
        match queue_id {
            Some(queue_id) => self.db.queue_next_pending_spec_id(queue_id),
            None => self.preview_next_spec_id(graph_id, None),
        }
    }

    /// CM29/CB69: whether launching the next spec in `workdir` should be refused
    /// because a different, earlier spec on this same workdir ended without
    /// completing and left overlapping dirt (`find_dirty_predecessor_specs` plus
    /// [`Self::blocking_dirty_predecessor`] — the BLOCKING predicate). On
    /// refusal the message names that predecessor; when the next spec is
    /// resuming its own dirt that sentence also names the next spec. On launch
    /// the attribution comes from `find_last_dirty_run_spec` (the spec whose
    /// recorded run in `graph_id` last left the workdir dirty; "dirty before
    /// this graph ran" when none did). Read-only: two DB lookups (plus the next
    /// spec's own row when resuming) and one live `git status --porcelain` call
    /// (captured only when a notice can exist — same short-circuit cost as
    /// before, plus the resume case).
    /// When the launch proceeds the message says WHY: the graph's
    /// `allow_dirty_start` flag, or — when no predecessor blocks and the next
    /// spec is resuming the dirt it left itself — the spec resuming its own
    /// dirty work (that case previously produced no message at all).
    pub async fn dirty_start_check(
        &self,
        graph_id: &str,
        workdir: &str,
        next_spec_id: Option<&str>,
        allow_dirty_start: bool,
    ) -> Result<Option<DirtyStartNotice>> {
        // Cheap predicate first: the workdir's non-completing specs (the
        // current spec excluded). Whether any of them blocks depends on the
        // paths, so it is decided after the capture below.
        let candidates = self
            .db
            .find_dirty_predecessor_specs(workdir, next_spec_id)?;
        let attributed = self.db.find_last_dirty_run_spec(graph_id)?;
        let resuming = match (&attributed, next_spec_id) {
            (Some(spec), Some(next)) => spec.id == *next,
            _ => false,
        };
        if candidates.is_empty() && !resuming {
            return Ok(None);
        }
        let (dirty, paths) = capture_workdir_dirty_with_paths(workdir).await;
        let Some(count) = dirty.filter(|n| *n > 0) else {
            return Ok(None);
        };
        // FR1: the current spec's own latest recorded dirt. When the workdir
        // still holds exactly that, the dirty work is its own and nobody else
        // is blamed for it.
        let own = match next_spec_id {
            Some(id) => self
                .db
                .get_graph_spec(id)?
                .and_then(|spec| spec.spec_end_dirty_paths)
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let predecessor = Self::blocking_dirty_predecessor(&candidates, &own, resuming, &paths);
        if predecessor.is_none() && !resuming {
            return Ok(None);
        }
        let plural = if count == 1 { "y" } else { "ies" };
        let attribution = match &attributed {
            Some(spec) => {
                let name = if spec.name.trim().is_empty() {
                    spec.id.clone()
                } else {
                    spec.name.clone()
                };
                format!("spec '{name}' left it dirty on its last run in this graph")
            }
            None => "dirty before this graph ran".to_string(),
        };
        let head = format!("workdir '{workdir}' is dirty ({count} entr{plural})");
        let refuse = predecessor.is_some() && !allow_dirty_start;
        let message = if refuse {
            let pred = predecessor.expect("refuse implies a predecessor");
            let queue_driven = self
                .db
                .get_graph(graph_id)?
                .and_then(|lp| lp.active_run_queue_id)
                .is_some();
            let detail = Self::dirty_predecessor_detail(pred, queue_driven);
            if resuming {
                let next_name = match (&attributed, next_spec_id) {
                    (Some(spec), Some(_)) => Self::dirty_display_name(spec),
                    _ => next_spec_id.unwrap_or("unknown").to_string(),
                };
                format!(
                    "{head} — spec '{next_name}' is resuming its own dirty work, but {detail} also ended dirty on this workdir — not launched. Set allow_dirty_start on the graph to launch anyway."
                )
            } else {
                format!("{head} — {detail} also ended dirty on this workdir — not launched. Set allow_dirty_start on the graph to launch anyway.")
            }
        } else if predecessor.is_some() {
            format!(
                "{head} — {attribution} — launched because allow_dirty_start is set on the graph."
            )
        } else {
            format!("{head} — {attribution} — launched: the next spec is the one that left the dirt, resuming its own work.")
        };
        Ok(Some(DirtyStartNotice {
            refuse,
            message,
            paths,
        }))
    }

    /// CM29: called once at daemon boot, right after
    /// `reconcile_orphaned_graphs`/`reconcile_stranded_queue_specs` — those
    /// two live in `src/db/graphs.rs` and do no process I/O, so the
    /// end-of-spec git capture for a spec they just marked `Interrupted`
    /// happens here instead, with real async git access.
    pub async fn capture_end_dirty_for_interrupted_specs(&self) -> Result<()> {
        for (spec, workdir) in self.db.list_interrupted_specs_missing_end_dirty()? {
            let Some(workdir) = workdir else { continue };
            let (dirty, paths) = capture_workdir_dirty_with_paths(&workdir).await;
            if let Err(e) = self.db.set_graph_spec_end_state(&spec.id, dirty, &paths) {
                tracing::warn!(
                    "Failed to record end-dirty state for interrupted spec '{}': {}",
                    spec.id,
                    e
                );
            }
        }
        Ok(())
    }
}

/// FR2: the blocker a queue run pauses on when the dirty-tree guard refuses the
/// next spec. Keeps the guard's own text, lists the first 20 dirty paths, and
/// names the ways out.
pub(crate) fn dirty_advance_blocker(next_spec: &str, message: &str, paths: &[String]) -> String {
    const LISTED: usize = 20;
    let mut listed = paths
        .iter()
        .take(LISTED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > LISTED {
        listed.push_str(&format!(", ... (+{} more)", paths.len() - LISTED));
    }
    format!(
        "{message} Dirty paths: {listed}. Ways out: `graph_control continue` with mode \
         `run_from_node` on spec '{next_spec}'; clean the tree by hand, then `graph_control continue` \
         with mode `skip_next_spec`; or set `allow_dirty_start` on the graph."
    )
}
