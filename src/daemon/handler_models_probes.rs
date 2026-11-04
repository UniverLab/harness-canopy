//! MCP handler tools split out of `daemon::handler` (model catalog, probe tools, and model display helpers).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::{
    format_catalog_models, format_duration_short, format_native_models, format_platform_models,
    format_system_time, internal_error,
};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::db::Database;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::ErrorData as McpError;

impl TaskTriggerHandler {
    /// List available AI models.
    pub(crate) async fn harness_models_impl(
        &self,
        params: TaskModelsParams,
    ) -> Result<CallToolResult, McpError> {
        let force_refresh = params.refresh.unwrap_or(false);
        let full = params.full.unwrap_or(false);

        // Optional platform filter, validated against the platforms actually
        // configured in canopy (registry-driven) when that config is present.
        let platform = params
            .platform
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty());

        if let Some(platform) = platform {
            if let Some(err) = validate_platform_configured(platform) {
                return Ok(error_result(&err));
            }
            // CM35: an interactive-only platform has no headless invocation to
            // enumerate models with — return an empty list with the reason
            // rather than run `models_list_cmd` or fall through to models.dev.
            if let Some(msg) = crate::daemon::handler_helpers::interactive_only_rejection(platform)
            {
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&serde_json::json!({
                        "platform": platform,
                        "models": [],
                        "reason": msg,
                    }))
                    .unwrap_or_default(),
                )]));
            }
            // Registry-driven: a platform that can enumerate its own models is
            // the authoritative source of passable ids (each line is the literal
            // string its model flag accepts, prefix and all). models.dev cannot
            // know a gateway's provider-prefixed form or its private catalog, so
            // when the registry gives us an enumeration command we use it and
            // skip models.dev entirely — never requiring it to be reachable.
            if let Some((binary, args)) = platform_enumeration_cmd(platform) {
                let mut result =
                    native_models_result(platform, binary, args, force_refresh, full).await;
                prepend_recent_usage(&mut result, &recent_usage_section(&self.db));
                return Ok(result);
            }
        }

        // models.dev-derived path: the all-providers listing, or a platform
        // without native enumeration (e.g. claude, whose bare ids are correct).
        let mut result = catalogue_models_result(platform, force_refresh, full).await;
        prepend_recent_usage(&mut result, &recent_usage_section(&self.db));
        Ok(result)
    }

    pub(crate) async fn harness_probe_impl(
        &self,
        params: AgentProbeParams,
    ) -> Result<CallToolResult, McpError> {
        if params.model.is_some() && params.platform.is_none() {
            return Ok(error_result(
                "`model` requires `platform` — omit both to probe every configured platform's \
                 default model.",
            ));
        }

        let Some(home) = dirs::home_dir() else {
            return Err(internal_error("No home directory"));
        };
        let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));

        let targets: Vec<crate::daemon::probe::ProbeTarget> = match params.platform.as_deref() {
            Some(platform) => vec![crate::daemon::probe::ProbeTarget {
                platform: platform.to_string(),
                model: params.model.clone(),
                effort: None,
            }],
            None => config
                .clis
                .iter()
                .map(|cli| crate::daemon::probe::ProbeTarget {
                    platform: cli.name.clone(),
                    model: None,
                    effort: None,
                })
                .collect(),
        };

        if targets.is_empty() {
            return Ok(error_result(
                "No platforms configured in canopy. Run 'canopy setup'.",
            ));
        }

        let timeout_secs = clamp_probe_timeout(params.timeout_seconds);
        let reports = crate::daemon::probe::probe_targets(
            &config,
            &targets,
            None,
            std::time::Duration::from_secs(timeout_secs),
        )
        .await;

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "timeout_seconds": timeout_secs,
                "would_fail": crate::daemon::probe::would_fail_count(&reports),
                "unknown": crate::daemon::probe::unknown_count(&reports),
                "probes": reports.iter().map(|r| {
                    let mut v = r.to_json();
                    v["platform_display"] = serde_json::json!(display_for_slug(&config, &r.platform));
                    v
                }).collect::<Vec<_>>(),
            }))
            .unwrap_or_default(),
        )]))
    }

    pub(crate) async fn harness_probe_recent_impl(
        &self,
        params: AgentProbeRecentParams,
    ) -> Result<CallToolResult, McpError> {
        let since = chrono::Utc::now()
            - chrono::Duration::days(crate::db::graphs::RECENT_USAGE_WINDOW_DAYS);
        let sources = self
            .db
            .list_recent_platform_model_usage_sources(since)
            .map_err(|e| internal_error(e.to_string()))?;
        let mut grouped = crate::daemon::probe::group_recent_usage(&sources);
        let pairs_considered = grouped.len();
        let max_pairs = clamp_max_recent_pairs(params.max_pairs);
        grouped.truncate(max_pairs);
        if grouped.is_empty() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::json!({
                    "window_days": crate::db::graphs::RECENT_USAGE_WINDOW_DAYS,
                    "pairs_considered": 0,
                    "pairs_probed": 0,
                    "message": "Nothing to sweep: no recently-used platform+model pairs were recorded.",
                }).to_string(),
            )]));
        }

        let home = dirs::home_dir().ok_or_else(|| internal_error("No home directory"))?;
        let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
        let targets: Vec<_> = grouped.iter().map(|pair| pair.target.clone()).collect();
        let timeout_secs = clamp_probe_timeout(params.timeout_seconds);
        let reports = crate::daemon::probe::probe_targets(
            &config,
            &targets,
            None,
            std::time::Duration::from_secs(timeout_secs),
        )
        .await;
        let mut combined: Vec<_> = reports.iter().zip(grouped.iter()).collect();
        combined.sort_by_key(|(report, _)| {
            if report.outcome == crate::daemon::probe::ProbeOutcome::Unknown {
                1
            } else if !report.outcome.reachable() {
                0
            } else {
                2
            }
        });
        let probes = combined
            .iter()
            .map(|(report, pair)| {
                let mut value = report.to_json();
                if let serde_json::Value::Object(map) = &mut value {
                    map.insert("used_by".into(), serde_json::json!(pair.used_by));
                    map.insert(
                        "platform_display".into(),
                        serde_json::json!(display_for_slug(&config, &report.platform)),
                    );
                    map.insert(
                        "last_run".into(),
                        serde_json::json!(pair.last_run.to_rfc3339()),
                    );
                    map.insert("run_count".into(), serde_json::json!(pair.run_count));
                    map.insert("last_outcome".into(), serde_json::json!(pair.last_outcome));
                }
                value
            })
            .collect::<Vec<_>>();
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "window_days": crate::db::graphs::RECENT_USAGE_WINDOW_DAYS,
                "pairs_considered": pairs_considered,
                "pairs_probed": grouped.len(),
                "max_pairs": max_pairs,
                "timeout_seconds": timeout_secs,
                "would_fail": crate::daemon::probe::would_fail_count(&reports),
                "unknown": crate::daemon::probe::unknown_count(&reports),
                "probes": probes,
            }))
            .unwrap_or_default(),
        )]))
    }
}

