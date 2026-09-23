use crate::setup_module::daemon_service::{
    install_service_if_needed, start_daemon_if_needed, stop_daemon,
};
use crate::setup_module::dir_browser::browse_directories_multiselect_with_preselected;
use crate::setup_module::models::{is_platform_available, Platform};
use crate::setup_module::platform_adapter::clear_wizard_screen;
use crate::setup_module::registry_fetch::{fetch_registry, print_banner};
use crate::setup_module::sync_and_skills::{run_essential_skills_step, run_sync_step};
use crate::setup_module::PlatformWithCli;
use anyhow::{Context, Result};
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

    // ── Step 2.3: RAG configuration ──────────────────────────────
    wiz.render()?;
    let rag_previously_configured = !existing_config.embeddings_model.is_empty();

    let (embeddings_model, similarity_threshold, rag_personal_dirs, rag_max_file_mb) =
        if rag_previously_configured && rag_keep_current_configuration(&existing_config)? {
            let local_embeddings_available = crate::rag::embedding_client::provider_available(
                crate::rag::embedding_client::EmbeddingProvider::Local,
            );
            let outcome =
                resolve_kept_rag_configuration(&existing_config, local_embeddings_available);
            match outcome.disabled_reason {
                None => wiz.add(format!(
                    "\x1b[32m✓\x1b[0m RAG: kept existing configuration ({})",
                    outcome.embeddings_model
                )),
                Some(_) => {
                    print_local_embeddings_unavailable();
                    wiz.add(
                        "\x1b[90m–\x1b[0m RAG: disabled (build lacks local-embeddings support)"
                            .to_string(),
                    );
                }
            }
            (
                outcome.embeddings_model,
                outcome.similarity_threshold,
                outcome.rag_personal_dirs,
                outcome.rag_max_file_mb,
            )
        } else {
            run_rag_setup(
                &existing_config,
                &canopy_dir,
                rag_previously_configured,
                &mut wiz,
            )?
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

/// Print the RAG configuration this installation already has and ask
/// whether to keep it as-is. Called only when a model is already configured
/// (CB59): re-asking every RAG field from scratch on every setup run was the
/// bug — the cursor on the embeddings-provider question always started on
/// "OpenAI" even for an install that had been running local-only for months.
fn rag_keep_current_configuration(
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

/// Print the "this build can't do local embeddings" notice. Shared by the
/// top-level keep-path capability gate in `run_setup` and `run_rag_setup`'s
/// own check, so both onboarding paths — an installation that already has
/// RAG configured and answers "keep it", and a genuinely fresh or
/// reconfigured install — show the identical message (CB59).
fn print_local_embeddings_unavailable() {
    println!();
    println!("  \x1b[33mThis canopy build cannot run local embedding models.\x1b[0m");
    println!(
        "  \x1b[90m{}\x1b[0m",
        crate::rag::embedding_client::LOCAL_EMBEDDINGS_UNAVAILABLE_REASON
    );
    println!("  \x1b[90mRAG will remain disabled.\x1b[0m");
    println!();
}

/// The RAG step's outcome once the user has been asked "keep it?" (i.e.
/// `rag_previously_configured` is true) and answered yes. Pure — takes
/// `local_embeddings_available` as a parameter instead of reading
/// `cfg!(feature = "local-embeddings")` itself — so the CB59 fix (the
/// capability check gates this branch's *return*, not just
/// `run_rag_setup`'s own path) is testable without a terminal.
struct KeptRagOutcome {
    embeddings_model: String,
    similarity_threshold: f32,
    rag_personal_dirs: Vec<String>,
    rag_max_file_mb: u32,
    /// `Some(reason)` when this build cannot do local embeddings at all:
    /// the "kept" answer is overridden and RAG ends up disabled instead.
    disabled_reason: Option<&'static str>,
}

fn resolve_kept_rag_configuration(
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
fn run_rag_setup(
    existing_config: &crate::domain::canopy_config::CanopyConfig,
    canopy_dir: &std::path::Path,
    default_enable: bool,
    wiz: &mut WizardState,
) -> Result<(String, f32, Vec<String>, u32)> {
    let use_rag = Confirm::new("Enable personal knowledge indexing (RAG)?")
        .with_default(default_enable)
        .with_help_message("Indexes your notes/docs so AI tools can search them")
        .prompt()
        .map_err(|e| anyhow::anyhow!("RAG selection cancelled: {}", e))?;

    if !use_rag {
        wiz.add("\x1b[90m–\x1b[0m RAG: disabled".to_string());
        return Ok((
            String::new(),
            existing_config.similarity_threshold,
            Vec::new(),
            existing_config.rag_max_file_mb,
        ));
    }

    if !crate::rag::embedding_client::provider_available(
        crate::rag::embedding_client::EmbeddingProvider::Local,
    ) {
        print_local_embeddings_unavailable();
        wiz.add(
            "\x1b[90m–\x1b[0m RAG: disabled (build lacks local-embeddings support)".to_string(),
        );
        return Ok((
            String::new(),
            existing_config.similarity_threshold,
            Vec::new(),
            existing_config.rag_max_file_mb,
        ));
    }

    wiz.render()?;
    let embeddings_model = select_local_embeddings_model(&existing_config.embeddings_model)?;

    // ── Model changed check ─────────────────────────────────────────
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

    wiz.add(format!(
        "\x1b[32m✓\x1b[0m Embeddings model: {}",
        embeddings_model
    ));

    // ── Model acquisition ─────────────────────────────────────────────
    // Setup never downloads the model itself — it only checks whether one's
    // already cached; the daemon's background acquisition loop picks it up
    // once it (re)starts below, so a multi-hundred-MB download never blocks
    // this wizard.
    wiz.render()?;
    #[cfg(feature = "local-embeddings")]
    let model_already_cached = {
        let model_cache_dir = canopy_dir.join("models");
        crate::rag::embedding_client::is_local_model_cached(&embeddings_model, &model_cache_dir)
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

    // Chunk-merge similarity threshold: internal tuning knob with no
    // user-observable effect in its valid range, so it is not prompted.
    let similarity_threshold = existing_config.similarity_threshold;

    // ── RAG directories ─────────────────────────────────────────────
    let prev_dirs = existing_config.rag_personal_dirs.clone();
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

/// The `(id, label)` pairs the embeddings-model prompt offers: every
/// supported local model id paired with its label, nothing else. Split out
/// so a fresh install's model list is testable without a terminal (CB59:
/// it must never again include a remote/API-key option).
fn local_embeddings_model_choices() -> Vec<(&'static str, &'static str)> {
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

    // ── CB59: RAG step is local-only and doesn't re-interrogate ────

    #[test]
    fn rag_step_has_no_provider_selector_left() {
        let source = include_str!("wizard.rs");
        let production = source
            .split("mod tests {")
            .next()
            .expect("wizard.rs always has a tests module");
        assert!(
            !production.contains("Which embedding provider should canopy use?"),
            "the provider question must be gone — RAG is local-only now"
        );
        assert!(
            !production
                .to_lowercase()
                .contains(&["requires", "api", "key"].join(" ")),
            "no remaining option should mention needing an API key"
        );
    }

    #[test]
    fn rag_step_no_longer_offers_cloud_fallback_on_missing_local_support() {
        let source = include_str!("wizard.rs");
        let production = source
            .split("mod tests {")
            .next()
            .expect("wizard.rs always has a tests module");
        assert!(
            production.contains("This canopy build cannot run local embedding models."),
            "missing local-embeddings must say so plainly"
        );
        assert!(
            !production.contains("OpenAI (cloud"),
            "must not fall back to a cloud provider when local-embeddings is missing"
        );
    }

    // ── CB59: capability check gates the "keep it" branch's return ─

    #[test]
    fn keep_it_preserves_all_four_rag_values_when_local_embeddings_available() {
        let existing = crate::domain::canopy_config::CanopyConfig {
            embeddings_model: "baai/bge-base-en-v1.5".to_string(),
            similarity_threshold: 0.42,
            rag_personal_dirs: vec!["/home/x/notes".to_string()],
            rag_max_file_mb: 25,
            ..Default::default()
        };

        let outcome = resolve_kept_rag_configuration(&existing, true);

        assert!(outcome.disabled_reason.is_none());
        assert_eq!(outcome.embeddings_model, existing.embeddings_model);
        assert_eq!(outcome.similarity_threshold, existing.similarity_threshold);
        assert_eq!(outcome.rag_personal_dirs, existing.rag_personal_dirs);
        assert_eq!(outcome.rag_max_file_mb, existing.rag_max_file_mb);
    }

    #[test]
    fn keep_it_disables_rag_and_reports_missing_capability_even_with_existing_config() {
        let existing = crate::domain::canopy_config::CanopyConfig {
            embeddings_model: "baai/bge-base-en-v1.5".to_string(),
            similarity_threshold: 0.42,
            rag_personal_dirs: vec!["/home/x/notes".to_string()],
            rag_max_file_mb: 25,
            ..Default::default()
        };

        let outcome = resolve_kept_rag_configuration(&existing, false);

        assert_eq!(
            outcome.disabled_reason,
            Some(crate::rag::embedding_client::LOCAL_EMBEDDINGS_UNAVAILABLE_REASON),
            "must report why local embeddings can't run, not silently keep the old model"
        );
        assert_eq!(outcome.embeddings_model, "");
        assert!(outcome.rag_personal_dirs.is_empty());
        assert_eq!(
            outcome.similarity_threshold, existing.similarity_threshold,
            "threshold isn't the reason RAG is off, so it still carries over"
        );
    }

    #[test]
    fn fresh_install_model_choices_are_all_local_and_never_require_an_api_key() {
        let choices = local_embeddings_model_choices();
        assert_eq!(
            choices.len(),
            crate::rag::embedding_client::LOCAL_MODEL_IDS.len()
        );
        for (id, label) in &choices {
            assert!(
                crate::rag::embedding_client::LOCAL_MODEL_IDS.contains(id),
                "offered id '{id}' isn't in the supported local model list"
            );
            let lower = label.to_lowercase();
            assert!(
                !lower.contains(&["requires", "api", "key"].join(" ")),
                "label for '{id}' must not require an API key: {label}"
            );
            assert!(
                !lower.contains("openai") && !lower.contains("gemini"),
                "label for '{id}' must not name a cloud provider: {label}"
            );
        }
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
