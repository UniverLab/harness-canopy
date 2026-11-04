//! Part 01 of `tests` (split from `src/domain/cli_strategy.rs`, pure move).
use super::*;

#[test]
fn test_build_command_basic() {
    let strategy = sample_strategy();
    let cmd = strategy
        .build_command("test prompt", None, None, None)
        .unwrap();

    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("test-cli"));
}

#[test]
fn test_build_command_with_model() {
    let strategy = sample_strategy();
    let cmd = strategy
        .build_command("test prompt", Some("gpt-4"), None, None)
        .unwrap();

    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("--model"));
    assert!(cmd_str.contains("gpt-4"));
}

#[test]
fn test_build_command_with_working_dir() {
    let strategy = sample_strategy();
    let cmd = strategy
        .build_command("test prompt", None, Some("/tmp/project"), None)
        .unwrap();

    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("--workdir"));
    assert!(cmd_str.contains("/tmp/project"));
}

#[test]
fn test_build_command_no_working_dir_when_not_supported() {
    let mut strategy = sample_strategy();
    strategy.supports_working_dir = false;

    let cmd = strategy
        .build_command("test prompt", None, Some("/tmp/project"), None)
        .unwrap();

    let cmd_str = format!("{:?}", cmd);
    assert!(!cmd_str.contains("--workdir"));
}

#[test]
fn test_build_command_no_model_flag() {
    let mut strategy = sample_strategy();
    strategy.model_flag = None;

    let cmd = strategy
        .build_command("test prompt", Some("gpt-4"), None, None)
        .unwrap();

    let cmd_str = format!("{:?}", cmd);
    assert!(!cmd_str.contains("--model"));
}

/// CB34: a blank `model_flag` (antigravity's `model_flag = ""`) must not put
/// an empty argv word — or the bare model value — on the command line.
#[test]
fn build_command_blank_model_flag_omits_model_from_argv() {
    let mut strategy = sample_strategy();
    strategy.model_flag = Some(String::new());

    let cmd = strategy
        .build_command("test prompt", Some("gpt-4"), None, None)
        .unwrap();

    let args: Vec<String> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    assert!(
        !args.iter().any(|a| a.is_empty()),
        "blank model_flag must not render an empty argv word: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "gpt-4"),
        "the model value must be omitted, not passed bare: {args:?}"
    );
}

/// A real `model_flag` is unaffected: flag + value still land in argv, adjacent.
#[test]
fn build_command_real_model_flag_still_passes_model_in_argv() {
    let strategy = sample_strategy(); // model_flag = Some("--model")

    let cmd = strategy
        .build_command("test prompt", Some("gpt-4"), None, None)
        .unwrap();

    let args: Vec<String> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let i = args
        .iter()
        .position(|a| a == "--model")
        .expect("--model must be present");
    assert_eq!(args.get(i + 1).map(String::as_str), Some("gpt-4"));
}

#[test]
fn test_build_command_empty_headless_mode() {
    let mut strategy = sample_strategy();
    strategy.headless_mode = String::new();

    let cmd = strategy
        .build_command("test prompt", None, None, None)
        .unwrap();

    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("test-cli"));
}

#[test]
fn test_with_stdin_forced_overrides_flag() {
    let mut strategy = sample_strategy();
    strategy.prompt_via_stdin = false;
    let forced = strategy.with_stdin_forced();
    assert!(
        forced.prompt_via_stdin,
        "with_stdin_forced must set prompt_via_stdin to true"
    );
    assert!(!strategy.prompt_via_stdin, "original must be unchanged");
    assert_eq!(
        strategy.binary, forced.binary,
        "all other fields must be preserved"
    );
}

#[test]
fn test_build_command_prompt_via_stdin_keeps_prompt_out_of_argv() {
    let mut strategy = sample_strategy();
    strategy.prompt_via_stdin = true;

    let cmd = strategy
        .build_command("this must not appear in argv", None, None, None)
        .unwrap();

    let cmd_str = format!("{:?}", cmd);
    assert!(!cmd_str.contains("this must not appear in argv"));
}

#[tokio::test]
async fn test_build_command_prompt_via_stdin_delivers_huge_prompt() {
    // A multi-hundred-KB prompt would blow argv (Linux MAX_ARG_STRLEN is
    // 128KiB) if passed via `cmd.arg`. Piped via stdin it has no
    // input-size cliff: spawn `cat`, which just echoes stdin to stdout.
    let mut strategy = sample_strategy();
    strategy.binary = "/bin/cat".to_string();
    strategy.headless_mode = String::new();
    strategy.model_flag = None;
    strategy.supports_working_dir = false;
    strategy.prompt_via_stdin = true;

    let huge_prompt = "x".repeat(500 * 1024);
    let mut cmd = strategy
        .build_command(&huge_prompt, None, None, None)
        .unwrap();
    cmd.stdout(std::process::Stdio::piped());

    let output = cmd.output().await.unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), huge_prompt);
}

#[test]
fn build_command_with_session_injects_set_flag_and_id() {
    let mut strategy = sample_strategy();
    strategy.session_id_set_flag = Some("--session-id".to_string());
    let cmd = strategy
        .build_command_with_session(
            "p",
            None,
            None,
            Some("11111111-2222-3333-4444-555555555555"),
            None,
            None,
        )
        .unwrap();
    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("--session-id"));
    assert!(cmd_str.contains("11111111-2222-3333-4444-555555555555"));
}