/// The default probe timeout from the agents config.
pub(crate) fn default_probe_timeout(agents: &crate::domain::canopy_config::AgentsConfig) -> u64 {
    agents.probe_timeout_seconds
}

/// Resolve `harness probe`/`graph_control`'s `timeout_seconds` param to an
/// actual bound: the configured default when omitted, clamped to
/// `[MIN_PROBE_TIMEOUT_SECS, MAX_PROBE_TIMEOUT_SECS]` otherwise — a probe is
/// a liveness check, not a capability benchmark, so callers can't ask for an
/// unbounded wait.
pub(crate) fn clamp_probe_timeout(requested: Option<u64>) -> u64 {
    let canopy_dir = crate::domain::prompts::canopy_dir();
    let agents = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir).agents;
    requested.unwrap_or(default_probe_timeout(&agents)).clamp(
        crate::daemon::probe::MIN_PROBE_TIMEOUT_SECS,
        crate::daemon::probe::MAX_PROBE_TIMEOUT_SECS,
    )
}

pub(crate) fn clamp_max_recent_pairs(requested: Option<u32>) -> usize {
    requested.unwrap_or(15).clamp(1, 50) as usize
}

/// Human display string for a platform slug ("Provider · Tool" when the
/// registry knows both, slug otherwise). Probe JSON keeps `"platform"` as
/// the slug and carries this as the sibling `"platform_display"` key —
/// display is never matched on or accepted as input.
pub(crate) fn display_for_slug(
    config: &crate::domain::canopy_config::CanopyConfig,
    slug: &str,
) -> String {
    config
        .get_cli(slug)
        .map(|c| c.display_name())
        .unwrap_or_else(|| slug.to_string())
}

