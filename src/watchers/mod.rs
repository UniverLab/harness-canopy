//! File watcher engine using the `notify` crate.
//!
//! Manages filesystem watchers that trigger CLI executions when
//! specified events occur. Watchers survive agent disconnection
//! and are reloaded from `SQLite` on daemon startup.

use anyhow::Result;
use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::daemon::handler_run_exec::find_in_flight_run;
use crate::db::Database;
use crate::domain::graphs::Graph;
use crate::domain::models::{Trigger, WatchEvent};
use crate::executor::Executor;
use crate::graph_engine::GraphEngine;

/// Manages all active file system watchers.
pub struct WatcherEngine {
    db: Arc<Database>,
    #[allow(dead_code)]
    executor: Arc<Executor>,
    /// Graph engine used to launch watch-triggered graphs.
    graph_engine: Arc<GraphEngine>,
    /// Active notify watchers for graphs, keyed by graph ID. Backing
    /// graphs of migrated watch agents are keyed by the agent id (the
    /// backing graph id is the agent id), so the legacy per-agent map is
    /// deleted: there is one watcher map.
    active_graphs: Arc<Mutex<HashMap<String, RecommendedWatcher>>>,
}

/// What a watcher launches when its debounced event fires: the backing
/// graph of a migrated watch agent. The legacy agent fire target
/// (`FireTarget::Agent` / `fire_watch_agent`, which executed the CLI with
/// event context) is deleted: watch agents fire through their backing
/// graphs since the agent-to-graph migration.
#[derive(Clone)]
enum FireTarget {
    Graph {
        graph_id: String,
        db: Arc<Database>,
        graph_engine: Arc<GraphEngine>,
    },
}

impl FireTarget {
    fn id(&self) -> &str {
        match self {
            FireTarget::Graph { graph_id, .. } => graph_id,
        }
    }

    /// Run the target: launch the graph fire-and-forget (skipping graphs
    /// that are already running/paused so an event does not double-launch).
    async fn fire(self, file_path: String, evt_str: String) {
        match self {
            FireTarget::Graph {
                graph_id,
                db,
                graph_engine,
            } => fire_watch_graph(graph_id, &db, graph_engine, &file_path, &evt_str),
        }
    }
}

/// Launch a watch-triggered graph fire-and-forget, skipping graphs that are
/// disabled/expired or already running/paused so an event does not double-launch.
fn fire_watch_graph(
    graph_id: String,
    db: &Database,
    graph_engine: Arc<GraphEngine>,
    file_path: &str,
    evt_str: &str,
) {
    match db.get_graph(&graph_id) {
        Ok(Some(lp)) if lp.enabled && !lp.is_expired() && lp.is_fireable() => {
            // Busy reuses the in-flight run lookup on the backing graph
            // (`find_in_flight_run`): a busy graph records `Missed` and
            // skips the event — running/waiting only, no stale threshold.
            // `record_agent_backing_missed` re-checks the same predicate and
            // records the skip against the backing agent.
            let busy = find_in_flight_run(db, &graph_id).ok().flatten().is_some();
            if busy {
                let _ = db.record_agent_backing_missed(
                    &graph_id,
                    crate::domain::models::TriggerType::Watch,
                );
                trace_watch_graph_already_running(&graph_id);
            } else {
                launch_watch_graph(db, graph_engine, graph_id, evt_str, file_path);
            }
        }
        Ok(Some(_)) => {
            // Overlap suppression on the agent-backed graph: a busy backing
            // graph records `Missed` against its agent and skips the event.
            let _ = db
                .record_agent_backing_missed(&graph_id, crate::domain::models::TriggerType::Watch);
            trace_watch_graph_already_running(&graph_id);
        }
        Ok(None) => trace_watch_graph_missing(&graph_id),
        Err(e) => trace_watch_graph_lookup_failed(&graph_id, &e),
    }
}

/// Log the trigger and launch a fireable watch-triggered graph in the
/// background, carrying the watch graph's `trigger_prompt` as the run's
/// idea (a spec-less graph cannot launch on an empty idea) plus the
/// triggering path/event for legacy `{{FILE_PATH}}`/`{{EVENT_TYPE}}`
/// rendering and the watch block.
fn launch_watch_graph(
    db: &Database,
    graph_engine: Arc<GraphEngine>,
    graph_id: String,
    evt_str: &str,
    file_path: &str,
) {
    tracing::info!(
        "Watch graph '{}' triggered: {} on {}",
        graph_id,
        evt_str,
        file_path
    );
    let idea = db
        .get_graph(&graph_id)
        .ok()
        .flatten()
        .and_then(|lp| lp.trigger_prompt)
        .map(|prompt| prompt.trim().to_string())
        .filter(|prompt| !prompt.is_empty());
    graph_engine.start_background_watch_fire(
        graph_id,
        idea,
        file_path.to_string(),
        evt_str.to_string(),
    );
}

