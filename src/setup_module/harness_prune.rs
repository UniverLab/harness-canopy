//! Self-healing for uninstalled harnesses: a harness whose binary is no
//! longer on `PATH` is removed from `config.clis` on its own — on daemon
//! startup, on a registry refresh, and on each `canopy setup`.
//!
//! A harness that is still referenced (crew member, graph node, scheduled
//! agent) is never pruned: removing it would strand the reference. Nor is a
//! harness pruned on a single sighting — the binary must be absent at two
//! checks at least ten minutes apart, or at one explicit `canopy setup`
//! (an explicit user action is not a transient `PATH` problem).

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::application::ports::StateRepository;
use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;

/// Grace window between the first sighting of a missing binary and the
/// actual prune: transient `PATH` problems clear within minutes, an
/// uninstall does not.
pub const HARNESS_ABSENT_GRACE: Duration = Duration::from_secs(600);

fn absent_key(name: &str) -> String {
    format!("harness_absent_since:{name}")
}

/// Why the prune pass is running. An explicit `canopy setup` is a deliberate
/// user action, so one sighting is enough; a background daemon check must
/// see the binary missing twice, ten minutes apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneTrigger {
    /// Daemon startup or a registry refresh.
    DaemonCheck,
    /// An explicit `canopy setup` run.
    Explicit,
}

/// Remove every uninstalled, unreferenced harness from `config.clis`.
/// Returns the pruned names. Installed harnesses refresh their absent
/// marker (clearing a stale first-sighting timestamp); referenced ones are
/// kept whatever the trigger.
pub fn prune_absent(
    config: &mut CanopyConfig,
    db: &Database,
    trigger: PruneTrigger,
    now: SystemTime,
) -> Vec<String> {
    let referenced = referenced_harnesses(db);
    let mut pruned = Vec::new();
    config.clis.retain(|cli| {
        if cli.is_installed() {
            let _ = db.set_state(&absent_key(&cli.name), "");
            return true;
        }
        if referenced.contains(cli.name.as_str()) {
            return true;
        }
        let drop_now = match trigger {
            PruneTrigger::Explicit => true,
            PruneTrigger::DaemonCheck => first_absent_at_least(db, &cli.name, now),
        };
        if drop_now {
            pruned.push(cli.name.clone());
            tracing::info!("Removed harness '{}' not on PATH", cli.name);
            // Clear the first-sighting marker so a later re-add with a still-
            // missing binary starts a fresh grace window instead of pruning
            // at once on the stale timestamp.
            let _ = db.set_state(&absent_key(&cli.name), "");
            false
        } else {
            true
        }
    });
    pruned
}

/// `true` when the harness was already recorded absent at least
/// [`HARNESS_ABSENT_GRACE`] ago. The first sighting records `now` (RFC 3339,
/// the state store's convention) and keeps the entry; an unparseable stored
/// value is treated as a first sighting.
fn first_absent_at_least(db: &Database, name: &str, now: SystemTime) -> bool {
    let stored = db
        .get_state(&absent_key(name))
        .ok()
        .flatten()
        .unwrap_or_default();
    let first_secs = stored
        .parse::<chrono::DateTime<chrono::Utc>>()
        .ok()
        .map(|dt| dt.timestamp().max(0) as u64);
    match first_secs {
        Some(first) => now
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .is_some_and(|elapsed| {
                elapsed.as_secs().saturating_sub(first) >= HARNESS_ABSENT_GRACE.as_secs()
            }),
        None => {
            let stamp = chrono::DateTime::<chrono::Utc>::from(now).to_rfc3339();
            let _ = db.set_state(&absent_key(name), &stamp);
            false
        }
    }
}

/// Names still referenced by a crew member, a graph node config, or a
/// scheduled agent (see [`crate::db::harness_refs`]).
fn referenced_harnesses(db: &Database) -> HashSet<String> {
    db.harness_references()
        .into_iter()
        .map(|reference| reference.harness)
        .collect()
}

