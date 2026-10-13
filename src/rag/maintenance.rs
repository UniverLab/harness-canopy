//! RAG store maintenance: the periodic tick, compaction, and health.
//!
//! Nothing here is user-initiated: the daemon runs one tick at startup (after
//! the existing reconciliations) and every 6 hours, and `drain_queue`
//! compacts when a drain ends or every 500 inserted chunks. The self-healing
//! surface is deliberately narrow (spec §5): compaction, orphan purge and
//! ledger reconcile only — maintenance never resumes paused indexing, never
//! purges the store, and never downloads a model.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::application::ports::StateRepository;
use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::domain::sync::MessageKind;
use crate::rag::ingestion::IngestionManager;
use crate::rag::vector_store::{StoreFootprint, VectorStore};

/// How often the daemon runs the maintenance tick.
pub const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(6 * 3600);
/// A tick compacts past this many fragments...
const COMPACT_FRAGMENT_THRESHOLD: usize = 50;
/// ...or this many versions.
const COMPACT_VERSION_THRESHOLD: usize = 100;
/// Health degrades past this many fragments...
const DEGRADE_FRAGMENT_THRESHOLD: usize = 100;
/// ...or more than this many versions per indexed file.
const DEGRADE_VERSION_MULTIPLIER: usize = 10;
/// A paused queue item older than this many hours degrades health.
const STALE_QUEUE_HOURS: i64 = 24;
/// An ingestion error within this many hours degrades health.
const INGESTION_ERROR_WINDOW_HOURS: i64 = 24;
/// Persisted copy of the latest health, read by `canopy rag report` and
/// `canopy doctor`.
pub const RAG_HEALTH_STATE_KEY: &str = "rag_health_json";
/// Activity-stream source for health transitions.
const HEALTH_ACTIVITY_SOURCE: &str = "rag-maintenance";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RagHealthStatus {
    Ok,
    Degraded,
    Broken,
}

impl RagHealthStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Broken => "broken",
        }
    }
}

/// The store's computed health, kept in memory by the manager and persisted
/// so CLI surfaces can read it without re-scanning.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RagHealth {
    pub status: RagHealthStatus,
    pub reasons: Vec<String>,
    pub checked_at: chrono::DateTime<chrono::Utc>,
}

impl RagHealth {
    /// The state before any tick has run.
    pub fn initial() -> Self {
        Self {
            status: RagHealthStatus::Ok,
            reasons: Vec::new(),
            checked_at: chrono::Utc::now(),
        }
    }
}

/// Every input the health rules judge, as plain values — so `compute_health`
/// is a pure function, unit-testable without a store or a daemon.
pub struct HealthInput {
    /// `Some` when a configured store could not be opened → `broken`.
    pub store_open_error: Option<String>,
    /// False when no embeddings model is configured: there is no store to
    /// judge, so store-dependent reasons are skipped.
    pub store_checked: bool,
    pub fragments: usize,
    pub versions: usize,
    /// Files the ledger believes are indexed (`indexed_files_timestamps`).
    pub ledger_indexed_files: usize,
    /// Distinct `file_path`s the store holds. The ledger carries no per-file
    /// chunk totals, so file counts are the only directly comparable pair —
    /// that is what "ledger and store disagree" means with the APIs at hand.
    pub store_files: i64,
    pub queue_len: usize,
    pub queue_oldest_queued_at: Option<i64>,
    pub paused: bool,
    pub last_ingestion_error_24h: Option<String>,
    /// Previous consecutive ticks that flagged "needs compaction".
    pub degraded_ticks: usize,
    pub last_compact_error: Option<String>,
    pub now: i64,
}

