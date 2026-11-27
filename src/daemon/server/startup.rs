use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::application::notification_service::{DefaultNotificationService, NotificationService};
use crate::application::ports::StateRepository;
use crate::db::Database;
use crate::executor::Executor;
use crate::graph_engine::GraphEngine;
use crate::rag::ingestion::IngestionManager;
use crate::watchers::WatcherEngine;

use super::personal_rag::startup_personal_rag;

pub(super) struct HttpCore {
    pub(super) notification_service: Arc<dyn NotificationService>,
    pub(super) scheduled_prompt_notify: Arc<tokio::sync::Notify>,
    pub(super) executor: Arc<Executor>,
    pub(super) graph_engine: Arc<GraphEngine>,
    pub(super) watcher_engine: Arc<WatcherEngine>,
    pub(super) ingestion: Arc<IngestionManager>,
    pub(super) dynamic_skills: Arc<crate::dynamic_skills::SkillStore>,
    pub(super) canopy_config: crate::domain::canopy_config::CanopyConfig,
}

pub(super) fn build_http_core(db: &Arc<Database>, data_dir: &std::path::Path) -> HttpCore {
    let notification_service: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
    // Shared wake handle for scheduled sends: every successful daemon-side
    // `insert_scheduled_prompt` notifies it, and the scheduler's dedicated
    // delivery task waits on it.
    let scheduled_prompt_notify = Arc::new(tokio::sync::Notify::new());
    let executor = Arc::new(
        Executor::new(Arc::clone(db), Arc::clone(&notification_service))
            .with_scheduled_prompt_notify(Arc::clone(&scheduled_prompt_notify)),
    );
    let canopy_config = crate::domain::canopy_config::CanopyConfig::load(data_dir);
    // CM51 (D12): remember what the running daemon loaded, so the config
    // tool's list action can report `pending_restart`, and watch the file so a hand edit takes
    // effect without a restart (D4).
    crate::domain::settings::set_running_snapshot(canopy_config.clone());
    crate::daemon::config_watch::spawn(data_dir.to_path_buf(), Arc::clone(db));
    let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::from_config(
        data_dir,
        &canopy_config.skills,
    ));
    // CB82: the process-wide shutdown flag — set first in graceful shutdown,
    // before any node process is SIGTERMed, so the engine finalizes in-flight
    // runs as interrupted instead of routing their fail edges.
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let graph_engine = Arc::new(
        GraphEngine::new(Arc::clone(db), Arc::clone(&notification_service))
            .with_ensemble_concurrency_cap(canopy_config.ensemble_concurrency_cap)
            .with_spec_attempt_limit(canopy_config.spec_attempt_limit)
            .with_dynamic_skills(Arc::clone(&dynamic_skills))
            .with_shutdown_flag(Arc::clone(&shutdown_flag))
            .with_scheduled_prompt_notify(Arc::clone(&scheduled_prompt_notify)),
    );
    let watcher_engine = Arc::new(WatcherEngine::new(
        Arc::clone(db),
        Arc::clone(&executor),
        Arc::clone(&graph_engine),
    ));
    let ingestion = Arc::new(IngestionManager::new(
        Arc::clone(db),
        data_dir.to_path_buf(),
    ));

    HttpCore {
        notification_service,
        scheduled_prompt_notify,
        executor,
        graph_engine,
        watcher_engine,
        ingestion,
        dynamic_skills,
        canopy_config,
    }
}

/// Refresh the embedded `canopy` skill files for harnesses that already
/// have one, so an update done by any other means is corrected on the next
/// start. Hand-written files and missing harness directories are skipped by
/// the installer itself; failures are warn-only, never fatal.
fn refresh_canopy_skill_files(data_dir: &std::path::Path) {
    let baseline =
        crate::domain::registry_baseline::RegistryBaseline::load(data_dir).unwrap_or_default();
    if baseline.platforms.is_empty() {
        return;
    }
    let Some(home) = dirs::home_dir() else {
        return;
    };
    for line in crate::skills_module::install_for_home(&home, &baseline.platforms, true) {
        tracing::info!("canopy skill: {line}");
    }
}

pub(super) async fn run_http_startup_reconciliations(
    db: &Arc<Database>,
    data_dir: &std::path::Path,
    core: &HttpCore,
    port: u16,
) {
    reconcile_orphaned_graphs_on_startup(db, data_dir, port);
    refresh_canopy_skill_files(data_dir);
    reconcile_stranded_queue_specs_on_startup(db);
    reconcile_mutex_leases_on_startup(db);
    migrate_agents_to_backing_graphs(db);
    if let Err(e) = db.requeue_in_review_specs() {
        tracing::warn!("Failed to requeue in_review specs: {e}");
    }
    crate::graph_engine::spec_review::start_worker(
        Arc::clone(db),
        Arc::clone(&core.scheduled_prompt_notify),
    );
    // CT28: drop live-output chunks whose run is finished — leftovers from a
    // crash, since completion paths delete their own chunks.
    if let Err(e) = db.delete_graph_run_output_for_non_running() {
        tracing::error!("Failed to clean stale graph_run_output chunks: {}", e);
    }
    capture_interrupted_state(&core.graph_engine, &core.watcher_engine).await;

    startup_personal_rag(Arc::clone(&core.ingestion), data_dir).await;
    // CB91: one maintenance pass at startup, after the reconciliations that
    // `startup_personal_rag` runs; the periodic tick continues from `start()`.
    core.ingestion.run_rag_maintenance().await;
}

