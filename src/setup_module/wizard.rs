use crate::setup_module::daemon_service::{
    install_service_if_needed, start_daemon_if_needed, stop_daemon,
};
use crate::setup_module::dir_browser::browse_directories_multiselect_with_preselected;
use crate::setup_module::models::{is_platform_available, Platform};
use crate::setup_module::platform_adapter::clear_wizard_screen;
use crate::setup_module::registry_fetch::{fetch_registry, print_banner};
use crate::setup_module::sync_and_skills::{run_essential_skills_step, run_sync_step};
use crate::setup_module::PlatformWithCli;
use anyhow::{bail, Context, Result};
use inquire::{Confirm, CustomType, MultiSelect, Select};
use std::io::{self, Write};

pub fn run_setup(force_skills: bool) -> Result<()> {
    let mut wiz = WizardState::new();
    let home = dirs::home_dir().context("No home directory")?;
    let canopy_dir = home.join(".canopy");
    let existing_config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);

    // ── Step 1: Fetch registry ──────────────────────────────────
    clear_wizard_screen()?;
    print_banner();
    print!("  Fetching platform registry... ");
    io::stdout().flush()?;
    let mut registry = fetch_registry()?;

    // Legacy v5 compat: no longer needed with v6
    let _ = &mut registry;
    println!("\x1b[32m✓\x1b[0m");

    let detected: Vec<&Platform> = registry
        .platforms
        .iter()
        .filter(|p| is_platform_available(p))
        .collect();

    let detected_names: Vec<&str> = detected.iter().map(|p| p.name.as_str()).collect();
    wiz.add(format!(
        "\x1b[32m✓\x1b[0m Fetched registry — {} detected: {}",
        detected.len(),
        if detected_names.is_empty() {
            "(none)".to_string()
        } else {
            detected_names.join(", ")
        }
    ));

    // ── Step 2: Select platforms ─────────────────────────────────
    wiz.render()?;
    if detected.is_empty() {
        println!();
        println!("  \x1b[33mNo supported platform CLI detected on PATH.\x1b[0m");
        println!();
        println!("  This means no platform is installed at all — not that an");
        println!("  installed one is failing to respond. If a platform IS installed");
        println!("  but setup still can't see it, its binary isn't on PATH; to check");
        println!("  whether an installed platform actually answers, run");
        println!("  \x1b[1mcanopy doctor\x1b[0m or use \x1b[1magent_probe\x1b[0m.");
        println!();
        println!("  \"CLI\" here means the platform's command-line tool — the binary");
        println!("  canopy runs to drive it. canopy needs at least one platform's");
        println!("  CLI installed and authenticated before setup can configure");
        println!("  anything.");
        println!();
        println!("  Supported platforms and the CLI binary canopy will invoke:");
        println!();
        for p in &registry.platforms {
            let binary = p
                .cli
                .as_ref()
                .and_then(|v| v.get("binary").and_then(|b| b.as_str()))
                .unwrap_or("(unknown)");
            println!(
                "    \x1b[90m{:<14}\x1b[0m  binary: \x1b[1m{}\x1b[0m",
                p.name, binary
            );
        }
        println!();
        println!("  Install one of the above, make sure it's on your PATH, then");
        println!("  run \x1b[1mcanopy setup\x1b[0m again.");
        println!();
        return Ok(());
    }

    let selected = select_platforms(&detected)?;
    let selected_names: Vec<&str> = selected.iter().map(|p| p.name.as_str()).collect();
    wiz.add(format!(
        "\x1b[32m✓\x1b[0m Platforms: {}",
        if selected_names.is_empty() {
            "(none)".to_string()
        } else {
            selected_names.join(", ")
        }
    ));

    // ── Step 2.2: Temperature unit preference ────────────────────
    wiz.render()?;
    let temperature_unit = select_temperature_unit()?;
    wiz.add(format!(
        "\x1b[32m✓\x1b[0m Temperature unit: {}",
        match temperature_unit {
            crate::domain::canopy_config::TemperatureUnit::Celsius => "Celsius (°C)",
            crate::domain::canopy_config::TemperatureUnit::Fahrenheit => "Fahrenheit (°F)",
        }
    ));

    // ── Step 2.25: TUI theme preference ──────────────────────────
    wiz.render()?;
    let theme = select_theme(&existing_config.theme)?;
    wiz.add(format!(
        "\x1b[32m✓\x1b[0m Theme: {}",
        match theme.as_str() {
            "modern" => THEME_OPTION_MODERN,
            _ => THEME_OPTION_CLASSIC,
        }
    ));

    // ── Step 2.3: RAG opt-in ─────────────────────────────────────
    wiz.render()?;
    let rag_previously_configured = !existing_config.embeddings_model.is_empty()
        || !existing_config.rag_personal_dirs.is_empty();
    let use_rag = Confirm::new("Enable personal knowledge indexing (RAG)?")
        .with_default(rag_previously_configured)
        .with_help_message("Indexes your notes/docs so AI tools can search them")
        .prompt()
        .map_err(|e| anyhow::anyhow!("RAG selection cancelled: {}", e))?;

    let (embeddings_model, similarity_threshold, rag_personal_dirs, rag_max_file_mb) = if use_rag {
        // Offer all available providers (remote are always available, local only if compiled in)
        wiz.render()?;
        let provider = select_embedding_provider()?;

        let embeddings_model = match provider {
            Some(crate::rag::embedding_client::EmbeddingProvider::Local) => {
                select_local_embeddings_model(&existing_config.embeddings_model)?
            }
            Some(crate::rag::embedding_client::EmbeddingProvider::OpenAi) => {
                // Refuse a broken cloud config the same way we refuse an
                // unrunnable local model: warn, then stop — don't save a
                // model string the daemon can't index with.
                if !check_api_key_and_warn(crate::rag::embedding_client::EmbeddingProvider::OpenAi)?
                {
                    bail!(
                        "OPENAI_API_KEY is not set. Export it and re-run setup, \
                         or choose a different embedding provider."
                    );
                }
                select_remote_embeddings_model(
                    crate::rag::embedding_client::EmbeddingProvider::OpenAi,
                    &existing_config.embeddings_model,
                )?
            }
            Some(crate::rag::embedding_client::EmbeddingProvider::Gemini) => {
                if !check_api_key_and_warn(crate::rag::embedding_client::EmbeddingProvider::Gemini)?
                {
                    bail!(
                        "GEMINI_API_KEY is not set. Export it and re-run setup, \
                         or choose a different embedding provider."
                    );
                }
                select_remote_embeddings_model(
                    crate::rag::embedding_client::EmbeddingProvider::Gemini,
                    &existing_config.embeddings_model,
                )?
            }
            None => {
                // User explicitly chose "none" — disable RAG
                wiz.add("\x1b[90m–\x1b[0m RAG: disabled (no provider selected)".to_string());
                String::new()
            }
        };

        let (embeddings_model, similarity_threshold, rag_personal_dirs, rag_max_file_mb) =
            if embeddings_model.is_empty() {
                (
                    String::new(),
                    existing_config.similarity_threshold,
                    Vec::new(),
                    existing_config.rag_max_file_mb,
                )
            } else {
                // ── Model changed check ─────────────────────────────────
                if !existing_config.embeddings_model.is_empty()
                    && embeddings_model != existing_config.embeddings_model
                {
                    println!();
                    println!("  \x1b[33m⚠  Embeddings model changed.\x1b[0m");
                    println!(
                    "  \x1b[90mAll previously indexed documents will need to be re-indexed.\x1b[0m"
                );
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

                wiz.add(format!(
                    "\x1b[32m✓\x1b[0m Embeddings model: {}",
                    embeddings_model
                ));

                // ── Model acquisition ─────────────────────────────────────────
                // Setup never downloads the model itself — it only checks whether
                // one's already cached, so this returns immediately either way. If
                // it isn't cached, the daemon's background acquisition loop (see
                // `IngestionManager::ensure_configured_model_acquired`) picks it up
                // once it (re)starts below, so a multi-hundred-MB download never
                // blocks this wizard. Remote (cloud) models need no download at
                // all — the daemon only needs the provider's API key.
                wiz.render()?;
                let is_local_provider = matches!(
                    provider,
                    Some(crate::rag::embedding_client::EmbeddingProvider::Local)
                );
                if !is_local_provider {
                    wiz.add(
                        "\x1b[32m✓\x1b[0m Cloud embeddings — no model download needed".to_string(),
                    );
                } else {
                    #[cfg(feature = "local-embeddings")]
                    let model_already_cached = {
                        let model_cache_dir = canopy_dir.join("models");
                        crate::rag::embedding_client::is_local_model_cached(
                            &embeddings_model,
                            &model_cache_dir,
                        )
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
                }

                // Chunk-merge similarity threshold: internal tuning knob with no
                // user-observable effect in its valid range, so it is not prompted.
                // The config.toml value (default 0.4) is carried forward and can
                // still be edited manually for experimentation.
                let similarity_threshold = existing_config.similarity_threshold;

                // ── RAG directories ─────────────────────────────────────────
                let prev_dirs = existing_config.rag_personal_dirs.clone();
                // Start browser at the parent of the first configured dir so the user
                // sees their selection highlighted instead of landing inside an empty dir.
                let browser_start = if !prev_dirs.is_empty() {
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
                };
                let rag_personal_dirs = pick_multiple_directories(
                "Personal RAG directories (your own notes/docs — indexed for global retrieval):",
                &browser_start,
                &prev_dirs,
            )?;
                wiz.add(format!(
                    "\x1b[32m✓\x1b[0m Personal RAG dirs: {}",
                    rag_personal_dirs.join(", ")
                ));

                // ── Per-file indexing size limit ─────────────────────────────
                wiz.render()?;
                let rag_max_file_mb = select_rag_max_file_mb(existing_config.rag_max_file_mb)?;
                wiz.add(format!(
                    "\x1b[32m✓\x1b[0m Indexing size limit: {rag_max_file_mb} MB per file"
                ));

                (
                    embeddings_model,
                    similarity_threshold,
                    rag_personal_dirs,
                    rag_max_file_mb,
                )
            };

        (
            embeddings_model,
            similarity_threshold,
            rag_personal_dirs,
            rag_max_file_mb,
        )
    } else {
        wiz.add("\x1b[90m–\x1b[0m RAG: disabled".to_string());
        (
            String::new(),
            existing_config.similarity_threshold,
            Vec::new(),
            existing_config.rag_max_file_mb,
        )
    };

    // ── Step 3: Install MCP servers + show matrix ───────────────
    if !selected.is_empty() {
        let sync_summary = run_sync_step(&mut wiz, &home, &selected, &registry.canonical_servers)?;
        if let Some(s) = sync_summary {
            wiz.add(s);
        }
    }

    // ── Step 4: Save CLI configuration ──────────────────────────
    let platforms_with_cli: Vec<PlatformWithCli> = selected
        .iter()
        .map(|p| p.to_platform_with_cli())
        .filter(|p| p.cli.is_some())
        .collect();

    let cli_registry =
        crate::domain::cli_config::CliRegistry::detect_available(&platforms_with_cli);
    std::fs::create_dir_all(&canopy_dir)?;
    for dir in &rag_personal_dirs {
        std::fs::create_dir_all(dir)?;
    }

    // ── Step 5: Essential Skills ─────────────────────────────────
    wiz.render()?;
    let skills_step = run_essential_skills_step(&home, &selected, force_skills);
    wiz.add(skills_step);

    // ── Step 6: Daemon + service ────────────────────────────────
    wiz.render()?;

    // Always restart daemon to pick up new MCP configs
    let _ = stop_daemon();
    let daemon_msg = match start_daemon_if_needed() {
        Ok(true) => "\x1b[32m✓\x1b[0m Daemon: (re)started",
        Ok(false) => "\x1b[32m✓\x1b[0m Daemon: already running",
        Err(_) => "\x1b[31m✗\x1b[0m Daemon: failed to start",
    };
    wiz.add(daemon_msg.to_string());

    let service_msg = match install_service_if_needed() {
        Ok(true) => "\x1b[32m✓\x1b[0m Service: installed",
        Ok(false) => "\x1b[32m✓\x1b[0m Service: already installed",
        Err(_) => "\x1b[31m✗\x1b[0m Service: failed to install",
    };
    wiz.add(service_msg.to_string());

    // ── Step 7: Announcements opt-in ─────────────────────────────
    wiz.render()?;
    println!();
    println!("  \x1b[1mAnnouncements\x1b[0m");
    println!();
    println!("  Canopy can show you important announcements from the Mission Log");
    println!("  (e.g. new releases, breaking changes, community events) as desktop");
    println!("  notifications.");
    println!();
    println!("  This requires a \x1b[1mpersistent outbound WebSocket connection\x1b[0m to");
    println!("  announcements.univerlab.org. While idle, this connection costs near");
    println!("  zero — but it does reveal when this canopy installation is running.");
    println!();

    let announcements = Confirm::new("  Enable announcements?")
        .with_default(false)
        .prompt()
        .unwrap_or(false);

    if announcements {
        wiz.add("Announcements enabled".to_string());
    } else {
        wiz.add("Announcements disabled (default)".to_string());
    }

    // ── Save unified config ──────────────────────────────────────
    let mut config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
    config.mark_configured();
    let baseline = crate::domain::registry_baseline::RegistryBaseline::load(&canopy_dir);
    let changed_fields = merge_cli_registry_into_config(
        &mut config,
        &cli_registry,
        &platforms_with_cli,
        baseline.as_ref(),
    );
    config.temperature_unit = temperature_unit;
    config.theme = theme;
    config.embeddings_model = embeddings_model;
    config.similarity_threshold = similarity_threshold;
    config.rag_personal_dirs = rag_personal_dirs;
    config.rag_max_file_mb = rag_max_file_mb;
    config.announcements_enabled = announcements;
    let config_step = match config.save(&canopy_dir) {
        Ok(_) => format!(
            "\x1b[32m✓\x1b[0m Config: {} CLI(s) saved to config.toml",
            config.clis.len()
        ),
        Err(e) => format!("\x1b[33m⚠\x1b[0m Config: {e}"),
    };
    wiz.add(config_step);
    if !changed_fields.is_empty() {
        wiz.add(format!(
            "\x1b[90m  Updated from registry: {}\x1b[0m",
            changed_fields.join(", ")
        ));
    }

    // ── Final summary ───────────────────────────────────────────
    wiz.render()?;
    println!("  \x1b[1;32m✅ Setup complete! canopy is ready.\x1b[0m");
    println!("  Run \x1b[1mcanopy\x1b[0m or \x1b[1mcanopy tui\x1b[0m to launch the interface.");
    println!();

    Ok(())
}

/// Merge the registry's detected CLIs into `config.clis` through the same
/// per-field three-way merge `apply_registry_refresh` uses (CB58), instead
/// of replacing the whole list outright. A key present locally and absent
/// from the registry survives.
///
/// `infra_retry_limit`, `infra_crash_max_seconds` and `infra_backoff_seconds`
/// are canopy-side settings the registry never publishes. The generic
/// per-field merge cannot tell "the registry doesn't own this field" apart
/// from "the registry wants it unset" when there is no baseline yet for a
/// CLI (first-ever refresh: `merge_cli_fields` treats every field as
/// user-untouched and adopts the registry's `null` outright) -- getting that
/// wrong here is the exact CB58 incident (`infra_retry_limit = 0` silently
/// dropped). So these three fields are restored to their pre-merge local
/// value unconditionally, after the generic merge runs, regardless of
/// baseline state.
///
/// Returns the registry-owned fields that actually changed
/// (`"{cli_name}.{field}"`), with any `infra_*` entries stripped back out
/// since those never really changed -- for the wizard's own-run summary.
/// Silence is the failure mode this fixes.
fn merge_cli_registry_into_config(
    config: &mut crate::domain::canopy_config::CanopyConfig,
    cli_registry: &crate::domain::cli_config::CliRegistry,
    platforms_with_cli: &[PlatformWithCli],
    baseline: Option<&crate::domain::registry_baseline::RegistryBaseline>,
) -> Vec<String> {
    let registry_by_name: std::collections::HashMap<String, crate::domain::cli_config::CliConfig> =
        cli_registry
            .available_clis
            .iter()
            .map(|c| (c.name.clone(), c.clone()))
            .collect();

    let mut changed_fields: Vec<String> = Vec::new();

    for existing in config.clis.iter_mut() {
        if let Some(registry_cli) = registry_by_name.get(&existing.name) {
            let baseline_cli = baseline.and_then(|b| b.get(&existing.name));
            let name = existing.name.clone();
            let pre_merge_infra_retry_limit = existing.infra_retry_limit;
            let pre_merge_infra_crash_max_seconds = existing.infra_crash_max_seconds;
            let pre_merge_infra_backoff_seconds = existing.infra_backoff_seconds;

            let mut merged = crate::setup_module::registry_fetch::merge_cli_fields(
                existing,
                baseline_cli,
                registry_cli,
                &name,
                &mut changed_fields,
            );

            // CM30 settings: canopy-side only, never registry-owned. Restore
            // regardless of what the generic merge just did.
            merged.infra_retry_limit = pre_merge_infra_retry_limit;
            merged.infra_crash_max_seconds = pre_merge_infra_crash_max_seconds;
            merged.infra_backoff_seconds = pre_merge_infra_backoff_seconds;
            changed_fields.retain(|f| {
                f != &format!("{name}.infra_retry_limit")
                    && f != &format!("{name}.infra_crash_max_seconds")
                    && f != &format!("{name}.infra_backoff_seconds")
            });

            *existing = merged;
        }
    }

    // Remove CLIs only when the registry explicitly knows the platform AND
    // the binary is confirmed missing -- same rule `apply_registry_refresh`
    // uses, so a manually-added CLI the registry has never heard of is kept.
    let known_names: std::collections::HashSet<String> = platforms_with_cli
        .iter()
        .filter_map(|p| p.cli.as_ref().map(|c| c.name.clone()))
        .collect();
    config.clis.retain(|c| {
        if !known_names.contains(&c.name) {
            return true;
        }
        c.is_available()
    });

    // Add newly detected CLIs that aren't already in config.
    let existing_names: std::collections::HashSet<String> =
        config.clis.iter().map(|c| c.name.clone()).collect();
    for cli in cli_registry.available_clis.iter().cloned() {
        if !existing_names.contains(&cli.name) {
            config.clis.push(cli);
        }
    }

    changed_fields
}

/// Tracks completed wizard steps so we can re-render a clean summary
/// after clearing the screen between interactive phases.
pub(crate) struct WizardState {
    steps: Vec<String>,
}

impl WizardState {
    fn new() -> Self {
        Self { steps: vec![] }
    }

    fn add(&mut self, summary: String) {
        self.steps.push(summary);
    }

    /// Clear screen → banner → all completed step summaries.
    pub(crate) fn render(&self) -> Result<()> {
        clear_wizard_screen()?;
        print_banner();
        for step in &self.steps {
            println!("  {step}");
        }
        if !self.steps.is_empty() {
            println!();
        }
        Ok(())
    }
}

fn select_platforms<'a>(detected: &[&'a Platform]) -> Result<Vec<&'a Platform>> {
    if detected.is_empty() {
        println!("  Press Enter to continue...");
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        return Ok(vec![]);
    }

    let platform_names: Vec<&str> = detected.iter().map(|p| p.name.as_str()).collect();
    let all_indices: Vec<usize> = (0..detected.len()).collect();

    let selected = MultiSelect::new("Select platforms to configure:", platform_names)
        .with_default(&all_indices)
        .with_help_message("space: toggle | enter: confirm | ↑↓: navigate")
        .prompt()
        .map_err(|e| anyhow::anyhow!("Selection cancelled: {}", e))?;

    Ok(selected
        .iter()
        .filter_map(|name| detected.iter().find(|p| p.name == *name).copied())
        .collect())
}

