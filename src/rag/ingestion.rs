#![allow(dead_code)]
//! `IngestionManager` — async queue + background worker for personal RAG indexing.
//!
//! Indexes only `.md`, `.mdx`, and `.pdf` files from the personal RAG root
//! (`~/.canopy/rag/` by default). Uses LanceDB as the vector search backend.

use std::collections::{HashSet, VecDeque};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

use crate::application::ports::StateRepository;
use crate::db::Database;
use crate::rag::chunker::{chunk_semantic, detect_lang, SemanticChunk};
use crate::rag::embedding_client::{model_dimensions, EmbeddingClient};
use crate::rag::vector_store::{VectorChunk, VectorStore};

mod queue;
use queue::*;
mod reconcile;
pub use reconcile::*;
// `indexing` is the chunking/embedding pipeline: its helpers are called only
// from inside it (the pipeline entry points are `impl IngestionManager`
// methods, reachable through the type), so it needs no re-export here.
mod indexing;
mod pdf;
pub use pdf::*;

const QUEUE_MAX: usize = 10_000;
/// How often the background task checks whether the cached embedding client
/// has been idle long enough to unload.
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(30);
/// How often the background task checks whether the configured local model
/// needs to be (re)acquired — proactively, so indexing doesn't wait on the
/// first query to trigger a download.
const MODEL_ACQUISITION_CHECK_INTERVAL: Duration = Duration::from_secs(15);
/// Max number of recorded `"error"` events before a file is given up on
/// permanently, even if the error message doesn't match a known-fatal pattern.
const MAX_RAG_ATTEMPTS: i64 = 3;

/// Returns true when an indexing error is non-transient (retrying will never help).
fn is_permanent_rag_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("overflowed its stack")
        || m.contains("stack overflow")
        || m.contains("invalidcontentstream")
        || m.contains("not a valid pdf")
        || m.contains("no extractable text")
}

struct Queue {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl Queue {
    fn new() -> Self {
        Self {
            order: VecDeque::new(),
            set: HashSet::new(),
        }
    }

    fn len(&self) -> usize {
        self.order.len()
    }

    fn push(&mut self, path: &str) -> bool {
        let key = path.to_owned();
        if self.set.contains(&key) {
            self.order.retain(|k| k != &key);
        }
        if self.order.len() >= QUEUE_MAX {
            return false;
        }
        self.order.push_back(key.clone());
        self.set.insert(key);
        true
    }

    fn pop(&mut self) -> Option<String> {
        let item = self.order.pop_front()?;
        self.set.remove(&item);
        Some(item)
    }
}

/// A lazily-loaded embedding client plus enough bookkeeping to unload it
/// after it has sat idle, without racing an in-flight indexing operation.
struct CachedClient {
    model: String,
    client: Arc<dyn EmbeddingClient>,
    last_used: Instant,
}

pub struct IngestionManager {
    db: Arc<Database>,
    data_dir: PathBuf,
    queue: Arc<Mutex<Queue>>,
    notify: Arc<Notify>,
    _personal_watcher: std::sync::Mutex<Option<RecommendedWatcher>>,
    /// Cached embedding client keyed by model id so we load the ONNX model once.
    /// Dropped after `embeddings_idle_unload_secs` of inactivity (see
    /// `idle_unload_graph`) and reloaded transparently on next use.
    cached_client: Mutex<Option<CachedClient>>,
}

impl IngestionManager {
    pub fn new(db: Arc<Database>, data_dir: PathBuf) -> Self {
        crate::rag::ragignore::ensure_ragignore(&data_dir);
        // `cached_client` always starts empty, so the persisted flag other
        // processes read (see `rag::status`) must agree — otherwise a stale
        // "1" surviving a daemon restart would make `canopy rag report` lie
        // about the model being loaded before anything has queried it.
        let _ = db.set_state(crate::rag::status::RAG_MODEL_LOADED_KEY, "0");

        // A "downloading"/"preparing" acquisition state can only be true
        // while a process is actively driving it — and this fresh process
        // hasn't started anything yet. If either survived from a daemon
        // that died mid-acquisition, it's stale: clear it so
        // `ensure_configured_model_acquired` treats the model as never
        // attempted and restarts it, rather than leaving a permanent
        // "downloading" that nothing is driving. A persisted "failed" is
        // left alone — that's a real outcome, not a restart artifact, and
        // stays until an explicit retry.
        let config = crate::domain::canopy_config::CanopyConfig::load(&data_dir);
        let model_id = config.embeddings_model.trim();
        if !model_id.is_empty() {
            if let Some(
                crate::rag::status::AcquisitionState::Downloading { .. }
                | crate::rag::status::AcquisitionState::Preparing { .. },
            ) = crate::rag::status::read_acquisition_state(&db, model_id)
            {
                tracing::warn!(
                    "RAG: clearing stale in-progress acquisition state for '{model_id}' \
                     left over from a previous daemon run"
                );
                crate::rag::status::clear_acquisition(&db, model_id);
            }
        }

        Self {
            db,
            data_dir,
            queue: Arc::new(Mutex::new(Queue::new())),
            notify: Arc::new(Notify::new()),
            _personal_watcher: std::sync::Mutex::new(None),
            cached_client: Mutex::new(None),
        }
    }

    pub async fn enqueue(&self, source_path: &str) -> bool {
        let mut q = self.queue.lock().await;
        let ok = q.push(source_path);
        if ok {
            let now = chrono::Utc::now().timestamp();
            if let Err(e) = self.db.enqueue_rag_item(source_path, now) {
                tracing::warn!("RAG queue state error {source_path}: {e}");
            }
            self.notify.notify_one();
        }
        ok
    }

    pub async fn queue_len(&self) -> usize {
        self.queue.lock().await.len()
    }

    /// Clear both the in-memory queue and the DB queue (used on model change).
    pub async fn clear_queue(&self) {
        let mut q = self.queue.lock().await;
        q.order.clear();
        q.set.clear();
        if let Err(e) = self.db.clear_rag_queue() {
            tracing::warn!("RAG: failed to clear DB queue: {e}");
        }
    }

    /// Expose the underlying database for state queries (e.g. model change checks).
    pub(crate) fn db(&self) -> &Arc<Database> {
        &self.db
    }

    pub fn db_pending_queue(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .db
            .list_rag_queue(10_000)?
            .into_iter()
            .map(|i| i.source_path)
            .collect())
    }

    pub async fn refresh_snapshot(&self) {
        refresh_rag_snapshot(&self.db, &self.data_dir).await;
    }