/// Log that the watched graph exists but is not fireable right now.
fn trace_watch_graph_already_running(graph_id: &str) {
    tracing::debug!(
        "Watch graph '{}' event ignored (graph already running/paused)",
        graph_id
    );
}

/// Log that the watched graph no longer exists.
fn trace_watch_graph_missing(graph_id: &str) {
    tracing::warn!("Watch graph '{}' no longer exists", graph_id);
}

/// Log the database failure that kept a watch-triggered graph from launching.
fn trace_watch_graph_lookup_failed(graph_id: &str, error: &impl std::fmt::Display) {
    tracing::error!("Watch graph '{}' lookup failed: {}", graph_id, error);
}

/// Resolved watch target: the actual path to watch and an optional filename filter.
struct WatchTarget {
    path: PathBuf,
    /// When set, only events whose path filename matches this value are processed.
    file_filter: Option<String>,
}

impl WatcherEngine {
    pub fn new(db: Arc<Database>, executor: Arc<Executor>, graph_engine: Arc<GraphEngine>) -> Self {
        Self {
            db,
            executor,
            graph_engine,
            active_graphs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Load and start all enabled watch-triggered graphs from the database.
    /// Watch agents fire through their backing graphs since the
    /// agent-to-graph migration: the legacy agent watcher loader is deleted.
    pub async fn reload_from_db(&self) -> Result<()> {
        self.reload_watch_graphs().await?;
        Ok(())
    }

    /// Start every watch-triggered graph registered in the database, logging
    /// (never propagating) per-graph start failures.
    async fn reload_watch_graphs(&self) -> Result<()> {
        let graphs = self.db.list_watch_graphs()?;
        tracing::info!(
            "Reloading {} watch-triggered graphs from database",
            graphs.len()
        );
        for lp in graphs {
            if !lp.enabled || lp.is_expired() {
                continue;
            }
            if let Err(e) = self.start_graph_watcher(&lp).await {
                tracing::error!("Failed to start watcher for graph '{}': {}", lp.id, e);
            }
        }
        Ok(())
    }

    /// Start watching for a watch-triggered graph. On a matching event the graph
    /// engine launches the graph (unless it is already running).
    pub async fn start_graph_watcher(&self, lp: &Graph) -> Result<()> {
        if !lp.enabled || lp.is_expired() {
            return Ok(());
        }
        let Some(Trigger::Watch {
            path,
            events,
            debounce_seconds,
            recursive,
        }) = lp.trigger.as_ref()
        else {
            return Err(anyhow::anyhow!("Graph '{}' has no Watch trigger", lp.id));
        };

        let target = resolve_watch_target(path);
        let mode = watch_mode(*recursive, target.file_filter.is_some());

        let mut watcher = build_notify_watcher(
            FireTarget::Graph {
                graph_id: lp.id.clone(),
                db: Arc::clone(&self.db),
                graph_engine: Arc::clone(&self.graph_engine),
            },
            events.clone(),
            *debounce_seconds,
            target.file_filter.clone(),
        )?;

        log_watcher_start(&lp.id, path, &target, events, *recursive);

        if !target.path.exists() {
            log_missing_path(&lp.id, path, target.file_filter.is_some());
        }

        watcher.watch(&target.path, mode)?;

        self.active_graphs
            .lock()
            .await
            .insert(lp.id.clone(), watcher);
        Ok(())
    }

    /// Stop a specific watcher by ID. Backing graphs of migrated watch
    /// agents are keyed by the agent id, so this stops the graph watcher.
    pub async fn stop_watcher(&self, id: &str) -> Result<()> {
        self.stop_graph_watcher(id).await
    }

    /// Stop a graph watcher by graph ID.
    pub async fn stop_graph_watcher(&self, id: &str) -> Result<()> {
        if self.active_graphs.lock().await.remove(id).is_some() {
            tracing::info!("Stopped graph watcher '{}'", id);
        }
        Ok(())
    }

    /// Stop all active watchers.
    pub async fn stop_all(&self) {
        let mut active_graphs = self.active_graphs.lock().await;
        let graph_count = active_graphs.len();
        active_graphs.clear();
        tracing::info!("Stopped {} graph watchers", graph_count);
    }

    pub async fn active_count(&self) -> usize {
        self.active_graphs.lock().await.len()
    }

    pub async fn is_active(&self, id: &str) -> bool {
        self.active_graphs.lock().await.contains_key(id)
    }
}

// ── Free functions ────────────────────────────────────────────────────────

/// Determine the actual path to watch and an optional filename filter.
///
/// On macOS, FSEvents works at directory level. For single-file targets we
/// watch the parent directory and filter by filename.
fn resolve_watch_target(path: &str) -> WatchTarget {
    let buf = PathBuf::from(path);
    let is_file_target = buf.is_file()
        || (!buf.exists()
            && buf.extension().is_some()
            && buf.parent().map(|p| p.is_dir()).unwrap_or(false));

    if is_file_target {
        let parent = buf.parent().unwrap_or(&buf).to_path_buf();
        let file_filter = buf.file_name().map(|f| f.to_string_lossy().to_string());
        WatchTarget {
            path: parent,
            file_filter,
        }
    } else {
        WatchTarget {
            path: buf,
            file_filter: None,
        }
    }
}

fn watch_mode(recursive: bool, is_file_filter: bool) -> RecursiveMode {
    if is_file_filter || !recursive {
        RecursiveMode::NonRecursive
    } else {
        RecursiveMode::Recursive
    }
}

/// Map a notify `EventKind` to a `WatchEvent`, returning `None` for irrelevant kinds.
fn map_event_kind(kind: &EventKind, agent_id: &str) -> Option<WatchEvent> {
    match kind {
        EventKind::Create(_) => Some(WatchEvent::Create),
        EventKind::Modify(notify::event::ModifyKind::Name(_)) => Some(WatchEvent::Move),
        EventKind::Modify(_) => Some(WatchEvent::Modify),
        EventKind::Remove(_) => Some(WatchEvent::Delete),
        _ => {
            tracing::debug!("Watcher '{}' ignoring event kind: {:?}", agent_id, kind);
            None
        }
    }
}

/// Returns `true` if the event matches the configured watch events.
fn event_matches(evt: WatchEvent, configured: &[WatchEvent]) -> bool {
    configured.contains(&evt)
        || (evt == WatchEvent::Modify && configured.contains(&WatchEvent::Create))
}

/// Build the notify watcher with the event-handling closure.
fn build_notify_watcher(
    target: FireTarget,
    events: Vec<WatchEvent>,
    debounce_secs: u64,
    file_filter: Option<String>,
) -> Result<RecommendedWatcher> {
    let last_trigger: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let rt = tokio::runtime::Handle::current();

    let watcher = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            handle_notify_event(
                res,
                &events,
                file_filter.as_deref(),
                debounce_secs,
                &last_trigger,
                &target,
                &rt,
            );
        },
        Config::default(),
    )?;
    Ok(watcher)
}