fn select_temperature_unit() -> Result<crate::domain::canopy_config::TemperatureUnit> {
    let options = ["Celsius (°C)", "Fahrenheit (°F)"];
    let selected = Select::new("Temperature unit for sysinfo:", options.to_vec())
        .with_starting_cursor(0)
        .with_help_message("enter: confirm | ↑↓: navigate")
        .prompt()
        .map_err(|e| anyhow::anyhow!("Temperature selection cancelled: {}", e))?;

    Ok(match selected {
        "Fahrenheit (°F)" => crate::domain::canopy_config::TemperatureUnit::Fahrenheit,
        _ => crate::domain::canopy_config::TemperatureUnit::Celsius,
    })
}

const THEME_OPTION_CLASSIC: &str = "Classic (bordered)";
const THEME_OPTION_MODERN: &str = "Modern (borderless)";

fn select_theme(current: &str) -> Result<String> {
    let options = vec![THEME_OPTION_CLASSIC, THEME_OPTION_MODERN];
    let starting_cursor = options
        .iter()
        .position(|option| theme_choice_to_config_value(option) == current)
        .unwrap_or(0);
    let selected = Select::new("TUI theme:", options)
        .with_starting_cursor(starting_cursor)
        .with_help_message("enter: confirm | restart the TUI to apply")
        .prompt()
        .map_err(|e| anyhow::anyhow!("Theme selection cancelled: {}", e))?;

    Ok(theme_choice_to_config_value(selected))
}