/// Validate a requested `harness models` platform against the CLIs actually
/// configured in canopy (registry-driven, from `~/.canopy/config.toml`).
/// Returns `Some(error_message)` if config exists and the platform isn't among
/// the configured CLIs; `None` when the platform is configured, or when no
/// config is present yet (stay lenient rather than rejecting everything).
pub(crate) fn validate_platform_configured(platform: &str) -> Option<String> {
    let home = dirs::home_dir()?;
    let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
    let configured = config.cli_names();
    if configured.is_empty() || configured.contains(&platform) {
        return None;
    }
    Some(format!(
        "Platform '{platform}' is not configured in canopy. Configured platforms: {}. \
         Omit `platform` to list all providers.",
        configured.join(", ")
    ))
}

pub(crate) fn platform_model_selection_warning(platform: &str) -> Option<String> {
    let home = dirs::home_dir()?;
    let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
    let cli = config.get_cli(platform)?;
    if crate::domain::cli_config::model_flag_selects_model(cli.model_flag.as_deref()) {
        return None;
    }
    Some(format!(
        "Platform '{}' does not support model selection (no model_flag configured).\n\
         This platform addresses named agents, not models.\n\
         Omit the `model` field when using this platform — the CLI will use its own default.\n\
         \n\
         The ids listed below are NOT valid input for the `model` field.",
        platform
    ))
}

/// The `(binary, enumeration args)` for a platform that can list its own
/// models, read straight from its registry-driven `CliConfig` — `None` when the
/// platform has no such command configured (so nothing is inferred from the CLI
/// name). This is the seam that decides opencode enumerates via `opencode
/// models` while claude does not.
pub(crate) fn platform_enumeration_cmd(platform: &str) -> Option<(String, String)> {
    let home = dirs::home_dir()?;
    let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
    let cli = config.get_cli(platform)?;
    let args = cli.models_list_cmd.as_deref()?.trim();
    if args.is_empty() || cli.binary.is_empty() {
        return None;
    }
    Some((cli.binary.clone(), args.to_string()))
}

/// Build the `harness models` result from a platform's native enumeration. The
/// CLI run happens off the async executor (it may spawn a process); a fresh
/// cache returns with no CLI call at all.
pub(crate) async fn native_models_result(
    platform: &str,
    binary: String,
    args: String,
    force_refresh: bool,
    full: bool,
) -> CallToolResult {
    let platform_owned = platform.to_string();
    let ttl = configured_models().native_ttl();
    let load = tokio::task::spawn_blocking(move || {
        crate::domain::models_db::load_native_models(
            &platform_owned,
            &binary,
            &args,
            force_refresh,
            ttl,
        )
    })
    .await
    .ok()
    .flatten();

    let Some(load) = load else {
        // Enumeration failed — fall back to the provider catalogue rather than
        // returning a hard error, per FR3. The NOTE tells the caller what
        // happened; the catalogue path provides whatever models.dev knows about.
        let note = format!(
            "NOTE: '{platform}' model enumeration failed (CLI unreachable); showing \
             the provider catalogue instead.\n\n"
        );
        let fallback_text = catalogue_models_text(Some(platform), force_refresh, full).await;
        return match fallback_text {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(format!("{note}{text}"))]),
            Err(_) => error_result(&format!(
                "Could not enumerate '{platform}' models and no provider catalogue \
                 is available as fallback. Retry once the CLI is reachable."
            )),
        };
    };
    let crate::domain::models_db::NativeLoad { catalog, source } = load;

    let (listing, truncation) = format_native_models(&catalog.ids, full);
    let mut listing = format!(
        "Models available to platform '{platform}' (enumerated from the CLI — ids are \
         passable verbatim):\n{listing}"
    );
    // FR4: a platform can expose a `models_list_cmd` while still addressing
    // named agents rather than models (no `model_flag`) — the 2026-08-13
    // `mistral` shape. In that case the ids above are not valid `model`
    // input regardless of where they were enumerated from, so the same
    // warning the models.dev path prints must lead here too.
    if let Some(warning) = platform_model_selection_warning(platform) {
        listing = format!("{warning}\n\n{listing}");
    }
    CallToolResult::success(vec![ContentBlock::text(model_result_footer(
        &listing,
        source,
        catalog.fetched_at,
        ttl,
        &truncation,
        true,
    ))])
}

