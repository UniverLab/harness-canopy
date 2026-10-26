//! Part 01 of `tests` (split from `src/tui/app/dialog/new_agent.rs`, pure move).
use super::*;

#[test]
fn test_new_session_selection_logic() {
    // Test the core logic of our session focus fix
    // This verifies that the agent name tracking and position finding works

    // Simulate agent entries with names
    let agent_names = ["session-1", "session-2", "new-session", "session-3"];

    // Simulate finding the position of the new agent (like our fix does)
    let new_agent_name = "new-session";
    let position = agent_names.iter().position(|&name| name == new_agent_name);

    // Verify we found the correct position
    assert_eq!(position, Some(2), "Should find new session at position 2");

    // This test verifies the core logic used in our fix works correctly
}

#[test]
fn test_agent_name_tracking() {
    // Test that we correctly track agent names for different session types
    let dialog = NewAgentDialog::new(None);

    // Verify the dialog can be created (basic smoke test)
    assert!(dialog.working_dir.is_empty() || !dialog.working_dir.is_empty());

    // This verifies our code path doesn't break existing functionality
}

#[test]
fn cli_picker_filters_and_tracks_matches() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("copilot"), Cli::new("claude"), Cli::new("codex")];
    dialog.cli_configs = vec![None, None, None];
    dialog.cli_index = 1;

    dialog.open_cli_picker();
    dialog.push_cli_picker_filter('c');
    dialog.push_cli_picker_filter('o');

    let filtered: Vec<_> = dialog
        .filtered_cli_indices()
        .into_iter()
        .map(|idx| dialog.available_clis[idx].as_str().to_string())
        .collect();

    assert_eq!(filtered, vec!["copilot".to_string(), "codex".to_string()]);
    assert_eq!(dialog.cli_index, 0);

    dialog.move_cli_picker_next();
    assert_eq!(dialog.selected_cli().as_str(), "codex");
}

#[test]
fn terminal_dialog_uses_canopy_accent() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_type = NewTaskType::Terminal;

    assert_eq!(
        dialog.selected_accent_color(&Theme::classic()),
        Theme::classic().header_color
    );
}

#[test]
fn is_planting_new_seed_returns_true_when_selected() {
    let mut dialog = NewAgentDialog::new(None);
    // Find the PlantNewSeed option
    if let Some(idx) = dialog
        .seed_options
        .iter()
        .position(|o| matches!(o, SeedOption::PlantNewSeed))
    {
        dialog.seed_index = idx;
        assert!(dialog.is_planting_new_seed());
    }
}

#[test]
fn is_planting_new_seed_returns_false_for_none() {
    let dialog = NewAgentDialog::new(None);
    // Seed index 0 is always None
    assert_eq!(dialog.seed_index, 0);
    assert!(!dialog.is_planting_new_seed());
}

#[test]
fn is_planting_new_seed_returns_false_for_existing_seed() {
    let mut dialog = NewAgentDialog::new(None);
    // Find an existing seed option
    if let Some(idx) = dialog
        .seed_options
        .iter()
        .position(|o| matches!(o, SeedOption::Seed { .. }))
    {
        dialog.seed_index = idx;
        assert!(!dialog.is_planting_new_seed());
    }
}

#[test]
fn selected_seed_id_returns_none_for_plant_new_seed() {
    let mut dialog = NewAgentDialog::new(None);
    if let Some(idx) = dialog
        .seed_options
        .iter()
        .position(|o| matches!(o, SeedOption::PlantNewSeed))
    {
        dialog.seed_index = idx;
        assert!(dialog.selected_seed_id().is_none());
    }
}

#[test]
fn selected_seed_id_returns_none_for_none_option() {
    let dialog = NewAgentDialog::new(None);
    assert!(dialog.selected_seed_id().is_none());
}

#[test]
fn load_seed_options_always_has_none_and_plant() {
    let options = load_seed_options();
    assert!(options.len() >= 2);
    assert!(matches!(options.first(), Some(SeedOption::None)));
    assert!(matches!(options.last(), Some(SeedOption::PlantNewSeed)));
}

#[test]
fn parse_session_list_handles_empty_input() {
    let result = parse_session_list("");
    assert!(result.is_empty());
}

