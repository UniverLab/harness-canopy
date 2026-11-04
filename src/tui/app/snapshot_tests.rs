//! FR1/FR8 snapshot tests: the worker-mode slow tick performs no database
//! call on the calling thread, and the snapshot it swaps in carries the same
//! outputs the synchronous refresh reads.
use std::sync::Arc;

use tempfile::{tempdir, NamedTempFile};

use crate::application::ports::{AgentRepository, RunRepository, StateRepository};
use crate::db::connection::guard::with_ui_tick;
use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::domain::graphs::{Graph, GraphSpec, GraphSpecStatus, GraphStatus};
use crate::domain::models::{Agent, AgentAction, Cli, RunLog, RunStatus, TriggerType};
use crate::tui::app::hub::HubTab;
use crate::tui::app::snapshot::{SlowSnapshot, SnapshotWorker, WorkerState};
use crate::tui::app::snapshot_build::build_snapshot;
use crate::tui::app::types::{AgentEntry, App, PanelFace};

fn test_db() -> Arc<Database> {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(Database::new(&path).expect("create test db"))
}

fn test_app(db: &Arc<Database>) -> (App, tempfile::TempDir) {
    let data_dir = tempdir().expect("create data dir");
    let app =
        App::new(Arc::clone(db), data_dir.path(), &CanopyConfig::default()).expect("create app");
    (app, data_dir)
}

fn make_agent(id: &str) -> Agent {
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

fn make_run(id: &str, agent_id: &str, status: RunStatus) -> RunLog {
    RunLog {
        id: id.to_string(),
        background_agent_id: agent_id.to_string(),
        status,
        trigger_type: TriggerType::Manual,
        summary: Some("test run".to_string()),
        started_at: chrono::Utc::now(),
        finished_at: None,
        exit_code: None,
        timeout_at: None,
        executed_platform: None,
        executed_model: None,
    }
}

fn make_graph(id: &str) -> Graph {
    Graph {
        id: id.to_string(),
        name: format!("{id} name"),
        description: None,
        workdir: "/tmp/snap-test".to_string(),
        status: GraphStatus::Draft,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        archived: false,
        paused_by_reconciliation: false,
        infra_node_id: None,
        allow_dirty_start: false,
        require_spec_type: false,
    }
}

fn make_standalone_spec(id: &str) -> GraphSpec {
    GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: format!("{id} name"),
        description: None,
        position: 0,
        parallelizable: false,
        status: GraphSpecStatus::Pending,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    }
}

fn background_agent_ids(app: &App) -> Vec<String> {
    app.agents
        .iter()
        .filter_map(|entry| match entry {
            AgentEntry::Agent(agent) => Some(agent.id.clone()),
            _ => None,
        })
        .collect()
}

/// FR1/FR8: with a worker attached, the slow tick swaps the latest snapshot
/// in without a single database call on the calling thread — the guard armed
/// by `with_ui_tick` panics if one slips through.
#[test]
fn refresh_slow_with_worker_touches_no_database() {
    let db = test_db();
    db.upsert_agent(&make_agent("bg-guard")).unwrap();
    let (mut app, _data_dir) = test_app(&db);
    // Cover the port reads too: the Crews face and the Settings tab fetch
    // through them, so they must resolve from the snapshot-fed field.
    app.panel_face = PanelFace::Crews;
    app.crews.stale = true;
    app.hub_open = true;
    app.hub.tab = HubTab::Settings;
    app.hub.settings.stale = true;

    let (worker, snap_tx) = SnapshotWorker::fake();
    app.snapshot_worker = Some(worker);
    let snapshot = SlowSnapshot {
        daemon_version: "9.9.9-test".to_string(),
        daemon_port: 7761,
        agents: vec![make_agent("bg-from-snapshot")],
        hub_counts_digest: (1, 2, 3, 4),
        ..SlowSnapshot::default()
    };
    snap_tx.send(snapshot).unwrap();

    with_ui_tick(|| app.refresh_slow().expect("worker refresh"));

    assert_eq!(app.daemon_version, "9.9.9-test");
    assert_eq!(app.daemon_port, 7761);
    assert_eq!(background_agent_ids(&app), vec!["bg-from-snapshot"]);
    assert_eq!(app.hub_counts_digest, (1, 2, 3, 4));
    assert!(app.snapshot_dirty);
}

/// Negative control for the guard above: without a worker the same tick
/// under the same guard must panic — otherwise the guard proves nothing.
#[test]
fn refresh_slow_without_worker_panics_under_the_guard() {
    let db = test_db();
    let (mut app, _data_dir) = test_app(&db);
    assert!(app.snapshot_worker.is_none());

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_ui_tick(|| {
            let _ = app.refresh_slow();
        });
    }));
    let err = result.expect_err("sync refresh must trip the guard");
    let message = err
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| err.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("");
    assert!(message.contains("UI thread"), "unexpected panic: {message}");
}

