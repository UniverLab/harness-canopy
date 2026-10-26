//! Part 01 of `tests` (split from `src/daemon/doctor.rs`, pure move).
use super::*;

#[test]
fn parse_systemd_exec_start_binary_extracts_path_before_args() {
    let unit = "[Service]\nExecStart=/usr/local/bin/canopy serve --port 4177\n";
    assert_eq!(
        parse_systemd_exec_start_binary(unit),
        Some(PathBuf::from("/usr/local/bin/canopy"))
    );
}

#[test]
fn parse_systemd_exec_start_binary_none_without_exec_start() {
    let unit = "[Service]\nEnvironment=PATH=/usr/bin\n";
    assert_eq!(parse_systemd_exec_start_binary(unit), None);
}

#[test]
fn parse_launchd_program_binary_extracts_first_array_entry() {
    let plist = r#"<?xml version="1.0"?>
<plist>
<dict>
    <key>Label</key>
    <string>com.canopy</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/canopy</string>
        <string>serve</string>
        <string>--port</string>
        <string>4177</string>
    </array>
</dict>
</plist>
"#;
    assert_eq!(
        parse_launchd_program_binary(plist),
        Some(PathBuf::from("/usr/local/bin/canopy"))
    );
}

#[test]
fn parse_launchd_program_binary_none_without_program_arguments_key() {
    let plist = "<plist><dict><key>Label</key><string>com.canopy</string></dict></plist>";
    assert_eq!(parse_launchd_program_binary(plist), None);
}

#[test]
fn parse_unit_binary_dispatches_on_manager() {
    let systemd_unit = "ExecStart=/opt/canopy serve --port 1\n";
    assert_eq!(
        parse_unit_binary("systemd", systemd_unit),
        Some(PathBuf::from("/opt/canopy"))
    );

    let launchd_plist = "<key>ProgramArguments</key><array><string>/opt/canopy</string></array>";
    assert_eq!(
        parse_unit_binary("launchd", launchd_plist),
        Some(PathBuf::from("/opt/canopy"))
    );
}

#[test]
fn diagnose_service_unit_binary_missing_when_binary_does_not_exist() {
    let status = diagnose_service_unit_binary(
        Path::new("/does/not/exist/canopy"),
        false,
        Some(Path::new("/usr/bin/canopy")),
    );
    assert_eq!(status, ServiceUnitBinaryStatus::Missing);
}

#[test]
fn diagnose_service_unit_binary_missing_takes_precedence_over_skew() {
    // Even if a different `canopy` is on PATH, a unit naming a binary
    // that doesn't exist must report Missing, not Skew — that's the
    // actionable defect (reinstall), not a version mismatch.
    let status = diagnose_service_unit_binary(
        Path::new("/gone/canopy"),
        false,
        Some(Path::new("/usr/bin/canopy")),
    );
    assert_eq!(status, ServiceUnitBinaryStatus::Missing);
}

#[test]
fn diagnose_service_unit_binary_skew_when_paths_differ() {
    let status = diagnose_service_unit_binary(
        Path::new("/opt/canopy-old/canopy"),
        true,
        Some(Path::new("/usr/bin/canopy")),
    );
    assert_eq!(
        status,
        ServiceUnitBinaryStatus::Skew(PathBuf::from("/usr/bin/canopy"))
    );
}

#[test]
fn diagnose_service_unit_binary_consistent_when_paths_match() {
    let status = diagnose_service_unit_binary(
        Path::new("/usr/bin/canopy"),
        true,
        Some(Path::new("/usr/bin/canopy")),
    );
    assert_eq!(status, ServiceUnitBinaryStatus::Consistent);
}

#[test]
fn diagnose_service_unit_binary_consistent_when_nothing_on_path() {
    // Nothing to compare against — the unit's binary existing is enough.
    let status = diagnose_service_unit_binary(Path::new("/usr/bin/canopy"), true, None);
    assert_eq!(status, ServiceUnitBinaryStatus::Consistent);
}

#[test]
fn unit_defines_browser_true_when_present() {
    let content = "[Service]\nEnvironment=PATH=/usr/bin\nEnvironment=BROWSER=/x/wsl-browser\n";
    assert!(unit_defines_browser(content));
}

#[test]
fn unit_defines_browser_false_when_absent() {
    let content = "[Service]\nEnvironment=PATH=/usr/bin\nEnvironment=RUST_LOG=info\n";
    assert!(!unit_defines_browser(content));
}

#[test]
fn unit_defines_browser_false_when_empty_value() {
    let content = "[Service]\nEnvironment=BROWSER=\n";
    assert!(!unit_defines_browser(content));
}

#[test]
fn unit_defines_browser_false_when_quoted_empty_value() {
    let content = "[Service]\nEnvironment=BROWSER=\"\"\n";
    assert!(!unit_defines_browser(content));
}

// ── CB73: BROWSER check reads the effective environment ───────

#[test]
fn effective_browser_true_when_dropin_sets_it_despite_bare_unit() {
    // The CB73 bug verbatim: unit file has no BROWSER, the drop-in
    // does — with systemctl unavailable the file fallback must still
    // see the drop-in, or doctor proposes creating the file that
    // already exists.
    let dir = tempfile::tempdir().unwrap();
    let unit_path = dir.path().join("canopy.service");
    std::fs::write(&unit_path, "[Service]\nExecStart=/usr/bin/canopy serve\n").unwrap();
    let dropin_dir = dir.path().join("canopy.service.d");
    std::fs::create_dir_all(&dropin_dir).unwrap();
    std::fs::write(
        dropin_dir.join("browser.conf"),
        "[Service]\nEnvironment=BROWSER=/home/u/bin/wsl-browser\n",
    )
    .unwrap();
    assert!(read_effective_unit_browser(&unit_path, None));
}

