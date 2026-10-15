//! RAG onboarding for the setup wizard: keep-vs-reconfigure, local model
//! selection, directory picker and per-file size limit. Split out of
//! `wizard.rs` to keep every production file within the file-size gate.

use super::{print_local_embeddings_unavailable, WizardState};
use crate::setup_module::dir_browser::browse_directories_multiselect_with_preselected;
use anyhow::Result;
use inquire::{Confirm, CustomType, Select};

/// Print the RAG configuration this installation already has and ask
/// whether to keep it as-is. Called only when a model is already configured
/// (CB59): re-asking every RAG field from scratch on every setup run was the
/// bug — the cursor on the embeddings-provider question always started on
/// "OpenAI" even for an install that had been running local-only for months.
pub(super) fn rag_keep_current_configuration(
    existing_config: &crate::domain::canopy_config::CanopyConfig,
) -> Result<bool> {
    println!();
    println!("  \x1b[1mPersonal knowledge indexing (RAG)\x1b[0m — currently configured:");
    println!("    Model:       {}", existing_config.embeddings_model);
    println!("    Threshold:   {}", existing_config.similarity_threshold);
    println!(
        "    Directories: {}",
        if existing_config.rag_personal_dirs.is_empty() {
            "(none)".to_string()
        } else {
            existing_config.rag_personal_dirs.join(", ")
        }
    );
    println!(
        "    Size limit:  {} MB per file",
        existing_config.rag_max_file_mb
    );
    println!();

    Confirm::new("Keep this configuration?")
        .with_default(true)
        .with_help_message("no: reconfigure model, directories, or size limit")
        .prompt()
        .map_err(|e| anyhow::anyhow!("RAG keep-configuration selection cancelled: {}", e))
}

/// The RAG step's outcome once the user has been asked "keep it?" (i.e.
/// `rag_previously_configured` is true) and answered yes. Pure — takes
/// `local_embeddings_available` as a parameter instead of reading
/// `cfg!(feature = "local-embeddings")` itself — so the CB59 fix (the
/// capability check gates this branch's *return*, not just
/// `run_rag_setup`'s own path) is testable without a terminal.
pub(super) struct KeptRagOutcome {
    pub(super) embeddings_model: String,
    pub(super) similarity_threshold: f32,
    pub(super) rag_personal_dirs: Vec<String>,
    pub(super) rag_max_file_mb: u32,
    /// `Some(reason)` when this build cannot do local embeddings at all:
    /// the "kept" answer is overridden and RAG ends up disabled instead.
    pub(super) disabled_reason: Option<&'static str>,
}

pub(super) fn resolve_kept_rag_configuration(
    existing_config: &crate::domain::canopy_config::CanopyConfig,
    local_embeddings_available: bool,
) -> KeptRagOutcome {
    if local_embeddings_available {
        KeptRagOutcome {
            embeddings_model: existing_config.embeddings_model.clone(),
            similarity_threshold: existing_config.similarity_threshold,
            rag_personal_dirs: existing_config.rag_personal_dirs.clone(),
            rag_max_file_mb: existing_config.rag_max_file_mb,
            disabled_reason: None,
        }
    } else {
        KeptRagOutcome {
            embeddings_model: String::new(),
            similarity_threshold: existing_config.similarity_threshold,
            rag_personal_dirs: Vec::new(),
            rag_max_file_mb: existing_config.rag_max_file_mb,
            disabled_reason: Some(
                crate::rag::embedding_client::LOCAL_EMBEDDINGS_UNAVAILABLE_REASON,
            ),
        }
    }
}

