use super::*;

/// Human-readable name for display in doctor messages.
impl crate::rag::embedding_client::EmbeddingProvider {
    pub fn name(&self) -> &'static str {
        match self {
            crate::rag::embedding_client::EmbeddingProvider::OpenAi => "OpenAI",
            crate::rag::embedding_client::EmbeddingProvider::Gemini => "Gemini",
            crate::rag::embedding_client::EmbeddingProvider::Local => "Local",
        }
    }
}

/// CB19: classify the embeddings_model config before doctor prints or
/// walks API-key / ONNX paths. Pure so unit tests can assert the
/// "not configured" vs "configured but unrunnable" distinction without
/// spinning up a full `run_doctor` (those black-box tests stay `#[ignore]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EmbeddingsConfigDiagnosis {
    /// Empty model string — nothing is configured to judge.
    NotConfigured,
    /// A provider is named in config but this binary cannot serve it.
    ProviderUnavailable { provider_name: &'static str },
    /// Model string is non-empty and either maps to an available provider
    /// or is unrecognized (caller continues into key / support checks).
    Configured,
}

pub(crate) fn diagnose_embeddings_config(model: &str) -> EmbeddingsConfigDiagnosis {
    if model.is_empty() {
        return EmbeddingsConfigDiagnosis::NotConfigured;
    }
    match crate::rag::embedding_client::provider_for_model(model) {
        Some(p) if !crate::rag::embedding_client::provider_available(p) => {
            EmbeddingsConfigDiagnosis::ProviderUnavailable {
                provider_name: p.name(),
            }
        }
        Some(_) | None => EmbeddingsConfigDiagnosis::Configured,
    }
}

/// Doctor's embeddings section: model, provider availability, API key
/// presence, and (for the local provider) model acquisition status.
///
/// declaration: names the configured model string; whether it's actually
/// usable is what the branches below (API key presence, provider
/// capability) exist to verify. A config string is not a capability, so
/// this is informational, not a tick.
pub(super) fn report_embeddings(
    config: &crate::domain::canopy_config::CanopyConfig,
    db_path: &PathBuf,
    canopy_dir: &Path,
    issues: &mut Vec<String>,
) {
    match diagnose_embeddings_config(&config.embeddings_model) {
        EmbeddingsConfigDiagnosis::NotConfigured => {
            println!(" \x1b[31m✗\x1b[0m Embeddings model not configured (run 'canopy setup')");
            issues.push("Configure embeddings model via 'canopy setup'".to_string());
        }
        EmbeddingsConfigDiagnosis::ProviderUnavailable { provider_name } => {
            println!(
                " \x1b[90m–\x1b[0m Embeddings model: {}",
                config.embeddings_model
            );
            println!(" \x1b[31m✗\x1b[0m Provider '{provider_name}' is not available in this build");
            issues.push(format!(
                "The configured provider ({provider_name}) requires a build with 'local-embeddings'. \
                 Either reinstall with that feature, or re-run setup and choose a cloud provider."
            ));
            // Capability gap is the verdict — skip API-key / ONNX checks that
            // would only confuse ("no API key required" for a provider this
            // binary cannot run).
        }
        EmbeddingsConfigDiagnosis::Configured => {
            report_configured_embeddings(config, db_path, canopy_dir, issues);
        }
    }
}