#[test]
fn effective_browser_false_when_neither_sets_it() {
    let dir = tempfile::tempdir().unwrap();
    let unit_path = dir.path().join("canopy.service");
    std::fs::write(&unit_path, "[Service]\nExecStart=/usr/bin/canopy serve\n").unwrap();
    // No canopy.service.d/ at all, and no BROWSER in the unit: the
    // caller must reach the warning + fix-hint arm.
    assert!(!read_effective_unit_browser(&unit_path, None));
    // A missing unit file counts as empty, never as set.
    assert!(!read_effective_unit_browser(
        &dir.path().join("absent.service"),
        None
    ));
    assert_eq!(
        diagnose_browser_env(false, None),
        BrowserEnvStatus::UnitMissing
    );
}

#[test]
fn orphan_environ_missing_reported_when_unit_set() {
    // Unit is fine, orphan isn't: both facts reported, no fix hint.
    assert_eq!(
        diagnose_browser_env(true, Some(false)),
        BrowserEnvStatus::UnitSetOrphanMissing
    );
    assert!(!browser_set_in_proc_environ(b"PATH=/usr/bin\0FOO=1\0"));
    assert!(browser_set_in_proc_environ(b"BROWSER=/x\0"));
    // Entries are matched per NUL-separated entry, on their own
    // prefix: BROWSER after another var is found, and a key that
    // merely ends in "BROWSER" is not BROWSER.
    assert!(browser_set_in_proc_environ(b"PATH=/usr/bin\0BROWSER=/x"));
    assert!(!browser_set_in_proc_environ(b"NOBROWSER=x\0"));
    assert!(!browser_set_in_proc_environ(b"BROWSER=\0PATH=/x"));
}

#[test]
fn orphan_environ_set_reported_when_unit_missing() {
    // CB73 FR2: a running daemon that HAS BROWSER is a distinct
    // verdict — `running daemon: set` — and must never be printed as
    // "missing" just because the unit lacks it. Collapsing this
    // combination is the regression this test pins.
    assert_eq!(
        diagnose_browser_env(false, Some(true)),
        BrowserEnvStatus::UnitMissingOrphanSet
    );
    assert!(browser_set_in_proc_environ(
        b"BROWSER=/home/u/bin/wsl-browser\0"
    ));
}

#[test]
fn systemctl_show_parser_finds_browser_among_merged_vars() {
    // `show` collapses all Environment= lines into one, so BROWSER
    // may sit anywhere among merged variables.
    assert!(browser_set_in_systemctl_show(
        "Environment=PATH=/usr/bin BROWSER=/x/wsl-browser FOO=1\n"
    ));
    assert!(!browser_set_in_systemctl_show(
        "Environment=PATH=/usr/bin\n"
    ));
    assert!(!browser_set_in_systemctl_show("Environment=BROWSER=\n"));
    assert!(!browser_set_in_systemctl_show("Environment=\n"));
    assert!(!browser_set_in_systemctl_show(""));
    // Quoted value with a space stays one token and counts as set.
    assert!(browser_set_in_systemctl_show(
        "Environment=BROWSER=\"/x/my browser\"\n"
    ));
    // Quoted-but-empty is not a value, mirroring unit_defines_browser.
    assert!(!browser_set_in_systemctl_show("Environment=BROWSER=\"\"\n"));
}

#[test]
fn systemctl_show_takes_precedence_over_files() {
    let dir = tempfile::tempdir().unwrap();
    let unit_path = dir.path().join("canopy.service");
    std::fs::write(&unit_path, "[Service]\nExecStart=/usr/bin/canopy serve\n").unwrap();

    // show says set while the files don't → set (systemd's merged
    // view wins over parsing files).
    assert!(read_effective_unit_browser(
        &unit_path,
        Some("Environment=BROWSER=/from-show\n")
    ));

    // show says unset while a drop-in on disk has it → not set
    // (show is authoritative; files must not override it).
    let dropin_dir = dir.path().join("canopy.service.d");
    std::fs::create_dir_all(&dropin_dir).unwrap();
    std::fs::write(
        dropin_dir.join("browser.conf"),
        "Environment=BROWSER=/x/wsl-browser\n",
    )
    .unwrap();
    assert!(!read_effective_unit_browser(
        &unit_path,
        Some("Environment=\n")
    ));
}

#[test]
fn diagnose_browser_env_truth_table() {
    // Exhaustive over unit_set × orphan_environ_set (None = no
    // orphan): any branch swap or collapsed combination misreports
    // CB73.
    assert_eq!(diagnose_browser_env(true, None), BrowserEnvStatus::UnitSet);
    assert_eq!(
        diagnose_browser_env(false, None),
        BrowserEnvStatus::UnitMissing
    );
    assert_eq!(
        diagnose_browser_env(true, Some(true)),
        BrowserEnvStatus::UnitSetOrphanSet
    );
    assert_eq!(
        diagnose_browser_env(true, Some(false)),
        BrowserEnvStatus::UnitSetOrphanMissing
    );
    assert_eq!(
        diagnose_browser_env(false, Some(true)),
        BrowserEnvStatus::UnitMissingOrphanSet
    );
    assert_eq!(
        diagnose_browser_env(false, Some(false)),
        BrowserEnvStatus::UnitMissingOrphanMissing
    );
}

#[test]
fn unit_defines_browser_accepts_indented_dropin_line() {
    assert!(unit_defines_browser(
        " [Service]\n Environment=BROWSER=/x\n"
    ));
    // The quote rule is unchanged: indented but empty is still empty.
    assert!(!unit_defines_browser(" Environment=BROWSER=\"\"\n"));
}

