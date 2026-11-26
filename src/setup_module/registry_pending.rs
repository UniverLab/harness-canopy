//! Staged registry changes: executable CLI fields never apply unattended.
//!
//! The platform registry owns every field of [`CliConfig`], but the ones
//! that change what the daemon executes (binary, flags, argv templates)
//! are only ever applied after the operator approves them. A refresh that
//! finds such a change keeps the local value in `config.toml`, records the
//! registry value in `registry-pending.json`, and reports it as held.
//! `canopy registry pending` shows the held diff; `canopy registry apply`
//! writes it; `canopy registry discard` drops it (the next refresh stages
//! it again while the registry still differs).

use crate::domain::cli_config::CliConfig;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// [`CliConfig`] fields that are not command lines: display names, probe
/// patterns, paste behavior, retry budgets. Everything else a serialized
/// `CliConfig` carries is an executable field and is held by default, so a
/// field added later is held until it is classified here.
pub(crate) const METADATA_FIELDS: &[&str] = &[
    "name",
    "headless",
    "ctrl_c_exits",
    "supports_working_dir",
    "session_id_pattern",
    "identity_pattern",
    "accent_color",
    "instruction_file",
    "prompt_via_stdin",
    "paste_submit_delay_ms",
    "paste_submit_key",
    "paste_submit_presses",
    "infra_retry_limit",
    "infra_crash_max_seconds",
    "infra_backoff_seconds",
    "provider",
    "tool_name",
];

/// File holding the staged set, relative to the canopy data directory.
pub(crate) const PENDING_FILE: &str = "registry-pending.json";

/// One registry change waiting for approval: the value in use (`old`) and
/// the value the registry published (`new`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct PendingChange {
    pub(crate) cli: String,
    pub(crate) field: String,
    pub(crate) old: serde_json::Value,
    pub(crate) new: serde_json::Value,
    pub(crate) fetched_at: String,
}

/// The on-disk shape of [`PENDING_FILE`].
#[derive(Debug, Default, Serialize, Deserialize)]
struct PendingFile {
    #[serde(default)]
    changes: Vec<PendingChange>,
}

/// Split a merged CLI into the CLI to keep and the executable changes to
/// hold: for every non-metadata key whose merged value differs from the
/// local one, restore the local value and emit one [`PendingChange`].
/// `fetched_at` labels when the registry published the value (RFC 3339).
pub(crate) fn hold_executable_changes(
    local: &CliConfig,
    merged: &CliConfig,
    fetched_at: &str,
) -> (CliConfig, Vec<PendingChange>) {
    let merged_val = serde_json::to_value(merged).unwrap_or_default();
    let merged_obj = merged_val.as_object().cloned().unwrap_or_default();

    // Start from the merged CLI so metadata updates survive, then revert
    // every held executable field to the value still in use.
    let mut held = merged_obj.clone();
    let mut pending = Vec::new();
    let cli_name = local.name.clone();
    for (key, new_value) in &merged_obj {
        if METADATA_FIELDS.contains(&key.as_str()) {
            continue;
        }
        let old_value = cli_field_value(local, key);
        if &old_value != new_value {
            held.insert(key.clone(), old_value.clone());
            pending.push(PendingChange {
                cli: cli_name.clone(),
                field: key.clone(),
                old: old_value,
                new: new_value.clone(),
                fetched_at: fetched_at.to_string(),
            });
        }
    }
    let cli =
        serde_json::from_value(serde_json::Value::Object(held)).unwrap_or_else(|_| local.clone());
    (cli, pending)
}

/// Read one serialized [`CliConfig`] field by name.
pub(crate) fn cli_field_value(cli: &CliConfig, field: &str) -> serde_json::Value {
    serde_json::to_value(cli)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .and_then(|mut o| o.remove(field))
        .unwrap_or(serde_json::Value::Null)
}

