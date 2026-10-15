use super::*;

/// Everything the RAG watcher's notify callback needs; moved by value into the
/// closure exactly the clones `start_personal_watcher` already made
/// (rt/db/queue/notify/roots/patterns/data_dir).
pub(super) struct RagWatchContext {
    pub(super) rt: tokio::runtime::Handle,
    pub(super) db: Arc<Database>,
    pub(super) queue: Arc<Mutex<Queue>>,
    pub(super) notify: Arc<Notify>,
    pub(super) roots: Vec<PathBuf>,
    pub(super) patterns: Vec<String>,
    pub(super) data_dir: PathBuf,
}

/// Watch every personal RAG root recursively, logging each outcome; returns
/// how many roots were successfully watched.
pub(super) fn watch_personal_roots(
    watcher: &mut RecommendedWatcher,
    personal_roots: &[PathBuf],
) -> usize {
    let mut watched = 0usize;
    for root in personal_roots {
        if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
            tracing::warn!("Personal RAG watcher could not watch {:?}: {e}", root);
        } else {
            tracing::info!("Personal RAG watcher active on {:?}", root);
            watched += 1;
        }
    }
    watched
}

/// Route one watcher event: queue indexable files that sit inside a watched
/// root and aren't ragignored, and purge vector chunks for removed files.
pub(super) fn handle_rag_watcher_event(ctx: &RagWatchContext, event: Event) {
    let Event { paths, kind, .. } = event;
    for path in &paths {
        let matching_root = ctx.roots.iter().find(|r| path.starts_with(r));
        let Some(root) = matching_root else { continue };
        if crate::rag::ragignore::is_ignored(path, root, &ctx.patterns) {
            continue;
        }
        let path_str = path.to_string_lossy().to_string();
        match kind {
            EventKind::Create(_) | EventKind::Modify(_) if detect_lang(&path_str).is_some() => {
                spawn_rag_index_enqueue(ctx, &path_str, kind);
            }
            EventKind::Remove(_) => {
                spawn_rag_chunk_purge(ctx, &path_str);
            }
            _ => {}
        }
    }
}

/// Spawn the queue-push + DB-enqueue + notify for a freshly changed file.
pub(super) fn spawn_rag_index_enqueue(ctx: &RagWatchContext, path_str: &str, kind: EventKind) {
    tracing::info!(
        "RAG watcher: queuing '{}' for indexing ({:?})",
        path_str,
        kind
    );
    let q = Arc::clone(&ctx.queue);
    let n = Arc::clone(&ctx.notify);
    let p = path_str.to_string();
    let db2 = Arc::clone(&ctx.db);
    ctx.rt.spawn(async move {
        let now = chrono::Utc::now().timestamp();
        let ok = {
            let mut lock = q.lock().await;
            lock.push(&p)
        };
        if ok {
            let _ = db2.enqueue_rag_item(&p, now);
            n.notify_one();
        }
    });
}

/// Spawn the vector-chunk purge for a file the watcher saw removed.
pub(super) fn spawn_rag_chunk_purge(ctx: &RagWatchContext, path_str: &str) {
    tracing::info!("RAG watcher: '{}' removed — purging chunks", path_str);
    let data_dir2 = ctx.data_dir.clone();
    let db3 = Arc::clone(&ctx.db);
    let p = path_str.to_string();
    ctx.rt.spawn(async move {
        purge_vector_chunks(&data_dir2, &p, Some(&db3)).await;
    });
}

/// Why an indexed path's chunks are orphans: `(missing_on_disk, outside_roots)`,
/// or `None` when the file still exists inside a configured root.
pub(super) fn orphan_purge_reason(path: &Path, personal_roots: &[PathBuf]) -> Option<(bool, bool)> {
    // Case 1: file no longer exists on disk.
    let missing_on_disk = !path.exists();

    // Case 2: file exists but is outside every configured root
    // (i.e. the user changed the RAG root via `canopy setup`).
    let outside_roots =
        !personal_roots.is_empty() && !personal_roots.iter().any(|root| path.starts_with(root));

    (missing_on_disk || outside_roots).then_some((missing_on_disk, outside_roots))
}

/// An indexing failure is permanent when its message matches a known-fatal
/// pattern or the file has burned through `MAX_RAG_ATTEMPTS` total attempts.
pub(super) fn permanent_index_failure(error_detail: &str, prior_errors: i64) -> bool {
    is_permanent_rag_error(error_detail) || prior_errors + 1 >= MAX_RAG_ATTEMPTS
}

/// Extract a file's indexable content, returning `Ok(None)` (with the same
/// warning the index path always emitted) when nothing usable was extracted.
pub(super) fn read_indexable_content(
    source_path: &str,
    path: &Path,
    lang: &str,
) -> anyhow::Result<Option<String>> {
    let content = extract_file_content(path, lang)?;
    if content.trim().is_empty() {
        tracing::warn!("RAG: skipping {source_path} — content is empty after extraction");
        return Ok(None);
    }
    tracing::debug!(
        "RAG index_file: {source_path} — extracted {} bytes (lang={lang})",
        content.len()
    );
    Ok(Some(content))
}
