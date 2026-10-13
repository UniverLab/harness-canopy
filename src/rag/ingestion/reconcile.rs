use super::*;

impl IngestionManager {
    /// Scan the vector store for chunks whose source file no longer exists on disk
    /// **or** falls outside the currently configured personal RAG roots, and delete
    /// them. Run once at startup to clean up orphans left from:
    ///  - files deleted while the daemon was offline, and
    ///  - a previous RAG root path that the user changed via `canopy setup`.
    pub async fn reconcile_orphan_chunks(&self) {
        let config = crate::domain::canopy_config::CanopyConfig::load(&self.data_dir);
        let Some(store) = self.open_store(&config).await else {
            return;
        };
        let paths = match store.list_unique_paths().await {
            Ok(p) => p,
            Err(e) => {
                Self::trace_reconcile_list_failed(&e);
                return;
            }
        };
        if paths.is_empty() {
            return;
        }

        // Build the set of currently configured roots (as PathBufs) so we can
        // check whether each indexed path still belongs to them.
        let personal_roots: Vec<std::path::PathBuf> = config
            .rag_personal_dirs
            .iter()
            .map(std::path::PathBuf::from)
            .collect();

        tracing::info!(
            "RAG reconcile: checking {} indexed path(s) for orphan chunks (roots: {:?})",
            paths.len(),
            personal_roots
        );
        let purged = self
            .purge_orphaned_paths(&store, &paths, &personal_roots)
            .await;
        Self::trace_reconcile_outcome(purged);
        if purged > 0 {
            refresh_rag_snapshot(&self.db, &self.data_dir).await;
        }
    }

    /// WARN that the orphan sweep could not enumerate the store's indexed paths.
    pub(super) fn trace_reconcile_list_failed(error: &anyhow::Error) {
        tracing::warn!("RAG reconcile: failed to list indexed paths: {error:#}");
    }

    /// Log how many orphaned paths the sweep purged, or that none were found.
    pub(super) fn trace_reconcile_outcome(purged: usize) {
        if purged > 0 {
            tracing::info!(
                "RAG reconcile: removed chunks for {purged} deleted/out-of-scope file(s)"
            );
        } else {
            tracing::debug!("RAG reconcile: no orphan chunks found");
        }
    }

    /// Delete the chunks of every indexed path that [`orphan_purge_reason`]
    /// flags, logging each decision and its outcome; returns how many paths
    /// were successfully purged.
    pub(super) async fn purge_orphaned_paths(
        &self,
        store: &VectorStore,
        paths: &[String],
        personal_roots: &[PathBuf],
    ) -> usize {
        let mut purged = 0usize;
        for path in paths {
            let disk_path = std::path::Path::new(path);

            let Some((missing_on_disk, outside_roots)) =
                orphan_purge_reason(disk_path, personal_roots)
            else {
                continue;
            };

            let reason = if missing_on_disk {
                "file no longer on disk — orphan chunks purged at startup"
            } else {
                "file outside configured RAG roots — purged after root change"
            };
            tracing::info!(
                "RAG reconcile: '{}' — {} (missing={}, outside_roots={})",
                path,
                reason,
                missing_on_disk,
                outside_roots
            );
            if let Err(e) = store.delete_by_path(path).await {
                tracing::warn!("RAG reconcile: failed to purge '{}': {e:#}", path);
            } else {
                purged += 1;
                let _ = self.db.log_rag_event(
                    path,
                    "deleted",
                    Some(reason),
                    chrono::Utc::now().timestamp(),
                );
            }
        }
        purged
    }

