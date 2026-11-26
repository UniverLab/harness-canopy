use super::*;
use ratatui::crossterm::event::KeyCode;

fn entry(name: &str, status: HarnessInstallStatus, free_tier: Option<&str>) -> HarnessEntry {
    HarnessEntry {
        platform: Platform {
            name: name.to_string(),
            config_path: format!(".config/{name}"),
            config_format: None,
            toml_array_format: false,
            command_format: "separate".to_string(),
            mcp_servers_key: vec![],
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: std::collections::HashMap::new(),
            required_fields: std::collections::HashMap::new(),
            server_extras: std::collections::HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            instruction_precedence: vec![],
            user_instruction_file: None,
            provider: None,
            tool_name: None,
            project_paths: Vec::new(),
            docs_url: None,
            free_tier: free_tier.map(str::to_string),
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: Vec::new(),
            install_commands: Vec::new(),
            login_command: None,
            login_check_command: None,
            login_checked_at: None,
            cli: None,
        },
        status,
        binary_path: None,
    }
}

fn state_from(entries: Vec<HarnessEntry>) -> PickerState {
    let selected = entries
        .iter()
        .filter(|e| e.status != HarnessInstallStatus::NotInstalled)
        .map(|e| e.platform.name.clone())
        .collect();
    let registry_names = entries.iter().map(|e| e.platform.name.clone()).collect();
    PickerState {
        entries,
        registry_names,
        free_first: false,
        cursor: 0,
        selected,
        filter: String::new(),
        filtering: false,
        scroll: 0,
        install_notes: HashMap::new(),
        last_failure_tail: Vec::new(),
        login_status: HashMap::new(),
        login_check_rx: None,
    }
}

#[test]
fn picker_preselects_installed_toggles_only_installed_sorts_and_filters() {
    let mut state = state_from(vec![
        entry("z-paid", HarnessInstallStatus::NotInstalled, Some("no")),
        entry("a-free", HarnessInstallStatus::Installed, Some("yes")),
    ]);
    assert!(state.selected.contains("a-free"));
    assert!(!state.selected.contains("z-paid"));

    // Space on missing harness (cursor 0 = z-paid) does nothing.
    let rows = state.visible();
    handle_key(&mut state, &rows, KeyCode::Char(' '));
    assert!(!state.selected.contains("z-paid"));

    // f sorts free-tier first (a-free moves to index 0), then restores.
    handle_key(&mut state, &rows, KeyCode::Char('f'));
    assert_eq!(state.entries[0].platform.name, "a-free");
    let rows = state.visible();
    handle_key(&mut state, &rows, KeyCode::Char(' '));
    assert!(!state.selected.contains("a-free"));
    handle_key(&mut state, &rows, KeyCode::Char('f'));
    assert_eq!(state.entries[0].platform.name, "z-paid");

    // / enters filter mode, typing narrows, Esc clears.
    let rows = state.visible();
    handle_key(&mut state, &rows, KeyCode::Char('/'));
    handle_key(&mut state, &rows, KeyCode::Char('f'));
    assert_eq!(state.visible().len(), 1);
    handle_key(&mut state, &rows, KeyCode::Esc);
    assert_eq!(state.visible().len(), 2);
}

fn cmd_for_running_os(
    command: &str,
    requires: &[&str],
) -> crate::setup_module::models::InstallCommand {
    crate::setup_module::models::InstallCommand {
        os: crate::setup_module::install::running_os().to_string(),
        command: command.to_string(),
        description: "Install".to_string(),
        requires: requires.iter().map(|s| s.to_string()).collect(),
        source_url: "https://example.com/install".to_string(),
    }
}