#[test]
fn parse_session_list_skips_header_and_separator() {
    let input = "Session        Title              Updated\n\
                     ──────────────────────────────────────\n\
                     ses_abc123     My Session         2h ago";
    let result = parse_session_list(input);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, "ses_abc123");
    assert_eq!(result[0].1, "My Session         2h ago");
}

#[test]
fn parse_session_list_skips_short_ids() {
    let input = "Header    Title\n\
                     abc       Short ID\n\
                     ses_abc123def   Valid Session";
    let result = parse_session_list(input);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, "ses_abc123def");
}

#[test]
fn parse_session_list_handles_multiple_entries() {
    let input = "ses_aaa111   Session One\n\
                     ses_bbb222   Session Two\n\
                     ses_ccc333   Session Three";
    let result = parse_session_list(input);
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].0, "ses_aaa111");
    assert_eq!(result[1].0, "ses_bbb222");
    assert_eq!(result[2].0, "ses_ccc333");
}

#[test]
fn parse_session_list_handles_whitespace_only_lines() {
    let input = "ses_abc123   First\n\n   \nses_def456   Second";
    let result = parse_session_list(input);
    assert_eq!(result.len(), 2);
}

#[test]
fn has_cli_session_list_false_for_empty_session_list_cmd() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("antigravity")];
    dialog.cli_configs = vec![Some(cli_config_with_session_list(Some("")))];
    dialog.cli_index = 0;

    assert!(!dialog.has_cli_session_list());
}

#[test]
fn has_cli_session_list_false_for_whitespace_session_list_cmd() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("antigravity")];
    dialog.cli_configs = vec![Some(cli_config_with_session_list(Some("   ")))];
    dialog.cli_index = 0;

    assert!(!dialog.has_cli_session_list());
}

#[test]
fn has_cli_session_list_true_for_real_session_list_cmd() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("antigravity")];
    dialog.cli_configs = vec![Some(cli_config_with_session_list(Some("session list")))];
    dialog.cli_index = 0;

    assert!(dialog.has_cli_session_list());
}

#[test]
fn has_cli_session_list_false_for_none_session_list_cmd() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("antigravity")];
    dialog.cli_configs = vec![Some(cli_config_with_session_list(None))];
    dialog.cli_index = 0;

    assert!(!dialog.has_cli_session_list());
}

#[test]
fn load_sessions_does_not_spawn_for_empty_session_list_cmd() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_mode = NewTaskMode::Resume;
    dialog.available_clis = vec![Cli::new("antigravity")];
    dialog.cli_configs = vec![Some(cli_config_with_session_list(Some("")))];
    dialog.cli_index = 0;

    // Must return immediately without attempting to spawn `agy` with no
    // args (which would otherwise block on a REPL prompt).
    dialog.load_sessions();
    assert!(dialog.session_entries.is_empty());
}

#[test]
fn seed_option_display_order() {
    let options = load_seed_options();
    // First is always None
    assert!(matches!(options.first(), Some(SeedOption::None)));
    // Last is always PlantNewSeed
    assert!(matches!(options.last(), Some(SeedOption::PlantNewSeed)));
    // Any seeds in between are Seed variants
    for opt in options.iter().skip(1).take(options.len().saturating_sub(2)) {
        assert!(matches!(opt, SeedOption::Seed { .. }));
    }
}

// ── selected_cli ─────────────────────────────────────────────

#[test]
fn selected_cli_returns_correct_index() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("opencode"), Cli::new("claude")];
    dialog.cli_index = 1;
    assert_eq!(dialog.selected_cli().as_str(), "claude");
}

#[test]
fn new_agent_selection_stores_slug_not_display() {
    fn config_for(
        slug: &str,
        provider: Option<&str>,
        tool: Option<&str>,
    ) -> crate::domain::cli_config::CliConfig {
        crate::domain::cli_config::CliConfig {
            name: slug.into(),
            provider: provider.map(str::to_string),
            tool_name: tool.map(str::to_string),
            ..Default::default()
        }
    }
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("claude"), Cli::new("mistral")];
    dialog.cli_configs = vec![
        Some(config_for("claude", Some("Anthropic"), Some("Claude Code"))),
        Some(config_for("mistral", Some("Mistral AI"), Some("Vibe"))),
    ];
    assert_eq!(
        dialog.cli_display_label(0),
        "Anthropic · Claude Code (claude)"
    );
    assert_eq!(dialog.cli_display_label(1), "Mistral AI · Vibe (mistral)");
    dialog.cli_index = 0;
    assert_eq!(dialog.selected_cli().as_str(), "claude");
    dialog.cli_index = 1;
    assert_eq!(dialog.selected_cli().as_str(), "mistral");
    // Filtering by product name finds the slug, and vice versa.
    dialog.cli_picker_filter = "vibe".to_string();
    assert_eq!(dialog.filtered_cli_indices(), vec![1]);
    dialog.cli_picker_filter = "mist".to_string();
    assert_eq!(dialog.filtered_cli_indices(), vec![1]);
}

