//! Platform-level registry reconciliation: which registry platforms were
//! added, updated, unchanged or deleted, plus the surviving set that becomes
//! the next baseline.
//!
//! Split out from `registry_fetch` so that module stays under the file-size
//! gate; the refresh calls back in through [`reconcile_platforms`] and
//! [`warn_platform_reconciliation`].

use crate::domain::registry_baseline::RegistryBaseline;
use crate::setup_module::models::{resolve_config_path, Platform, RegistryRaw};
use std::path::Path;

/// Platform-level reconciliation outcome: which registry platforms were
/// added, updated, unchanged or deleted, plus the surviving set that
/// becomes the next baseline.
pub(crate) struct PlatformRefresh {
    pub(crate) added: Vec<String>,
    pub(crate) updated: Vec<String>,
    pub(crate) unchanged: Vec<String>,
    pub(crate) deleted: Vec<String>,
    pub(crate) reconciled_platforms: Vec<Platform>,
}

/// Reconcile the registry's platforms against the previous baseline:
/// add / update / delete / unchanged.
pub(crate) fn reconcile_platforms(
    home: &Path,
    baseline: Option<&RegistryBaseline>,
    registry: &RegistryRaw,
) -> PlatformRefresh {
    let baseline_platforms: std::collections::HashMap<String, &Platform> = baseline
        .map(|b| b.platforms.iter().map(|p| (p.name.clone(), p)).collect())
        .unwrap_or_default();

    let mut refresh = PlatformRefresh {
        added: Vec::new(),
        updated: Vec::new(),
        unchanged: Vec::new(),
        deleted: Vec::new(),
        reconciled_platforms: Vec::new(),
    };

    for platform in &registry.platforms {
        if !resolve_config_path(home, &platform.config_path).exists() {
            continue;
        }
        match baseline_platforms.get(&platform.name) {
            Some(baseline_platform) => {
                if platforms_differ(baseline_platform, platform) {
                    refresh.updated.push(platform.name.clone());
                } else {
                    refresh.unchanged.push(platform.name.clone());
                }
            }
            None => {
                refresh.added.push(platform.name.clone());
            }
        }
        refresh.reconciled_platforms.push((*platform).clone());
    }

    let new_platform_names: std::collections::HashSet<String> = refresh
        .reconciled_platforms
        .iter()
        .map(|p| p.name.clone())
        .collect();
    if let Some(b) = baseline {
        for baseline_platform in &b.platforms {
            if !new_platform_names.contains(&baseline_platform.name) {
                // Only count as deleted if the platform's config was part of
                // the baseline's detected set; platforms never detected are
                // not reconciled and should not appear here, but since we
                // only ever store detected platforms, any missing name is a
                // genuine deletion.
                refresh.deleted.push(baseline_platform.name.clone());
            }
        }
    }

    refresh
}

/// Warn about the platforms a refresh added, updated or deleted — summary
/// first, then names in that order — but only when something changed.
pub(crate) fn warn_platform_reconciliation(refresh: &PlatformRefresh) {
    if refresh.added.is_empty() && refresh.updated.is_empty() && refresh.deleted.is_empty() {
        return;
    }
    warn_platform_summary(refresh);
    warn_platform_name_list("updated", &refresh.updated);
    warn_platform_name_list("added", &refresh.added);
    warn_platform_name_list("deleted", &refresh.deleted);
}

/// Warn the one-line reconciliation summary.
fn warn_platform_summary(refresh: &PlatformRefresh) {
    tracing::warn!(
        "registry refresh: {} added, {} updated, {} deleted, {} unchanged platform(s)",
        refresh.added.len(),
        refresh.updated.len(),
        refresh.deleted.len(),
        refresh.unchanged.len()
    );
}

/// Warn the concrete names behind one non-empty reconciliation bucket.
fn warn_platform_name_list(label: &str, names: &[String]) {
    if !names.is_empty() {
        tracing::warn!("  {label}: {}", names.join(", "));
    }
}

/// Structural comparison of two Platform objects. Returns true when any
/// registry-controlled field differs. The `cli` field is excluded — it is
/// handled by the existing CLI three-way merge.
fn platforms_differ(old: &Platform, new: &Platform) -> bool {
    old.config_path != new.config_path
        || old.config_format != new.config_format
        || old.toml_array_format != new.toml_array_format
        || old.command_format != new.command_format
        || old.mcp_servers_key != new.mcp_servers_key
        || old.deprecated_keys != new.deprecated_keys
        || old.unsupported_keys != new.unsupported_keys
        || old.fields_mapping != new.fields_mapping
        || old.required_fields != new.required_fields
        || old.server_extras != new.server_extras
        || old.skills_dir != new.skills_dir
        || old.instruction_file != new.instruction_file
        || old.instruction_precedence != new.instruction_precedence
        || old.docs_url != new.docs_url
        || old.free_tier != new.free_tier
        || old.free_tier_checked_at != new.free_tier_checked_at
        || old.free_tier_source != new.free_tier_source
        || old.install_paths != new.install_paths
}
