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

// ── resume picker relaunches the picked session's own conversation ──

fn muse_cli_config() -> crate::domain::cli_config::CliConfig {
    crate::domain::cli_config::CliConfig {
        name: "muse".into(),
        binary: "muse".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: Some("--provider echo".into()),
        fallback_interactive_args: None,
        resume_args: Some("resume --last".into()),
        session_list_cmd: None,
        session_resume_cmd: Some("resume".into()),
        session_id_set_flag: None,
        session_list_format_args: None,
        session_id_pattern: None,
        session_title_flag: None,
        session_list_args: None,
        session_end_args: None,
        models_list_cmd: None,
        identity_check: None,
        identity_args: None,
        identity_pattern: None,
        accent_color: None,
        yolo_flag: Some("--yolo".into()),
        trust_flag: None,
        instruction_file: None,
        prompt_via_stdin: false,
        paste_submit_delay_ms: None,
        paste_submit_key: None,
        paste_submit_presses: 1,
        invocation_template: None,
        effort_declaration: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        decision_args: None,

        provider: None,
        tool_name: None,
    }
}

#[test]
fn selected_args_for_resume_choice_with_recorded_id_resumes_that_session() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_mode = NewTaskMode::Resume;
    dialog.available_clis = vec![Cli::new("muse")];
    dialog.cli_configs = vec![Some(muse_cli_config())];
    dialog.cli_index = 0;
    let mut picked = resumable_session("muse", "/proj", Some("--provider echo"));
    picked.harness_conversation_id = Some("uuid-picked".to_string());
    dialog.apply_resume_choice(picked);

    let args = dialog.selected_args().expect("resume args");
    assert_eq!(args, "--provider echo resume uuid-picked");
    assert!(
        !args.contains("--last"),
        "picking a session must resume that session, never the newest: {args}"
    );
}

#[test]
fn selected_args_for_resume_choice_without_id_keeps_the_last_fallback() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_mode = NewTaskMode::Resume;
    dialog.available_clis = vec![Cli::new("muse")];
    dialog.cli_configs = vec![Some(muse_cli_config())];
    dialog.cli_index = 0;
    dialog.apply_resume_choice(resumable_session("muse", "/proj", Some("--provider echo")));

    let args = dialog.selected_args().expect("resume args");
    assert_eq!(args, "--provider echo resume --last");
}

#[test]
fn new_with_usage_preselects_last_launch_prefs_and_respects_yolo_support() {
    let usage = crate::domain::usage_stats::CliUsage {
        last_launch: crate::domain::usage_stats::LastLaunchPrefs {
            workdir: Some("/tmp/prev-dir".to_string()),
            kind: Some("terminal".to_string()),
            yolo: true,
            sandbox: true,
        },
        ..Default::default()
    };

    let dialog = NewAgentDialog::new_with_usage(Some("/tmp/fallback"), &usage);
    assert_eq!(dialog.step, DialogStep::FolderAndKind);
    assert_eq!(dialog.working_dir, "/tmp/prev-dir");
    assert!(matches!(dialog.task_type, NewTaskType::Terminal));
    assert!(dialog.sandbox_mode);
    // yolo_mode is true only when the top harness has yolo_flag configured
    assert_eq!(dialog.yolo_mode, dialog.selected_yolo_flag().is_some());
}