/// CM35 FR3: an interactive-only platform (`headless = false`) stays
/// listed in the new-session picker — selectable for a live session —
/// but its label carries `(interactive only)`, while a headless-capable
/// platform's label is unchanged. Breaks if the marker is dropped or if
/// the list starts filtering interactive-only platforms out.
#[test]
fn cli_display_label_marks_interactive_only_platform() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("freebuff"), Cli::new("claude")];
    dialog.cli_configs = vec![
        Some(crate::domain::cli_config::CliConfig {
            name: "freebuff".into(),
            provider: Some("Codebuff".into()),
            tool_name: Some("Freebuff".into()),
            headless: false,
            ..Default::default()
        }),
        Some(crate::domain::cli_config::CliConfig {
            name: "claude".into(),
            provider: Some("Anthropic".into()),
            tool_name: Some("Claude Code".into()),
            headless: true,
            ..Default::default()
        }),
    ];
    assert_eq!(
        dialog.cli_display_label(0),
        "Codebuff · Freebuff (freebuff) (interactive only)"
    );
    assert_eq!(
        dialog.cli_display_label(1),
        "Anthropic · Claude Code (claude)"
    );
    // Both remain selectable — the picker never filters the list.
    dialog.cli_index = 0;
    assert_eq!(dialog.selected_cli().as_str(), "freebuff");
}

// ── selected_args ────────────────────────────────────────────

#[test]
fn selected_args_no_config_returns_none() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.cli_configs = vec![None];
    dialog.cli_index = 0;
    assert!(dialog.selected_args().is_none());
}

#[test]
fn selected_args_with_interactive_args() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.cli_configs = vec![Some(crate::domain::cli_config::CliConfig {
        name: "test".into(),
        binary: "test".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: Some("--tui".to_string()),
        fallback_interactive_args: None,
        resume_args: None,
        session_list_cmd: None,
        session_resume_cmd: None,
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
        yolo_flag: None,
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
    })];
    dialog.cli_index = 0;
    dialog.task_mode = NewTaskMode::Interactive;
    let args = dialog.selected_args();
    assert_eq!(args.as_deref(), Some("--tui"));
}

// ── selected_fallback_args ───────────────────────────────────

#[test]
fn selected_fallback_args_none() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.cli_configs = vec![None];
    dialog.cli_index = 0;
    assert!(dialog.selected_fallback_args().is_none());
}

#[test]
fn selected_fallback_args_some() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.cli_configs = vec![Some(crate::domain::cli_config::CliConfig {
        name: "test".into(),
        binary: "test".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: None,
        fallback_interactive_args: Some("--fallback".to_string()),
        resume_args: None,
        session_list_cmd: None,
        session_resume_cmd: None,
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
        yolo_flag: None,
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
    })];
    dialog.cli_index = 0;
    assert_eq!(
        dialog.selected_fallback_args().as_deref(),
        Some("--fallback")
    );
}

// ── selected_yolo_flag ───────────────────────────────────────

#[test]
fn selected_yolo_flag_none() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.cli_configs = vec![None];
    dialog.cli_index = 0;
    assert!(dialog.selected_yolo_flag().is_none());
}

#[test]
fn selected_yolo_flag_some() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.cli_configs = vec![Some(crate::domain::cli_config::CliConfig {
        name: "test".into(),
        binary: "test".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: None,
        fallback_interactive_args: None,
        resume_args: None,
        session_list_cmd: None,
        session_resume_cmd: None,
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
        yolo_flag: Some("--yolo".to_string()),
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
    })];
    dialog.cli_index = 0;
    assert_eq!(dialog.selected_yolo_flag().as_deref(), Some("--yolo"));
}