/// Ask whether to enable RAG at all, then — if yes and this build can run
/// local embeddings — which local model, which directories, and what
/// per-file size limit. `default_enable` is `true` when called because an
/// existing configuration is being revised (so declining to keep it still
/// defaults to staying enabled) and `false` for a genuinely fresh install.
/// No provider question: canopy's RAG is local-only (CB59) — see
/// `functional_requirements` 1 in the CB59 spec for why the cloud-provider
/// question was removed rather than merely reordered.
#[cfg_attr(not(feature = "local-embeddings"), allow(unused_variables))]
pub(super) fn run_rag_setup(
    existing_config: &crate::domain::canopy_config::CanopyConfig,
    canopy_dir: &std::path::Path,
    default_enable: bool,
    wiz: &mut WizardState,
) -> Result<(String, f32, Vec<String>, u32)> {
    let use_rag = confirm_enable_rag_prompt(default_enable)?;

    if !use_rag {
        wiz.add("\x1b[90m–\x1b[0m RAG: disabled".to_string());
        return disabled_rag(existing_config);
    }

    if !crate::rag::embedding_client::provider_available(
        crate::rag::embedding_client::EmbeddingProvider::Local,
    ) {
        print_local_embeddings_unavailable();
        wiz.add(
            "\x1b[90m–\x1b[0m RAG: disabled (build lacks local-embeddings support)".to_string(),
        );
        return disabled_rag(existing_config);
    }

    wiz.render()?;
    let embeddings_model = select_local_embeddings_model(&existing_config.embeddings_model)?;

    confirm_model_change(existing_config, &embeddings_model)?;

    wiz.add(format!(
        "\x1b[32m✓\x1b[0m Embeddings model: {}",
        embeddings_model
    ));

    record_model_acquisition_step(wiz, canopy_dir, &embeddings_model)?;

    // Chunk-merge similarity threshold: internal tuning knob with no
    // user-observable effect in its valid range, so it is not prompted.
    let similarity_threshold = existing_config.similarity_threshold;

    // ── RAG directories ─────────────────────────────────────────────
    let browser_start = rag_browser_starting_dir(existing_config);
    let rag_personal_dirs = pick_multiple_directories(
        "Personal RAG directories (your own notes/docs — indexed for global retrieval):",
        &browser_start,
        &existing_config.rag_personal_dirs,
    )?;
    wiz.add(format!(
        "\x1b[32m✓\x1b[0m Personal RAG dirs: {}",
        rag_personal_dirs.join(", ")
    ));

    // ── Per-file indexing size limit ─────────────────────────────────
    wiz.render()?;
    let rag_max_file_mb = select_rag_max_file_mb(existing_config.rag_max_file_mb)?;
    wiz.add(format!(
        "\x1b[32m✓\x1b[0m Indexing size limit: {rag_max_file_mb} MB per file"
    ));

    Ok((
        embeddings_model,
        similarity_threshold,
        rag_personal_dirs,
        rag_max_file_mb,
    ))
}

/// Ask whether to enable RAG at all — `default_enable` is `true` when
/// called because an existing configuration is being revised (so declining
/// to keep it still defaults to staying enabled) and `false` for a
/// genuinely fresh install.
fn confirm_enable_rag_prompt(default_enable: bool) -> Result<bool> {
    Confirm::new("Enable personal knowledge indexing (RAG)?")
        .with_default(default_enable)
        .with_help_message("Indexes your notes/docs so AI tools can search them")
        .prompt()
        .map_err(|e| anyhow::anyhow!("RAG selection cancelled: {}", e))
}

/// The RAG step's outcome when indexing stays off: no model, no
/// directories, the existing tuning knobs otherwise.
pub(super) fn disabled_rag(
    existing_config: &crate::domain::canopy_config::CanopyConfig,
) -> Result<(String, f32, Vec<String>, u32)> {
    Ok((
        String::new(),
        existing_config.similarity_threshold,
        Vec::new(),
        existing_config.rag_max_file_mb,
    ))
}

/// Warn that switching embeddings models forces a full re-index and get
/// explicit confirmation before proceeding (the check only fires when a
/// model was configured before).
fn confirm_model_change(
    existing_config: &crate::domain::canopy_config::CanopyConfig,
    embeddings_model: &str,
) -> Result<()> {
    if !existing_config.embeddings_model.is_empty()
        && embeddings_model != existing_config.embeddings_model
    {
        println!();
        println!("  \x1b[33m⚠  Embeddings model changed.\x1b[0m");
        println!("  \x1b[90mAll previously indexed documents will need to be re-indexed.\x1b[0m");
        println!("  \x1b[90mThis is a heavy operation and may take a while.\x1b[0m");
        println!();
        let confirmed = Confirm::new("Continue with the new model?")
            .with_default(false)
            .with_help_message("enter: confirm")
            .prompt()
            .unwrap_or(false);
        if !confirmed {
            anyhow::bail!("Embeddings model change cancelled by user");
        }
    }
    Ok(())
}

