use super::registry_platforms::{reconcile_platforms, warn_platform_reconciliation};
use crate::application::ports::StateRepository;
use crate::db::Database;
use crate::domain::cli_config::CliConfig;
use crate::domain::db_paths::database_path;
use crate::domain::registry_baseline::RegistryBaseline;
use crate::setup_module::models::{resolve_config_path, CanonicalServers, Platform, RegistryRaw};
use crate::setup_module::PlatformWithCli;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Global override for local registry development mode.
/// When set, canopy reads registry files from this local directory
/// instead of fetching from the remote GitHub repository.
static LOCAL_REGISTRY_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Set the local registry path for development mode.
/// This should be called early (e.g. in main.rs CLI parsing) before any registry fetch.
pub fn set_local_registry(path: PathBuf) {
    let _ = LOCAL_REGISTRY_PATH.set(path);
}

/// Get the currently configured local registry path, if any.
/// `pub` so the `canopy mcp` wizard can label which registry source it used.
pub fn local_registry_path() -> Option<&'static PathBuf> {
    LOCAL_REGISTRY_PATH.get()
}

/// Outcome of a [`refresh_registry_inner`] run, suitable for surfacing to an
/// operator via the `canopy registry refresh` CLI command or the
/// `harness refresh_registry` action.
#[derive(Debug, Serialize)]
pub(crate) struct RegistryRefreshReport {
    /// Registry-owned CLI fields that were rewritten on platforms the operator
    /// already had configured, keyed by platform name.
    pub(crate) changed: BTreeMap<String, Vec<String>>,
    /// Local overrides (fields that drifted from the baseline) that were
    /// preserved verbatim, keyed by platform name.
    pub(crate) kept: BTreeMap<String, Vec<String>>,
    /// Where the fetched registry was read from: `commit <sha7>` for a pinned
    /// fetch, `branch main (...)` after a failed commit lookup, or
    /// `local registry <path>`.
    pub(crate) source: String,
    /// Platforms newly added to the baseline this refresh.
    pub(crate) added: Vec<String>,
    /// Platforms whose structural definition was updated this refresh.
    pub(crate) updated: Vec<String>,
    /// Platforms removed from the registry.
    pub(crate) deleted: Vec<String>,
    /// Executable CLI changes staged for approval, not applied.
    pub(crate) pending: Vec<super::registry_pending::PendingChange>,
    /// True only when this refresh changed nothing.
    pub(crate) up_to_date: bool,
}

impl RegistryRefreshReport {
    /// Human-readable, one line per affected platform — or a single
    /// "registry already up to date" line when there was nothing to report —
    /// always ending with the `source:` line naming where it was read from.
    pub(crate) fn render_text(&self) -> String {
        if self.up_to_date {
            return format!("registry already up to date\nsource: {}", self.source);
        }

        let mut lines: Vec<String> = Vec::new();
        for (platform, fields) in &self.changed {
            if !fields.is_empty() {
                lines.push(format!("{}: changed {}", platform, fields.join(", ")));
            }
        }
        for (platform, fields) in &self.kept {
            if !fields.is_empty() {
                lines.push(format!(
                    "{}: kept local override {}",
                    platform,
                    fields.join(", ")
                ));
            }
        }
        for platform in &self.added {
            lines.push(format!("{}: added", platform));
        }
        for platform in &self.updated {
            lines.push(format!("{}: updated", platform));
        }
        for platform in &self.deleted {
            lines.push(format!("{}: removed", platform));
        }
        for held in &self.pending {
            lines.push(format!(
                "{}: held {} (waiting for approval)",
                held.cli, held.field
            ));
        }
        if !self.pending.is_empty() {
            lines.push(format!(
                "{} registry change(s) waiting for approval; run canopy registry pending",
                self.pending.len()
            ));
        }

        if lines.is_empty() {
            format!("registry already up to date\nsource: {}", self.source)
        } else {
            lines.push(format!("source: {}", self.source));
            lines.join("\n")
        }
    }
}

/// Group `cli.key` style entries (as produced by [`merge_cli_fields`]) into a
/// per-platform map of bare field names.
fn group_by_platform(entries: Vec<String>) -> BTreeMap<String, Vec<String>> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in entries {
        if let Some(pos) = entry.find('.') {
            let (platform, rest) = entry.split_at(pos);
            let key = rest[1..].to_string();
            grouped.entry(platform.to_string()).or_default().push(key);
        }
    }
    grouped
}

/// Lightweight index for the per-platform registry (v6).
#[derive(Deserialize)]
struct RegistryIndex {
    #[allow(dead_code)]
    version: u32,
    platforms: Vec<IndexEntry>,
}

#[derive(Deserialize)]
struct IndexEntry {
    name: String,
    #[allow(dead_code)]
    binary: String,
}

/// Legacy index (v5, JSON).
#[derive(Deserialize)]
struct LegacyRegistryIndex {
    #[allow(dead_code)]
    version: u32,
    platforms: Vec<IndexEntry>,
}

const REGISTRY_API_URL: &str =
    "https://api.github.com/repos/UniverLab/canopy-registry/commits/main";
const REGISTRY_RAW_ROOT: &str = "https://raw.githubusercontent.com/UniverLab/canopy-registry";

/// How often to refresh the registry in the background (24 hours).
const REGISTRY_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Fetch the platform registry (public for use by config commands) with the
/// label naming where it was read from (see [`super::registry_source`]).
#[allow(dead_code)]
pub fn fetch_registry_raw() -> Result<(RegistryRaw, String)> {
    fetch_registry()
}

pub(crate) fn fetch_registry() -> Result<(RegistryRaw, String)> {
    // Development mode: read from local filesystem if configured
    if let Some(local_path) = local_registry_path() {
        if let Some(reg) = try_fetch_local(local_path) {
            return Ok((reg, format!("local registry {}", local_path.display())));
        }
        anyhow::bail!(
            "Local registry not found or invalid at: {}",
            local_path.display()
        );
    }

    fetch_registry_from(REGISTRY_API_URL, REGISTRY_RAW_ROOT)
}

/// Fetch the remote registry with injectable endpoints (production values are
/// [`REGISTRY_API_URL`] and [`REGISTRY_RAW_ROOT`]). The commit sha is resolved
/// once and every file of this refresh is read under it, so the index and the
/// platform files can never come from two different commits.
pub(crate) fn fetch_registry_from(api_url: &str, raw_root: &str) -> Result<(RegistryRaw, String)> {
    let client = reqwest::blocking::Client::new();
    let base = super::registry_source::resolve_registry_base(&client, api_url, raw_root);

    // Try v6 (TOML) first
    if let Some(reg) = try_fetch_v6(&client, &base.base_url) {
        return Ok((reg, base.source));
    }

    // Try v5 (JSON per-platform)
    if let Some(reg) = try_fetch_v5(&client, &base.base_url) {
        return Ok((reg, base.source));
    }

    // Fallback: legacy monolithic platforms.json (v4)
    let response = client
        .get(format!("{}platforms.json", base.base_url))
        .header("User-Agent", "canopy")
        .send()
        .context("Failed to connect to platform registry")?;

    if !response.status().is_success() {
        anyhow::bail!("Registry returned HTTP {}", response.status());
    }

    #[derive(Deserialize)]
    struct LegacyRaw {
        platforms: Vec<Platform>,
    }

    let legacy: LegacyRaw = response.json().context("Invalid registry JSON")?;
    Ok((
        RegistryRaw {
            platforms: legacy.platforms,
            canonical_servers: CanonicalServers::default(),
        },
        base.source,
    ))
}

