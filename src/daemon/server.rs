use anyhow::Result;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use rmcp::ServiceExt;

use crate::application::notification_service::{DefaultNotificationService, NotificationService};
use crate::application::ports::StateRepository;
use crate::daemon::handler::HandlerDeps;
use crate::daemon::health_routine::HealthRoutine;
use crate::daemon::process::{
    acquire_daemon_lock, kill_port_occupant, remove_pid_file, write_pid_file,
};
use crate::daemon::TaskTriggerHandler;
use crate::db::Database;
use crate::domain::db_paths::database_path;
use crate::executor::Executor;
use crate::graph_engine::GraphEngine;
use crate::rag::ingestion::IngestionManager;
use crate::scheduler::cron_scheduler::CronScheduler;
use crate::watchers::WatcherEngine;

mod personal_rag;

use personal_rag::startup_personal_rag;

#[cfg(test)]
use personal_rag::{
    migrate_personal_rag_chunking, migrate_personal_rag_model, prepare_personal_rag_roots,
    queue_existing_personal_rag_files, reload_personal_rag_queue, RAG_CHUNKING_VERSION,
};

pub(crate) async fn run_http_server(port_override: Option<u16>) -> Result<()> {
    crate::domain::notification::register_aumid();
    crate::domain::notification::clear_stale_notifications();
    init_tracing();

    // CB83: one instant per daemon process; every MCP session's handler
    // inherits it so `agent_status` Uptime is the daemon's age.
    let daemon_start = std::time::Instant::now();

    let port = crate::resolve_port(port_override);
    let data_dir = crate::ensure_data_dir()?;
    let _daemon_lock = acquire_daemon_lock(&data_dir)?;
    let db = Arc::new(Database::new(&database_path(&data_dir))?);
    if let Err(e) = crate::domain::prompts::seed_builtin_prompt_presets(&data_dir) {
        tracing::warn!("Could not seed builtin prompt presets: {e}");
    }
    let notification_service: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
    let executor = Arc::new(Executor::new(
        Arc::clone(&db),
        Arc::clone(&notification_service),
    ));
    let canopy_config = crate::domain::canopy_config::CanopyConfig::load(&data_dir);
    // CM51 (D12): remember what the running daemon loaded, so `config_list`
    // can report `pending_restart`, and watch the file so a hand edit takes
    // effect without a restart (D4).
    crate::domain::settings::set_running_snapshot(canopy_config.clone());
    crate::daemon::config_watch::spawn(data_dir.clone(), Arc::clone(&db));
    let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::from_config(
        &data_dir,
        &canopy_config.skills,
    ));
    // CB82: the process-wide shutdown flag — set first in graceful shutdown,
    // before any node process is SIGTERMed, so the engine finalizes in-flight
    // runs as interrupted instead of routing their fail edges.
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let graph_engine = Arc::new(
        GraphEngine::new(Arc::clone(&db), Arc::clone(&notification_service))
            .with_ensemble_concurrency_cap(canopy_config.ensemble_concurrency_cap)
            .with_spec_attempt_limit(canopy_config.spec_attempt_limit)
            .with_dynamic_skills(Arc::clone(&dynamic_skills))
            .with_shutdown_flag(Arc::clone(&shutdown_flag)),
    );
    let watcher_engine = Arc::new(WatcherEngine::new(
        Arc::clone(&db),
        Arc::clone(&executor),
        Arc::clone(&graph_engine),
    ));

    let ingestion = Arc::new(IngestionManager::new(Arc::clone(&db), data_dir.clone()));
    let _ingestion_cancel = Arc::clone(&ingestion).start();

    announce_daemon_start(port);
    write_pid_file(&data_dir)?;
    write_startup_state(&db, port)?;

    reconcile_orphaned_graphs_on_startup(&db, &data_dir);
    reconcile_stranded_queue_specs_on_startup(&db);
    reconcile_mutex_leases_on_startup(&db);
    // CT28: drop live-output chunks whose run is finished — leftovers from a
    // crash, since completion paths delete their own chunks.
    if let Err(e) = db.delete_graph_run_output_for_non_running() {
        tracing::error!("Failed to clean stale graph_run_output chunks: {}", e);
    }
    capture_interrupted_state(&graph_engine, &watcher_engine).await;

    startup_personal_rag(Arc::clone(&ingestion), &data_dir).await;
    // CB91: one maintenance pass at startup, after the reconciliations that
    // `startup_personal_rag` runs; the periodic tick continues from `start()`.
    ingestion.run_rag_maintenance().await;

    let background = start_background_tasks(
        &db,
        &executor,
        &graph_engine,
        &notification_service,
        &data_dir,
        canopy_config.announcements_enabled,
    );

    let ct = tokio_util::sync::CancellationToken::new();
    let router = build_mcp_router(
        HandlerDeps {
            db: Arc::clone(&db),
            executor: Arc::clone(&executor),
            watcher_engine: Arc::clone(&watcher_engine),
            scheduler_notify: Arc::clone(&background.scheduler_notify),
            graph_engine: Arc::clone(&graph_engine),
            notification_service: Arc::clone(&notification_service),
            ingestion: Arc::clone(&ingestion),
            dynamic_skills: Arc::clone(&dynamic_skills),
            start_time: daemon_start,
            port,
        },
        &ct,
    );

    bind_and_serve(router, port, ct).await?;

    shutdown_http_server(background, watcher_engine, &db, &data_dir, &graph_engine).await;

    Ok(())
}

/// The startup banner line: version and port, logged once the port and data
/// directory are resolved.
fn announce_daemon_start(port: u16) {
    tracing::info!(
        "canopy v{} starting on port {}",
        env!("CARGO_PKG_VERSION"),
        port
    );
}

/// The daemon's own record of being up: port, version and start time, in
/// that order — every later `daemon status` / doctor probe reads these.
fn write_startup_state(db: &Database, port: u16) -> Result<()> {
    db.set_state("port", &port.to_string())?;
    db.set_state("version", env!("CARGO_PKG_VERSION"))?;
    db.set_state("last_start", &chrono::Utc::now().to_rfc3339())?;
    Ok(())
}