/// The snapshot carries the same outputs the synchronous refresh reads: one
/// app refreshes synchronously, the other swaps in a freshly built snapshot,
/// and every slow-tick output agrees.
#[test]
fn worker_snapshot_matches_sync_refresh() {
    let db = test_db();
    db.upsert_agent(&make_agent("bg-eq")).unwrap();
    db.insert_run(&make_run("run-eq", "bg-eq", RunStatus::Pending))
        .unwrap();
    db.insert_graph(&make_graph("g-eq")).unwrap();
    db.insert_graph_spec(&make_standalone_spec("s-eq")).unwrap();
    db.set_state("version", "9.9.9-test").unwrap();
    db.set_state("port", "7761").unwrap();

    let (mut sync_app, _sync_dir) = test_app(&db);
    sync_app.refresh_slow().expect("sync refresh");

    let (mut worker_app, _worker_dir) = test_app(&db);
    let request = worker_app.build_snapshot_request();
    let snapshot = build_snapshot(&db, &mut WorkerState::default(), &request);
    worker_app.apply_snapshot(snapshot).expect("apply snapshot");

    assert_eq!(
        background_agent_ids(&worker_app),
        background_agent_ids(&sync_app)
    );
    let mut sync_active: Vec<_> = sync_app.active_runs.keys().cloned().collect();
    let mut worker_active: Vec<_> = worker_app.active_runs.keys().cloned().collect();
    sync_active.sort();
    worker_active.sort();
    assert_eq!(worker_active, sync_active);
    let sync_recent: Vec<_> = sync_app.recent_runs.iter().map(|r| &r.id).collect();
    let worker_recent: Vec<_> = worker_app.recent_runs.iter().map(|r| &r.id).collect();
    assert_eq!(worker_recent, sync_recent);
    let sync_backlog: Vec<_> = sync_app.backlog_specs.iter().map(|s| &s.id).collect();
    let worker_backlog: Vec<_> = worker_app.backlog_specs.iter().map(|s| &s.id).collect();
    assert_eq!(worker_backlog, sync_backlog);
    let sync_graphs: Vec<_> = sync_app.graphs.iter().map(|g| &g.id).collect();
    let worker_graphs: Vec<_> = worker_app.graphs.iter().map(|g| &g.id).collect();
    assert_eq!(worker_graphs, sync_graphs);
    assert_eq!(
        worker_app.archived_graph_count,
        sync_app.archived_graph_count
    );
    let mut sync_meta: Vec<_> = sync_app
        .graph_sidebar_meta
        .iter()
        .map(|(id, meta)| (id.clone(), meta.blocked))
        .collect();
    let mut worker_meta: Vec<_> = worker_app
        .graph_sidebar_meta
        .iter()
        .map(|(id, meta)| (id.clone(), meta.blocked))
        .collect();
    sync_meta.sort();
    worker_meta.sort();
    assert_eq!(worker_meta, sync_meta);
    assert_eq!(worker_app.selected_graph_id, sync_app.selected_graph_id);
    assert_eq!(
        worker_app.graph_details.as_ref().map(|d| &d.lp.id),
        sync_app.graph_details.as_ref().map(|d| &d.lp.id)
    );
    assert_eq!(
        worker_app
            .graph_live_state
            .as_ref()
            .map(|s| s.spec_queue.len()),
        sync_app
            .graph_live_state
            .as_ref()
            .map(|s| s.spec_queue.len())
    );
    assert_eq!(worker_app.daemon_version, sync_app.daemon_version);
    assert_eq!(worker_app.daemon_port, sync_app.daemon_port);
    assert_eq!(worker_app.hub_counts_digest, sync_app.hub_counts_digest);
    assert_eq!(worker_app.activity_digest, sync_app.activity_digest);
    assert_eq!(
        worker_app.overview_sessions_digest,
        sync_app.overview_sessions_digest
    );
    assert_eq!(
        worker_app.pending_sends_digest,
        sync_app.pending_sends_digest
    );
    assert_eq!(
        worker_app.rag_info.total_chunks,
        sync_app.rag_info.total_chunks
    );
    assert_eq!(
        worker_app.rag_info.indexed_files,
        sync_app.rag_info.indexed_files
    );
    let mut sync_preview: Vec<_> = sync_app.project_preview_cache.keys().cloned().collect();
    let mut worker_preview: Vec<_> = worker_app.project_preview_cache.keys().cloned().collect();
    sync_preview.sort();
    worker_preview.sort();
    assert_eq!(worker_preview, sync_preview);
    assert_eq!(worker_app.panel_face, sync_app.panel_face);
    assert_eq!(
        worker_app.panel_last_graph_running,
        sync_app.panel_last_graph_running
    );
    assert_eq!(worker_app.has_parallel_graph, sync_app.has_parallel_graph);
}

/// A dead worker retires itself and the tick falls back to synchronous reads.
#[test]
fn dead_worker_falls_back_to_sync() {
    let db = test_db();
    db.upsert_agent(&make_agent("bg-fallback")).unwrap();
    let (mut app, _data_dir) = test_app(&db);
    let (worker, snap_tx) = SnapshotWorker::fake();
    app.snapshot_worker = Some(worker);
    drop(snap_tx);

    app.refresh_slow().expect("fallback refresh");

    assert!(app.snapshot_worker.is_none());
    assert_eq!(background_agent_ids(&app), vec!["bg-fallback"]);
}

/// A swapped-in snapshot redraws: the fast tick reports dirty once, then the
/// flag is consumed.
#[test]
fn fast_tick_reports_a_fresh_snapshot() {
    let db = test_db();
    let (mut app, _data_dir) = test_app(&db);
    let (worker, snap_tx) = SnapshotWorker::fake();
    app.snapshot_worker = Some(worker);
    snap_tx.send(SlowSnapshot::default()).unwrap();

    app.refresh_slow().expect("worker refresh");
    assert!(app.snapshot_dirty);
    assert!(app.refresh_fast());
    assert!(!app.snapshot_dirty);
}
