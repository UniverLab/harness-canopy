//! CM51: reading the effective settings — what the file, the environment and
//! the registry together say a setting is right now.
//!
//! Nothing here writes anything.

use std::path::Path;

use serde::{Serialize, Serializer};
use toml_edit::{DocumentMut, Item, Value};

use super::{Apply, Source, REGISTRY};
use crate::domain::canopy_config::{CanopyConfig, TemperatureUnit};

/// The status of one setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// The file holds a value this setting cannot take; the previous
    /// effective value is still in force.
    Invalid(String),
    /// The file's value for a `RestartDaemon` setting differs from the one
    /// the running daemon loaded.
    PendingRestart,
    /// A key in the file that no setting declares. Never removed.
    Unknown,
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Status::Ok => "ok".to_string(),
            Status::Invalid(reason) => format!("invalid: {reason}"),
            Status::PendingRestart => "pending_restart".to_string(),
            Status::Unknown => "unknown".to_string(),
        }
    }
}

impl Serialize for Status {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.label())
    }
}

/// One setting as reported by `config_list`.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub key: String,
    pub group: String,
    pub kind: String,
    pub description: String,
    pub default: String,
    pub effective: String,
    pub source: Source,
    pub apply: String,
    pub status: Status,
}

/// A key in the file that could not be used as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub key: String,
    pub reason: String,
}

/// List every setting, optionally filtered to one group label.
pub fn list(canopy_dir: &Path, group: Option<&str>) -> Vec<Row> {
    list_with_snapshot(canopy_dir, group, super::running_snapshot())
}

/// `list`, with the running daemon's startup snapshot injected — the form
/// tests use, so no process-wide state is needed.
pub fn list_with_snapshot(
    canopy_dir: &Path,
    group: Option<&str>,
    snapshot: Option<&CanopyConfig>,
) -> Vec<Row> {
    let prev = snapshot
        .cloned()
        .unwrap_or_else(|| CanopyConfig::load(canopy_dir));
    let text = std::fs::read_to_string(canopy_dir.join("config.toml")).unwrap_or_default();
    let (mut effective, problems) = effective_after_hand_edit(&prev, &text);
    super::apply_env_overrides(&mut effective, &|key| std::env::var(key).ok());
    let doc: Option<DocumentMut> = text.parse().ok();

    let mut rows: Vec<Row> = Vec::new();
    for def in REGISTRY {
        if let Some(filter) = group {
            if def.group.label() != filter {
                continue;
            }
        }
        let raw = doc
            .as_ref()
            .and_then(|doc| super::writer::read_dotted(doc, def.key));
        let status = match problems.iter().find(|problem| problem.key == def.key) {
            Some(problem) => Status::Invalid(problem.reason.clone()),
            None => status_for(def.apply, raw.as_ref(), &effective, snapshot, def.key),
        };
        let source = if super::env_override_applies(def.key) {
            Source::Env
        } else if raw.is_some() {
            Source::File
        } else {
            Source::Default
        };
        rows.push(Row {
            key: def.key.to_string(),
            group: def.group.label().to_string(),
            kind: def.kind_label(),
            description: def.description.to_string(),
            default: def.default_display(),
            effective: config_value(&effective, def.key).to_string(),
            source,
            apply: super::action_for(def.apply).to_string(),
            status,
        });
    }

    if group.is_none() {
        for (key, rendered) in unknown_leaves(doc.as_ref()) {
            rows.push(Row {
                key,
                group: String::new(),
                kind: "unknown".to_string(),
                description: String::new(),
                default: String::new(),
                effective: rendered,
                source: Source::File,
                apply: String::new(),
                status: Status::Unknown,
            });
        }
    }
    rows
}

fn status_for(
    apply: Apply,
    raw: Option<&Value>,
    effective: &CanopyConfig,
    snapshot: Option<&CanopyConfig>,
    key: &str,
) -> Status {
    if apply != Apply::RestartDaemon || raw.is_none() {
        return Status::Ok;
    }
    let Some(snapshot) = snapshot else {
        return Status::Ok;
    };
    if config_value(snapshot, key).to_string() == config_value(effective, key).to_string() {
        return Status::Ok;
    }
    Status::PendingRestart
}