/// The health rules, in the spec's order: broken beats degraded beats ok.
pub fn compute_health(input: &HealthInput) -> RagHealth {
    let mut reasons: Vec<String> = Vec::new();

    if let Some(error) = &input.store_open_error {
        reasons.push(format!("vector store cannot be opened: {error}"));
        return health_at(RagHealthStatus::Broken, reasons, input.now);
    }

    if input.store_checked {
        if needs_compaction(input.fragments, input.versions, input.ledger_indexed_files) {
            let mut reason = format!(
                "store needs compaction: {} fragments, {} versions for {} indexed file(s) — canopy rag compact",
                input.fragments, input.versions, input.ledger_indexed_files
            );
            // Quoted only once the degradation has persisted across two
            // ticks: a single bad tick is transient by construction.
            if input.degraded_ticks >= 1 {
                if let Some(error) = &input.last_compact_error {
                    reason.push_str(&format!(" — last compaction error: {error}"));
                }
            }
            reasons.push(reason);
        }

        if input.ledger_indexed_files > 0 && input.store_files != input.ledger_indexed_files as i64
        {
            reasons.push(format!(
                "ledger and store disagree: {} indexed file(s) in the ledger, {} in the store — the next reconciliation requeues them",
                input.ledger_indexed_files, input.store_files
            ));
        }
    }

    if input.paused {
        if let Some(oldest) = input.queue_oldest_queued_at {
            if input.now - oldest > STALE_QUEUE_HOURS * 3600 {
                reasons.push(format!(
                    "{} file(s) waiting since {} — canopy rag auto-index start",
                    input.queue_len,
                    format_day(oldest)
                ));
            }
        }
    }

    if let Some(error) = &input.last_ingestion_error_24h {
        reasons.push(format!("indexing error in the last 24 h: {error}"));
    }

    let status = if reasons.is_empty() {
        RagHealthStatus::Ok
    } else {
        RagHealthStatus::Degraded
    };
    health_at(status, reasons, input.now)
}

/// Whether this tick's store figures are past the degrade thresholds. The
/// version budget is per indexed file, so with an empty ledger there is no
/// budget to compare against — only the fragment ceiling applies then.
fn needs_compaction(fragments: usize, versions: usize, ledger_indexed_files: usize) -> bool {
    fragments > DEGRADE_FRAGMENT_THRESHOLD
        || (ledger_indexed_files > 0
            && versions > DEGRADE_VERSION_MULTIPLIER * ledger_indexed_files)
}

fn health_at(status: RagHealthStatus, reasons: Vec<String>, now: i64) -> RagHealth {
    RagHealth {
        status,
        reasons,
        checked_at: chrono::DateTime::from_timestamp(now, 0).unwrap_or_else(chrono::Utc::now),
    }
}

