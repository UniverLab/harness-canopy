//! CM51: the settings registry.
//!
//! Every setting in `~/.canopy/config.toml` is declared exactly once, here and
//! in [`registry`]: its type, its default, its group, whether it needs a
//! restart and whether the setup wizard asks for it. The CLI, the MCP tools,
//! the TUI and the setup wizard all read and write through this module, so a
//! default or a valid range has exactly one definition.

use std::sync::OnceLock;

use crate::domain::canopy_config::CanopyConfig;

pub mod inspect;
pub mod registry;
pub mod validate;
pub mod writer;

pub use registry::REGISTRY;

/// The value type a setting accepts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Bool,
    Int {
        min: i64,
        max: i64,
    },
    Float {
        min: f64,
        max: f64,
    },
    Enum {
        options: &'static [&'static str],
    },
    String,
    /// A chrono `strftime` format string; valid only if it formats a fixed instant.
    ChronoFormat,
    Duration {
        unit: DurationUnit,
        min: u64,
        max: u64,
    },
    /// A TOML array of directory paths; every entry must exist.
    PathList,
    /// A table or array-of-tables that `config_set` refuses (D6).
    Structured,
}

/// The unit a [`Kind::Duration`] value is counted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationUnit {
    Seconds,
    Minutes,
    Days,
}

impl DurationUnit {
    pub fn label(self) -> &'static str {
        match self {
            DurationUnit::Seconds => "seconds",
            DurationUnit::Minutes => "minutes",
            DurationUnit::Days => "days",
        }
    }
}

/// When a changed value takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    /// The running daemon and TUI pick it up without a restart.
    Hot,
    /// Read when a graph run, a probe or another operation starts.
    NextRun,
    RestartDaemon,
    RestartTui,
}

/// A registry default: a literal in TOML syntax, or the home directory
/// resolved at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultValue {
    Literal(&'static str),
    Home,
}

/// The group a setting is presented under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    General,
    TuiAppearance,
    GraphsEngine,
    AgentsHarnesses,
    RagIntelligence,
    DaemonStorage,
    Notifications,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::General => "General",
            Group::TuiAppearance => "TUI & appearance",
            Group::GraphsEngine => "Graphs & engine",
            Group::AgentsHarnesses => "Agents & harnesses",
            Group::RagIntelligence => "RAG & intelligence",
            Group::DaemonStorage => "Daemon & storage",
            Group::Notifications => "Notifications",
        }
    }
}

/// Where a setting's effective value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Default,
    File,
    Env,
}

/// One declared setting.
#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    /// Dotted path matching the TOML (`theme`, `clean.retention_days`).
    pub key: &'static str,
    pub kind: Kind,
    pub default: DefaultValue,
    pub group: Group,
    pub description: &'static str,
    pub apply: Apply,
    /// Whether the setup wizard asks for it.
    pub wizard: bool,
}

impl SettingDef {
    /// The TOML text of this setting's default, with `Home` resolved.
    pub fn default_value(&self) -> toml_edit::Value {
        match self.default {
            DefaultValue::Home => toml_edit::Value::from(home_dir_string()),
            DefaultValue::Literal(text) => text
                .parse::<toml_edit::Value>()
                .expect("registry default is valid TOML"),
        }
    }

    /// The default rendered for display.
    pub fn default_display(&self) -> String {
        self.default_value().to_string()
    }

    /// A short label for the kind, used in `config_list` and error messages.
    pub fn kind_label(&self) -> String {
        match self.kind {
            Kind::Bool => "bool".to_string(),
            Kind::Int { min, max } => format!("int ({min}..={max})"),
            Kind::Float { min, max } => format!("float ({min}..={max})"),
            Kind::Enum { options } => format!("enum ({})", options.join(",")),
            Kind::String => "string".to_string(),
            Kind::ChronoFormat => "chrono format".to_string(),
            Kind::Duration { unit, min, max } => {
                format!("duration ({min}..={max} {})", unit.label())
            }
            Kind::PathList => "path_list".to_string(),
            Kind::Structured => "structured".to_string(),
        }
    }
}