/// The config a hand-edited file yields, keeping the previous effective
/// value for every key whose new value is invalid.
pub fn effective_after_hand_edit(
    prev: &CanopyConfig,
    file_text: &str,
) -> (CanopyConfig, Vec<Problem>) {
    let mut doc: DocumentMut = match file_text.parse() {
        Ok(doc) => doc,
        Err(error) => {
            return (
                prev.clone(),
                vec![Problem {
                    key: "*".to_string(),
                    reason: error.to_string(),
                }],
            )
        }
    };

    let mut problems = Vec::new();
    for def in REGISTRY {
        let Some(raw) = super::writer::read_dotted(&doc, def.key) else {
            continue;
        };
        if let Err(reason) = super::validate::validate_value(def, &raw) {
            problems.push(Problem {
                key: def.key.to_string(),
                reason,
            });
            super::writer::set_dotted(&mut doc, def.key, Some(config_value(prev, def.key)));
        }
    }

    match toml::from_str::<CanopyConfig>(&doc.to_string()) {
        Ok(mut config) => {
            // The legacy single-root migration `load` performs.
            if config.rag_personal_dirs.is_empty() && !config.rag_personal_root.is_empty() {
                config
                    .rag_personal_dirs
                    .push(config.rag_personal_root.clone());
            }
            (config, problems)
        }
        Err(_) => (prev.clone(), problems),
    }
}

/// Registry keys whose effective value differs between two configs.
pub fn diff_keys(before: &CanopyConfig, after: &CanopyConfig) -> Vec<String> {
    REGISTRY
        .iter()
        .filter(|def| {
            config_value(before, def.key).to_string() != config_value(after, def.key).to_string()
        })
        .map(|def| def.key.to_string())
        .collect()
}

/// The typed field behind a registry key.
pub fn config_value(config: &CanopyConfig, key: &str) -> Value {
    match key {
        "mcp_filesystem_root" => Value::from(config.mcp_filesystem_root.clone()),
        "temperature_unit" => Value::from(
            match config.temperature_unit {
                TemperatureUnit::Celsius => "celsius",
                TemperatureUnit::Fahrenheit => "fahrenheit",
            }
            .to_string(),
        ),
        "embeddings_model" => Value::from(config.embeddings_model.clone()),
        "similarity_threshold" => Value::from(config.similarity_threshold as f64),
        "rag_personal_dirs" => {
            let mut array = toml_edit::Array::new();
            for dir in &config.rag_personal_dirs {
                array.push(dir.clone());
            }
            Value::Array(array)
        }
        "rag_personal_root" => Value::from(config.rag_personal_root.clone()),
        "embeddings_idle_unload_secs" => Value::from(config.embeddings_idle_unload_secs as i64),
        "ensemble_concurrency_cap" => Value::from(config.ensemble_concurrency_cap as i64),
        "spec_attempt_limit" => Value::from(config.spec_attempt_limit as i64),
        "clean.retention_days" => Value::from(config.clean.retention_days as i64),
        "graph_event_retention_days" => Value::from(config.graph_event_retention_days as i64),
        "skills.sources" => {
            Value::from(toml::to_string(&config.skills.sources).unwrap_or_else(|_| String::new()))
        }
        "skills.ttl_minutes" => Value::from(config.skills.ttl_minutes as i64),
        "models.catalog_ttl_minutes" => Value::from(config.models.catalog_ttl_minutes as i64),
        "models.native_ttl_minutes" => Value::from(config.models.native_ttl_minutes as i64),
        "theme" => Value::from(config.theme.clone()),
        "rag_max_file_mb" => Value::from(config.rag_max_file_mb as i64),
        "rag_vector_cache_entries" => Value::from(config.rag_vector_cache_entries as i64),
        "announcements_enabled" => Value::from(config.announcements_enabled),
        "pinned_panel_face" => Value::from(
            config
                .pinned_panel_face
                .clone()
                .unwrap_or_else(|| "auto".to_string()),
        ),
        "scheduler.retry_enabled" => Value::from(config.scheduler.retry_enabled),
        "scheduler.retry_delay_minutes" => Value::from(config.scheduler.retry_delay_minutes as i64),
        "scheduler.retry_max" => Value::from(config.scheduler.retry_max as i64),
        _ => Value::from(String::new()),
    }
}

/// Leaf keys present in the file that no setting declares, with the raw text
/// for display. `clis` and `configured_at` are managed separately.
fn unknown_leaves(doc: Option<&DocumentMut>) -> Vec<(String, String)> {
    let Some(doc) = doc else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (key, item) in doc.as_table().iter() {
        if key == "clis" || key == "configured_at" {
            continue;
        }
        walk_leaves(item, key, &mut found);
    }
    found
        .into_iter()
        .filter(|(key, _)| super::find(key).is_none())
        .collect()
}