/// Startup reconciliation 1/2: graphs a previous daemon left `Running`.
fn reconcile_orphaned_graphs_on_startup(db: &Database, data_dir: &std::path::Path) {
    match db.reconcile_orphaned_graphs(data_dir) {
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
fn reconcile_stranded_queue_specs_on_startup(db: &Database) {
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
fn reconcile_mutex_leases_on_startup(db: &Database) {
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
async fn capture_interrupted_state(graph_engine: &GraphEngine, watcher_engine: &WatcherEngine) {
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

/// The daemon's background tasks plus the tokens that stop them, started in
/// the order the daemon has always started them: cron scheduler, health
/// routine, then (only when enabled) the announcements client.
struct HttpBackgroundTasks {
    scheduler_notify: Arc<tokio::sync::Notify>,
    scheduler_cancel: tokio_util::sync::CancellationToken,
    health_routine_cancel: tokio_util::sync::CancellationToken,
    announcements_cancel: Option<tokio_util::sync::CancellationToken>,
}

fn start_background_tasks(
    db: &Arc<Database>,
    executor: &Arc<Executor>,
    graph_engine: &Arc<GraphEngine>,
    notification_service: &Arc<dyn NotificationService>,
    data_dir: &std::path::Path,
    announcements_enabled: bool,
) -> HttpBackgroundTasks {
    let cron_scheduler = Arc::new(
        CronScheduler::with_graphs(
            Arc::clone(db),
            Arc::clone(executor),
            Arc::clone(graph_engine),
        )
        .with_retry(crate::scheduler::cron_scheduler::RetryPolicy::from_config(
            &crate::domain::canopy_config::CanopyConfig::load(data_dir).scheduler,
        )),
    );
    let scheduler_notify = cron_scheduler.notifier();
    let scheduler_cancel = Arc::clone(&cron_scheduler).start();

    let health_routine = Arc::new(HealthRoutine::new(Arc::clone(db), data_dir.to_path_buf()));
    let health_routine_cancel = health_routine.start();

    let announcements_cancel = if announcements_enabled {
        let client = Arc::new(crate::daemon::announcements::AnnouncementsClient::new(
            Arc::clone(db),
            Arc::clone(notification_service),
        ));
        Some(client.start())
    } else {
        None
    };

    HttpBackgroundTasks {
        scheduler_notify,
        scheduler_cancel,
        health_routine_cancel,
        announcements_cancel,
    }
}

/// Everything the `/mcp` handler closure clones per session, plus the port
/// it reports — the daemon-side owners stay alive in `run_http_server`.
/// `HandlerDeps` (re-exported from handler) is the canonical bundle.
/// Builds the `/mcp` router: the streamable-HTTP service (one
/// `TaskTriggerHandler` per session), the session manager, and the server
/// config wired to the shutdown cancellation token, layered with the
/// error-status logger.
fn build_mcp_router(deps: HandlerDeps, ct: &tokio_util::sync::CancellationToken) -> axum::Router {
    let HandlerDeps {
        db: handler_db,
        executor: handler_executor,
        watcher_engine: handler_watcher_engine,
        scheduler_notify: handler_scheduler_notify,
        graph_engine: handler_graph_engine,
        notification_service,
        ingestion: handler_ingestion,
        dynamic_skills: handler_dynamic_skills,
        start_time,
        port,
    } = deps;

    let service = rmcp::transport::streamable_http_server::StreamableHttpService::new(
        move || {
            Ok(TaskTriggerHandler::with_deps(HandlerDeps {
                db: Arc::clone(&handler_db),
                executor: Arc::clone(&handler_executor),
                watcher_engine: Arc::clone(&handler_watcher_engine),
                scheduler_notify: Arc::clone(&handler_scheduler_notify),
                graph_engine: Arc::clone(&handler_graph_engine),
                notification_service: Arc::clone(&notification_service),
                ingestion: Arc::clone(&handler_ingestion),
                dynamic_skills: Arc::clone(&handler_dynamic_skills),
                start_time,
                port,
            }))
        },
        {
            let mut mgr =
                rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default();
            mgr.session_config.keep_alive = None;
            mgr.into()
        },
        {
            #[allow(clippy::field_reassign_with_default)]
            {
                let mut cfg =
                    rmcp::transport::streamable_http_server::StreamableHttpServerConfig::default();
                cfg.cancellation_token = ct.child_token();
                cfg
            }
        },
    );

    axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn(log_mcp_error_responses))
}

/// Frees the port (a stale occupant from a previous daemon), binds, logs the
/// listening address, and serves until the shutdown signal cancels `ct`.
async fn bind_and_serve(
    router: axum::Router,
    port: u16,
    ct: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let bind_addr = format!("127.0.0.1:{port}");

    kill_port_occupant(port);

    let tcp_listener = tokio::net::TcpListener::bind(&bind_addr).await?;

    tracing::info!(
        "Streamable HTTP MCP server listening on http://{}/mcp",
        bind_addr
    );

    axum::serve(tcp_listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            tracing::info!("Shutdown signal received");
            ct.cancel();
        })
        .await?;

    Ok(())
}

/// The shutdown sequence, in order: cancel the scheduler, the health
/// routine and (when it was started) the announcements client; await
/// watcher stop; terminate this daemon's node processes; remove the PID
/// file; clear notifications; log stopped.
async fn shutdown_http_server(
    background: HttpBackgroundTasks,
    watcher_engine: Arc<WatcherEngine>,
    db: &Arc<Database>,
    data_dir: &std::path::Path,
    graph_engine: &Arc<GraphEngine>,
) {
    // CB82: FIRST — before the scheduler/health cancels and before
    // `terminate_all_...` sends SIGTERM — so the engine finalizes in-flight
    // runs as interrupted instead of routing the fail edge of a killed node.
    graph_engine.mark_shutting_down();
    background.scheduler_cancel.cancel();
    background.health_routine_cancel.cancel();
    if let Some(cancel) = background.announcements_cancel {
        cancel.cancel();
    }
    watcher_engine.stop_all().await;
    terminate_all_running_node_processes_at_shutdown(db).await;
    remove_pid_file(data_dir);
    crate::domain::notification::clear_notifications_on_exit();
    tracing::info!("Daemon stopped");
}

/// rmcp's `/mcp` handler returns 404 "Session not found" (and other non-2xx
/// statuses) without ever calling `tracing::` — see the two 404 branches in
/// `rmcp::transport::streamable_http_server::tower`. That silence is exactly
/// what let a bridge's stale post-restart session id fail forever without a
/// trace in daemon.log. This logs the status and whether a session id was
/// attached (never the header value, and never the auth token) for any
/// `/mcp` response with status >= 400.
async fn log_mcp_error_responses(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let session_id_present = request.headers().contains_key("mcp-session-id");
    let response = next.run(request).await;
    let status = response.status();
    if status.as_u16() >= 400 {
        tracing::warn!(
            status = %status,
            session_id_present,
            "/mcp request failed"
        );
    }
    response
}

/// Terminate every graph node run's process on the machine (B12), so a
/// graceful daemon shutdown never leaves a `mimo run`/check process behind
/// the way an abandoned timeout used to. Correct only because it is called
/// exactly once, at daemon shutdown, from the sole process that ever holds
/// the daemon lock (`acquire_daemon_lock` in [`run_http_server`]) — at that
/// point every `Running` node run in the database genuinely is this
/// process's own, so scanning all of them needs no ownership filter. Any
/// caller without that guarantee (a bridge, an embedded stdio server, a CLI
/// subcommand) must never call this; see the module-level invariant that
/// only the daemon lifecycle process may act on graphs globally.
/// `SIGTERM`s every process up front, waits out a single shared grace
/// period, then `SIGKILL`s survivors — awaited inline (unlike the detached
/// `terminate_process_group_async` used elsewhere) because the daemon
/// process is about to exit, so a detached grace-kill task would never get
/// to fire, and a *sequential* terminate-and-wait per process would
/// multiply the shutdown delay by the number of processes instead of
/// bounding it by one grace period total.
///
/// CB82: the shutdown flag is already set before this runs, so rows still
/// `Running` here are ones whose dispatch died without finalizing — they are
/// marked `interrupted` (not `terminated`) so the same output-based check
/// covers late completions from both the engine finalizer and this sweep.
async fn terminate_all_running_node_processes_at_shutdown(db: &Database) {
    let Ok(runs) = db.list_all_running_graph_runs() else {
        return;
    };
    if runs.is_empty() {
        return;
    }

    #[cfg(unix)]
    for run in &runs {
        if let Some(pid) = run.pid {
            let _ = crate::daemon::process::send_signal_to_group(pid as i32, libc::SIGTERM);
        }
    }
    #[cfg(unix)]
    {
        tokio::time::sleep(crate::daemon::process::KILL_GRACE).await;
        for run in &runs {
            if let Some(pid) = run.pid {
                let _ = crate::daemon::process::send_signal_to_group(pid as i32, libc::SIGKILL);
            }
        }
    }

    // CB84: after the SIGKILL pass, best-effort end every run's harness
    // session concurrently — the opencode v2 service-side session keeps
    // ticking after the client process group is gone, so this is the only
    // way to stop it. One detached task per run so they run in parallel (and
    // in parallel with the DB finalization below), each with its own 30s
    // timeout: total wall time stays inside the existing KILL_GRACE budget
    // plus at most 30s.
    for run in &runs {
        let Some(platform) = run.executed_platform.clone() else {
            continue;
        };
        let workdir = db
            .get_graph(&run.graph_id)
            .ok()
            .flatten()
            .map(|g| g.workdir)
            .unwrap_or_default();
        let run = run.clone();
        tokio::spawn(async move {
            crate::graph_engine::end_harness_session(&platform, &run, &workdir).await;
        });
    }

    for run in &runs {
        let _ = db.update_graph_run_result(
            &run.id,
            crate::domain::graphs::GraphRunStatus::Fail,
            Some(&serde_json::json!({ "interrupted": true, "reason": "daemon shutdown" })),
            Some(chrono::Utc::now()),
        );
    }
}

pub(crate) async fn run_stdio_server() -> Result<()> {
    init_tracing();
    tracing::info!("Starting in stdio MCP transport mode");

    let data_dir = crate::ensure_data_dir()?;
    let db = Arc::new(Database::new_safe(&database_path(&data_dir), &data_dir)?);
    let startup = stdio_server_startup(Arc::clone(&db), &data_dir).await;

    serve_stdio_transport(&db, &startup).await?;
    stop_stdio_server(startup).await;

    Ok(())
}

/// The MCP handler over the startup dependencies — port 0, because stdio
/// has no port of its own to report.
fn build_stdio_handler(db: &Arc<Database>, startup: &StdioServerStartup) -> TaskTriggerHandler {
    TaskTriggerHandler::with_deps(HandlerDeps {
        db: Arc::clone(db),
        executor: Arc::clone(&startup.executor),
        watcher_engine: Arc::clone(&startup.watcher_engine),
        scheduler_notify: Arc::clone(&startup.scheduler_notify),
        graph_engine: Arc::clone(&startup.graph_engine),
        notification_service: Arc::clone(&startup.notification_service),
        ingestion: Arc::clone(&startup.ingestion),
        dynamic_skills: Arc::clone(&startup.dynamic_skills),
        start_time: startup.start_time,
        port: 0,
    })
}

/// Serves JSON-RPC over the stdio transport until the client closes it:
/// build the handler, serve, log the started line, then wait for the
/// transport to end.
async fn serve_stdio_transport(db: &Arc<Database>, startup: &StdioServerStartup) -> Result<()> {
    let handler = build_stdio_handler(db, startup);

    let transport = rmcp::transport::stdio();
    let server = handler.serve(transport).await?;
    tracing::info!("MCP stdio server started");

    server.waiting().await?;
    Ok(())
}

/// The stdio cleanup sequence, in order: scheduler stop (a stop, not a
/// cancellation), await watcher stop, clear notifications, then the
/// stopped log. The startup guards (`_ingestion_cancel`, scheduler and
/// health cancellation tokens) drop with `startup` right after.
async fn stop_stdio_server(startup: StdioServerStartup) {
    startup.cron_scheduler.stop();
    startup.watcher_engine.stop_all().await;
    crate::domain::notification::clear_notifications_on_exit();
    tracing::info!("Stdio server stopped");
}

/// Everything `run_stdio_server` needs beyond `db` itself: the handler's
/// dependencies plus the background-task guards that must outlive the serve
/// graph. Split out from `run_stdio_server` so a test can drive this
/// DB-touching startup sequence directly, without a real stdio transport, and
/// assert it never mutates graph state — see the doc comment on
/// `Database::reconcile_orphaned_graphs` for the incident this guards
/// against: this function deliberately does not call `reconcile_orphaned_graphs`
/// or `reconcile_stranded_queue_specs`. A stdio server is not the daemon and
/// must never perform daemon-lifecycle graph recovery.
struct StdioServerStartup {
    executor: Arc<Executor>,
    watcher_engine: Arc<WatcherEngine>,
    notification_service: Arc<dyn NotificationService>,
    graph_engine: Arc<GraphEngine>,
    ingestion: Arc<IngestionManager>,
    dynamic_skills: Arc<crate::dynamic_skills::SkillStore>,
    /// CB83: captured in `stdio_server_startup`, when this stdio server
    /// process starts — shared by the handler so `agent_status` Uptime
    /// matches the process, not a late-built session.
    start_time: std::time::Instant,
    cron_scheduler: Arc<CronScheduler>,
    scheduler_notify: Arc<tokio::sync::Notify>,
    _ingestion_cancel: tokio_util::sync::CancellationToken,
    _scheduler_cancel: tokio_util::sync::CancellationToken,
    _health_routine_cancel: tokio_util::sync::CancellationToken,
}

async fn stdio_server_startup(db: Arc<Database>, data_dir: &std::path::Path) -> StdioServerStartup {
    if let Err(e) = crate::domain::prompts::seed_builtin_prompt_presets(data_dir) {
        tracing::warn!("Could not seed builtin prompt presets: {e}");
    }
    let notification_service: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
    let executor = Arc::new(Executor::new(
        Arc::clone(&db),
        Arc::clone(&notification_service),
    ));
    let canopy_config = crate::domain::canopy_config::CanopyConfig::load(data_dir);
    let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::from_config(
        data_dir,
        &canopy_config.skills,
    ));
    let graph_engine = Arc::new(
        GraphEngine::new(Arc::clone(&db), Arc::clone(&notification_service))
            .with_ensemble_concurrency_cap(canopy_config.ensemble_concurrency_cap)
            .with_spec_attempt_limit(canopy_config.spec_attempt_limit)
            .with_dynamic_skills(Arc::clone(&dynamic_skills)),
    );
    let watcher_engine = Arc::new(WatcherEngine::new(
        Arc::clone(&db),
        Arc::clone(&executor),
        Arc::clone(&graph_engine),
    ));

    let ingestion = Arc::new(IngestionManager::new(
        Arc::clone(&db),
        data_dir.to_path_buf(),
    ));
    let ingestion_cancel = Arc::clone(&ingestion).start();

    // No `reconcile_orphaned_graphs` / `reconcile_stranded_queue_specs` call
    // here: this is a stdio MCP server, not the daemon, and it must never
    // perform daemon-lifecycle graph recovery — see the doc comment on
    // `Database::reconcile_orphaned_graphs` for the incident this guards
    // against.
    // No announcements client here: this is a stdio MCP server, not the
    // daemon, and the announcements WebSocket is a daemon-only background task.
    startup_personal_rag(Arc::clone(&ingestion), data_dir).await;
    // CB91: startup maintenance pass — see `run_http_server`'s identical call.
    ingestion.run_rag_maintenance().await;

    if let Err(e) = watcher_engine.reload_from_db().await {
        tracing::error!("Failed to reload watchers: {}", e);
    }

    let cron_scheduler = Arc::new(
        CronScheduler::with_graphs(
            Arc::clone(&db),
            Arc::clone(&executor),
            Arc::clone(&graph_engine),
        )
        .with_retry(crate::scheduler::cron_scheduler::RetryPolicy::from_config(
            &canopy_config.scheduler,
        )),
    );
    let scheduler_notify = cron_scheduler.notifier();
    let scheduler_cancel = Arc::clone(&cron_scheduler).start();

    let health_routine = Arc::new(HealthRoutine::new(Arc::clone(&db), data_dir.to_path_buf()));
    let health_routine_cancel = health_routine.start();

    StdioServerStartup {
        executor,
        watcher_engine,
        notification_service,
        graph_engine,
        ingestion,
        dynamic_skills,
        start_time: std::time::Instant::now(),
        cron_scheduler,
        scheduler_notify,
        _ingestion_cancel: ingestion_cancel,
        _scheduler_cancel: scheduler_cancel,
        _health_routine_cancel: health_routine_cancel,
    }
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");

        tokio::select! {
            _ = ctrl_c => {},
            _ = sigterm.recv() => {},
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
    }
}

fn init_tracing() {
    // Logs must never touch stdout: in stdio MCP mode it is reserved
    // exclusively for JSON-RPC messages.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing_subscriber::filter::LevelFilter::INFO.into()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

/// DIAGNOSTIC (temporary): reproduces the reported hang over the *real*
/// transport a node actually uses — a Streamable HTTP POST that goes through
/// `rmcp`'s `LocalSessionManager`/SSE machinery, not a direct in-process call
/// into `TaskTriggerHandler` (which the `diag_concurrent_db_write_during_dispatch`
/// test in `graph_engine.rs` already proved returns in milliseconds even mid-
/// dispatch). Isolates whether the hang lives in the HTTP/session layer.
#[cfg(test)]
mod hang_repro {
    use super::*;
    use crate::application::notification_service::{
        DefaultNotificationService, NotificationService,
    };
    use crate::daemon::TaskTriggerHandler;
    use crate::domain::graphs::{
        Graph, GraphNode, GraphNodeKind, GraphSpec, GraphSpecStatus, GraphStatus,
    };
    use crate::executor::Executor;
    use crate::graph_engine::GraphEngine;
    use crate::rag::ingestion::IngestionManager;
    use crate::watchers::WatcherEngine;
    use std::os::unix::fs::PermissionsExt;

    /// Serializes tests in this module that touch `CANOPY_HOME_OVERRIDE`
    /// (process-wide env var) against each other. Mirrors the `HomeGuard`
    /// pattern in `graph_engine.rs`'s test module (a distinct static — this is
    /// a diagnostic test run in isolation, not meant to coexist with the
    /// wider suite's parallelism).
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn setup_sleeping_cli_home() -> tempfile::TempDir {
        let fake_home = tempfile::tempdir().unwrap();
        let canopy_dir = fake_home.path().join(".canopy");
        std::fs::create_dir_all(&canopy_dir).unwrap();
        let script = fake_home.path().join("sleep-cli.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nsleep \"${SLEEP_SECONDS:-4}\"\necho done\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let config = crate::domain::canopy_config::CanopyConfig {
            configured_at: Some(chrono::Utc::now().to_rfc3339()),
            clis: vec![crate::domain::cli_config::CliConfig {
                name: "sleep-cli".to_string(),
                binary: script.to_string_lossy().to_string(),
                headless_mode: "-c".to_string(),
                model_flag: None,
                supports_working_dir: false,
                working_dir_flag: None,
                env_vars: std::collections::HashMap::new(),
                interactive_args: None,
                fallback_interactive_args: None,
                resume_args: None,
                session_list_cmd: None,
                session_resume_cmd: None,
                accent_color: None,
                yolo_flag: None,
                prompt_via_stdin: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        config.save_for_tests(&canopy_dir).unwrap();
        fake_home
    }

    /// Spawn a bare-bones Streamable HTTP MCP server (same construction as
    /// `run_http_server`, minus pid-file/daemon-lock/graceful-shutdown
    /// machinery) bound to an OS-chosen port. Returns the bound port and the
    /// pieces the test needs to drive a concurrent dispatch directly.
    async fn spawn_test_server(db: Arc<Database>) -> (u16, Arc<GraphEngine>) {
        let notif: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
        let executor = Arc::new(Executor::new(Arc::clone(&db), Arc::clone(&notif)));
        let graph_engine = Arc::new(GraphEngine::new(Arc::clone(&db), Arc::clone(&notif)));
        let watcher_engine = Arc::new(WatcherEngine::new(
            Arc::clone(&db),
            Arc::clone(&executor),
            Arc::clone(&graph_engine),
        ));
        let tmp = tempfile::tempdir().unwrap();
        let ingestion = Arc::new(IngestionManager::new(
            Arc::clone(&db),
            tmp.path().to_path_buf(),
        ));
        let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::new(
            tmp.path().join("skills"),
            Vec::new(),
            15,
        ));
        let cron_scheduler = Arc::new(
            crate::scheduler::cron_scheduler::CronScheduler::with_graphs(
                Arc::clone(&db),
                Arc::clone(&executor),
                Arc::clone(&graph_engine),
            ),
        );
        let scheduler_notify = cron_scheduler.notifier();
        let _scheduler_cancel = Arc::clone(&cron_scheduler).start();

        let handler_db = Arc::clone(&db);
        let handler_executor = Arc::clone(&executor);
        let handler_watcher_engine = Arc::clone(&watcher_engine);
        let handler_scheduler_notify = Arc::clone(&scheduler_notify);
        let handler_graph_engine = Arc::clone(&graph_engine);
        let handler_notif = Arc::clone(&notif);
        let handler_ingestion = Arc::clone(&ingestion);
        let handler_dynamic_skills = Arc::clone(&dynamic_skills);

        let service = rmcp::transport::streamable_http_server::StreamableHttpService::new(
            move || {
                Ok(TaskTriggerHandler::with_deps(HandlerDeps {
                    db: Arc::clone(&handler_db),
                    executor: Arc::clone(&handler_executor),
                    watcher_engine: Arc::clone(&handler_watcher_engine),
                    scheduler_notify: Arc::clone(&handler_scheduler_notify),
                    graph_engine: Arc::clone(&handler_graph_engine),
                    notification_service: Arc::clone(&handler_notif),
                    ingestion: Arc::clone(&handler_ingestion),
                    dynamic_skills: Arc::clone(&handler_dynamic_skills),
                    start_time: std::time::Instant::now(),
                    port: 0,
                }))
            },
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default()
                .into(),
            Default::default(),
        );
        let router = axum::Router::new().nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        // give the listener a beat to start accepting
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (port, graph_engine)
    }

    struct McpClient {
        client: reqwest::Client,
        endpoint: String,
        session_id: Option<String>,
    }

    impl McpClient {
        fn new(port: u16) -> Self {
            Self {
                client: reqwest::Client::new(),
                endpoint: format!("http://127.0.0.1:{port}/mcp"),
                session_id: None,
            }
        }

        /// POST one JSON-RPC message, return the concatenated `data:` payload(s)
        /// of the SSE response body (mirrors `bridge.rs`'s `forward_request`).
        async fn post(&mut self, body: serde_json::Value) -> anyhow::Result<String> {
            let mut req = self
                .client
                .post(&self.endpoint)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(
                    reqwest::header::ACCEPT,
                    "application/json, text/event-stream",
                )
                .body(body.to_string());
            if let Some(sid) = &self.session_id {
                req = req.header("mcp-session-id", sid);
            }
            let resp = req.send().await?;
            if let Some(sid) = resp.headers().get("mcp-session-id") {
                self.session_id = Some(sid.to_str()?.to_string());
            }
            let text = resp.text().await?;
            let mut out = String::new();
            for line in text.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    out.push_str(data.trim());
                }
            }
            Ok(out)
        }

        async fn initialize(&mut self) -> anyhow::Result<()> {
            self.post(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "repro", "version": "0.1"}
                }
            }))
            .await?;
            // notifications/initialized — no id, server responds 202 Accepted.
            self.post(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }))
            .await?;
            Ok(())
        }

        async fn call_tool(
            &mut self,
            name: &str,
            args: serde_json::Value,
        ) -> anyhow::Result<String> {
            self.post(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": name, "arguments": args}
            }))
            .await
        }
    }

    fn insert_graph_and_sleeping_node(db: &Database, workdir: &str) -> (String, String) {
        let graph_id = "wf-hang-repro".to_string();
        let spec_id = "spec-hang-repro".to_string();
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: graph_id.clone(),
            name: "Repro Graph".to_string(),
            description: None,
            workdir: workdir.to_string(),
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
            require_spec_type: false,
        })
        .unwrap();
        db.insert_graph_spec(&GraphSpec {
            id: spec_id.clone(),
            graph_id: Some(graph_id.clone()),
            name: "Spec".to_string(),
            description: Some("desc".to_string()),
            position: 1,
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
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "node-sleep".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "sleep".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": "sleep-cli",
                "timeout_minutes": 1,
            }),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        (graph_id, spec_id)
    }

    /// Regression (a): while a node is mid-dispatch on graph L (agent process
    /// running, graph status == `Running`), a *second*, independent MCP
    /// session — standing in for that node's own bridge connection calling
    /// back into the daemon — calls `graph_control` for the SAME
    /// graph L over real Streamable HTTP, exactly as a resilience node
    /// scheduling its own graph's resume does in production. The response
    /// must arrive promptly (well under any node timeout) and the schedule
    /// must be durably persisted.
    ///
    /// Also covers requirement 3 (fast-fail): `graph_control` and `graph_control`
    /// against the SAME graph, mid-dispatch, must reject immediately with an
    /// explanatory error rather than blocking — proven here under a real
    /// concurrent dispatch, not just against a synthetic `GraphStatus::Running`
    /// value.
    #[tokio::test]
    // The HOME_LOCK guard is held for the entire test lifetime so concurrent
    // runs of CANOPY_HOME_OVERRIDE-touching tests can't race on the env var.
    // Holding a `std::sync::MutexGuard` across `.await` points is intentional
    // here (the lock is process-wide and is *meant* to be serializing) — and
    // clippy's await-holding-lock lint can't tell the difference between
    // "unintentional deadlock risk" and "deliberate cross-async test
    // serialization", so opt out at the test level.
    #[allow(clippy::await_holding_lock)]
    async fn node_initiated_autorun_call_during_own_dispatch_over_http() {
        let _home_lock = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let fake_home = setup_sleeping_cli_home();
        let prev_home = std::env::var("CANOPY_HOME_OVERRIDE").ok();
        unsafe {
            std::env::set_var("CANOPY_HOME_OVERRIDE", fake_home.path());
        }

        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
        let (graph_id, _spec_id) =
            insert_graph_and_sleeping_node(&db, &dir.path().to_string_lossy());

        let (port, graph_engine) = spawn_test_server(Arc::clone(&db)).await;

        let dispatch_graph_id = graph_id.clone();
        let dispatch = tokio::spawn(async move {
            graph_engine
                .run_graph(dispatch_graph_id, None, None, None, None, None)
                .await
        });

        // Wait until the graph is actually Running (node process spawned).
        for _ in 0..50 {
            if db.get_graph(&graph_id).unwrap().unwrap().status == GraphStatus::Running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(
            db.get_graph(&graph_id).unwrap().unwrap().status,
            GraphStatus::Running,
            "dispatch never reached Running before the repro call"
        );

        let mut client = McpClient::new(port);
        client.initialize().await.unwrap();

        let at_dt = chrono::Utc::now() + chrono::Duration::hours(1);
        let at = at_dt.to_rfc3339();
        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.call_tool(
                "graph_control",
                serde_json::json!({"action": "schedule_autorun", "graph_id": graph_id, "at": at}),
            ),
        )
        .await;
        let elapsed = start.elapsed();

        let schedule_response = result
            .expect("node-initiated graph_control must respond promptly mid-dispatch")
            .expect("graph_control call must succeed");
        assert!(
            schedule_response.contains("scheduled to autorun"),
            "{schedule_response}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "response took {elapsed:?}, expected well under the node timeout"
        );

        let persisted = db.get_graph(&graph_id).unwrap().unwrap().autorun_at;
        assert_eq!(
            persisted.map(|v| v.timestamp()),
            Some(at_dt.timestamp()),
            "schedule must be durably persisted at the requested instant"
        );

        // Requirement 3: graph_control / graph_control against the SAME graph, still
        // mid-dispatch, must fail fast (not hang, not queue).
        let run_result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.call_tool(
                "graph_control",
                serde_json::json!({"action": "run", "graph_id": graph_id}),
            ),
        )
        .await
        .expect("graph_control must respond promptly against a running graph")
        .unwrap();
        assert!(
            run_result.contains("already running"),
            "expected fast-fail error, got: {run_result}"
        );

        let reset_result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.call_tool(
                "graph_control",
                serde_json::json!({"action": "reset", "graph_id": graph_id}),
            ),
        )
        .await
        .expect("graph_control must respond promptly against a running graph")
        .unwrap();
        // The ground truth is the `graph_runs` table, not graph status (see
        // `Database::reset_graph`'s `InFlight` guard) — the refusal now names
        // the node and run still executing rather than just pointing at
        // `graph_control`.
        assert!(
            reset_result.contains("sleep") && reset_result.contains("still executing"),
            "expected fast-fail error, got: {reset_result}"
        );

        dispatch.abort();
        match prev_home {
            Some(v) => unsafe { std::env::set_var("CANOPY_HOME_OVERRIDE", v) },
            None => unsafe { std::env::remove_var("CANOPY_HOME_OVERRIDE") },
        }
        drop(fake_home);
    }

    /// Regression (b): a quota-shaped implementer failure — modeled here as
    /// the raw CLI message the resilience node would observe — ends with
    /// `autorun_at` set to the *correct* instant, computed deterministically
    /// by the engine rather than by model arithmetic. Exercises the full
    /// `quota_reset_message` path through the real `#[tool]` handler, over
    /// the real HTTP transport.
    #[tokio::test]
    async fn quota_shaped_failure_schedules_correct_autorun_instant() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
        let (graph_id, _spec_id) =
            insert_graph_and_sleeping_node(&db, &dir.path().to_string_lossy());
        db.update_graph_status(
            &graph_id,
            GraphStatus::Failed,
            None,
            Some(chrono::Utc::now()),
        )
        .unwrap();

        let (port, _graph_engine) = spawn_test_server(Arc::clone(&db)).await;
        let mut client = McpClient::new(port);
        client.initialize().await.unwrap();

        let before = chrono::Utc::now();
        let result = client
            .call_tool(
                "graph_control",
                serde_json::json!({
                    "action": "schedule_autorun",
                    "graph_id": graph_id,
                    "quota_reset_message":
                        "You've hit your session limit · resets 1pm (America/Bogota)"
                }),
            )
            .await
            .unwrap();
        let after = chrono::Utc::now();
        assert!(result.contains("scheduled to autorun"), "{result}");

        let persisted = db
            .get_graph(&graph_id)
            .unwrap()
            .unwrap()
            .autorun_at
            .expect("autorun_at must be set");
        // The handler computed `at` from its own `Utc::now()` sometime
        // between `before` and `after`; recomputing against either bound
        // must agree with what got persisted, proving it's the deterministic
        // parser's output (not a model-guessed value).
        let expected_lo = crate::domain::quota_reset::parse_quota_reset_instant(
            "resets 1pm (America/Bogota)",
            before,
        )
        .unwrap();
        let expected_hi = crate::domain::quota_reset::parse_quota_reset_instant(
            "resets 1pm (America/Bogota)",
            after,
        )
        .unwrap();
        assert_eq!(expected_lo.timestamp(), expected_hi.timestamp());
        assert_eq!(persisted.timestamp(), expected_lo.timestamp());
    }

    /// Mutually exclusive params: passing both `at` and `quota_reset_message`
    /// must be rejected up front, not silently prefer one.
    #[tokio::test]
    async fn at_and_quota_reset_message_are_mutually_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
        let (graph_id, _spec_id) =
            insert_graph_and_sleeping_node(&db, &dir.path().to_string_lossy());

        let (port, _graph_engine) = spawn_test_server(Arc::clone(&db)).await;
        let mut client = McpClient::new(port);
        client.initialize().await.unwrap();

        let result = client
            .call_tool(
                "graph_control",
                serde_json::json!({
                    "action": "schedule_autorun",
                    "graph_id": graph_id,
                    "at": chrono::Utc::now().to_rfc3339(),
                    "quota_reset_message": "resets 1pm (America/Bogota)",
                }),
            )
            .await
            .unwrap();
        assert!(result.contains("not both"), "{result}");
        assert!(
            db.get_graph(&graph_id)
                .unwrap()
                .unwrap()
                .autorun_at
                .is_none(),
            "rejected call must not have scheduled anything"
        );
    }

    /// Requirement 4: a retried `graph_control` (simulating a lost
    /// ack whose write already committed) must not double-schedule — the
    /// second call with the same graph id + target instant leaves exactly one
    /// pending schedule, at that instant, not a queued second one.
    #[tokio::test]
    async fn retried_schedule_call_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
        let (graph_id, _spec_id) =
            insert_graph_and_sleeping_node(&db, &dir.path().to_string_lossy());

        let (port, _graph_engine) = spawn_test_server(Arc::clone(&db)).await;
        let mut client = McpClient::new(port);
        client.initialize().await.unwrap();

        let at = chrono::Utc::now() + chrono::Duration::hours(1);
        for _ in 0..2 {
            let result = client
                .call_tool(
                    "graph_control",
                    serde_json::json!({"action": "schedule_autorun", "graph_id": graph_id, "at": at.to_rfc3339()}),
                )
                .await
                .unwrap();
            assert!(result.contains("scheduled to autorun"), "{result}");
        }

        let lp = db.get_graph(&graph_id).unwrap().unwrap();
        assert_eq!(lp.autorun_at.map(|v| v.timestamp()), Some(at.timestamp()));
    }
}