/// The `Configured` half of the embeddings section: provider API-key
/// presence (and, for the local provider, ONNX runtime readiness and
/// download/preparation status read from a database that must already
/// exist — doctor never creates it).
pub(super) fn report_configured_embeddings(
    config: &crate::domain::canopy_config::CanopyConfig,
    db_path: &PathBuf,
    canopy_dir: &Path,
    issues: &mut Vec<String>,
) {
    println!(
        " \x1b[90m–\x1b[0m Embeddings model: {}",
        config.embeddings_model
    );

    let provider = crate::rag::embedding_client::provider_for_model(&config.embeddings_model);

    // Check that the required API key is present (local models need none).
    let api_key_info = match provider {
        Some(crate::rag::embedding_client::EmbeddingProvider::OpenAi) => {
            Some(("OPENAI_API_KEY", std::env::var("OPENAI_API_KEY").is_ok()))
        }
        Some(crate::rag::embedding_client::EmbeddingProvider::Gemini) => {
            Some(("GEMINI_API_KEY", std::env::var("GEMINI_API_KEY").is_ok()))
        }
        Some(crate::rag::embedding_client::EmbeddingProvider::Local) => {
            // Configured + Local means this build can run it
            // (ProviderUnavailable would have caught the other case).
            #[cfg(all(feature = "local-embeddings", target_os = "linux"))]
            match crate::rag::ort_runtime::ort_runtime_path() {
                Some(path) => {
                    success_nested(format!("ONNX Runtime loaded from {}", path.display()));
                }
                None => {
                    println!(
                        "  \x1b[33m⚠\x1b[0m ONNX Runtime not yet downloaded (will be fetched on first local RAG use)"
                    );
                }
            }

            // Only open the DB if it already exists — doctor is a
            // passive diagnostic and must not create the database
            // as a side effect on a machine that's never run
            // setup.
            let acquisition = db_path
                .exists()
                .then(|| Database::new_safe(db_path, canopy_dir).ok())
                .flatten()
                .and_then(|db| {
                    crate::rag::status::read_acquisition_state(&db, &config.embeddings_model)
                });
            match acquisition {
                Some(crate::rag::status::AcquisitionState::Downloading { started_at }) => {
                    println!(
                        " \x1b[33m⬇\x1b[0m Local model downloading ({}s so far)",
                        crate::rag::status::elapsed_secs(started_at)
                    );
                }
                Some(crate::rag::status::AcquisitionState::Preparing { started_at }) => {
                    println!(
                        " \x1b[33m⚙\x1b[0m Local model preparing ({}s so far)",
                        crate::rag::status::elapsed_secs(started_at)
                    );
                }
                Some(crate::rag::status::AcquisitionState::Failed { reason }) => {
                    println!(" \x1b[31m✗\x1b[0m Local model download failed — {reason}");
                    issues.push(format!(
                        "Local embedding model download failed: {reason}. \
                         Run 'canopy rag model retry' to try again."
                    ));
                }
                None => {
                    success("Local model — no API key required");
                }
            }
            None
        }
        None => {
            // provider_for_model returned None - unknown model
            println!(
                " \x1b[31m✗\x1b[0m Model '{}' is not supported. Run 'canopy setup' to pick a compatible model.",
                config.embeddings_model
            );
            issues.push("Run 'canopy setup' and select a supported embedding model".to_string());
            None
        }
    };

    // declaration: an env var being set doesn't prove it's a valid
    // credential — only a real API call could confirm that, which is
    // too expensive for an interactive check. This reports presence,
    // not validity.
    if let Some((key_var, present)) = api_key_info {
        if present {
            success(format!("API key {key_var} is set"));
        } else {
            println!(" \x1b[31m✗\x1b[0m {key_var} is NOT set — indexing will fail silently");
            issues.push("Export the required API key before starting the daemon".to_string());
        }
    }
}

/// Doctor's indexing size-limit section.
///
/// Report the *configured* value's validity explicitly —
/// `rag_max_file_bytes()` silently falls back to the default for an
/// out-of-range config.toml value (indexing must never honor an
/// unbounded/huge cap), but that fallback must not read as silently green
/// here.
/// capability: `validate_rag_max_file_mb` re-derives whether the
/// configured value is actually the one indexing will use, rather than
/// trusting the config.toml number at face value — that's what catches
/// the silent-fallback case flagged below.
pub(super) fn report_rag_size_limit(
    config: &crate::domain::canopy_config::CanopyConfig,
    issues: &mut Vec<String>,
) {
    match crate::domain::canopy_config::validate_rag_max_file_mb(config.rag_max_file_mb) {
        Ok(()) => {
            success(format!(
                "Indexing size limit: {} MB per file",
                config.rag_max_file_mb
            ));
        }
        Err(reason) => {
            let effective_mb = config.rag_max_file_bytes() / (1024 * 1024);
            println!(
                " \x1b[31m✗\x1b[0m Indexing size limit: {} MB is invalid ({reason}) — \
                 falling back to {effective_mb} MB",
                config.rag_max_file_mb
            );
            issues.push(format!(
                "config.toml's rag_max_file_mb ({}) is invalid: {reason}. Run 'canopy setup' or fix config.toml.",
                config.rag_max_file_mb
            ));
        }
    }
}

