//! CLI handlers for `canopy missions`.
//!
//! Read-only: opens the local database directly, exactly like
//! `canopy agent show` (see `agent_cli.rs`), and never requires the daemon.
//! Unlock state comes from the same `daemon_state` keys `AchievementStore`
//! reads, so unknown `achievement:*` keys (e.g. a legacy
//! `achievement:loop_survivor`) are ignored and never counted.

use std::sync::Arc;

use anyhow::Result;

use crate::db::achievements::AchievementStore;
use crate::db::Database;
use crate::domain::db_paths::database_path;
use crate::domain::gamification::MISSIONS;

/// One mission row of the `canopy missions` report.
#[derive(Debug, serde::Serialize)]
pub(crate) struct MissionEntry {
    pub id: String,
    pub title: String,
    pub icon: String,
    pub category: String,
    pub challenge: String,
    #[serde(skip)]
    pub unlocked: bool,
    /// RFC 3339 UTC instant of the unlock, or null while locked.
    pub unlocked_at: Option<String>,
}

/// The full `canopy missions` report: every mission plus the unlock count.
#[derive(Debug, serde::Serialize)]
pub(crate) struct MissionsReport {
    pub unlocked: usize,
    pub total: usize,
    pub missions: Vec<MissionEntry>,
}

/// Build the report from an `AchievementStore`: one entry per registered
/// mission, in registry order.
pub(crate) fn build_missions_report(store: &AchievementStore) -> MissionsReport {
    let missions = MISSIONS
        .iter()
        .map(|def| MissionEntry {
            id: def.id.as_str().to_string(),
            title: def.title.to_string(),
            icon: def.icon.to_string(),
            category: def.category.as_str().to_string(),
            challenge: def.challenge.to_string(),
            unlocked: store.is_unlocked(def.id),
            unlocked_at: store
                .unlock_timestamp(def.id)
                .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        })
        .collect::<Vec<_>>();
    let unlocked = missions.iter().filter(|m| m.unlocked).count();
    MissionsReport {
        unlocked,
        total: missions.len(),
        missions,
    }
}

pub(crate) async fn handle_missions_action(json: bool) -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let db = Arc::new(Database::new_safe(&database_path(&data_dir), &data_dir)?);
    let store = AchievementStore::load(db)?;
    let report = build_missions_report(&store);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    print_missions_human(&report);
    Ok(())
}

/// Human-readable layout: every mission with its unlock state, then the total.
fn print_missions_human(report: &MissionsReport) {
    for mission in &report.missions {
        if mission.unlocked {
            let date = mission.unlocked_at.as_deref().unwrap_or("unknown date");
            println!(
                "{} {} [{}] — unlocked {date}",
                mission.icon, mission.title, mission.category
            );
        } else {
            println!(
                "{} {} [{}] — locked: {}",
                mission.icon, mission.title, mission.category, mission.challenge
            );
        }
    }
    println!("{} / {} unlocked", report.unlocked, report.total);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::StateRepository;
    use crate::domain::gamification::MissionId;
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
    fn listing_counts_22_ignores_loop_survivor() {
        let store = fixture_store();
        let report = build_missions_report(&store);
        assert_eq!(report.total, 28);
        assert_eq!(report.missions.len(), 28);
        assert_eq!(report.unlocked, 22);
        assert!(
            store.is_unlocked(MissionId::FireflyCatcher),
            "a fixture key must read back through the same keys AchievementStore uses"
        );
    }

    #[test]
    fn listing_json_parses() {
        let store = fixture_store();
        let report = build_missions_report(&store);
        let json = serde_json::to_string_pretty(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["unlocked"], 22);
        assert_eq!(value["total"], 28);
        let missions = value["missions"].as_array().unwrap();
        assert_eq!(missions.len(), 28);
        let first = &missions[0];
        for key in [
            "id",
            "title",
            "icon",
            "category",
            "challenge",
            "unlocked_at",
        ] {
            assert!(first.get(key).is_some(), "mission entry lacks {key}");
        }
        assert!(first["unlocked_at"].is_string());
        assert!(missions[27]["unlocked_at"].is_null());
    }
}