#[test]
fn install_decline_runs_nothing_and_keeps_not_installed() {
    let mut e = entry("demo", HarnessInstallStatus::NotInstalled, None);
    e.platform.install_commands = vec![cmd_for_running_os("echo hi", &[])];
    let mut state = state_from(vec![e]);

    let mut runner_called = false;
    let mut confirm_saw_warning = false;
    perform_row_install(
        &mut state,
        0,
        None,
        "",
        |lines| {
            confirm_saw_warning = lines
                .iter()
                .any(|l| l.contains(crate::setup_module::install::CONSENT_WARNING));
            false
        },
        |_| {
            runner_called = true;
            Ok((0, vec![]))
        },
    );
    assert!(confirm_saw_warning);
    assert!(!runner_called);
    assert_eq!(state.entries[0].status, HarnessInstallStatus::NotInstalled);
    assert!(!state.selected.contains("demo"));
}

#[cfg(unix)]
#[test]
fn install_success_flips_row_and_selects_it() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("demo");
    let mut e = entry("demo", HarnessInstallStatus::NotInstalled, None);
    e.platform.cli = Some(serde_json::json!({"binary": "demo"}));
    e.platform.install_commands = vec![cmd_for_running_os("install demo", &[])];
    let mut state = state_from(vec![e]);
    let path_env = dir.path().display().to_string();

    perform_row_install(
        &mut state,
        0,
        Some(dir.path()),
        &path_env,
        |_| true,
        |_| {
            std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).unwrap();
            Ok((0, vec![]))
        },
    );
    assert_eq!(state.entries[0].status, HarnessInstallStatus::Installed);
    assert!(state.selected.contains("demo"));
}

#[test]
fn install_failure_keeps_last_20_lines() {
    let mut e = entry("demo", HarnessInstallStatus::NotInstalled, None);
    e.platform.install_commands = vec![cmd_for_running_os("install demo", &[])];
    let mut state = state_from(vec![e]);
    let tail: Vec<String> = (11..=30).map(|i| format!("line {i}")).collect();

    perform_row_install(&mut state, 0, None, "", |_| true, |_| Ok((2, tail.clone())));
    assert_eq!(state.entries[0].status, HarnessInstallStatus::NotInstalled);
    assert_eq!(state.last_failure_tail, tail);
    assert_eq!(
        state.install_notes.get("demo").map(String::as_str),
        Some("failed: 2")
    );
}

#[test]
fn row_install_hint_handles_missing_requires_and_no_command() {
    let mut needs = entry("needs-tool", HarnessInstallStatus::NotInstalled, None);
    needs.platform.install_commands = vec![cmd_for_running_os("npm i -g x", &["missing_npm_xyz"])];
    assert_eq!(
        format_row_install_hint(&needs, None).as_deref(),
        Some("needs missing_npm_xyz first")
    );

    let mut with_docs = entry("no-cmd-docs", HarnessInstallStatus::NotInstalled, None);
    with_docs.platform.docs_url = Some("https://example.com/docs".to_string());
    assert_eq!(
        format_row_install_hint(&with_docs, None).as_deref(),
        Some("install: see https://example.com/docs")
    );

    let without_docs = entry("no-cmd", HarnessInstallStatus::NotInstalled, None);
    assert_eq!(
        format_row_install_hint(&without_docs, None).as_deref(),
        Some("install: not available for this OS")
    );
}

#[test]
fn login_success_flips_row_to_authenticated() {
    let mut e = entry("demo", HarnessInstallStatus::Installed, None);
    e.platform.login_command = Some("demo login".to_string());
    e.platform.login_check_command = Some("demo whoami".to_string());
    let mut state = state_from(vec![e]);
    perform_row_login(
        &mut state,
        0,
        Some(std::path::Path::new("/tmp")),
        |_| crate::setup_module::login::LoginRun::Exited(0),
        |_| true,
    );
    assert_eq!(
        state.login_status.get("demo"),
        Some(&crate::setup_module::login::LoginStatus::Authenticated)
    );
}

