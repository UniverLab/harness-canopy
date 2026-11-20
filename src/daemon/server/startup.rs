use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::application::notification_service::{DefaultNotificationService, NotificationService};
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

pub(super) async fn run_http_startup_reconciliations(
    db: &Arc<Database>,
    data_dir: &std::path::Path,
    core: &HttpCore,
) {
    super::reconcile_orphaned_graphs_on_startup(db, data_dir);
    super::reconcile_stranded_queue_specs_on_startup(db);
    super::reconcile_mutex_leases_on_startup(db);
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
    super::capture_interrupted_state(&core.graph_engine, &core.watcher_engine).await;

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