    pub fn start_personal_watcher(self: Arc<Self>, personal_roots: &[PathBuf]) {
        if personal_roots.is_empty() {
            return;
        }

        let rt = tokio::runtime::Handle::current();
        let db = Arc::clone(&self.db);
        let queue = Arc::clone(&self.queue);
        let notify_handle = Arc::clone(&self.notify);
        let roots = personal_roots.to_vec();
        let data_dir = self.data_dir.clone();

        let patterns = crate::rag::ragignore::load_patterns(&data_dir);

        let ctx = RagWatchContext {
            rt,
            db,
            queue,
            notify: notify_handle,
            roots,
            patterns,
            data_dir,
        };

        let mut watcher = match RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                let Ok(event) = res else { return };
                handle_rag_watcher_event(&ctx, event);
            },
            Config::default(),
        ) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("Personal RAG watcher creation failed: {e}");
                return;
            }
        };

        let watched = watch_personal_roots(&mut watcher, personal_roots);

        if watched > 0 {
            if let Ok(mut guard) = self._personal_watcher.lock() {
                *guard = Some(watcher);
            }
        }
    }

    pub fn start(self: Arc<Self>) -> tokio_util::sync::CancellationToken {
        let ct = tokio_util::sync::CancellationToken::new();

        let ct_run = ct.child_token();
        let mgr_run = Arc::clone(&self);
        tokio::spawn(async move {
            mgr_run.run(ct_run).await;
        });

        let ct_idle = ct.child_token();
        let mgr_idle = Arc::clone(&self);
        tokio::spawn(async move {
            mgr_idle.idle_unload_graph(ct_idle).await;
        });

        let ct_acquire = ct.child_token();
        let mgr_acquire = Arc::clone(&self);
        tokio::spawn(async move {
            mgr_acquire.model_acquisition_graph(ct_acquire).await;
        });

        ct
    }

    /// Proactively ensures the configured local embedding model is
    /// available, so indexing can proceed without waiting on the first
    /// query to trigger acquisition (FR1/FR2: setup returns immediately and
    /// the download happens in the background, owned by the daemon). Runs
    /// once right away and then on a slow poll — the poll (rather than a
    /// one-shot at startup) is what picks up a model chosen by a `canopy
    /// setup` run against an already-running daemon.
    async fn model_acquisition_graph(&self, ct: tokio_util::sync::CancellationToken) {
        self.ensure_configured_model_acquired().await;
        loop {
            tokio::select! {
                _ = ct.cancelled() => break,
                _ = tokio::time::sleep(MODEL_ACQUISITION_CHECK_INTERVAL) => {
                    self.ensure_configured_model_acquired().await;
                }
            }
        }
    }

    /// If the configured embeddings model is a local one this build can
    /// serve, isn't already cached, and isn't already tracked as
    /// downloading/preparing/failed, acquires it — reusing
    /// `get_embedding_client` so the resulting client is cached for the
    /// next query or indexing pass instead of being built twice.
    ///
    /// Only runs when the indexing queue is non-empty: a daemon with
    /// nothing queued has no imminent need for the model, and eagerly
    /// acquiring it here would silently reintroduce the startup load this
    /// loop's own scope excludes (C8) — the model is loaded lazily, from
    /// whichever of a query or an indexing pass asks for it first.
    async fn ensure_configured_model_acquired(&self) {
        if self.queue_len().await == 0 {
            return;
        }
        let config = crate::domain::canopy_config::CanopyConfig::load(&self.data_dir);
        let model_id = config.embeddings_model.trim();
        if model_id.is_empty() {
            return;
        }
        if crate::rag::embedding_client::provider_for_model(model_id)
            != Some(crate::rag::embedding_client::EmbeddingProvider::Local)
        {
            return; // Cloud clients build instantly from an API key — nothing to acquire proactively.
        }
        if !crate::rag::embedding_client::provider_available(
            crate::rag::embedding_client::EmbeddingProvider::Local,
        ) {
            return; // Doctor/setup already surface this capability gap; nothing to acquire.
        }
        if crate::rag::status::read_acquisition_state(&self.db, model_id).is_some() {
            // Already in flight (this tick or a concurrent one), or failed
            // and awaiting an explicit `canopy rag model retry` — either
            // way, not this loop's job right now.
            return;
        }

        if let Err(e) = self.get_embedding_client(&config).await {
            tracing::warn!("RAG: proactive model acquisition for '{model_id}' failed: {e:#}");
        }
    }

    /// Periodically checks whether the cached embedding client has been idle
    /// long enough to drop, freeing the model's RAM until it's needed again.
    async fn idle_unload_graph(&self, ct: tokio_util::sync::CancellationToken) {
        loop {
            tokio::select! {
                _ = ct.cancelled() => break,
                _ = tokio::time::sleep(IDLE_CHECK_INTERVAL) => {
                    let idle_timeout = std::time::Duration::from_secs(
                        crate::domain::canopy_config::CanopyConfig::load(&self.data_dir)
                            .embeddings_idle_unload_secs,
                    );
                    self.maybe_unload_idle_client(idle_timeout).await;
                }
            }
        }
    }

    /// Drops the cached embedding client if it has been idle for at least
    /// `idle_timeout` and nothing else currently holds a reference to it.
    async fn maybe_unload_idle_client(&self, idle_timeout: Duration) {
        if let Some((model, idle_for)) = self
            .maybe_unload_idle_client_at(idle_timeout, Instant::now())
            .await
        {
            tracing::info!(
                "RAG: unloaded embedding client for model '{model}' after {:.0}s idle",
                idle_for.as_secs_f64()
            );
        }
    }

    /// Testable core of `maybe_unload_idle_client`: takes `now` explicitly so
    /// tests can simulate an idle timeout without sleeping in real time.
    /// Never unloads while another clone of the `Arc` is still in flight
    /// (e.g. mid-indexing) — that's read straight off the strong count, not
    /// a separately-tracked "busy" flag, so it can't drift out of sync.
    /// Returns the unloaded model id plus how long it actually sat idle.
    async fn maybe_unload_idle_client_at(
        &self,
        idle_timeout: Duration,
        now: Instant,
    ) -> Option<(String, Duration)> {
        if idle_timeout.is_zero() {
            return None;
        }

        let mut guard = self.cached_client.lock().await;
        let cached = guard.as_ref()?;

        let idle_for = now.saturating_duration_since(cached.last_used);
        if idle_for < idle_timeout {
            return None;
        }
        if Arc::strong_count(&cached.client) > 1 {
            return None;
        }

        let model = cached.model.clone();
        *guard = None;
        let _ = self
            .db
            .set_state(crate::rag::status::RAG_MODEL_LOADED_KEY, "0");
        Some((model, idle_for))
    }

    /// Items enqueued directly into the DB (e.g. by `canopy rag backfill`
    /// while the daemon is already running) bypass the in-memory queue, so
    /// poll the DB periodically and reload anything pending. Idempotent:
    /// [`Queue::push`] skips paths already held in memory.
    async fn reload_queue_from_db(&self) {
        let failed = self.db.permanently_failed_rag_files().unwrap_or_default();
        let pending = match self.db_pending_queue() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("rag_backfill: failed to read pending DB queue: {e:#}");
                return;
            }
        };
        let mut reloaded = 0usize;
        for path in &pending {
            if failed.contains(path) {
                continue;
            }
            if self.enqueue(path).await {
                reloaded += 1;
            }
        }
        if reloaded > 0 {
            tracing::info!("rag_backfill: reloaded {reloaded} pending queue item(s) from DB");
            self.notify.notify_one();
        }
    }

    async fn run(&self, ct: tokio_util::sync::CancellationToken) {
        let mut db_poll = tokio::time::interval(std::time::Duration::from_secs(60));
        db_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = ct.cancelled() => break,
                _ = self.notify.notified() => {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    self.drain_queue(&ct).await;
                }
                _ = db_poll.tick() => {
                    // Pick up items added by `canopy rag backfill` or other CLI commands
                    self.reload_queue_from_db().await;
                    self.drain_queue(&ct).await;
                }
            }
        }
    }

    fn is_paused(&self) -> bool {
        self.db.get_state("rag_paused").ok().flatten().as_deref() == Some("1")
    }

    async fn drain_queue(&self, ct: &tokio_util::sync::CancellationToken) {
        let initial_count = self.queue.lock().await.len();
        if initial_count > 0 {
            crate::domain::notification::send_notification(
                "RAG indexing",
                &format!("{initial_count} file(s) pending"),
                crate::domain::notification::NotificationLevel::Info,
            );
        }

        let mut processed = 0usize;
        let mut skipped = 0usize;
        let mut indexed_paths: Vec<String> = Vec::with_capacity(initial_count);

        loop {
            if !self.wait_while_paused(ct).await {
                return;
            }

            let Some(source_path) = self.queue.lock().await.pop() else {
                break;
            };

            let now = chrono::Utc::now().timestamp();
            let _ = self.db.mark_rag_item_processing(&source_path, now);
            tracing::info!("RAG drain_queue: indexing '{}'", source_path);

            match self.index_file(&source_path).await {
                Ok(()) => {
                    processed += 1;
                    indexed_paths.push(source_path.clone());
                    let _ = self.db.remove_rag_item(&source_path);
                    let _ = self.db.log_rag_event(
                        &source_path,
                        "indexed",
                        None,
                        chrono::Utc::now().timestamp(),
                    );
                }
                Err(e) => {
                    tracing::error!("RAG index error {source_path}: {e:#}");
                    skipped += 1;
                    let _ = self.db.remove_rag_item(&source_path);
                    let error_detail = format!("{e:#}");
                    self.record_queued_file_failure(&source_path, &error_detail);
                }
            }
        }

        Self::report_drain_queue_outcome(processed, skipped, &indexed_paths);
    }

    /// Emit the end-of-drain summary: the indexed count (with its directory
    /// annotation and success notification) and the failure warning.
    fn report_drain_queue_outcome(processed: usize, skipped: usize, indexed_paths: &[String]) {
        if processed > 0 {
            let dir_note = indexing_dir_summary(indexed_paths);
            tracing::info!("Personal RAG: indexed {processed} file(s){dir_note}");
            crate::domain::notification::send_notification(
                "RAG indexing",
                &format!("{processed} file(s) indexed{dir_note}"),
                crate::domain::notification::NotificationLevel::Success,
            );
        }
        if skipped > 0 {
            tracing::warn!("Personal RAG: {skipped} file(s) failed — check logs above");
        }
    }

    /// Record the `"error"` event for a failed indexing attempt and notify
    /// the user — escalating to a permanent give-up (a `"failed"` event and
    /// the permanent-failure notification) once
    /// [`permanent_index_failure`] says no further retry can help.
    fn record_queued_file_failure(&self, source_path: &str, error_detail: &str) {
        let now = chrono::Utc::now().timestamp();
        let _ = self
            .db
            .log_rag_event(source_path, "error", Some(error_detail), now);

        let prior_errors = self.db.rag_error_count(source_path).unwrap_or(0);
        let attempt = prior_errors + 1;
        let permanent = permanent_index_failure(error_detail, prior_errors);

        let filename = std::path::Path::new(source_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| source_path.to_string());

        if permanent {
            let _ = self
                .db
                .log_rag_event(source_path, "failed", Some(error_detail), now);
            tracing::warn!(
                "RAG index permanent failure {source_path}: giving up after {attempt} attempt(s)"
            );
            crate::domain::notification::send_notification(
                "RAG indexing",
                &format!(
                    "{filename} permanently failed after {attempt} attempt(s) — giving up\nCause: {error_detail}"
                ),
                crate::domain::notification::NotificationLevel::Error,
            );
        } else {
            // Immediate per-file error notification so the user knows right away.
            crate::domain::notification::send_notification(
                "RAG indexing",
                &format!("{filename} could not be indexed\nCause: {error_detail}"),
                crate::domain::notification::NotificationLevel::Error,
            );
        }
    }

    async fn wait_while_paused(&self, ct: &tokio_util::sync::CancellationToken) -> bool {
        while self.is_paused() {
            tokio::select! {
                _ = ct.cancelled() => return false,
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            }
        }
        !ct.is_cancelled()
    }

    /// Return a cached embedding client, creating it (in a blocking task) if needed.
    /// If the configured model changed since last call, the client is recreated.
    /// This is the sole load point: the model is never loaded eagerly at
    /// startup, only here, on the first query or indexing pass that needs it.
    /// `pub(crate)` so `rag_search` shares the same cache (B22) instead of
    /// building throwaway clients that dodge the model-loaded status.
    ///
    /// For a local model still being downloaded/prepared, this fails fast
    /// with a message naming the state instead of blocking on the
    /// (possibly minutes-long) acquisition — callers never silently hang,
    /// and never race the background acquisition loop into a duplicate
    /// attempt by both trying to build the client at once.
    pub(crate) async fn get_embedding_client(
        &self,
        config: &crate::domain::canopy_config::CanopyConfig,
    ) -> anyhow::Result<Arc<dyn EmbeddingClient>> {
        let model_id = config.embeddings_model.trim().to_string();

        if let Some(state) = crate::rag::status::read_acquisition_state(&self.db, &model_id) {
            anyhow::bail!(crate::rag::status::acquisition_message(&model_id, &state));
        }

        // Claim any needed download before touching the client-cache lock
        // below (see the function's own doc comment for why this ordering
        // matters): a caller racing us now sees the state via the check
        // above and bails immediately, instead of blocking on that lock for
        // the whole download that follows.
        crate::rag::model_acquisition::claim_local_download_if_needed(&self.db, &model_id);

        let config_clone = config.clone();
        let db_clone = Arc::clone(&self.db);
        self.get_or_load_client(model_id, move || async move {
            // Load the model (potentially heavy for local ONNX models) off the async executor.
            tokio::task::spawn_blocking(move || {
                crate::rag::model_acquisition::client_from_config_for_ingestion(
                    &config_clone,
                    &db_clone,
                )
                .map(Arc::from)
            })
            .await
            .map_err(|e| anyhow::anyhow!("Embedding client task panicked: {e}"))?
        })
        .await
    }

    /// Cache-lookup-or-load core shared by `get_embedding_client` and tests: returns
    /// the cached client for `model_id` if present and refreshes its idle timer,
    /// otherwise runs `loader` and caches the result. Kept generic over `loader` so
    /// tests can exercise the caching/idle-tracking behavior with a lightweight mock
    /// client instead of a real (network- or ONNX-backed) `EmbeddingClient`.
    async fn get_or_load_client<F, Fut>(
        &self,
        model_id: String,
        loader: F,
    ) -> anyhow::Result<Arc<dyn EmbeddingClient>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<Arc<dyn EmbeddingClient>>>,
    {
        let mut guard = self.cached_client.lock().await;

        if let Some(cached) = guard.as_mut() {
            if cached.model == model_id {
                cached.last_used = Instant::now();
                return Ok(Arc::clone(&cached.client));
            }
        }

        tracing::info!("RAG: loading embedding client for model '{model_id}'");
        let client = loader().await?;

        *guard = Some(CachedClient {
            model: model_id.clone(),
            client: Arc::clone(&client),
            last_used: Instant::now(),
        });
        tracing::info!("RAG: embedding client loaded and cached for model '{model_id}'");

        let _ = self
            .db
            .set_state(crate::rag::status::RAG_MODEL_LOADED_KEY, "1");
        let _ = self
            .db
            .set_state(crate::rag::status::RAG_MODEL_NAME_KEY, &model_id);
        let _ = self.db.set_state(
            crate::rag::status::RAG_MODEL_SINCE_KEY,
            &chrono::Utc::now().timestamp().to_string(),
        );

        Ok(client)
    }
}

