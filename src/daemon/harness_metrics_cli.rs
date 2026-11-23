//! CLI handlers for `canopy metrics harnesses`.
//!
//! Read-only: loads `~/.canopy/usage.toml` through [`CliUsage::load`] and
//! never writes. Mission totals reuse `build_missions_report`, the same
//! source `canopy missions --json` prints.

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::Result;

use crate::daemon::missions_cli::build_missions_report;
use crate::db::achievements::AchievementStore;
use crate::db::Database;
use crate::domain::db_paths::database_path;
use crate::domain::usage_stats::CliUsage;

/// One harness row of the `canopy metrics harnesses` report.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct HarnessRow {
    pub harness: String,
    pub launches: u64,
    pub last_used: Option<String>,
}

/// The `missions` key of the harnesses JSON report.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct HarnessMissionsSummary {
    pub unlocked: usize,
    pub total: usize,
}

/// The full `canopy metrics harnesses` JSON report.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct HarnessReport {
    pub harnesses: Vec<HarnessRow>,
    pub first_run_at: Option<String>,
    pub missions: HarnessMissionsSummary,
}

/// Build one row per harness in the file: the union of `[counts]` and
/// `[last_used]` keys, sorted by launch count descending with ties broken
/// by harness name ascending.
pub(crate) fn build_harness_rows(usage: &CliUsage) -> Vec<HarnessRow> {
    let mut names = BTreeSet::new();
    for key in usage.counts.keys() {
        names.insert(key.clone());
    }
    for key in usage.last_used.keys() {
        names.insert(key.clone());
    }
    let mut rows: Vec<HarnessRow> = names
        .into_iter()
        .map(|harness| HarnessRow {
            launches: usage.counts.get(&harness).copied().unwrap_or(0),
            last_used: usage.last_used.get(&harness).cloned(),
            harness,
        })
        .collect();
    rows.sort_by(|a, b| b.launches.cmp(&a.launches).then(a.harness.cmp(&b.harness)));
    rows
}

/// Assemble the full report from already-loaded state: harness rows from
/// `usage`, mission totals from the same `build_missions_report` source
/// `canopy missions --json` prints.
pub(crate) fn build_harness_report(usage: &CliUsage, store: &AchievementStore) -> HarnessReport {
    let missions = build_missions_report(store);
    HarnessReport {
        harnesses: build_harness_rows(usage),
        first_run_at: usage.first_run_at.clone(),
        missions: HarnessMissionsSummary {
            unlocked: missions.unlocked,
            total: missions.total,
        },
    }
}

pub(crate) async fn handle_harnesses_action(json: bool) -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let usage = CliUsage::load(&data_dir);
    let db = Arc::new(Database::new_safe(&database_path(&data_dir), &data_dir)?);
    let store = AchievementStore::load(db)?;
    let report = build_harness_report(&usage, &store);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    print_harness_human(&report);
    Ok(())
}

/// Human-readable layout: the table, then the first-run line, then the
/// missions total in the exact `canopy missions` format.
fn print_harness_human(report: &HarnessReport) {
    if report.harnesses.is_empty() {
        println!("No harness launches recorded.");
    } else {
        println!("{:<30} {:>8} LAST_USED", "HARNESS", "LAUNCHES");
        for row in &report.harnesses {
            println!(
                "{:<30} {:>8} {}",
                row.harness,
                row.launches,
                row.last_used.as_deref().unwrap_or("never")
            );
        }
    }
    println!(
        "first run: {}",
        report.first_run_at.as_deref().unwrap_or("unknown")
    );
    println!(
        "{} / {} unlocked",
        report.missions.unlocked, report.missions.total
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::StateRepository;
    use crate::domain::gamification::MISSIONS;
    use tempfile::TempDir;

    fn fixture_store() -> AchievementStore {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
        for def in MISSIONS.iter().take(22) {
            db.set_state(&format!("achievement:{}", def.id.as_str()), "1700000000")
                .unwrap();
        }
        // Legacy unknown keys must be ignored, not counted.
        db.set_state("achievement:loop_survivor", "1").unwrap();
        AchievementStore::load(db).unwrap()
    }

    #[test]
    fn last_used_only_key_renders_zero_launches() {
        let mut usage = CliUsage::default();
        usage
            .last_used
            .insert("ghost".to_string(), "2024-05-01T00:00:00Z".to_string());

        let rows = build_harness_rows(&usage);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].harness, "ghost");
        assert_eq!(rows[0].launches, 0);
        assert_eq!(rows[0].last_used.as_deref(), Some("2024-05-01T00:00:00Z"));
    }

    #[test]
    fn counts_only_key_renders_null_last_used() {
        let mut usage = CliUsage::default();
        usage.counts.insert("fresh".to_string(), 4);

        let rows = build_harness_rows(&usage);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].harness, "fresh");
        assert_eq!(rows[0].launches, 4);
        assert!(rows[0].last_used.is_none());
    }

    #[test]
    fn equal_counts_sort_by_name_ascending() {
        let mut usage = CliUsage::default();
        for name in ["zeta", "alpha", "mid"] {
            usage.counts.insert(name.to_string(), 2);
        }
        usage.counts.insert("top".to_string(), 5);

        let rows = build_harness_rows(&usage);

        let names: Vec<&str> = rows.iter().map(|row| row.harness.as_str()).collect();
        assert_eq!(names, vec!["top", "alpha", "mid", "zeta"]);
    }

    #[test]
    fn missions_totals_match_build_missions_report_on_fixture() {
        let store = fixture_store();
        let expected = build_missions_report(&store);
        assert_eq!(expected.total, 28);
        assert_eq!(expected.unlocked, 22);

        let report = build_harness_report(&CliUsage::default(), &store);

        assert_eq!(report.missions.unlocked, expected.unlocked);
        assert_eq!(report.missions.total, expected.total);
        let value = serde_json::to_value(&report.missions).unwrap();
        assert_eq!(value["unlocked"], expected.unlocked);
        assert_eq!(value["total"], expected.total);
    }

    #[test]
    fn json_report_shape_has_harness_rows_first_run_and_missions() {
        let mut usage = CliUsage::default();
        usage.record("opencode");
        usage.first_run_at = Some("2024-01-01T00:00:00Z".to_string());
        let store = fixture_store();
        let report = build_harness_report(&usage, &store);

        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["first_run_at"], "2024-01-01T00:00:00Z");
        assert_eq!(value["missions"]["unlocked"], 22);
        assert_eq!(value["missions"]["total"], 28);
        let rows = value["harnesses"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["harness"], "opencode");
        assert_eq!(rows[0]["launches"], 1);
        assert!(rows[0]["last_used"].is_string());
    }
}