/// Try reading registry v6 from a local directory (development mode).
/// Expects the directory to contain: index.toml, servers.toml, platforms/*.toml
fn try_fetch_local(base: &Path) -> Option<RegistryRaw> {
    let index_path = base.join("index.toml");
    let index_text = std::fs::read_to_string(&index_path).ok()?;
    let index: RegistryIndex = toml::from_str(&index_text).ok()?;

    // Read canonical servers
    let servers_path = base.join("servers.toml");
    let canonical_servers: CanonicalServers = if servers_path.exists() {
        let text = std::fs::read_to_string(&servers_path).ok()?;
        toml::from_str(&text).unwrap_or_default()
    } else {
        CanonicalServers::default()
    };

    let platforms_dir = base.join("platforms");
    let mut platforms = Vec::new();
    for entry in &index.platforms {
        let file_path = platforms_dir.join(format!("{}.toml", entry.name));
        match std::fs::read_to_string(&file_path) {
            Ok(text) => match toml::from_str::<Platform>(&text) {
                Ok(p) => platforms.push(p),
                Err(e) => {
                    tracing::warn!("Failed to parse local platform '{}': {e}", entry.name);
                }
            },
            Err(e) => {
                tracing::warn!(
                    "Failed to read local platform file '{}': {e}",
                    file_path.display()
                );
            }
        }
    }

    Some(RegistryRaw {
        platforms,
        canonical_servers,
    })
}

/// Try fetching registry v6 (TOML index + servers + platforms) under one
/// commit-pinned `base` (see [`super::registry_source`]).
fn try_fetch_v6(client: &reqwest::blocking::Client, base: &str) -> Option<RegistryRaw> {
    let index_resp = client
        .get(format!("{base}index.toml"))
        .header("User-Agent", "canopy")
        .send()
        .ok()?;

    if !index_resp.status().is_success() {
        return None;
    }

    let index_text = index_resp.text().ok()?;
    let index: RegistryIndex = toml::from_str(&index_text).ok()?;

    // Fetch canonical servers
    let servers_resp = client
        .get(format!("{base}servers.toml"))
        .header("User-Agent", "canopy")
        .send()
        .ok()?;

    let canonical_servers: CanonicalServers = if servers_resp.status().is_success() {
        let text = servers_resp.text().ok()?;
        toml::from_str(&text).unwrap_or_default()
    } else {
        CanonicalServers::default()
    };

    let mut platforms = Vec::new();
    for entry in &index.platforms {
        let url = format!("{base}platforms/{}.toml", entry.name);
        match client
            .get(&url)
            .header("User-Agent", "canopy")
            .send()
            .and_then(|r| r.text())
        {
            Ok(text) => match toml::from_str::<Platform>(&text) {
                Ok(p) => platforms.push(p),
                Err(e) => {
                    tracing::warn!("Failed to parse platform '{}': {e}", entry.name);
                }
            },
            Err(e) => {
                tracing::warn!("Failed to fetch platform '{}': {e}", entry.name);
            }
        }
    }

    Some(RegistryRaw {
        platforms,
        canonical_servers,
    })
}

/// Try fetching registry v5 (JSON per-platform) under one commit-pinned `base`.
fn try_fetch_v5(client: &reqwest::blocking::Client, base: &str) -> Option<RegistryRaw> {
    let resp = client
        .get(format!("{base}index.json"))
        .header("User-Agent", "canopy")
        .send()
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let index: LegacyRegistryIndex = resp.json().ok()?;

    let mut platforms = Vec::new();
    for entry in &index.platforms {
        let url = format!("{base}platforms/{}.json", entry.name);
        match client
            .get(&url)
            .header("User-Agent", "canopy")
            .send()
            .and_then(|r| r.json::<Platform>())
        {
            Ok(p) => platforms.push(p),
            Err(e) => {
                tracing::warn!("Failed to fetch platform '{}': {e}", entry.name);
            }
        }
    }

    Some(RegistryRaw {
        platforms,
        canonical_servers: CanonicalServers::default(),
    })
}

use crate::shared::banner;

pub(crate) fn print_banner() {
    banner::print_banner_with_gradient("Agent Hub — Setup Wizard");
}

/// State-table key holding the RFC 3339 timestamp of the last *successful*
/// registry fetch. Deliberately not derived from `config.toml`'s mtime:
/// anything that writes the config (canopy adding a CLI, a user changing a
/// setting) used to reset that clock and could postpone a refresh
/// indefinitely on a config that's touched regularly.
const REGISTRY_LAST_REFRESH_STATE_KEY: &str = "registry_last_refresh_at";

pub fn maybe_refresh_registry() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let canopy_dir = home.join(".canopy");
    // Only ever refresh a config that already exists -- pre-setup, there's
    // nothing to reconcile.
    if !canopy_dir.join("config.toml").exists() {
        return false;
    }

    if !needs_refresh(&canopy_dir) {
        return false;
    }

    // Fetch and update in background thread
    std::thread::spawn(move || {
        let _ = refresh_registry_inner(&home, fetch_registry);
    });

    true
}

/// Whether enough time has passed since the last *successful* refresh,
/// reading the clock from daemon state rather than any file's mtime. No
/// recorded refresh (fresh database, or a database that doesn't exist yet)
/// is treated the same as an overdue one.
fn needs_refresh(canopy_dir: &Path) -> bool {
    let db_path = database_path(canopy_dir);
    let last_refresh = db_path
        .exists()
        .then(|| Database::new_safe(&db_path, canopy_dir).ok())
        .flatten()
        .and_then(|db| db.get_state(REGISTRY_LAST_REFRESH_STATE_KEY).ok().flatten());

    let Some(last_refresh) = last_refresh else {
        return true;
    };

    match chrono::DateTime::parse_from_rfc3339(&last_refresh) {
        Ok(ts) => {
            let elapsed = chrono::Utc::now().signed_duration_since(ts.with_timezone(&chrono::Utc));
            elapsed
                .to_std()
                .map(|d| d > REGISTRY_REFRESH_INTERVAL)
                .unwrap_or(true)
        }
        Err(_) => true,
    }
}

pub(crate) fn refresh_registry_inner(
    home: &Path,
    fetch: impl FnOnce() -> Result<(RegistryRaw, String)>,
) -> Result<RegistryRefreshReport> {
    let (registry, source) = fetch()?;
    apply_registry_refresh(home, &registry, &source)
}