/// WARN about a file dropped for exceeding the indexing limit and record the
/// `"skipped_oversize"` ledger event for it.
pub(super) fn warn_and_record_oversize(
    db: &Database,
    source_path: &str,
    size_bytes: u64,
    max_bytes: u64,
) {
    let size_mb = size_bytes as f64 / (1024.0 * 1024.0);
    let cap_mb = max_bytes as f64 / (1024.0 * 1024.0);
    // WARN, not DEBUG/INFO: a file dropped from the index is a fact the
    // operator must be able to see — the same visibility `canopy doctor`
    // and `canopy rag report` now give the aggregate count (CB20).
    tracing::warn!(
        "Personal RAG: skipping '{source_path}' — {size_mb:.1} MB exceeds the \
         {cap_mb:.0} MB indexing limit (config.toml: rag_max_file_mb)"
    );
    record_oversize_skip(db, source_path, size_bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_permanent_rag_error_detects_known_fatal_patterns() {
        assert!(is_permanent_rag_error(
            "thread 'main' has overflowed its stack"
        ));
        assert!(is_permanent_rag_error("Stack overflow detected"));
        assert!(is_permanent_rag_error(
            "PDF text extraction failed for foo.pdf: InvalidContentStream"
        ));
        assert!(is_permanent_rag_error(
            "Could not extract text from 'foo.pdf': not a valid PDF or recognisable text file"
        ));
        assert!(is_permanent_rag_error(
            "File 'foo.html' appears to be HTML but contains no extractable text"
        ));
    }

    #[test]
    fn is_permanent_rag_error_allows_transient_messages() {
        assert!(!is_permanent_rag_error("connection reset by peer"));
        assert!(!is_permanent_rag_error("embedding request timed out"));
        assert!(!is_permanent_rag_error(
            "Failed to launch PDF extraction subprocess (/usr/bin/canopy): No such file or directory"
        ));
    }

    /// Mirrors the permanence decision made in `drain_queue`'s `Err` branch:
    /// a file is given up on once it has accumulated `MAX_RAG_ATTEMPTS`
    /// total attempts (prior errors + the current one), even for otherwise
    /// generic/transient-looking error messages.
    fn is_permanent(error_detail: &str, prior_errors: i64) -> bool {
        let attempt = prior_errors + 1;
        is_permanent_rag_error(error_detail) || attempt >= MAX_RAG_ATTEMPTS
    }

    #[test]
    fn permanence_decision_respects_attempt_threshold() {
        let transient = "embedding request timed out";
        assert!(!is_permanent(transient, 0)); // attempt 1
        assert!(!is_permanent(transient, 1)); // attempt 2
        assert!(is_permanent(transient, 2)); // attempt 3 == MAX_RAG_ATTEMPTS
        assert!(is_permanent(transient, 5)); // well past the threshold
    }

    #[test]
    fn permanence_decision_short_circuits_on_fatal_message() {
        // A known-fatal message is permanent immediately, regardless of attempt count.
        assert!(is_permanent("document overflowed its stack", 0));
    }

    #[test]
    fn lancedb_purge_flag_prevents_double_purge() {
        // Reset the flag so this test is deterministic.
        LANCEDB_PURGE_DONE.store(false, Ordering::SeqCst);

        // First swap should return false (not yet purged) and set the flag.
        assert!(
            !LANCEDB_PURGE_DONE.swap(true, Ordering::SeqCst),
            "first purge check should return false"
        );

        // Second swap should return true (already purged) — no second purge.
        assert!(
            LANCEDB_PURGE_DONE.swap(true, Ordering::SeqCst),
            "second purge check should return true (already purged)"
        );

        // Reset for other tests.
        LANCEDB_PURGE_DONE.store(false, Ordering::SeqCst);
    }

    use crate::rag::embedding_client::MockEmbeddingClient;

    fn test_manager() -> (IngestionManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
        let mgr = IngestionManager::new(db, dir.path().to_path_buf());
        (mgr, dir)
    }

    fn mock_client() -> Arc<dyn EmbeddingClient> {
        Arc::new(MockEmbeddingClient::new(4))
    }

    /// (1) After startup with an empty queue, the embedding client is not loaded.
    #[tokio::test]
    async fn embedding_client_not_loaded_on_startup() {
        let (mgr, _dir) = test_manager();
        assert!(mgr.cached_client.lock().await.is_none());
    }

    /// (1b) A local model being *configured* isn't enough to load it: the
    /// proactive acquisition path (`ensure_configured_model_acquired`, run
    /// by `model_acquisition_graph` right after `start()`) must also see an
    /// empty queue and skip, or a daemon that never indexes anything would
    /// still eagerly load the model — reintroducing the exact startup load
    /// this spec removes, just from a different call site than
    /// `startup_personal_rag`.
    #[tokio::test]
    async fn configured_local_model_not_acquired_with_empty_queue() {
        let (mgr, dir) = test_manager();
        let mut config = crate::domain::canopy_config::CanopyConfig::load(dir.path());
        config.embeddings_model = "baai/bge-small-en-v1.5".to_string();
        config.save(dir.path()).unwrap();

        assert_eq!(mgr.queue_len().await, 0);
        mgr.ensure_configured_model_acquired().await;

        assert!(mgr.cached_client.lock().await.is_none());
    }

    /// (2) A query (via `get_or_load_client`, the shared core behind
    /// `get_embedding_client`) loads and caches the client on first use.
    #[tokio::test]
    async fn query_loads_embedding_client() {
        let (mgr, _dir) = test_manager();
        assert!(mgr.cached_client.lock().await.is_none());

        let client = mgr
            .get_or_load_client("mock-model".to_string(), || async { Ok(mock_client()) })
            .await
            .expect("load should succeed");
        assert_eq!(client.embed("hello").unwrap().len(), 4);

        let guard = mgr.cached_client.lock().await;
        let cached = guard.as_ref().expect("client should now be cached");
        assert_eq!(cached.model, "mock-model");
        drop(guard);

        assert!(crate::rag::status::is_model_loaded(&mgr.db));
        assert!(crate::rag::status::model_loaded_since(&mgr.db).is_some());
    }

    /// A fresh manager persists "not loaded" so a stale flag left over from a
    /// previous daemon run (the DB file survives restarts) can't make
    /// `canopy rag report` claim the model is warm before anything has used it.
    #[tokio::test]
    async fn new_manager_persists_model_not_loaded() {
        let (mgr, _dir) = test_manager();
        assert!(!crate::rag::status::is_model_loaded(&mgr.db));
    }

    /// (3) Simulated idle timeout releases the cached client.
    #[tokio::test]
    async fn idle_timeout_unloads_cached_client() {
        let (mgr, _dir) = test_manager();
        mgr.get_or_load_client("mock-model".to_string(), || async { Ok(mock_client()) })
            .await
            .unwrap();
        assert!(mgr.cached_client.lock().await.is_some());

        let idle_timeout = Duration::from_secs(600);
        let long_after = Instant::now() + Duration::from_secs(700);
        let unloaded = mgr
            .maybe_unload_idle_client_at(idle_timeout, long_after)
            .await;

        assert_eq!(
            unloaded.map(|(model, _)| model),
            Some("mock-model".to_string())
        );
        assert!(mgr.cached_client.lock().await.is_none());
        assert!(!crate::rag::status::is_model_loaded(&mgr.db));
    }

    /// (3b) A request arriving after an idle release reloads transparently
    /// and produces the same results as before the release (decision 5:
    /// "reloading must be correct").
    #[tokio::test]
    async fn request_after_idle_unload_reloads_with_identical_results() {
        let (mgr, _dir) = test_manager();
        let before = mgr
            .get_or_load_client("mock-model".to_string(), || async { Ok(mock_client()) })
            .await
            .unwrap()
            .embed("hello world")
            .unwrap();

        let idle_timeout = Duration::from_secs(600);
        let long_after = Instant::now() + Duration::from_secs(700);
        assert!(mgr
            .maybe_unload_idle_client_at(idle_timeout, long_after)
            .await
            .is_some());
        assert!(mgr.cached_client.lock().await.is_none());

        let after = mgr
            .get_or_load_client("mock-model".to_string(), || async { Ok(mock_client()) })
            .await
            .unwrap()
            .embed("hello world")
            .unwrap();

        assert_eq!(before, after);
        assert!(mgr.cached_client.lock().await.is_some());
    }

    /// (4) The client is not released while an in-progress use still holds the Arc,
    /// even past the idle timeout — checked via the strong count, not a busy flag.
    #[tokio::test]
    async fn idle_unload_skipped_while_client_in_use() {
        let (mgr, _dir) = test_manager();
        // Simulates an in-flight indexing/query call still holding the Arc it
        // got back from `get_or_load_client` (the cache itself holds a second
        // strong reference, so the count is 2 while `held` is alive).
        let held = mgr
            .get_or_load_client("mock-model".to_string(), || async { Ok(mock_client()) })
            .await
            .unwrap();

        let idle_timeout = Duration::from_secs(600);
        let long_after = Instant::now() + Duration::from_secs(700);
        let unloaded = mgr
            .maybe_unload_idle_client_at(idle_timeout, long_after)
            .await;

        assert!(
            unloaded.is_none(),
            "must not unload while a use is in flight"
        );
        assert!(mgr.cached_client.lock().await.is_some());

        drop(held);
        let unloaded = mgr
            .maybe_unload_idle_client_at(idle_timeout, long_after)
            .await;
        assert!(
            unloaded.is_some(),
            "should unload once the in-flight use ends"
        );
        assert!(mgr.cached_client.lock().await.is_none());
    }

    /// (5) Concurrent first-requests for the same model must not each start
    /// their own load: `get_or_load_client` holds `cached_client`'s lock
    /// across the whole check-then-load, so a second caller arriving while
    /// the first is still loading simply blocks on the mutex and then finds
    /// the cache already populated, instead of racing its own loader.
    #[tokio::test]
    async fn concurrent_first_requests_produce_one_load() {
        let (mgr, _dir) = test_manager();
        let mgr = Arc::new(mgr);
        let load_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let handles: Vec<_> = (0..5)
            .map(|_| {
                let mgr = Arc::clone(&mgr);
                let load_count = Arc::clone(&load_count);
                tokio::spawn(async move {
                    mgr.get_or_load_client("mock-model".to_string(), || async move {
                        load_count.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        Ok(mock_client())
                    })
                    .await
                })
            })
            .collect();

        let mut clients = Vec::new();
        for h in handles {
            clients.push(h.await.unwrap().expect("load should succeed"));
        }

        assert_eq!(
            load_count.load(Ordering::SeqCst),
            1,
            "loader must run exactly once across concurrent first-requests"
        );
        for client in &clients[1..] {
            assert!(
                Arc::ptr_eq(&clients[0], client),
                "every caller should receive the same cached client"
            );
        }
    }

    #[test]
    fn store_loss_detected_when_ledger_has_files_but_store_is_empty() {
        assert!(store_loss_detected(310, Some(0)));
    }

    #[test]
    fn store_loss_not_detected_when_ledger_and_store_agree() {
        // Consistent state: ledger has entries and the store has chunks too.
        assert!(!store_loss_detected(310, Some(1200)));
    }

    #[test]
    fn store_loss_not_detected_when_ledger_is_empty() {
        // Nothing indexed yet — an empty store is expected, not a loss.
        assert!(!store_loss_detected(0, Some(0)));
    }

    #[test]
    fn store_loss_not_detected_on_transient_open_failure() {
        // A4 protection: a store that failed to open (`None`) must never be
        // conflated with "the store is empty" — that would wipe a healthy
        // ledger on a transient error.
        assert!(!store_loss_detected(310, None));
    }

    /// Ledger reports indexed files but the store comes back with zero
    /// chunks (store loss) → the ledger and pending queue are purged so the
    /// caller's directory scan requeues everything for a full re-index.
    #[tokio::test]
    async fn reconcile_purges_ledger_and_queue_when_store_loss_detected() {
        let (mgr, _dir) = test_manager();
        mgr.db()
            .log_rag_event(
                "/docs/a.md",
                "indexed",
                None,
                chrono::Utc::now().timestamp(),
            )
            .unwrap();
        mgr.db()
            .log_rag_event(
                "/docs/b.md",
                "indexed",
                None,
                chrono::Utc::now().timestamp(),
            )
            .unwrap();
        mgr.enqueue("/docs/stale.md").await;
        assert_eq!(mgr.db().indexed_files_timestamps().unwrap().len(), 2);

        // A scratch path standing in for the (destroyed/empty) LanceDB
        // directory — never the real home-dir-rooted store.
        let scratch_lancedb = tempfile::tempdir().unwrap();
        let handled = mgr
            .reconcile_ledger_with_store_chunk_count_at(Some(0), scratch_lancedb.path())
            .await;

        assert!(handled, "store loss should have been detected and handled");
        assert!(mgr.db().indexed_files_timestamps().unwrap().is_empty());
        assert!(mgr.db_pending_queue().unwrap().is_empty());
        assert_eq!(mgr.queue_len().await, 0);
    }

    /// Ledger and store agree (store has chunks) → nothing is purged and
    /// nothing extra is queued.
    #[tokio::test]
    async fn reconcile_leaves_healthy_state_untouched() {
        let (mgr, _dir) = test_manager();
        mgr.db()
            .log_rag_event(
                "/docs/a.md",
                "indexed",
                None,
                chrono::Utc::now().timestamp(),
            )
            .unwrap();
        mgr.enqueue("/docs/pending.md").await;

        let scratch_lancedb = tempfile::tempdir().unwrap();
        let handled = mgr
            .reconcile_ledger_with_store_chunk_count_at(Some(5), scratch_lancedb.path())
            .await;

        assert!(!handled, "consistent state must not trigger reconciliation");
        assert_eq!(mgr.db().indexed_files_timestamps().unwrap().len(), 1);
        assert_eq!(mgr.db_pending_queue().unwrap(), vec!["/docs/pending.md"]);
        assert_eq!(mgr.queue_len().await, 1);
    }

    /// A file over the configured indexing limit records a
    /// `"skipped_oversize"` ledger event carrying its size, instead of
    /// vanishing silently.
    #[tokio::test]
    async fn record_oversize_skip_logs_event_with_size() {
        let (mgr, _dir) = test_manager();
        record_oversize_skip(mgr.db(), "/docs/huge.pdf", 6_000_000);

        let events = mgr.db().rag_events_for_file("/docs/huge.pdf").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "skipped_oversize");
        assert_eq!(events[0].detail.as_deref(), Some("6000000"));
    }

    /// Recording the same oversize skip again (e.g. on the next startup scan,
    /// file unchanged) must not duplicate the ledger row.
    #[tokio::test]
    async fn record_oversize_skip_is_idempotent_for_unchanged_size() {
        let (mgr, _dir) = test_manager();
        record_oversize_skip(mgr.db(), "/docs/huge.pdf", 6_000_000);
        record_oversize_skip(mgr.db(), "/docs/huge.pdf", 6_000_000);
        record_oversize_skip(mgr.db(), "/docs/huge.pdf", 6_000_000);

        let events = mgr.db().rag_events_for_file("/docs/huge.pdf").unwrap();
        assert_eq!(
            events.len(),
            1,
            "unchanged oversize file must not spam events"
        );
    }

    /// If the file's size changes (e.g. grew further), a new event is
    /// recorded so the ledger reflects the current size.
    #[tokio::test]
    async fn record_oversize_skip_records_new_event_when_size_changes() {
        let (mgr, _dir) = test_manager();
        record_oversize_skip(mgr.db(), "/docs/huge.pdf", 6_000_000);
        record_oversize_skip(mgr.db(), "/docs/huge.pdf", 7_000_000);

        let events = mgr.db().rag_events_for_file("/docs/huge.pdf").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].detail.as_deref(), Some("7000000"));
    }

    /// End-to-end: `index_file` on a file above the cap does not error, does
    /// not index anything, and leaves a `"skipped_oversize"` ledger trail.
    /// Uses the default cap (10 MB — no config.toml in the test manager's
    /// data dir) rather than a hardcoded constant.
    #[tokio::test]
    async fn index_file_skips_oversize_file_and_records_ledger_event() {
        let (mgr, dir) = test_manager();
        let max_bytes = crate::domain::canopy_config::CanopyConfig::default().rag_max_file_bytes();
        let big_path = dir.path().join("huge.md");
        std::fs::write(&big_path, vec![b'a'; (max_bytes + 1) as usize]).unwrap();
        let source_path = big_path.to_string_lossy().to_string();

        mgr.index_file(&source_path).await.unwrap();

        let events = mgr.db().rag_events_for_file(&source_path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "skipped_oversize");
        assert_eq!(
            events[0].detail.as_deref(),
            Some((max_bytes + 1).to_string().as_str())
        );

        // Re-running against the unchanged file must not duplicate the event.
        mgr.index_file(&source_path).await.unwrap();
        let events = mgr.db().rag_events_for_file(&source_path).unwrap();
        assert_eq!(events.len(), 1);
    }

    /// A `rag_max_file_mb` written to config.toml is honored on the very
    /// next `index_file` call against the *same, already-constructed*
    /// manager — no daemon restart and no new `IngestionManager` needed.
    /// This is the "read per ingestion pass" behavior the spec requires.
    #[tokio::test]
    async fn index_file_honors_a_lowered_configured_limit_without_restart() {
        let (mgr, dir) = test_manager();

        // 2 MB: comfortably under the 10 MB default, so before the config
        // change below this file would sail past the size check.
        let path = dir.path().join("medium.md");
        std::fs::write(&path, vec![b'a'; 2 * 1024 * 1024]).unwrap();
        let source_path = path.to_string_lossy().to_string();

        // Lower the configured limit to 1 MB, below this file's size — set
        // *after* the manager was constructed, and read fresh by `index_file`.
        let config = crate::domain::canopy_config::CanopyConfig {
            rag_max_file_mb: 1,
            ..Default::default()
        };
        config.save(dir.path()).unwrap();

        mgr.index_file(&source_path).await.unwrap();

        let events = mgr.db().rag_events_for_file(&source_path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "skipped_oversize");
    }

    /// The size-based discard must announce itself at a visible level — a
    /// DEBUG/INFO-only drop is the exact bug CB20 exists to kill (81 of ~270
    /// PDFs vanished with nothing above DEBUG to show for it). The repo has no
    /// tracing-capture harness, so this pins the log level at the source: a
    /// future edit cannot quietly demote the call without failing here.
    #[test]
    fn oversize_skip_is_logged_at_warn() {
        let source = include_str!("ingestion.rs");
        let production_code = source
            .split("mod tests {")
            .next()
            .expect("ingestion.rs always contains the literal \"mod tests {\"");
        let marker = "MB exceeds the";
        let idx = production_code
            .find(marker)
            .expect("the oversize-skip log message must still exist");
        let call_start = production_code[..idx]
            .rfind("tracing::")
            .expect("the oversize-skip message must be emitted through a tracing macro");
        assert!(
            production_code[call_start..].starts_with("tracing::warn!"),
            "the oversize-skip discard must be logged at WARN, not DEBUG/INFO — found: {:?}",
            &production_code[call_start..call_start + 20.min(production_code.len() - call_start)]
        );
    }

    /// The permanent-vs-transient decision inside
    /// `record_queued_file_failure` — previously reachable only through
    /// `drain_queue`'s internals.
    #[test]
    fn permanent_index_failure_respects_threshold_and_fatal_messages() {
        let transient = "embedding request timed out";
        assert!(!permanent_index_failure(transient, 1)); // attempt 2
        assert!(permanent_index_failure(transient, 2)); // attempt 3 == MAX_RAG_ATTEMPTS
        assert!(permanent_index_failure("document overflowed its stack", 0));
    }

    /// The permanent path in `record_queued_file_failure` (give-up event +
    /// notification) used to be reachable only through drain internals.
    #[tokio::test]
    async fn record_queued_file_failure_permanent_after_max_attempts() {
        let (mgr, _dir) = test_manager();
        let path = "/docs/flaky.md";
        let now = chrono::Utc::now().timestamp();
        mgr.db().log_rag_event(path, "error", None, now).unwrap();
        mgr.db().log_rag_event(path, "error", None, now).unwrap();

        mgr.record_queued_file_failure(path, "embedding request timed out");

        let events = mgr.db().rag_events_for_file(path).unwrap();
        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert!(types.contains(&"error"), "events: {types:?}");
        assert!(types.contains(&"failed"), "events: {types:?}");
        assert!(!types.contains(&"indexed"), "events: {types:?}");
    }

    /// Before extraction the per-path orphan decision only ran inside
    /// `reconcile_orphan_chunks`' store loop.
    #[test]
    fn orphan_purge_reason_ignores_existing_in_scope_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("note.md");
        std::fs::write(&file, "hi").unwrap();
        let roots = vec![dir.path().to_path_buf()];
        assert_eq!(orphan_purge_reason(&file, &roots), None);
    }

    #[test]
    fn orphan_purge_reason_purges_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone.md");
        let roots = vec![dir.path().to_path_buf()];
        assert_eq!(orphan_purge_reason(&missing, &roots), Some((true, false)));
    }

    #[test]
    fn orphan_purge_reason_purges_file_outside_roots() {
        let file_dir = tempfile::tempdir().unwrap();
        let file = file_dir.path().join("note.md");
        std::fs::write(&file, "hi").unwrap();
        let other_dir = tempfile::tempdir().unwrap();
        let roots = vec![other_dir.path().to_path_buf()];
        assert_eq!(orphan_purge_reason(&file, &roots), Some((false, true)));
    }

    #[test]
    fn orphan_purge_reason_purges_nothing_when_roots_empty() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("note.md");
        std::fs::write(&file, "hi").unwrap();
        assert_eq!(orphan_purge_reason(&file, &[]), None);
    }

    /// The insert loop used to live inline in `sync_vector_store`; pin its
    /// ok/fail accounting against a real store.
    #[tokio::test]
    async fn insert_chunks_into_store_counts_ok_and_failed_chunks() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = VectorStore::open_at(&VectorStore::path_for_tests(temp_dir.path()), 4, None)
            .await
            .unwrap();
        let good = VectorChunk {
            id: "good".to_string(),
            file_path: "/docs/a.md".to_string(),
            content: "ok".to_string(),
            embedding: vec![0.0, 0.0, 0.0, 0.0],
            created_at: 1,
        };
        let bad = VectorChunk {
            id: "bad".to_string(),
            file_path: "/docs/a.md".to_string(),
            content: "wrong dims".to_string(),
            embedding: vec![0.0, 0.0],
            created_at: 1,
        };

        let (ok, fail) = insert_chunks_into_store(&store, "/docs/a.md", &[good, bad]).await;
        assert_eq!((ok, fail), (1, 1));
    }

    /// The watcher callback's routing used to be buried in the notify closure;
    /// drive a create event (→ queue) and a remove event (→ purge spawn).
    #[tokio::test]
    async fn handle_rag_watcher_event_enqueue_and_purge() {
        let (mgr, dir) = test_manager();
        let root = dir.path().to_path_buf();
        let ctx = RagWatchContext {
            rt: tokio::runtime::Handle::current(),
            db: Arc::clone(&mgr.db),
            queue: Arc::clone(&mgr.queue),
            notify: Arc::clone(&mgr.notify),
            roots: vec![root.clone()],
            patterns: crate::rag::ragignore::load_patterns(dir.path()),
            data_dir: dir.path().to_path_buf(),
        };
        let file = root.join("note.md");
        std::fs::write(&file, "# hi").unwrap();
        assert_eq!(mgr.queue_len().await, 0);

        let create =
            Event::new(EventKind::Create(notify::event::CreateKind::File)).add_path(file.clone());
        handle_rag_watcher_event(&ctx, create);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(mgr.queue_len().await, 1);

        // The remove arm spawns a purge; with no embeddings model configured
        // it finds no store and must leave the queue untouched.
        let remove = Event::new(EventKind::Remove(notify::event::RemoveKind::File)).add_path(file);
        handle_rag_watcher_event(&ctx, remove);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(mgr.queue_len().await, 1);
    }
}