/// One graph per background agent: every agent row gets its spec-less
/// backing graph before the scheduler and the watchers start, so the graph
/// triggers own every future fire. Idempotent; failures leave the row
/// unmigrated (never fired — the legacy firing paths are deleted) and are
/// reported, never fatal.
fn migrate_agents_to_backing_graphs(db: &Arc<Database>) {
    match super::super::agent_graph_migration::migrate_all_agents_to_graphs(db) {
        Ok(report) if !report.failed.is_empty() => {
            tracing::warn!(
                "Agent-to-graph migration left {} agent(s) unmigrated: {}",
                report.failed.len(),
                report
                    .failed
                    .iter()
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Ok(_) => {}
        Err(e) => tracing::error!("Agent-to-graph migration failed: {e:#}"),
    }
}

/// The startup banner line: version and port, logged once the port and data
/// directory are resolved.
pub(super) fn announce_daemon_start(port: u16) {
    tracing::info!(
        "canopy v{} starting on port {}",
        env!("CARGO_PKG_VERSION"),
        port
    );
}

/// The daemon's own record of being up: port, version and start time, in
/// that order — every later `daemon status` / doctor probe reads these.
pub(super) fn write_startup_state(db: &Database, port: u16) -> anyhow::Result<()> {
    db.set_state("port", &port.to_string())?;
    db.set_state("version", env!("CARGO_PKG_VERSION"))?;
    db.set_state("last_start", &chrono::Utc::now().to_rfc3339())?;
    Ok(())
}

/// Close interactive-session rows whose process is already gone. Called
/// AFTER the resume above, on purpose: rows the resume relaunched carry a
/// fresh live pid again, so whatever is still stale here belongs to a
/// process that is really gone. Reconciling first would close the rows the
/// resume needs and kill auto-resume.
pub(super) fn reconcile_stale_sessions_on_startup(db: &Database) {
    match db.reconcile_stale_interactive_sessions() {
        Ok(closed) if closed > 0 => {
            tracing::info!("Closed {closed} stale interactive session(s) on startup");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("Failed to reconcile stale interactive sessions: {e}"),
    }
}

/// Startup reconciliation 1/2: graphs a previous daemon left `Running`.
pub(super) fn reconcile_orphaned_graphs_on_startup(
    db: &Database,
    data_dir: &std::path::Path,
    port: u16,
) {
    match db.reconcile_orphaned_graphs(data_dir, port) {
        Ok(reconciled) if !reconciled.is_empty() => {
            tracing::warn!(
                "Reconciled {} graph(s) left running by a previous daemon",
                reconciled.len()
            );
            // CB84: best-effort end the harness sessions behind every
            // reconciled run — the previous daemon's harness service may
            // still be ticking. Detached so startup isn't blocked.
            for (lp, run) in reconciled {
                let platform = run.executed_platform.clone().unwrap_or_default();
                let workdir = lp.workdir.clone();
                if !platform.is_empty() {
                    tokio::spawn(async move {
                        crate::graph_engine::end_harness_session(&platform, &run, &workdir).await;
                    });
                }
            }
        }
        Ok(_) => {}
        Err(e) => tracing::error!("Failed to reconcile orphaned graphs: {}", e),
    }
}

/// Startup reconciliation 2/2: queue members a previous daemon left
/// `Running` after their graph was reconciled.
pub(super) fn reconcile_stranded_queue_specs_on_startup(db: &Database) {
    match db.reconcile_stranded_queue_specs() {
        Ok(count) if count > 0 => {
            tracing::warn!(
                "Reconciled {} stranded queue spec(s) left running by a previous daemon",
                count
            );
        }
        Ok(_) => {}
        Err(e) => tracing::error!("Failed to reconcile stranded queue specs: {}", e),
    }
}

/// Startup reconciliation 3/3: mutex leases a previous daemon left behind.
/// Runs after the graph reconciliations above (which finalize the dangling
/// runs whose leases this then frees).
pub(super) fn reconcile_mutex_leases_on_startup(db: &Database) {
    match db.reconcile_mutex_leases() {
        Ok((leases, waiters)) if leases > 0 || waiters > 0 => {
            tracing::warn!(
                "Reconciled {} stale mutex lease(s) and {} orphaned waiter(s) left by a previous daemon",
                leases,
                waiters
            );
        }
        Ok(_) => {}
        Err(e) => tracing::error!("Failed to reconcile mutex leases: {}", e),
    }
}

/// The two awaited startup reads, in order: capture the dirty-tree state of
/// interrupted specs, then reload the watcher set from the database.
pub(super) async fn capture_interrupted_state(
    graph_engine: &GraphEngine,
    watcher_engine: &WatcherEngine,
) {
    if let Err(e) = graph_engine.capture_end_dirty_for_interrupted_specs().await {
        tracing::error!(
            "Failed to capture dirty-tree state for interrupted specs: {}",
            e
        );
    }

    if let Err(e) = watcher_engine.reload_from_db().await {
        tracing::error!("Failed to reload watchers: {}", e);
    }
}