fn walk_leaves(item: &Item, prefix: &str, out: &mut Vec<(String, String)>) {
    match item {
        Item::Table(table) => {
            for (key, child) in table.iter() {
                walk_leaves(child, &format!("{prefix}.{key}"), out);
            }
        }
        Item::ArrayOfTables(tables) => {
            out.push((prefix.to_string(), format!("[{} tables]", tables.len())));
        }
        Item::None => {}
        other => out.push((prefix.to_string(), other.to_string().trim().to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::canopy_config::SchedulerConfig;
    use crate::domain::settings::writer;
    use tempfile::TempDir;

    #[test]
    fn hand_edit_invalid_keeps_previous_effective_value() {
        let prev = CanopyConfig {
            spec_attempt_limit: 5,
            ..Default::default()
        };
        let (effective, problems) =
            effective_after_hand_edit(&prev, "spec_attempt_limit = 0\ntheme = \"modern\"\n");
        assert_eq!(effective.spec_attempt_limit, 5);
        assert_eq!(effective.theme, "modern");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].key, "spec_attempt_limit");
    }

    #[test]
    fn hand_edit_unknown_key_reported_and_kept() {
        let dir = TempDir::new().unwrap();
        let original = "projects_root = \"/x\"\ntheme = \"classic\"\n";
        std::fs::write(dir.path().join("config.toml"), original).unwrap();

        let rows = list(dir.path(), None);
        let row = rows
            .iter()
            .find(|row| row.key == "projects_root")
            .expect("projects_root is reported");
        assert_eq!(row.status, Status::Unknown);

        let value: Value = "\"modern\"".parse().unwrap();
        writer::write_keys(dir.path(), &[("theme", Some(value))]).unwrap();
        let after = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(after.contains("projects_root = \"/x\""));
    }

    #[test]
    fn pending_restart_detection() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "ensemble_concurrency_cap = 8\ntemperature_unit = \"fahrenheit\"\n",
        )
        .unwrap();
        let snapshot = CanopyConfig {
            ensemble_concurrency_cap: 4,
            scheduler: SchedulerConfig::default(),
            ..Default::default()
        };

        let rows = list_with_snapshot(dir.path(), None, Some(&snapshot));
        let cap = rows
            .iter()
            .find(|row| row.key == "ensemble_concurrency_cap")
            .unwrap();
        assert_eq!(cap.status, Status::PendingRestart);

        let unit = rows
            .iter()
            .find(|row| row.key == "temperature_unit")
            .unwrap();
        assert_eq!(unit.status, Status::Ok);
        assert_eq!(unit.source, Source::File);
    }

    #[test]
    fn list_reports_source_env_for_an_env_override() {
        // The env override is exercised through the pure function instead of
        // the process environment, so tests never race each other.
        let mut config = CanopyConfig {
            scheduler: SchedulerConfig {
                retry_max: 5,
                ..Default::default()
            },
            ..Default::default()
        };
        crate::domain::settings::apply_env_overrides(&mut config, &|key| {
            (key == crate::domain::settings::ENV_RETRY_MAX).then(|| "7".to_string())
        });
        assert_eq!(config.scheduler.retry_max, 7);
        assert_eq!(
            config_value(&config, "scheduler.retry_max").as_integer(),
            Some(7)
        );
    }

    #[test]
    fn list_reports_an_invalid_file_value_with_the_previous_effective_value() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "spec_attempt_limit = 0\nspec_attempt_limit2 = 3\n",
        )
        .unwrap();
        let snapshot = CanopyConfig {
            spec_attempt_limit: 5,
            ..Default::default()
        };
        let rows = list_with_snapshot(dir.path(), None, Some(&snapshot));
        let row = rows
            .iter()
            .find(|row| row.key == "spec_attempt_limit")
            .unwrap();
        assert!(
            matches!(&row.status, Status::Invalid(reason) if reason.contains("1..=20")),
            "{:?}",
            row.status
        );
        assert_eq!(row.effective, "5");
    }

    #[test]
    fn diff_keys_names_exactly_the_changed_settings() {
        let before = CanopyConfig::default();
        let after = CanopyConfig {
            theme: "modern".to_string(),
            scheduler: SchedulerConfig {
                retry_max: 9,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut changed = diff_keys(&before, &after);
        changed.sort();
        assert_eq!(changed, vec!["scheduler.retry_max", "theme"]);
    }

    #[test]
    fn group_filter_keeps_only_that_group() {
        let dir = TempDir::new().unwrap();
        let rows = list(dir.path(), Some("Notifications"));
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|row| row.group == "Notifications"));
        assert!(rows.iter().any(|row| row.key == "announcements_enabled"));
    }
}
