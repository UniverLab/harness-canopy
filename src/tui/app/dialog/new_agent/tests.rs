//! `tests` tests (split from `src/tui/app/dialog/new_agent.rs`, pure move).
use super::*;
use crate::tui::app::session_resume::ResumableSession;
use tempfile::tempdir;

fn cli_config_with_session_list(cmd: Option<&str>) -> crate::domain::cli_config::CliConfig {
    crate::domain::cli_config::CliConfig {
        name: "antigravity".into(),
        binary: "agy".into(),
        headless_mode: String::new(),
        headless: true,
        ctrl_c_exits: false,
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        interactive_args: None,
        fallback_interactive_args: None,
        resume_args: Some("--continue".into()),
        session_list_cmd: cmd.map(|s| s.to_string()),
        session_resume_cmd: Some("--conversation".into()),
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
    }
}

// ── canopy-native resume picker: apply_resume_choice / args (C25) ──

fn resumable_session(cli: &str, working_dir: &str, args: Option<&str>) -> ResumableSession {
    ResumableSession {
        id: "s1".to_string(),
        name: "picked-session".to_string(),
        cli: cli.to_string(),
        last_active: "2026-08-19T10:00:00Z".to_string(),
        working_dir: working_dir.to_string(),
        args: args.map(str::to_string),
    }
}

mod part01_tests;
mod part02_tests;