/// The home directory as a string, with the same `/` fallback the config
/// module uses.
pub fn home_dir_string() -> String {
    dirs::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|| "/".to_string())
}

/// Find one setting by its dotted key.
pub fn find(key: &str) -> Option<&'static SettingDef> {
    REGISTRY.iter().find(|def| def.key == key)
}

/// The default integer for `key` (0 when the key is unknown — a build test
/// asserts every key a caller names exists in the registry).
pub fn default_int(key: &str) -> i64 {
    find(key)
        .and_then(|def| def.default_value().as_integer())
        .unwrap_or(0)
}

/// The default boolean for `key`.
pub fn default_bool(key: &str) -> bool {
    find(key)
        .and_then(|def| def.default_value().as_bool())
        .unwrap_or(false)
}

/// The default float for `key`.
pub fn default_float(key: &str) -> f64 {
    find(key)
        .and_then(|def| def.default_value().as_float())
        .unwrap_or(0.0)
}

/// The default string for `key`.
pub fn default_string(key: &str) -> String {
    find(key)
        .and_then(|def| def.default_value().as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The default duration for `key`, in whatever unit the registry declares.
pub fn default_duration_units(key: &str) -> u64 {
    find(key)
        .and_then(|def| def.default_value().as_integer())
        .map(|n| n as u64)
        .unwrap_or(0)
}

/// What the user must do for a value to take effect.
pub fn action_for(apply: Apply) -> &'static str {
    match apply {
        Apply::Hot => "applied",
        Apply::NextRun => "applies on next run",
        Apply::RestartDaemon => "restart the daemon",
        Apply::RestartTui => "restart the TUI",
    }
}

// ── Environment overrides (D1) ──────────────────────────────────────

/// Environment variable overriding `scheduler.retry_enabled`.
pub const ENV_RETRY_ENABLED: &str = "CANOPY_RETRY_ENABLED";
/// Environment variable overriding `scheduler.retry_delay_minutes`.
pub const ENV_RETRY_DELAY_MINUTES: &str = "CANOPY_RETRY_DELAY_MINUTES";
/// Environment variable overriding `scheduler.retry_max`.
pub const ENV_RETRY_MAX: &str = "CANOPY_RETRY_MAX";

/// The environment variable overriding `key`, if any.
pub fn env_name_for(key: &str) -> Option<&'static str> {
    match key {
        "scheduler.retry_enabled" => Some(ENV_RETRY_ENABLED),
        "scheduler.retry_delay_minutes" => Some(ENV_RETRY_DELAY_MINUTES),
        "scheduler.retry_max" => Some(ENV_RETRY_MAX),
        _ => None,
    }
}

/// Whether an environment override applies to `key`: the variable is set
/// *and* its value parses the way [`apply_env_overrides`] will use it, so
/// `config_list`'s `source` column never claims `env` for a value that is
/// ignored. The lookup is injected so tests never touch the process
/// environment (D1).
pub fn env_override_applies(key: &str, env: &dyn Fn(&str) -> Option<String>) -> bool {
    let Some(name) = env_name_for(key) else {
        return false;
    };
    let Some(raw) = env(name) else {
        return false;
    };
    let raw = raw.trim();
    match key {
        "scheduler.retry_enabled" => parse_env_bool(raw).is_some(),
        "scheduler.retry_delay_minutes" => raw.parse::<u64>().is_ok(),
        "scheduler.retry_max" => raw.parse::<u32>().is_ok(),
        _ => false,
    }
}

fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Apply the `CANOPY_RETRY_*` environment overrides on top of a loaded
/// config. `env` is injected so tests never touch the process environment
/// (D1). An unparseable value is ignored, which is today's behaviour.
pub fn apply_env_overrides(config: &mut CanopyConfig, env: &dyn Fn(&str) -> Option<String>) {
    if let Some(raw) = env(ENV_RETRY_ENABLED).as_deref().and_then(parse_env_bool) {
        config.scheduler.retry_enabled = raw;
    }
    if let Some(raw) = env(ENV_RETRY_DELAY_MINUTES)
        .as_deref()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        config.scheduler.retry_delay_minutes = raw;
    }
    if let Some(raw) = env(ENV_RETRY_MAX)
        .as_deref()
        .and_then(|v| v.trim().parse::<u32>().ok())
    {
        config.scheduler.retry_max = raw;
    }
}