/// Map a `select_theme` menu label to the `CanopyConfig::theme` value.
/// Pure so it's testable without an interactive prompt.
fn theme_choice_to_config_value(selected: &str) -> String {
    if selected == THEME_OPTION_MODERN {
        "modern".to_string()
    } else {
        "classic".to_string()
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
fn local_model_label(id: &str) -> &'static str {
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

fn select_local_embeddings_model(current: &str) -> Result<String> {
    let ids = crate::rag::embedding_client::LOCAL_MODEL_IDS;
    let options: Vec<&str> = ids.iter().map(|id| local_model_label(id)).collect();

    let start = ids.iter().position(|id| *id == current).unwrap_or(0);

    let selected = Select::new("Embeddings model (local, no API key required):", options)
        .with_starting_cursor(start)
        .with_help_message(
            "Downloaded once to ~/.canopy/models/ — no internet needed after that | ↑↓: navigate | enter: confirm",
        )
        .prompt()
        .map_err(|e| anyhow::anyhow!("Embeddings model selection cancelled: {}", e))?;

    ids.iter()
        .find(|id| local_model_label(id) == selected)
        .map(|id| id.to_string())
        .ok_or_else(|| anyhow::anyhow!("Unknown embeddings model selection"))
}

/// Model IDs for remote embedding providers.
pub const OPENAI_MODEL_IDS: &[&str] = &[
    "text-embedding-3-small",
    "text-embedding-3-large",
    "text-embedding-ada-002",
];

pub const GEMINI_MODEL_IDS: &[&str] = &["gemini-embedding-001", "gemini-embedding-2"];

/// Human-readable label for a supported OpenAI model id.
fn openai_model_label(id: &str) -> &'static str {
    match id {
        "text-embedding-3-small" => "text-embedding-3-small (OpenAI · 1536d) — fast, balanced",
        "text-embedding-3-large" => "text-embedding-3-large (OpenAI · 3072d) — best quality",
        "text-embedding-ada-002" => "text-embedding-ada-002 (OpenAI · 1536d) — legacy, reliable",
        _ => "(unlabeled OpenAI model)",
    }
}

/// Human-readable label for a supported Gemini model id.
fn gemini_model_label(id: &str) -> &'static str {
    match id {
        "gemini-embedding-001" => "gemini-embedding-001 (Gemini · 3072d) — standard",
        "gemini-embedding-2" => "gemini-embedding-2 (Gemini · 3072d) — latest",
        _ => "(unlabeled Gemini model)",
    }
}