    /// Reconcile the ledger against the vector store at startup. If the
    /// ledger believes files are indexed but the store comes back empty —
    /// store loss, e.g. a hard crash destroyed the LanceDB directory while
    /// the SQLite ledger survived — purge the stale ledger rows and the
    /// pending queue so the caller's directory scan requeues everything for
    /// a full re-index. A healthy/consistent startup, or a store that failed
    /// to open, leaves everything untouched. Returns `true` if store loss
    /// was detected and handled.
    pub async fn reconcile_ledger_with_store(&self) -> bool {
        let store_chunk_count = self.store_chunk_count().await;
        let lancedb_path = match self.lancedb_store_path() {
            Ok(path) => path,
            Err(e) => {
                tracing::error!("RAG reconcile: cannot determine LanceDB path: {e:#}");
                return false;
            }
        };
        self.reconcile_ledger_with_store_chunk_count_at(store_chunk_count, &lancedb_path)
            .await
    }

    /// Path this manager's vector store lives at: the real home-dir store in
    /// production, a scratch path under test.
    pub(crate) fn lancedb_store_path(&self) -> anyhow::Result<PathBuf> {
        match &self.store_path_override {
            Some(path) => Ok(path.clone()),
            None => VectorStore::default_lancedb_path(),
        }
    }

    /// Open this manager's vector store. Production behavior is unchanged:
    /// with no override it delegates to `open_vector_store` (including its
    /// one-shot corruption recovery); tests pass an override so no test ever
    /// opens, prunes, or recreates the real `~/.canopy/rag` store.
    pub(crate) async fn open_store(
        &self,
        config: &crate::domain::canopy_config::CanopyConfig,
    ) -> Option<VectorStore> {
        let Some(path) = &self.store_path_override else {
            return open_vector_store(config, &self.db).await;
        };
        let model = config.embeddings_model.trim();
        if model.is_empty() {
            return None;
        }
        let dimensions = model_dimensions_for_store(model)?;
        match VectorStore::open_at(path, dimensions, Some(config.rag_vector_cache_entries)).await {
            Ok(store) => Some(store),
            Err(error) => {
                tracing::warn!(
                    "RAG: could not open test-overridden store at {}: {error:#}",
                    path.display()
                );
                None
            }
        }
    }

    /// Core of `reconcile_ledger_with_store`, taking the store's chunk count
    /// and the LanceDB directory as plain values so the decision + purge
    /// logic can be unit tested against a scratch directory instead of the
    /// real home-dir-rooted vector store.
    pub(super) async fn reconcile_ledger_with_store_chunk_count_at(
        &self,
        store_chunk_count: Option<i64>,
        lancedb_path: &Path,
    ) -> bool {
        let ledger_indexed_count = self.db.indexed_files_timestamps().unwrap_or_default().len();
        if !store_loss_detected(ledger_indexed_count, store_chunk_count) {
            return false;
        }

        tracing::warn!(
            "RAG reconcile: ledger reports {ledger_indexed_count} indexed file(s) but the \
             vector store has 0 chunks — the store was likely destroyed (e.g. a hard crash) \
             while the SQLite ledger survived; purging stale ledger rows and requeuing a full re-index"
        );
        if let Err(e) = wipe_lancedb_at(
            lancedb_path,
            &self.db,
            "ledger/store reconciliation: ledger has indexed files but store is empty",
        )
        .await
        {
            tracing::error!(
                "RAG reconcile: failed to purge ledger after detecting store loss: {e:#}"
            );
        }
        self.clear_queue().await;
        true
    }

    /// Chunk count in the vector store, or `None` if it could not be opened
    /// (no model configured, or a genuine — not transient — open failure).
    pub(super) async fn store_chunk_count(&self) -> Option<i64> {
        let config = crate::domain::canopy_config::CanopyConfig::load(&self.data_dir);
        let store = self.open_store(&config).await?;
        match store.count_chunks().await {
            Ok(count) => Some(count),
            Err(e) => {
                tracing::warn!("RAG reconcile: failed to count vector store chunks: {e:#}");
                None
            }
        }
    }
}