/// Doctor's personal-RAG-directories section: existence per root, then the
/// shared corpus scan (indexable count plus size exclusions).
pub(super) fn report_rag_directories(
    canopy_dir: &Path,
    config: &crate::domain::canopy_config::CanopyConfig,
    issues: &mut Vec<String>,
) {
    if config.rag_personal_dirs.is_empty() {
        println!(" \x1b[33m⚠\x1b[0m No personal RAG directories configured");
        issues.push("Add personal RAG directories via 'canopy setup'".to_string());
        return;
    }

    let max_bytes = config.rag_max_file_bytes();
    let cap_mb = max_bytes as f64 / (1024.0 * 1024.0);

    // Per-directory existence is reported line-by-line; the size
    // accounting below is delegated to the shared scan so doctor and
    // `canopy rag report` describe the same corpus with the same
    // ragignore/extension rules.
    for dir in &config.rag_personal_dirs {
        if std::path::Path::new(dir).exists() {
            success(format!("RAG dir: {dir}"));
        } else {
            println!(" \x1b[31m✗\x1b[0m RAG dir missing: {dir}");
            issues.push("Personal RAG directory not found on disk".to_string());
        }
    }

    // capability: walks the configured roots with the ingestion filters
    // and counts by filesystem metadata only — never opening a file.
    match crate::rag::size_report::scan(canopy_dir, &config.rag_personal_dirs, max_bytes) {
        Ok(scan) => {
            let total_files = scan.indexable_files.len();
            let oversize_files = scan.oversize_files.len();
            if total_files == 0 {
                println!(
                    " \x1b[33m⚠\x1b[0m No indexable files found (.md, .mdx, .pdf) in RAG directories"
                );
            } else {
                success(format!("RAG corpus: {total_files} indexable file(s)"));
            }
            // Printed even at zero — a silent exclusion is exactly the bug.
            let icon = if oversize_files > 0 {
                "\x1b[33m⚠\x1b[0m"
            } else {
                "\x1b[90m–\x1b[0m"
            };
            println!(
                " {icon} {}",
                crate::rag::size_report::exclusion_summary(oversize_files, max_bytes)
            );
            if oversize_files > 0 {
                issues.push(format!(
                    "{oversize_files} configured file(s) exceed the {cap_mb:.0} MB indexing \
                     limit and are skipped — see 'canopy rag report', or raise \
                     rag_max_file_mb in config.toml"
                ));
            }
        }
        Err(err) => {
            println!(
                " \x1b[33m⚠\x1b[0m Could not scan RAG directories for size exclusions: {err:#}"
            );
            issues.push(
                "Failed to scan personal RAG directories for size exclusions — resolve the \
                 error above so the exclusion count is trustworthy"
                    .to_string(),
            );
        }
    }
}

/// Doctor's ragignore line.
///
/// capability: presence of the file is exactly the claim made — ragignore
/// has no separate "does it work" question beyond existing on disk.
pub(super) fn report_ragignore(canopy_dir: &Path) {
    let ragignore_path = canopy_dir.join("ragignore");
    if ragignore_path.exists() {
        success(format!("ragignore: {}", ragignore_path.display()));
    } else {
        println!(" \x1b[90m–\x1b[0m ragignore not found (optional — create ~/.canopy/ragignore to exclude files)");
    }
}

