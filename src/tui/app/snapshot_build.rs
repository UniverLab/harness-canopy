//! Snapshot builders: one function per `refresh_slow` output, all running on
//! the worker thread against the worker's own [`Database`] connection. No
//! function here may touch the UI thread's database handle.

use std::collections::{HashMap, HashSet};

use crate::application::ports::{AgentRepository, RunRepository, StateRepository};
use crate::db::project::RagInfoSummary;
use crate::db::Database;
use crate::domain::graphs::{Graph, GraphStatus};
use crate::tui::app::snapshot::{
    FinishedRunInfo, GraphWhimInfo, MissionDbCounters, SlowSnapshot, SnapshotRequest, WorkerState,
};
use crate::tui::app::system_monitor::calculate_log_hash;
use crate::tui::gamification::MissionEvent;

/// Build one full snapshot for `request`.
pub(crate) fn build_snapshot(
    db: &Database,
    state: &mut WorkerState,
    request: &SnapshotRequest,
) -> SlowSnapshot {
    let mut snapshot = SlowSnapshot::default();
    build_daemon(db, &mut snapshot);
    build_agents(db, &mut snapshot);
    build_runs(db, request, &mut snapshot);
    // Graphs before projects: the preview cache's `graph_running` badge reads
    // the running workdirs off the graphs just built.
    build_graphs(db, request, &mut snapshot);
    build_projects(db, request, &mut snapshot);
    build_project_graph(db, &mut snapshot);
    build_rag(db, request, &mut snapshot);
    build_panel_inputs(db, request, &mut snapshot);
    build_mission(db, state, request, &mut snapshot);
    build_digests(db, request, &mut snapshot);
    snapshot
}

fn build_daemon(db: &Database, snapshot: &mut SlowSnapshot) {
    snapshot.daemon_version = db.get_state("version").ok().flatten().unwrap_or_default();
    snapshot.daemon_port = db
        .get_state("port")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(7755);
    snapshot.settings_revision = db
        .get_state(crate::daemon::config_watch::SETTINGS_REVISION_KEY)
        .ok()
        .flatten()
        .unwrap_or_default();
}

fn build_agents(db: &Database, snapshot: &mut SlowSnapshot) {
    snapshot.agents = db.list_agents().unwrap_or_default();
    snapshot.corrupt_agents = db.list_corrupt_agents().unwrap_or_default();
}

fn build_runs(db: &Database, request: &SnapshotRequest, snapshot: &mut SlowSnapshot) {
    snapshot.active_runs = db.list_active_runs().unwrap_or_default();
    snapshot.recent_runs = db.list_all_recent_runs(50).unwrap_or_default();
    // Daemonless-TUI completion toast inputs: previous actives that vanished,
    // resolved here (the UI must not query them itself).
    for id in &request.prev_active_ids {
        if snapshot.active_runs.contains_key(id) {
            continue;
        }
        if db.get_agent(id).ok().flatten().is_none() {
            continue;
        }
        if let Some(run) = db
            .list_runs(id, 1)
            .ok()
            .and_then(|mut runs| runs.drain(..).next())
        {
            snapshot.finished_runs.push(FinishedRunInfo {
                agent_id: id.clone(),
                success: matches!(run.status, crate::domain::models::RunStatus::Success),
                exit_code: run.exit_code,
            });
        }
    }
}

fn build_projects(db: &Database, request: &SnapshotRequest, snapshot: &mut SlowSnapshot) {
    let ranked = db.list_projects_by_activity().unwrap_or_default();
    snapshot.project_last_activity = ranked
        .iter()
        .map(|entry| (entry.project.hash.clone(), entry.last_activity))
        .collect();
    snapshot.projects = ranked.into_iter().map(|entry| entry.project).collect();

    snapshot.backlog_specs = db
        .list_specs(request.backlog_workdir.as_deref(), None, true, None)
        .unwrap_or_default();

    if let Some(hash) = request.selected_project_hash.as_deref() {
        snapshot.project_knowledge = db
            .list_project_knowledge(hash, None, 50)
            .unwrap_or_default();
    }

    let running_workdirs: HashSet<String> = snapshot
        .graphs
        .iter()
        .filter(|lp| lp.status == GraphStatus::Running)
        .map(|lp| lp.workdir.clone())
        .collect();
    let mut cache = HashMap::new();
    for project in &snapshot.projects {
        let pending_backlog = db
            .count_specs(Some(project.path.as_str()), false)
            .unwrap_or(0);
        let knowledge_entries = db.count_project_knowledge(&project.hash).unwrap_or(0);
        let last_activity = db
            .list_graphs(Some(project.path.as_str()), true)
            .ok()
            .and_then(|graphs| graphs.iter().map(|lp| lp.created_at.timestamp()).max());
        cache.insert(
            project.hash.clone(),
            crate::tui::app::types::ProjectPreviewSummary {
                pending_backlog,
                knowledge_entries,
                last_activity,
                graph_running: running_workdirs.contains(&project.path),
            },
        );
    }
    snapshot.project_preview_cache = cache;

    if let Some(hash) = request.history_hash.as_deref() {
        let workdir = snapshot
            .projects
            .iter()
            .find(|p| p.hash == hash)
            .map(|p| p.path.clone());
        if let Some(workdir) = workdir {
            snapshot.project_history = db.list_project_history(&workdir, 100).unwrap_or_default();
            snapshot.project_history_hash = Some(hash.to_string());
        }
    }
}