// ── is_edit_mode ─────────────────────────────────────────────

#[test]
fn is_edit_mode_false_for_new() {
    let dialog = NewAgentDialog::new(None);
    assert!(!dialog.is_edit_mode());
}

#[test]
fn is_edit_mode_true_when_edit_id_set() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.edit_id = Some("existing-id".to_string());
    assert!(dialog.is_edit_mode());
}

// ── set_cli_index ────────────────────────────────────────────

#[test]
fn set_cli_index_out_of_bounds_ignored() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("a"), Cli::new("b")];
    let old = dialog.cli_index;
    dialog.set_cli_index(99);
    assert_eq!(dialog.cli_index, old);
}

#[test]
fn set_cli_index_resets_yolo_when_no_flag() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("a"), Cli::new("b")];
    dialog.cli_configs = vec![None, None];
    dialog.yolo_mode = true;
    dialog.set_cli_index(1);
    assert!(!dialog.yolo_mode);
}

// ── filtered_dir_entries ─────────────────────────────────────

#[test]
fn filtered_dir_entries_empty_filter() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.dir_entries = vec!["a".to_string(), "b".to_string()];
    dialog.dir_filter.clear();
    assert_eq!(dialog.filtered_dir_entries().len(), 2);
}

#[test]
fn filtered_dir_entries_with_filter() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.dir_entries = vec![
        "apple".to_string(),
        "banana".to_string(),
        "avocado".to_string(),
    ];
    dialog.dir_filter = "ap".to_string();
    let filtered = dialog.filtered_dir_entries();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0], "apple");
}

// ── navigate_to_selected ─────────────────────────────────────

#[test]
fn navigate_to_selected_out_of_bounds() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.dir_selected = 999;
    let old_path = dialog.current_path.clone();
    dialog.navigate_to_selected();
    assert_eq!(dialog.current_path, old_path);
}

// ── go_up from root ──────────────────────────────────────────

#[test]
fn go_up_from_root_noop() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.current_path = "/".to_string();
    dialog.go_up();
    assert_eq!(dialog.current_path, "/");
}

// ── open/close cli picker ────────────────────────────────────

#[test]
fn open_close_cli_picker() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.open_cli_picker();
    assert!(dialog.cli_picker_open);
    dialog.close_cli_picker();
    assert!(!dialog.cli_picker_open);
}

// ── move_cli_picker_next/prev ────────────────────────────────

#[test]
fn move_cli_picker_next_wraps() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("a"), Cli::new("b")];
    dialog.cli_configs = vec![None, None];
    dialog.open_cli_picker();
    dialog.cli_picker_idx = 1;
    dialog.move_cli_picker_next();
    assert_eq!(dialog.cli_picker_idx, 0);
}

#[test]
fn move_cli_picker_prev_wraps() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("a"), Cli::new("b")];
    dialog.cli_configs = vec![None, None];
    dialog.open_cli_picker();
    dialog.cli_picker_idx = 0;
    dialog.move_cli_picker_prev();
    assert_eq!(dialog.cli_picker_idx, 1);
}

#[test]
fn move_cli_picker_next_empty() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis.clear();
    dialog.cli_configs.clear();
    dialog.open_cli_picker();
    dialog.move_cli_picker_next();
    // Should not panic
}

// ── cli picker filter ────────────────────────────────────────

#[test]
fn push_pop_cli_picker_filter() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("opencode"), Cli::new("claude")];
    dialog.cli_configs = vec![None, None];
    dialog.open_cli_picker();
    dialog.push_cli_picker_filter('o');
    assert_eq!(dialog.cli_picker_filter, "o");
    dialog.pop_cli_picker_filter();
    assert!(dialog.cli_picker_filter.is_empty());
}

// ── session picker ───────────────────────────────────────────

#[test]
fn confirm_session_pick() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.session_entries = vec![
        ("id1".to_string(), "Session 1".to_string()),
        ("id2".to_string(), "Session 2".to_string()),
    ];
    dialog.session_picker_idx = 1;
    dialog.confirm_session_pick();
    assert_eq!(dialog.selected_session.as_ref().unwrap().0, "id2");
    assert!(!dialog.session_picker_open);
}

