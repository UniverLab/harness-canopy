//! Slow-refresh fingerprint: visible slow outputs, captured before and after
//! [`App::refresh_slow`](crate::tui::app::types::App::refresh_slow), so the
//! loop redraws for the slow path only when something in it actually changed.

use std::collections::HashMap;

use super::dialog::GraphActionMessage;
use super::graph_live_state::GraphLiveState;
use super::types::{AgentEntry, App, PanelFace};
use super::*;
use crate::domain::graphs::{GraphDetails, GraphRunStatus};
use crate::rag::status::AcquisitionState;

/// Visible slow-refresh outputs, captured before and after
/// [`App::refresh_slow`]: the tick redraws for the slow path only when the
/// fingerprint moved, so settled data never draws on its own (FR5).
///
/// Content digests (`u64`) replace length checks everywhere a same-length
/// edit is visible (an agent's enable flip, a project's re-rank, a graph's
/// blocker): a length-only fingerprint would leave the screen stale. Every
/// digest hashes stored state only — never a wall-clock-derived label like
/// `last_run_label` — so time passing alone never dirties the tick.
#[derive(PartialEq, Eq, Debug)]
pub(crate) struct SlowFingerprint {
    agents: u64,
    orphaned_sessions: u64,
    split_groups: u64,
    graphs: u64,
    graphs_meta: u64,
    archived_graphs: u64,
    daemon_running: bool,
    daemon_version: String,
    update_available: Option<String>,
    hub_notice: Option<String>,
    active_runs: u64,
    backlog: u64,
    projects: u64,
    project_activity: u64,
    project_knowledge: u64,
    project_graph_edges: u64,
    project_graph_trees: u64,
    graph_details: Option<u64>,
    graph_specs: Vec<(String, String, GraphSpecStatus)>,
    graph_runs: Vec<(String, GraphRunStatus, i64, i64, Option<i64>)>,
    graph_live: Option<u64>,
    log_hash: u64,
    rag_queue: u64,
    rag_paused: bool,
    rag_model_loaded: bool,
    rag_acquisition: Option<AcquisitionState>,
    rag_info: (i64, i64, i64, i64),
    rag_files: u64,
    missions_unlocked: usize,
    playground_results: u64,
    playground_pending: bool,
    playground_executed_query: String,
    project_preview: u64,
    project_history: u64,
    activity_digest: u64,
    overview_sessions_digest: u64,
    hub_counts: (i64, i64, i64, i64),
    pending_sends: u64,
    cli_usage: u64,
    settings_seen: String,
    panel_face: PanelFace,
    panel_dwell_face: Option<PanelFace>,
    panel_last_reason: Option<String>,
    crews_snapshot: bool,
    crews_rows: u64,
    crews_message: Option<(bool, String)>,
    settings_rows: u64,
    settings_message: Option<(bool, String)>,
    remote_signed_in: bool,
    remote_login: Option<String>,
    remote_enabled: bool,
    remote_relay: String,
    remote_live_devices: Vec<String>,
    remote_devices: u64,
    remote_pair: bool,
    remote_message: Option<(bool, String)>,
}

/// `(is_error, text)` of a transient daemon-operation message, if one shows.
fn message_summary(message: &Option<GraphActionMessage>) -> Option<(bool, String)> {
    message
        .as_ref()
        .map(|message| (message.is_error, message.text.clone()))
}

/// Hash of the Crews face's pair rows: availability flips keep the row count
/// but must still dirty the tick.
fn crews_rows_digest(app: &App) -> u64 {
    let mut summary = String::new();
    for row in &app.crews.pair_rows {
        use std::fmt::Write as _;
        let _ = writeln!(
            summary,
            "{}|{}|{}|{}|{}|{}",
            row.platform, row.model, row.available, row.expired, row.graph_count, row.crew_count,
        );
    }
    calculate_log_hash(&summary)
}

/// Hash of the Settings tab's rows: a refetch that changes values but not
/// the row count must still dirty the tick.
fn settings_rows_digest(app: &App) -> u64 {
    let mut summary = String::new();
    for row in &app.hub.settings.rows {
        use std::fmt::Write as _;
        let _ = writeln!(
            summary,
            "{}|{}|{}|{}",
            row.key, row.effective, row.source, row.status,
        );
    }
    calculate_log_hash(&summary)
}

