//! Personal-RAG startup: root preparation, model/chunking migrations, queue
//! recovery and the initial scan. Split out of `server.rs` to keep every
//! production file within the file-size gate.

use std::sync::Arc;

use crate::application::ports::StateRepository;
use crate::rag::ingestion::IngestionManager;

/// Scan the personal RAG root for existing files, enqueue them, and start the watcher.
/// Version of the chunking algorithm; bump to force a full re-index.
/// v2: merge lexically similar neighbors (size-capped) instead of dissimilar ones.
pub(super) const RAG_CHUNKING_VERSION: &str = "v2";

pub(super) async fn startup_personal_rag(
    ingestion: Arc<IngestionManager>,
    data_dir: &std::path::Path,
) {
    let config = crate::domain::canopy_config::CanopyConfig::load(data_dir);
    let personal_roots: Vec<std::path::PathBuf> = config
        .rag_personal_dirs
        .iter()
        .map(std::path::PathBuf::from)
        .collect();

    if !prepare_personal_rag_roots(&ingestion, &personal_roots).await {
        return;
    }

    match crate::rag::vector_store::VectorStore::default_lancedb_path() {
        Ok(lancedb_path) => {
            migrate_personal_rag_model(&ingestion, &config, &lancedb_path).await;
            migrate_personal_rag_chunking(&ingestion, &lancedb_path).await;
        }
        Err(e) => tracing::error!("startup_personal_rag: cannot resolve LanceDB path: {e:#}"),
    }

    // ── Ledger vs vector-store reconciliation ───────────────────────────
    // A hard crash (e.g. a WSL kill) can destroy the LanceDB directory while
    // the SQLite ledger (`rag_file_events`) survives on disk. Pure store
    // deletion doesn't go through the corrupt-store recovery path (which
    // already clears the ledger), so without this check the daemon would
    // trust the stale ledger, queue nothing, and the RAG index would stay
    // silently empty forever.
    ingestion.reconcile_ledger_with_store().await;

    recover_stale_processing_items(&ingestion).await;

    // Files that have permanently failed indexing should not be silently
    // re-queued by the automatic startup rescan/reload; an explicit user
    // Modify/Create event via the filesystem watcher still gives them a
    // fresh chance.
    let failed = ingestion
        .db()
        .permanently_failed_rag_files()
        .unwrap_or_default();

    reload_personal_rag_queue(&ingestion, &failed).await;
    queue_existing_personal_rag_files(&ingestion, data_dir, &personal_roots, &failed).await;

    finish_personal_rag_startup(&ingestion, &personal_roots).await;
}

/// Creates each configured root (the scan and the watcher below both need
/// them on disk — a `create_dir_all` failure is only a warning, the scan
/// then simply sees nothing there). When no personal RAG directory is
/// configured at all, still refreshes the persisted snapshot so TUI
/// counters reset to empty, and reports there is nothing else to do.
pub(super) async fn prepare_personal_rag_roots(
    ingestion: &IngestionManager,
    personal_roots: &[std::path::PathBuf],
) -> bool {
    if personal_roots.is_empty() {
        tracing::info!("startup_personal_rag: no directories configured, skipping");
        // Still initialize snapshot even if no RAG dirs configured
        ingestion.refresh_snapshot().await;
        return false;
    }

    for root in personal_roots {
        if let Err(e) = std::fs::create_dir_all(root) {
            tracing::warn!("Could not create personal RAG dir {:?}: {e}", root);
        }
    }
    true
}

/// ── Model-change detection ─────────────────────────────────────────────
/// If the configured embeddings model differs from the last run, wipe the
/// vector store and clear the queue so everything is re-indexed with the
/// new model.  This covers both dimension changes (already handled inside
/// `open_at`) *and* same-dimension model swaps where old vectors would give
/// wrong results. The new model is persisted either way.
pub(super) async fn migrate_personal_rag_model(
    ingestion: &IngestionManager,
    config: &crate::domain::canopy_config::CanopyConfig,
    lancedb_path: &std::path::Path,
) {
    let current_model = config.embeddings_model.trim().to_string();
    if current_model.is_empty() {
        return;
    }
    let last_model = ingestion
        .db()
        .get_state("rag_last_model")
        .ok()
        .flatten()
        .unwrap_or_default();

    if !last_model.is_empty() && last_model != current_model {
        tracing::warn!(
            "startup_personal_rag: embeddings model changed '{}' → '{}' \
             — wiping vector store and clearing queue for full re-index",
            last_model,
            current_model
        );
        if let Err(e) =
            crate::rag::ingestion::wipe_lancedb_at(lancedb_path, ingestion.db(), "model change")
                .await
        {
            tracing::error!("startup_personal_rag: LanceDB wipe failed: {e:#}");
        }
        ingestion.clear_queue().await;
    }

    let _ = ingestion.db().set_state("rag_last_model", &current_model);
}