/// Doctor's vector-store section: open the store, count chunks, count
/// unique indexed paths, and compare them with what's on disk.
///
/// capability: this must open the store — a corrupt LanceDB manifest,
/// for instance, previously left the directory on disk while every
/// query against it failed, and the old check (`lancedb_path.exists()`)
/// printed the same green tick for that as for a healthy store. Opening
/// it is also what's needed to read the chunk count below, so the two
/// checks that used to live in separate sections are merged into one
/// capability probe.
pub(super) async fn report_vector_store(
    config: &crate::domain::canopy_config::CanopyConfig,
    issues: &mut Vec<String>,
) {
    let lancedb_path = match crate::rag::vector_store::VectorStore::default_lancedb_path() {
        Ok(p) => p,
        Err(_) => {
            println!(" \x1b[33m⚠\x1b[0m Could not determine LanceDB path");
            issues.push("Home directory not found".to_string());
            dirs::home_dir()
                .unwrap_or_default()
                .join(".canopy/rag/vectors.lancedb")
        }
    };

    // Opening the store requires knowing its embedding dimensionality,
    // which only a resolvable embeddings model tells us.
    let known_dimensions = (!config.embeddings_model.is_empty())
        .then(|| crate::rag::embedding_client::model_dimensions(&config.embeddings_model).ok())
        .flatten();

    match known_dimensions {
        None => {
            // declaration: no embeddings model to open the store with, so
            // this can only report whether something is present on disk —
            // never a tick, since presence was never verified to work.
            if lancedb_path.exists() {
                println!(
                    " \x1b[90m–\x1b[0m Vector store present at {} (cannot verify without a configured embeddings model)",
                    lancedb_path.display()
                );
            } else {
                println!(
                    " \x1b[90m–\x1b[0m Vector store not yet created (will be created on first indexing)"
                );
            }
        }
        Some(dimensions) => match crate::rag::vector_store::VectorStore::new(
            dimensions,
            Some(config.rag_vector_cache_entries),
        )
        .await
        {
            Err(e) => {
                println!(" \x1b[31m✗\x1b[0m Could not open LanceDB: {e}");
                issues.push(
                    "LanceDB open error — check if the embeddings model is supported".to_string(),
                );
            }
            Ok(store) => match store.count_chunks().await {
                Err(_) => {
                    println!(" \x1b[90m–\x1b[0m Could not read chunk count from LanceDB");
                }
                Ok(total) => {
                    success(format!("Vector store: {}", lancedb_path.display()));
                    if total > 0 {
                        success(format!("Indexed chunks: {total}"));
                        if let Ok(unique) = store.count_unique_paths().await {
                            success(format!("Indexed files: {unique}"));

                            // Surface mismatch between files on disk and indexed files.
                            let disk_files = count_indexable_disk_files(config);

                            if disk_files > 0 && (unique as usize) < disk_files {
                                let unindexed = disk_files - unique as usize;
                                println!(
                                    " \x1b[33m⚠\x1b[0m {disk_files} indexable file(s) on disk but only {unique} indexed — \
                                     {unindexed} not yet indexed"
                                );
                                issues.push(format!(
                                    "{unindexed} file(s) are not yet indexed — run 'canopy rag backfill' to index them"
                                ));
                            }
                        }
                    } else {
                        println!(" \x1b[31m✗\x1b[0m No chunks indexed yet");
                        if !config.rag_personal_dirs.is_empty() {
                            let is_local = matches!(
                                crate::rag::embedding_client::provider_for_model(
                                    &config.embeddings_model
                                ),
                                Some(crate::rag::embedding_client::EmbeddingProvider::Local)
                            );
                            issues.push(
                                if is_local {
                                    "RAG directories are configured but nothing is indexed — \
                                     ensure the daemon is running"
                                } else {
                                    "RAG directories are configured but nothing is indexed — \
                                     ensure the daemon is running and the API key env var is set"
                                }
                                .to_string(),
                            );
                        }
                    }
                }
            },
        },
    }
}