#[test]
fn build_command_with_session_without_flag_drops_id() {
    // sample_strategy has no session_id_set_flag: the id must be
    // silently dropped, never passed as a stray argument.
    let strategy = sample_strategy();
    let cmd = strategy
        .build_command_with_session("p", None, None, Some("sid-123"), None, None)
        .unwrap();
    let cmd_str = format!("{:?}", cmd);
    assert!(!cmd_str.contains("sid-123"));
}

#[test]
fn build_command_never_injects_session_flag_without_id() {
    let mut strategy = sample_strategy();
    strategy.session_id_set_flag = Some("--session-id".to_string());
    let cmd = strategy.build_command("p", None, None, None).unwrap();
    let cmd_str = format!("{:?}", cmd);
    assert!(!cmd_str.contains("--session-id"));
}

#[test]
fn build_command_adds_title_flag_when_configured() {
    let mut s = sample_strategy();
    s.session_title_flag = Some("--title".to_string());
    let cmd = s
        .build_command("prompt", None, None, Some("run-123"))
        .unwrap();
    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("--title"));
    assert!(cmd_str.contains("canopy-run-123"));
}

#[test]
fn build_command_omits_title_flag_when_not_configured() {
    let s = sample_strategy(); // session_title_flag = None
    let cmd = s
        .build_command("prompt", None, None, Some("run-123"))
        .unwrap();
    let cmd_str = format!("{:?}", cmd);
    assert!(!cmd_str.contains("--title"));
    assert!(!cmd_str.contains("canopy-run-123"));
}

#[test]
fn build_command_omits_title_flag_when_run_id_none() {
    let mut s = sample_strategy();
    s.session_title_flag = Some("--title".to_string());
    let cmd = s.build_command("prompt", None, None, None).unwrap();
    let cmd_str = format!("{:?}", cmd);
    assert!(!cmd_str.contains("--title"));
}

#[test]
fn build_command_with_session_adds_title_flag_when_configured() {
    let mut s = sample_strategy();
    s.session_title_flag = Some("--title".to_string());
    let cmd = s
        .build_command_with_session("prompt", None, None, None, None, Some("run-456"))
        .unwrap();
    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("--title"));
    assert!(cmd_str.contains("canopy-run-456"));
}

#[test]
fn build_session_end_command_substitutes_session_id() {
    let mut s = sample_strategy();
    s.session_end_args = Some(vec![
        "api".to_string(),
        "session.interrupt".to_string(),
        "--param".to_string(),
        "sessionID={session_id}".to_string(),
    ]);
    let cmd = s.build_session_end_command("ses_abc", "/tmp").unwrap();
    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("sessionID=ses_abc"));
    assert!(!cmd_str.contains("{session_id}"));
}

#[test]
fn build_session_end_command_none_without_config() {
    let s = sample_strategy();
    assert!(s.build_session_end_command("ses_abc", "/tmp").is_none());
}

#[test]
fn build_session_list_command_none_without_config() {
    let s = sample_strategy();
    assert!(s.build_session_list_command_by_title("/tmp").is_none());
}

#[test]
fn test_build_command_all_options() {
    let strategy = sample_strategy();
    let cmd = strategy
        .build_command("my prompt", Some("claude-3"), Some("/home/project"), None)
        .unwrap();

    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("my prompt"));
    assert!(cmd_str.contains("--model"));
    assert!(cmd_str.contains("claude-3"));
    assert!(cmd_str.contains("--workdir"));
    assert!(cmd_str.contains("/home/project"));
}

#[test]
fn can_capture_session_after_run_requires_list_and_pattern_without_set_flag() {
    let mut s = sample_strategy();
    assert!(!s.can_capture_session_after_run(), "nothing configured");

    s.session_list_cmd = Some("session list".to_string());
    assert!(!s.can_capture_session_after_run(), "pattern still missing");

    s.session_id_pattern = Some("\"id\"\\s*:\\s*\"([^\"]+)\"".to_string());
    assert!(s.can_capture_session_after_run(), "list + pattern present");

    // Set-at-spawn takes precedence and disables list-after-run capture.
    s.session_id_set_flag = Some("--session-id".to_string());
    assert!(!s.can_capture_session_after_run(), "set-at-spawn wins");
}

#[test]
fn extract_session_ids_pulls_id_key_from_opencode_family_json() {
    let mut s = sample_strategy();
    s.session_id_pattern = Some("\"id\"\\s*:\\s*\"([^\"]+)\"".to_string());
    // Real opencode/mimo/kilo shape: a JSON array whose objects also carry
    // a `projectId` (which must NOT be mistaken for `id`).
    let output = r#"[
          {"id": "ses_AAA", "projectId": "hexhexhex", "directory": "/x"},
          {"id": "ses_BBB", "projectId": "hexhexhex", "directory": "/y"}
        ]"#;
    let ids = s.extract_session_ids(output);
    assert_eq!(ids.len(), 2);
    assert!(ids.contains("ses_AAA"));
    assert!(ids.contains("ses_BBB"));
}

#[test]
fn extract_session_ids_pulls_id_key_from_cn_json() {
    let mut s = sample_strategy();
    s.session_id_pattern = Some("\"id\"\\s*:\\s*\"([^\"]+)\"".to_string());
    // Real cn shape: an object wrapping a `sessions` array.
    let output = r#"{"sessions": [
          {"id": "8ae15a84-fec0-43b4-9cb8-47293662302e", "title": "x"},
          {"id": "633286f6-0820-46a5-8c8f-ed315faa5e49", "title": "y"}
        ]}"#;
    let ids = s.extract_session_ids(output);
    assert_eq!(ids.len(), 2);
    assert!(ids.contains("8ae15a84-fec0-43b4-9cb8-47293662302e"));
}