/// Map entries sorted by key: `HashMap` iteration order is not a pure
/// function of content, so every map digest sorts first — otherwise two
/// captures of identical data could differ and dirty an idle tick.
fn sorted_entries<V>(map: &HashMap<String, V>) -> Vec<(&str, &V)> {
    let mut entries: Vec<(&str, &V)> = map
        .iter()
        .map(|(key, value)| (key.as_str(), value))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
}

/// Sidebar entries: background-agent rows hash whole (`enabled`, trigger,
/// last-run outcome and prompt all render), index rows hash their kind and
/// slot (membership moves the slots; live statuses are fast-path state).
fn agents_digest(app: &App) -> u64 {
    use std::fmt::Write as _;
    let mut summary = String::new();
    for entry in &app.agents {
        match entry {
            AgentEntry::Agent(agent) => {
                let _ = writeln!(summary, "agent|{agent:?}");
            }
            AgentEntry::Corrupt(corrupt) => {
                let _ = writeln!(summary, "corrupt|{corrupt:?}");
            }
            AgentEntry::Interactive(idx) => {
                let _ = writeln!(summary, "interactive|{idx}");
            }
            AgentEntry::Terminal(idx) => {
                let _ = writeln!(summary, "terminal|{idx}");
            }
            AgentEntry::Orphaned(idx) => {
                let _ = writeln!(summary, "orphaned|{idx}");
            }
            AgentEntry::Group(idx) => {
                let _ = writeln!(summary, "group|{idx}");
            }
        }
    }
    calculate_log_hash(&summary)
}

/// Orphaned sessions render name, CLI, workdir and status.
fn orphaned_sessions_digest(app: &App) -> u64 {
    use std::fmt::Write as _;
    let mut summary = String::new();
    for session in &app.orphaned_sessions {
        let _ = writeln!(
            summary,
            "{}|{}|{}|{}|{}",
            session.id, session.name, session.cli, session.working_dir, session.status,
        );
    }
    calculate_log_hash(&summary)
}

/// Split groups render their id, orientation and both member names.
fn split_groups_digest(app: &App) -> u64 {
    use std::fmt::Write as _;
    let mut summary = String::new();
    for group in &app.split_groups {
        let _ = writeln!(
            summary,
            "{}|{}|{}|{}",
            group.id,
            group.orientation.as_str(),
            group.session_a,
            group.session_b,
        );
    }
    calculate_log_hash(&summary)
}

/// Per-graph sidebar meta: the raw last-activity instant (the sort key)
/// and the blocker flag (‖ vs ⊘). The precomputed relative labels are
/// deliberately excluded: they drift with wall-clock time, and hashing
/// them would dirty an idle tick every minute.
fn graphs_meta_digest(app: &App) -> u64 {
    use std::fmt::Write as _;
    let mut summary = String::new();
    for (id, meta) in sorted_entries(&app.graph_sidebar_meta) {
        let _ = writeln!(
            summary,
            "{}|{}|{}",
            id,
            meta.last_activity.timestamp(),
            meta.blocked,
        );
    }
    calculate_log_hash(&summary)
}

/// Selected graph's structure: identity and trigger, node/edge shape, and
/// hook-run count plus the latest run (history only ever appends, so the
/// count plus the tail covers it). Node configs are excluded: they change
/// through the graph editor (input always dirties) and would bloat every
/// capture with full prompts.
fn graph_details_digest(details: &Option<GraphDetails>) -> Option<u64> {
    use std::fmt::Write as _;
    let details = details.as_ref()?;
    let mut summary = String::new();
    let _ = writeln!(
        summary,
        "lp|{}|{}|{:?}|{}|{:?}",
        details.lp.id, details.lp.name, details.lp.status, details.lp.workdir, details.lp.trigger,
    );
    for node in &details.graph_nodes {
        let _ = writeln!(summary, "node|{}|{}|{:?}", node.id, node.name, node.kind);
    }
    for edge in &details.graph_edges {
        let _ = writeln!(
            summary,
            "edge|{}|{}|{:?}",
            edge.from_node, edge.to_node, edge.condition,
        );
    }
    let _ = writeln!(summary, "hooks|{}", details.completion_hook_runs.len());
    if let Some(last) = details.completion_hook_runs.last() {
        let _ = writeln!(summary, "hook-last|{last:?}");
    }
    Some(calculate_log_hash(&summary))
}