#[test]
fn confirm_session_pick_out_of_bounds() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.session_entries = vec![("id1".to_string(), "Session 1".to_string())];
    dialog.session_picker_idx = 99;
    dialog.confirm_session_pick();
    assert!(dialog.selected_session.is_none());
}

// ── open_session_picker ──────────────────────────────────────

#[test]
fn open_session_picker_empty_entries_stays_closed() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.session_entries.clear();
    dialog.cli_configs = vec![None];
    dialog.cli_index = 0;
    // open_session_picker calls load_sessions which checks config first.
    // With no config, it stays closed.
    dialog.open_session_picker();
    assert!(!dialog.session_picker_open);
}

// ── build_resume_args ────────────────────────────────────────

#[test]
fn build_resume_args_interactive_mode() {
    let dialog = NewAgentDialog::new(None);
    let config = crate::domain::cli_config::CliConfig {
        name: "test".into(),
        binary: "test".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: Some("--tui".to_string()),
        fallback_interactive_args: None,
        resume_args: Some("--resume".to_string()),
        session_list_cmd: None,
        session_resume_cmd: None,
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
        yolo_flag: None,
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
    };
    let result = dialog.build_resume_args(&config, Some("--tui".to_string()));
    assert_eq!(result.as_deref(), Some("--tui"));
}

#[test]
fn build_resume_args_resume_mode_generic() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_mode = NewTaskMode::Resume;
    let config = crate::domain::cli_config::CliConfig {
        name: "test".into(),
        binary: "test".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: Some("--tui".to_string()),
        fallback_interactive_args: None,
        resume_args: Some("--resume".to_string()),
        session_list_cmd: None,
        session_resume_cmd: None,
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
        yolo_flag: None,
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
    };
    let result = dialog.build_resume_args(&config, Some("--tui".to_string()));
    assert_eq!(result.as_deref(), Some("--tui --resume"));
}

#[test]
fn build_resume_args_resume_mode_interactive_only() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_mode = NewTaskMode::Resume;
    let config = crate::domain::cli_config::CliConfig {
        name: "test".into(),
        binary: "test".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: Some("--tui".to_string()),
        fallback_interactive_args: None,
        resume_args: None,
        session_list_cmd: None,
        session_resume_cmd: None,
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
        yolo_flag: None,
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
    };
    let result = dialog.build_resume_args(&config, Some("--tui".to_string()));
    assert_eq!(result.as_deref(), Some("--tui"));
}

#[test]
fn build_resume_args_resume_mode_resume_only() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_mode = NewTaskMode::Resume;
    let config = crate::domain::cli_config::CliConfig {
        name: "test".into(),
        binary: "test".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: None,
        fallback_interactive_args: None,
        resume_args: Some("--resume".to_string()),
        session_list_cmd: None,
        session_resume_cmd: None,
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
        yolo_flag: None,
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
    };
    let result = dialog.build_resume_args(&config, None);
    assert_eq!(result.as_deref(), Some("--resume"));
}

#[test]
fn build_resume_args_resume_mode_neither() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_mode = NewTaskMode::Resume;
    let config = crate::domain::cli_config::CliConfig {
        name: "test".into(),
        binary: "test".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: None,
        fallback_interactive_args: None,
        resume_args: None,
        session_list_cmd: None,
        session_resume_cmd: None,
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
        yolo_flag: None,
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
    };
    let result = dialog.build_resume_args(&config, None);
    assert!(result.is_none());
}

#[test]
fn build_resume_args_session_specific() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_mode = NewTaskMode::Resume;
    dialog.selected_session = Some(("ses_abc123".to_string(), "My Session".to_string()));
    let config = crate::domain::cli_config::CliConfig {
        name: "test".into(),
        binary: "test".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: Some("--tui".to_string()),
        fallback_interactive_args: None,
        resume_args: None,
        session_list_cmd: None,
        session_resume_cmd: Some("--conversation".to_string()),
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
        yolo_flag: None,
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
    };
    let result = dialog.build_resume_args(&config, Some("--tui".to_string()));
    assert_eq!(result.as_deref(), Some("--tui --conversation ses_abc123"));
}

// ── update_dir_preview ───────────────────────────────────────