fn build_graphs(db: &Database, request: &SnapshotRequest, snapshot: &mut SlowSnapshot) {
    snapshot.graphs = db.list_graphs(None, false).unwrap_or_default();
    snapshot.archived_graph_count = db.count_archived_graphs().unwrap_or(0) as usize;
    if request.graph_view_archived {
        snapshot.archived_graphs = db.list_graphs(None, true).unwrap_or_default();
        snapshot.archived_graphs.retain(|lp| lp.archived);
    }

    // Sidebar meta: same labels `refresh_graph_sidebar_meta` computes, but
    // here on the worker — including the paused-blocked flag from the newest
    // run summary only (no full history load).
    let last_run_times = db.list_graph_last_run_times().unwrap_or_default();
    for lp in &snapshot.graphs {
        let last_run_at = last_run_times.get(&lp.id).copied();
        let last_run_label = if lp.status == GraphStatus::Pausing {
            crate::tui::app::graph_live_state::pausing_label(
                &db.pause_waiting_node_names(&lp.id).unwrap_or_default(),
            )
        } else if lp.status == GraphStatus::Running {
            "running".to_string()
        } else {
            match last_run_at {
                Some(at) => crate::tui::app::utils::relative_time_compact(&at),
                None => "never".to_string(),
            }
        };
        let autorun_label = lp.autorun_at.map(|at| {
            format!(
                "resumes {}",
                crate::tui::app::utils::relative_time_until_compact(&at)
            )
        });
        snapshot.graph_sidebar_meta.insert(
            lp.id.clone(),
            crate::tui::app::types::GraphSidebarMeta {
                last_activity: last_run_at.unwrap_or(lp.created_at),
                last_run_label,
                blocked: graph_blocked(db, lp),
                autorun_label,
            },
        );
    }

    // Header inputs for every graph under the same conditions the UI used to
    // query them: completed labels for running/pausing graphs, the last node
    // for blocked/failed ones.
    for lp in &snapshot.graphs {
        if let Some(info) = graph_whim_info(db, lp) {
            snapshot.graph_whim.insert(lp.id.clone(), info);
        }
    }

    // Selected graph: details, its spec's runs, and the live state.
    if let Some(graph_id) = request.selected_graph_id.as_deref() {
        snapshot.graph_details = db.get_graph_details(graph_id).ok().flatten();
        if let Some(spec_id) = request.selected_spec_id.as_deref() {
            snapshot.graph_runs = db.list_graph_runs_for_spec(spec_id).unwrap_or_default();
        }
        if let Some(details) = snapshot.graph_details.as_ref() {
            snapshot.graph_live_state =
                crate::tui::app::graph_live_state::assemble_graph_live_state(db, details);
        }
    }
}

/// Sidebar blocked flag for one graph: the `Paused`-blocker check plus the
/// admin `graph_blocker` row.
fn graph_blocked(db: &Database, lp: &Graph) -> bool {
    if lp.status == GraphStatus::Paused
        && db
            .last_graph_run_summary(&lp.id)
            .ok()
            .flatten()
            .is_some_and(|summary| summary.has_blocker)
    {
        return true;
    }
    db.graph_blocker(&lp.id).ok().flatten().is_some()
}

/// Per-graph header inputs under the same conditions the UI used to query
/// them: running/pausing graphs get completed labels, blocked/failed graphs
/// get the last node.
fn graph_whim_info(db: &Database, lp: &Graph) -> Option<GraphWhimInfo> {
    let mut info = GraphWhimInfo::default();
    if matches!(lp.status, GraphStatus::Running | GraphStatus::Pausing) {
        info.done_specs = crate::tui::app::graph_live_state::completed_spec_labels(db, lp);
    }
    if matches!(lp.status, GraphStatus::Paused | GraphStatus::Failed) {
        info.node = db
            .last_graph_run_summary(&lp.id)
            .ok()
            .flatten()
            .map(|summary| summary.node_id);
    }
    if info.node.is_some() || !info.done_specs.is_empty() {
        Some(info)
    } else {
        None
    }
}