/// Providers the wizard can offer on THIS build: remote providers are
/// compiled into every binary, local only when built with
/// `local-embeddings`. `select_embedding_provider` renders exactly this
/// list (plus an explicit opt-out), so this is the testable core of the
/// "wizard offers remote providers" requirement — the inquire prompt
/// itself needs a TTY and can't be unit-tested.
pub fn available_embedding_providers() -> Vec<crate::rag::embedding_client::EmbeddingProvider> {
    use crate::rag::embedding_client::{provider_available, EmbeddingProvider};
    let mut providers = vec![EmbeddingProvider::OpenAi, EmbeddingProvider::Gemini];
    if provider_available(EmbeddingProvider::Local) {
        providers.push(EmbeddingProvider::Local);
    }
    providers
}

/// Prompt the user to select an embedding provider.
/// Returns None if the user explicitly opts out.
pub fn select_embedding_provider() -> Result<Option<crate::rag::embedding_client::EmbeddingProvider>>
{
    let mut options: Vec<&str> = Vec::new();
    let mut values: Vec<Option<crate::rag::embedding_client::EmbeddingProvider>> = Vec::new();

    // Always offer remote providers (they're compiled into every binary)
    for provider in available_embedding_providers() {
        let label = match provider {
            crate::rag::embedding_client::EmbeddingProvider::OpenAi => {
                "OpenAI (cloud · requires API key)"
            }
            crate::rag::embedding_client::EmbeddingProvider::Gemini => {
                "Gemini (cloud · requires API key)"
            }
            crate::rag::embedding_client::EmbeddingProvider::Local => {
                "Local (on-machine · no API key)"
            }
        };
        options.push(label);
        values.push(Some(provider));
    }

    options.push("None (disable RAG)");
    values.push(None);

    let selected = Select::new(
        "Which embedding provider should canopy use?",
        options.clone(),
    )
    .with_help_message(
        "Remote providers work out of the box. Local requires building with 'local-embeddings'.",
    )
    .prompt()
    .map_err(|e| anyhow::anyhow!("Provider selection cancelled: {}", e))?;

    // Find the corresponding value
    let idx = options
        .iter()
        .position(|o| *o == selected)
        .ok_or_else(|| anyhow::anyhow!("Unknown provider selection"))?;
    Ok(values[idx])
}