/// Indexable files currently on disk across every configured root, counted
/// with the same ingestion filters indexing uses (ragignore-aware walk,
/// indexable extensions only) and reading filesystem metadata only — never
/// opening a file.
pub(super) fn count_indexable_disk_files(
    config: &crate::domain::canopy_config::CanopyConfig,
) -> usize {
    config
        .rag_personal_dirs
        .iter()
        .map(std::path::Path::new)
        .filter(|p| p.exists())
        .flat_map(|p| {
            walkdir::WalkDir::new(p)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_type().is_file()
                        && crate::rag::chunker::detect_lang(&e.path().to_string_lossy()).is_some()
                })
        })
        .count()
}

/// Doctor's SQLite RAG-queue section: how many files are waiting for or
/// undergoing indexing.
pub(super) fn report_rag_queue(db_path: &PathBuf, canopy_dir: &Path) {
    if !db_path.exists() {
        return;
    }
    let Ok(db) = Database::new_safe(db_path, canopy_dir) else {
        return;
    };
    let Ok((queued, processing)) = db.rag_queue_counts() else {
        return;
    };
    if queued == 0 && processing == 0 {
        return;
    }
    if processing > 0 {
        println!(" \x1b[33m⚠\x1b[0m Pending queue: {queued} queued, {processing} indexing");
    } else {
        println!(" \x1b[33m⚠\x1b[0m Pending queue: {queued} file(s) awaiting indexing");
    }
}

/// Doctor's vector-store *health* section (CB91): the daemon's last
/// maintenance verdict, with each reason and its fix command.
///
/// Doctor deliberately does not open the store here: the running daemon owns
/// it, and its own tick is the only process that can truthfully say whether
/// compaction succeeded. This reads the persisted verdict instead.
pub(super) fn report_rag_store_health(
    db_path: &PathBuf,
    canopy_dir: &Path,
    config: &crate::domain::canopy_config::CanopyConfig,
    issues: &mut Vec<String>,
) {
    if config.embeddings_model.trim().is_empty() {
        println!(
            " \x1b[90m–\x1b[0m RAG store health: not applicable (no embeddings model configured)"
        );
        return;
    }

    let never_checked =
        " \x1b[90m–\x1b[0m RAG store health: never checked (daemon maintenance runs at startup)";
    if !db_path.exists() {
        println!("{never_checked}");
        return;
    }
    let Ok(db) = Database::new_safe(db_path, canopy_dir) else {
        println!(" \x1b[33m⚠\x1b[0m RAG store health: could not read the database");
        return;
    };
    let health = db
        .get_state(crate::rag::maintenance::RAG_HEALTH_STATE_KEY)
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<crate::rag::maintenance::RagHealth>(&json).ok());
    let Some(health) = health else {
        println!("{never_checked}");
        return;
    };

    let checked = health.checked_at.format("%Y-%m-%d %H:%M UTC");
    match health.status {
        crate::rag::maintenance::RagHealthStatus::Ok => {
            success(format!("RAG store health: ok (checked {checked})"));
        }
        crate::rag::maintenance::RagHealthStatus::Degraded => {
            println!(" \x1b[33m⚠\x1b[0m RAG store health: degraded (checked {checked})");
            for reason in &health.reasons {
                println!("     {reason}");
                if reason.contains("canopy rag compact") {
                    println!("     fix: canopy rag compact");
                } else if reason.contains("canopy rag auto-index start") {
                    println!("     fix: canopy rag auto-index start");
                }
            }
            issues.push(format!(
                "RAG store health degraded — {}",
                health.reasons.join("; ")
            ));
        }
        crate::rag::maintenance::RagHealthStatus::Broken => {
            println!(" \x1b[31m✗\x1b[0m RAG store health: broken (checked {checked})");
            for reason in &health.reasons {
                println!("     {reason}");
            }
            println!("     fix: canopy rag purge");
            issues.push(format!(
                "RAG store open failed: {}",
                health.reasons.join("; ")
            ));
        }
    }
}