#[cfg(target_os = "linux")]
#[test]
fn report_service_unit_no_browser_issue_when_effective_env_set() {
    // The CB73 bug verbatim, wired end-to-end: systemctl reports
    // BROWSER in the merged environment → success arm → the fix
    // hint and its issue must not appear even though the unit file
    // on disk names no BROWSER at all.
    let dir = tempfile::tempdir().unwrap();
    write_minimal_unit(dir.path());
    let mut issues = Vec::new();
    report_service_unit(
        dir.path(),
        None,
        None,
        Some("Environment=BROWSER=/x/wsl-browser\n"),
        None,
        None,
        &mut issues,
    );
    assert!(browser_issues(&issues).is_empty(), "{issues:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn report_service_unit_orphan_missing_blames_the_orphan_not_the_unit() {
    // Unit effective env is set, orphan's environ isn't: FR3 — no
    // drop-in hint (the file it creates already exists); the issue
    // pushed must be the orphan-labeled one.
    let dir = tempfile::tempdir().unwrap();
    write_minimal_unit(dir.path());
    let mut issues = Vec::new();
    report_service_unit(
        dir.path(),
        Some(4242),
        None,
        Some("Environment=BROWSER=/x/wsl-browser\n"),
        Some(b"PATH=/usr/bin\0"),
        None,
        &mut issues,
    );
    let browser = browser_issues(&issues);
    assert_eq!(browser.len(), 1, "{browser:?}");
    assert!(browser[0].contains("orphan"), "{}", browser[0]);
    assert!(
        !browser[0].contains("Daemon unit has no BROWSER"),
        "fix hint must not fire when the effective env is set: {}",
        browser[0]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn report_service_unit_unit_missing_orphan_set_pushes_unit_issue() {
    // FR2's fourth combination: the running orphan HAS BROWSER but
    // the unit's effective environment lacks it. The unit-missing
    // arm must fire (hint allowed per FR3) — and the arm must not
    // falsely claim the daemon is missing BROWSER.
    let dir = tempfile::tempdir().unwrap();
    write_minimal_unit(dir.path());
    let mut issues = Vec::new();
    report_service_unit(
        dir.path(),
        Some(4242),
        Some(99),
        Some("Environment=PATH=/usr/bin\n"),
        Some(b"BROWSER=/x/wsl-browser\0"),
        None,
        &mut issues,
    );
    let browser = browser_issues(&issues);
    assert_eq!(browser.len(), 1, "{browser:?}");
    assert!(
        browser[0].contains("Daemon unit has no BROWSER"),
        "{}",
        browser[0]
    );
    assert!(
        !browser[0].contains("without BROWSER (unit"),
        "the orphan has BROWSER and must not be reported missing: {}",
        browser[0]
    );
}

/// CB81 FR3: pin the parse of `systemctl --user show -p OOMPolicy`.
#[cfg(target_os = "linux")]
#[test]
fn oom_policy_is_stop_parser() {
    assert!(oom_policy_is_stop("OOMPolicy=stop\n"));
    assert!(oom_policy_is_stop("OOMPolicy=stop"));
    assert!(!oom_policy_is_stop("OOMPolicy=continue\n"));
    assert!(!oom_policy_is_stop("OOMPolicy=kill\n"));
    assert!(!oom_policy_is_stop("Environment=PATH=/usr/bin\n"));
    assert!(!oom_policy_is_stop(""));
}

/// CB81 FR3: an installed unit whose effective OOMPolicy is `stop`
/// warns with the verbatim fix wording.
#[cfg(target_os = "linux")]
#[test]
fn report_service_unit_warns_when_oom_policy_stop() {
    let dir = tempfile::tempdir().unwrap();
    write_minimal_unit(dir.path());
    let mut issues = Vec::new();
    report_service_unit(
        dir.path(),
        None,
        None,
        None,
        None,
        Some("OOMPolicy=stop\n"),
        &mut issues,
    );
    let oom = oom_policy_issues(&issues);
    assert_eq!(oom.len(), 1, "{issues:?}");
    assert!(oom[0].contains("OOMPolicy=stop"));
    assert!(oom[0].contains("a node that runs out of memory stops the daemon"));
    assert!(oom[0].contains("reinstall the service (canopy daemon install)"));
    assert!(oom[0].contains("OOMPolicy=continue"));
}

/// CB81 FR3: `OOMPolicy=continue` must not warn (no false positive).
#[cfg(target_os = "linux")]
#[test]
fn report_service_unit_silent_when_oom_policy_continue() {
    let dir = tempfile::tempdir().unwrap();
    write_minimal_unit(dir.path());
    let mut issues = Vec::new();
    report_service_unit(
        dir.path(),
        None,
        None,
        None,
        None,
        Some("OOMPolicy=continue\n"),
        &mut issues,
    );
    assert!(oom_policy_issues(&issues).is_empty(), "{issues:?}");
}

/// CB81 FR3: systemctl unavailable (`None`) must not warn.
#[cfg(target_os = "linux")]
#[test]
fn report_service_unit_silent_when_oom_policy_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    write_minimal_unit(dir.path());
    let mut issues = Vec::new();
    report_service_unit(dir.path(), None, None, None, None, None, &mut issues);
    assert!(oom_policy_issues(&issues).is_empty(), "{issues:?}");
}

#[test]
fn doctor_renders_display_with_slug_suffix() {
    let mut cli = crate::domain::cli_config::CliConfig {
        name: "mistral".to_string(),
        provider: Some("Mistral AI".to_string()),
        tool_name: Some("Vibe".to_string()),
        // Explicit: this fixture represents an ordinary headless-capable
        // platform, which is what `CliConfig`'s hand-written `Default`
        // (and the serde default) give — pinned here so the label test
        // cannot pass by accident if that default ever changes.
        headless: true,
        ..Default::default()
    };
    assert_eq!(super::doctor_cli_label(&cli), "Mistral AI · Vibe [mistral]");
    cli.provider = None;
    cli.tool_name = None;
    assert_eq!(cli.display_name(), "mistral");
    assert_eq!(super::doctor_cli_label(&cli), "mistral");
}

/// CM35: doctor marks a `headless = false` platform `(interactive only)`
/// without changing its display/slug rendering. Fails if the marker is
/// dropped.
#[test]
fn doctor_cli_label_marks_interactive_only_platform() {
    let cli = crate::domain::cli_config::CliConfig {
        name: "freebuff".to_string(),
        provider: Some("Codebuff".to_string()),
        tool_name: Some("Freebuff".to_string()),
        headless: false,
        ..Default::default()
    };
    assert_eq!(
        super::doctor_cli_label(&cli),
        "Codebuff · Freebuff [freebuff] (interactive only)"
    );
}

#[test]
fn binary_is_executable_true_for_executable_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake-canopy");
    std::fs::write(&path, "#!/bin/sh\necho hi\n").unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    assert!(binary_is_executable(&path));
}

#[test]
fn binary_is_executable_false_for_non_executable_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake-canopy");
    std::fs::write(&path, "not executable").unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    assert!(!binary_is_executable(&path));
}