/// Prompt the user to select a remote embedding model.
pub fn select_remote_embeddings_model(
    provider: crate::rag::embedding_client::EmbeddingProvider,
    current: &str,
) -> Result<String> {
    let (ids, label_fn): (Vec<&str>, fn(&str) -> &'static str) = match provider {
        crate::rag::embedding_client::EmbeddingProvider::OpenAi => {
            (OPENAI_MODEL_IDS.to_vec(), openai_model_label)
        }
        crate::rag::embedding_client::EmbeddingProvider::Gemini => {
            (GEMINI_MODEL_IDS.to_vec(), gemini_model_label)
        }
        _ => bail!("select_remote_embeddings_model only supports remote providers"),
    };

    let options: Vec<&str> = ids.iter().map(|id| label_fn(id)).collect();

    let start = ids.iter().position(|id| *id == current).unwrap_or(0);

    let selected = Select::new("Embeddings model:", options)
        .with_starting_cursor(start)
        .with_help_message("Select the model to use for embeddings")
        .prompt()
        .map_err(|e| anyhow::anyhow!("Embeddings model selection cancelled: {}", e))?;

    ids.iter()
        .find(|id| label_fn(id) == selected)
        .map(|id| id.to_string())
        .ok_or_else(|| anyhow::anyhow!("Unknown embeddings model selection"))
}