/// Live graph view: spec queue, effective node/edge shape, ensemble member
/// states, taken router routes (sorted — the source map is unordered) and
/// the current node. Timestamps are raw instants (stable); nothing here
/// derives from wall-clock time.
fn graph_live_digest(live: &Option<GraphLiveState>) -> Option<u64> {
    use std::fmt::Write as _;
    let live = live.as_ref()?;
    let mut summary = String::new();
    let _ = writeln!(
        summary,
        "meta|{}|{}|{:?}|{}|{}|{:?}|{:?}|{:?}",
        live.graph_id,
        live.graph_name,
        live.graph_status,
        live.workdir,
        live.trigger_type,
        live.schedule_expr,
        live.watch_path,
        live.autorun_at.map(|at| at.timestamp()),
    );
    for spec in &live.spec_queue {
        let _ = writeln!(
            summary,
            "spec|{}|{}|{:?}|{:?}|{:?}|{:?}|{:?}",
            spec.spec_id,
            spec.spec_name,
            spec.status,
            spec.failure_reason,
            spec.spec_code,
            spec.spec_type,
            spec.spec_scope,
        );
    }
    let _ = writeln!(
        summary,
        "progress|{}|{}|{:?}",
        live.done_count, live.total_count, live.current_spec_id,
    );
    for node in &live.effective_nodes {
        let _ = writeln!(summary, "node|{}|{}|{:?}", node.id, node.name, node.kind);
    }
    for edge in &live.effective_edges {
        let _ = writeln!(
            summary,
            "edge|{}|{}|{:?}",
            edge.from_node, edge.to_node, edge.condition,
        );
    }
    for ensemble in &live.ensembles {
        let _ = writeln!(
            summary,
            "ensemble|{}|{}|{}|{:?}|{:?}",
            ensemble.ensemble_id,
            ensemble.name,
            ensemble.join_node_id,
            ensemble.straggler_timeout_minutes,
            ensemble.quorum_grace_minutes,
        );
        for member in &ensemble.members {
            let _ = writeln!(
                summary,
                "member|{}|{}|{:?}",
                member.node_id, member.label, member.status,
            );
        }
    }
    let mut routes: Vec<(&str, &str)> = live
        .router_taken_routes
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    routes.sort();
    for (node, route) in routes {
        let _ = writeln!(summary, "route|{node}|{route}");
    }
    let _ = writeln!(
        summary,
        "current|{:?}|{:?}|{:?}|{:?}",
        live.current_node_id,
        live.current_node_status,
        live.current_node_started_at.map(|at| at.timestamp()),
        live.current_node_iteration,
    );
    Some(calculate_log_hash(&summary))
}

/// Project preview cards: pending backlog, knowledge count, last activity
/// and the running flag, per project hash.
fn project_preview_digest(app: &App) -> u64 {
    use std::fmt::Write as _;
    let mut summary = String::new();
    for (hash, preview) in sorted_entries(&app.project_preview_cache) {
        let _ = writeln!(
            summary,
            "{}|{}|{}|{:?}|{}",
            hash,
            preview.pending_backlog,
            preview.knowledge_entries,
            preview.last_activity,
            preview.graph_running,
        );
    }
    calculate_log_hash(&summary)
}

/// Project history tabs: every cached entry (kind, name, status, instant),
/// per project hash.
fn project_history_digest(app: &App) -> u64 {
    use std::fmt::Write as _;
    let mut summary = String::new();
    for (hash, entries) in sorted_entries(&app.project_history_cache) {
        for entry in entries {
            let _ = writeln!(
                summary,
                "{}|{:?}|{}|{}|{}",
                hash, entry.kind, entry.name, entry.status, entry.at,
            );
        }
    }
    calculate_log_hash(&summary)
}

/// Last-activity instants per project hash (raw values — the sidebar
/// formats them as relative times at draw time, which must not dirty).
fn project_activity_digest(app: &App) -> u64 {
    use std::fmt::Write as _;
    let mut summary = String::new();
    let mut entries: Vec<(&str, i64)> = app
        .project_last_activity
        .iter()
        .map(|(hash, at)| (hash.as_str(), *at))
        .collect();
    entries.sort();
    for (hash, at) in entries {
        let _ = writeln!(summary, "{hash}|{at}");
    }
    calculate_log_hash(&summary)
}

/// CLI launch counters (the hub legend's usage panel), sorted by name.
fn cli_usage_digest(app: &App) -> u64 {
    use std::fmt::Write as _;
    let mut summary = String::new();
    let mut counts: Vec<(&str, u64)> = app
        .cli_usage
        .counts
        .iter()
        .map(|(name, count)| (name.as_str(), *count))
        .collect();
    counts.sort();
    for (name, count) in counts {
        let _ = writeln!(summary, "{name}|{count}");
    }
    calculate_log_hash(&summary)
}