/// Startup pass: reload the saved config, re-add baseline harnesses that
/// are installed again (setup or startup detection re-adds as today), prune
/// the absent ones, and write the result back atomically when anything
/// changed. A missing database or config read failure warns and keeps the
/// running config untouched.
pub fn prune_on_startup(data_dir: &Path, db: &Database) {
    let mut config = CanopyConfig::load(data_dir);
    let before: HashSet<String> = config.clis.iter().map(|c| c.name.clone()).collect();
    readd_baseline_installed(&mut config, data_dir);
    let now = SystemTime::now();
    prune_absent(&mut config, db, PruneTrigger::DaemonCheck, now);
    let after: HashSet<String> = config.clis.iter().map(|c| c.name.clone()).collect();
    if before != after {
        if let Err(e) = crate::domain::settings::writer::write_clis(data_dir, &config.clis) {
            tracing::warn!("Could not prune uninstalled harnesses from config: {e}");
        }
    }
}

/// Harnesses the registry baseline knows that resolve again get their saved
/// entry back — the mirror of pruning: an uninstall removes, a reinstall
/// restores, with no setup run in between.
fn readd_baseline_installed(config: &mut CanopyConfig, data_dir: &Path) {
    let Some(baseline) = crate::domain::registry_baseline::RegistryBaseline::load(data_dir) else {
        return;
    };
    let known: HashSet<String> = config.clis.iter().map(|c| c.name.clone()).collect();
    for cli in &baseline.clis {
        if !known.contains(cli.name.as_str()) && cli.is_installed() {
            config.clis.push(cli.clone());
        }
    }
}