#[cfg(test)]
mod mcp_error_logging_tests {
    use super::*;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use std::sync::{Arc as StdArc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Layer;

    #[derive(Default, Clone)]
    struct CapturedEvents(StdArc<Mutex<Vec<String>>>);

    struct RecordingLayer(CapturedEvents);

    impl<S: tracing::Subscriber> Layer<S> for RecordingLayer {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct Visitor(String);
            impl tracing::field::Visit for Visitor {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    self.0.push_str(&format!("{}={:?} ", field.name(), value));
                }
            }
            let mut visitor = Visitor(String::new());
            event.record(&mut visitor);
            self.0 .0.lock().unwrap().push(visitor.0);
        }
    }

    async fn always_401(_req: axum::extract::Request) -> axum::response::Response {
        (axum::http::StatusCode::UNAUTHORIZED, "nope").into_response()
    }

    async fn always_200(_req: axum::extract::Request) -> axum::response::Response {
        (axum::http::StatusCode::OK, "ok").into_response()
    }

    /// Spawn a router with only `log_mcp_error_responses` wired in front of a
    /// stub handler, and a recording tracing layer as the thread's default
    /// subscriber. `#[tokio::test]` defaults to a current-thread runtime, so
    /// the spawned server task runs on the same thread as the guard and
    /// observes the same subscriber.
    async fn spawn_with_recording_layer(
        handler: axum::routing::MethodRouter,
    ) -> (u16, CapturedEvents, tracing::subscriber::DefaultGuard) {
        let captured = CapturedEvents::default();
        let subscriber = tracing_subscriber::registry().with(RecordingLayer(captured.clone()));
        let guard = tracing::subscriber::set_default(subscriber);

        let router = axum::Router::new()
            .route("/mcp", handler)
            .layer(axum::middleware::from_fn(log_mcp_error_responses));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (port, captured, guard)
    }

    #[tokio::test]
    async fn logs_status_and_session_presence_never_header_value() {
        let (port, captured, _guard) = spawn_with_recording_layer(post(always_401)).await;

        let client = reqwest::Client::new();
        client
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .header("mcp-session-id", "super-secret-session-value")
            .send()
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events = captured.0.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.contains("status") && e.contains("401")),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| e.contains("session_id_present") && e.contains("true")),
            "{events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| e.contains("super-secret-session-value")),
            "header value must never be logged: {events:?}"
        );
    }

    #[tokio::test]
    async fn logs_session_absent_when_no_header_sent() {
        let (port, captured, _guard) = spawn_with_recording_layer(post(always_401)).await;

        let client = reqwest::Client::new();
        client
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .send()
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events = captured.0.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.contains("session_id_present") && e.contains("false")),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn does_not_log_for_successful_responses() {
        let (port, captured, _guard) = spawn_with_recording_layer(post(always_200)).await;

        let client = reqwest::Client::new();
        client
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .send()
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events = captured.0.lock().unwrap();
        let middleware_events: Vec<_> = events
            .iter()
            .filter(|e| e.contains("status") || e.contains("session_id_present"))
            .collect();
        assert!(
            middleware_events.is_empty(),
            "middleware must not log for 200 responses, got: {middleware_events:?}"
        );
    }
}