/// The testable heart of a registry refresh: given an already-fetched
/// registry, detect which CLIs apply to this machine, three-way merge their
/// registry-owned fields into the existing config, add newly detected CLIs,
/// remove ones whose binary vanished, persist the baseline sidecar, and
/// record the successful refresh time. Split out from [`refresh_registry_inner`]
/// so tests can drive it with an in-memory [`RegistryRaw`] instead of a real
/// fetch.
fn apply_registry_refresh(
    home: &Path,
    registry: &RegistryRaw,
    source: &str,
) -> Result<RegistryRefreshReport> {
    let detected: Vec<&Platform> = registry
        .platforms
        .iter()
        .filter(|p| resolve_config_path(home, &p.config_path).exists())
        .collect();

    let platforms_with_cli: Vec<PlatformWithCli> = detected
        .iter()
        .map(|p| p.to_platform_with_cli())
        .filter(|p| p.cli.is_some())
        .collect();

    let cli_registry =
        crate::domain::cli_config::CliRegistry::detect_available(&platforms_with_cli);

    let canopy_dir = home.join(".canopy");
    let mut config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);

    let registry_by_name: std::collections::HashMap<String, CliConfig> = cli_registry
        .available_clis
        .iter()
        .map(|c| (c.name.clone(), c.clone()))
        .collect();

    let baseline = RegistryBaseline::load(&canopy_dir);

    // First-ever refresh: there's no baseline to tell a deliberate local
    // edit apart from an untouched value, so adopt registry truth outright
    // for every existing CLI -- but back up the config first. The set of
    // users with intentional hand edits to these fields is small and their
    // config is recoverable from this backup; the set stuck with broken
    // invocation flags is everyone, permanently, with no way to even know.
    backup_config_before_first_refresh(&canopy_dir, baseline.is_none())?;

    let (changed_fields, kept_fields, mut pending) =
        merge_existing_cli_entries(&mut config, &registry_by_name, baseline.as_ref());
    add_newly_detected_clis(&mut config, &cli_registry);

    // This is the one moment where silence is the failure mode being fixed:
    // an operator who never asked for a value to change should see that it
    // did.
    warn_updated_non_customised_fields(&changed_fields);

    // Platform-level reconciliation: add / update / delete / unchanged.
    // Structural comparison (field-by-field) rather than textual ensures
    // reformatted content with identical semantics is not treated as a change.
    let fetched_at = chrono::Utc::now().to_rfc3339();
    let refresh = reconcile_platforms(home, baseline.as_ref(), registry, &fetched_at);
    warn_platform_reconciliation(&refresh);
    pending.extend(refresh.pending);

    let db = Database::new_safe(&database_path(&canopy_dir), &canopy_dir);
    // Self-healing: drop harnesses whose binary is gone (grace-gated, so a
    // transient PATH problem never prunes), except ones something still
    // references. Runs before the config write so a prune is persisted by
    // the same write. An unopenable database keeps every entry — the refresh
    // still saves the config and baseline below, and only the refresh-time
    // record fails afterwards.
    if let Ok(db) = &db {
        crate::setup_module::harness_prune::prune_absent(
            &mut config,
            db,
            crate::setup_module::harness_prune::PruneTrigger::DaemonCheck,
            std::time::SystemTime::now(),
        );
    }

    // Always persist: an empty `clis` here only happens when the prune pass
    // confirmed every remaining entry's harness is gone (referenced entries
    // are kept, so that's a real state worth writing, not a detection glitch
    // to shield the config from).
    //
    // CM51: written through the one writer, so a registry refresh can no
    // longer wipe the user's comments out of config.toml.
    crate::domain::settings::writer::write_clis(&canopy_dir, &config.clis)?;

    // The baseline must remember the value still in use for every held
    // field — the local one, not the registry's — or the next refresh
    // would read the held field as a deliberate local edit and never
    // offer it again.
    let mut baseline_clis = cli_registry.available_clis;
    for held in &pending {
        if let Some(entry) = baseline_clis.iter_mut().find(|c| c.name == held.cli) {
            super::registry_pending::set_cli_field(entry, &held.field, held.old.clone());
        }
    }
    RegistryBaseline {
        clis: baseline_clis,
        platforms: refresh.reconciled_platforms,
    }
    .save(&canopy_dir)?;

    let previous = super::registry_pending::load_pending(&canopy_dir);
    super::registry_pending::save_pending(&canopy_dir, &pending)?;
    if !pending.is_empty()
        && !super::registry_pending::pending_eq_ignoring_time(&pending, &previous)
    {
        crate::domain::notification::send_notification(
            "Canopy registry",
            &format!(
                "{} registry change(s) waiting for approval: run canopy registry pending",
                pending.len()
            ),
            crate::domain::notification::NotificationLevel::Warning,
        );
    }

    db?.set_state(
        REGISTRY_LAST_REFRESH_STATE_KEY,
        &chrono::Utc::now().to_rfc3339(),
    )?;

    let changed = group_by_platform(changed_fields);
    let kept = group_by_platform(kept_fields);
    let up_to_date = changed.is_empty()
        && kept.is_empty()
        && refresh.added.is_empty()
        && refresh.updated.is_empty()
        && refresh.deleted.is_empty()
        && pending.is_empty();

    Ok(RegistryRefreshReport {
        changed,
        kept,
        source: source.to_string(),
        added: refresh.added,
        updated: refresh.updated,
        deleted: refresh.deleted,
        pending,
        up_to_date,
    })
}

/// Back up `config.toml` before the first-ever refresh adopts registry
/// truth outright. Never copies when there is no config file yet — there is
/// nothing to protect in that case.
fn backup_config_before_first_refresh(canopy_dir: &Path, is_first_refresh: bool) -> Result<()> {
    if is_first_refresh {
        let config_path = canopy_dir.join("config.toml");
        if config_path.exists() {
            let backup_path = canopy_dir.join(format!(
                "config.toml.bak-{}",
                chrono::Utc::now().to_rfc3339()
            ));
            std::fs::copy(&config_path, &backup_path)
                .context("Failed to back up config.toml before first registry reconciliation")?;
        }
    }
    Ok(())
}

/// Three-way merge, per field, not per CLI: a field still equal to what
/// the registry published at the previous baseline is user-untouched, so
/// the new registry value wins; a field that has drifted from the
/// baseline was deliberately edited, so it's left alone. A user who
/// customised `model_flag` must still receive a corrected `prompt_via_stdin`.
/// Executable changes (binary, flags, argv templates) are staged for
/// approval instead of applied: the local value stays in the config and one
/// [`super::registry_pending::PendingChange`] is emitted per held field.
/// Returns the (rewritten, preserved, staged) field lists for the CLIs that
/// were three-way merged. The first two are emitted as `"cli.key"` strings.
fn merge_existing_cli_entries(
    config: &mut crate::domain::canopy_config::CanopyConfig,
    registry_by_name: &std::collections::HashMap<String, CliConfig>,
    baseline: Option<&RegistryBaseline>,
) -> (
    Vec<String>,
    Vec<String>,
    Vec<super::registry_pending::PendingChange>,
) {
    let mut changed_fields: Vec<String> = Vec::new();
    let mut kept_fields: Vec<String> = Vec::new();
    let mut pending: Vec<super::registry_pending::PendingChange> = Vec::new();
    let fetched_at = chrono::Utc::now().to_rfc3339();

    for existing in config.clis.iter_mut() {
        if let Some(registry_cli) = registry_by_name.get(&existing.name) {
            let baseline_cli = baseline.and_then(|b| b.get(&existing.name));
            let name = existing.name.clone();
            let merged = merge_cli_fields(
                existing,
                baseline_cli,
                registry_cli,
                &name,
                &mut changed_fields,
                &mut kept_fields,
            );
            let (held_back, mut staged) =
                super::registry_pending::hold_executable_changes(existing, &merged, &fetched_at);
            for held in &staged {
                changed_fields.retain(|f| f != &format!("{}.{}", held.cli, held.field));
            }
            *existing = held_back;
            pending.append(&mut staged);
        }
    }

    (changed_fields, kept_fields, pending)
}

/// Add newly detected CLIs that aren't already in config.
fn add_newly_detected_clis(
    config: &mut crate::domain::canopy_config::CanopyConfig,
    cli_registry: &crate::domain::cli_config::CliRegistry,
) {
    let existing_names: std::collections::HashSet<String> =
        config.clis.iter().map(|c| c.name.clone()).collect();
    for cli in cli_registry.available_clis.iter().cloned() {
        if !existing_names.contains(&cli.name) {
            config.clis.push(cli);
        }
    }
}