/// The harness names still referenced by a crew member, a graph node, or a
/// scheduled agent. Empty when the database is absent or unreadable —
// harnesses must never be kept-or-dropped on a DB error beyond "keep".
pub fn harness_references(data_dir: &Path) -> HashSet<String> {
    let db_path = crate::domain::db_paths::database_path(data_dir);
    if !db_path.exists() {
        return HashSet::new();
    }
    Database::new_safe(&db_path, data_dir)
        .map(|db| {
            db.harness_references()
                .into_iter()
                .map(|reference| reference.harness)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::AgentRepository;
    use crate::application::ports::StateRepository;
    use crate::domain::cli_config::CliConfig;
    use crate::domain::crews::CrewMember;
    use crate::domain::models::{Agent, AgentAction, Cli};

    const GHOST: &str = "canopy-test-fixture-cli-missing-xyz";

    fn ghost_cli() -> CliConfig {
        CliConfig {
            name: "ghost".to_string(),
            binary: GHOST.to_string(),
            ..Default::default()
        }
    }

    fn installed_cli() -> CliConfig {
        CliConfig {
            name: "ls-harness".to_string(),
            binary: "ls".to_string(),
            ..Default::default()
        }
    }

    fn test_db() -> Database {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Database::new(&path).expect("create test db")
    }

    fn backdate(backdated_secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(backdated_secs)
    }

    #[test]
    fn explicit_trigger_drops_absent_harness_at_once() {
        let db = test_db();
        let mut config = CanopyConfig {
            clis: vec![ghost_cli()],
            ..Default::default()
        };
        let pruned = prune_absent(&mut config, &db, PruneTrigger::Explicit, SystemTime::now());
        assert_eq!(pruned, vec!["ghost".to_string()]);
        assert!(config.clis.is_empty());
    }

    #[test]
    fn daemon_check_first_sighting_keeps_and_marks() {
        let db = test_db();
        let mut config = CanopyConfig {
            clis: vec![ghost_cli()],
            ..Default::default()
        };
        let pruned = prune_absent(
            &mut config,
            &db,
            PruneTrigger::DaemonCheck,
            SystemTime::now(),
        );
        assert!(pruned.is_empty());
        assert_eq!(config.clis.len(), 1);
        let stored = db
            .get_state("harness_absent_since:ghost")
            .unwrap()
            .expect("first sighting is recorded");
        assert!(!stored.is_empty());
    }

    #[test]
    fn daemon_check_nine_minutes_later_still_keeps() {
        let db = test_db();
        let nine_minutes_ago = chrono::Utc::now() - chrono::Duration::minutes(9);
        db.set_state("harness_absent_since:ghost", &nine_minutes_ago.to_rfc3339())
            .unwrap();
        let mut config = CanopyConfig {
            clis: vec![ghost_cli()],
            ..Default::default()
        };
        let pruned = prune_absent(
            &mut config,
            &db,
            PruneTrigger::DaemonCheck,
            SystemTime::now(),
        );
        assert!(pruned.is_empty());
        assert_eq!(config.clis.len(), 1);
    }

    #[test]
    fn daemon_check_eleven_minutes_later_drops() {
        let db = test_db();
        let eleven_minutes_ago = chrono::Utc::now() - chrono::Duration::minutes(11);
        db.set_state(
            "harness_absent_since:ghost",
            &eleven_minutes_ago.to_rfc3339(),
        )
        .unwrap();
        let mut config = CanopyConfig {
            clis: vec![ghost_cli()],
            ..Default::default()
        };
        let pruned = prune_absent(
            &mut config,
            &db,
            PruneTrigger::DaemonCheck,
            SystemTime::now(),
        );
        assert_eq!(pruned, vec!["ghost".to_string()]);
        assert!(config.clis.is_empty());
    }

    #[test]
    fn referenced_harness_is_kept_by_crew_even_on_explicit_setup() {
        let db = test_db();
        db.crew_create(
            "lab-general",
            None,
            &[CrewMember {
                position: 1,
                platform: "ghost".to_string(),
                model: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            }],
        )
        .unwrap();
        let mut config = CanopyConfig {
            clis: vec![ghost_cli()],
            ..Default::default()
        };
        let pruned = prune_absent(&mut config, &db, PruneTrigger::Explicit, SystemTime::now());
        assert!(pruned.is_empty());
        assert_eq!(config.clis.len(), 1);
    }

    #[test]
    fn referenced_harness_is_kept_by_scheduled_agent() {
        let db = test_db();
        db.upsert_agent(&Agent {
            id: "nightly".to_string(),
            prompt: "do things".to_string(),
            trigger: None,
            cli: Cli::new("ghost"),
            model: None,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: chrono::Utc::now(),
            log_path: "/tmp/harness-prune-test.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: AgentAction::Headless,
        })
        .unwrap();
        let mut config = CanopyConfig {
            clis: vec![ghost_cli()],
            ..Default::default()
        };
        let pruned = prune_absent(&mut config, &db, PruneTrigger::DaemonCheck, backdate(0));
        assert!(pruned.is_empty());
        assert_eq!(config.clis.len(), 1);
    }

    #[test]
    fn referenced_harness_is_kept_by_graph_node() {
        use crate::domain::graphs::{
            Graph, GraphNode, GraphNodeKind, GraphSpec, GraphSpecStatus, GraphStatus,
        };
        let db = test_db();
        db.insert_graph(&Graph {
            id: "g1".to_string(),
            name: "refs-graph".to_string(),
            description: None,
            workdir: "/tmp/project".to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            archived: false,
            paused_by_reconciliation: false,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        })
        .unwrap();
        db.insert_graph_spec(&GraphSpec {
            id: "s1".to_string(),
            graph_id: Some("g1".to_string()),
            name: "refs-spec".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_type: None,
            spec_scope: None,
            spec_code: None,
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "n1".to_string(),
            spec_id: Some("s1".to_string()),
            graph_id: None,
            name: "builder".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "ghost"}),
            position: 0,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        let mut config = CanopyConfig {
            clis: vec![ghost_cli()],
            ..Default::default()
        };
        let pruned = prune_absent(&mut config, &db, PruneTrigger::DaemonCheck, backdate(0));
        assert!(pruned.is_empty());
        assert_eq!(config.clis.len(), 1);
    }

    #[test]
    fn referenced_harness_is_kept_by_graph_hook_without_a_node() {
        use crate::domain::graphs::{Graph, GraphHookEvent, GraphStatus};
        let db = test_db();
        let mut graph = Graph {
            id: "g1".to_string(),
            name: "refs-graph".to_string(),
            description: None,
            workdir: "/tmp/project".to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            archived: false,
            paused_by_reconciliation: false,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        };
        graph.hooks.insert(
            GraphHookEvent::OnCompleted,
            vec![crate::domain::graphs::GraphCompletionHook {
                platform: Some("ghost".to_string()),
                crew: None,
                spec_ids: None,
                model: None,
                effort: None,
                prompt: Some("done".to_string()),
                command: None,
                target_session_id: None,
                target_session_name: None,
                timeout_minutes: None,
                target_graph_id: None,
                queue_id: None,
                workdir_override: None,
                idea: None,
            }],
        );
        db.insert_graph(&graph).unwrap();
        let mut config = CanopyConfig {
            clis: vec![ghost_cli()],
            ..Default::default()
        };
        let pruned = prune_absent(&mut config, &db, PruneTrigger::Explicit, SystemTime::now());
        assert!(pruned.is_empty());
        assert_eq!(config.clis.len(), 1);
    }

    #[test]
    fn pruned_harness_clears_its_absent_marker_for_a_fresh_grace() {
        let db = test_db();
        let eleven_minutes_ago = chrono::Utc::now() - chrono::Duration::minutes(11);
        db.set_state(
            "harness_absent_since:ghost",
            &eleven_minutes_ago.to_rfc3339(),
        )
        .unwrap();
        let mut config = CanopyConfig {
            clis: vec![ghost_cli()],
            ..Default::default()
        };
        let pruned = prune_absent(
            &mut config,
            &db,
            PruneTrigger::DaemonCheck,
            SystemTime::now(),
        );
        assert_eq!(pruned, vec!["ghost".to_string()]);
        assert_eq!(
            db.get_state("harness_absent_since:ghost")
                .unwrap()
                .as_deref(),
            Some("")
        );
        // A later re-add with a still-missing binary starts a fresh grace
        // window instead of pruning at once on the stale timestamp.
        let mut config = CanopyConfig {
            clis: vec![ghost_cli()],
            ..Default::default()
        };
        let pruned = prune_absent(
            &mut config,
            &db,
            PruneTrigger::DaemonCheck,
            SystemTime::now(),
        );
        assert!(pruned.is_empty());
        assert_eq!(config.clis.len(), 1);
    }

    #[test]
    fn installed_harness_clears_a_stale_absent_marker() {
        let db = test_db();
        let an_hour_ago = chrono::Utc::now() - chrono::Duration::hours(1);
        db.set_state("harness_absent_since:ls-harness", &an_hour_ago.to_rfc3339())
            .unwrap();
        let mut config = CanopyConfig {
            clis: vec![installed_cli()],
            ..Default::default()
        };
        let pruned = prune_absent(
            &mut config,
            &db,
            PruneTrigger::DaemonCheck,
            SystemTime::now(),
        );
        assert!(pruned.is_empty());
        assert_eq!(config.clis.len(), 1);
        assert_eq!(
            db.get_state("harness_absent_since:ls-harness")
                .unwrap()
                .as_deref(),
            Some("")
        );
    }

    #[test]
    fn startup_prune_removes_absent_harness_from_config_atomically() {
        let home = tempfile::tempdir().unwrap();
        let canopy_dir = home.path().join(".canopy");
        let mut config = CanopyConfig::load(&canopy_dir);
        config.clis = vec![ghost_cli()];
        config.mark_configured();
        config.save_for_tests(&canopy_dir).unwrap();

        let db_path = crate::domain::db_paths::database_path(&canopy_dir);
        let db = Database::new_safe(&db_path, &canopy_dir).unwrap();
        let eleven_minutes_ago = chrono::Utc::now() - chrono::Duration::minutes(11);
        db.set_state(
            "harness_absent_since:ghost",
            &eleven_minutes_ago.to_rfc3339(),
        )
        .unwrap();
        drop(db);

        prune_on_startup(
            &canopy_dir,
            &Database::new_safe(&db_path, &canopy_dir).unwrap(),
        );

        let reloaded = CanopyConfig::load(&canopy_dir);
        assert!(reloaded.clis.is_empty());
        assert!(!canopy_dir.join("config.toml.tmp").exists());
        let content = std::fs::read_to_string(canopy_dir.join("config.toml")).unwrap();
        assert!(!content.contains("ghost"));
    }

    #[test]
    fn startup_prune_readds_baseline_harness_installed_again() {
        use crate::domain::registry_baseline::RegistryBaseline;
        let home = tempfile::tempdir().unwrap();
        let canopy_dir = home.path().join(".canopy");
        let mut config = CanopyConfig::load(&canopy_dir);
        config.mark_configured();
        config.save_for_tests(&canopy_dir).unwrap();
        RegistryBaseline {
            clis: vec![installed_cli()],
            ..Default::default()
        }
        .save(&canopy_dir)
        .unwrap();

        let db_path = crate::domain::db_paths::database_path(&canopy_dir);
        let db = Database::new_safe(&db_path, &canopy_dir).unwrap();
        prune_on_startup(&canopy_dir, &db);

        let reloaded = CanopyConfig::load(&canopy_dir);
        assert!(reloaded.get_cli("ls-harness").is_some());
    }
}