pub(super) async fn sync_vector_store(
    store: &VectorStore,
    db: &Database,
    data_dir: &Path,
    config: &crate::domain::canopy_config::CanopyConfig,
    source_path: &str,
    chunks: &[VectorChunk],
) -> anyhow::Result<()> {
    let model = config.embeddings_model.trim();
    trace_sync_vector_store_start(chunks, source_path, model);

    if let Err(error) = store.delete_by_path(source_path).await {
        tracing::warn!("RAG vector cleanup error {source_path}: {error:#}");
        // Non-fatal — continue inserting fresh chunks even if delete failed.
    }

    let (ok, fail) = insert_chunks_into_store(store, source_path, chunks).await;

    tracing::info!("RAG sync_vector_store: {source_path} — {ok} inserted, {fail} failed");

    if fail > 0 && ok == 0 {
        anyhow::bail!("All {fail} chunk(s) failed to insert for {source_path} — see errors above");
    }
    if ok > 0 {
        refresh_rag_snapshot(db, data_dir).await;
    }
    Ok(())
}

/// Log the start of a vector-store sync for one file (chunk count + model).
pub(super) fn trace_sync_vector_store_start(
    chunks: &[VectorChunk],
    source_path: &str,
    model: &str,
) {
    tracing::info!(
        "RAG sync_vector_store: {} chunk(s) for {source_path} (model={})",
        chunks.len(),
        model
    );
}

/// Insert every chunk of one file as a single LanceDB batch (CB91): one
/// fragment and one version per file instead of one per chunk. All embedding
/// validation happens before the write, so the batch is all-or-nothing;
/// returns `(inserted, failed)` counts.
pub(super) async fn insert_chunks_into_store(
    store: &VectorStore,
    source_path: &str,
    chunks: &[VectorChunk],
) -> (usize, usize) {
    if chunks.is_empty() {
        return (0, 0);
    }
    match store.insert_chunks(chunks).await {
        Ok(()) => (chunks.len(), 0),
        Err(error) => {
            tracing::error!(
                "RAG insert failed for {source_path} ({} chunk(s)): {error:#}",
                chunks.len()
            );
            (0, chunks.len())
        }
    }
}

pub(super) async fn purge_vector_chunks(
    data_dir: &Path,
    source_path: &str,
    db: Option<&crate::db::Database>,
) {
    let config = crate::domain::canopy_config::CanopyConfig::load(data_dir);
    let Some(store) = open_vector_store(&config, db.unwrap()).await else {
        trace_purge_store_unavailable(source_path);
        return;
    };

    if let Err(error) = store.delete_by_path(source_path).await {
        trace_purge_delete_failed(source_path, &error);
        return;
    }

    tracing::info!("RAG purge: removed chunks for '{}'", source_path);
    if let Some(db) = db {
        let _ = db.log_rag_event(
            source_path,
            "deleted",
            Some("file removed — watcher triggered chunk purge"),
            chrono::Utc::now().timestamp(),
        );
        refresh_rag_snapshot(db, data_dir).await;
    }
}

/// WARN that the vector store could not be opened, so this purge can only
/// leave the file's chunks orphaned.
pub(super) fn trace_purge_store_unavailable(source_path: &str) {
    tracing::warn!(
        "RAG purge: vector store unavailable — chunks for '{}' may be orphaned",
        source_path
    );
}

/// WARN that a file's chunks could not be removed from the store.
pub(super) fn trace_purge_delete_failed(source_path: &str, error: &anyhow::Error) {
    tracing::warn!(
        "RAG purge: failed to remove chunks for '{}': {error:#}",
        source_path
    );
}

/// Record a `"skipped_oversize"` ledger event for a file that exceeds the
/// configured indexing limit (`CanopyConfig::rag_max_file_bytes`), unless the
/// latest recorded event for that path is already a `"skipped_oversize"` for
/// the same size — the file is rescanned
/// on every startup and watcher pass, so without this check an unchanged
/// oversize file would spam a duplicate row every time.
pub(super) fn record_oversize_skip(db: &Database, source_path: &str, size_bytes: u64) {
    let detail = size_bytes.to_string();
    let already_recorded = db
        .rag_events_for_file(source_path)
        .ok()
        .and_then(|events| events.into_iter().next())
        .is_some_and(|e| {
            e.event_type == "skipped_oversize" && e.detail.as_deref() == Some(detail.as_str())
        });
    if already_recorded {
        return;
    }
    let _ = db.log_rag_event(
        source_path,
        "skipped_oversize",
        Some(&detail),
        chrono::Utc::now().timestamp(),
    );
}