/// Write one serialized [`CliConfig`] field by name. False when the value
/// does not round-trip (the caller keeps the CLI unchanged).
pub(crate) fn set_cli_field(cli: &mut CliConfig, field: &str, value: serde_json::Value) -> bool {
    let mut obj = match serde_json::to_value(&*cli) {
        Ok(v) => v.as_object().cloned().unwrap_or_default(),
        Err(_) => return false,
    };
    obj.insert(field.to_string(), value);
    match serde_json::from_value(serde_json::Value::Object(obj)) {
        Ok(next) => {
            *cli = next;
            true
        }
        Err(_) => false,
    }
}

/// Load the staged set. Missing or unparseable means nothing is staged.
pub(crate) fn load_pending(canopy_dir: &Path) -> Vec<PendingChange> {
    let Ok(content) = std::fs::read_to_string(canopy_dir.join(PENDING_FILE)) else {
        return Vec::new();
    };
    serde_json::from_str::<PendingFile>(&content)
        .map(|f| f.changes)
        .unwrap_or_default()
}

/// Replace the staged set. An empty set deletes the file so a repeated
/// fetch leaves no residue and `pending` reports nothing waiting.
pub(crate) fn save_pending(canopy_dir: &Path, changes: &[PendingChange]) -> std::io::Result<()> {
    let path = canopy_dir.join(PENDING_FILE);
    if changes.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        return Ok(());
    }
    std::fs::create_dir_all(canopy_dir)?;
    let content = serde_json::to_string_pretty(&PendingFile {
        changes: changes.to_vec(),
    })
    .unwrap_or_default();
    let temp = canopy_dir.join(format!("{PENDING_FILE}.tmp"));
    std::fs::write(&temp, content)?;
    std::fs::rename(&temp, path)
}

/// True when two staged sets carry the same changes, ignoring when each
/// was fetched — so a repeated identical refresh keeps one entry and no
/// new notification fires.
pub(crate) fn pending_eq_ignoring_time(a: &[PendingChange], b: &[PendingChange]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut remaining: Vec<(&str, &str, &serde_json::Value, &serde_json::Value)> = b
        .iter()
        .map(|c| (c.cli.as_str(), c.field.as_str(), &c.old, &c.new))
        .collect();
    for change in a {
        let Some(pos) = remaining.iter().position(|(cli, field, old, new)| {
            *cli == change.cli.as_str()
                && *field == change.field.as_str()
                && *old == &change.old
                && *new == &change.new
        }) else {
            return false;
        };
        remaining.remove(pos);
    }
    true
}