#[test]
fn binary_is_executable_false_for_missing_file() {
    assert!(!binary_is_executable(Path::new(
        "/does/not/exist/fake-canopy"
    )));
}

#[test]
fn binary_version_returns_trimmed_stdout_on_success() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake-canopy");
    std::fs::write(&path, "#!/bin/sh\necho 'canopy 1.2.3'\n").unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    assert_eq!(binary_version(&path), Some("canopy 1.2.3".to_string()));
}

#[test]
fn binary_version_none_when_binary_missing() {
    assert_eq!(
        binary_version(Path::new("/does/not/exist/fake-canopy")),
        None
    );
}

#[test]
fn binary_version_none_on_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake-canopy");
    std::fs::write(&path, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    assert_eq!(binary_version(&path), None);
}

#[test]
fn doctor_reports_wrong_binary_with_resolved_path_when_identity_check_fails() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_identity_script(
        &dir,
        "blackbox",
        "echo \"blackbox: another window manager is already running on display ':0'\"\n",
    );
    let cli = identity_cli_config(&script, Some("--version"), Some("Blackbox CLI"));
    let wb = diagnose_cli_identity(&cli, &script)
        .expect("window-manager output must fail the identity check");
    assert_eq!(wb.resolved, script);
    let issue = wb.report(&cli.binary, "--version");
    assert!(
        issue.contains(&script.to_string_lossy().to_string()),
        "issue must name the resolved absolute path: {issue}"
    );
    assert!(issue.contains("does not identify as the blackbox CLI"));
    assert!(issue.contains("another window manager"));
}

#[test]
fn doctor_does_not_report_wrong_binary_when_check_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_identity_script(
        &dir,
        "blackbox",
        "echo 'another window manager is already running'\n",
    );
    let cli = identity_cli_config(&script, None, None);
    assert!(diagnose_cli_identity(&cli, &script).is_none());
}

#[test]
fn doctor_does_not_report_wrong_binary_when_identity_check_passes() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_identity_script(&dir, "real-bb", "echo 'Blackbox CLI v1.2.3'\n");
    let cli = identity_cli_config(&script, Some("--version"), Some("Blackbox"));
    assert!(diagnose_cli_identity(&cli, &script).is_none());
}

#[test]
fn doctor_reports_not_installed_when_identity_args_pattern_fails() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_identity_script(
            &dir,
            "blackbox",
            "if [ \"$1\" = \"--version\" ]; then echo \"blackbox: another window manager is already running on display ':0'\"; else echo \"$1\"; fi\n",
        );
    let cli =
        binary_identity_cli_config(&script, Some(vec!["--version"]), Some("(?i)blackbox ?ai"));
    let err = diagnose_binary_identity(&cli, &script)
        .expect("a mismatch must be reported as not-installed");
    let message = err.message();
    assert!(
        message.contains(&script.to_string_lossy().to_string()),
        "must name the resolved path: {message}"
    );
    assert!(
        message.contains("is not blackbox"),
        "must say the binary is not the platform: {message}"
    );
    assert!(
        message.contains("identity check failed: blackbox: another window manager"),
        "{message}"
    );
}

#[test]
fn doctor_reports_nothing_when_identity_args_pattern_passes() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_identity_script(&dir, "real-bb", "echo 'blackbox ai 1.2.3'\n");
    let cli =
        binary_identity_cli_config(&script, Some(vec!["--version"]), Some("(?i)blackbox ?ai"));
    assert!(diagnose_binary_identity(&cli, &script).is_none());
}

#[test]
fn doctor_reports_nothing_when_identity_fields_absent() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_identity_script(&dir, "plain-cli", "echo \"$1\"\n");
    let cli = binary_identity_cli_config(&script, None, None);
    assert!(diagnose_binary_identity(&cli, &script).is_none());
}

#[test]
fn doctor_respects_absolute_path_override() {
    // Two binaries share the bare name: the failing one would win a
    // PATH search, but the config points at the absolute path of the
    // real CLI — doctor must check exactly that path and pass.
    let dir = tempfile::tempdir().unwrap();
    let failing = write_identity_script(&dir, "bb-failing", "echo 'another window manager'\n");
    let passing = write_identity_script(&dir, "bb-passing", "echo 'Blackbox CLI'\n");
    let cli = identity_cli_config(&passing, Some("--version"), Some("Blackbox"));
    assert_eq!(
        PathBuf::from(cli.binary.clone()),
        passing,
        "the override must be the absolute path, not a bare name"
    );
    assert!(diagnose_cli_identity(&cli, &passing).is_none());
    let failing_cli = identity_cli_config(&failing, Some("--version"), Some("Blackbox CLI"));
    assert!(diagnose_cli_identity(&failing_cli, &failing).is_some());
}