/// Surface the registry-owned fields a refresh changed that the operator
/// never customised locally — silence is the failure mode this fixes.
fn warn_updated_non_customised_fields(changed_fields: &[String]) {
    if !changed_fields.is_empty() {
        tracing::warn!(
            "registry refresh updated {} field(s) not locally customised: {}",
            changed_fields.len(),
            changed_fields.join(", ")
        );
    }
}

/// Merges one CLI's registry-owned fields into `local`, field by field: a
/// field still equal to what the registry published at the previous
/// `baseline` refresh is user-untouched, so the new `registry` value wins; a
/// field that has drifted from the baseline was deliberately edited, so it's
/// left alone. No `baseline` entry for this CLI (first-ever refresh, or a
/// config entry the baseline sidecar never recorded) means there is no way
/// to tell a customised field from an untouched one, so the registry value
/// is adopted outright.
///
/// Operates generically over whatever fields `registry` serializes rather
/// than naming them, so a newly added `CliConfig` field participates
/// automatically without this function changing -- the merge knows fields,
/// never which harness it's looking at.
pub(crate) fn merge_cli_fields(
    local: &CliConfig,
    baseline: Option<&CliConfig>,
    registry: &CliConfig,
    cli_name: &str,
    changed_fields: &mut Vec<String>,
    kept: &mut Vec<String>,
) -> CliConfig {
    let local_val = serde_json::to_value(local).unwrap_or_default();
    let registry_val = serde_json::to_value(registry).unwrap_or_default();
    let baseline_obj = baseline
        .and_then(|b| serde_json::to_value(b).ok())
        .and_then(|v| v.as_object().cloned());

    let mut merged = local_val.as_object().cloned().unwrap_or_default();
    let registry_obj = registry_val.as_object().cloned().unwrap_or_default();

    for (key, registry_field) in &registry_obj {
        let local_field = merged.get(key).cloned().unwrap_or(serde_json::Value::Null);
        let user_untouched = match &baseline_obj {
            Some(b) => b.get(key).cloned().unwrap_or(serde_json::Value::Null) == local_field,
            None => true,
        };
        if user_untouched && &local_field != registry_field {
            changed_fields.push(format!("{cli_name}.{key}"));
            merged.insert(key.clone(), registry_field.clone());
        } else if !user_untouched && &local_field != registry_field {
            // Deliberate local edit diverging from the registry: keep the
            // user's value verbatim and report it as a preserved override.
            kept.push(format!("{cli_name}.{key}"));
        }
    }

    serde_json::from_value(serde_json::Value::Object(merged)).unwrap_or_else(|_| local.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::registry_baseline::REGISTRY_BASELINE_FILE_NAME;
    use tempfile::TempDir;

    /// A registry platform whose config file already exists under `home`
    /// (so it's "detected") and whose `cli.binary` resolves via PATH (so
    /// it's "available") -- `ls` is present on every machine these tests
    /// run on. Callers can override `cli_json` fields.
    fn detected_platform(home: &Path, name: &str, cli_json: serde_json::Value) -> Platform {
        let config_path = format!("{name}.marker");
        std::fs::write(home.join(&config_path), "").unwrap();
        Platform {
            name: name.to_string(),
            config_path,
            config_format: None,
            toml_array_format: false,
            command_format: "separate".to_string(),
            mcp_servers_key: vec![],
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: std::collections::HashMap::new(),
            required_fields: std::collections::HashMap::new(),
            server_extras: std::collections::HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            instruction_precedence: vec![],
            user_instruction_file: None,
            cli: Some(cli_json),

            provider: None,
            tool_name: None,
            project_paths: Vec::new(),
            docs_url: None,
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: Vec::new(),
            install_commands: Vec::new(),
            login_command: None,
            login_check_command: None,
            login_checked_at: None,
        }
    }

    fn registry_of(platforms: Vec<Platform>) -> RegistryRaw {
        RegistryRaw {
            platforms,
            canonical_servers: CanonicalServers::default(),
        }
    }

    fn write_config(canopy_dir: &Path, clis: Vec<CliConfig>) {
        let mut config = crate::domain::canopy_config::CanopyConfig::load(canopy_dir);
        config.clis = clis;
        config.save_for_tests(canopy_dir).unwrap();
    }

    fn write_baseline(canopy_dir: &Path, clis: Vec<CliConfig>) {
        RegistryBaseline {
            clis,
            ..Default::default()
        }
        .save(canopy_dir)
        .unwrap();
    }

    #[test]
    fn field_never_touched_by_user_is_updated_on_refresh() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let local = CliConfig {
            name: "testcli".to_string(),
            binary: "ls".to_string(),
            headless_mode: "--headless".to_string(),
            ..Default::default()
        };
        write_config(&canopy_dir, vec![local.clone()]);
        write_baseline(&canopy_dir, vec![local]);

        // `prompt_via_stdin` is metadata, so the registry value applies
        // directly; executable fields such as `headless_mode` are staged
        // for approval instead (see `registry_pending`).
        let registry = registry_of(vec![detected_platform(
            home.path(),
            "testcli",
            serde_json::json!({"binary": "ls", "headless_mode": "--headless", "prompt_via_stdin": true}),
        )]);

        apply_registry_refresh(home.path(), &registry, "test").unwrap();

        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        let cli = config.get_cli("testcli").unwrap();
        assert!(cli.prompt_via_stdin);
    }

    #[test]
    fn field_customised_by_user_survives_refresh() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        // Baseline reflects what the registry said last time.
        let baseline_cli = CliConfig {
            name: "testcli".to_string(),
            binary: "ls".to_string(),
            model_flag: Some("--model-old".to_string()),
            headless_mode: "--headless".to_string(),
            ..Default::default()
        };
        // Local drifted from the baseline on model_flag (deliberate edit)
        // but never touched prompt_via_stdin.
        let local_cli = CliConfig {
            model_flag: Some("--model-custom".to_string()),
            ..baseline_cli.clone()
        };
        write_config(&canopy_dir, vec![local_cli]);
        write_baseline(&canopy_dir, vec![baseline_cli]);

        let registry = registry_of(vec![detected_platform(
            home.path(),
            "testcli",
            serde_json::json!({
                "binary": "ls",
                "model_flag": "--model-new",
                "headless_mode": "--headless",
                "prompt_via_stdin": true
            }),
        )]);

        apply_registry_refresh(home.path(), &registry, "test").unwrap();

        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        let cli = config.get_cli("testcli").unwrap();
        // Customised field is untouched...
        assert_eq!(cli.model_flag.as_deref(), Some("--model-custom"));
        // ...but the metadata field the user never touched still applies.
        assert!(cli.prompt_via_stdin);
    }

    /// CB44: the generic `merge_cli_fields` graph must carry a new field —
    /// here a metadata one (`ctrl_c_exits`), which a platform whose
    /// entry the user never touched gets from the registry on refresh.
    /// Executable fields such as `identity_check` are staged for approval
    /// instead (see `registry_pending`). (`instruction_file` cannot serve
    /// here: `to_platform_with_cli` overwrites the `[cli]` value with the
    /// top-level platform field.)
    #[test]
    fn metadata_field_never_touched_by_user_is_updated_on_refresh() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let old = CliConfig {
            name: "testcli".to_string(),
            binary: "ls".to_string(),
            headless_mode: "--headless".to_string(),
            ..Default::default()
        };
        write_config(&canopy_dir, vec![old.clone()]);
        write_baseline(&canopy_dir, vec![old]);

        let registry = registry_of(vec![detected_platform(
            home.path(),
            "testcli",
            serde_json::json!({
                "binary": "ls",
                "headless_mode": "--headless",
                "ctrl_c_exits": true
            }),
        )]);

        apply_registry_refresh(home.path(), &registry, "test").unwrap();

        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        let cli = config.get_cli("testcli").unwrap();
        assert!(cli.ctrl_c_exits);
    }

    /// CB44: a user-customised `identity_check` survives a registry refresh
    /// that changes the same field — deliberate edits win over the registry.
    #[test]
    fn identity_check_customised_by_user_survives_refresh() {
        use crate::domain::cli_config::IdentityCheck;
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let baseline_cli = CliConfig {
            name: "testcli".to_string(),
            binary: "ls".to_string(),
            headless_mode: "--headless".to_string(),
            identity_check: Some(IdentityCheck {
                cmd: "--version".to_string(),
                contains: "old".to_string(),
            }),
            ..Default::default()
        };
        let local_cli = CliConfig {
            identity_check: Some(IdentityCheck {
                cmd: "--version".to_string(),
                contains: "custom".to_string(),
            }),
            ..baseline_cli.clone()
        };
        write_config(&canopy_dir, vec![local_cli]);
        write_baseline(&canopy_dir, vec![baseline_cli]);

        let registry = registry_of(vec![detected_platform(
            home.path(),
            "testcli",
            serde_json::json!({
                "binary": "ls",
                "headless_mode": "--headless",
                "identity_check": {"cmd": "--version", "contains": "new"}
            }),
        )]);

        apply_registry_refresh(home.path(), &registry, "test").unwrap();

        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        let cli = config.get_cli("testcli").unwrap();
        assert_eq!(
            cli.identity_check.as_ref().map(|c| c.contains.as_str()),
            Some("custom")
        );
    }

    #[test]
    fn provider_tool_never_touched_adopts_registry() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let local = CliConfig {
            name: "testcli".to_string(),
            binary: "ls".to_string(),
            headless_mode: "--headless".to_string(),
            ..Default::default()
        };
        write_config(&canopy_dir, vec![local.clone()]);
        write_baseline(&canopy_dir, vec![local]);

        let mut platform = detected_platform(
            home.path(),
            "testcli",
            serde_json::json!({"binary": "ls", "headless_mode": "--headless"}),
        );
        platform.provider = Some("Mistral AI".to_string());
        platform.tool_name = Some("Vibe".to_string());
        apply_registry_refresh(home.path(), &registry_of(vec![platform]), "test").unwrap();

        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        let cli = config.get_cli("testcli").unwrap();
        assert_eq!(cli.provider.as_deref(), Some("Mistral AI"));
        assert_eq!(cli.tool_name.as_deref(), Some("Vibe"));
        assert_eq!(cli.display_name(), "Mistral AI · Vibe");
    }

    #[test]
    fn provider_tool_customised_survives() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let baseline_cli = CliConfig {
            name: "testcli".to_string(),
            binary: "ls".to_string(),
            headless_mode: "--headless".to_string(),
            provider: Some("Anthropic".to_string()),
            tool_name: Some("Claude Code".to_string()),
            ..Default::default()
        };
        let local_cli = CliConfig {
            provider: Some("Custom".to_string()),
            ..baseline_cli.clone()
        };
        write_config(&canopy_dir, vec![local_cli]);
        write_baseline(&canopy_dir, vec![baseline_cli]);

        let mut platform = detected_platform(
            home.path(),
            "testcli",
            serde_json::json!({"binary": "ls", "headless_mode": "--headless"}),
        );
        platform.provider = Some("New".to_string());
        platform.tool_name = Some("New Tool".to_string());
        apply_registry_refresh(home.path(), &registry_of(vec![platform]), "test").unwrap();

        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        let cli = config.get_cli("testcli").unwrap();
        assert_eq!(cli.provider.as_deref(), Some("Custom"));
    }

    #[test]
    fn brand_new_cli_is_still_added() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");
        write_config(&canopy_dir, vec![]);
        write_baseline(&canopy_dir, vec![]);

        let registry = registry_of(vec![detected_platform(
            home.path(),
            "brandnew",
            serde_json::json!({"binary": "ls", "headless_mode": "--headless"}),
        )]);

        apply_registry_refresh(home.path(), &registry, "test").unwrap();

        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        assert!(config.get_cli("brandnew").is_some());
    }

    #[test]
    fn cli_whose_binary_vanished_is_kept_on_first_refresh_then_dropped() {
        use crate::application::ports::StateRepository;
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let ghost = CliConfig {
            name: "ghost".to_string(),
            binary: "canopy-test-fixture-cli-missing-xyz".to_string(),
            ..Default::default()
        };
        write_config(&canopy_dir, vec![ghost.clone()]);
        write_baseline(&canopy_dir, vec![ghost]);

        // Registry still knows about "ghost" (its marker file is detected),
        // but its binary can never resolve.
        let registry = || {
            registry_of(vec![detected_platform(
                home.path(),
                "ghost",
                serde_json::json!({"binary": "canopy-test-fixture-cli-missing-xyz"}),
            )])
        };

        // Refresh #1: first sighting — kept, and the sighting is recorded.
        apply_registry_refresh(home.path(), &registry(), "test").unwrap();
        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        assert!(config.get_cli("ghost").is_some());
        let db = Database::new_safe(&database_path(&canopy_dir), &canopy_dir).unwrap();
        assert!(db
            .get_state("harness_absent_since:ghost")
            .unwrap()
            .is_some_and(|v| !v.is_empty()));

        // Eleven minutes later: refresh #2 prunes it.
        let backdated = chrono::Utc::now() - chrono::Duration::minutes(11);
        db.set_state("harness_absent_since:ghost", &backdated.to_rfc3339())
            .unwrap();
        apply_registry_refresh(home.path(), &registry(), "test").unwrap();
        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        assert!(config.get_cli("ghost").is_none());
    }

    #[test]
    fn first_refresh_with_no_baseline_adopts_and_backs_up() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let local = CliConfig {
            name: "testcli".to_string(),
            binary: "ls".to_string(),
            headless_mode: "--stale".to_string(),
            ..Default::default()
        };
        write_config(&canopy_dir, vec![local]);
        // No baseline written -- this is the very first refresh.
        assert!(RegistryBaseline::load(&canopy_dir).is_none());

        let registry = registry_of(vec![detected_platform(
            home.path(),
            "testcli",
            serde_json::json!({
                "binary": "ls",
                "headless_mode": "--corrected",
                "prompt_via_stdin": true
            }),
        )]);

        let report = apply_registry_refresh(home.path(), &registry, "test").unwrap();

        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        // Metadata is adopted outright...
        assert!(config.get_cli("testcli").unwrap().prompt_via_stdin);
        // ...but the executable correction is staged, not applied.
        assert_eq!(config.get_cli("testcli").unwrap().headless_mode, "--stale");
        assert_eq!(report.pending.len(), 1);

        // A timestamped backup of the pre-refresh config exists and holds
        // the old value.
        let backups: Vec<_> = std::fs::read_dir(&canopy_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("config.toml.bak-"))
            .collect();
        assert_eq!(backups.len(), 1);
        let backup_content = std::fs::read_to_string(canopy_dir.join(&backups[0])).unwrap();
        assert!(backup_content.contains("--stale"));

        // A baseline now exists for the next refresh to compare against.
        assert!(RegistryBaseline::load(&canopy_dir).is_some());
    }

    /// The extracted backup helper's skip path — first refresh, but no
    /// `config.toml` exists yet — was previously only reachable through
    /// `apply_registry_refresh` on a home that always had a config file.
    #[test]
    fn first_refresh_without_config_file_skips_backup() {
        let dir = TempDir::new().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        std::fs::create_dir_all(&canopy_dir).unwrap();

        let list_backups = || -> Vec<String> {
            std::fs::read_dir(&canopy_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with("config.toml.bak-"))
                .collect()
        };

        // First refresh, config file absent: nothing to copy, no backup.
        backup_config_before_first_refresh(&canopy_dir, true).unwrap();
        assert!(list_backups().is_empty());

        // And even with a config file present, a later refresh never backs up.
        std::fs::write(canopy_dir.join("config.toml"), "x = 1\n").unwrap();
        backup_config_before_first_refresh(&canopy_dir, false).unwrap();
        assert!(list_backups().is_empty());
    }

    #[test]
    fn failed_fetch_leaves_config_and_baseline_untouched() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let local = CliConfig {
            name: "testcli".to_string(),
            binary: "ls".to_string(),
            headless_mode: "--untouched".to_string(),
            ..Default::default()
        };
        write_config(&canopy_dir, vec![local.clone()]);
        write_baseline(&canopy_dir, vec![local]);

        let config_before = std::fs::read_to_string(canopy_dir.join("config.toml")).unwrap();
        let baseline_before =
            std::fs::read_to_string(canopy_dir.join(REGISTRY_BASELINE_FILE_NAME)).unwrap();

        // Fetch from an empty local registry directory so the fetch fails
        // deterministically, with no network call.
        let empty_registry_dir = TempDir::new().unwrap();

        let result = refresh_registry_inner(home.path(), || {
            try_fetch_local(empty_registry_dir.path())
                .context("x")
                .map(|reg| (reg, "test".to_string()))
        });
        assert!(result.is_err());

        let config_after = std::fs::read_to_string(canopy_dir.join("config.toml")).unwrap();
        let baseline_after =
            std::fs::read_to_string(canopy_dir.join(REGISTRY_BASELINE_FILE_NAME)).unwrap();
        assert_eq!(config_before, config_after);
        assert_eq!(baseline_before, baseline_after);
    }

    #[test]
    fn needs_refresh_true_when_no_state_recorded() {
        let dir = TempDir::new().unwrap();
        assert!(needs_refresh(dir.path()));
    }

    #[test]
    fn needs_refresh_false_shortly_after_a_recorded_refresh() {
        let dir = TempDir::new().unwrap();
        let db = Database::new(&database_path(dir.path())).unwrap();
        db.set_state(
            REGISTRY_LAST_REFRESH_STATE_KEY,
            &chrono::Utc::now().to_rfc3339(),
        )
        .unwrap();

        assert!(!needs_refresh(dir.path()));
    }

    #[test]
    fn needs_refresh_true_when_recorded_refresh_is_stale() {
        let dir = TempDir::new().unwrap();
        let db = Database::new(&database_path(dir.path())).unwrap();
        let stale = chrono::Utc::now() - chrono::Duration::hours(25);
        db.set_state(REGISTRY_LAST_REFRESH_STATE_KEY, &stale.to_rfc3339())
            .unwrap();

        assert!(needs_refresh(dir.path()));
    }

    /// The defect this spec fixes: rewriting `config.toml` for an unrelated
    /// reason must not reset the refresh clock, because that clock no
    /// longer lives on the file's mtime.
    #[test]
    fn rewriting_config_toml_does_not_reset_the_refresh_clock() {
        let dir = TempDir::new().unwrap();
        let db = Database::new(&database_path(dir.path())).unwrap();
        let stale = chrono::Utc::now() - chrono::Duration::hours(25);
        db.set_state(REGISTRY_LAST_REFRESH_STATE_KEY, &stale.to_rfc3339())
            .unwrap();

        // Simulate an unrelated config write (e.g. a settings change),
        // which touches config.toml's mtime.
        write_config(
            dir.path(),
            vec![CliConfig {
                name: "unrelated".to_string(),
                ..Default::default()
            }],
        );

        // The stale state-table timestamp is what still governs eligibility.
        assert!(needs_refresh(dir.path()));
    }

    #[test]
    fn platform_fields_are_updated_on_refresh() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let old_platform = Platform {
            name: "testcli".to_string(),
            config_path: "testcli.marker".to_string(),
            config_format: None,
            toml_array_format: false,
            command_format: "separate".to_string(),
            mcp_servers_key: vec!["old_key".to_string()],
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: std::collections::HashMap::new(),
            required_fields: std::collections::HashMap::new(),
            server_extras: std::collections::HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            instruction_precedence: vec![],
            user_instruction_file: None,
            cli: Some(serde_json::json!({"binary": "ls", "headless_mode": "--headless"})),

            provider: None,
            tool_name: None,
            project_paths: Vec::new(),
            docs_url: None,
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: Vec::new(),
            install_commands: Vec::new(),
            login_command: None,
            login_check_command: None,
            login_checked_at: None,
        };

        std::fs::write(home.path().join(&old_platform.config_path), "").unwrap();

        write_config(&canopy_dir, vec![]);
        let baseline = RegistryBaseline {
            clis: vec![],
            platforms: vec![old_platform],
        };
        baseline.save(&canopy_dir).unwrap();

        let new_platform = Platform {
            name: "testcli".to_string(),
            config_path: "testcli.marker".to_string(),
            config_format: None,
            toml_array_format: false,
            command_format: "merged".to_string(),
            mcp_servers_key: vec!["new_key".to_string()],
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: std::collections::HashMap::new(),
            required_fields: std::collections::HashMap::new(),
            server_extras: std::collections::HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            instruction_precedence: vec![],
            user_instruction_file: None,
            cli: Some(serde_json::json!({"binary": "ls", "headless_mode": "--headless"})),

            provider: None,
            tool_name: None,
            project_paths: Vec::new(),
            docs_url: None,
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: Vec::new(),
            install_commands: Vec::new(),
            login_command: None,
            login_check_command: None,
            login_checked_at: None,
        };
        let registry = registry_of(vec![new_platform]);

        apply_registry_refresh(home.path(), &registry, "test").unwrap();

        let updated_baseline = RegistryBaseline::load(&canopy_dir).unwrap();
        let updated_platform = updated_baseline
            .platforms
            .iter()
            .find(|p| p.name == "testcli")
            .unwrap();
        assert_eq!(updated_platform.command_format, "merged");
        assert_eq!(
            updated_platform.mcp_servers_key,
            vec!["new_key".to_string()]
        );
    }

    #[test]
    fn platform_unchanged_when_registry_matches_baseline() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let platform = Platform {
            name: "testcli".to_string(),
            config_path: "testcli.marker".to_string(),
            config_format: None,
            toml_array_format: false,
            command_format: "separate".to_string(),
            mcp_servers_key: vec!["key".to_string()],
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: std::collections::HashMap::new(),
            required_fields: std::collections::HashMap::new(),
            server_extras: std::collections::HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            instruction_precedence: vec![],
            user_instruction_file: None,
            cli: Some(serde_json::json!({"binary": "ls", "headless_mode": "--headless"})),

            provider: None,
            tool_name: None,
            project_paths: Vec::new(),
            docs_url: None,
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: Vec::new(),
            install_commands: Vec::new(),
            login_command: None,
            login_check_command: None,
            login_checked_at: None,
        };
        std::fs::write(home.path().join(&platform.config_path), "").unwrap();

        write_config(&canopy_dir, vec![]);
        RegistryBaseline {
            clis: vec![],
            platforms: vec![platform.clone()],
        }
        .save(&canopy_dir)
        .unwrap();

        let registry = registry_of(vec![platform]);
        apply_registry_refresh(home.path(), &registry, "test").unwrap();

        let updated = RegistryBaseline::load(&canopy_dir).unwrap();
        assert_eq!(updated.platforms.len(), 1);
        assert_eq!(updated.platforms[0].command_format, "separate");
        assert_eq!(
            updated.platforms[0].mcp_servers_key,
            vec!["key".to_string()]
        );
    }

    #[test]
    fn platform_added_when_new_in_registry() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        write_config(&canopy_dir, vec![]);
        RegistryBaseline {
            clis: vec![],
            platforms: vec![],
        }
        .save(&canopy_dir)
        .unwrap();

        let platform = Platform {
            name: "brandnew".to_string(),
            config_path: "brandnew.marker".to_string(),
            config_format: None,
            toml_array_format: false,
            command_format: "separate".to_string(),
            mcp_servers_key: vec!["mcpServers".to_string()],
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: std::collections::HashMap::new(),
            required_fields: std::collections::HashMap::new(),
            server_extras: std::collections::HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            instruction_precedence: vec![],
            user_instruction_file: None,
            cli: Some(serde_json::json!({"binary": "ls", "headless_mode": "--headless"})),

            provider: None,
            tool_name: None,
            project_paths: Vec::new(),
            docs_url: None,
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: Vec::new(),
            install_commands: Vec::new(),
            login_command: None,
            login_check_command: None,
            login_checked_at: None,
        };
        std::fs::write(home.path().join(&platform.config_path), "").unwrap();

        let registry = registry_of(vec![platform]);
        apply_registry_refresh(home.path(), &registry, "test").unwrap();

        let updated = RegistryBaseline::load(&canopy_dir).unwrap();
        assert_eq!(updated.platforms.len(), 1);
        assert_eq!(updated.platforms[0].name, "brandnew");
    }

    #[test]
    fn platform_deleted_when_removed_from_registry() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let platform = Platform {
            name: "oldone".to_string(),
            config_path: "oldone.marker".to_string(),
            config_format: None,
            toml_array_format: false,
            command_format: "separate".to_string(),
            mcp_servers_key: vec![],
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: std::collections::HashMap::new(),
            required_fields: std::collections::HashMap::new(),
            server_extras: std::collections::HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            instruction_precedence: vec![],
            user_instruction_file: None,
            cli: Some(serde_json::json!({"binary": "ls"})),

            provider: None,
            tool_name: None,
            project_paths: Vec::new(),
            docs_url: None,
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: Vec::new(),
            install_commands: Vec::new(),
            login_command: None,
            login_check_command: None,
            login_checked_at: None,
        };
        std::fs::write(home.path().join(&platform.config_path), "").unwrap();

        write_config(&canopy_dir, vec![]);
        RegistryBaseline {
            clis: vec![],
            platforms: vec![platform],
        }
        .save(&canopy_dir)
        .unwrap();

        let registry = registry_of(vec![]);
        apply_registry_refresh(home.path(), &registry, "test").unwrap();

        let updated = RegistryBaseline::load(&canopy_dir).unwrap();
        assert!(updated.platforms.is_empty());
    }

    #[test]
    fn refresh_is_idempotent_for_platforms() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let old_platform = Platform {
            name: "testcli".to_string(),
            config_path: "testcli.marker".to_string(),
            config_format: None,
            toml_array_format: false,
            command_format: "separate".to_string(),
            mcp_servers_key: vec!["old_key".to_string()],
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: std::collections::HashMap::new(),
            required_fields: std::collections::HashMap::new(),
            server_extras: std::collections::HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            instruction_precedence: vec![],
            user_instruction_file: None,
            cli: Some(serde_json::json!({"binary": "ls"})),

            provider: None,
            tool_name: None,
            project_paths: Vec::new(),
            docs_url: None,
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: Vec::new(),
            install_commands: Vec::new(),
            login_command: None,
            login_check_command: None,
            login_checked_at: None,
        };
        std::fs::write(home.path().join(&old_platform.config_path), "").unwrap();

        write_config(&canopy_dir, vec![]);
        RegistryBaseline {
            clis: vec![],
            platforms: vec![old_platform],
        }
        .save(&canopy_dir)
        .unwrap();

        let new_platform = Platform {
            name: "testcli".to_string(),
            config_path: "testcli.marker".to_string(),
            config_format: None,
            toml_array_format: false,
            command_format: "merged".to_string(),
            mcp_servers_key: vec!["new_key".to_string()],
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: std::collections::HashMap::new(),
            required_fields: std::collections::HashMap::new(),
            server_extras: std::collections::HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            instruction_precedence: vec![],
            user_instruction_file: None,
            cli: Some(serde_json::json!({"binary": "ls"})),

            provider: None,
            tool_name: None,
            project_paths: Vec::new(),
            docs_url: None,
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: Vec::new(),
            install_commands: Vec::new(),
            login_command: None,
            login_check_command: None,
            login_checked_at: None,
        };
        let registry = registry_of(vec![new_platform]);

        apply_registry_refresh(home.path(), &registry, "test").unwrap();
        let after_first = RegistryBaseline::load(&canopy_dir).unwrap();
        let first_platform = after_first
            .platforms
            .iter()
            .find(|p| p.name == "testcli")
            .unwrap()
            .clone();

        apply_registry_refresh(home.path(), &registry, "test").unwrap();
        let after_second = RegistryBaseline::load(&canopy_dir).unwrap();
        let second_platform = after_second
            .platforms
            .iter()
            .find(|p| p.name == "testcli")
            .unwrap();

        assert_eq!(
            first_platform.command_format,
            second_platform.command_format
        );
        assert_eq!(
            first_platform.mcp_servers_key,
            second_platform.mcp_servers_key
        );
        // Second refresh must be idempotent: still exactly one platform, unchanged.
        assert_eq!(after_second.platforms.len(), 1);
    }

    fn write_local_registry(dir: &Path, entries: &[(&str, &str, &str)]) {
        let mut index_toml = String::from("version = 6\nplatforms = [\n");
        for (name, binary, _platform_toml) in entries {
            index_toml.push_str(&format!(
                "  {{ name = \"{name}\", binary = \"{binary}\" }},\n"
            ));
        }
        index_toml.push_str("]\n");
        std::fs::write(dir.join("index.toml"), &index_toml).unwrap();

        let platforms_dir = dir.join("platforms");
        std::fs::create_dir_all(&platforms_dir).unwrap();
        for (name, _binary, platform_toml) in entries {
            std::fs::write(platforms_dir.join(format!("{name}.toml")), platform_toml).unwrap();
        }
    }

    const MINIMAL_PLATFORM_TOML: &str = r#"