/// One held change as the `pending` text surface prints it.
pub(crate) fn format_pending(change: &PendingChange) -> String {
    let old = serde_json::to_string(&change.old).unwrap_or_else(|_| "null".to_string());
    let new = serde_json::to_string(&change.new).unwrap_or_else(|_| "null".to_string());
    format!(
        "{}.{rename}: {old} -> {new}",
        change.cli,
        rename = change.field
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup_module::registry_fetch::refresh_registry_inner;

    fn cli_with(headless_mode: &str, model_flag: Option<&str>) -> CliConfig {
        CliConfig {
            name: "testcli".to_string(),
            binary: "ls".to_string(),
            headless_mode: headless_mode.to_string(),
            model_flag: model_flag.map(str::to_string),
            paste_submit_presses: 1,
            ..Default::default()
        }
    }

    fn serialized_keys(cli: &CliConfig) -> Vec<String> {
        serde_json::to_value(cli)
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default()
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn every_serialized_key_is_classified() {
        let keys = serialized_keys(&CliConfig {
            name: "x".to_string(),
            ..Default::default()
        });
        assert!(!keys.is_empty());
        let executable = [
            "binary",
            "headless_mode",
            "model_flag",
            "working_dir_flag",
            "env_vars",
            "interactive_args",
            "fallback_interactive_args",
            "resume_args",
            "session_list_cmd",
            "session_resume_cmd",
            "session_id_set_flag",
            "session_list_format_args",
            "session_title_flag",
            "session_list_args",
            "session_end_args",
            "models_list_cmd",
            "identity_check",
            "identity_args",
            "yolo_flag",
            "trust_flag",
            "invocation_template",
            "effort_declaration",
            "decision_args",
        ];
        for key in &keys {
            assert!(
                METADATA_FIELDS.contains(&key.as_str()) || executable.contains(&key.as_str()),
                "unclassified CliConfig field is held by default but not listed: {key}"
            );
        }
        for field in METADATA_FIELDS {
            assert!(
                keys.contains(&field.to_string()),
                "stale metadata field: {field}"
            );
        }
    }

    #[test]
    fn metadata_only_change_applies_with_nothing_held() {
        let local = cli_with("--headless", None);
        let mut merged = local.clone();
        merged.provider = Some("Someone".to_string());
        let (cli, pending) = hold_executable_changes(&local, &merged, "t");
        assert!(pending.is_empty());
        assert_eq!(cli.provider.as_deref(), Some("Someone"));
    }

    #[test]
    fn changed_headless_mode_is_held_and_local_restored() {
        let local = cli_with("--old", None);
        let merged = cli_with("--new", None);
        let (cli, pending) = hold_executable_changes(&local, &merged, "t");
        assert_eq!(cli.headless_mode, "--old");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].cli, "testcli");
        assert_eq!(pending[0].field, "headless_mode");
        assert_eq!(pending[0].old, serde_json::json!("--old"));
        assert_eq!(pending[0].new, serde_json::json!("--new"));
    }

    #[test]
    fn new_executable_field_is_held_by_default() {
        // `model_flag` must not be metadata: a registry change to it is held.
        assert!(!METADATA_FIELDS.contains(&"model_flag"));
        let local = cli_with("--headless", None);
        let merged = cli_with("--headless", Some("--model"));
        let (_, pending) = hold_executable_changes(&local, &merged, "t");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].field, "model_flag");
    }

    #[test]
    fn pending_file_round_trip_and_empty_deletes() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(load_pending(dir.path()).is_empty());
        let changes = vec![PendingChange {
            cli: "c".to_string(),
            field: "headless_mode".to_string(),
            old: serde_json::json!("--old"),
            new: serde_json::json!("--new"),
            fetched_at: "t".to_string(),
        }];
        save_pending(dir.path(), &changes).unwrap();
        assert!(dir.path().join(PENDING_FILE).exists());
        let back = load_pending(dir.path());
        assert_eq!(back, changes);
        assert!(pending_eq_ignoring_time(&back, &changes));
        let relabeled: Vec<PendingChange> = changes
            .iter()
            .map(|c| PendingChange {
                fetched_at: "later".to_string(),
                ..c.clone()
            })
            .collect();
        assert!(pending_eq_ignoring_time(&changes, &relabeled));
        save_pending(dir.path(), &[]).unwrap();
        assert!(!dir.path().join(PENDING_FILE).exists());
        assert!(load_pending(dir.path()).is_empty());
    }

    /// Test-only twin of the refresh test helpers in `registry_fetch`: a
    /// detected platform whose marker exists and whose binary resolves.
    fn detected(
        home: &std::path::Path,
        marker: &str,
        cli: serde_json::Value,
    ) -> crate::setup_module::models::Platform {
        std::fs::write(home.join(marker), "").unwrap();
        crate::setup_module::models::Platform {
            name: "testcli".to_string(),
            config_path: marker.to_string(),
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
            cli: Some(cli),
            provider: None,
            tool_name: None,
            project_paths: Vec::new(),
            docs_url: None,
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: Vec::new(),
            install_commands: Vec::new(),
        }
    }

    fn write_config(canopy_dir: &std::path::Path, clis: Vec<CliConfig>) {
        let mut config = crate::domain::canopy_config::CanopyConfig::load(canopy_dir);
        config.clis = clis;
        config.save_for_tests(canopy_dir).unwrap();
    }

    /// Seed `config.toml` through the production writer so a byte comparison
    /// afterwards is meaningful: the refresh writes through the same writer.
    fn write_config_via_writer(canopy_dir: &std::path::Path, clis: &[CliConfig]) {
        crate::domain::settings::writer::write_clis(canopy_dir, clis).unwrap();
    }

    fn write_baseline(canopy_dir: &std::path::Path, clis: Vec<CliConfig>) {
        crate::domain::registry_baseline::RegistryBaseline {
            clis,
            ..Default::default()
        }
        .save(canopy_dir)
        .unwrap();
    }

    fn registry_of(
        platforms: Vec<crate::setup_module::models::Platform>,
    ) -> crate::setup_module::models::RegistryRaw {
        crate::setup_module::models::RegistryRaw {
            platforms,
            canonical_servers: crate::setup_module::models::CanonicalServers::default(),
        }
    }

    #[test]
    fn refresh_holds_headless_mode_and_keeps_config_bytes() {
        use std::io::Read as _;
        let home = tempfile::TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let local = cli_with("--old", None);
        write_config_via_writer(&canopy_dir, std::slice::from_ref(&local));
        write_baseline(&canopy_dir, vec![local]);
        let before = std::fs::read(canopy_dir.join("config.toml")).unwrap();

        let registry = registry_of(vec![detected(
            home.path(),
            "testcli.marker",
            serde_json::json!({"binary": "ls", "headless_mode": "--new"}),
        )]);
        let report = refresh_registry_inner(home.path(), || Ok(registry)).unwrap();

        // config.toml is byte-identical: the registry value never landed.
        let mut after = Vec::new();
        std::fs::File::open(canopy_dir.join("config.toml"))
            .unwrap()
            .read_to_end(&mut after)
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(report.pending.len(), 1);
        assert!(!report.up_to_date);
        assert!(report
            .changed
            .get("testcli")
            .is_none_or(|f| !f.contains(&"headless_mode".to_string())));
        assert!(report.render_text().contains("testcli: held headless_mode"));
        assert!(report.render_text().contains("run canopy registry pending"));

        // The baseline carries the old value, not the registry's.
        let baseline =
            crate::domain::registry_baseline::RegistryBaseline::load(&canopy_dir).unwrap();
        assert_eq!(baseline.get("testcli").unwrap().headless_mode, "--old");
    }

    #[test]
    fn refresh_applies_metadata_only_change() {
        let home = tempfile::TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");

        let local = cli_with("--headless", None);
        write_config(&canopy_dir, vec![local.clone()]);
        write_baseline(&canopy_dir, vec![local]);

        let mut platform = detected(
            home.path(),
            "testcli.marker",
            serde_json::json!({"binary": "ls", "headless_mode": "--headless"}),
        );
        platform.provider = Some("Someone".to_string());
        let report =
            refresh_registry_inner(home.path(), || Ok(registry_of(vec![platform]))).unwrap();

        assert!(report.pending.is_empty());
        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        assert_eq!(
            config.get_cli("testcli").unwrap().provider.as_deref(),
            Some("Someone")
        );
    }

    #[test]
    fn second_identical_refresh_keeps_a_single_entry() {
        let home = tempfile::TempDir::new().unwrap();
        let registry = || {
            Ok::<_, anyhow::Error>(registry_of(vec![detected(
                home.path(),
                "testcli.marker",
                serde_json::json!({"binary": "ls", "headless_mode": "--new"}),
            )]))
        };

        let local = cli_with("--old", None);
        write_config(&home.path().join(".canopy"), vec![local.clone()]);
        write_baseline(&home.path().join(".canopy"), vec![local]);

        refresh_registry_inner(home.path(), registry).unwrap();
        let first = load_pending(&home.path().join(".canopy"));
        assert_eq!(first.len(), 1);
        // Local config still holds the old value, so the second refresh
        // must stage the same change again — exactly once, never duplicated.
        let registry = || {
            Ok::<_, anyhow::Error>(registry_of(vec![detected(
                home.path(),
                "testcli.marker",
                serde_json::json!({"binary": "ls", "headless_mode": "--new"}),
            )]))
        };
        refresh_registry_inner(home.path(), registry).unwrap();
        let second = load_pending(&home.path().join(".canopy"));
        assert_eq!(second.len(), 1);
        assert!(pending_eq_ignoring_time(&first, &second));
    }
}