fn format_day(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn bytes_to_mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// A status change is what gets published to the activity stream; a repeated
/// status never is.
fn health_transitioned(previous: &RagHealthStatus, current: &RagHealthStatus) -> bool {
    previous != current
}

/// The most recent `"error"` ingestion event within the last 24 hours.
fn last_ingestion_error_24h(db: &Database, now: i64) -> Option<String> {
    let cutoff = now - INGESTION_ERROR_WINDOW_HOURS * 3600;
    db.list_rag_events(1_000)
        .ok()?
        .into_iter()
        .find(|event| event.event_type == "error" && event.occurred_at >= cutoff)
        .map(|event| match event.detail {
            Some(detail) => format!("{detail} ({})", event.file_path),
            None => event.file_path,
        })
}

/// One tick's verdict on the store.
struct StoreSnapshot {
    open_error: Option<String>,
    checked: bool,
    fragments: usize,
    versions: usize,
    store_files: i64,
}

impl StoreSnapshot {
    /// No store to judge (no embeddings model configured).
    fn unchecked() -> Self {
        Self {
            open_error: None,
            checked: false,
            fragments: 0,
            versions: 0,
            store_files: 0,
        }
    }
}

impl IngestionManager {
    /// The daemon's own store handle, opened on first use. Every insert and
    /// every compaction goes through this one `Table`, so no prune can ever
    /// surprise an independent connection holding its own.
    pub(crate) async fn shared_store(&self) -> tokio::sync::MutexGuard<'_, Option<VectorStore>> {
        let mut guard = self.vector_store.lock().await;
        if guard.is_none() {
            let config = CanopyConfig::load(self.data_dir());
            *guard = self.open_store(&config).await;
        }
        guard
    }

    /// The latest computed health.
    pub fn rag_health(&self) -> RagHealth {
        self.health
            .lock()
            .map(|health| health.clone())
            .unwrap_or_else(|_| RagHealth::initial())
    }

    /// The one compaction implementation. The caller holds the shared store
    /// mutex; success resets the chunk counter, failure is recorded (and
    /// returned) so health can quote it on the next tick.
    async fn compact_locked(
        &self,
        guard: &mut Option<VectorStore>,
    ) -> anyhow::Result<(StoreFootprint, StoreFootprint)> {
        let Some(store) = guard.as_ref() else {
            anyhow::bail!("RAG vector store unavailable — check model config");
        };
        match store.compact().await {
            Ok(footprints) => {
                self.chunks_since_compact.store(0, Ordering::Relaxed);
                self.set_last_compact_error(None);
                Ok(footprints)
            }
            Err(error) => {
                let message = format!("{error:#}");
                self.set_last_compact_error(Some(message.clone()));
                Err(anyhow::anyhow!(message))
            }
        }
    }

    fn set_last_compact_error(&self, error: Option<String>) {
        if let Ok(mut slot) = self.last_compact_error.lock() {
            *slot = error;
        }
    }

    /// Compact the shared store, logging why and the outcome. Fire-and-forget
    /// by design: a compaction failure must never bubble into indexing.
    pub(crate) async fn compact_shared_store(&self, why: &str) {
        let mut guard = self.shared_store().await;
        match self.compact_locked(&mut guard).await {
            Ok(_) => tracing::info!("RAG maintenance: compaction done ({why})"),
            Err(error) => {
                tracing::warn!("RAG maintenance: compaction failed ({why}): {error:#}");
            }
        }
    }

    /// Behind the `rag_compact` MCP tool / `canopy rag compact`: run a
    /// compaction now and return a human-readable before/after block.
    /// Refuses politely when a maintenance pass is already running.
    pub(crate) async fn compact_now_report(&self) -> String {
        let Ok(_maintenance_guard) = self.maintenance_lock.try_lock() else {
            return "RAG maintenance is already running — try again shortly.".to_string();
        };
        let mut guard = self.shared_store().await;
        if guard.is_none() {
            return "RAG vector store unavailable — check model config.".to_string();
        }
        match self.compact_locked(&mut guard).await {
            Ok((before, after)) => format!(
                "RAG compact done — fragments {} → {}, versions {} → {}, on disk {:.1} MB → {:.1} MB",
                before.fragments,
                after.fragments,
                before.versions,
                after.versions,
                bytes_to_mib(before.bytes_on_disk),
                bytes_to_mib(after.bytes_on_disk),
            ),
            Err(error) => format!("RAG compaction failed: {error:#}"),
        }
    }

    /// Spawn the periodic tick. It sleeps first — the startup pass is awaited
    /// by the daemon's own startup sequence.
    pub(crate) fn maintenance_loop(self: Arc<Self>, ct: tokio_util::sync::CancellationToken) {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = ct.cancelled() => break,
                    _ = tokio::time::sleep(MAINTENANCE_INTERVAL) => {
                        self.run_rag_maintenance().await;
                    }
                }
            }
        });
    }

    /// One maintenance pass: compact when due, run both reconciliations,
    /// compute health, persist it, and publish a transition. Never two at
    /// once — a second caller returns immediately.
    pub async fn run_rag_maintenance(&self) {
        let Ok(_maintenance_guard) = self.maintenance_lock.try_lock() else {
            tracing::debug!("RAG maintenance: a tick is already running — skipping");
            return;
        };

        let now = chrono::Utc::now().timestamp();
        let config = CanopyConfig::load(self.data_dir());
        let snapshot = if config.embeddings_model.trim().is_empty() {
            StoreSnapshot::unchecked()
        } else {
            self.tick_store().await
        };

        // The same two reconciliations that run at startup; the tick is what
        // makes them periodic (the store-loss case clears the ledger here,
        // which is exactly what "until the reconcile requeues" relies on).
        self.reconcile_orphan_chunks().await;
        self.reconcile_ledger_with_store().await;

        let previous_status = self.rag_health().status;
        let input = self.health_input(&snapshot, now);
        let health = compute_health(&input);
        let flagged = needs_compaction(input.fragments, input.versions, input.ledger_indexed_files);
        let previous_ticks = self.degraded_ticks.load(Ordering::Relaxed);
        self.degraded_ticks.store(
            if flagged { previous_ticks + 1 } else { 0 },
            Ordering::Relaxed,
        );

        let _ = self.db().set_state(
            RAG_HEALTH_STATE_KEY,
            &serde_json::to_string(&health).unwrap_or_default(),
        );
        if let Ok(mut slot) = self.health.lock() {
            *slot = health.clone();
        }
        tracing::info!(
            "RAG maintenance: health {} — {}",
            health.status.as_str(),
            if health.reasons.is_empty() {
                "no reasons".to_string()
            } else {
                health.reasons.join("; ")
            }
        );
        self.publish_health_transition(&previous_status, &health);
    }

    /// Store half of a tick: compact when due (unless a drain is actively
    /// indexing — it compacts at its end), then read the post-compaction
    /// footprint and distinct-file count.
    async fn tick_store(&self) -> StoreSnapshot {
        let mut guard = self.shared_store().await;
        let Some(store) = guard.as_ref() else {
            return StoreSnapshot {
                open_error: Some(
                    "could not be opened — run `canopy rag purge` to rebuild it".to_string(),
                ),
                checked: true,
                ..StoreSnapshot::unchecked()
            };
        };
        let pre = match store.footprint().await {
            Ok(footprint) => footprint,
            Err(error) => {
                return StoreSnapshot {
                    open_error: Some(format!("{error:#}")),
                    checked: true,
                    ..StoreSnapshot::unchecked()
                };
            }
        };
        let due = (pre.fragments > COMPACT_FRAGMENT_THRESHOLD
            || pre.versions > COMPACT_VERSION_THRESHOLD)
            && !self.draining.load(Ordering::Relaxed);
        if due {
            if let Err(error) = self.compact_locked(&mut guard).await {
                tracing::warn!("RAG maintenance: scheduled compaction failed: {error:#}");
            }
        }

        let Some(store) = guard.as_ref() else {
            unreachable!("compaction never drops the shared store handle");
        };
        let footprint = match store.footprint().await {
            Ok(footprint) => footprint,
            Err(error) => {
                return StoreSnapshot {
                    open_error: Some(format!("{error:#}")),
                    checked: true,
                    ..StoreSnapshot::unchecked()
                };
            }
        };
        StoreSnapshot {
            open_error: None,
            checked: true,
            fragments: footprint.fragments,
            versions: footprint.versions,
            store_files: store.count_unique_paths().await.unwrap_or(-1),
        }
    }

    /// Assemble the pure-function inputs from the database and this tick's
    /// store snapshot.
    fn health_input(&self, snapshot: &StoreSnapshot, now: i64) -> HealthInput {
        let ledger = self.db().indexed_files_timestamps().unwrap_or_default();
        let queue = self.db().list_rag_queue(10_000).unwrap_or_default();
        HealthInput {
            store_open_error: snapshot.open_error.clone(),
            store_checked: snapshot.checked,
            fragments: snapshot.fragments,
            versions: snapshot.versions,
            ledger_indexed_files: ledger.len(),
            store_files: snapshot.store_files,
            queue_len: queue.len(),
            queue_oldest_queued_at: queue.iter().map(|item| item.queued_at).min(),
            paused: self.is_paused(),
            last_ingestion_error_24h: last_ingestion_error_24h(self.db(), now),
            degraded_ticks: self.degraded_ticks.load(Ordering::Relaxed),
            last_compact_error: self
                .last_compact_error
                .lock()
                .ok()
                .and_then(|error| error.clone()),
            now,
        }
    }

    /// Publish exactly one activity entry per status transition — never one
    /// per tick.
    fn publish_health_transition(&self, previous: &RagHealthStatus, health: &RagHealth) {
        if !health_transitioned(previous, &health.status) {
            return;
        }
        let message = if health.reasons.is_empty() {
            format!(
                "RAG store health {} → {}",
                previous.as_str(),
                health.status.as_str()
            )
        } else {
            format!(
                "RAG store health {} → {}: {}",
                previous.as_str(),
                health.status.as_str(),
                health.reasons.join("; ")
            )
        };
        let workdir = self.data_dir().to_string_lossy().to_string();
        if let Err(error) = self.db().insert_activity_log_entry(
            &workdir,
            HEALTH_ACTIVITY_SOURCE,
            None,
            MessageKind::Info.as_str(),
            &message,
            None,
        ) {
            tracing::warn!("RAG maintenance: failed to publish health transition: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rag::vector_store::VectorChunk;
    use tempfile::TempDir;

    /// The configured model whose dimensions this test store uses.
    const TEST_MODEL: &str = "baai/bge-small-en-v1.5";
    const TEST_DIMS: usize = 384;

    fn test_manager(dir: &TempDir) -> IngestionManager {
        let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
        IngestionManager::new(db, dir.path().to_path_buf())
    }

    /// A manager whose store is redirected to a scratch path under `dir`,
    /// with a resolvable embeddings model configured so the tick opens it.
    fn test_manager_with_store(dir: &TempDir) -> (IngestionManager, std::path::PathBuf) {
        let mut config = CanopyConfig::load(dir.path());
        config.embeddings_model = TEST_MODEL.to_string();
        config.save(dir.path()).unwrap();
        let store_path = dir.path().join("vectors.lancedb");
        let mut mgr = test_manager(dir);
        mgr.set_store_path_for_tests(store_path.clone());
        (mgr, store_path)
    }

    /// A distinct unit vector per chunk index, so orderings never tie.
    fn big_embedding(i: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; TEST_DIMS];
        v[i % TEST_DIMS] = 1.0;
        v[(i + 1) % TEST_DIMS] = i as f32 * 0.0001;
        v
    }

    /// A real file inside the scratch dir whose path the test chunks claim:
    /// orphan reconciliation deletes chunks whose file is missing, so the
    /// store must point at files that exist (and this keeps the test from
    /// ever looking at the real home store).
    fn scratch_file(dir: &TempDir, i: usize) -> String {
        let path = dir.path().join(format!("doc-{i}.md"));
        std::fs::write(&path, format!("# doc {i}\n")).unwrap();
        path.to_string_lossy().to_string()
    }

    fn health_input() -> HealthInput {
        HealthInput {
            store_open_error: None,
            store_checked: true,
            fragments: 1,
            versions: 5,
            ledger_indexed_files: 10,
            store_files: 10,
            queue_len: 0,
            queue_oldest_queued_at: None,
            paused: false,
            last_ingestion_error_24h: None,
            degraded_ticks: 0,
            last_compact_error: None,
            now: 1_700_000_000,
        }
    }

    /// Spec test: the tick compacts a 120-fragment store and reports ok.
    #[tokio::test]
    async fn tick_compacts_120_fragments_and_reports_ok() {
        let dir = TempDir::new().unwrap();
        let (mgr, store_path) = test_manager_with_store(&dir);

        let store = VectorStore::open_at(&store_path, TEST_DIMS, None)
            .await
            .unwrap();
        for i in 0..120 {
            let file_path = scratch_file(&dir, i);
            store
                .insert_chunk(&VectorChunk {
                    id: format!("chunk-{i}"),
                    file_path,
                    content: format!("chunk {i}"),
                    embedding: big_embedding(i),
                    created_at: 1_700_000_000,
                })
                .await
                .unwrap();
        }
        assert!(
            store.footprint().await.unwrap().fragments > 50,
            "120 single-row adds must produce more than 50 fragments"
        );
        drop(store);

        mgr.run_rag_maintenance().await;

        let store = VectorStore::open_at(&store_path, TEST_DIMS, None)
            .await
            .unwrap();
        assert!(
            store.footprint().await.unwrap().fragments <= 2,
            "the tick must have compacted the store"
        );
        assert_eq!(
            mgr.rag_health().status,
            RagHealthStatus::Ok,
            "reasons: {:?}",
            mgr.rag_health().reasons
        );
    }

    /// FR3: while a drain is actively indexing the tick must NOT compact —
    /// the drain compacts at its own end, and the two must never run at once.
    #[tokio::test]
    async fn tick_skips_compaction_while_draining() {
        let dir = TempDir::new().unwrap();
        let (mgr, store_path) = test_manager_with_store(&dir);

        let store = VectorStore::open_at(&store_path, TEST_DIMS, None)
            .await
            .unwrap();
        for i in 0..120 {
            let file_path = scratch_file(&dir, i);
            store
                .insert_chunk(&VectorChunk {
                    id: format!("chunk-{i}"),
                    file_path,
                    content: format!("chunk {i}"),
                    embedding: big_embedding(i),
                    created_at: 1_700_000_000,
                })
                .await
                .unwrap();
        }
        drop(store);

        // Pretend a drain is mid-flight (it would compact itself at the end).
        mgr.draining.store(true, Ordering::Relaxed);
        mgr.run_rag_maintenance().await;

        let store = VectorStore::open_at(&store_path, TEST_DIMS, None)
            .await
            .unwrap();
        assert!(
            store.footprint().await.unwrap().fragments > 50,
            "the tick must leave compaction to an active drain"
        );
    }

    /// Spec test: paused indexing with a 25 h old item reports degraded — and
    /// maintenance never resumes it.
    #[tokio::test]
    async fn paused_indexing_reports_degraded_and_does_not_resume() {
        let dir = TempDir::new().unwrap();
        let mgr = test_manager(&dir);
        mgr.db().set_state("rag_paused", "1").unwrap();
        let now = chrono::Utc::now().timestamp();
        mgr.db()
            .enqueue_rag_item("/docs/waiting.md", now - 25 * 3600)
            .unwrap();

        mgr.run_rag_maintenance().await;

        let health = mgr.rag_health();
        assert_eq!(health.status, RagHealthStatus::Degraded);
        assert!(
            health
                .reasons
                .iter()
                .any(|r| r.contains("waiting since") && r.contains("canopy rag auto-index start")),
            "reasons: {:?}",
            health.reasons
        );
        assert_eq!(
            mgr.db().get_state("rag_paused").unwrap().as_deref(),
            Some("1"),
            "maintenance must never resume paused indexing"
        );
    }

    /// Spec test: one activity entry per transition, never one per tick.
    #[tokio::test]
    async fn activity_entry_once_per_transition() {
        let dir = TempDir::new().unwrap();
        let mgr = test_manager(&dir);
        mgr.db().set_state("rag_paused", "1").unwrap();
        let now = chrono::Utc::now().timestamp();
        mgr.db()
            .enqueue_rag_item("/docs/waiting.md", now - 25 * 3600)
            .unwrap();

        // ok → degraded: one entry.
        mgr.run_rag_maintenance().await;
        // degraded → degraded: nothing new, however many times it runs.
        mgr.run_rag_maintenance().await;

        let workdir = dir.path().to_string_lossy().to_string();
        let entries = mgr.db().list_activity_log_entries(&workdir, 50).unwrap();
        let transitions: Vec<_> = entries
            .iter()
            .filter(|entry| entry.source == HEALTH_ACTIVITY_SOURCE)
            .collect();
        assert_eq!(
            transitions.len(),
            1,
            "exactly one entry per transition, got {:?}",
            transitions.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        assert!(
            transitions[0].message.contains("ok → degraded"),
            "{}",
            transitions[0].message
        );
    }

    #[test]
    fn health_transition_publishes_only_on_status_change() {
        assert!(health_transitioned(
            &RagHealthStatus::Ok,
            &RagHealthStatus::Degraded
        ));
        assert!(health_transitioned(
            &RagHealthStatus::Degraded,
            &RagHealthStatus::Broken
        ));
        assert!(health_transitioned(
            &RagHealthStatus::Broken,
            &RagHealthStatus::Ok
        ));
        assert!(!health_transitioned(
            &RagHealthStatus::Degraded,
            &RagHealthStatus::Degraded
        ));
        assert!(!health_transitioned(
            &RagHealthStatus::Ok,
            &RagHealthStatus::Ok
        ));
    }

    /// The compaction reason quotes the last compaction error only once the
    /// degradation has persisted across two ticks.
    #[test]
    fn degraded_reason_quotes_compaction_error_on_second_tick() {
        let crowded = |degraded_ticks: usize| HealthInput {
            fragments: 120,
            degraded_ticks,
            last_compact_error: Some("boom".to_string()),
            ..health_input()
        };

        let first = compute_health(&crowded(0));
        assert_eq!(first.status, RagHealthStatus::Degraded);
        assert!(
            first
                .reasons
                .iter()
                .any(|r| r.contains("store needs compaction") && r.contains("canopy rag compact")),
            "reasons: {:?}",
            first.reasons
        );
        assert!(
            !first.reasons.iter().any(|r| r.contains("boom")),
            "a first degraded tick must not quote the error: {:?}",
            first.reasons
        );

        let second = compute_health(&crowded(1));
        assert!(
            second
                .reasons
                .iter()
                .any(|r| r.contains("last compaction error: boom")),
            "reasons: {:?}",
            second.reasons
        );
    }

    #[test]
    fn broken_when_store_cannot_open() {
        let health = compute_health(&HealthInput {
            store_open_error: Some("permission denied".to_string()),
            ..health_input()
        });
        assert_eq!(health.status, RagHealthStatus::Broken);
        assert!(
            health
                .reasons
                .iter()
                .any(|r| r.contains("permission denied")),
            "reasons: {:?}",
            health.reasons
        );
    }

    #[test]
    fn ledger_and_store_disagreement_degrades() {
        let health = compute_health(&HealthInput {
            store_files: 4,
            ..health_input()
        });
        assert_eq!(health.status, RagHealthStatus::Degraded);
        assert!(
            health.reasons.iter().any(|r| r.contains("disagree")),
            "reasons: {:?}",
            health.reasons
        );
    }

    #[test]
    fn healthy_store_with_no_flags_is_ok() {
        assert_eq!(compute_health(&health_input()).status, RagHealthStatus::Ok);
    }

    #[test]
    fn recent_ingestion_error_degrades_with_its_message() {
        let health = compute_health(&HealthInput {
            last_ingestion_error_24h: Some("embedding request timed out".to_string()),
            ..health_input()
        });
        assert_eq!(health.status, RagHealthStatus::Degraded);
        assert!(
            health
                .reasons
                .iter()
                .any(|r| r.contains("indexing error in the last 24 h")
                    && r.contains("embedding request timed out")),
            "reasons: {:?}",
            health.reasons
        );
    }

    /// The version budget is per indexed file: with an empty ledger only the
    /// fragment ceiling can flag degradation, so a freshly compacted store
    /// that still carries recent versions reads as ok.
    #[test]
    fn version_count_is_not_judged_against_an_empty_ledger() {
        let health = compute_health(&HealthInput {
            fragments: 2,
            versions: 121,
            ledger_indexed_files: 0,
            store_files: 0,
            ..health_input()
        });
        assert_eq!(health.status, RagHealthStatus::Ok, "{:?}", health.reasons);
    }
}