#[test]
fn service_unit_location_returns_current_platform_manager() {
    let home = tempfile::tempdir().unwrap();
    let location = service_unit_location(home.path());
    if cfg!(target_os = "linux") {
        let (manager, path) = location.expect("linux always has a supported manager");
        assert_eq!(manager, "systemd");
        assert!(path.ends_with(".config/systemd/user/canopy.service"));
    } else if cfg!(target_os = "macos") {
        let (manager, path) = location.expect("macos always has a supported manager");
        assert_eq!(manager, "launchd");
        assert!(path.ends_with("Library/LaunchAgents/com.canopy.plist"));
    }
}

#[test]
fn report_layout_split_silent_when_fully_migrated() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("usage.toml"), "[counts]\n").unwrap();
    let mut issues = Vec::new();
    report_layout_split(dir.path(), None, &mut issues);
    assert!(issues.is_empty());
}

#[test]
fn report_layout_split_flags_usage_split_and_defers_with_live_daemon() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("usage.json"), r#"{"counts":{}}"#).unwrap();
    std::fs::write(dir.path().join("usage.toml"), "[counts]\n").unwrap();
    let mut issues = Vec::new();
    report_layout_split(dir.path(), Some(4242), &mut issues);
    assert_eq!(issues.len(), 1);
    assert!(issues[0].contains("deferred"));
}

#[test]
fn report_layout_split_flags_cache_split_without_a_running_daemon() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("cache")).unwrap();
    std::fs::write(
        dir.path().join("cache").join("models_native_claude.json"),
        "{}",
    )
    .unwrap();
    std::fs::write(dir.path().join("models_native_claude.json"), "{}").unwrap();
    let mut issues = Vec::new();
    report_layout_split(dir.path(), None, &mut issues);
    assert_eq!(issues.len(), 1);
    assert!(
        issues[0].contains("no daemon running"),
        "issue was: {}",
        issues[0]
    );
}

/// CB52 FR5: `report_orphan_joins` pushes the CB52 issue for a database
/// holding a join node with no ensemble row, and stays silent when there
/// is no database yet or nothing is orphaned. Asserts on `issues` (not
/// captured stdout) so it runs in CI, unlike the `#[ignore]`d black-box
/// `run_doctor` tests.
#[test]
fn report_orphan_joins_flags_orphans_and_stays_silent_when_healthy() {
    use crate::domain::graphs::{
        Graph, GraphNode, GraphNodeKind, GraphSpec, GraphSpecStatus, GraphStatus,
    };

    let dir = tempfile::tempdir().unwrap();
    let canopy_dir = dir.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    // No database yet: silent, no issue.
    let mut issues = Vec::new();
    report_orphan_joins(&canopy_dir, &mut issues);
    assert!(issues.is_empty());

    let db = Database::new(&database_path(&canopy_dir)).unwrap();
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "g1".to_string(),
        name: "Graph".to_string(),
        description: None,
        workdir: dir.path().to_string_lossy().to_string(),
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
        require_spec_type: false,
    })
    .unwrap();
    db.insert_graph_spec(&GraphSpec {
        id: "s1".to_string(),
        graph_id: Some("g1".to_string()),
        name: "Spec".to_string(),
        description: Some("desc".to_string()),
        position: 1,
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
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    })
    .unwrap();

    // Healthy database, no orphans: silent.
    let mut issues = Vec::new();
    report_orphan_joins(&canopy_dir, &mut issues);
    assert!(issues.is_empty());

    // Simulated pre-CB52 damage: a join row with no ensemble row.
    db.insert_graph_node(&GraphNode {
        id: "orphan-join".to_string(),
        spec_id: Some("s1".to_string()),
        graph_id: None,
        name: "orphaned quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({"ensemble_id": "gone"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    drop(db);

    let mut issues = Vec::new();
    report_orphan_joins(&canopy_dir, &mut issues);
    assert_eq!(issues.len(), 1);
    assert!(
        issues[0].contains("Orphaned join nodes"),
        "issue was: {}",
        issues[0]
    );
}

/// A totally fresh `$HOME` — nothing configured. Walks nearly every
/// "not found" / "not configured" branch in one pass and confirms the
/// closing summary lists remediation suggestions rather than the
/// all-clear banner.
// Note: Stdout capture via fd redirection is unreliable in CI environments
// where output is captured at a higher level. These tests work locally but
// fail in CI. Marked as ignored until a more robust capture mechanism is found.
#[tokio::test]
#[ignore]
async fn run_doctor_reports_every_gap_on_a_fresh_home() {
    let home = tempfile::tempdir().unwrap();
    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok(), "run_doctor must not error on a bare home");
    assert!(output.contains("Data directory not found"));
    assert!(output.contains("Database not found"));
    assert!(output.contains("Config not found"));
    assert!(output.contains("Daemon not running"));
    assert!(output.contains("Setup not completed"));
    assert!(output.contains("No harnesses configured"));
    assert!(output.contains("Embeddings model not configured"));
    assert!(output.contains("No personal RAG directories configured"));
    assert!(output.contains("ragignore not found"));
    assert!(output.contains("Vector store not yet created"));
    assert!(output.contains("Suggestions:"));
    assert!(!output.contains("All checks passed"));
}