#[test]
fn extract_session_ids_empty_without_pattern() {
    let s = sample_strategy();
    assert!(s.extract_session_ids(r#"[{"id":"ses_X"}]"#).is_empty());
}

#[test]
fn build_session_list_command_none_without_list_cmd() {
    let s = sample_strategy();
    assert!(s.build_session_list_command("/tmp").unwrap().is_none());
}

#[test]
fn build_session_list_command_appends_format_args_and_sets_cwd() {
    let mut s = sample_strategy();
    s.session_list_cmd = Some("session list".to_string());
    s.session_list_format_args = Some("--format json".to_string());
    let cmd = s
        .build_session_list_command("/tmp/project")
        .unwrap()
        .expect("list command must be built");
    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("session"));
    assert!(cmd_str.contains("list"));
    assert!(cmd_str.contains("--format"));
    assert!(cmd_str.contains("json"));
    // The prompt/headless/model flags must never appear on a list command.
    assert!(!cmd_str.contains("--headless"));
    assert!(!cmd_str.contains("--model"));
}

#[test]
fn supports_resume_by_id_follows_session_resume_cmd() {
    let mut s = sample_strategy();
    assert!(!s.supports_resume_by_id());
    s.session_resume_cmd = Some("--session".to_string());
    assert!(s.supports_resume_by_id());
}

#[test]
fn build_resume_command_injects_resume_flag_and_id_before_prompt() {
    let mut s = sample_strategy();
    s.session_resume_cmd = Some("--session".to_string());
    let cmd = s
        .build_resume_command("ses_abc", "the prompt", None, None, None)
        .unwrap();
    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("--session"));
    assert!(cmd_str.contains("ses_abc"));
    // The resume flag+id must precede the positional prompt.
    let flag_at = cmd_str.find("--session").unwrap();
    let prompt_at = cmd_str.find("the prompt").unwrap();
    assert!(
        flag_at < prompt_at,
        "resume flag+id must come before prompt"
    );
}

#[test]
fn build_resume_command_errors_without_session_resume_cmd() {
    let s = sample_strategy();
    let err = s
        .build_resume_command("ses_abc", "p", None, None, None)
        .unwrap_err();
    assert!(err.to_string().contains("session_resume_cmd"));
}

#[test]
fn build_resume_command_never_uses_set_at_spawn_flag() {
    // A platform can have BOTH a set-at-spawn flag and a resume flag;
    // resume must use the resume flag, not mint via the set-at-spawn one.
    let mut s = sample_strategy();
    s.session_id_set_flag = Some("--session-id".to_string());
    s.session_resume_cmd = Some("--resume".to_string());
    let cmd = s
        .build_resume_command("ses_xyz", "p", None, None, None)
        .unwrap();
    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("--resume"));
    assert!(cmd_str.contains("ses_xyz"));
    assert!(!cmd_str.contains("--session-id"));
}

#[test]
fn resolve_binary_absolute_path_used_as_is_without_touching_path() {
    // Deliberately a path that does not exist: absolute paths must be
    // returned verbatim, with no PATH lookup and no existence check.
    let resolved = resolve_binary_with_path("/nonexistent/somewhere/mimo", "").unwrap();
    assert_eq!(resolved, PathBuf::from("/nonexistent/somewhere/mimo"));
}

#[test]
fn daemon_path_at_none_when_unit_file_missing() {
    let home = tempfile::tempdir().unwrap();
    assert!(daemon_path_at(home.path()).is_none());
}

#[test]
fn daemon_path_at_none_when_unit_has_no_path_line() {
    let home = tempfile::tempdir().unwrap();
    let unit_dir = home.path().join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir).unwrap();
    std::fs::write(
        unit_dir.join("canopy.service"),
        "[Service]\nExecStart=/usr/local/bin/canopy daemon run\n",
    )
    .unwrap();
    assert!(daemon_path_at(home.path()).is_none());
}

#[test]
fn daemon_path_at_reads_environment_path_line() {
    let home = tempfile::tempdir().unwrap();
    let unit_dir = home.path().join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir).unwrap();
    std::fs::write(
            unit_dir.join("canopy.service"),
            "[Service]\nEnvironment=PATH=/usr/bin:/bin:/home/user/.opencode/bin\nExecStart=/usr/local/bin/canopy daemon run\n",
        )
        .unwrap();
    assert_eq!(
        daemon_path_at(home.path()),
        Some("/usr/bin:/bin:/home/user/.opencode/bin".to_string())
    );
}

#[test]
fn resolve_binary_not_found_names_binary_and_searched_path() {
    let err =
        resolve_binary_with_path("canopy-test-fixture-cli-missing", "/usr/bin:/bin").unwrap_err();
    let message = err.to_string();

    assert!(message.contains("canopy-test-fixture-cli-missing"));
    assert!(message.contains("/usr/bin:/bin"));
}

#[test]
fn resolution_step_label_absolute_path() {
    assert_eq!(ResolutionStep::AbsolutePath.label(), "absolute path");
}

#[test]
fn resolution_step_label_path() {
    assert_eq!(ResolutionStep::Path.label(), "PATH");
}

#[test]
fn resolution_step_equality() {
    assert_eq!(ResolutionStep::AbsolutePath, ResolutionStep::AbsolutePath);
    assert_eq!(ResolutionStep::Path, ResolutionStep::Path);
    assert_ne!(ResolutionStep::AbsolutePath, ResolutionStep::Path);
}