#[cfg(test)]
mod stdio_startup_reconciliation_tests {
    use super::*;
    use crate::domain::graphs::{
        Graph, GraphNode, GraphNodeKind, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus,
        GraphStatus,
    };
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Database {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Database::new(&path).expect("create test db")
    }

    fn init_git_repo(path: &std::path::Path) {
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .status()
                .expect("git command failed to run");
            assert!(status.success(), "git {:?} failed", args);
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "user.email", "test@example.com"]);
        std::fs::write(path.join("README.md"), "test").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
    }

    fn git_head(path: &std::path::Path) -> String {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output()
            .expect("git rev-parse failed to run");
        assert!(output.status.success(), "git rev-parse HEAD failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn git_is_clean(path: &std::path::Path) -> bool {
        let output = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(path)
            .output()
            .expect("git status failed to run");
        output.stdout.is_empty()
    }

    /// Seeds a `Running` graph with a dangling `running` node run over a dirty
    /// git worktree — exactly the shape `reconcile_orphaned_graphs` (called
    /// from a real daemon boot) would pause, interrupt, and mark `Interrupted`
    /// without touching git. Returns the graph id, run id, and the workdir
    /// (kept alive for the caller via the returned `TempDir`). `suffix`
    /// distinguishes multiple graphs seeded into the same database (each
    /// gets its own tempdir workdir, so two calls never collide on ids or
    /// worktree).
    fn seed_running_graph_with_dirty_worktree(
        db: &Database,
        suffix: &str,
    ) -> (String, String, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let head = git_head(dir.path());
        std::fs::write(dir.path().join("truncated.rs"), "fn broken(").unwrap();

        let lp = Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: format!("wf-stdio-startup-{suffix}"),
            name: format!("Stdio startup test loop {suffix}"),
            description: None,
            workdir: dir.path().to_string_lossy().to_string(),
            status: GraphStatus::Running,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        };
        let spec = GraphSpec {
            id: format!("spec-stdio-startup-{suffix}"),
            graph_id: Some(lp.id.clone()),
            name: "Spec".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Running,
            started_at: None,
            completed_at: None,
            spec_start_head: Some(head),
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_committed_head: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        };
        let node = GraphNode {
            id: format!("node-stdio-startup-{suffix}"),
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Node".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: chrono::Utc::now(),
        };
        let run = GraphNodeRun {
            id: format!("run-stdio-startup-{suffix}"),
            graph_id: lp.id.clone(),
            spec_id: spec.id.clone(),
            node_id: node.id.clone(),
            status: GraphRunStatus::Running,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        };

        db.insert_graph(&lp).unwrap();
        db.insert_graph_spec(&spec).unwrap();
        db.insert_graph_node(&node).unwrap();
        db.insert_graph_run(&run).unwrap();

        (lp.id, run.id, dir)
    }

    /// The regression test for the 2026-08-03 incident: `run_stdio_server`'s
    /// startup sequence (`stdio_server_startup` — the same DB-touching setup
    /// a `canopy bridge` embedded-stdio fallback runs) must never reconcile a
    /// `Running` graph, even though the daemon's own startup path
    /// (`run_http_server`) calls `reconcile_orphaned_graphs` at the equivalent
    /// point in its own sequence.
    #[tokio::test]
    async fn stdio_startup_never_reconciles_running_graph() {
        let db = Arc::new(test_db());
        let (graph_id, run_id, dir) = seed_running_graph_with_dirty_worktree(&db, "solo");
        let data_dir = tempdir().unwrap();

        let _startup = stdio_server_startup(Arc::clone(&db), data_dir.path()).await;

        let lp_after = db.get_graph(&graph_id).unwrap().unwrap();
        assert_eq!(
            lp_after.status,
            GraphStatus::Running,
            "stdio startup must not pause a graph the daemon still owns"
        );
        let run_after = db.get_graph_run(&run_id).unwrap().unwrap();
        assert_eq!(
            run_after.status,
            GraphRunStatus::Running,
            "stdio startup must not mark the run interrupted"
        );
        assert!(
            !git_is_clean(dir.path()),
            "stdio startup must not touch the worktree's uncommitted changes"
        );
    }

    /// Concurrency invariant, not just the single-graph regression above:
    /// two graphs in two different workdirs are both `Running` when a third
    /// process (a `canopy bridge` embedded-stdio fallback) runs the stdio
    /// server's startup path. Neither graph's status, run, or worktree may
    /// be touched — graph B's presence must not change what happens to
    /// graph A, and vice versa.
    #[tokio::test]
    async fn stdio_startup_leaves_two_concurrent_graphs_in_different_workdirs_untouched() {
        let db = Arc::new(test_db());
        let (graph_a, run_a, dir_a) = seed_running_graph_with_dirty_worktree(&db, "a");
        let (graph_b, run_b, dir_b) = seed_running_graph_with_dirty_worktree(&db, "b");
        let data_dir = tempdir().unwrap();

        let _startup = stdio_server_startup(Arc::clone(&db), data_dir.path()).await;

        for (graph_id, run_id, dir) in [(&graph_a, &run_a, &dir_a), (&graph_b, &run_b, &dir_b)] {
            let lp_after = db.get_graph(graph_id).unwrap().unwrap();
            assert_eq!(
                lp_after.status,
                GraphStatus::Running,
                "stdio startup must not pause graph '{graph_id}' just because another graph is also live"
            );
            let run_after = db.get_graph_run(run_id).unwrap().unwrap();
            assert_eq!(
                run_after.status,
                GraphRunStatus::Running,
                "stdio startup must not mark graph '{graph_id}''s run interrupted"
            );
            assert!(
                !git_is_clean(dir.path()),
                "stdio startup must not touch graph '{graph_id}''s uncommitted changes"
            );
        }
    }

    // ── startup_personal_rag's extracted helpers ────────────────────────

    #[tokio::test]
    async fn prepare_personal_rag_roots_handles_empty_and_configured_roots() {
        let dir = tempdir().unwrap();
        let db = Arc::new(test_db());
        let ingestion = IngestionManager::new(Arc::clone(&db), dir.path().to_path_buf());

        // No roots configured: the snapshot is still refreshed (so TUI
        // counters reset to empty) and there is nothing else to do.
        assert!(!prepare_personal_rag_roots(&ingestion, &[]).await);
        assert_eq!(
            db.get_state("rag_indexed_files").unwrap().as_deref(),
            Some("0")
        );

        // A configured root: created on disk for the scan and the watcher.
        let root = dir.path().join("notes");
        assert!(prepare_personal_rag_roots(&ingestion, std::slice::from_ref(&root)).await);
        assert!(root.is_dir());
    }

    #[tokio::test]
    async fn reload_personal_rag_queue_skips_permanently_failed_files() {
        let dir = tempdir().unwrap();
        let db = Arc::new(test_db());
        let ingestion = IngestionManager::new(Arc::clone(&db), dir.path().to_path_buf());
        db.enqueue_rag_item("notes/bad.md", chrono::Utc::now().timestamp())
            .unwrap();

        let mut failed = std::collections::HashSet::new();
        failed.insert("notes/bad.md".to_string());
        reload_personal_rag_queue(&ingestion, &failed).await;
        assert_eq!(
            ingestion.queue_len().await,
            0,
            "a permanently failed file must not be silently re-queued"
        );

        reload_personal_rag_queue(&ingestion, &std::collections::HashSet::new()).await;
        assert_eq!(
            ingestion.queue_len().await,
            1,
            "a pending item left by a previous session must be reloaded"
        );
    }

    #[tokio::test]
    async fn queue_existing_personal_rag_files_skips_failed_files_and_queues_the_rest() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("notes");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("good.md"), "# hi").unwrap();
        std::fs::write(root.join("bad.md"), "# no").unwrap();
        let db = Arc::new(test_db());
        let ingestion = IngestionManager::new(Arc::clone(&db), dir.path().to_path_buf());

        let mut failed = std::collections::HashSet::new();
        failed.insert(root.join("bad.md").to_string_lossy().to_string());

        queue_existing_personal_rag_files(
            &ingestion,
            dir.path(),
            std::slice::from_ref(&root),
            &failed,
        )
        .await;

        assert_eq!(ingestion.queue_len().await, 1);
        let queued = ingestion.db_pending_queue().unwrap();
        assert!(
            queued.iter().any(|p| p.ends_with("good.md")),
            "an indexable new file must be queued: {queued:?}"
        );
        assert!(
            !queued.iter().any(|p| p.ends_with("bad.md")),
            "a permanently failed file must be skipped: {queued:?}"
        );
    }

    #[tokio::test]
    async fn migrate_personal_rag_model_then_chunking_persists_in_order() {
        use crate::application::ports::StateRepository;
        let dir = tempdir().unwrap();
        let db = Arc::new(test_db());
        let ingestion = IngestionManager::new(Arc::clone(&db), dir.path().to_path_buf());
        db.set_state("rag_last_model", "old-model").unwrap();
        db.set_state("rag_chunking_version", "v1").unwrap();
        let scratch_store = dir.path().join("vectors.lancedb");
        std::fs::create_dir_all(&scratch_store).unwrap();
        std::fs::write(scratch_store.join("marker"), b"stale").unwrap();
        let config = crate::domain::canopy_config::CanopyConfig {
            embeddings_model: "new-model".to_string(),
            ..Default::default()
        };
        migrate_personal_rag_model(&ingestion, &config, &scratch_store).await;
        assert_eq!(
            db.get_state("rag_last_model").unwrap().as_deref(),
            Some("new-model")
        );
        migrate_personal_rag_chunking(&ingestion, &scratch_store).await;
        assert_eq!(
            db.get_state("rag_chunking_version").unwrap().as_deref(),
            Some(super::RAG_CHUNKING_VERSION)
        );
        assert!(
            !scratch_store.exists(),
            "the wipe must delete the scratch LanceDB directory, not a home-dir store"
        );
    }

    /// RAII guard: sets the real `$HOME` env var to `path` for the test body,
    /// restoring the previous value on drop. `VectorStore::default_lancedb_path`
    /// reads `dirs::home_dir()` (real `$HOME`), so a migration test that must
    /// prove the production default path is untouched needs this to avoid
    /// touching the real developer's `~/.canopy`. Safe under `cargo nextest`
    /// (one process per test) but would race under plain `cargo test`.
    struct HomeVar {
        prev: Option<std::ffi::OsString>,
    }

    impl HomeVar {
        fn set(path: &std::path::Path) -> Self {
            let prev = std::env::var_os("HOME");
            unsafe { std::env::set_var("HOME", path) };
            HomeVar { prev }
        }
    }

    impl Drop for HomeVar {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => unsafe { std::env::set_var("HOME", v) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    #[tokio::test]
    async fn personal_rag_migrations_never_touch_the_home_store() {
        use crate::application::ports::StateRepository;
        let home = tempdir().unwrap();
        let _home = HomeVar::set(home.path());

        let home_store = home.path().join(".canopy/rag/vectors.lancedb");
        std::fs::create_dir_all(&home_store).unwrap();
        std::fs::write(home_store.join("marker"), b"live").unwrap();

        let scratch = tempdir().unwrap();
        let scratch_store = scratch.path().join("vectors.lancedb");

        let db = Arc::new(test_db());
        let ingestion = IngestionManager::new(Arc::clone(&db), scratch.path().to_path_buf());
        db.set_state("rag_last_model", "old-model").unwrap();
        db.set_state("rag_chunking_version", "v1").unwrap();
        let config = crate::domain::canopy_config::CanopyConfig {
            embeddings_model: "new-model".to_string(),
            ..Default::default()
        };

        migrate_personal_rag_model(&ingestion, &config, &scratch_store).await;
        migrate_personal_rag_chunking(&ingestion, &scratch_store).await;

        assert!(
            home_store.join("marker").exists(),
            "$HOME/.canopy/rag/vectors.lancedb/marker must survive both migrations"
        );
    }

    #[test]
    fn startup_reconciliation_helpers_cover_empty_branches() {
        use crate::application::ports::StateRepository;
        let dir = tempdir().unwrap();
        let db = test_db();
        reconcile_orphaned_graphs_on_startup(&db, dir.path());
        reconcile_stranded_queue_specs_on_startup(&db);
        let _ = db.get_state("port").unwrap();
    }

    #[tokio::test]
    async fn capture_interrupted_state_covers_success_branches() {
        use crate::application::notification_service::DefaultNotificationService;
        let dir = tempdir().unwrap();
        let db = Arc::new(test_db());
        let notif: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
        let executor = Arc::new(Executor::new(Arc::clone(&db), Arc::clone(&notif)));
        let graph_engine = GraphEngine::new(Arc::clone(&db), Arc::clone(&notif));
        let watcher = WatcherEngine::new(
            Arc::clone(&db),
            Arc::clone(&executor),
            Arc::new(graph_engine),
        );
        let graph_engine2 = GraphEngine::new(Arc::clone(&db), Arc::clone(&notif));
        capture_interrupted_state(&graph_engine2, &watcher).await;
        let _ = dir.path().join("placeholder");
    }
}