/// Active background-agent runs keyed by agent id (status and fixed
/// instants — nothing wall-clock-derived).
fn active_runs_digest(app: &App) -> u64 {
    use std::fmt::Write as _;
    let mut summary = String::new();
    for (id, run) in sorted_entries(&app.active_runs) {
        let _ = writeln!(summary, "{id}|{run:?}");
    }
    calculate_log_hash(&summary)
}

/// Project-relation edges: endpoint names, hashes and relation.
fn project_graph_edges_digest(app: &App) -> u64 {
    use std::fmt::Write as _;
    let mut summary = String::new();
    for edge in &app.project_graph_edges {
        let _ = writeln!(
            summary,
            "{}|{}|{}|{}|{}",
            edge.from_name, edge.to_name, edge.from_hash, edge.to_hash, edge.relation,
        );
    }
    calculate_log_hash(&summary)
}

impl SlowFingerprint {
    pub(crate) fn capture(app: &App) -> Self {
        let mut graph_specs = Vec::new();
        if let Some(details) = &app.graph_details {
            for spec in &details.specs {
                graph_specs.push((
                    spec.spec.id.clone(),
                    spec.spec.name.clone(),
                    spec.spec.status,
                ));
            }
        }
        Self {
            agents: agents_digest(app),
            orphaned_sessions: orphaned_sessions_digest(app),
            split_groups: split_groups_digest(app),
            graphs: calculate_log_hash(&format!("{:?}", app.graphs)),
            graphs_meta: graphs_meta_digest(app),
            archived_graphs: calculate_log_hash(&format!("{:?}", app.archived_graphs)),
            daemon_running: app.daemon_running,
            daemon_version: app.daemon_version.clone(),
            update_available: app.update_available.clone(),
            hub_notice: app.hub_notice.clone(),
            active_runs: active_runs_digest(app),
            backlog: calculate_log_hash(&format!("{:?}", app.backlog_specs)),
            projects: calculate_log_hash(&format!("{:?}", app.projects)),
            project_activity: project_activity_digest(app),
            project_knowledge: calculate_log_hash(&format!("{:?}", app.project_knowledge)),
            project_graph_edges: project_graph_edges_digest(app),
            project_graph_trees: calculate_log_hash(&format!("{:?}", app.project_graph_trees)),
            graph_details: graph_details_digest(&app.graph_details),
            graph_specs,
            graph_runs: app
                .graph_runs
                .iter()
                .map(|run| {
                    (
                        run.node_id.clone(),
                        run.status,
                        run.iteration,
                        run.started_at.timestamp(),
                        run.completed_at.map(|at| at.timestamp()),
                    )
                })
                .collect(),
            graph_live: graph_live_digest(&app.graph_live_state),
            log_hash: calculate_log_hash(&app.log_content),
            rag_queue: calculate_log_hash(&format!("{:?}", app.global_rag_queue)),
            rag_paused: app.rag_paused,
            rag_model_loaded: app.rag_model_loaded,
            rag_acquisition: app.rag_acquisition_state.clone(),
            rag_info: (
                app.rag_info.total_chunks,
                app.rag_info.indexed_files,
                app.rag_info.queued_items,
                app.rag_info.processing_items,
            ),
            rag_files: calculate_log_hash(&format!("{:?}", app.rag_file_status)),
            missions_unlocked: app.mission_manager.unlocked_count(),
            playground_results: calculate_log_hash(&format!("{:?}", app.playground_results)),
            playground_pending: app.playground_search_pending,
            playground_executed_query: app.playground_last_executed_query.clone(),
            project_preview: project_preview_digest(app),
            project_history: project_history_digest(app),
            activity_digest: app.activity_digest,
            overview_sessions_digest: app.overview_sessions_digest,
            hub_counts: app.hub_counts_digest,
            pending_sends: app.pending_sends_digest,
            cli_usage: cli_usage_digest(app),
            settings_seen: app.settings_seen.clone(),
            panel_face: app.panel_face,
            panel_dwell_face: app.panel_dwell_face,
            panel_last_reason: app.panel_last_reason.clone(),
            crews_snapshot: app.crews.snapshot.is_some(),
            crews_rows: crews_rows_digest(app),
            crews_message: message_summary(&app.crews.message),
            settings_rows: settings_rows_digest(app),
            settings_message: message_summary(&app.hub.settings.message),
            remote_signed_in: app.hub.remote.view.signed_in,
            remote_login: app.hub.remote.view.login.clone(),
            remote_enabled: app.hub.remote.view.enabled,
            remote_relay: app.hub.remote.view.relay.clone(),
            remote_live_devices: app.hub.remote.view.live_devices.clone(),
            remote_devices: calculate_log_hash(&format!("{:?}", app.hub.remote.view.devices)),
            remote_pair: app.hub.remote.pair.is_some(),
            remote_message: message_summary(&app.hub.remote.message),
        }
    }
}