/// Record whether the chosen embeddings model is already cached. Setup
/// never downloads the model itself — it only checks whether one's already
/// cached; the daemon's background acquisition loop picks it up once it
/// (re)starts below, so a multi-hundred-MB download never blocks this
/// wizard.
#[cfg_attr(not(feature = "local-embeddings"), allow(unused_variables))]
fn record_model_acquisition_step(
    wiz: &mut WizardState,
    canopy_dir: &std::path::Path,
    embeddings_model: &str,
) -> Result<()> {
    wiz.render()?;
    #[cfg(feature = "local-embeddings")]
    let model_already_cached = {
        let model_cache_dir = canopy_dir.join("models");
        crate::rag::embedding_client::is_local_model_cached(embeddings_model, &model_cache_dir)
            .unwrap_or(false)
    };
    #[cfg(not(feature = "local-embeddings"))]
    let model_already_cached = false;
    if model_already_cached {
        wiz.add(format!(
            "\x1b[32m✓\x1b[0m Model ready: {embeddings_model} (already cached)"
        ));
    } else {
        wiz.add(
            "\x1b[33m⬇\x1b[0m Model will download in the background — indexing begins once it's ready"
                .to_string(),
        );
    }
    Ok(())
}

/// Where the directory browser should start: the parent of the first
/// currently-configured RAG directory when that parent still exists, else
/// the directory itself, else the parent of the configured RAG root, else
/// nothing at all.
pub(super) fn rag_browser_starting_dir(
    existing_config: &crate::domain::canopy_config::CanopyConfig,
) -> String {
    let prev_dirs = existing_config.rag_personal_dirs.clone();
    if !prev_dirs.is_empty() {
        prev_dirs
            .first()
            .and_then(|p| {
                std::path::Path::new(p)
                    .parent()
                    .map(|pp| pp.to_string_lossy().to_string())
            })
            .filter(|pp| std::path::Path::new(pp).is_dir())
            .unwrap_or_else(|| prev_dirs.first().unwrap().clone())
    } else if !existing_config.rag_personal_root.is_empty() {
        std::path::Path::new(&existing_config.rag_personal_root)
            .parent()
            .map(|pp| pp.to_string_lossy().to_string())
            .filter(|pp| std::path::Path::new(pp).is_dir())
            .unwrap_or(existing_config.rag_personal_root.clone())
    } else {
        String::new()
    }
}

/// Prompt for the per-file indexing size cap, in MB, with the currently
/// configured value (or the 10 MB default on first run) preselected. Rejects
/// out-of-range input inline via the same `validate_rag_max_file_mb` doctor
/// and ingestion both defer to, so the wizard can't save a value neither of
/// them would actually honor.
fn select_rag_max_file_mb(current: u32) -> Result<u32> {
    use inquire::validator::Validation;

    CustomType::<u32>::new("Per-file indexing size limit (MB):")
        .with_default(current)
        .with_help_message(&format!(
            "Files larger than this are skipped during indexing | ceiling: {} MB",
            crate::domain::canopy_config::RAG_MAX_FILE_MB_CEILING
        ))
        .with_validator(|mb: &u32| {
            Ok(
                match crate::domain::canopy_config::validate_rag_max_file_mb(*mb) {
                    Ok(()) => Validation::Valid,
                    Err(reason) => Validation::Invalid(reason.into()),
                },
            )
        })
        .prompt()
        .map_err(|e| anyhow::anyhow!("Indexing size limit selection cancelled: {}", e))
}

