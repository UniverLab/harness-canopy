//! Workdir tests: every spawned process runs IN the resolved `workdir` —
//! via cwd for all platforms and, when the platform declares a flag, via argv
//! too — plus refusal paths for a workdir that cannot be used. Sits in its
//! own part-file (`part03`) because `part01` is at the file-size gate.
use super::*;

/// A strategy whose absolute binary path is never resolved against the
/// filesystem (absolute paths short-circuit to `AbsolutePath` without an
/// existence check), so no real CLI is needed — the commands are built,
/// never spawned.
fn workdir_strategy(template: Option<&str>, supports_working_dir: bool) -> CliStrategy {
    CliStrategy {
        binary: "/usr/local/bin/test-cli".to_string(),
        headless_mode: "run".to_string(),
        model_flag: None,
        supports_working_dir,
        working_dir_flag: None,
        env_vars: HashMap::new(),
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
        invocation_template: template.map(str::to_string),
        effort_declaration: None,
        decision_args: None,
        identity: None,
    }
}

fn argv_of(cmd: &std::process::Command) -> Vec<String> {
    cmd.get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn opencode_flag_runs_inside_workdir_argv_and_cwd() {
    // T1: the opencode shape (`supports_working_dir = true`, flag `--dir`).
    // Both halves of the contract are asserted so either alone regressing
    // breaks the test: the argv gets `--dir <dir>` AND the child's cwd is
    // `<dir>`. A bug that passes the flag but leaves the child in the
    // daemon's cwd (the SQLite-visible cwd path) fails the cwd assert; a
    // bug that sets only cwd fails the argv assert.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_string_lossy().into_owned();
    let mut s = workdir_strategy(None, true);
    s.working_dir_flag = Some("--dir".to_string());

    let cmd = s
        .build_command_with_mcp_config("hello", None, Some(&dir), None, None, None)
        .unwrap();
    let std_cmd = cmd.as_std();
    let argv = argv_of(std_cmd);

    assert!(
        argv.windows(2).any(|w| w[0] == "--dir" && w[1] == dir),
        "argv must carry `--dir {dir}`, got: {argv:?}"
    );
    assert_eq!(
        std_cmd.get_current_dir(),
        Some(tmp.path()),
        "child must run with cwd = workdir"
    );
}

#[test]
fn legacy_platform_gets_cwd_even_without_workdir_support() {
    // T2: a platform with `supports_working_dir = false` emits NO working-dir
    // flag — but its child still runs inside the given workdir via cwd.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_string_lossy().into_owned();
    let s = workdir_strategy(None, false);

    let cmd = s.build_command("hello", None, Some(&dir), None).unwrap();
    let std_cmd = cmd.as_std();
    let argv = argv_of(std_cmd);

    assert!(
        !argv.contains(&"--dir".to_string()) && !argv.contains(&dir),
        "no working-dir flag may reach argv, got: {argv:?}"
    );
    assert_eq!(
        std_cmd.get_current_dir(),
        Some(tmp.path()),
        "cwd must be set even when the platform has no flag for it"
    );
}

#[test]
fn template_path_runs_inside_workdir() {
    // T3a: a template carrying `{{workdir}}` renders the directory into argv,
    // and the child's cwd is set to the same directory.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_string_lossy().into_owned();
    let template = "run --dir {{workdir}} {{prompt}}";
    let mut s = workdir_strategy(Some(template), true);
    // headless_mode is always prepended — clear it so only the template's
    // own `run` word appears in the expected argv.
    s.headless_mode = String::new();
    s.working_dir_flag.clone_from(&Some("--dir".to_string()));

    let cmd = s
        .build_command_with_mcp_config("hello", None, Some(&dir), None, None, None)
        .unwrap();
    let std_cmd = cmd.as_std();
    let argv = argv_of(std_cmd);

    assert!(
        argv.contains(&dir),
        "template must render the workdir into argv, got: {argv:?}"
    );
    assert_eq!(std_cmd.get_current_dir(), Some(tmp.path()));
}

#[test]
fn template_without_workdir_drops_token_and_leaves_cwd_inherited() {
    // T3b: no `working_dir` passed — the `{{workdir}}` token and its bound
    // `--dir` glue are dropped from argv, and NO cwd is set (the command
    // inherits the caller's), so the `None` default keeps today's behaviour.
    let template = "run --dir {{workdir}} {{prompt}}";
    let mut s = workdir_strategy(Some(template), true);
    // headless_mode is always prepended — clear it so only the template's
    // own `run` word appears in the expected argv.
    s.headless_mode = String::new();
    s.working_dir_flag.clone_from(&Some("--dir".to_string()));

    let cmd = s
        .build_command_with_mcp_config("hello", None, None, None, None, None)
        .unwrap();
    let std_cmd = cmd.as_std();
    let argv = argv_of(std_cmd);

    assert_eq!(
        argv,
        vec!["run", "hello"],
        "`--dir {{workdir}}` must be dropped entirely, got: {argv:?}"
    );
    assert!(
        std_cmd.get_current_dir().is_none(),
        "no cwd may be pinned when no workdir was given"
    );
}