/// Check if the required API key is set for the given provider, warn if not.
/// Returns true if the key is present, false otherwise.
pub fn check_api_key_and_warn(
    provider: crate::rag::embedding_client::EmbeddingProvider,
) -> Result<bool> {
    let (key_var, key_name) = match provider {
        crate::rag::embedding_client::EmbeddingProvider::OpenAi => ("OPENAI_API_KEY", "OpenAI"),
        crate::rag::embedding_client::EmbeddingProvider::Gemini => ("GEMINI_API_KEY", "Gemini"),
        crate::rag::embedding_client::EmbeddingProvider::Local => {
            // Local models don't need an API key
            return Ok(true);
        }
    };

    if std::env::var(key_var).is_ok() {
        println!("  \x1b[32m✓\x1b[0m {} API key found", key_name);
        Ok(true)
    } else {
        println!(
            "  \x1b[33m⚠\x1b[0m {} API key not set (set {} to enable indexing)",
            key_name, key_var
        );
        Ok(false)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every id the wizard could offer (derived from `LOCAL_MODEL_IDS`) must
    /// have a real label, not the id-echoing fallback arm in
    /// `local_model_label` — that fallback exists only so a missing label
    /// can't panic mid-setup; this test is what actually catches it.
    #[test]
    fn every_local_model_id_has_a_label() {
        for id in crate::rag::embedding_client::LOCAL_MODEL_IDS {
            assert_ne!(
                local_model_label(id),
                *id,
                "missing a descriptive label for '{id}'"
            );
        }
    }

    #[test]
    fn local_model_labels_are_unique() {
        let ids = crate::rag::embedding_client::LOCAL_MODEL_IDS;
        let mut labels: Vec<&str> = ids.iter().map(|id| local_model_label(id)).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(
            labels.len(),
            ids.len(),
            "two ids resolved to the same label — select_local_embeddings_model \
             maps the chosen label back to an id and needs them distinct"
        );
    }

    // ── CB19: wizard offers remote embedding providers ─────────────

    #[test]
    fn available_embedding_providers_always_offers_remote() {
        // Remote providers are compiled into every binary — the wizard must
        // offer them with or without the `local-embeddings` feature. If
        // remote support were ever removed, OpenAi/Gemini would disappear
        // from this list and the wizard would fall back to disabling RAG.
        let providers = available_embedding_providers();
        assert!(
            providers.contains(&crate::rag::embedding_client::EmbeddingProvider::OpenAi),
            "OpenAI must always be offered, got: {providers:?}"
        );
        assert!(
            providers.contains(&crate::rag::embedding_client::EmbeddingProvider::Gemini),
            "Gemini must always be offered, got: {providers:?}"
        );
    }

    #[test]
    fn available_embedding_providers_local_matches_build_capability() {
        // Local is offered exactly when this binary can run it — present
        // with `local-embeddings`, omitted without. Choosing "none" (the
        // opt-out `select_embedding_provider` appends after this list) is
        // what disables RAG, never the absence of local support.
        let providers = available_embedding_providers();
        let offers_local =
            providers.contains(&crate::rag::embedding_client::EmbeddingProvider::Local);
        assert_eq!(
            offers_local,
            crate::rag::embedding_client::provider_available(
                crate::rag::embedding_client::EmbeddingProvider::Local
            ),
            "local must be offered iff this build can run it"
        );
    }

    #[test]
    fn openai_and_gemini_model_ids_resolve_to_remote_providers() {
        // The model ids the wizard offers for each remote provider must
        // route back to that provider via `provider_for_model` — otherwise
        // setup would save a config string doctor/client can't serve.
        for id in OPENAI_MODEL_IDS {
            assert_eq!(
                crate::rag::embedding_client::provider_for_model(id),
                Some(crate::rag::embedding_client::EmbeddingProvider::OpenAi),
                "'{id}' must resolve to the OpenAI provider"
            );
        }
        for id in GEMINI_MODEL_IDS {
            assert_eq!(
                crate::rag::embedding_client::provider_for_model(id),
                Some(crate::rag::embedding_client::EmbeddingProvider::Gemini),
                "'{id}' must resolve to the Gemini provider"
            );
        }
    }

    #[test]
    fn every_remote_model_id_has_a_label() {
        for id in OPENAI_MODEL_IDS {
            assert_ne!(
                openai_model_label(id),
                "(unlabeled OpenAI model)",
                "missing a descriptive label for '{id}'"
            );
        }
        for id in GEMINI_MODEL_IDS {
            assert_ne!(
                gemini_model_label(id),
                "(unlabeled Gemini model)",
                "missing a descriptive label for '{id}'"
            );
        }
    }

    #[test]
    fn remote_model_labels_are_unique() {
        // `select_remote_embeddings_model` maps the chosen label back to an
        // id and needs them distinct — same invariant as the local selector.
        let mut labels: Vec<&str> = OPENAI_MODEL_IDS
            .iter()
            .map(|id| openai_model_label(id))
            .collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), OPENAI_MODEL_IDS.len());

        let mut labels: Vec<&str> = GEMINI_MODEL_IDS
            .iter()
            .map(|id| gemini_model_label(id))
            .collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), GEMINI_MODEL_IDS.len());
    }

    #[test]
    fn check_api_key_and_warn_local_needs_no_key() {
        assert!(
            check_api_key_and_warn(crate::rag::embedding_client::EmbeddingProvider::Local).unwrap(),
            "local models need no API key"
        );
    }

    #[test]
    fn check_api_key_and_warn_openai_warns_when_missing() {
        // Safe under `cargo nextest` (one process per test); the doctor
        // module's black-box tests mutate process env vars on the same
        // basis.
        let prev = std::env::var("OPENAI_API_KEY").ok();
        unsafe { std::env::remove_var("OPENAI_API_KEY") };
        let result =
            check_api_key_and_warn(crate::rag::embedding_client::EmbeddingProvider::OpenAi)
                .unwrap();
        match prev {
            Some(v) => unsafe { std::env::set_var("OPENAI_API_KEY", v) },
            None => unsafe { std::env::remove_var("OPENAI_API_KEY") },
        }
        assert!(
            !result,
            "missing OPENAI_API_KEY must warn and return false, not save a config that can't run"
        );
    }

    #[test]
    fn check_api_key_and_warn_openai_passes_when_present() {
        let prev = std::env::var("OPENAI_API_KEY").ok();
        unsafe { std::env::set_var("OPENAI_API_KEY", "test-key") };
        let result =
            check_api_key_and_warn(crate::rag::embedding_client::EmbeddingProvider::OpenAi)
                .unwrap();
        match prev {
            Some(v) => unsafe { std::env::set_var("OPENAI_API_KEY", v) },
            None => unsafe { std::env::remove_var("OPENAI_API_KEY") },
        }
        assert!(result, "set OPENAI_API_KEY must pass without warning");
    }

    #[test]
    fn check_api_key_and_warn_gemini_warns_when_missing() {
        let prev = std::env::var("GEMINI_API_KEY").ok();
        unsafe { std::env::remove_var("GEMINI_API_KEY") };
        let result =
            check_api_key_and_warn(crate::rag::embedding_client::EmbeddingProvider::Gemini)
                .unwrap();
        match prev {
            Some(v) => unsafe { std::env::set_var("GEMINI_API_KEY", v) },
            None => unsafe { std::env::remove_var("GEMINI_API_KEY") },
        }
        assert!(
            !result,
            "missing GEMINI_API_KEY must warn and return false, not save a config that can't run"
        );
    }

    #[test]
    fn check_api_key_and_warn_gemini_passes_when_present() {
        let prev = std::env::var("GEMINI_API_KEY").ok();
        unsafe { std::env::set_var("GEMINI_API_KEY", "test-key") };
        let result =
            check_api_key_and_warn(crate::rag::embedding_client::EmbeddingProvider::Gemini)
                .unwrap();
        match prev {
            Some(v) => unsafe { std::env::set_var("GEMINI_API_KEY", v) },
            None => unsafe { std::env::remove_var("GEMINI_API_KEY") },
        }
        assert!(result, "set GEMINI_API_KEY must pass without warning");
    }

    #[test]
    fn theme_choice_writes_modern_for_the_modern_menu_label() {
        assert_eq!(theme_choice_to_config_value(THEME_OPTION_MODERN), "modern");
    }

    #[test]
    fn theme_choice_writes_classic_for_the_classic_menu_label() {
        assert_eq!(
            theme_choice_to_config_value(THEME_OPTION_CLASSIC),
            "classic"
        );
    }

    #[test]
    fn theme_choice_defaults_unrecognized_input_to_classic() {
        // Defensive: any label that isn't the modern one falls back to classic
        // rather than writing an unexpected value to config.
        assert_eq!(theme_choice_to_config_value("not a real option"), "classic");
    }

    #[test]
    fn theme_choice_modern_constant_value() {
        assert_eq!(THEME_OPTION_MODERN, "Modern (borderless)");
    }

    #[test]
    fn wizard_lists_modern_again() {
        let source = include_str!("wizard.rs");
        assert!(source.contains("vec![THEME_OPTION_CLASSIC, THEME_OPTION_MODERN]"));
    }

    #[test]
    fn theme_choice_classic_constant_value() {
        assert_eq!(THEME_OPTION_CLASSIC, "Classic (bordered)");
    }

    #[test]
    fn wizard_state_new_is_empty() {
        let wiz = WizardState::new();
        assert!(wiz.steps.is_empty());
    }

    #[test]
    fn wizard_state_add_stores_steps() {
        let mut wiz = WizardState::new();
        wiz.add("step 1".to_string());
        wiz.add("step 2".to_string());
        assert_eq!(wiz.steps.len(), 2);
        assert_eq!(wiz.steps[0], "step 1");
        assert_eq!(wiz.steps[1], "step 2");
    }

    #[test]
    fn wizard_state_render_returns_ok() {
        // render() calls clear_wizard_screen() which does I/O, but we test
        // that the function at least constructs without panic.
        let wiz = WizardState::new();
        // This may fail in headless CI (no terminal), but the test compiles
        // and demonstrates the function is reachable.
        let _ = wiz.render();
    }

    #[test]
    fn wizard_state_add_preserves_order() {
        let mut wiz = WizardState::new();
        for i in 0..10 {
            wiz.add(format!("step {i}"));
        }
        for (i, step) in wiz.steps.iter().enumerate() {
            assert_eq!(*step, format!("step {i}"));
        }
    }

    // ── Additional edge cases ────────────────────────────────────

    #[test]
    fn theme_choice_modern_roundtrip() {
        let result = theme_choice_to_config_value(THEME_OPTION_MODERN);
        assert_eq!(result, "modern");
    }

    #[test]
    fn theme_choice_classic_roundtrip() {
        let result = theme_choice_to_config_value(THEME_OPTION_CLASSIC);
        assert_eq!(result, "classic");
    }

    #[test]
    fn theme_choice_empty_string() {
        assert_eq!(theme_choice_to_config_value(""), "classic");
    }

    #[test]
    fn theme_choice_arbitrary_string() {
        assert_eq!(theme_choice_to_config_value("anything"), "classic");
    }

    #[test]
    fn wizard_state_new_has_zero_steps() {
        let wiz = WizardState::new();
        assert_eq!(wiz.steps.len(), 0);
    }

    #[test]
    fn wizard_state_add_single_step() {
        let mut wiz = WizardState::new();
        wiz.add("single step".to_string());
        assert_eq!(wiz.steps.len(), 1);
        assert_eq!(wiz.steps[0], "single step");
    }

    #[test]
    fn wizard_state_add_many_steps() {
        let mut wiz = WizardState::new();
        for i in 0..100 {
            wiz.add(format!("step {i}"));
        }
        assert_eq!(wiz.steps.len(), 100);
    }

    #[test]
    fn theme_choice_case_sensitivity() {
        // "Modern (borderless)" is the exact constant
        assert_eq!(
            theme_choice_to_config_value("Modern (borderless)"),
            "modern"
        );
        // Different casing should fall back to classic
        assert_eq!(
            theme_choice_to_config_value("modern (borderless)"),
            "classic"
        );
    }

    /// CB19: the wizard must never tell the user to hand-edit config.toml
    /// for embeddings when remote providers are available — that was the
    /// old dead-end message when local-embeddings was missing.
    #[test]
    fn wizard_does_not_send_user_to_hand_edit_config_for_embeddings() {
        let source = include_str!("wizard.rs");
        let production = source
            .split("mod tests {")
            .next()
            .expect("wizard.rs always has a tests module");
        assert!(
            !production.contains("configure a cloud embeddings model manually in config.toml"),
            "old hand-edit dead-end must be gone"
        );
        assert!(
            !production.contains("RAG: disabled — build lacks local-embeddings support"),
            "wizard must not disable RAG solely because local-embeddings is missing"
        );
        assert!(
            !production.contains("This canopy build cannot run local embedding models"),
            "wizard must offer remotes instead of refusing on missing local support"
        );
    }

    /// CB19: run_setup must gate remote providers on the key check's bool —
    /// if someone reverts to discarding `check_api_key_and_warn`'s return,
    /// this source scan fails. The interactive bail itself needs a TTY.
    #[test]
    fn run_setup_refuses_remote_provider_when_api_key_missing() {
        let source = include_str!("wizard.rs");
        let production = source
            .split("mod tests {")
            .next()
            .expect("wizard.rs always has a tests module");
        assert!(
            production.contains("if !check_api_key_and_warn("),
            "run_setup must gate on check_api_key_and_warn's bool, not discard it"
        );
        assert!(
            production.contains("OPENAI_API_KEY is not set"),
            "missing OpenAI key must bail with an actionable message"
        );
        assert!(
            production.contains("GEMINI_API_KEY is not set"),
            "missing Gemini key must bail with an actionable message"
        );
    }

    // ── CB58: wizard save merges the registry into config.clis ─────

    fn cb58_cli(name: &str, binary: &str) -> crate::domain::cli_config::CliConfig {
        crate::domain::cli_config::CliConfig {
            name: name.to_string(),
            binary: binary.to_string(),
            ..Default::default()
        }
    }

    fn cb58_platform_with_cli(
        name: &str,
        cli: crate::domain::cli_config::CliConfig,
    ) -> PlatformWithCli {
        PlatformWithCli {
            name: name.to_string(),
            config_path: format!("{name}.marker"),
            cli: Some(cli),
        }
    }

    /// The literal line CB58 is about must never come back: the wizard's
    /// save path must not assign the registry's CLI list wholesale.
    #[test]
    fn wizard_save_path_has_no_whole_list_assignment_from_the_registry() {
        let source = include_str!("wizard.rs");
        let production = source
            .split("mod tests {")
            .next()
            .expect("wizard.rs always has a tests module");
        assert!(
            !production.contains("config.clis = cli_registry.available_clis"),
            "the wizard must merge the registry's CLI list, not replace config.clis wholesale"
        );
    }

    #[test]
    fn merge_preserves_hand_set_infra_retry_limit_and_drifted_model_flag_while_updating_stale_headless_mode(
    ) {
        use crate::domain::canopy_config::CanopyConfig;
        use crate::domain::cli_config::CliConfig;
        use crate::domain::registry_baseline::RegistryBaseline;

        // Platform A: registry corrects a stale headless_mode the user never
        // touched, but the user's hand-set infra_retry_limit = 0 must survive
        // regardless of what the baseline says about it (registry never
        // publishes infra_* fields, so baseline.infra_retry_limit is always
        // None here -- this is the exact CB58 incident shape).
        let local_a = CliConfig {
            headless_mode: "--old-headless".to_string(),
            infra_retry_limit: Some(0),
            ..cb58_cli("opencode", "ls")
        };
        let baseline_a = CliConfig {
            headless_mode: "--old-headless".to_string(),
            ..cb58_cli("opencode", "ls")
        };
        let registry_a = CliConfig {
            headless_mode: "--new-headless".to_string(),
            ..cb58_cli("opencode", "ls")
        };

        // Platform B: user drifted model_flag away from the baseline
        // (deliberate edit) -- must survive even though the registry offers
        // a newer value.
        let baseline_b = CliConfig {
            model_flag: Some("--model-old".to_string()),
            ..cb58_cli("cursor", "ls")
        };
        let local_b = CliConfig {
            model_flag: Some("--model-custom".to_string()),
            ..baseline_b.clone()
        };
        let registry_b = CliConfig {
            model_flag: Some("--model-new".to_string()),
            ..cb58_cli("cursor", "ls")
        };

        let mut config = CanopyConfig {
            clis: vec![local_a, local_b],
            ..Default::default()
        };

        let cli_registry = crate::domain::cli_config::CliRegistry {
            version: 2,
            available_clis: vec![registry_a.clone(), registry_b.clone()],
        };
        let platforms_with_cli = vec![
            cb58_platform_with_cli("opencode", registry_a),
            cb58_platform_with_cli("cursor", registry_b),
        ];
        let baseline = RegistryBaseline {
            clis: vec![baseline_a, baseline_b],
            platforms: vec![],
        };

        let changed = merge_cli_registry_into_config(
            &mut config,
            &cli_registry,
            &platforms_with_cli,
            Some(&baseline),
        );

        let opencode = config.get_cli("opencode").unwrap();
        assert_eq!(
            opencode.infra_retry_limit,
            Some(0),
            "hand-set infra_retry_limit must survive"
        );
        assert_eq!(
            opencode.headless_mode, "--new-headless",
            "untouched field must still update"
        );

        let cursor = config.get_cli("cursor").unwrap();
        assert_eq!(
            cursor.model_flag.as_deref(),
            Some("--model-custom"),
            "drifted field must survive"
        );

        assert!(changed.contains(&"opencode.headless_mode".to_string()));
        assert!(
            !changed.iter().any(|f| f.contains("infra_retry_limit")),
            "infra_retry_limit must never be reported as registry-changed: {changed:?}"
        );
    }

    /// The exact CB58 regression: no baseline exists at all (e.g. first-ever
    /// setup run). Without the explicit infra_* restore, `merge_cli_fields`
    /// treats every field as user-untouched and would silently null out a
    /// hand-set infra_retry_limit.
    #[test]
    fn merge_preserves_infra_retry_limit_with_no_baseline_at_all() {
        use crate::domain::canopy_config::CanopyConfig;
        use crate::domain::cli_config::CliConfig;

        let local = CliConfig {
            headless_mode: "--old-headless".to_string(),
            infra_retry_limit: Some(0),
            infra_crash_max_seconds: Some(120),
            infra_backoff_seconds: Some(15),
            ..cb58_cli("opencode", "ls")
        };
        let registry = CliConfig {
            headless_mode: "--new-headless".to_string(),
            ..cb58_cli("opencode", "ls")
        };

        let mut config = CanopyConfig {
            clis: vec![local],
            ..Default::default()
        };

        let cli_registry = crate::domain::cli_config::CliRegistry {
            version: 2,
            available_clis: vec![registry.clone()],
        };
        let platforms_with_cli = vec![cb58_platform_with_cli("opencode", registry)];

        merge_cli_registry_into_config(&mut config, &cli_registry, &platforms_with_cli, None);

        let opencode = config.get_cli("opencode").unwrap();
        assert_eq!(opencode.infra_retry_limit, Some(0));
        assert_eq!(opencode.infra_crash_max_seconds, Some(120));
        assert_eq!(opencode.infra_backoff_seconds, Some(15));
        assert_eq!(opencode.headless_mode, "--new-headless");
    }

    #[test]
    fn merge_adds_newly_detected_platform() {
        use crate::domain::canopy_config::CanopyConfig;

        let mut config = CanopyConfig::default();
        let registry_cli = cb58_cli("brandnew", "ls");
        let cli_registry = crate::domain::cli_config::CliRegistry {
            version: 2,
            available_clis: vec![registry_cli.clone()],
        };
        let platforms_with_cli = vec![cb58_platform_with_cli("brandnew", registry_cli)];

        merge_cli_registry_into_config(&mut config, &cli_registry, &platforms_with_cli, None);

        assert!(config.get_cli("brandnew").is_some());
    }

    #[test]
    fn merge_removes_platform_whose_binary_vanished() {
        use crate::domain::canopy_config::CanopyConfig;

        let ghost_local = cb58_cli("ghost", "canopy-test-fixture-cli-missing-xyz");
        let mut config = CanopyConfig {
            clis: vec![ghost_local],
            ..Default::default()
        };

        // Registry still knows about "ghost" (it's in platforms_with_cli),
        // but its binary can never resolve, so `cli_registry.available_clis`
        // does not include it (CliRegistry::detect_available filters on
        // is_available()).
        let ghost_registry_entry = cb58_cli("ghost", "canopy-test-fixture-cli-missing-xyz");
        let cli_registry = crate::domain::cli_config::CliRegistry {
            version: 2,
            available_clis: vec![],
        };
        let platforms_with_cli = vec![cb58_platform_with_cli("ghost", ghost_registry_entry)];

        merge_cli_registry_into_config(&mut config, &cli_registry, &platforms_with_cli, None);

        assert!(config.get_cli("ghost").is_none());
    }

    #[test]
    fn merge_keeps_manually_added_cli_the_registry_has_never_heard_of() {
        use crate::domain::canopy_config::CanopyConfig;

        let manual = cb58_cli("manual-only", "ls");
        let mut config = CanopyConfig {
            clis: vec![manual],
            ..Default::default()
        };

        let cli_registry = crate::domain::cli_config::CliRegistry {
            version: 2,
            available_clis: vec![],
        };
        let platforms_with_cli: Vec<PlatformWithCli> = vec![];

        merge_cli_registry_into_config(&mut config, &cli_registry, &platforms_with_cli, None);

        assert!(
            config.get_cli("manual-only").is_some(),
            "a CLI the registry doesn't know about at all must never be removed"
        );
    }
}