pub(super) async fn refresh_rag_snapshot(db: &Database, data_dir: &Path) {
    let config = crate::domain::canopy_config::CanopyConfig::load(data_dir);
    let Some(store) = open_vector_store(&config, db).await else {
        let _ = db.set_state("rag_total_chunks", "0");
        let _ = db.set_state("rag_indexed_files", "0");
        return;
    };

    let total_chunks = store.count_chunks().await.unwrap_or(0);
    let indexed_files = store.count_unique_paths().await.unwrap_or(0);
    let _ = db.set_state("rag_total_chunks", &total_chunks.to_string());
    let _ = db.set_state("rag_indexed_files", &indexed_files.to_string());
}

pub(super) fn extract_file_content(path: &Path, lang: &str) -> anyhow::Result<String> {
    if lang == "text" && path.extension().and_then(|e| e.to_str()) == Some("pdf") {
        extract_pdf_text(path)
    } else {
        Ok(std::fs::read_to_string(path)?)
    }
}

/// Build a short directory annotation for the indexing summary notification.
/// If all indexed files share the same parent directory, returns " in <dir>".
/// If they span multiple directories, returns " across <n> dirs".
/// Returns an empty string when the list is empty.
pub(super) fn indexing_dir_summary(paths: &[String]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let dirs: std::collections::HashSet<String> = paths
        .iter()
        .filter_map(|p| {
            std::path::Path::new(p)
                .parent()
                .map(|d| d.to_string_lossy().to_string())
        })
        .collect();
    match dirs.len() {
        0 => String::new(),
        1 => {
            let dir = dirs.into_iter().next().unwrap_or_default();
            let leaf = dir.rsplit('/').next().unwrap_or("?");
            format!(" in {leaf}")
        }
        n => format!(" across {n} dirs"),
    }
}

/// Gate: at most one corruption-driven purge per process lifetime.
pub(super) static LANCEDB_PURGE_DONE: AtomicBool = AtomicBool::new(false);

pub(super) async fn open_vector_store(
    config: &crate::domain::canopy_config::CanopyConfig,
    db: &Database,
) -> Option<VectorStore> {
    let model = config.embeddings_model.trim();
    if model.is_empty() {
        return None;
    }

    let dimensions = model_dimensions_for_store(model)?;
    tracing::info!("RAG open_vector_store: model='{model}' dimensions={dimensions}");

    match VectorStore::new(dimensions, Some(config.rag_vector_cache_entries)).await {
        Ok(store) => {
            tracing::info!("RAG open_vector_store: store opened OK");
            Some(store)
        }
        Err(error) => recover_from_corrupted_store(config, db, dimensions, error).await,
    }
}

/// Resolve the configured model's embedding dimensions, or `None` — after
/// logging the same warning the open path always emitted — when the model is
/// unknown to this build.
pub(super) fn model_dimensions_for_store(model: &str) -> Option<usize> {
    match model_dimensions(model) {
        Ok(dimensions) => Some(dimensions),
        Err(error) => {
            tracing::warn!("RAG vector store unavailable for model {model}: {error:#}");
            None
        }
    }
}

/// Handle a failed `VectorStore::new`: purge the corrupted directory and
/// retry exactly once, gated by [`LANCEDB_PURGE_DONE`] so at most one
/// corruption-driven purge happens per process lifetime.
pub(super) async fn recover_from_corrupted_store(
    config: &crate::domain::canopy_config::CanopyConfig,
    db: &Database,
    dimensions: usize,
    error: anyhow::Error,
) -> Option<VectorStore> {
    // If the store genuinely fails to open AND we haven't purged yet this
    // process, remove the corrupted directory and retry exactly once.
    // A missing directory is NOT an error — VectorStore::new creates it
    // fresh — so any Err means the store is genuinely corrupted.
    if LANCEDB_PURGE_DONE.swap(true, Ordering::Relaxed) {
        trace_purge_already_used(&error);
        return None;
    }
    purge_and_retry_store(config, db, dimensions, &error).await
}