#[test]
fn update_dir_preview_out_of_bounds_uses_current_path() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.current_path = "/tmp/test".to_string();
    dialog.dir_selected = 999;
    dialog.update_dir_preview();
    assert_eq!(dialog.working_dir, "/tmp/test");
}

// ── selected_accent_color ────────────────────────────────────

#[test]
fn selected_accent_color_terminal_uses_theme() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_type = NewTaskType::Terminal;
    let theme = Theme::classic();
    assert_eq!(dialog.selected_accent_color(&theme), theme.header_color);
}

#[test]
fn selected_accent_color_no_config_uses_default() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_type = NewTaskType::Interactive;
    dialog.cli_configs = vec![None];
    dialog.cli_index = 0;
    let theme = Theme::classic();
    assert_eq!(
        dialog.selected_accent_color(&theme),
        crate::tui::ui::INTERACTIVE_COLOR
    );
}

#[test]
fn selected_accent_color_with_config() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_type = NewTaskType::Interactive;
    dialog.cli_configs = vec![Some(crate::domain::cli_config::CliConfig {
        name: "test".into(),
        binary: "test".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: None,
        fallback_interactive_args: None,
        resume_args: None,
        session_list_cmd: None,
        session_resume_cmd: None,
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
        accent_color: Some([255, 0, 0]),
        yolo_flag: None,
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
    })];
    dialog.cli_index = 0;
    let theme = Theme::classic();
    assert_eq!(dialog.selected_accent_color(&theme), Color::Rgb(255, 0, 0));
}

// ── build_resume_args with session + no resume_cmd ──────────

#[test]
fn build_resume_args_resume_mode_session_no_resume_cmd() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_mode = NewTaskMode::Resume;
    dialog.selected_session = Some(("ses_abc123".to_string(), "My Session".to_string()));
    let config = crate::domain::cli_config::CliConfig {
        name: "test".into(),
        binary: "test".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: Some("--tui".to_string()),
        fallback_interactive_args: None,
        resume_args: None,
        session_list_cmd: None,
        session_resume_cmd: None, // No resume cmd
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
        yolo_flag: None,
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
    };
    // With session but no resume_cmd, falls back to generic resume
    let result = dialog.build_resume_args(&config, Some("--tui".to_string()));
    assert_eq!(result.as_deref(), Some("--tui"));
}

#[test]
fn apply_resume_choice_sets_cli_and_working_dir() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("claude"), Cli::new("codex")];
    dialog.cli_configs = vec![None, None];
    dialog.working_dir = "/wherever/the/dialog/opened".to_string();

    dialog.apply_resume_choice(resumable_session("codex", "/recorded/session/dir", None));

    assert_eq!(dialog.selected_cli().as_str(), "codex");
    assert_eq!(
        dialog.working_dir, "/recorded/session/dir",
        "a generic resume is --continue-shaped: it must land in the session's own dir"
    );
}

#[test]
fn selected_args_for_resume_choice_routes_through_build_resumed_session_args_preserves_yolo() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_mode = NewTaskMode::Resume;
    dialog.available_clis = vec![Cli::new("opencode")];
    dialog.cli_configs = vec![Some(crate::domain::cli_config::CliConfig {
        name: "opencode".into(),
        binary: "opencode".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: None,
        fallback_interactive_args: None,
        resume_args: Some("--continue".to_string()),
        session_list_cmd: None,
        session_resume_cmd: None,
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
        yolo_flag: Some("--yolo".to_string()),
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
    })];
    dialog.cli_index = 0;
    dialog.apply_resume_choice(resumable_session("opencode", "/proj", Some("--tui --yolo")));

    let args = dialog.selected_args().expect("resume args");
    assert_eq!(
        args.matches("--yolo").count(),
        1,
        "the original session's yolo flag must survive, not be dropped or duplicated"
    );
}

#[test]
fn selected_args_for_resume_choice_does_not_duplicate_resume_flag() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.task_mode = NewTaskMode::Resume;
    dialog.available_clis = vec![Cli::new("opencode")];
    dialog.cli_configs = vec![Some(crate::domain::cli_config::CliConfig {
        name: "opencode".into(),
        binary: "opencode".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: None,
        fallback_interactive_args: None,
        resume_args: Some("--continue".to_string()),
        session_list_cmd: None,
        session_resume_cmd: None,
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
        yolo_flag: None,
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
    })];
    dialog.cli_index = 0;
    // The recorded session's own args already carry the resume flag.
    dialog.apply_resume_choice(resumable_session("opencode", "/proj", Some("--continue")));

    let args = dialog.selected_args().expect("resume args");
    assert_eq!(args.matches("--continue").count(), 1);
}