/// Why [`catalogue_models_text`] could not produce a listing — the two cases
/// have different remedies, so the caller must tell them apart.
pub(crate) enum CatalogTextError {
    /// `platform` has no models.dev provider mapping at all, so no id list can
    /// be derived from the catalogue. Not a transient outage — retrying or
    /// `refresh: true` will not help.
    NoProviderMapping,
    /// models.dev is unreachable and no local cache exists.
    Unavailable,
}

/// Format the models.dev provider catalogue as text (without wrapping in a
/// `CallToolResult`). Returns [`CatalogTextError`] when no listing can be
/// produced. Used by both `catalogue_models_result` (normal path) and
/// `native_models_result` (enumeration-failure fallback).
pub(crate) async fn catalogue_models_text(
    platform: Option<&str>,
    force_refresh: bool,
    full: bool,
) -> Result<String, CatalogTextError> {
    let ttl = configured_models().catalog_ttl();
    let load = tokio::task::spawn_blocking(move || {
        crate::domain::models_db::load_catalog_with_source(force_refresh, ttl)
    })
    .await
    .ok()
    .flatten()
    .ok_or(CatalogTextError::Unavailable)?;

    let crate::domain::models_db::CatalogLoad { catalog, source } = load;

    let (listing, truncation) = match platform {
        Some(platform) => {
            let providers = crate::domain::models_db::providers_for_cli(platform);
            if providers.is_empty() {
                return Err(CatalogTextError::NoProviderMapping);
            }
            let (formatted, trunc) = format_platform_models(&catalog, providers, full);
            let mut listing = format!(
                "Models available to platform '{platform}' (providers: {}):\n{formatted}",
                providers.join(", ")
            );
            if let Some(warning) = platform_model_selection_warning(platform) {
                listing = format!("{warning}\n\n{listing}");
            }
            (listing, trunc)
        }
        None => {
            let (formatted, trunc) = format_catalog_models(&catalog, full);
            (
                format!("Available models (use the model id as the model field):\n{formatted}"),
                trunc,
            )
        }
    };

    Ok(model_result_footer(
        &listing,
        source,
        catalog.fetched_at,
        ttl,
        &truncation,
        false,
    ))
}

/// Build the `harness models` result from the models.dev provider catalogue.
/// Used by `harness_models_impl` for platforms without native enumeration.
pub(crate) async fn catalogue_models_result(
    platform: Option<&str>,
    force_refresh: bool,
    full: bool,
) -> CallToolResult {
    match catalogue_models_text(platform, force_refresh, full).await {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
        Err(CatalogTextError::NoProviderMapping) => error_result(&format!(
            "No known model providers are mapped for platform '{}'. \
             Omit `platform` to list all providers.",
            platform.unwrap_or_default(),
        )),
        Err(CatalogTextError::Unavailable) => error_result(
            "Model catalog unavailable: could not reach models.dev and no local \
             cache exists at ~/.canopy/cache/models_catalog.json. Omit the model field to \
             use the CLI's default, or retry once network access is restored.",
        ),
    }
}