/// Purge the corrupted LanceDB directory once and retry the open; `None`
/// when the retry also fails.
pub(super) async fn purge_and_retry_store(
    config: &crate::domain::canopy_config::CanopyConfig,
    db: &Database,
    dimensions: usize,
    error: &anyhow::Error,
) -> Option<VectorStore> {
    tracing::warn!(
        "RAG vector store is corrupted ({error:#}); \
         purging directory and retrying once"
    );
    wipe_corrupt_store(db).await;
    match VectorStore::new(dimensions, Some(config.rag_vector_cache_entries)).await {
        Ok(store) => {
            trace_recovered_after_purge();
            Some(store)
        }
        Err(retry_err) => {
            trace_retry_after_purge_failed(&retry_err);
            None
        }
    }
}

/// Remove the corrupted LanceDB directory, warning (never failing) when the
/// wipe itself cannot complete.
pub(super) async fn wipe_corrupt_store(db: &Database) {
    if let Err(e) = wipe_lancedb(db, "corrupt store recovery").await {
        tracing::warn!("RAG: failed to purge corrupt store: {e:#}");
    }
}

/// WARN that the one corruption-driven retry succeeded.
pub(super) fn trace_recovered_after_purge() {
    tracing::warn!("RAG open_vector_store: recovered after purge");
}

/// WARN that the open still fails after the one corruption-driven retry.
pub(super) fn trace_retry_after_purge_failed(retry_err: &anyhow::Error) {
    tracing::warn!("RAG open_vector_store: retry after purge also failed: {retry_err:#}");
}

/// WARN that the store failed to open after the one purge already used.
pub(super) fn trace_purge_already_used(error: &anyhow::Error) {
    tracing::warn!("RAG vector store open error (purge already used): {error:#}");
}

/// Decide whether the vector store has silently lost data relative to the
/// ledger — the signature of a hard crash that destroyed the LanceDB
/// directory while the SQLite ledger (`rag_file_events`) survived. A store
/// that failed to open (`None`) is never treated as loss: per the A4 fix, a
/// transient open error on a known-existing table must not be conflated with
/// "the store is empty".
pub(super) fn store_loss_detected(
    ledger_indexed_count: usize,
    store_chunk_count: Option<i64>,
) -> bool {
    ledger_indexed_count > 0 && store_chunk_count == Some(0)
}

/// Delete the entire LanceDB directory so the next `open_vector_store` starts fresh.
/// Used when the embeddings model is changed so stale vectors don't pollute results.
pub async fn wipe_lancedb(db: &Database, reason: &str) -> anyhow::Result<()> {
    let lancedb_path = VectorStore::default_lancedb_path()?;
    wipe_lancedb_at(&lancedb_path, db, reason).await
}

/// Core of `wipe_lancedb`, taking the LanceDB directory as a plain path so
/// tests can point it at a scratch directory instead of the real
/// home-directory-rooted store.
pub(crate) async fn wipe_lancedb_at(
    lancedb_path: &Path,
    db: &Database,
    reason: &str,
) -> anyhow::Result<()> {
    if lancedb_path.exists() {
        tokio::fs::remove_dir_all(lancedb_path).await.map_err(|e| {
            anyhow::anyhow!("Failed to wipe LanceDB at {}: {e}", lancedb_path.display())
        })?;
        tracing::info!(
            "RAG: wiped LanceDB at {} ({reason})",
            lancedb_path.display()
        );
    }
    // Clear rag_file_events so the report starts clean for the new model.
    let _ = db.clear_rag_file_events();
    let _ = db.set_state("rag_total_chunks", "0");
    let _ = db.set_state("rag_indexed_files", "0");
    Ok(())
}