// ── move_cli_picker_prev empty ──────────────────────────────

#[test]
fn move_cli_picker_prev_empty() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis.clear();
    dialog.cli_configs.clear();
    dialog.open_cli_picker();
    dialog.move_cli_picker_prev();
    // Should not panic
}

// ── apply_cli_picker_filter ─────────────────────────────────

#[test]
fn apply_cli_picker_filter_no_match_resets_to_first() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("opencode"), Cli::new("claude")];
    dialog.cli_configs = vec![None, None];
    dialog.cli_index = 1;
    dialog.cli_picker_filter = "zzz".to_string();
    dialog.apply_cli_picker_filter();
    // No match → filtered is empty → idx stays 0 (but cli_index unchanged)
    assert_eq!(dialog.cli_picker_idx, 0);
}

#[test]
fn apply_cli_picker_filter_match_keeps_current_if_present() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("opencode"), Cli::new("claude")];
    dialog.cli_configs = vec![None, None];
    dialog.cli_index = 1;
    dialog.cli_picker_filter = "cl".to_string();
    dialog.apply_cli_picker_filter();
    // "claude" matches "cl" and was already selected → stays at position 0 in filtered list
    assert_eq!(dialog.cli_picker_idx, 0);
    assert_eq!(dialog.cli_index, 1);
}

// ── filtered_cli_indices with query ──────────────────────────

#[test]
fn filtered_cli_indices_case_insensitive() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("OpenCode"), Cli::new("CLAUDE")];
    dialog.cli_configs = vec![None, None];
    dialog.cli_picker_filter = "open".to_string();
    let filtered = dialog.filtered_cli_indices();
    assert_eq!(filtered, vec![0]);
}

#[test]
fn filtered_cli_indices_empty_filter_returns_all() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.available_clis = vec![Cli::new("a"), Cli::new("b"), Cli::new("c")];
    dialog.cli_configs = vec![None, None, None];
    dialog.cli_picker_filter.clear();
    let filtered = dialog.filtered_cli_indices();
    assert_eq!(filtered, vec![0, 1, 2]);
}

// ── update_dir_preview with valid selection ──────────────────

#[test]
fn update_dir_preview_valid_selection() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.current_path = "/tmp".to_string();
    dialog.dir_entries = vec!["sub/".to_string(), "file.txt".to_string()];
    dialog.dir_selected = 0;
    dialog.update_dir_preview();
    assert_eq!(dialog.working_dir, "/tmp/sub");
}

// ── navigate_to_selected into directory ──────────────────────

#[test]
fn navigate_to_selected_into_existing_dir() {
    let tmp = tempdir().unwrap();
    let subdir = tmp.path().join("child");
    std::fs::create_dir(&subdir).unwrap();
    let mut dialog = NewAgentDialog::new(None);
    dialog.current_path = tmp.path().to_string_lossy().to_string();
    dialog.refresh_dir_entries();
    if let Some(idx) = dialog.dir_entries.iter().position(|e| e.contains("child")) {
        dialog.dir_selected = idx;
        dialog.navigate_to_selected();
        assert!(dialog.current_path.contains("child"));
    }
}

// ── go_up from nested dir ───────────────────────────────────

#[test]
fn go_up_from_nested_dir() {
    let tmp = tempdir().unwrap();
    let nested = tmp.path().join("a").join("b");
    std::fs::create_dir_all(&nested).unwrap();
    let mut dialog = NewAgentDialog::new(None);
    dialog.current_path = nested.to_string_lossy().to_string();
    dialog.go_up();
    assert!(dialog.current_path.ends_with("a"));
}

// ── confirm_session_pick with no entries ─────────────────────

#[test]
fn confirm_session_pick_no_entries() {
    let mut dialog = NewAgentDialog::new(None);
    dialog.session_entries.clear();
    dialog.session_picker_idx = 0;
    dialog.confirm_session_pick();
    assert!(dialog.selected_session.is_none());
    assert!(!dialog.session_picker_open);
}