#[cfg(test)]
mod cadence_tests {
    use super::*;

    use crate::domain::models::{Agent, AgentAction, Cli};
    use crate::tui::app::types::{AgentEntry, GraphSidebarMeta, PanelFace};

    fn test_app() -> (App, tempfile::TempDir) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = std::sync::Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        (app, data_dir)
    }

    fn bg_agent(id: &str) -> Agent {
        Agent {
            id: id.to_string(),
            prompt: String::new(),
            trigger: None,
            cli: Cli::new("claude"),
            model: None,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: chrono::Utc::now(),
            log_path: format!("/tmp/{id}.log"),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: AgentAction::Headless,
        }
    }

    /// Same-length agent-list edits (enable flip, new last-run outcome) are
    /// visible on the sidebar cards, so the fingerprint must see them.
    #[test]
    fn fingerprint_sees_agent_content_change() {
        let (mut app, _dir) = test_app();
        app.agents.push(AgentEntry::Agent(bg_agent("a1")));
        let before = SlowFingerprint::capture(&app);
        let AgentEntry::Agent(agent) = &mut app.agents[0] else {
            panic!("fixture must be a background agent");
        };
        agent.enabled = false;
        agent.last_run_ok = Some(false);
        assert_ne!(
            before,
            SlowFingerprint::capture(&app),
            "an enable flip must move the fingerprint"
        );
    }

    /// A blocker landing on a paused graph flips its sidebar icon (‖ → ⊘)
    /// without changing any status, so the graph meta must be fingerprinted.
    #[test]
    fn fingerprint_sees_graph_blocker_flip() {
        let (mut app, _dir) = test_app();
        app.graph_sidebar_meta.insert(
            "g1".to_string(),
            GraphSidebarMeta {
                last_activity: chrono::Utc::now(),
                last_run_label: "never".to_string(),
                blocked: false,
                autorun_label: None,
            },
        );
        let before = SlowFingerprint::capture(&app);
        app.graph_sidebar_meta
            .get_mut("g1")
            .expect("fixture meta must exist")
            .blocked = true;
        assert_ne!(
            before,
            SlowFingerprint::capture(&app),
            "a blocker flip must move the fingerprint"
        );
    }

    /// A daemon-published settings revision re-derives theme, units and the
    /// animation toggles; the fingerprint must see the revision move so the
    /// new settings render without another interaction.
    #[test]
    fn fingerprint_sees_settings_revision() {
        let (mut app, _dir) = test_app();
        let before = SlowFingerprint::capture(&app);
        app.settings_seen = "rev-2-changed-keys".to_string();
        assert_ne!(
            before,
            SlowFingerprint::capture(&app),
            "a settings revision must move the fingerprint"
        );
    }

    /// The right panel's face is slow-refresh output; a face switch must
    /// move the fingerprint so the tick draws it.
    #[test]
    fn fingerprint_sees_panel_face_change() {
        let (mut app, _dir) = test_app();
        let before = SlowFingerprint::capture(&app);
        app.panel_face = PanelFace::Knowledge;
        assert_ne!(
            before,
            SlowFingerprint::capture(&app),
            "a panel face switch must move the fingerprint"
        );
    }

    /// FR2: the panel-face tick (two MAX queries per call) runs on the slow
    /// cadence, not on every 50 ms tick.
    #[test]
    fn slow_refresh_seeds_panel_baselines() {
        let (mut app, _dir) = test_app();
        app.panel_baselines_init = false;
        app.refresh_slow().unwrap();
        assert!(
            app.panel_baselines_init,
            "refresh_slow must run the panel-face tick"
        );
    }

    /// Two captures with no change in between are equal: the fingerprint
    /// hashes stored state, never wall-clock drift, so an idle slow run
    /// never draws on its own (FR5).
    #[test]
    fn fingerprint_stable_without_changes() {
        let (app, _dir) = test_app();
        assert_eq!(
            SlowFingerprint::capture(&app),
            SlowFingerprint::capture(&app),
            "settled data must fingerprint identically"
        );
    }
}
