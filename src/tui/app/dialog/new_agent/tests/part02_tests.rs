//! Part 02 of `tests` (split from `src/tui/app/dialog/new_agent.rs`, pure move).
use super::*;

// ── open_session_picker with entries ─────────────────────────

#[test]
fn open_session_picker_with_entries_opens() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.session_entries = vec![
        ("id1".to_string(), "Session 1".to_string()),
        ("id2".to_string(), "Session 2".to_string()),
    ];
    dialog.open_session_picker();
    assert!(dialog.session_picker_open);
}

// ── cli_picker sync ─────────────────────────────────────────

#[test]
fn sync_cli_picker_to_current_matches() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("a"), Cli::new("b"), Cli::new("c")];
    dialog.cli_configs = vec![None, None, None];
    dialog.cli_index = 2;
    dialog.sync_cli_picker_to_current();
    assert_eq!(dialog.cli_picker_idx, 2);
}

#[test]
fn sync_cli_picker_to_current_no_match() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("a"), Cli::new("b")];
    dialog.cli_configs = vec![None, None];
    dialog.cli_index = 99; // Not in list
    dialog.sync_cli_picker_to_current();
    assert_eq!(dialog.cli_picker_idx, 0);
}

// ── set_cli_index valid index ───────────────────────────────

#[test]
fn set_cli_index_valid_index() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("a"), Cli::new("b")];
    dialog.cli_configs = vec![None, None];
    dialog.set_cli_index(1);
    assert_eq!(dialog.cli_index, 1);
}

// ── detect_available_shells ──────────────────────────────────

#[test]
fn detect_available_shells_returns_at_least_bash() {
    let shells = detect_available_shells();
    assert!(!shells.is_empty());
    // bash should be present (it's a fallback)
    assert!(shells.iter().any(|s| s == "bash"));
}

// ── load_seed_options: count >= 2 ───────────────────────────

#[test]
fn load_seed_options_at_least_two() {
    let options = load_seed_options();
    assert!(options.len() >= 2);
}

// ── SeedOption Clone and Debug ──────────────────────────────

#[test]
fn seed_option_clone() {
    let opt = SeedOption::Seed {
        id: "id".to_string(),
        name: "name".to_string(),
    };
    let other = opt.clone();
    match other {
        SeedOption::Seed { id, name } => {
            assert_eq!(id, "id");
            assert_eq!(name, "name");
        }
        _ => panic!("expected Seed variant"),
    }
    // Verify original still usable
    assert!(matches!(opt, SeedOption::Seed { .. }));
}

// ── NewTaskType / BackgroundTrigger / NewTaskMode ────────────

#[test]
fn new_task_type_eq() {
    assert!(NewTaskType::Interactive == NewTaskType::Interactive);
    assert!(NewTaskType::Interactive != NewTaskType::Terminal);
    assert!(NewTaskType::Background != NewTaskType::Interactive);
}

#[test]
fn background_trigger_eq() {
    assert!(BackgroundTrigger::Cron == BackgroundTrigger::Cron);
    assert!(BackgroundTrigger::Cron != BackgroundTrigger::Watch);
}

#[test]
fn new_task_mode_eq() {
    assert!(NewTaskMode::Interactive == NewTaskMode::Interactive);
    assert!(NewTaskMode::Interactive != NewTaskMode::Resume);
}

// ── ordering survives a deferred usage.json/usage.toml split ────
//
// Reproduces the dialog symptom from the usage-stats migration spec:
// a still-running older binary keeps writing counts to the legacy
// `usage.json` after migration created `usage.toml`. The dialog's
// ordering must reflect those counts, not silently fall back to
// default order because it only ever looked at `usage.toml`.
#[test]
fn sort_clis_by_usage_reflects_counts_across_a_deferred_legacy_split() {
    let dir = tempfile::tempdir().unwrap();

    let mut stale_toml = crate::domain::usage_stats::CliUsage::default();
    stale_toml.record("codex");
    stale_toml.save(dir.path()).unwrap();

    // The still-running old binary's daemon keeps appending to the
    // legacy path after migration, so it ends up newer and with higher
    // counts than the migrated usage.toml snapshot.
    std::thread::sleep(std::time::Duration::from_millis(10));
    let mut fresher_legacy = crate::domain::usage_stats::CliUsage::default();
    fresher_legacy.record("claude");
    fresher_legacy.record("claude");
    fresher_legacy.record("claude");
    std::fs::write(
        dir.path().join("usage.json"),
        serde_json::to_string_pretty(&fresher_legacy).unwrap(),
    )
    .unwrap();

    let usage = crate::domain::usage_stats::CliUsage::load(dir.path());
    let pairs = vec![(Cli::new("codex"), None), (Cli::new("claude"), None)];

    let (sorted, _) = NewAgentDialog::sort_clis_by_usage(pairs, &usage);

    assert_eq!(
        sorted[0].as_str(),
        "claude",
        "the newer legacy counts must win the ordering, not the stale usage.toml snapshot"
    );
}
