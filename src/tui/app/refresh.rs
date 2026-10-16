//! App refresh methods moved from the composition root.

use super::*;

impl App {
    pub fn refresh(&mut self) -> Result<()> {
        self.animation_tick = self.animation_tick.wrapping_add(1);
        // The project type-ahead only ever applies to the Knowledge layer's
        // list, so it is dropped here — the one place every way of leaving
        // that layer (arrows, F2, click, Home) passes through.
        self.sync_project_filter_to_layer();
        if self.update_available.is_none() {
            self.update_available = crate::autoupdate::update_available_tag();
        }
        self.refresh_daemon_status();
        self.refresh_agents()?;
        self.refresh_projects()?;
        self.refresh_graphs()?;
        // CT14: correct stale sidebar indices on the existing refresh tick
        // (pure state, no extra redraw) so navigation works after data
        // shrinks between ticks.
        self.normalize_automation_kind();
        self.normalize_agent_section_focus();
        self.clamp_sidebar_selection();
        self.refresh_project_graph().ok();
        self.refresh_rag_state()?;
        self.refresh_active_runs()?;
        self.poll_interactive_agents();
        self.poll_terminal_agents();
        self.deliver_due_scheduled_sends();
        self.tick_banner_animation();
        self.ensure_sidebar_brain();
        self.refresh_log();
        self.auto_hide_sidebar();
        self.system_monitor_active
            .store(self.sidebar_visible, Ordering::Relaxed);
        self.dismiss_copied();
        self.update_whimsg_context();
        self.tick_atmosphere();
        self.tick_missions()?;
        self.resize_interactive_agents();
        self.poll_playground_search();
        self.refresh_playground_search()?;
        self.poll_graph_action();
        self.poll_node_tail_dialog();
        self.dismiss_graph_action_message();
        // CT30: apply any finished Crews-face fetch/write, then start a new
        // one if the face is visible and its throttle allows (D5). Runs
        // before the panel-face tick so the recomputation sees current data.
        self.poll_crews_face();
        self.poll_crews_write();
        self.refresh_crews_face();
        // CM51 (D4): a `config_set` from anywhere, or a hand edit the
        // daemon's watcher saw, publishes a settings revision. When it moves,
        // re-read the settings this TUI renders — no restart needed.
        self.poll_settings_revision();
        // CT31: apply any finished Settings-tab fetch/write, then start a new
        // one if the hub's Settings tab is visible and the throttle allows.
        self.poll_settings_rows();
        self.poll_settings_write();
        self.refresh_settings_rows();
        self.dismiss_settings_message();
        // CT1 multi-face panel: recompute the visible face from the
        // switching rule after every data refresh above has run.
        self.tick_panel_face();
        if let Some(dialog) = self.simple_prompt_dialog.as_mut() {
            dialog.tick_at_picker();
        }

        // Non-blocking check for updated system info from background thread
        while let Ok(info) = self.system_info_rx.try_recv() {
            self.system_info_target = info;
            self.last_system_update = std::time::Instant::now();
        }
        self.interpolate_system_info();

        Ok(())
    }

    /// CM51 (D4): re-read the settings this TUI renders when the daemon
    /// publishes a newer `settings_revision` — from a `config_set`, or from a
    /// hand edit its file watcher saw. The revision row carries the changed
    /// keys, but the cheap and always-correct move is to reload the config
    /// and re-derive theme, temperature unit and the pinned face.
    pub(crate) fn poll_settings_revision(&mut self) {
        let Ok(Some(revision)) = self
            .db
            .get_state(crate::daemon::config_watch::SETTINGS_REVISION_KEY)
        else {
            return;
        };
        if revision == self.settings_seen {
            return;
        }
        self.settings_seen = revision;
        let config = crate::domain::canopy_config::CanopyConfig::load(&self.data_dir);
        self.theme = crate::tui::ui::theme::Theme::resolve(&config.theme);
        self.temperature_unit = config.temperature_unit;
        self.panel_pinned = App::load_panel_pinned_face(&config.pinned_panel_face);
        // CT26: a registry refresh that flips `ctrl_c_exits` publishes a
        // revision; re-deriving here means the guard picks it up without a
        // restart, exactly like theme and the pinned face.
        self.ctrl_c_exits_clis = crate::tui::app::types::ctrl_c_exits_clis(&config);
        self.tui_settings = config.tui;
    }

