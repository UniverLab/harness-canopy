//! CM51: the one declaration per setting. Dotted keys match the TOML in
//! `~/.canopy/config.toml`; nothing else in the tree defines a default.

use super::{Apply, DefaultValue, DurationUnit, Group, Kind, SettingDef};

/// Every setting canopy knows about.
pub static REGISTRY: &[SettingDef] = &[
    SettingDef {
        key: "mcp_filesystem_root",
        kind: Kind::String,
        default: DefaultValue::Home,
        group: Group::General,
        description: "Root directory the MCP filesystem server exposes.",
        apply: Apply::NextRun,
        wizard: false,
    },
    SettingDef {
        key: "temperature_unit",
        kind: Kind::Enum {
            options: &["celsius", "fahrenheit"],
        },
        default: DefaultValue::Literal("\"celsius\""),
        group: Group::TuiAppearance,
        description: "Temperature unit used by the TUI's system widgets.",
        apply: Apply::RestartTui,
        wizard: true,
    },
    SettingDef {
        key: "embeddings_model",
        kind: Kind::String,
        default: DefaultValue::Literal("\"\""),
        group: Group::RagIntelligence,
        description: "Embeddings model identifier used by the knowledge layer.",
        apply: Apply::RestartDaemon,
        wizard: true,
    },
    SettingDef {
        key: "similarity_threshold",
        kind: Kind::Float { min: 0.0, max: 1.0 },
        default: DefaultValue::Literal("0.25"),
        group: Group::RagIntelligence,
        description: "Lexical-cohesion threshold for semantic chunk merging (0.0 - 1.0).",
        apply: Apply::NextRun,
        wizard: true,
    },
    SettingDef {
        key: "rag_personal_dirs",
        kind: Kind::PathList,
        default: DefaultValue::Literal("[]"),
        group: Group::RagIntelligence,
        description: "Personal RAG directories; every directory is indexed recursively.",
        apply: Apply::NextRun,
        wizard: true,
    },
    SettingDef {
        key: "rag_personal_root",
        kind: Kind::String,
        default: DefaultValue::Literal("\"\""),
        group: Group::DaemonStorage,
        description: "Legacy single RAG path, migrated to rag_personal_dirs on first load.",
        apply: Apply::NextRun,
        wizard: false,
    },
    SettingDef {
        key: "embeddings_idle_unload_secs",
        kind: Kind::Duration {
            unit: DurationUnit::Seconds,
            min: 1,
            max: 86_400,
        },
        default: DefaultValue::Literal("600"),
        group: Group::RagIntelligence,
        description: "Seconds of inactivity after which the embedding model is unloaded.",
        apply: Apply::RestartDaemon,
        wizard: false,
    },
    SettingDef {
        key: "ensemble_concurrency_cap",
        kind: Kind::Int { min: 1, max: 64 },
        default: DefaultValue::Literal("4"),
        group: Group::GraphsEngine,
        description: "How many ensemble members run concurrently across every graph run.",
        apply: Apply::RestartDaemon,
        wizard: false,
    },
    SettingDef {
        key: "spec_attempt_limit",
        kind: Kind::Int { min: 1, max: 20 },
        default: DefaultValue::Literal("3"),
        group: Group::GraphsEngine,
        description: "Separate graph executions a spec may fail with a verdict before it is blocked.",
        apply: Apply::RestartDaemon,
        wizard: false,
    },
    SettingDef {
        key: "clean.retention_days",
        kind: Kind::Duration {
            unit: DurationUnit::Days,
            min: 1,
            max: 3_650,
        },
        default: DefaultValue::Literal("7"),
        group: Group::DaemonStorage,
        description: "Days of retention before canopy clean may remove sessions and files.",
        apply: Apply::NextRun,
        wizard: false,
    },
    SettingDef {
        key: "graph_event_retention_days",
        kind: Kind::Duration {
            unit: DurationUnit::Days,
            min: 1,
            max: 3_650,
        },
        default: DefaultValue::Literal("90"),
        group: Group::DaemonStorage,
        description: "Days a graph's durable event log is kept before canopy clean prunes it.",
        apply: Apply::NextRun,
        wizard: false,
    },
    SettingDef {
        key: "skills.sources",
        kind: Kind::Structured,
        default: DefaultValue::Literal("[{url=\"https://github.com/UniverLab/skills\"}]"),
        group: Group::AgentsHarnesses,
        description: "Git sources for the dynamic skill store, checked in list order.",
        apply: Apply::RestartDaemon,
        wizard: false,
    },
    SettingDef {
        key: "skills.ttl_minutes",
        kind: Kind::Duration {
            unit: DurationUnit::Minutes,
            min: 0,
            max: 10_080,
        },
        default: DefaultValue::Literal("15"),
        group: Group::AgentsHarnesses,
        description: "Minutes a fetched skill is served before its source is re-checked.",
        apply: Apply::NextRun,
        wizard: false,
    },
    SettingDef {
        key: "models.catalog_ttl_minutes",
        kind: Kind::Duration {
            unit: DurationUnit::Minutes,
            min: 1,
            max: 43_200,
        },
        default: DefaultValue::Literal("1440"),
        group: Group::AgentsHarnesses,
        description: "Minutes the models.dev catalog cache is served before a refresh.",
        apply: Apply::NextRun,
        wizard: false,
    },
    SettingDef {
        key: "models.native_ttl_minutes",
        kind: Kind::Duration {
            unit: DurationUnit::Minutes,
            min: 1,
            max: 43_200,
        },
        default: DefaultValue::Literal("60"),
        group: Group::AgentsHarnesses,
        description: "Minutes a platform's native CLI model enumeration is served.",
        apply: Apply::NextRun,
        wizard: false,
    },
    SettingDef {
        key: "theme",
        kind: Kind::Enum {
            options: &["classic", "modern"],
        },
        default: DefaultValue::Literal("\"classic\""),
        group: Group::TuiAppearance,
        description: "TUI color theme: classic (bordered) or modern (borderless).",
        apply: Apply::Hot,
        wizard: true,
    },
    SettingDef {
        key: "rag_max_file_mb",
        kind: Kind::Int {
            min: 1,
            max: crate::domain::canopy_config::RAG_MAX_FILE_MB_CEILING as i64,
        },
        default: DefaultValue::Literal("10"),
        group: Group::RagIntelligence,
        description: "Per-file indexing size cap for personal RAG, in MB.",
        apply: Apply::NextRun,
        wizard: true,
    },
    SettingDef {
        key: "rag_vector_cache_entries",
        kind: Kind::Int { min: 1, max: 4_096 },
        default: DefaultValue::Literal("64"),
        group: Group::RagIntelligence,
        description: "Ceiling on LanceDB's index cache, in entries.",
        apply: Apply::RestartDaemon,
        wizard: false,
    },
    SettingDef {
        key: "announcements_enabled",
        kind: Kind::Bool,
        default: DefaultValue::Literal("true"),
        group: Group::Notifications,
        description: "Show upgrade and tip announcements. Turn off with `canopy config set announcements_enabled false`.",
        apply: Apply::NextRun,
        wizard: false,
    },
    SettingDef {
        key: "pinned_panel_face",
        kind: Kind::Enum {
            options: &["auto", "activity", "knowledge", "graph", "crews"],
        },
        default: DefaultValue::Literal("\"auto\""),
        group: Group::TuiAppearance,
        description: "Pinned right-panel face; auto lets the panel switch on its own.",
        apply: Apply::Hot,
        wizard: false,
    },
    SettingDef {
        key: "scheduler.retry_enabled",
        kind: Kind::Bool,
        default: DefaultValue::Literal("true"),
        group: Group::GraphsEngine,
        description: "Re-attempt a failed scheduled run after a delay instead of waiting for its next slot.",
        apply: Apply::NextRun,
        wizard: false,
    },
    SettingDef {
        key: "scheduler.retry_delay_minutes",
        kind: Kind::Duration {
            unit: DurationUnit::Minutes,
            min: 1,
            max: 1_440,
        },
        default: DefaultValue::Literal("60"),
        group: Group::GraphsEngine,
        description: "Minutes to wait before a scheduled run is retried.",
        apply: Apply::NextRun,
        wizard: false,
    },
    SettingDef {
        key: "scheduler.retry_max",
        kind: Kind::Int { min: 0, max: 20 },
        default: DefaultValue::Literal("3"),
        group: Group::GraphsEngine,
        description: "How many times a failed scheduled run is retried.",
        apply: Apply::NextRun,
        wizard: false,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::settings::find;

    #[test]
    fn registry_has_one_entry_per_setting_and_they_are_unique() {
        assert_eq!(REGISTRY.len(), 23);
        for def in REGISTRY {
            assert_eq!(find(def.key).map(|d| d.key), Some(def.key));
        }
    }
}