/// ── Chunking-algorithm change detection ─────────────────────────────────
/// Bump RAG_CHUNKING_VERSION whenever chunk boundaries change (e.g. the
/// v2 switch from merge-dissimilar to merge-similar semantics). Existing
/// chunks were produced by the old algorithm, so wipe and re-index.
pub(super) async fn migrate_personal_rag_chunking(
    ingestion: &IngestionManager,
    lancedb_path: &std::path::Path,
) {
    let last_chunking = ingestion
        .db()
        .get_state("rag_chunking_version")
        .ok()
        .flatten()
        .unwrap_or_default();
    if last_chunking == RAG_CHUNKING_VERSION {
        return;
    }
    tracing::warn!(
        "startup_personal_rag: chunking algorithm changed '{}' → '{}' \
         — wiping vector store and clearing queue for full re-index",
        if last_chunking.is_empty() {
            "v1"
        } else {
            &last_chunking
        },
        RAG_CHUNKING_VERSION
    );
    if let Err(e) = crate::rag::ingestion::wipe_lancedb_at(
        lancedb_path,
        ingestion.db(),
        "chunking version change",
    )
    .await
    {
        tracing::error!("startup_personal_rag: LanceDB wipe failed: {e:#}");
    }
    ingestion.clear_queue().await;
    let _ = ingestion
        .db()
        .set_state("rag_chunking_version", RAG_CHUNKING_VERSION);
}

/// Returns any queue item a previous session was mid-processing on to the
/// pending state, so a crash mid-index can't leave it stuck in
/// `processing` forever.
async fn recover_stale_processing_items(ingestion: &IngestionManager) {
    let recovered = ingestion
        .db()
        .requeue_processing_rag_items(chrono::Utc::now().timestamp())
        .unwrap_or(0);
    if recovered > 0 {
        tracing::warn!(
            "startup_personal_rag: recovered {recovered} stale processing queue item(s) after previous crash"
        );
    }
}

/// Reloads any items already in the DB queue from a previous session,
/// skipping the permanently-failed files.
pub(super) async fn reload_personal_rag_queue(
    ingestion: &IngestionManager,
    failed: &std::collections::HashSet<String>,
) {
    let Ok(pending) = ingestion.db_pending_queue() else {
        return;
    };
    let mut reloaded = 0usize;
    for path in &pending {
        if failed.contains(path) {
            continue;
        }
        ingestion.enqueue(path).await;
        reloaded += 1;
    }
    if reloaded > 0 {
        tracing::info!("startup_personal_rag: reloaded {reloaded} pending queue items");
    }
}

/// Scan existing files across all personal roots.
/// Skip files that are already indexed and haven't been modified since,
/// and skip the permanently-failed files (see
/// [`reload_personal_rag_queue`]) — only an explicit watcher event gives
/// those a fresh chance.
pub(super) async fn queue_existing_personal_rag_files(
    ingestion: &IngestionManager,
    data_dir: &std::path::Path,
    personal_roots: &[std::path::PathBuf],
    failed: &std::collections::HashSet<String>,
) {
    let already_indexed = ingestion
        .db()
        .indexed_files_timestamps()
        .unwrap_or_default();
    let patterns = crate::rag::ragignore::load_patterns(data_dir);
    let mut queued = 0usize;
    for root in personal_roots {
        let walker = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file());

        for entry in walker {
            let path = entry.path();
            if crate::rag::ragignore::is_ignored(path, root, &patterns) {
                continue;
            }
            let path_str = path.to_string_lossy().to_string();
            if crate::rag::chunker::detect_lang(&path_str).is_none() {
                continue;
            }
            // Skip if indexed and file hasn't changed since.
            if let Some(&indexed_at) = already_indexed.get(&path_str) {
                let mtime = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(i64::MAX);
                if mtime <= indexed_at {
                    continue;
                }
            }
            if failed.contains(&path_str) {
                continue;
            }
            ingestion.enqueue(&path_str).await;
            queued += 1;
        }
    }
    if queued > 0 {
        tracing::info!("startup_personal_rag: queued {queued} existing file(s) for indexing");
    }
}

/// The startup tail, in order: reconcile orphan chunks from files deleted
/// while the daemon was offline, keep a persisted snapshot so TUI reads
/// counters without querying LanceDB, then start the filesystem watcher for
/// live updates.
async fn finish_personal_rag_startup(
    ingestion: &Arc<IngestionManager>,
    personal_roots: &[std::path::PathBuf],
) {
    ingestion.reconcile_orphan_chunks().await;
    ingestion.refresh_snapshot().await;
    Arc::clone(ingestion).start_personal_watcher(personal_roots);
}