/// This daemon's `[models]` TTL configuration, read fresh from
/// `~/.canopy/config.toml` on every call (like `rag_max_file_bytes`) so a
/// config edit is picked up on the next `harness models` call without
/// restarting the daemon. Falls back to the compiled defaults when no home
/// directory or config file can be found.
pub(crate) fn configured_models() -> crate::domain::canopy_config::ModelsConfig {
    dirs::home_dir()
        .map(|home| crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy")).models)
        .unwrap_or_default()
}

/// CB43: the "recently used" section that leads `harness models` output — one
/// line per platform+model pair this installation has run in the last 30
/// days (most recent first, capped at fifteen): when it last ran, how many
/// times, and how the most recent run ended. Computed only from this
/// installation's own runs; a pair never run here never appears. The
/// catalogue below is unchanged.
pub(crate) fn recent_usage_section(db: &Database) -> String {
    let since =
        chrono::Utc::now() - chrono::Duration::days(crate::db::graphs::RECENT_USAGE_WINDOW_DAYS);
    match db.list_recent_platform_model_usage(since, 15) {
        Ok(rows) => format_recent_usage_section(&rows),
        Err(_) => String::new(),
    }
}

/// CB43: render the section (testable without a database). A NULL model is
/// the CLI's default. The last outcome is what keeps the list honest: a
/// pair used forty times whose last run timed out must not read as a
/// recommendation.
pub(crate) fn format_recent_usage_section(rows: &[crate::db::graphs::RecentModelUsage]) -> String {
    const HEADER: &str =
        "Recently used on this machine (last 30 days) — from this installation's own runs:";
    if rows.is_empty() {
        return format!("{HEADER}\nNo models have run on this installation in the last 30 days.");
    }
    let now = chrono::Utc::now();
    let mut out = String::from(HEADER);
    for row in rows {
        let model = row
            .model
            .as_deref()
            .filter(|model| !model.is_empty())
            .unwrap_or("(default)");
        let age = now
            .signed_duration_since(row.last_run)
            .to_std()
            .map(format_duration_short)
            .unwrap_or_else(|_| "0s".to_string());
        out.push_str(&format!(
            "\n- {}/{}: last used {} ago ({}), {} runs, last outcome: {}",
            row.platform,
            model,
            age,
            row.last_run.to_rfc3339(),
            row.count,
            row.last_outcome,
        ));
    }
    out
}

/// CB43: lead a successful `harness models` result with the recently-used
/// section. Error results (unknown platform, unreachable catalogue) are left
/// untouched — the section informs a model choice, and there is no choice
/// on offer when the listing itself failed.
pub(crate) fn prepend_recent_usage(result: &mut CallToolResult, section: &str) {
    if section.is_empty() || result.is_error == Some(true) {
        return;
    }
    let existing = result
        .content
        .first()
        .and_then(|content| content.as_text().map(|text| text.text.clone()));
    if let Some(existing) = existing {
        if !result.content.is_empty() {
            result.content[0] = ContentBlock::text(format!("{section}\n\n{existing}"));
        }
    }
}

/// The shared provenance/footer block for `harness models`, used by both the
/// models.dev and native-enumeration paths. States the cache's age and
/// whether a refresh is due, not just its timestamp — and when the source was
/// unreachable, folds how stale the served cache is into that same notice.
pub(crate) fn model_result_footer(
    listing: &str,
    source: crate::domain::models_db::CatalogSource,
    fetched_at: std::time::SystemTime,
    ttl: std::time::Duration,
    truncation: &crate::daemon::handler_formatting::ModelTruncation,
    from_cli: bool,
) -> String {
    let age = fetched_at.elapsed().unwrap_or_default();
    let age_str = format_duration_short(age);
    let ttl_str = format_duration_short(ttl);
    let refresh_due = age >= ttl;

    let provenance_note = if source == crate::domain::models_db::CatalogSource::Stale {
        format!(
            " (source unreachable — serving a cache that is already {age_str} old, past \
             the {ttl_str} refresh interval; retry with refresh: true once it's reachable)"
        )
    } else if refresh_due {
        " (refresh due — pass refresh: true to update now)".to_string()
    } else {
        String::new()
    };

    let truncation_notice = truncation
        .notice()
        .map(|n| format!("\n{n}"))
        .unwrap_or_default();
    if from_cli {
        format!(
            "{listing}\n\n\
             Source: CLI enumeration — every id above is accepted verbatim by this \
             platform's model flag. {}{provenance_note} · age: {age_str} \
             (refresh interval {ttl_str}) · fetched_at: {}{truncation_notice}",
            source.as_str(),
            format_system_time(fetched_at),
        )
    } else {
        format!(
            "{listing}\n\n\
             Source: provider catalogue (models.dev) — advertised, not \
             account-verified. {}{provenance_note} · age: {age_str} \
             (refresh interval {ttl_str}) · fetched_at: {}\n\
             WARNING: This lists the PROVIDER'S CATALOG, not what your account can use.\n\
             Actual availability depends on your API key tier, account type, and region.\n\
             A model listed here may still fail at runtime — use `harness probe` or `graph_control preflight`\n\
             to validate a specific platform+model pair before relying on it.{truncation_notice}",
            source.as_str(),
            format_system_time(fetched_at),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_probe_timeout_reads_from_config() {
        let agents = crate::domain::canopy_config::AgentsConfig {
            probe_timeout_seconds: 45,
        };
        assert_eq!(default_probe_timeout(&agents), 45);
    }
}
