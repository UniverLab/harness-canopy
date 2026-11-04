//! Real-muse contract: the binary behaviors canopy's resume-by-identity
//! relies on, verified against the installed `muse` in scratch dirs
//! (never the live store). Each test skips when no `muse` is on `PATH`.
//!
//! * the session store honors `XDG_DATA_HOME`, else `~/.local/share`;
//! * a saved session records its workspace root in `session.jsonl`;
//! * two sessions have distinct ids;
//! * there is no `--session-id` flag; identity resume is `resume <ref>`;
//! * resuming an unknown id fails fast with "no saved log".

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn real_muse() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("muse"))
        .find(|candidate| candidate.is_file())
}

/// Scratch world for one test: home, XDG dir and workdir.
struct Scratch {
    #[allow(dead_code)]
    root: tempfile::TempDir,
    home: PathBuf,
    xdg: PathBuf,
    work: PathBuf,
}

impl Scratch {
    fn setup() -> Self {
        let root = tempfile::tempdir().expect("scratch root");
        let home = root.path().join("home");
        let xdg = root.path().join("xdg");
        let work = root.path().join("work");
        for dir in [&home, &xdg, &work] {
            std::fs::create_dir_all(dir).expect("scratch dir");
        }
        Self {
            root,
            home,
            xdg,
            work,
        }
    }
}

fn run_muse(
    real: &Path,
    scratch: &Scratch,
    use_xdg: bool,
    args: &[&str],
    timeout: Duration,
) -> Option<std::process::Output> {
    let mut cmd = Command::new(real);
    cmd.args(args)
        .env("HOME", &scratch.home)
        .env("TERM", "xterm-256color")
        .current_dir(&scratch.work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if use_xdg {
        cmd.env("XDG_DATA_HOME", &scratch.xdg);
    } else {
        cmd.env_remove("XDG_DATA_HOME");
    }
    let mut child = cmd.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Every `session.jsonl` outside `subagent/` subtrees.
fn top_level_session_logs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.components().any(|c| c.as_os_str() == "subagent") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|name| name == "session.jsonl") {
                found.push(path);
            }
        }
    }
    found
}

fn with_muse(name: &str) -> Option<(PathBuf, Scratch)> {
    let Some(real) = real_muse() else {
        eprintln!("SKIP {name}: no `muse` on PATH");
        return None;
    };
    Some((real, Scratch::setup()))
}

#[test]
fn help_has_no_session_id_flag() {
    let Some((real, _)) = with_muse("help_has_no_session_id_flag") else {
        return;
    };
    let out = Command::new(&real)
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .expect("muse --help");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        !help.contains("--session-id") && !help.contains("--session_id"),
        "muse has no session-id flag to pass"
    );
}

#[test]
fn resume_help_documents_ref_and_root_opts_either_side() {
    let Some((real, _)) = with_muse("resume_help") else {
        return;
    };
    let out = Command::new(&real)
        .args(["resume", "--help"])
        .stdin(Stdio::null())
        .output()
        .expect("muse resume --help");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("<session-ref>"),
        "identity resume takes a session ref: {help}"
    );
    assert!(
        help.contains("either side"),
        "root options may appear on either side of resume: {help}"
    );
}

#[test]
fn exec_records_session_with_workspace_root_under_xdg() {
    let Some((real, scratch)) = with_muse("exec_under_xdg") else {
        return;
    };
    let out = run_muse(
        &real,
        &scratch,
        true,
        &["exec", "--provider", "echo", "xdg probe"],
        Duration::from_secs(120),
    )
    .expect("muse exec must complete");
    assert!(
        out.status.success(),
        "muse exec failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let logs = top_level_session_logs(&scratch.xdg);
    assert_eq!(
        logs.len(),
        1,
        "one session must land under XDG_DATA_HOME: {logs:?}"
    );
    let body = std::fs::read_to_string(&logs[0]).expect("read session.jsonl");
    let want = format!("\"workspace_root\":\"{}\"", scratch.work.display());
    assert!(
        body.contains(&want),
        "the saved session records its workspace root"
    );
    assert!(
        top_level_session_logs(&scratch.home).is_empty(),
        "nothing may leak outside the XDG store"
    );
}

#[test]
fn exec_falls_back_to_home_local_share() {
    let Some((real, scratch)) = with_muse("exec_home_fallback") else {
        return;
    };
    let out = run_muse(
        &real,
        &scratch,
        false,
        &["exec", "--provider", "echo", "home probe"],
        Duration::from_secs(120),
    )
    .expect("muse exec must complete");
    assert!(
        out.status.success(),
        "muse exec failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expect_root = scratch.home.join(".local").join("share");
    let logs = top_level_session_logs(&expect_root);
    assert_eq!(
        logs.len(),
        1,
        "one session must land under ~/.local/share: {logs:?}"
    );
}

#[test]
fn two_execs_have_distinct_session_ids() {
    let Some((real, scratch)) = with_muse("two_execs") else {
        return;
    };
    for prompt in ["first contract session", "second contract session"] {
        let out = run_muse(
            &real,
            &scratch,
            true,
            &["exec", "--provider", "echo", prompt],
            Duration::from_secs(120),
        )
        .expect("muse exec must complete");
        assert!(out.status.success());
    }
    let logs = top_level_session_logs(&scratch.xdg);
    assert_eq!(logs.len(), 2, "two sessions must be saved: {logs:?}");
    let dirs: std::collections::HashSet<_> = logs
        .iter()
        .filter_map(|log| log.parent().map(Path::to_path_buf))
        .collect();
    assert_eq!(dirs.len(), 2, "the two sessions have distinct ids");
}

#[test]
fn resume_bogus_id_fails_fast_with_no_saved_log() {
    let Some((real, scratch)) = with_muse("resume_bogus") else {
        return;
    };
    let out = run_muse(
        &real,
        &scratch,
        true,
        &[
            "--provider",
            "echo",
            "resume",
            "00000000-0000-0000-0000-000000000000",
        ],
        Duration::from_secs(90),
    )
    .expect("muse resume must terminate");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "resuming an unknown id must fail: {combined}"
    );
    assert!(
        combined.contains("no saved log"),
        "the failure names the missing log: {combined}"
    );
}