/// Human-readable label (name, dimensions, approximate download size) for a
/// supported local model id. The set of ids offered is derived from
/// `embedding_client::LOCAL_MODEL_IDS` — the same single source of truth
/// `model_id_to_fastembed` maps from — instead of a second hand-maintained
/// id list, so the wizard can no longer drift out of step with which models
/// are actually supported. Only the descriptive text lives here; coverage
/// against `LOCAL_MODEL_IDS` is asserted by
/// `every_local_model_id_has_a_label` below.
pub(super) fn local_model_label(id: &str) -> &'static str {
    match id {
        "baai/bge-small-en-v1.5" => {
            "BGE Small EN v1.5     (local · 384d · ~130 MB)  — fast, great for English"
        }
        "baai/bge-base-en-v1.5" => {
            "BGE Base EN v1.5      (local · 768d · ~430 MB)  — balanced, English"
        }
        "baai/bge-large-en-v1.5" => {
            "BGE Large EN v1.5     (local · 1024d · ~1.3 GB) — best quality, English"
        }
        "intfloat/multilingual-e5-small" => {
            "Multilingual E5 Small (local · 384d · ~480 MB)  — fast, multilingual"
        }
        "intfloat/multilingual-e5-base" => {
            "Multilingual E5 Base  (local · 768d · ~1.1 GB)  — balanced, multilingual"
        }
        "intfloat/multilingual-e5-large" => {
            "Multilingual E5 Large (local · 1024d · ~2.2 GB) — best quality, multilingual"
        }
        // Unreached in practice — every id in LOCAL_MODEL_IDS is covered
        // above, and every_local_model_id_has_a_label fails the build if a
        // new one is added here without it.
        _ => "(unlabeled model)",
    }
}

/// The `(id, label)` pairs the embeddings-model prompt offers: every
/// supported local model id paired with its label, nothing else. Split out
/// so a fresh install's model list is testable without a terminal (CB59:
/// it must never again include a remote/API-key option).
pub(super) fn local_embeddings_model_choices() -> Vec<(&'static str, &'static str)> {
    crate::rag::embedding_client::LOCAL_MODEL_IDS
        .iter()
        .map(|id| (*id, local_model_label(id)))
        .collect()
}

fn select_local_embeddings_model(current: &str) -> Result<String> {
    let choices = local_embeddings_model_choices();
    let options: Vec<&str> = choices.iter().map(|(_, label)| *label).collect();

    let start = choices
        .iter()
        .position(|(id, _)| *id == current)
        .unwrap_or(0);

    let selected = Select::new("Embeddings model (local, no API key required):", options)
        .with_starting_cursor(start)
        .with_help_message(
            "Downloaded once to ~/.canopy/models/ — no internet needed after that | ↑↓: navigate | enter: confirm",
        )
        .prompt()
        .map_err(|e| anyhow::anyhow!("Embeddings model selection cancelled: {}", e))?;

    choices
        .iter()
        .find(|(_, label)| *label == selected)
        .map(|(id, _)| id.to_string())
        .ok_or_else(|| anyhow::anyhow!("Unknown embeddings model selection"))
}

/// Interactively pick one or more directories for personal RAG indexing.
fn pick_multiple_directories(
    message: &str,
    initial: &str,
    existing: &[String],
) -> Result<Vec<String>> {
    println!("  {message}");
    if !existing.is_empty() {
        println!("  \x1b[90mCurrently configured:\x1b[0m");
        for dir in existing {
            println!("    \x1b[90m• {dir}\x1b[0m");
        }
    }
    println!("  \x1b[90mUse Space to mark directories, navigate with ↑↓, → to enter folders, ← to go up, Enter to confirm.\x1b[0m");

    let pre_selected: std::collections::HashSet<String> = existing.iter().cloned().collect();
    let selected = browse_directories_multiselect_with_preselected(initial, pre_selected);

    if selected.is_empty() {
        anyhow::bail!("At least one RAG directory is required");
    }

    // Sort for consistency
    let mut dirs = selected;
    dirs.sort();

    println!("\n  \x1b[32m✓\x1b[0m Selected directories:");
    for dir in &dirs {
        println!("    \x1b[90m• {dir}\x1b[0m");
    }
    println!();

    Ok(dirs)
}