#[test]
fn extract_session_ids_invalid_regex_returns_empty() {
    let mut s = sample_strategy();
    s.session_id_pattern = Some("[invalid".to_string());
    let ids = s.extract_session_ids(r#"[{"id":"ses_X"}]"#);
    assert!(ids.is_empty());
}

#[test]
fn extract_session_ids_no_match_returns_empty() {
    let mut s = sample_strategy();
    s.session_id_pattern = Some(r#""id"\s*:\s*"([^"]+)""#.to_string());
    let ids = s.extract_session_ids("no ids here at all");
    assert!(ids.is_empty());
}

#[test]
fn extract_session_ids_uses_group_1_when_present() {
    let mut s = sample_strategy();
    s.session_id_pattern = Some(r#"session_(\w+)"#.to_string());
    let ids = s.extract_session_ids("session_abc session_xyz");
    assert_eq!(ids.len(), 2);
    assert!(ids.contains("abc"));
    assert!(ids.contains("xyz"));
}

#[test]
fn extract_session_ids_falls_back_to_full_match_without_group() {
    let mut s = sample_strategy();
    s.session_id_pattern = Some(r#""id"\s*:\s*"[^"]+""#.to_string());
    let ids = s.extract_session_ids(r#""id": "ses_full""#);
    assert_eq!(ids.len(), 1);
    // Without a capture group, the full match is used
    assert!(ids.contains(r#""id": "ses_full""#));
}

#[test]
fn with_stdin_forced_preserves_all_other_fields() {
    let mut strategy = sample_strategy();
    strategy.session_id_set_flag = Some("--sid".to_string());
    strategy.session_list_cmd = Some("ls".to_string());
    strategy.session_resume_cmd = Some("--resume".to_string());

    let forced = strategy.with_stdin_forced();

    assert!(forced.prompt_via_stdin);
    assert_eq!(forced.session_id_set_flag.as_deref(), Some("--sid"));
    assert_eq!(forced.session_list_cmd.as_deref(), Some("ls"));
    assert_eq!(forced.session_resume_cmd.as_deref(), Some("--resume"));
    assert_eq!(forced.binary, strategy.binary);
    assert_eq!(forced.headless_mode, strategy.headless_mode);
}

#[test]
fn binary_resolution_error_display() {
    let err = BinaryResolutionError {
        binary: "my-cli".to_string(),
        path: "/usr/bin:/bin".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("my-cli"));
    assert!(msg.contains("/usr/bin:/bin"));
}

#[test]
fn template_prompt_positional_at_end() {
    // Case 1: prompt as positional at end (majority of current platforms).
    let mut s = sample_strategy();
    s.invocation_template =
        Some("--session-id {{session_id}} {{prompt}} --model {{model}}".to_string());
    s.session_id_set_flag = Some("--session-id".to_string());

    let cmd = s
        .build_command_with_session(
            "the prompt",
            Some("gpt-4"),
            None,
            Some("ses_123"),
            None,
            None,
        )
        .unwrap();
    let cmd_str = format!("{:?}", cmd);

    // Assert order: headless flags, then session-id, then prompt, then model.
    let headless_at = cmd_str.find("--headless").unwrap();
    let session_at = cmd_str.find("--session-id").unwrap();
    let prompt_at = cmd_str.find("the prompt").unwrap();
    let model_at = cmd_str.find("--model").unwrap();
    assert!(headless_at < session_at);
    assert!(session_at < prompt_at);
    assert!(prompt_at < model_at);
}

#[test]
fn template_prompt_as_flag_value() {
    // Case 2: prompt as value of a flag (copilot -p). The flag that consumes
    // the next argv word must be immediately followed by the prompt, not by
    // --session-id (which would be misinterpreted as the prompt).
    let mut s = sample_strategy();
    s.invocation_template = Some("-p {{prompt}} --session-id {{session_id}}".to_string());
    s.session_id_set_flag = Some("--session-id".to_string());

    let cmd = s
        .build_command_with_session("the prompt", None, None, Some("ses_456"), None, None)
        .unwrap();
    let cmd_str = format!("{:?}", cmd);

    // Assert: -p immediately followed by prompt (no --session-id in between).
    let p_flag_at = cmd_str.find("-p").unwrap();
    let prompt_at = cmd_str.find("the prompt").unwrap();
    let session_at = cmd_str.find("--session-id").unwrap();
    assert!(p_flag_at < prompt_at);
    assert!(prompt_at < session_at);
    // Critical: --session-id must NOT appear between -p and the prompt.
    let between = &cmd_str[p_flag_at..prompt_at];
    assert!(!between.contains("--session-id"));
}

#[test]
fn template_flag_equals_value_form() {
    // Case 3: --flag=value in a single word.
    let mut s = sample_strategy();
    s.invocation_template = Some("--model={{model}} {{prompt}}".to_string());

    let cmd = s
        .build_command("the prompt", Some("gpt-4"), None, None)
        .unwrap();
    let cmd_str = format!("{:?}", cmd);

    // Assert: --model=gpt-4 is a single argv word (no space between flag and value).
    assert!(cmd_str.contains("--model=gpt-4"));
    // And it's distinct from --model gpt-4 (two words).
    assert!(!cmd_str.contains("--model \"gpt-4\""));
}

#[test]
fn template_composite_marker_in_value() {
    // Case 4: marker composed within another marker's value (cursor effort).
    let s = sample_strategy();
    let argv = s.build_argv_from_template(
        "{{model}}[context=1m,effort={{effort}},fast=false] {{prompt}}",
        "the prompt",
        TemplateArgs {
            model: Some("claude-opus-4-8"),
            working_dir: None,
            session_arg: None,
            effort: Some("high"),
            mcp_config: None,
        },
    );

    assert_eq!(argv.len(), 2);
    assert_eq!(
        argv[0],
        "claude-opus-4-8[context=1m,effort=high,fast=false]"
    );
    assert_eq!(argv[1], "the prompt");
}

#[test]
fn template_elides_unavailable_marker_and_companion_flag() {
    // If {{model}} is unavailable, --model is also elided (no orphan flag).
    let mut s = sample_strategy();
    s.invocation_template = Some("--model {{model}} {{prompt}}".to_string());

    let cmd = s.build_command("the prompt", None, None, None).unwrap();
    let cmd_str = format!("{:?}", cmd);

    assert!(cmd_str.contains("the prompt"));
    assert!(!cmd_str.contains("--model"));
}

#[test]
fn template_elides_flag_equals_value_when_marker_unavailable() {
    // If {{model}} is unavailable, --model={{model}} is elided as a unit.
    let mut s = sample_strategy();
    s.invocation_template = Some("--model={{model}} {{prompt}}".to_string());

    let cmd = s.build_command("the prompt", None, None, None).unwrap();
    let cmd_str = format!("{:?}", cmd);

    assert!(cmd_str.contains("the prompt"));
    assert!(!cmd_str.contains("--model"));
}

#[test]
fn template_elides_composite_when_any_marker_unavailable() {
    // If {{effort}} is unavailable, the whole composite token is elided.
    let s = sample_strategy();
    let argv = s.build_argv_from_template(
        "{{model}}[context=1m,effort={{effort}},fast=false] {{prompt}}",
        "the prompt",
        TemplateArgs {
            model: Some("claude-opus-4-8"),
            working_dir: None,
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );

    assert_eq!(argv.len(), 1);
    assert_eq!(argv[0], "the prompt");
}

#[test]
fn template_none_falls_back_to_legacy_assembly() {
    // Backward compat: no template → legacy fixed-order behavior.
    let s = sample_strategy(); // invocation_template is None
    let cmd = s
        .build_command("the prompt", Some("gpt-4"), Some("/tmp"), None)
        .unwrap();
    let cmd_str = format!("{:?}", cmd);

    // Legacy order: headless, prompt, model, workdir.
    assert!(cmd_str.contains("--headless"));
    assert!(cmd_str.contains("the prompt"));
    assert!(cmd_str.contains("--model"));
    assert!(cmd_str.contains("gpt-4"));
    assert!(cmd_str.contains("--workdir"));
    assert!(cmd_str.contains("/tmp"));
}

#[test]
fn template_resume_uses_correct_flag() {
    let mut s = sample_strategy();
    s.invocation_template = Some("{{session_flag}} {{session_id}} {{prompt}}".to_string());
    s.session_resume_cmd = Some("--resume".to_string());
    s.session_id_set_flag = Some("--session-id".to_string());

    let cmd = s
        .build_resume_command("ses_123", "the prompt", None, None, None)
        .unwrap();
    let cmd_str = format!("{:?}", cmd);

    assert!(cmd_str.contains("--resume"));
    assert!(cmd_str.contains("ses_123"));
    assert!(cmd_str.contains("the prompt"));
    // Critical: --session-id must NOT appear (resume uses --resume, not --session-id)
    let resume_at = cmd_str.find("--resume").unwrap();
    let ses_at = cmd_str.find("ses_123").unwrap();
    let between = &cmd_str[resume_at..ses_at];
    assert!(!between.contains("--session-id"));
}

#[test]
fn template_stdin_forced_elides_prompt_marker() {
    let mut s = sample_strategy();
    s.invocation_template = Some("--model {{model}} {{prompt}}".to_string());
    s.prompt_via_stdin = true;

    let cmd = s
        .build_command("the prompt", Some("gpt-4"), None, None)
        .unwrap();
    let cmd_str = format!("{:?}", cmd);

    assert!(cmd_str.contains("--model"));
    assert!(cmd_str.contains("gpt-4"));
    // Prompt is delivered via stdin, not argv
    assert!(!cmd_str.contains("the prompt"));
}

#[test]
fn template_workdir_marker() {
    let mut s = sample_strategy();
    s.invocation_template = Some("--workdir {{workdir}} {{prompt}}".to_string());

    let cmd = s
        .build_command("the prompt", None, Some("/tmp/project"), None)
        .unwrap();
    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("--workdir"));
    assert!(cmd_str.contains("/tmp/project"));
    assert!(cmd_str.contains("the prompt"));

    // Without workdir, --workdir is elided
    let cmd2 = s.build_command("the prompt", None, None, None).unwrap();
    let cmd_str2 = format!("{:?}", cmd2);
    assert!(!cmd_str2.contains("--workdir"));
    assert!(cmd_str2.contains("the prompt"));
}

#[test]
fn template_optional_marker_glue_renders_when_available() {
    let s = sample_strategy();
    let argv = s.build_argv_from_template(
        "-m {{model}}#{{effort?}} {{prompt}}",
        "implement",
        TemplateArgs {
            model: Some("opencode/big-pickle"),
            working_dir: None,
            session_arg: None,
            effort: Some("high"),
            mcp_config: None,
        },
    );
    assert_eq!(argv, vec!["-m", "opencode/big-pickle#high", "implement"]);
}

#[test]
fn template_optional_marker_glue_elided_when_absent() {
    let s = sample_strategy();
    let argv = s.build_argv_from_template(
        "-m {{model}}#{{effort?}} {{prompt}}",
        "implement",
        TemplateArgs {
            model: Some("opencode/big-pickle"),
            working_dir: None,
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(argv, vec!["-m", "opencode/big-pickle", "implement"]);
    assert!(argv.contains(&"opencode/big-pickle".to_string()));
    assert!(!argv.iter().any(|w| w.contains('#')));
}

#[test]
fn template_optional_marker_hyphen_glue() {
    let s = sample_strategy();
    let template = "--model {{model}}-{{effort?}} {{prompt}}";
    let with_effort = s.build_argv_from_template(
        template,
        "go",
        TemplateArgs {
            model: Some("claude-sonnet-5"),
            working_dir: None,
            session_arg: None,
            effort: Some("high"),
            mcp_config: None,
        },
    );
    assert_eq!(with_effort, vec!["--model", "claude-sonnet-5-high", "go"]);
    let without_effort = s.build_argv_from_template(
        template,
        "go",
        TemplateArgs {
            model: Some("claude-sonnet-5"),
            working_dir: None,
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(without_effort, vec!["--model", "claude-sonnet-5", "go"]);
}

#[test]
fn template_required_marker_still_drops_token_and_flag() {
    let s = sample_strategy();
    let template = "--variant {{effort}} {{prompt}}";
    let argv = s.build_argv_from_template(
        template,
        "go",
        TemplateArgs {
            model: None,
            working_dir: None,
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(argv, vec!["go"]);
    assert!(!argv.contains(&"--variant".to_string()));
}

#[test]
fn template_optional_marker_alone_in_token_drops_with_flag() {
    let s = sample_strategy();
    let template = "--variant {{effort?}} {{prompt}}";
    let argv = s.build_argv_from_template(
        template,
        "go",
        TemplateArgs {
            model: None,
            working_dir: None,
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(argv, vec!["go"]);
    let argv2 = s.build_argv_from_template(
        template,
        "go",
        TemplateArgs {
            model: None,
            working_dir: None,
            session_arg: None,
            effort: Some("high"),
            mcp_config: None,
        },
    );
    assert_eq!(argv2, vec!["--variant", "high", "go"]);
}

#[test]
fn unsafe_optional_model_token_rejects_all_optional_model_bearing_token() {
    let msg = CliStrategy::unsafe_optional_model_token(
        "-m {{model?}}#{{effort?}} {{prompt}}",
        "opencode",
    );
    assert!(msg.is_some());
    let msg = msg.unwrap();
    assert!(msg.contains("opencode"));
    assert!(msg.contains("{{model?}}#{{effort?}}"));
}

#[test]
fn unsafe_optional_model_token_allows_required_model_with_optional_effort() {
    assert!(CliStrategy::unsafe_optional_model_token(
        "-m {{model}}#{{effort?}} {{prompt}}",
        "opencode",
    )
    .is_none());
}

#[test]
fn unsafe_optional_model_token_none_when_no_model_marker() {
    assert!(CliStrategy::unsafe_optional_model_token(
        "-p {{prompt}} --add-dir {{workdir}}",
        "antigravity",
    )
    .is_none());
}

#[test]
fn real_template_copilot_prompt_immediately_after_p_flag() {
    // Source: canopy-registry/platforms/copilot.toml @ 0f90d39
    // Template: "-p {{prompt}} {{session_flag}} {{session_id}} --model {{model}}"
    let template = "-p {{prompt}} {{session_flag}} {{session_id}} --model {{model}}";
    let s = strategy_with_template(template);

    // Without session: session markers elided, no orphan flags.
    let argv = s.build_argv_from_template(
        template,
        "fix the bug",
        TemplateArgs {
            model: Some("gpt-4"),
            working_dir: None,
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(argv, vec!["-p", "fix the bug", "--model", "gpt-4"]);
    assert_eq!(argv[0], "-p");
    assert_eq!(argv[1], "fix the bug");

    // With session: -p still immediately followed by prompt, not by session flag.
    let argv2 = s.build_argv_from_template(
        template,
        "fix the bug",
        TemplateArgs {
            model: Some("gpt-4"),
            working_dir: None,
            session_arg: Some(("--session-id", "uuid-123")),
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(
        argv2,
        vec![
            "-p",
            "fix the bug",
            "--session-id",
            "uuid-123",
            "--model",
            "gpt-4"
        ]
    );
    assert_eq!(argv2[0], "-p");
    assert_eq!(argv2[1], "fix the bug");
    // Guard against the RS1 regression: `copilot -p --session-id <uuid> <PROMPT>`
    assert_ne!(argv2[1], "--session-id");
}

#[test]
fn real_template_claude_argv_with_effort() {
    // Source: canopy-registry/platforms/claude.toml @ 0f90d39
    // Template: "--effort {{effort}} --model {{model}} {{session_flag}} {{session_id}} {{prompt}}"
    let template =
        "--effort {{effort}} --model {{model}} {{session_flag}} {{session_id}} {{prompt}}";
    let s = strategy_with_template(template);
    let argv = s.build_argv_from_template(
        template,
        "do the thing",
        TemplateArgs {
            model: Some("claude-opus-4-8"),
            working_dir: None,
            session_arg: Some(("--session-id", "ses_abc")),
            effort: Some("high"),
            mcp_config: None,
        },
    );
    assert_eq!(
        argv,
        vec![
            "--effort",
            "high",
            "--model",
            "claude-opus-4-8",
            "--session-id",
            "ses_abc",
            "do the thing"
        ]
    );
}

#[test]
fn real_template_claude_argv_without_effort_elides_flag() {
    // Source: canopy-registry/platforms/claude.toml @ 0f90d39
    let template =
        "--effort {{effort}} --model {{model}} {{session_flag}} {{session_id}} {{prompt}}";
    let s = strategy_with_template(template);
    let argv = s.build_argv_from_template(
        template,
        "do the thing",
        TemplateArgs {
            model: Some("claude-opus-4-8"),
            working_dir: None,
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(argv, vec!["--model", "claude-opus-4-8", "do the thing"]);
    assert!(!argv.contains(&"--effort".to_string()));
    assert!(!argv.contains(&"--session-id".to_string()));
}

#[test]
fn real_template_gemini_session_flag_before_prompt() {
    // Source: canopy-registry/platforms/gemini.toml @ 0f90d39
    // Template: "{{session_flag}} {{session_id}} -p {{prompt}} --model {{model}}"
    let template = "{{session_flag}} {{session_id}} -p {{prompt}} --model {{model}}";
    let s = strategy_with_template(template);
    let argv = s.build_argv_from_template(
        template,
        "analyze this",
        TemplateArgs {
            model: Some("gemini-2.5-pro"),
            working_dir: None,
            session_arg: Some(("--session-id", "ses_gem")),
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(
        argv,
        vec![
            "--session-id",
            "ses_gem",
            "-p",
            "analyze this",
            "--model",
            "gemini-2.5-pro"
        ]
    );
    // Without session: session markers elided, no orphan flags.
    let argv2 = s.build_argv_from_template(
        template,
        "analyze this",
        TemplateArgs {
            model: Some("gemini-2.5-pro"),
            working_dir: None,
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(
        argv2,
        vec!["-p", "analyze this", "--model", "gemini-2.5-pro"]
    );
}

#[test]
fn real_template_codex_effort_as_single_argv_word() {
    // Source: canopy-registry/platforms/codex.toml @ 0f90d39
    // Template: "-m {{model}} -C {{workdir}} -c 'model_reasoning_effort=\"{{effort}}\"' {{prompt}}"
    let template =
        "-m {{model}} -C {{workdir}} -c 'model_reasoning_effort=\"{{effort}}\"' {{prompt}}";
    let s = strategy_with_template(template);
    let argv = s.build_argv_from_template(
        template,
        "refactor",
        TemplateArgs {
            model: Some("o3"),
            working_dir: Some("/proj"),
            session_arg: None,
            effort: Some("high"),
            mcp_config: None,
        },
    );
    assert_eq!(
        argv,
        vec![
            "-m",
            "o3",
            "-C",
            "/proj",
            "-c",
            "'model_reasoning_effort=\"high\"'",
            "refactor"
        ]
    );
    // Verify -c value is one single argv word containing the quotes.
    assert_eq!(argv[4], "-c");
    assert_eq!(argv[5], "'model_reasoning_effort=\"high\"'");
    // Without effort: flag and quoted token elided together.
    let argv2 = s.build_argv_from_template(
        template,
        "refactor",
        TemplateArgs {
            model: Some("o3"),
            working_dir: Some("/proj"),
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(argv2, vec!["-m", "o3", "-C", "/proj", "refactor"]);
    assert!(!argv2.contains(&"-c".to_string()));
}

#[test]
fn real_template_cursor_effort_embedded_in_model() {
    // Source: canopy-registry/platforms/cursor.toml @ 0f90d39
    // Template: "{{model}}[context=1m,effort={{effort}},fast=false] --workspace {{workdir}} {{prompt}}"
    let template =
        "{{model}}[context=1m,effort={{effort}},fast=false] --workspace {{workdir}} {{prompt}}";
    let s = strategy_with_template(template);
    let argv = s.build_argv_from_template(
        template,
        "build ui",
        TemplateArgs {
            model: Some("claude-opus-4-8"),
            working_dir: Some("/proj"),
            session_arg: None,
            effort: Some("high"),
            mcp_config: None,
        },
    );
    assert_eq!(
        argv,
        vec![
            "claude-opus-4-8[context=1m,effort=high,fast=false]",
            "--workspace",
            "/proj",
            "build ui"
        ]
    );
    assert_eq!(
        argv[0],
        "claude-opus-4-8[context=1m,effort=high,fast=false]"
    );
}

#[test]
fn real_template_cursor_effort_elided_when_unavailable() {
    // Source: canopy-registry/platforms/cursor.toml @ 0f90d39
    let template =
        "{{model}}[context=1m,effort={{effort}},fast=false] --workspace {{workdir}} {{prompt}}";
    let s = strategy_with_template(template);
    // Without effort, the whole composite token must be elided (both markers required).
    let argv = s.build_argv_from_template(
        template,
        "build ui",
        TemplateArgs {
            model: Some("claude-opus-4-8"),
            working_dir: Some("/proj"),
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(argv, vec!["--workspace", "/proj", "build ui"]);
    assert!(!argv.iter().any(|w| w.contains("effort=")));
}

#[test]
fn real_template_cline_thinking_flag() {
    // Source: canopy-registry/platforms/cline.toml @ 0f90d39
    // Template: "--thinking {{effort}} -m {{model}} -c {{workdir}} {{prompt}}"
    let template = "--thinking {{effort}} -m {{model}} -c {{workdir}} {{prompt}}";
    let s = strategy_with_template(template);
    let argv = s.build_argv_from_template(
        template,
        "explain",
        TemplateArgs {
            model: Some("gpt-4"),
            working_dir: Some("/proj"),
            session_arg: None,
            effort: Some("medium"),
            mcp_config: None,
        },
    );
    assert_eq!(
        argv,
        vec![
            "--thinking",
            "medium",
            "-m",
            "gpt-4",
            "-c",
            "/proj",
            "explain"
        ]
    );
    // Without effort: no orphan --thinking flag.
    let argv2 = s.build_argv_from_template(
        template,
        "explain",
        TemplateArgs {
            model: Some("gpt-4"),
            working_dir: Some("/proj"),
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(argv2, vec!["-m", "gpt-4", "-c", "/proj", "explain"]);
    assert!(!argv2.contains(&"--thinking".to_string()));
}

#[test]
fn real_template_opencode_v2_effort_in_model() {
    // Source: CM31 — opencode v2 removed `--variant`; reasoning level now
    // rides inside `-m provider/model#variant`. `{{effort?}}` optional so a
    // dispatch with no effort still gets `-m <model>` (not a dropped flag).
    let template = "-m {{model}}#{{effort?}} --dir {{workdir}} {{prompt}}";
    let s = strategy_with_template(template);
    let argv = s.build_argv_from_template(
        template,
        "implement",
        TemplateArgs {
            model: Some("opencode/big-pickle"),
            working_dir: Some("/proj"),
            session_arg: None,
            effort: Some("max"),
            mcp_config: None,
        },
    );
    assert_eq!(
        argv,
        vec![
            "-m",
            "opencode/big-pickle#max",
            "--dir",
            "/proj",
            "implement"
        ]
    );
    // Without effort: `-m opencode/big-pickle` survives whole — this is
    // the CM31 fix; the old template lost `-m` and the model entirely here.
    let argv2 = s.build_argv_from_template(
        template,
        "implement",
        TemplateArgs {
            model: Some("opencode/big-pickle"),
            working_dir: Some("/proj"),
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(
        argv2,
        vec!["-m", "opencode/big-pickle", "--dir", "/proj", "implement"]
    );
    assert!(!argv2.iter().any(|w| w.contains('#')));
}

#[test]
fn real_template_antigravity_no_effort_elides_cleanly() {
    // Source: canopy-registry/platforms/antigravity.toml @ 0f90d39
    // Template: "-p {{prompt}} --add-dir {{workdir}}"
    // effort_declaration = { form = "", values = [] } — declared as not supporting effort.
    let template = "-p {{prompt}} --add-dir {{workdir}}";
    let s = strategy_with_template(template);
    let argv = s.build_argv_from_template(
        template,
        "hello",
        TemplateArgs {
            model: None,
            working_dir: Some("/proj"),
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );
    assert_eq!(argv, vec!["-p", "hello", "--add-dir", "/proj"]);
    // Even if effort is passed, template has no {{effort}} marker so argv is unchanged.
    let argv2 = s.build_argv_from_template(
        template,
        "hello",
        TemplateArgs {
            model: None,
            working_dir: Some("/proj"),
            session_arg: None,
            effort: Some("high"),
            mcp_config: None,
        },
    );
    assert_eq!(argv2, vec!["-p", "hello", "--add-dir", "/proj"]);
    assert!(!argv2.iter().any(|w| w.contains("high")));
}

#[test]
fn build_command_with_mcp_config_injects_path() {
    let template = "-p {{prompt}} --mcp-config {{mcp_config}}";
    let s = strategy_with_template(template);
    let argv = s.build_argv_from_template(
        template,
        "hello",
        TemplateArgs {
            model: None,
            working_dir: None,
            session_arg: None,
            effort: None,
            mcp_config: Some("/tmp/mcp.json"),
        },
    );
    assert!(argv.contains(&"/tmp/mcp.json".to_string()));
    assert!(argv.contains(&"--mcp-config".to_string()));
}

#[test]
fn build_command_with_mcp_config_none_omits_marker() {
    let template = "-p {{prompt}} --mcp-config {{mcp_config}}";
    let s = strategy_with_template(template);
    let argv = s.build_argv_from_template(
        template,
        "hello",
        TemplateArgs {
            model: None,
            working_dir: None,
            session_arg: None,
            effort: None,
            mcp_config: None,
        },
    );
    assert!(!argv.iter().any(|w| w.contains("mcp")));
    assert_eq!(argv, vec!["-p", "hello"]);
}

#[test]
fn build_command_with_mcp_config_with_effort_reaches_argv() {
    let template = "-p {{prompt}} --mcp-config {{mcp_config}} --effort {{effort}}";
    let s = strategy_with_template(template);
    let cmd = s
        .build_command_with_mcp_config(
            "hello",
            None,
            None,
            Some("/tmp/mcp.json"),
            Some("high"),
            None,
        )
        .unwrap();
    let cmd_str = format!("{:?}", cmd);
    assert!(cmd_str.contains("--effort"));
    assert!(cmd_str.contains("high"));
}

#[test]
fn build_command_with_mcp_config_without_effort_elides_flag() {
    let template = "-p {{prompt}} --mcp-config {{mcp_config}} --effort {{effort}}";
    let s = strategy_with_template(template);
    let cmd = s
        .build_command_with_mcp_config("hello", None, None, Some("/tmp/mcp.json"), None, None)
        .unwrap();
    let cmd_str = format!("{:?}", cmd);
    assert!(!cmd_str.contains("--effort"));
}