// ── The running snapshot (D12) ──────────────────────────────────────

static RUNNING_SNAPSHOT: OnceLock<CanopyConfig> = OnceLock::new();

/// Remember the config the running daemon loaded at startup, so
/// `config_list` can report `pending_restart` for keys that changed on disk
/// since. Called once, from the daemon.
pub fn set_running_snapshot(config: CanopyConfig) {
    let _ = RUNNING_SNAPSHOT.set(config);
}

/// The config the running daemon loaded at startup, if one was recorded.
pub fn running_snapshot() -> Option<&'static CanopyConfig> {
    RUNNING_SNAPSHOT.get()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::canopy_config::{CanopyConfig, SchedulerConfig};

    /// Every field `CanopyConfig` serializes must have a registry row, so a
    /// new field without one fails here (`clis` and `configured_at` are
    /// exempt by name).
    #[test]
    fn every_config_field_has_a_setting_def() {
        let value = default_config_value();
        let mut keys = Vec::new();
        flatten(&value, "", &mut keys);

        for key in &keys {
            if key == "clis" || key == "configured_at" {
                continue;
            }
            assert!(
                find(key).is_some(),
                "config field `{key}` has no SettingDef in the registry"
            );
        }
        // Fields absent from the serialized form (None / skip_serializing)
        // still need a row.
        assert!(find("rag_personal_root").is_some());
        assert!(find("pinned_panel_face").is_some());
    }

    /// The two models caches must not drift from the constants they were
    /// read from before this registry existed.
    #[test]
    fn models_registry_defaults_equal_models_db_ttls() {
        use crate::domain::models_db::{DEFAULT_CATALOG_TTL, DEFAULT_NATIVE_TTL};
        assert_eq!(
            default_duration_units("models.catalog_ttl_minutes"),
            DEFAULT_CATALOG_TTL.as_secs() / 60
        );
        assert_eq!(
            default_duration_units("models.native_ttl_minutes"),
            DEFAULT_NATIVE_TTL.as_secs() / 60
        );
    }

    /// The registry is the only place a default is defined, so its literals
    /// must equal what `CanopyConfig::default()` produces.
    #[test]
    fn registry_defaults_match_canopy_config_default() {
        let value = default_config_value();

        for def in REGISTRY {
            if matches!(def.kind, Kind::Structured) {
                continue;
            }
            if def.key == "pinned_panel_face" {
                assert_eq!(def.default_value().as_str(), Some("auto"));
                continue;
            }
            // Not serialized (`skip_serializing`), so absent from the
            // default config's TOML; its default is the empty string.
            if def.key == "rag_personal_root" {
                assert_eq!(def.default_value().as_str(), Some(""));
                continue;
            }
            if matches!(def.default, DefaultValue::Home) {
                assert_eq!(
                    def.default_value().as_str(),
                    Some(home_dir_string().as_str())
                );
                continue;
            }
            let expected = lookup(&value, def.key)
                .unwrap_or_else(|| panic!("no config default for `{}`", def.key));
            let actual = def.default_value();
            // Compared as rendered TOML: same text means the same value for
            // every kind, without caring which float/int width it uses.
            assert_eq!(
                expected.to_string(),
                actual.to_string(),
                "`{}` default drifted",
                def.key
            );
        }
    }

    /// The structured `skills.sources` default is skipped by the loop above,
    /// so it gets its own drift check: the registry literal must hold the
    /// one source `SkillsConfig::default()` starts with.
    #[test]
    fn skills_sources_registry_default_matches_the_model_default() {
        use crate::domain::canopy_config::SkillsConfig;
        let def = find("skills.sources").expect("skills.sources is declared");
        let text = def.default_value().to_string();
        let parsed = toml::from_str::<SkillsConfig>(&format!("sources = {text}\n"))
            .expect("registry default parses as SkillsConfig");
        assert_eq!(
            parsed.sources,
            SkillsConfig::default().sources,
            "skills.sources default drifted"
        );
    }

    /// `retry_max` env override wins over the file, and an unparseable value
    /// is ignored.
    #[test]
    fn env_override_precedence() {
        let mut config = CanopyConfig {
            scheduler: SchedulerConfig {
                retry_max: 5,
                ..Default::default()
            },
            ..Default::default()
        };
        apply_env_overrides(&mut config, &|k| {
            (k == ENV_RETRY_MAX).then(|| "7".to_string())
        });
        assert_eq!(config.scheduler.retry_max, 7);
        assert!(env_name_for("scheduler.retry_max").is_some());

        let mut config = CanopyConfig {
            scheduler: SchedulerConfig {
                retry_max: 5,
                ..Default::default()
            },
            ..Default::default()
        };
        apply_env_overrides(&mut config, &|k| {
            (k == ENV_RETRY_MAX).then(|| "x".to_string())
        });
        assert_eq!(config.scheduler.retry_max, 5);
    }

    #[test]
    fn action_for_covers_every_apply_mode() {
        assert_eq!(action_for(Apply::Hot), "applied");
        assert_eq!(action_for(Apply::NextRun), "applies on next run");
        assert_eq!(action_for(Apply::RestartDaemon), "restart the daemon");
        assert_eq!(action_for(Apply::RestartTui), "restart the TUI");
    }

    #[test]
    fn every_registry_key_is_unique_and_findable() {
        for (index, def) in REGISTRY.iter().enumerate() {
            assert!(
                !REGISTRY[index + 1..].iter().any(|o| o.key == def.key),
                "duplicate registry key {}",
                def.key
            );
            assert_eq!(find(def.key).map(|d| d.key), Some(def.key));
            assert!(
                !def.description.is_empty(),
                "{} has no description",
                def.key
            );
        }
    }

    /// The whole point of CM51: `config.toml` has one writer. Any new
    /// `CanopyConfig` serialize-to-file call site fails this.
    #[test]
    fn no_config_save_outside_writer() {
        // Built at runtime so this test's own source cannot match.
        let needles = [
            format!("{}{}", "CanopyConfig::", "save("),
            format!("{}{}", "config", ".save("),
        ];
        let mut offenders = Vec::new();
        walk_rs(std::path::Path::new("src"), &needles, &mut offenders);
        assert!(
            offenders.is_empty(),
            "CanopyConfig::save callers remain outside the writer: {offenders:?}"
        );
    }

    fn walk_rs(dir: &std::path::Path, needles: &[String], out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk_rs(&path, needles, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (number, line) in text.lines().enumerate() {
                if needles.iter().any(|needle| line.contains(needle)) {
                    out.push(format!("{}:{}", path.display(), number + 1));
                }
            }
        }
    }

    /// The default config as a TOML value, serialized the same way
    /// `CanopyConfig::save` does so `None` fields are skipped identically.
    fn default_config_value() -> toml::Value {
        let text = toml::to_string_pretty(&CanopyConfig::default()).expect("serializes");
        toml::Value::Table(text.parse::<toml::Table>().expect("re-parses"))
    }

    /// Flatten a TOML value into dotted leaf keys. Arrays are leaves so an
    /// array-of-tables reports `skills.sources`, not its elements.
    fn flatten(value: &toml::Value, prefix: &str, out: &mut Vec<String>) {
        let toml::Value::Table(table) = value else {
            return;
        };
        for (key, child) in table {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            match child {
                toml::Value::Table(_) => flatten(child, &path, out),
                _ => out.push(path),
            }
        }
    }

    /// The serialized default for a dotted path, if present.
    fn lookup<'a>(value: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
        let mut current = value;
        for part in key.split('.') {
            current = current.get(part)?;
        }
        Some(current)
    }
}