#[cfg(test)]
mod additional_tests {
    use super::*;

    // ── Queue ──────────────────────────────────────────────────────

    #[test]
    fn queue_new_is_empty() {
        let mut q = Queue::new();
        assert_eq!(q.len(), 0);
        assert!(q.pop().is_none());
    }

    #[test]
    fn queue_push_and_pop_round_trip() {
        let mut q = Queue::new();
        assert!(q.push("/a.md"));
        assert!(q.push("/b.md"));
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop(), Some("/a.md".to_string()));
        assert_eq!(q.pop(), Some("/b.md".to_string()));
        assert_eq!(q.len(), 0);
        assert!(q.pop().is_none());
    }

    #[test]
    fn queue_push_duplicate_moves_to_end() {
        let mut q = Queue::new();
        q.push("/a.md");
        q.push("/b.md");
        q.push("/c.md");
        // Re-push /b.md — it should move to the end.
        q.push("/b.md");
        assert_eq!(q.len(), 3);
        assert_eq!(q.pop(), Some("/a.md".to_string()));
        assert_eq!(q.pop(), Some("/c.md".to_string()));
        assert_eq!(q.pop(), Some("/b.md".to_string()));
    }

    #[test]
    fn queue_push_rejects_when_full() {
        let mut q = Queue::new();
        for i in 0..QUEUE_MAX {
            assert!(q.push(&format!("/file-{i}.md")));
        }
        assert_eq!(q.len(), QUEUE_MAX);
        assert!(!q.push("/overflow.md"));
        assert_eq!(q.len(), QUEUE_MAX);
    }

    #[test]
    fn queue_pop_removes_from_set() {
        let mut q = Queue::new();
        q.push("/a.md");
        q.pop();
        // Re-push after pop — set should allow it.
        assert!(q.push("/a.md"));
        assert_eq!(q.len(), 1);
    }

    // ── indexing_dir_summary ───────────────────────────────────────

    #[test]
    fn indexing_dir_summary_empty_returns_empty() {
        assert_eq!(indexing_dir_summary(&[]), "");
    }

    #[test]
    fn indexing_dir_summary_single_dir() {
        let paths = vec![
            "/home/user/docs/a.md".to_string(),
            "/home/user/docs/b.md".to_string(),
        ];
        let summary = indexing_dir_summary(&paths);
        assert!(summary.contains("in docs"), "{summary}");
    }

    #[test]
    fn indexing_dir_summary_multiple_dirs() {
        let paths = vec![
            "/home/user/docs/a.md".to_string(),
            "/home/user/projects/b.md".to_string(),
        ];
        let summary = indexing_dir_summary(&paths);
        assert!(summary.contains("across 2 dirs"), "{summary}");
    }

    #[test]
    fn indexing_dir_summary_files_with_no_parent() {
        let paths = vec!["standalone.md".to_string()];
        let summary = indexing_dir_summary(&paths);
        // Files with no parent dir produce an empty dir string; the summary
        // still formats it as " in " (the leaf of "" is "").
        assert!(summary.contains(" in"), "{summary}");
    }

    // ── is_html_bytes ─────────────────────────────────────────────

    #[test]
    fn is_html_bytes_detects_doctype() {
        assert!(is_html_bytes(
            b"<!DOCTYPE html><html><body>hello</body></html>"
        ));
    }

    #[test]
    fn is_html_bytes_detects_html_tag() {
        assert!(is_html_bytes(b"<html><head></head><body></body></html>"));
    }

    #[test]
    fn is_html_bytes_detects_html_with_leading_bom() {
        assert!(is_html_bytes(b"\xef\xbb\xbf<!doctype html>"));
    }

    #[test]
    fn is_html_bytes_rejects_pdf_magic() {
        assert!(!is_html_bytes(b"%PDF-1.4 some pdf content"));
    }

    #[test]
    fn is_html_bytes_rejects_random_binary() {
        assert!(!is_html_bytes(&[0x00, 0x01, 0x02, 0x03, 0xFF, 0xFE]));
    }

    #[test]
    fn is_html_bytes_rejects_empty() {
        assert!(!is_html_bytes(b""));
    }

    #[test]
    fn is_html_bytes_rejects_short_input() {
        assert!(!is_html_bytes(b"<h"));
    }

    // ── decode_entity ─────────────────────────────────────────────

    #[test]
    fn decode_entity_known_entities() {
        assert_eq!(decode_entity("amp"), "&");
        assert_eq!(decode_entity("lt"), "<");
        assert_eq!(decode_entity("gt"), ">");
        assert_eq!(decode_entity("nbsp"), " ");
        assert_eq!(decode_entity("#160"), " ");
        assert_eq!(decode_entity("quot"), "\"");
        assert_eq!(decode_entity("apos"), "'");
        assert_eq!(decode_entity("#39"), "'");
    }

    #[test]
    fn decode_entity_unknown_returns_space() {
        assert_eq!(decode_entity("unknown"), " ");
        assert_eq!(decode_entity(""), " ");
    }

    // ── extract_tag_name ──────────────────────────────────────────

    #[test]
    fn extract_tag_name_simple() {
        assert_eq!(extract_tag_name("<div>"), "div");
    }

    #[test]
    fn extract_tag_name_with_attributes() {
        assert_eq!(extract_tag_name("<img src=\"x.png\" />"), "img");
    }

    #[test]
    fn extract_tag_name_closing_tag() {
        assert_eq!(extract_tag_name("</p>"), "p");
    }

    #[test]
    fn extract_tag_name_self_closing() {
        // Note: the function doesn't strip trailing '/' since it's not in the split criteria.
        assert_eq!(extract_tag_name("<br/>"), "br/");
    }

    #[test]
    fn extract_tag_name_with_whitespace() {
        assert_eq!(extract_tag_name("<H1>"), "h1");
    }

    // ── is_block_level_tag ────────────────────────────────────────

    #[test]
    fn is_block_level_tag_true_for_common_blocks() {
        for tag in [
            "p",
            "div",
            "br",
            "li",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "tr",
            "td",
            "th",
            "blockquote",
            "section",
            "article",
        ] {
            assert!(
                is_block_level_tag(tag),
                "expected '{tag}' to be block-level"
            );
        }
    }

    #[test]
    fn is_block_level_tag_false_for_inline() {
        for tag in ["span", "a", "em", "strong", "img", "b", "i", "code"] {
            assert!(
                !is_block_level_tag(tag),
                "expected '{tag}' to NOT be block-level"
            );
        }
    }

    // ── collapse_blank_lines ──────────────────────────────────────

    #[test]
    fn collapse_blank_lines_collapses_consecutive_blanks() {
        let input = "line1\n\n\n\nline2\n";
        let result = collapse_blank_lines(input);
        assert_eq!(result, "line1\n\nline2\n");
    }

    #[test]
    fn collapse_blank_lines_preserves_single_blank() {
        let input = "line1\n\nline2\n";
        let result = collapse_blank_lines(input);
        assert_eq!(result, "line1\n\nline2\n");
    }

    #[test]
    fn collapse_blank_lines_no_blanks() {
        let input = "line1\nline2\nline3\n";
        let result = collapse_blank_lines(input);
        assert_eq!(result, "line1\nline2\nline3\n");
    }

    #[test]
    fn collapse_blank_lines_empty_input() {
        assert_eq!(collapse_blank_lines(""), "");
    }

    // ── salvage_printable_text ────────────────────────────────────

    #[test]
    fn salvage_printable_text_extracts_long_runs() {
        let mut bytes = vec![0xFF; 20];
        bytes.extend_from_slice(b"Hello World this is text");
        bytes.extend_from_slice(&[0xFF; 20]);
        let result = salvage_printable_text(&bytes);
        assert!(result.contains("Hello World this is text"), "{result}");
    }

    #[test]
    fn salvage_printable_text_ignores_short_runs() {
        let mut bytes = vec![0xFF; 10];
        bytes.extend_from_slice(b"ab"); // only 2 chars, below MIN_RUN
        bytes.extend_from_slice(&[0xFF; 10]);
        let result = salvage_printable_text(&bytes);
        assert!(result.is_empty(), "short runs should be ignored: {result}");
    }

    #[test]
    fn salvage_printable_text_empty_input() {
        assert_eq!(salvage_printable_text(b""), "");
    }

    #[test]
    fn salvage_printable_text_all_printable() {
        let text = b"Hello, this is a printable string with enough length to pass the run filter.";
        let result = salvage_printable_text(text);
        assert!(result.contains("Hello"), "{result}");
    }

    #[test]
    fn salvage_printable_text_all_binary() {
        let bytes: Vec<u8> = (0..100).map(|i| (i * 37 % 256) as u8).collect();
        let result = salvage_printable_text(&bytes);
        // May or may not find short runs, but should not panic.
        let _ = result;
    }

    // ── strip_html_to_text ────────────────────────────────────────

    #[test]
    fn strip_html_to_text_strips_simple_tags() {
        let html = "<p>Hello <b>world</b></p>";
        let text = strip_html_to_text(html);
        assert!(text.contains("Hello"), "{text}");
        assert!(text.contains("world"), "{text}");
        assert!(!text.contains("<p>"), "{text}");
        assert!(!text.contains("<b>"), "{text}");
    }

    #[test]
    fn strip_html_to_text_handles_nested_tags() {
        let html = "<div><p>First</p><p>Second</p></div>";
        let text = strip_html_to_text(html);
        assert!(text.contains("First"), "{text}");
        assert!(text.contains("Second"), "{text}");
    }

    #[test]
    fn strip_html_to_text_collapses_whitespace() {
        let html = "<p>   Hello   </p>";
        let text = strip_html_to_text(html);
        // Should not have leading/trailing whitespace from tags.
        let trimmed = text.trim();
        assert!(trimmed.contains("Hello"), "{text}");
    }

    #[test]
    fn strip_html_to_text_empty_input() {
        assert_eq!(strip_html_to_text(""), "");
    }

    // ── is_permanent_rag_error edge cases ─────────────────────────

    #[test]
    fn is_permanent_rag_error_case_insensitive() {
        assert!(is_permanent_rag_error("STACK OVERFLOW"));
        assert!(is_permanent_rag_error("Not A Valid Pdf"));
    }

    #[test]
    fn is_permanent_rag_error_empty_string() {
        assert!(!is_permanent_rag_error(""));
    }

    #[test]
    fn is_permanent_rag_error_partial_match_not_enough() {
        assert!(!is_permanent_rag_error("pdf")); // must be "not a valid pdf"
    }

    // ── store_loss_detected edge cases ────────────────────────────

    #[test]
    fn store_loss_detected_both_zero() {
        // Ledger has 0 files, store has 0 chunks — not a loss.
        assert!(!store_loss_detected(0, Some(0)));
    }

    #[test]
    fn store_loss_detected_store_none() {
        // Store couldn't open — not treated as loss.
        assert!(!store_loss_detected(100, None));
    }

    #[test]
    fn store_loss_detected_large_ledger_zero_store() {
        assert!(store_loss_detected(1000, Some(0)));
    }

    // ── QUEUE_MAX boundary ────────────────────────────────────────

    #[test]
    fn queue_exactly_at_max_allows_push() {
        let mut q = Queue::new();
        for i in 0..QUEUE_MAX {
            q.push(&format!("/{i}.md"));
        }
        // The last push succeeded (len == QUEUE_MAX).
        assert_eq!(q.len(), QUEUE_MAX);
    }

    #[test]
    fn queue_one_over_max_rejects() {
        let mut q = Queue::new();
        for i in 0..QUEUE_MAX {
            q.push(&format!("/{i}.md"));
        }
        assert!(!q.push("/overflow.md"));
    }

    // ── decode_html_entity integration ────────────────────────────

    #[test]
    fn strip_html_to_text_decodes_entities() {
        let html = "<p>5&gt;3&amp;2&lt;4</p>";
        let text = strip_html_to_text(html);
        assert!(text.contains("5>3&2<4"), "{text}");
    }

    #[test]
    fn strip_html_to_text_decodes_nbsp() {
        let html = "<p>a&nbsp;b</p>";
        let text = strip_html_to_text(html);
        assert!(text.contains("a b"), "{text}");
    }

    // ── HtmlStripState edge cases ─────────────────────────────────

    #[test]
    fn strip_html_to_text_script_content_skipped() {
        let html = "<p>before</p><script>alert('xss')</script><p>after</p>";
        let text = strip_html_to_text(html);
        assert!(text.contains("before"), "{text}");
        assert!(text.contains("after"), "{text}");
        assert!(!text.contains("alert"), "{text}");
    }

    #[test]
    fn strip_html_to_text_style_content_skipped() {
        let html = "<p>text</p><style>.red{color:red}</style>";
        let text = strip_html_to_text(html);
        assert!(text.contains("text"), "{text}");
        assert!(!text.contains("color"), "{text}");
    }

    #[test]
    fn strip_html_to_text_block_tags_add_newlines() {
        let html = "<p>one</p><p>two</p>";
        let text = strip_html_to_text(html);
        // Block-level tags (p) should produce newlines.
        assert!(
            text.contains("\n"),
            "block tags should add newlines: {text}"
        );
    }

    // ── Queue push with empty string ──────────────────────────────

    #[test]
    fn queue_push_empty_string() {
        let mut q = Queue::new();
        assert!(q.push(""));
        assert_eq!(q.len(), 1);
        assert_eq!(q.pop(), Some("".to_string()));
    }

    // ── salvage_printable_text runs joined with space ─────────────

    #[test]
    fn salvage_printable_text_joins_runs_with_space() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"Hello");
        bytes.push(0xFF); // separator
        bytes.extend_from_slice(b"World");
        let result = salvage_printable_text(&bytes);
        assert!(result.contains("Hello"), "{result}");
        assert!(result.contains("World"), "{result}");
    }

    // ── extract_tag_name edge cases ──────────────────────────────

    #[test]
    fn extract_tag_name_empty_string() {
        assert_eq!(extract_tag_name(""), "");
    }

    #[test]
    fn extract_tag_name_just_angle_bracket() {
        assert_eq!(extract_tag_name("<"), "");
    }

    #[test]
    fn extract_tag_name_no_closing_bracket() {
        assert_eq!(extract_tag_name("<div class='x'"), "div");
    }

    #[test]
    fn extract_tag_name_nested_angle() {
        // Tag with inner '>' — stops at first '>'
        assert_eq!(extract_tag_name("<div>a>b</div>"), "div");
    }

    // ── decode_html_entity edge cases ────────────────────────────

    #[test]
    fn decode_entity_long_entity_breaks_at_8_chars() {
        // Entity longer than 8 chars breaks early and returns space
        assert_eq!(decode_entity("verylongentity"), " ");
    }

    #[test]
    fn decode_entity_single_char() {
        assert_eq!(decode_entity("a"), " ");
    }

    // ── strip_html_to_text more scenarios ────────────────────────

    #[test]
    fn strip_html_to_text_multiple_block_tags() {
        let html = "<h1>Title</h1><p>Para1</p><p>Para2</p><div>Div</div>";
        let text = strip_html_to_text(html);
        assert!(text.contains("Title"), "{text}");
        assert!(text.contains("Para1"), "{text}");
        assert!(text.contains("Para2"), "{text}");
        assert!(text.contains("Div"), "{text}");
    }

    #[test]
    fn strip_html_to_text_entity_in_text() {
        let html = "<p>a&amp;b</p>";
        let text = strip_html_to_text(html);
        assert!(text.contains("a&b"), "{text}");
    }

    #[test]
    fn strip_html_to_text_only_tags_no_text() {
        let html = "<div><p></p></div>";
        let text = strip_html_to_text(html);
        assert!(text.trim().is_empty(), "expected empty: {text}");
    }

    #[test]
    fn strip_html_to_text_nested_script() {
        let html = "<div>before<script>var x=1;</script>after</div>";
        let text = strip_html_to_text(html);
        assert!(text.contains("before"), "{text}");
        assert!(!text.contains("after"), "{text}");
        assert!(!text.contains("var"), "{text}");
    }

    // ── collapse_blank_lines edge cases ──────────────────────────

    #[test]
    fn collapse_blank_lines_only_blanks() {
        let input = "\n\n\n\n";
        let result = collapse_blank_lines(input);
        assert_eq!(result, "\n");
    }

    #[test]
    fn collapse_blank_lines_mixed() {
        let input = "a\n\n\nb\n\nc\n";
        let result = collapse_blank_lines(input);
        assert_eq!(result, "a\n\nb\n\nc\n");
    }

    #[test]
    fn collapse_blank_lines_trailing_blank() {
        let input = "a\n\n";
        let result = collapse_blank_lines(input);
        assert_eq!(result, "a\n\n");
    }

    // ── is_html_bytes more edge cases ────────────────────────────

    #[test]
    fn is_html_bytes_lowercase_html() {
        assert!(is_html_bytes(b"<html>"));
    }

    #[test]
    fn is_html_bytes_uppercase_html() {
        assert!(is_html_bytes(b"<HTML>"));
    }

    #[test]
    fn is_html_bytes_mixed_case_doctype() {
        assert!(is_html_bytes(b"<!DoCtYpE html>"));
    }

    // ── salvage_printable_text edge cases ────────────────────────

    #[test]
    fn salvage_printable_text_single_long_run() {
        let text = b"This is a single long printable run of text that exceeds the minimum";
        let result = salvage_printable_text(text);
        assert!(result.contains("single long printable"), "{result}");
    }

    #[test]
    fn salvage_printable_text_newlines_and_tabs() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"line1\nline2\ttab");
        let result = salvage_printable_text(&bytes);
        assert!(result.contains("line1"), "{result}");
        assert!(result.contains("line2"), "{result}");
    }

    // ── is_block_level_tag edge cases ────────────────────────────

    #[test]
    fn is_block_level_tag_empty_string() {
        assert!(!is_block_level_tag(""));
    }

    #[test]
    fn is_block_level_tag_unknown_tag() {
        assert!(!is_block_level_tag("custom-element"));
    }

    // ── Queue edge cases ─────────────────────────────────────────

    #[test]
    fn queue_pop_empty_returns_none() {
        let mut q = Queue::new();
        assert!(q.pop().is_none());
    }

    #[test]
    fn queue_push_same_path_moves_to_end() {
        let mut q = Queue::new();
        q.push("/a.md");
        q.push("/b.md");
        q.push("/a.md");
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop(), Some("/b.md".to_string()));
        assert_eq!(q.pop(), Some("/a.md".to_string()));
    }

    // ── extract_file_content edge cases ──────────────────────────

    #[test]
    fn extract_file_content_text_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello world").unwrap();
        let result = extract_file_content(&path, "text").unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn extract_file_content_md_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.md");
        std::fs::write(&path, "# Title\nContent").unwrap();
        let result = extract_file_content(&path, "markdown").unwrap();
        assert!(result.contains("Title"));
    }
    // ── Coverage for helpers extracted during the quality refactor ────

    /// The end-of-drain summary block moved out of `drain_queue` (which no
    /// test drives end-to-end); exercise both of its branches directly.
    #[test]
    fn report_drain_queue_outcome_covers_indexed_and_failed_branches() {
        let paths = vec!["/tmp/canopy-drain-summary/a.md".to_string()];
        IngestionManager::report_drain_queue_outcome(1, 1, &paths);
        IngestionManager::report_drain_queue_outcome(0, 0, &[]);
    }

    /// The reconcile outcome log (purged > 0 vs none found) moved out of
    /// `reconcile_orphan_chunks`, which only runs against a live store.
    #[test]
    fn trace_reconcile_outcome_covers_purged_and_none_branches() {
        IngestionManager::trace_reconcile_outcome(2);
        IngestionManager::trace_reconcile_outcome(0);
    }
}