/// A fully configured, fully healthy `$HOME`: existing data dir and DB
/// with an agent, a `config.toml` marked configured with one CLI that
/// resolves via an absolute path, a live daemon PID (the test process's
/// own pid — guaranteed running), a cloud embeddings model with its API
/// key exported, a RAG directory whose single indexable file is already
/// reflected 1:1 in the vector store, and a pre-existing (empty at
/// doctor-time) LanceDB directory. This is built to land on the
/// zero-issues "All checks passed!" branch.
///
/// Deliberately uses a cloud provider rather than a local model: doctor's
/// local-embeddings branch is capability-gated on the `local-embeddings`
/// feature (see the `run_doctor_reports_local_embeddings_*` tests below),
/// so a fixture asserting zero issues must not depend on that optional
/// feature being compiled in.
#[tokio::test]
#[ignore]
async fn run_doctor_reports_all_clear_on_a_healthy_home() {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    // DB with one agent.
    let db_path = canopy_dir.join("background_agents.db");
    let db = Database::new(&db_path).unwrap();
    db.upsert_agent(&sample_agent("agent-1")).unwrap();

    // RAG source directory with exactly one indexable file.
    let rag_dir = home.path().join("docs");
    std::fs::create_dir_all(&rag_dir).unwrap();
    std::fs::write(rag_dir.join("notes.md"), "# hello\nworld").unwrap();

    // Config: configured, one resolvable CLI, cloud embeddings model,
    // the RAG dir above, similarity threshold untouched.
    let config = CanopyConfig {
        configured_at: Some(chrono::Utc::now().to_rfc3339()),
        clis: vec![CliConfig {
            name: "echo-cli".to_string(),
            binary: "/bin/echo".to_string(),
            ..Default::default()
        }],
        embeddings_model: "text-embedding-3-small".to_string(),
        rag_personal_dirs: vec![rag_dir.to_string_lossy().to_string()],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();

    // ragignore present (optional but exercises the ✓ branch).
    std::fs::write(canopy_dir.join("ragignore"), "*.lock\n").unwrap();

    // Live PID: this test process is, definitionally, alive.
    std::fs::write(
        canopy_dir.join("daemon.pid"),
        std::process::id().to_string(),
    )
    .unwrap();

    // Pre-create the LanceDB dir + one chunk matching the single disk
    // file, so unique_paths == disk_files (no mismatch warning) and
    // the vector store already "exists" when doctor checks for it.
    // 1536 dims matches text-embedding-3-small.
    let lancedb_path = canopy_dir.join("rag").join("vectors.lancedb");
    let store = VectorStore::open_at(&lancedb_path, 1536, None)
        .await
        .unwrap();
    store
        .insert_chunk(&VectorChunk {
            id: "chunk-1".to_string(),
            file_path: rag_dir.join("notes.md").to_string_lossy().to_string(),
            content: "hello world".to_string(),
            embedding: vec![0.1f32; 1536],
            created_at: 1_715_000_000,
        })
        .await
        .unwrap();
    drop(store);

    let prev_key = std::env::var("OPENAI_API_KEY").ok();
    unsafe { std::env::set_var("OPENAI_API_KEY", "test-key") };

    let (result, output) = run_doctor_captured(home.path()).await;

    match prev_key {
        Some(v) => unsafe { std::env::set_var("OPENAI_API_KEY", v) },
        None => unsafe { std::env::remove_var("OPENAI_API_KEY") },
    }

    assert!(result.is_ok());
    assert!(output.contains("Data directory:"));
    assert!(output.contains("Agents: 1 (cron: 1, watch: 0)"));
    assert!(output.contains("Config: config.toml"));
    assert!(output.contains("Harnesses: echo-cli"));
    assert!(output.contains("Daemon running (PID:"));
    assert!(output.contains("Setup completed"));
    assert!(output.contains("echo-cli →"));
    assert!(output.contains("via absolute path"));
    assert!(output.contains("Embeddings model: text-embedding-3-small"));
    assert!(output.contains("API key OPENAI_API_KEY is set"));
    assert!(output.contains("RAG dir:"));
    assert!(output.contains("1 indexable file(s)"));
    // CB20: the size-exclusion count is stated even when it is zero, so a
    // clean corpus is a positive fact rather than an unexamined silence.
    assert!(
        output.contains("Size exclusions: 0 file(s) exceed the 10 MB"),
        "healthy home must still report a zero exclusion count:\n{output}"
    );
    assert!(output.contains("ragignore:"));
    assert!(output.contains("Vector store:"));
    assert!(output.contains("Indexed chunks: 1"));
    assert!(output.contains("Indexed files: 1"));
    assert!(
        !output.contains("indexable file(s) on disk but only"),
        "1:1 file mapping must not trigger the mismatch warning:\n{output}"
    );
    assert!(
        output.contains("All checks passed!"),
        "expected the all-clear banner, got:\n{output}"
    );
}

/// A degraded `$HOME`: legacy (pre-`config.toml`) marker files, a CLI
/// binary that can't be resolved, a stale daemon PID, an OpenAI
/// embeddings model with no API key exported, a configured RAG
/// directory that's missing on disk, and an oversize file that exceeds
/// the (default, since no config.toml is saved here) indexing limit.
/// Exercises the error/warning branches the healthy and fresh fixtures
/// above don't reach.
#[tokio::test]
#[ignore]
async fn run_doctor_reports_degraded_state_details() {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    // Legacy marker file, no config.toml → "legacy config found".
    std::fs::write(canopy_dir.join("cli_config.json"), "{}").unwrap();

    // Stale PID: astronomically unlikely to be a live process.
    std::fs::write(canopy_dir.join("daemon.pid"), "999999999").unwrap();

    // A RAG dir that's configured but missing, plus one that exists
    // and holds a file over the (default, since no config.toml is
    // saved here) indexing limit.
    let present_dir = home.path().join("present-docs");
    std::fs::create_dir_all(&present_dir).unwrap();
    let max_bytes = crate::domain::canopy_config::CanopyConfig::default().rag_max_file_bytes();
    let big = vec![b'a'; (max_bytes as usize) + 1];
    std::fs::write(present_dir.join("huge.md"), &big).unwrap();

    let config = CanopyConfig {
        configured_at: None, // config.toml won't even be written below —
        // is_configured() reads whatever CanopyConfig::load() sees, and
        // we intentionally never call config.save_for_tests() so the legacy-file
        // branch (not the config.toml branch) is what fires.
        clis: vec![CliConfig {
            name: "ghost-cli".to_string(),
            binary: "definitely-not-a-real-binary-xyz".to_string(),
            ..Default::default()
        }],
        embeddings_model: "text-embedding-3-small".to_string(),
        rag_personal_dirs: vec![
            home.path()
                .join("missing-docs")
                .to_string_lossy()
                .to_string(),
            present_dir.to_string_lossy().to_string(),
        ],
        ..Default::default()
    };
    // Doctor reads config via CanopyConfig::load(&canopy_dir), which
    // reads config.toml if present. We need `clis`/`embeddings_model`/
    // `rag_personal_dirs` to be seen while still hitting the
    // "legacy config" (not "configured") message, so config.toml IS
    // saved but without configured_at — is_configured() only checks
    // `configured_at.is_some()`.
    config.save_for_tests(&canopy_dir).unwrap();

    let prev_key = std::env::var("OPENAI_API_KEY").ok();
    unsafe { std::env::remove_var("OPENAI_API_KEY") };

    let (result, output) = run_doctor_captured(home.path()).await;

    if let Some(v) = prev_key {
        unsafe { std::env::set_var("OPENAI_API_KEY", v) };
    }

    assert!(result.is_ok());
    assert!(output.contains("Legacy config files found"));
    assert!(output.contains("Daemon not running (stale PID: 999999999)"));
    assert!(output.contains("ghost-cli — not found"));
    assert!(output.contains("'ghost-cli' binary 'definitely-not-a-real-binary-xyz' not found"));
    assert!(output.contains("Embeddings model: text-embedding-3-small"));
    assert!(output.contains("OPENAI_API_KEY is NOT set"));
    assert!(output.contains("RAG dir missing:"));
    assert!(output.contains("RAG dir:"));
    assert!(output.contains("configured file(s) exceed the 10 MB"));
    // CB20: the exclusion count and the effective limit are visible in the
    // report body, not only in the remediation suggestions.
    assert!(
        output.contains("Size exclusions: 1 file(s) exceed the 10 MB"),
        "degraded home must surface the size-exclusion count in the report body:\n{output}"
    );
    assert!(output.contains("Suggestions:"));
}

/// An embeddings model string that doesn't match any known provider —
/// the "Model '...' is not supported" branch, distinct from both the
/// empty-model and known-provider-missing-key cases above.
#[tokio::test]
#[ignore]
async fn run_doctor_reports_unsupported_embeddings_model() {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    let config = CanopyConfig {
        embeddings_model: "some-unknown-model-9000".to_string(),
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();

    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok());
    assert!(output.contains("Model 'some-unknown-model-9000' is not supported"));
    assert!(output.contains("select a supported embedding model"));
}

/// The dialog symptom from the usage-stats/model-cache layout migration
/// spec is only observable from outside as a lingering legacy/new
/// split — doctor must name it explicitly rather than leave an operator
/// unable to explain it.
#[tokio::test]
#[ignore]
async fn run_doctor_reports_legacy_new_layout_split() {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    // Both usage files present, as left behind by a deferred migration.
    std::fs::write(canopy_dir.join("usage.json"), r#"{"counts":{"kiro":2}}"#).unwrap();
    std::fs::write(canopy_dir.join("usage.toml"), "[counts]\nkiro = 1\n").unwrap();
    // Same split for the model catalog cache.
    std::fs::create_dir_all(canopy_dir.join("cache")).unwrap();
    std::fs::write(
        canopy_dir.join("cache").join("models_catalog.json"),
        r#"{"fresh":true}"#,
    )
    .unwrap();
    std::fs::write(canopy_dir.join("models_cache.json"), r#"{"stale":true}"#).unwrap();

    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok());
    assert!(
        output.contains("Legacy/new data layout split detected"),
        "doctor output missing layout-split line:\n{output}"
    );
    assert!(output.contains("usage.json"));
    assert!(output.contains("models_cache.json"));
}

/// No split, no daemon: doctor must stay quiet about layout migration —
/// this is the common, already-migrated case and must not cost a line
/// of noise or a false "unexplainable" issue.
#[tokio::test]
#[ignore]
async fn run_doctor_says_nothing_about_layout_split_when_fully_migrated() {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    std::fs::write(canopy_dir.join("usage.toml"), "[counts]\nkiro = 1\n").unwrap();

    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok());
    assert!(!output.contains("Legacy/new data layout split"));
}

/// A local embeddings model configured on a binary built WITHOUT the
/// 'local-embeddings' feature — the CB19 defect's doctor side: the old
/// code printed a green "no API key required" line by reading
/// configuration only. Doctor must now check capability and report red
/// with the reason, plus an actionable issue naming the cloud-provider
/// alternative the setup wizard offers.
#[tokio::test]
#[ignore]
#[cfg(not(feature = "local-embeddings"))]
async fn run_doctor_reports_local_embeddings_unavailable_without_feature() {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    let config = CanopyConfig {
        embeddings_model: "baai/bge-small-en-v1.5".to_string(),
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();

    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok());
    assert!(output.contains("Embeddings model: baai/bge-small-en-v1.5"));
    assert!(
        !output.contains("Local model — no API key required"),
        "must not claim a capability this build does not have:\n{output}"
    );
    assert!(
        output.contains("not available in this build"),
        "must name the capability gap, got:\n{output}"
    );
    assert!(output.contains("requires a build with 'local-embeddings'"));
    assert!(output.contains("choose a cloud provider"));
}

/// The same configuration on a binary built WITH the 'local-embeddings'
/// feature: doctor should report the capability as available.
#[tokio::test]
#[ignore]
#[cfg(feature = "local-embeddings")]
async fn run_doctor_reports_local_embeddings_available_with_feature() {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    let config = CanopyConfig {
        embeddings_model: "baai/bge-small-en-v1.5".to_string(),
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();

    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok());
    assert!(output.contains("Embeddings model: baai/bge-small-en-v1.5"));
    assert!(output.contains("Local model — no API key required"));
    assert!(!output.contains("Local embeddings unavailable"));
}

/// A local model still downloading must not read as "no API key
/// required" (green) or "unavailable" (capability gap) — it's a third,
/// distinct, honest state.
#[tokio::test]
#[ignore]
#[cfg(feature = "local-embeddings")]
async fn run_doctor_reports_downloading_state_for_local_model() {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    let config = CanopyConfig {
        embeddings_model: "baai/bge-small-en-v1.5".to_string(),
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();

    let db = Database::new(&canopy_dir.join("background_agents.db")).unwrap();
    crate::rag::status::mark_downloading(&db, "baai/bge-small-en-v1.5");
    drop(db);

    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok());
    assert!(output.contains("Local model downloading"));
    assert!(
        !output.contains("Local model — no API key required"),
        "must not claim ready while still downloading:\n{output}"
    );
}

/// A failed download must surface as red with the reason, plus an
/// actionable issue naming the retry command.
#[tokio::test]
#[ignore]
#[cfg(feature = "local-embeddings")]
async fn run_doctor_reports_failed_download_with_reason_and_retry_hint() {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    let config = CanopyConfig {
        embeddings_model: "baai/bge-small-en-v1.5".to_string(),
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();

    let db = Database::new(&canopy_dir.join("background_agents.db")).unwrap();
    crate::rag::status::mark_failed(&db, "baai/bge-small-en-v1.5", "connection reset");
    drop(db);

    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok());
    assert!(output.contains("Local model download failed"));
    assert!(output.contains("connection reset"));
    assert!(output.contains("canopy rag model retry"));
}

/// CB19: "no embedding provider configured" must read differently from
/// "the configured provider can't run on this build". An empty model
/// string reports "not configured" and must NOT mention build
/// availability — there is nothing configured whose availability could
/// be judged.
#[tokio::test]
#[ignore]
async fn run_doctor_reports_no_provider_configured_separately_from_unavailable() {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    let config = CanopyConfig {
        embeddings_model: String::new(),
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();

    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok());
    assert!(
        output.contains("Embeddings model not configured"),
        "empty model must report 'not configured', got:\n{output}"
    );
    assert!(
        !output.contains("not available in this build"),
        "nothing is configured, so availability must not be mentioned:\n{output}"
    );
}

/// CB19: a configured provider this binary cannot run (local model on
/// a build without `local-embeddings`) reports "not available in this
/// build" with a remediation naming the setup wizard's cloud-provider
/// alternative — never the bare "not configured" line.
#[tokio::test]
#[ignore]
#[cfg(not(feature = "local-embeddings"))]
async fn run_doctor_reports_configured_provider_not_available_in_build() {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    let config = CanopyConfig {
        embeddings_model: "baai/bge-small-en-v1.5".to_string(),
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();

    let (result, output) = run_doctor_captured(home.path()).await;

    assert!(result.is_ok());
    assert!(output.contains("Embeddings model: baai/bge-small-en-v1.5"));
    assert!(
        output.contains("not available in this build"),
        "must distinguish 'configured but unrunnable' from 'not configured', got:\n{output}"
    );
    assert!(
        !output.contains("Embeddings model not configured"),
        "a configured model must not read as unconfigured:\n{output}"
    );
}

/// CB19: a remote provider with no API key exported reports the missing
/// key — and must NOT report "not available in this build", since remote
/// providers are compiled into every binary.
#[tokio::test]
#[ignore]
async fn run_doctor_reports_remote_provider_without_key() {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    let config = CanopyConfig {
        embeddings_model: "text-embedding-3-small".to_string(),
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();

    let prev_key = std::env::var("OPENAI_API_KEY").ok();
    unsafe { std::env::remove_var("OPENAI_API_KEY") };

    let (result, output) = run_doctor_captured(home.path()).await;

    if let Some(v) = prev_key {
        unsafe { std::env::set_var("OPENAI_API_KEY", v) };
    }

    assert!(result.is_ok());
    assert!(output.contains("Embeddings model: text-embedding-3-small"));
    assert!(
        output.contains("OPENAI_API_KEY is NOT set"),
        "missing key must be reported, got:\n{output}"
    );
    assert!(
        !output.contains("not available in this build"),
        "remote providers are always available — only the key is missing:\n{output}"
    );
}

/// A LanceDB directory that exists on disk but is not a valid store
/// (here: a plain file sitting where the store's directory should be,
/// standing in for any on-disk corruption) must never read as healthy.
/// This is the regression test for the defect this module fixes: the
/// old check was `lancedb_path.exists()`, which is true for a corrupt
/// store exactly as it is for a working one.
#[tokio::test]
#[ignore]
async fn run_doctor_reports_corrupt_vector_store_without_a_green_tick() {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    let config = CanopyConfig {
        embeddings_model: "text-embedding-3-small".to_string(),
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();

    // Put a plain file where VectorStore::open_at's `create_dir_all`
    // needs to make a directory — opening it must fail, the way a
    // corrupt LanceDB manifest would fail in the wild.
    let rag_dir = canopy_dir.join("rag");
    std::fs::create_dir_all(&rag_dir).unwrap();
    std::fs::write(rag_dir.join("vectors.lancedb"), b"not a lancedb store").unwrap();

    let prev_key = std::env::var("OPENAI_API_KEY").ok();
    unsafe { std::env::set_var("OPENAI_API_KEY", "test-key") };

    let (result, output) = run_doctor_captured(home.path()).await;

    match prev_key {
        Some(v) => unsafe { std::env::set_var("OPENAI_API_KEY", v) },
        None => unsafe { std::env::remove_var("OPENAI_API_KEY") },
    }

    assert!(result.is_ok());
    assert!(
        !output.contains("\x1b[32m✓\x1b[0m Vector store"),
        "a corrupt store must not print a green tick:\n{output}"
    );
    assert!(
        output.contains("Could not open LanceDB"),
        "expected the open failure to be surfaced:\n{output}"
    );
}