name = "PLACEHOLDER"
config_path = "PLACEHOLDER.marker"
command_format = "separate"
mcp_servers_key = []
deprecated_keys = []
unsupported_keys = []
"#;

    fn platform_toml(name: &str) -> String {
        MINIMAL_PLATFORM_TOML.replace("PLACEHOLDER", name)
    }

    #[test]
    fn try_fetch_local_returns_all_platforms_regardless_of_binary_availability() {
        let dir = TempDir::new().unwrap();
        write_local_registry(
            dir.path(),
            &[
                ("available_plat", "ls", &platform_toml("available_plat")),
                (
                    "missing_plat",
                    "definitely_not_real_xyz",
                    &platform_toml("missing_plat"),
                ),
            ],
        );

        let reg = try_fetch_local(dir.path()).expect("should succeed");
        assert_eq!(reg.platforms.len(), 2);
        let names: Vec<&str> = reg.platforms.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"available_plat"));
        assert!(names.contains(&"missing_plat"));
    }

    #[test]
    fn try_fetch_local_returns_platforms_when_no_binaries_match() {
        let dir = TempDir::new().unwrap();
        write_local_registry(
            dir.path(),
            &[(
                "only_plat",
                "definitely_not_real_abc",
                &platform_toml("only_plat"),
            )],
        );

        let reg =
            try_fetch_local(dir.path()).expect("should succeed even with no binaries on PATH");
        assert_eq!(reg.platforms.len(), 1);
        assert_eq!(reg.platforms[0].name, "only_plat");
    }

    /// A platform TOML like the ones the real canopy-registry publishes, but
    /// with a `[cli]` table the caller fills in.
    fn platform_toml_with_cli(name: &str, cli_toml: &str) -> String {
        format!(
            "name = \"{name}\"\nconfig_path = \"{name}.marker\"\ncommand_format = \"separate\"\n\
             mcp_servers_key = []\n\n[cli]\n{cli_toml}"
        )
    }

    /// A test CLI built to match what a registry `[cli]` table parses to:
    /// all serde defaults intact (`paste_submit_presses` must be the serde
    /// default 1, not [`CliConfig`]'s `Default` trait 0, or the merge sees a
    /// phantom change).
    fn registry_equivalent_cli(headless: &str) -> CliConfig {
        CliConfig {
            name: "testcli".to_string(),
            binary: "ls".to_string(),
            headless_mode: headless.to_string(),
            paste_submit_presses: 1,
            ..Default::default()
        }
    }

    /// Everything in agreement: a detected marker, a config whose `testcli`
    /// entry and a baseline whose `testcli` entry and platform set both match
    /// the local registry's published values — so a refresh of this setup is
    /// a no-op.
    fn write_in_sync_setup(home: &Path, registry_dir: &Path, headless: &str) {
        std::fs::write(home.join("testcli.marker"), "").unwrap();
        let canopy_dir = home.join(".canopy");
        let cli = registry_equivalent_cli(headless);
        write_config(&canopy_dir, vec![cli.clone()]);
        let toml = platform_toml_with_cli(
            "testcli",
            &format!("binary = \"ls\"\nheadless_mode = \"{headless}\""),
        );
        let platform: Platform = toml::from_str(&toml).unwrap();
        RegistryBaseline {
            clis: vec![cli],
            platforms: vec![platform],
        }
        .save(&canopy_dir)
        .unwrap();
        write_local_registry(registry_dir, &[("testcli", "ls", &toml)]);
    }

    /// (a) `canopy registry refresh` driven from a local registry directory
    /// adds newly published metadata CLI fields to an existing platform
    /// entry; newly published executable fields are staged, not applied.
    #[test]
    fn local_registry_refresh_adds_new_cli_fields_to_existing_platform() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let old = registry_equivalent_cli("--old");
        write_config(&canopy_dir, vec![old.clone()]);
        write_baseline(&canopy_dir, vec![old]);
        std::fs::write(home.path().join("testcli.marker"), "").unwrap();

        let dir = TempDir::new().unwrap();
        write_local_registry(
            dir.path(),
            &[(
                "testcli",
                "ls",
                &platform_toml_with_cli(
                    "testcli",
                    "binary = \"ls\"\nheadless_mode = \"--new\"\nmodel_flag = \"--model\"\nprompt_via_stdin = true",
                ),
            )],
        );

        let report = refresh_registry_inner(home.path(), || {
            try_fetch_local(dir.path())
                .context("local registry fetch")
                .map(|reg| (reg, "test".to_string()))
        })
        .unwrap();

        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        let cli = config.get_cli("testcli").unwrap();
        assert!(cli.prompt_via_stdin);
        // Executable fields stay local; the registry values wait for approval.
        assert_eq!(cli.headless_mode, "--old");
        assert_eq!(cli.model_flag, None);
        assert_eq!(report.pending.len(), 2);

        let changed = report.changed.get("testcli").unwrap();
        assert!(
            changed.contains(&"prompt_via_stdin".to_string()),
            "{changed:?}"
        );
    }

    /// (b) A locally overridden field diverging from the registry is kept
    /// verbatim and reported as kept, not rewritten.
    #[test]
    fn local_registry_refresh_keeps_local_override_and_reports_it() {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let baseline_cli = registry_equivalent_cli("--baseline");
        let local_cli = CliConfig {
            headless_mode: "--custom".to_string(),
            ..baseline_cli.clone()
        };
        write_config(&canopy_dir, vec![local_cli]);
        write_baseline(&canopy_dir, vec![baseline_cli]);
        std::fs::write(home.path().join("testcli.marker"), "").unwrap();

        let dir = TempDir::new().unwrap();
        write_local_registry(
            dir.path(),
            &[(
                "testcli",
                "ls",
                &platform_toml_with_cli(
                    "testcli",
                    "binary = \"ls\"\nheadless_mode = \"--baseline\"",
                ),
            )],
        );

        let report = refresh_registry_inner(home.path(), || {
            try_fetch_local(dir.path())
                .context("local registry fetch")
                .map(|reg| (reg, "test".to_string()))
        })
        .unwrap();

        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        assert_eq!(config.get_cli("testcli").unwrap().headless_mode, "--custom");

        let kept = report.kept.get("testcli").unwrap();
        assert!(kept.contains(&"headless_mode".to_string()), "{kept:?}");
        assert!(
            report.render_text().contains("kept local override"),
            "{}",
            report.render_text()
        );
    }

    /// (c) When config, baseline and registry all agree, the report is the
    /// single "already up to date" line.
    #[test]
    fn local_registry_refresh_in_sync_reports_already_up_to_date() {
        let home = TempDir::new().unwrap();
        let dir = TempDir::new().unwrap();
        write_in_sync_setup(home.path(), dir.path(), "--headless");

        let report = refresh_registry_inner(home.path(), || {
            try_fetch_local(dir.path())
                .context("local registry fetch")
                .map(|reg| (reg, "test".to_string()))
        })
        .unwrap();

        assert!(report.up_to_date);
        assert_eq!(
            report.render_text(),
            "registry already up to date\nsource: test"
        );
    }

    /// (d) The on-demand refresh ignores the 24-hour interval: even with a
    /// state entry recorded only 23 hours ago (so the TUI's background
    /// refresh would skip), the manual refresh still runs and records a
    /// fresh timestamp in the same state key.
    #[test]
    fn manual_refresh_applies_despite_fresh_24h_state() {
        let home = TempDir::new().unwrap();
        let dir = TempDir::new().unwrap();
        write_in_sync_setup(home.path(), dir.path(), "--headless");

        let canopy_dir = home.path().join(".canopy");
        let db = Database::new_safe(&database_path(&canopy_dir), &canopy_dir).unwrap();
        let stale = chrono::Utc::now() - chrono::Duration::hours(23);
        db.set_state(REGISTRY_LAST_REFRESH_STATE_KEY, &stale.to_rfc3339())
            .unwrap();
        assert!(!needs_refresh(&canopy_dir));

        let report = refresh_registry_inner(home.path(), || {
            try_fetch_local(dir.path())
                .context("local registry fetch")
                .map(|reg| (reg, "test".to_string()))
        })
        .unwrap();
        assert!(report.up_to_date);

        let recorded = db
            .get_state(REGISTRY_LAST_REFRESH_STATE_KEY)
            .unwrap()
            .expect("refresh recorded its timestamp");
        let ts = chrono::DateTime::parse_from_rfc3339(&recorded).unwrap();
        let age = chrono::Utc::now().signed_duration_since(ts.with_timezone(&chrono::Utc));
        assert!(
            age < chrono::Duration::hours(1),
            "refresh timestamp should be fresh, age {age:?}"
        );
    }
}