fn build_project_graph(db: &Database, snapshot: &mut SlowSnapshot) {
    let mut edges = Vec::new();
    for project in &snapshot.projects {
        for (node, edge) in db
            .list_related_projects(&project.hash, 20)
            .unwrap_or_default()
        {
            edges.push(crate::tui::app::types::ProjectGraphEdge {
                from_name: project.name.clone(),
                to_name: node.title.clone(),
                from_hash: project.hash.clone(),
                to_hash: node.project_hash.clone().unwrap_or_else(|| node.id.clone()),
                relation: edge.relation,
            });
        }
    }
    let mut trees: Vec<Vec<String>> = Vec::new();
    for edge in &edges {
        let mut root: Option<String> = None;
        for tree in &trees {
            if tree.contains(&edge.from_name) || tree.contains(&edge.to_name) {
                root = Some(tree[0].clone());
                break;
            }
        }
        if let Some(root_name) = root {
            let tree = trees.iter_mut().find(|t| t.contains(&root_name)).unwrap();
            if !tree.contains(&edge.from_name) {
                tree.push(edge.from_name.clone());
            }
            if !tree.contains(&edge.to_name) {
                tree.push(edge.to_name.clone());
            }
        } else {
            trees.push(vec![edge.from_name.clone(), edge.to_name.clone()]);
        }
    }
    for project in &snapshot.projects {
        if !trees.iter().any(|tree| tree.contains(&project.name)) {
            trees.push(vec![project.name.clone()]);
        }
    }
    for tree in &mut trees {
        let mut seen = HashSet::new();
        tree.retain(|name| seen.insert(name.clone()));
    }
    snapshot.project_graph_edges = edges;
    snapshot.project_graph_trees = trees;
}

fn build_rag(db: &Database, request: &SnapshotRequest, snapshot: &mut SlowSnapshot) {
    snapshot.rag_queue = db.list_rag_queue(50).unwrap_or_default();
    snapshot.rag_paused = db
        .get_state("rag_paused")
        .ok()
        .flatten()
        .map(|v| v == "1")
        .unwrap_or(false);
    snapshot.rag_model_loaded = crate::rag::status::is_model_loaded(db);
    snapshot.rag_acquisition_state =
        crate::rag::status::read_acquisition_state(db, &request.rag_embeddings_model);
    let (queued, processing) = db.rag_queue_counts().unwrap_or((0, 0));
    let total_chunks = db
        .get_state("rag_total_chunks")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let indexed_files = db
        .get_state("rag_indexed_files")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    snapshot.rag_info = RagInfoSummary {
        total_chunks,
        indexed_files,
        queued_items: queued,
        processing_items: processing,
    };
    snapshot.rag_file_status = db.rag_per_file_status().unwrap_or_default();
}

fn build_panel_inputs(db: &Database, request: &SnapshotRequest, snapshot: &mut SlowSnapshot) {
    if let Some(hash) = request.selected_project_hash.as_deref() {
        snapshot.panel_knowledge_updated = db.max_project_knowledge_updated_at(hash).ok().flatten();
    }
    snapshot.panel_backlog_updated = db
        .max_backlog_updated_at(request.backlog_workdir.as_deref())
        .ok()
        .flatten();
}