#[test]
fn login_failing_check_leaves_not_authenticated_yet() {
    let mut e = entry("demo", HarnessInstallStatus::Installed, None);
    e.platform.login_command = Some("demo login".to_string());
    e.platform.login_check_command = Some("demo whoami".to_string());
    let mut state = state_from(vec![e]);
    perform_row_login(
        &mut state,
        0,
        Some(std::path::Path::new("/tmp")),
        |_| crate::setup_module::login::LoginRun::Exited(0),
        |_| false,
    );
    assert_eq!(
        state.login_status.get("demo"),
        Some(&crate::setup_module::login::LoginStatus::NotAuthenticated)
    );
}

#[test]
fn login_no_check_command_shows_not_verifiable() {
    let mut e = entry("demo", HarnessInstallStatus::Installed, None);
    e.platform.login_command = Some("demo login".to_string());
    let mut state = state_from(vec![e]);
    perform_row_login(
        &mut state,
        0,
        Some(std::path::Path::new("/tmp")),
        |_| crate::setup_module::login::LoginRun::Exited(0),
        |_| panic!("check should not run"),
    );
    assert_eq!(
        state.login_status.get("demo"),
        Some(&crate::setup_module::login::LoginStatus::NotVerifiable)
    );
}

#[test]
fn login_cancel_returns_cleanly() {
    let mut e = entry("demo", HarnessInstallStatus::Installed, None);
    e.platform.login_command = Some("demo login".to_string());
    e.platform.login_check_command = Some("demo whoami".to_string());
    let mut state = state_from(vec![e]);
    perform_row_login(
        &mut state,
        0,
        Some(std::path::Path::new("/tmp")),
        |_| crate::setup_module::login::LoginRun::Cancelled,
        |_| panic!("check should not run"),
    );
    assert!(!state.login_status.contains_key("demo"));
}

#[test]
fn r_retry_key_only_triggers_on_not_authenticated_installed_row() {
    let mut e = entry("demo", HarnessInstallStatus::Installed, None);
    e.platform.login_command = Some("demo login".to_string());
    let mut state = state_from(vec![e]);
    let rows = state.visible();

    // Unknown status: r does nothing, l triggers login.
    assert!(matches!(
        handle_key(&mut state, &rows, KeyCode::Char('r')),
        KeyAction::Continue
    ));
    assert!(matches!(
        handle_key(&mut state, &rows, KeyCode::Char('l')),
        KeyAction::Login
    ));

    state.login_status.insert(
        "demo".to_string(),
        crate::setup_module::login::LoginStatus::Authenticated,
    );
    let rows = state.visible();
    assert!(matches!(
        handle_key(&mut state, &rows, KeyCode::Char('r')),
        KeyAction::Continue
    ));

    state.login_status.insert(
        "demo".to_string(),
        crate::setup_module::login::LoginStatus::NotAuthenticated,
    );
    let rows = state.visible();
    assert!(matches!(
        handle_key(&mut state, &rows, KeyCode::Char('r')),
        KeyAction::Login
    ));

    // Not installed rows never trigger login/retry.
    let mut missing = entry("missing", HarnessInstallStatus::NotInstalled, None);
    missing.platform.login_command = Some("missing login".to_string());
    let mut state2 = state_from(vec![missing]);
    state2.login_status.insert(
        "missing".to_string(),
        crate::setup_module::login::LoginStatus::NotAuthenticated,
    );
    let rows2 = state2.visible();
    assert!(matches!(
        handle_key(&mut state2, &rows2, KeyCode::Char('r')),
        KeyAction::Continue
    ));
    assert!(matches!(
        handle_key(&mut state2, &rows2, KeyCode::Char('l')),
        KeyAction::Continue
    ));

    // Installed rows without a login_command never trigger login/retry.
    let plain = entry("plain", HarnessInstallStatus::Installed, None);
    let mut state3 = state_from(vec![plain]);
    let rows3 = state3.visible();
    assert!(matches!(
        handle_key(&mut state3, &rows3, KeyCode::Char('l')),
        KeyAction::Continue
    ));
    assert!(matches!(
        handle_key(&mut state3, &rows3, KeyCode::Char('r')),
        KeyAction::Continue
    ));
}