    pub(crate) fn interpolate_system_info(&mut self) {
        let now = std::time::Instant::now();
        let elapsed = now.saturating_duration_since(self.last_system_frame_at);
        self.last_system_frame_at = now;

        // Blend toward the latest sampled snapshot with a longer window so
        // values keep moving smoothly between monitoring samples.
        let blend = (elapsed.as_secs_f32() / 0.9).clamp(0.0, 1.0);
        if blend <= 0.0 {
            return;
        }

        blend_system_info(&mut self.system_info, &self.system_info_target, blend);
    }

    /// Perform debounced RAG search in playground mode. The search itself
    /// (embedding-model load + embed + vector search) runs on a worker
    /// thread (B23) — this only spawns it; [`Self::poll_playground_search`]
    /// applies the results when they arrive, so the UI never blocks.
    pub(crate) fn refresh_playground_search(&mut self) -> Result<()> {
        const PLAYGROUND_SEARCH_DEBOUNCE_MS: u128 = 2_000;

        if !self.playground_active {
            return Ok(());
        }
        if !self.playground_search_pending {
            return Ok(());
        }
        // One search at a time: results of the in-flight one arrive first,
        // and pending stays true, so a newer query re-triggers right after.
        if self.playground_search_rx.is_some() {
            return Ok(());
        }

        let since_last = self.playground_last_search.elapsed().as_millis();
        if since_last < PLAYGROUND_SEARCH_DEBOUNCE_MS {
            return Ok(());
        }

        let query = self.playground_query.trim().to_string();
        if query.is_empty() {
            self.playground_results.clear();
            self.playground_selected = 0;
            self.playground_last_executed_query.clear();
            self.playground_search_pending = false;
            return Ok(());
        }

        if self.playground_last_executed_query == query {
            self.playground_search_pending = false;
            return Ok(());
        }

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = playground_vector_search(&query, 50);
            let _ = tx.send((query, outcome));
        });
        self.playground_search_rx = Some(rx);
        Ok(())
    }

    /// Apply a finished background playground search, if any (B23). Called
    /// from the tick graph; never blocks.
    pub(crate) fn poll_playground_search(&mut self) {
        let Some(rx) = &self.playground_search_rx else {
            return;
        };
        let (executed_query, outcome) = match rx.try_recv() {
            Ok(message) => message,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.playground_search_rx = None;
                self.playground_search_pending = false;
                return;
            }
        };
        self.playground_search_rx = None;

        // Playground closed (Esc) while the search ran: abandon the result.
        if !self.playground_active {
            self.playground_search_pending = false;
            return;
        }

        if let Ok(results) = outcome {
            let month_ago = chrono::Utc::now().timestamp() - 30 * 24 * 3600;
            for result in &results {
                if result.distance.is_some_and(|d| d < 0.2) {
                    self.queue_mission_event(crate::tui::gamification::MissionEvent::DeepRagSearch);
                }
                if result.created_at < month_ago {
                    self.queue_mission_event(
                        crate::tui::gamification::MissionEvent::DigitalArcheologistFind,
                    );
                }
            }
            self.playground_results = results;
            self.playground_selected = 0;
        }
        self.playground_last_executed_query = executed_query;
        // Leave pending=true if the user kept typing (query changed while
        // the search ran) so the debounce re-triggers with the newer query.
        self.playground_search_pending =
            self.playground_query.trim() != self.playground_last_executed_query;
        self.playground_last_search = std::time::Instant::now();
    }

    // ── Navigation ──────────────────────────────────────────────
    //
    // Arrows never change the active sidebar tab (Live / Automation /
    // Knowledge) — they navigate that tab's own list exclusively, wrapping
    // at both ends. The one exception is the pinned RAG summary above the
    // tab bar: backing off the first item of whichever tab is active moves
    // up to RAG (when it has anything to show), and moving off RAG returns
    // to the first item of that same tab. Inside Knowledge, once a project
    // is entered (`project_focus.is_some()`) arrows navigate that project's
    // active tab list under the same rule (functional requirement 4).
    //
    // `refresh_projects` itself lives in `project_activity.rs`.

    /// Recompute the Knowledge layer's per-project Preview summary cache
    /// (pending backlog count, knowledge entry count, last activity, graph
    /// badge). Cheap aggregate queries over the small `projects` list, run
    /// once per refresh tick — never per keystroke/highlight move
    /// (functional requirement 3).
    pub(crate) fn refresh_project_preview_cache(&mut self) {
        let running_workdirs: HashSet<String> = self
            .sidebar_graphs()
            .iter()
            .filter(|lp| lp.status == GraphStatus::Running)
            .map(|lp| lp.workdir.clone())
            .collect();

        let mut cache = HashMap::new();
        for project in &self.projects {
            let pending_backlog = self
                .db
                .list_specs(Some(project.path.as_str()), None, true)
                .map(|specs| specs.len())
                .unwrap_or(0);
            let knowledge_entries = self
                .db
                .list_project_knowledge(&project.hash, None, 200)
                .map(|nodes| nodes.len())
                .unwrap_or(0);
            let last_activity = self
                .db
                .list_graphs(Some(project.path.as_str()), true)
                .ok()
                .and_then(|graphs| graphs.iter().map(|lp| lp.created_at.timestamp()).max());
            cache.insert(
                project.hash.clone(),
                types::ProjectPreviewSummary {
                    pending_backlog,
                    knowledge_entries,
                    last_activity,
                    graph_running: running_workdirs.contains(&project.path),
                },
            );
        }
        self.project_preview_cache = cache;
    }

    pub(crate) fn selected_project_preview(&self) -> Option<&types::ProjectPreviewSummary> {
        let project = self.selected_project()?;
        self.project_preview_cache.get(&project.hash)
    }

    /// Load (or reload) the persisted History tab entries for the project
    /// with `hash` into the cache.
    pub(crate) fn load_project_history(&mut self, hash: &str) {
        let Some(workdir) = self
            .projects
            .iter()
            .find(|p| p.hash == hash)
            .map(|p| p.path.clone())
        else {
            return;
        };
        let entries = self
            .db
            .list_project_history(&workdir, 100)
            .unwrap_or_default();
        self.project_history_cache.insert(hash.to_string(), entries);
    }

    /// Persisted History entries for the currently selected project, lazily
    /// loading them into the cache on first access.
    pub(crate) fn selected_project_history_entries(
        &mut self,
    ) -> &[crate::db::project::ProjectHistoryEntry] {
        let Some(hash) = self.selected_project().map(|p| p.hash.clone()) else {
            return &[];
        };
        if !self.project_history_cache.contains_key(&hash) {
            self.load_project_history(&hash);
        }
        self.project_history_cache
            .get(&hash)
            .map_or(&[], |v| v.as_slice())
    }

    /// Reload the standalone/backlog specs shown in the sidebar's `Backlog`
    /// section, tag-filtered to the selected project's workdir (or
    /// unfiltered when no project is registered/selected). Runs on the same
    /// cadence as `refresh_projects` — no dedicated polling graph.
    pub(crate) fn refresh_backlog_specs(&mut self) -> Result<()> {
        let workdir_filter = self.selected_project().map(|p| p.path.clone());
        self.backlog_specs = self.db.list_specs(workdir_filter.as_deref(), None, true)?;
        if self.backlog_specs.is_empty() {
            self.selected_backlog = 0;
        } else {
            self.selected_backlog = self.selected_backlog.min(self.backlog_specs.len() - 1);
        }
        Ok(())
    }

    pub fn refresh_project_knowledge(&mut self) -> Result<()> {
        if let Some(project) = self.projects.get(self.selected_project) {
            self.project_knowledge = self.db.list_project_knowledge(&project.hash, None, 50)?;
            self.normalize_selected_knowledge();
        } else {
            self.project_knowledge.clear();
            self.selected_knowledge = 0;
        }
        Ok(())
    }

    pub(crate) fn refresh_graphs(&mut self) -> Result<()> {
        self.graphs = self.db.list_graphs(None, false)?;
        self.archived_graph_count = self.db.count_archived_graphs().unwrap_or(0) as usize;
        if self.graph_view_archived {
            self.archived_graphs = self.db.list_graphs(None, true)?;
            self.archived_graphs.retain(|lp| lp.archived);
        } else {
            self.archived_graphs.clear();
        }
        self.refresh_graph_sidebar_meta();
        self.refresh_graphs_selection();
        Ok(())
    }

    /// Recompute the sidebar's per-graph "last activity" (see
    /// [`GraphSidebarMeta`]) and blocked status (a `Paused` graph whose latest
    /// run recorded a `graph_report_blocker` description). One
    /// `list_graph_last_run_times` query for every graph's last-run time, plus,
    /// for paused graphs only, one `list_graph_runs_for_graph` query — bounded
    /// by the (typically small) number of graphs, run on the existing refresh
    /// cadence rather than a dedicated poller.
    ///
    /// Deliberately reads `graph_runs` rather than `list_graph_specs`: a
    /// queue-driven graph's specs live on the queue, not on the graph's own
    /// `graph_specs` rows, so that query is always empty for it. `graph_runs`
    /// is populated regardless of how the spec was bound.
    pub(crate) fn refresh_graph_sidebar_meta(&mut self) {
        let last_run_times = self.db.list_graph_last_run_times().unwrap_or_default();
        let mut meta = HashMap::new();
        for lp in &self.graphs {
            let running = lp.status == GraphStatus::Running;
            let last_run_at = last_run_times.get(&lp.id).copied();
            let last_activity = last_run_at.unwrap_or(lp.created_at);
            let last_run_label = if running {
                "running".to_string()
            } else {
                match last_run_at {
                    Some(at) => utils::relative_time_compact(&at),
                    None => "never".to_string(),
                }
            };
            let blocked = lp.status == GraphStatus::Paused
                && self
                    .db
                    .list_graph_runs_for_graph(&lp.id)
                    .ok()
                    .and_then(|runs| runs.last().and_then(|run| run.output.clone()))
                    .is_some_and(|output| output.get("blocker").is_some());
            let autorun_label = lp
                .autorun_at
                .map(|at| format!("resumes {}", utils::relative_time_until_compact(&at)));
            meta.insert(
                lp.id.clone(),
                GraphSidebarMeta {
                    last_activity,
                    last_run_label,
                    blocked,
                    autorun_label,
                },
            );
        }
        self.graph_sidebar_meta = meta;
    }

    /// Every graph for the sidebar's `Graphs` section, ordered by last
    /// activity (most recent first) with **no filtering by status** — a
    /// graph that reaches a terminal state stays listed so the operator can
    /// see it failed/completed and act on it (selection and F4's
    /// confirmation flow reach every graph regardless of status). Ties
    /// broken by the existing `created_at DESC` order from `list_graphs`,
    /// since the sort is stable.
    ///
    /// `last_activity` is precomputed by [`Self::refresh_graph_sidebar_meta`]
    /// on the refresh cadence, not queried here, so listing every graph adds
    /// no per-tick database work.
    pub fn sidebar_graphs(&self) -> Vec<&crate::domain::graphs::Graph> {
        if self.graph_view_archived {
            let mut graphs: Vec<&crate::domain::graphs::Graph> =
                self.archived_graphs.iter().collect();
            graphs.sort_by_key(|lp| std::cmp::Reverse(lp.created_at));
            return graphs;
        }
        let mut graphs: Vec<&crate::domain::graphs::Graph> = self.graphs.iter().collect();
        graphs.sort_by_key(|lp| {
            std::cmp::Reverse(
                self.graph_sidebar_meta
                    .get(&lp.id)
                    .map(|meta| meta.last_activity)
                    .unwrap_or(lp.created_at),
            )
        });
        graphs
    }

    pub(crate) fn refresh_graphs_selection(&mut self) {
        let visible = self.visible_graphs();
        if visible.is_empty() {
            self.selected_graph_id = None;
            self.graph_details = None;
            self.graph_runs.clear();
            self.graph_selected_spec = 0;
            self.graph_selected_node = 0;
            self.graph_live_state = None;
            self.graph_live_follow = true;
            self.graph_live_selected_node = None;
            self.graph_live_follow_anchor = None;
            self.graph_live_focus = GraphLiveFocus::Graph;
            self.graph_spec_strip_selected = None;
            self.graph_spec_strip_scroll = 0;
            self.graph_live_view_scroll = 0;
            return;
        }

        let previous_selected = self.selected_graph_id.clone();
        if self
            .selected_graph_id
            .as_ref()
            .is_none_or(|selected| !visible.iter().any(|lp| lp.id == *selected))
        {
            self.selected_graph_id = Some(visible[0].id.clone());
        }

        let selected_changed = previous_selected != self.selected_graph_id;
        let Some(selected_id) = self.selected_graph_id.clone() else {
            return;
        };
        self.graph_details = self.db.get_graph_details(&selected_id).ok().flatten();
        if selected_changed {
            self.graph_selected_spec = self.default_graph_spec_index();
            self.graph_selected_node = 0;
            self.graph_live_follow = true;
            self.graph_live_selected_node = None;
            self.graph_live_follow_anchor = None;
            self.graph_live_focus = GraphLiveFocus::Graph;
            self.graph_spec_strip_selected = None;
            self.graph_spec_strip_scroll = 0;
            self.graph_live_view_scroll = 0;
        } else {
            self.clamp_graph_selection();
        }
        self.refresh_graph_runs_for_selected_spec();
        self.select_default_graph_node_if_needed(selected_changed);
        self.refresh_graph_live_state();
        // CT14: a graphs-list shrink between ticks can leave `automation_kind`
        // pointing at the now-empty side; correct it here (no redraw).
        self.normalize_automation_kind();
    }

    /// Assemble a fresh [`GraphLiveState`] snapshot for the currently selected
    /// graph. Zero cost when no graph is selected (no queries).
    pub(crate) fn refresh_graph_live_state(&mut self) {
        self.graph_live_state = self
            .graph_details
            .as_ref()
            .and_then(|details| graph_live_state::assemble_graph_live_state(&self.db, details));

        // A manually-highlighted node that no longer exists in the
        // (possibly just-advanced) effective graph falls back to
        // auto-follow rather than pointing at a stale/missing node.
        if !self.graph_live_follow {
            let still_present = self.graph_live_state.as_ref().is_some_and(|state| {
                self.graph_live_selected_node
                    .as_deref()
                    .is_some_and(|id| state.effective_nodes.iter().any(|n| n.id == id))
            });
            if !still_present {
                self.graph_live_follow = true;
                self.graph_live_selected_node = None;
                self.graph_live_follow_anchor = None;
            }
        }

        // Same rule for the spec marker strip's manual selection: a spec
        // that's dropped out of the (possibly just-advanced) queue can't
        // stay highlighted.
        if let Some(selected) = self.graph_spec_strip_selected.as_deref() {
            let still_present = self
                .graph_live_state
                .as_ref()
                .is_some_and(|state| state.spec_queue.iter().any(|e| e.spec_id == selected));
            if !still_present {
                self.graph_spec_strip_selected = None;
            }
        }
    }

    pub(crate) fn refresh_rag_state(&mut self) -> Result<()> {
        self.global_rag_queue = self.db.list_rag_queue(50)?;
        self.rag_paused = self
            .db
            .get_state("rag_paused")?
            .map(|v| v == "1")
            .unwrap_or(false);
        self.rag_model_loaded = crate::rag::status::is_model_loaded(&self.db);
        self.rag_acquisition_state =
            crate::rag::status::read_acquisition_state(&self.db, &self.rag_embeddings_model);

        let (queued, processing) = self
            .db
            .rag_queue_counts()
            .unwrap_or((self.rag_info.queued_items, self.rag_info.processing_items));
        let total_chunks = self
            .db
            .get_state("rag_total_chunks")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(self.rag_info.total_chunks);
        let indexed_files = self
            .db
            .get_state("rag_indexed_files")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(self.rag_info.indexed_files);
        self.rag_info = crate::db::project::RagInfoSummary {
            total_chunks,
            indexed_files,
            queued_items: queued,
            processing_items: processing,
        };

        if self.global_rag_queue.is_empty() {
            self.selected_rag_queue = 0;
        } else {
            self.selected_rag_queue = self
                .selected_rag_queue
                .min(self.global_rag_queue.len().saturating_sub(1));
        }

        if !self.rag_info.has_rag_activity() {
            self.agents_rag_focused = false;
        }

        self.rag_file_status = self.db.rag_per_file_status().unwrap_or_default();

        Ok(())
    }

    pub(crate) fn auto_hide_sidebar(&mut self) {
        let Ok((tw, _th)) = ratatui::crossterm::terminal::size() else {
            return;
        };

        self.term_width = tw;
        let should_hide =
            self.focus == Focus::Agent && self.selected_live_agent().is_some() && tw < 80;
        let should_show = tw >= 80 && !self.sidebar_visible;
        if should_hide {
            self.sidebar_visible = false;
        } else if should_show {
            self.sidebar_visible = true;
        }
    }

    pub(crate) fn clamp_graph_selection(&mut self) {
        let Some(details) = self.graph_details.as_ref() else {
            self.graph_selected_spec = 0;
            self.graph_selected_node = 0;
            return;
        };
        if details.specs.is_empty() {
            self.graph_selected_spec = 0;
            self.graph_selected_node = 0;
            return;
        }

        self.graph_selected_spec = self.graph_selected_spec.min(details.specs.len() - 1);
        let node_count = details.specs[self.graph_selected_spec].nodes.len();
        self.graph_selected_node = if node_count == 0 {
            0
        } else {
            self.graph_selected_node.min(node_count - 1)
        };
    }

    pub(crate) fn default_graph_spec_index(&self) -> usize {
        self.graph_details
            .as_ref()
            .and_then(|details| {
                details
                    .specs
                    .iter()
                    .position(|spec| spec.spec.status == GraphSpecStatus::Running)
                    .or_else(|| {
                        details.specs.iter().position(|spec| {
                            matches!(
                                spec.spec.status,
                                GraphSpecStatus::Pending | GraphSpecStatus::Interrupted
                            )
                        })
                    })
            })
            .unwrap_or(0)
    }

    pub(crate) fn refresh_graph_runs_for_selected_spec(&mut self) {
        self.graph_runs.clear();
        let Some(spec) = self.selected_graph_spec() else {
            return;
        };
        self.graph_runs = self
            .db
            .list_graph_runs_for_spec(&spec.spec.id)
            .unwrap_or_default();
    }

    pub(crate) fn select_default_graph_node_if_needed(&mut self, reset: bool) {
        let Some(spec) = self.selected_graph_spec() else {
            self.graph_selected_node = 0;
            return;
        };
        if spec.nodes.is_empty() {
            self.graph_selected_node = 0;
            return;
        }

        if !reset && self.graph_selected_node < spec.nodes.len() {
            return;
        }

        let current_node_id = self
            .graph_runs
            .iter()
            .rev()
            .find(|run| run.status == crate::domain::graphs::GraphRunStatus::Running)
            .or_else(|| self.graph_runs.last())
            .map(|run| run.node_id.as_str());

        self.graph_selected_node = current_node_id
            .and_then(|node_id| spec.nodes.iter().position(|node| node.id == node_id))
            .unwrap_or(0);
    }

    pub(crate) fn update_whimsg_context(&mut self) {
        use crate::tui::whimsg::WhimContext;

        if !self.daemon_running {
            self.whimsg.set_ambient(WhimContext::Idle);
            return;
        }

        self.check_recent_run_events();
        self.notify_whimsg_for_graphs();

        if self.last_scroll_at.elapsed() < std::time::Duration::from_secs(5) {
            self.whimsg.set_ambient(WhimContext::Scrolling);
            return;
        }

        self.check_log_context();
        self.update_ambient_context();
    }

    pub(crate) fn check_recent_run_events(&mut self) {
        use crate::tui::whimsg::events::{EventKind, WhimEvent};
        let now = Utc::now();
        let candidates: Vec<(String, String, EventKind)> = self
            .recent_runs
            .iter()
            .filter_map(|run| {
                let finished = run.finished_at?;
                if (now - finished).num_seconds() >= 60 {
                    return None;
                }
                let kind = match run.status {
                    crate::domain::models::RunStatus::Error
                    | crate::domain::models::RunStatus::Timeout => EventKind::AgentFailed,
                    crate::domain::models::RunStatus::Success => EventKind::AgentDone,
                    _ => return None,
                };
                Some((run.id.clone(), run.background_agent_id.clone(), kind))
            })
            .collect();
        for (run_id, subject, kind) in candidates {
            if self.whimsg_announced_runs.insert(run_id) {
                self.whimsg.notify_event(WhimEvent::new(kind, subject));
            }
        }
        // Announced ids that fell out of the recent list can never be
        // considered again; drop them so the set doesn't grow for the whole
        // session (it otherwise holds one id per run ever announced).
        let live: HashSet<&str> = self.recent_runs.iter().map(|run| run.id.as_str()).collect();
        self.whimsg_announced_runs
            .retain(|id| live.contains(id.as_str()));
    }

    pub(crate) fn check_log_context(&mut self) {
        let raw_log = self.selected_log_excerpt();
        if raw_log.is_empty() {
            return;
        }

        self.notify_whimsg_for_log(&raw_log);
    }

    pub(crate) fn selected_log_excerpt(&self) -> String {
        self.selected_live_agent()
            .map(|agent| agent.visible_text())
            .unwrap_or_else(|| self.log_content.clone())
    }

    pub(crate) fn notify_whimsg_for_log(&mut self, raw_log: &str) {
        use crate::tui::whimsg::events::{EventKind, WhimEvent};

        // A log event names a real agent; with none selected there is nothing
        // to attribute it to, so it is skipped.
        let Some(subject) = self.selected_live_agent().map(|agent| agent.name.clone()) else {
            return;
        };

        let log_hash = calculate_log_hash(raw_log);
        if log_hash == self.whimsg_last_log_hash {
            return;
        }
        self.whimsg_last_log_hash = log_hash;

        let log_up = raw_log.to_uppercase();
        let kind = if log_contains_error(&log_up) {
            EventKind::AgentFailed
        } else if log_contains_success(&log_up) {
            EventKind::AgentDone
        } else if log_contains_spawn(&log_up) {
            EventKind::AgentSpawned
        } else {
            return;
        };
        self.whimsg.notify_event(WhimEvent::new(kind, subject));
    }

    pub(crate) fn update_ambient_context(&mut self) {
        use crate::tui::whimsg::WhimContext;
        let running = self
            .interactive_agents
            .iter()
            .filter(|a| a.status == crate::tui::agent::AgentStatus::Running)
            .count();
        let active_runs = self.active_runs.len();

        let ctx = if running + active_runs > 0 {
            WhimContext::Busy(running + active_runs)
        } else {
            WhimContext::Idle
        };
        self.whimsg.set_ambient(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::StateRepository;
    use crate::db::Database;
    use crate::tui::ui::theme::Theme;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    /// A `settings_changed` revision for `theme` must swap the in-memory
    /// theme on the next poll, without a restart (CT31 hot apply).
    #[test]
    fn settings_changed_swaps_the_theme_next_frame() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempdir().unwrap();
        std::fs::write(data_dir.path().join("config.toml"), "theme = \"modern\"\n").unwrap();

        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        assert_eq!(app.theme, Theme::classic(), "starts on the passed config");

        app.db
            .set_state(
                crate::daemon::config_watch::SETTINGS_REVISION_KEY,
                "1|theme",
            )
            .unwrap();
        app.poll_settings_revision();

        assert_eq!(app.theme, Theme::modern(), "theme hot-applied");
    }

    /// CT26: `ctrl_c_exits_clis` is derived from the persisted `[[clis]]` table,
    /// so a platform that sets the flag is guarded and every other platform is
    /// not — no CLI name is ever inspected.
    #[test]
    fn ctrl_c_exits_clis_reads_the_persisted_clis_table() {
        use crate::domain::cli_config::CliConfig;

        let mut config = crate::domain::canopy_config::CanopyConfig::default();
        config.clis.push(CliConfig {
            name: "opencode".to_string(),
            ctrl_c_exits: true,
            ..Default::default()
        });
        config.clis.push(CliConfig {
            name: "claude".to_string(),
            ..Default::default()
        });

        let slugs = crate::tui::app::types::ctrl_c_exits_clis(&config);
        assert!(slugs.contains("opencode"));
        assert!(!slugs.contains("claude"));
    }
}