fn build_mission(
    db: &Database,
    state: &mut WorkerState,
    request: &SnapshotRequest,
    snapshot: &mut SlowSnapshot,
) {
    // One-shot daemon flags: consumed exactly once here, surfaced as events.
    for (key, event) in [
        (
            "gamification:identity_evolved",
            MissionEvent::IdentityEvolved,
        ),
        ("gamification:deep_rag_search", MissionEvent::DeepRagSearch),
        (
            "gamification:digital_archeologist",
            MissionEvent::DigitalArcheologistFind,
        ),
    ] {
        if db.get_state(key).ok().flatten().as_deref() == Some("1") {
            match db.set_state(key, "done") {
                Ok(()) => snapshot.mission_events.push(event),
                Err(error) => tracing::warn!(
                    "Failed to consume daemon flag '{key}', will retry next tick: {error}"
                ),
            }
        }
    }

    let has_parallel = if request.has_parallel_seen || state.has_parallel_seen {
        true
    } else {
        db.has_parallel_graph_run().unwrap_or(false)
    };
    state.note_cpu_frequency(db, request.cpu_frequency_mhz);

    snapshot.mission = MissionDbCounters {
        gardener_edits: db.count_gardener_projects().unwrap_or(0),
        graph_node_count_max: db.max_graph_nodes_in_any_graph().unwrap_or(0),
        completed_graph_runs: db.count_completed_graphs().unwrap_or(0),
        total_graph_node_runs: db.count_graph_node_runs().unwrap_or(0),
        has_parallel_graph: has_parallel,
        max_seed_session_bindings: db.max_seed_session_bindings().unwrap_or(0),
        intelligence_nodes: db.count_intelligence_nodes().unwrap_or(0),
        cross_project_links: db.count_cross_project_intelligence_links().unwrap_or(0),
        max_cpu_frequency_seen: state.max_cpu_frequency_seen,
        canopy_uptime_secs: state.tick_uptime_secs(db),
    };

    // Persist unlocks the UI awarded last tick; confirm what landed so the UI
    // marks them exactly once.
    for id in &request.persist_unlocks {
        match persist_unlock(db, *id) {
            Ok(Some(timestamp)) => snapshot.confirmed_unlocks.push((*id, timestamp)),
            Ok(None) => {}
            Err(error) => tracing::warn!("Failed to persist mission unlock: {error:#}"),
        }
    }
}

/// Record one mission unlock through the worker's connection: the same
/// `achievement:*` key the UI-side store reads, so both sides agree. An
/// already-present key still confirms (with its stored timestamp) so the UI
/// never re-queues the same unlock every tick.
fn persist_unlock(
    db: &Database,
    id: crate::domain::gamification::MissionId,
) -> anyhow::Result<Option<i64>> {
    let key = format!("achievement:{}", id.as_str());
    if let Some(existing) = db.get_state(&key)? {
        let timestamp = existing
            .parse::<i64>()
            .unwrap_or_else(|_| chrono::Utc::now().timestamp());
        return Ok(Some(timestamp));
    }
    let now = chrono::Utc::now().timestamp();
    db.set_state(&key, &now.to_string())?;
    Ok(Some(now))
}

fn build_digests(db: &Database, request: &SnapshotRequest, snapshot: &mut SlowSnapshot) {
    // Activity digest: the drawn workdirs' log entries plus active agent ids.
    let mut activity = String::new();
    if !request.activity_workdirs.is_empty() {
        for workdir in &request.activity_workdirs {
            let entries = db
                .list_activity_log_entries(workdir, request.activity_limit)
                .unwrap_or_default();
            let mut agent_ids = db.list_active_sync_agent_ids(workdir).unwrap_or_default();
            agent_ids.sort();
            use std::fmt::Write as _;
            let _ = writeln!(activity, "{workdir}|{entries:?}|{agent_ids:?}");
        }
    }
    snapshot.activity_digest = calculate_log_hash(&activity);

    // Overview digest: the selected project's recent-session summaries.
    let mut overview = String::new();
    if let (Some(hash), Some(path)) = (
        request.overview_project_hash.as_deref(),
        request.overview_project_path.as_deref(),
    ) {
        let nodes = db.search_operational_sessions(path, 12).unwrap_or_default();
        use std::fmt::Write as _;
        for node in nodes
            .iter()
            .filter(|node| {
                node.project_hash.as_deref() == Some(hash)
                    || node.metadata.as_deref().is_some_and(|metadata| {
                        serde_json::from_str::<serde_json::Value>(metadata)
                            .ok()
                            .and_then(|value| {
                                value
                                    .get("workdir")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string)
                            })
                            == Some(path.to_string())
                    })
            })
            .take(3)
        {
            let _ = writeln!(overview, "{}|{}|{}", node.title, node.body, node.id);
        }
    }
    snapshot.overview_sessions_digest = calculate_log_hash(&overview);

    // Hub counts: the four COUNT(*) queries, once per snapshot, not per tick.
    snapshot.hub_counts_digest = (
        db.count_interactive_sessions().unwrap_or(0),
        db.count_terminal_sessions().unwrap_or(0),
        db.count_background_agents().unwrap_or(0),
        db.count_runs().unwrap_or(0),
    );

    // Pending sends digest for the selected session.
    let mut sends = String::new();
    if let Some(session) = request.pending_sends_session.as_deref() {
        let pending = db
            .list_pending_scheduled_prompts_for_session(session)
            .unwrap_or_default();
        sends = format!("{pending:?}");
    }
    snapshot.pending_sends_digest = calculate_log_hash(&sends);
}
