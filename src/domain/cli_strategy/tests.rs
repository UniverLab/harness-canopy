//! `tests` tests (split from `src/domain/cli_strategy.rs`, pure move).
use super::*;

/// Uses an absolute (non-existent) path for `binary` so tests don't
/// depend on any real CLI being installed on the machine running them.
fn sample_strategy() -> CliStrategy {
    let mut env_vars = HashMap::new();
    env_vars.insert("FOO".to_string(), "bar".to_string());

    CliStrategy {
        binary: "/usr/local/bin/test-cli".to_string(),
        headless_mode: "--headless --quiet".to_string(),
        model_flag: Some("--model".to_string()),
        supports_working_dir: true,
        working_dir_flag: Some("--workdir".to_string()),
        env_vars,
        prompt_via_stdin: false,
        session_id_set_flag: None,
        session_list_cmd: None,
        session_list_format_args: None,
        session_id_pattern: None,
        session_title_flag: None,
        session_list_args: None,
        session_end_args: None,
        session_resume_cmd: None,
        trust_flag: None,
        invocation_template: None,
        effort_declaration: None,
        decision_args: None,
        identity: None,
    }
}

// ── CB3: real registry templates as fixtures ─────────────────
// All `invocation_template` strings below are copied verbatim from
// `canopy-registry` commit `0f90d39` (branch `feat/invocation-template`),
// one fixture per platform. Tests call `build_argv_from_template` directly
// and assert the exact argv word list — no CLI is spawned, no network.
fn strategy_with_template(template: &str) -> CliStrategy {
    let mut s = sample_strategy();
    s.headless_mode = String::new();
    s.invocation_template = Some(template.to_string());
    s
}

// ── CB44 identity check ──────────────────────────────────────

fn identity_cli(
    binary: &str,
    cmd: Option<&str>,
    contains: Option<&str>,
) -> super::super::cli_config::CliConfig {
    super::super::cli_config::CliConfig {
        name: "blackbox".to_string(),
        binary: binary.to_string(),
        identity_check: match (cmd, contains) {
            (Some(c), Some(s)) => Some(super::super::cli_config::IdentityCheck {
                cmd: c.to_string(),
                contains: s.to_string(),
            }),
            _ => None,
        },
        ..Default::default()
    }
}

fn write_identity_script(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

mod part01_tests;
mod part02_tests;