/// Synchronous event handler called by the notify thread.
fn handle_notify_event(
    res: Result<Event, notify::Error>,
    events: &[WatchEvent],
    file_filter: Option<&str>,
    debounce_secs: u64,
    last_trigger: &Arc<Mutex<Option<Instant>>>,
    target: &FireTarget,
    rt: &tokio::runtime::Handle,
) {
    let id = target.id();
    let event = match res {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("Watcher '{}' error: {}", id, e);
            return;
        }
    };

    if let Some(filter) = file_filter {
        let matches = event.paths.iter().any(|p| {
            p.file_name()
                .map(|f| f.to_string_lossy() == filter)
                .unwrap_or(false)
        });
        if !matches {
            return;
        }
    }

    let Some(evt) = map_event_kind(&event.kind, id) else {
        return;
    };
    if !event_matches(evt, events) {
        return;
    }

    let last_trigger = Arc::clone(last_trigger);
    let target = target.clone();
    let file_path = event
        .paths
        .first()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let evt_str = evt.to_string();

    rt.spawn(async move {
        {
            let mut lt = last_trigger.lock().await;
            if lt
                .map(|t| t.elapsed() < Duration::from_secs(debounce_secs))
                .unwrap_or(false)
            {
                return;
            }
            *lt = Some(Instant::now());
        }
        target.fire(file_path, evt_str).await;
    });
}

fn log_watcher_start(
    id: &str,
    original_path: &str,
    target: &WatchTarget,
    events: &[WatchEvent],
    recursive: bool,
) {
    if target.file_filter.is_some() {
        tracing::info!(
            "Started watcher '{}' on file '{}' (via parent dir '{}', events: {:?})",
            id,
            original_path,
            target.path.display(),
            events
        );
    } else {
        tracing::info!(
            "Started watcher '{}' on '{}' (events: {:?}, recursive: {})",
            id,
            original_path,
            events,
            recursive
        );
    }
}

fn log_missing_path(id: &str, path: &str, is_file_filter: bool) {
    if is_file_filter {
        tracing::info!(
            "Watcher '{}': file '{}' does not exist yet, watching parent dir for creation",
            id,
            path
        );
    } else {
        tracing::warn!(
            "Watcher '{}': path '{}' does not exist, watcher will activate when it's created",
            id,
            path
        );
    }
}

#[cfg(test)]
mod tests;
