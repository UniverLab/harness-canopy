//! `tests` tests (split from `src/daemon/doctor.rs`, pure move).
use super::doctor_cli_label;
use super::*;
use crate::application::ports::AgentRepository;
use crate::domain::canopy_config::CanopyConfig;
use crate::domain::cli_config::CliConfig;
use crate::domain::models::{Agent, Cli, Trigger};
use crate::rag::vector_store::{VectorChunk, VectorStore};
use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;

/// Writes a minimal `~/.config/systemd/user/canopy.service` under
/// `home` so `report_service_unit` reaches its browser block. The
/// ExecStart names a path that cannot exist: the binary check then
/// takes its Missing branch — which never spawns anything — and its
/// issue text carries no "BROWSER", so the browser assertions stay
/// clean. Linux-only because the block is gated on
/// `manager == "systemd"`.
#[cfg(target_os = "linux")]
fn write_minimal_unit(home: &Path) {
    let unit_dir = home.join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir).unwrap();
    std::fs::write(
        unit_dir.join("canopy.service"),
        "[Service]\nExecStart=/nonexistent/canopy serve\n",
    )
    .unwrap();
}

/// The browser-related entries of a doctor run's issues — the
/// missing ExecStart binary also contributes its own unrelated line.
#[cfg(target_os = "linux")]
fn browser_issues(issues: &[String]) -> Vec<&String> {
    issues.iter().filter(|i| i.contains("BROWSER")).collect()
}

/// Mirror of `browser_issues` for the CB81 OOMPolicy check.
#[cfg(target_os = "linux")]
fn oom_policy_issues(issues: &[String]) -> Vec<&String> {
    issues.iter().filter(|i| i.contains("OOMPolicy")).collect()
}

// ── CB44 identity check ──────────────────────────────────

fn identity_cli_config(
    binary: &std::path::Path,
    cmd: Option<&str>,
    contains: Option<&str>,
) -> crate::domain::cli_config::CliConfig {
    crate::domain::cli_config::CliConfig {
        name: "blackbox".to_string(),
        binary: binary.to_string_lossy().to_string(),
        identity_check: match (cmd, contains) {
            (Some(c), Some(s)) => Some(crate::domain::cli_config::IdentityCheck {
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
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    path
}

fn binary_identity_cli_config(
    binary: &std::path::Path,
    args: Option<Vec<&str>>,
    pattern: Option<&str>,
) -> CliConfig {
    CliConfig {
        name: "blackbox".to_string(),
        binary: binary.to_string_lossy().to_string(),
        identity_args: args.map(|a| a.into_iter().map(String::from).collect()),
        identity_pattern: pattern.map(String::from),
        ..Default::default()
    }
}

/// `run_doctor` reads `$HOME` (via `dirs::home_dir()`, transitively
/// through every helper it calls: `CanopyConfig::load`,
/// `VectorStore::default_lancedb_path`, `cli_strategy::daemon_path`,
/// etc.) and writes a human-readable report straight to real stdout —
/// there is no injected sink to assert against. To exercise it as a
/// black box we (1) point `$HOME` at a disposable fixture directory for
/// the duration of the call, and (2) redirect fd 1 into a pipe so the
/// printed report can be captured and asserted on.
///
/// Every test in this module mutates the real process-wide `$HOME`
/// env var. That's safe under `cargo nextest` (one process per test)
/// but would race under plain `cargo test` — this crate's CI and this
/// task's protocol both mandate nextest, so no extra mutex is added
/// here (unlike the `CANOPY_HOME_OVERRIDE` tests elsewhere, which
/// guard against both runners).
struct StdoutCapture {
    saved_fd: i32,
    read_end: std::fs::File,
}

impl StdoutCapture {
    fn start() -> Self {
        let mut fds = [0i32; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        let (read_fd, write_fd) = (fds[0], fds[1]);
        let saved_fd = unsafe { libc::dup(1) };
        assert!(saved_fd >= 0, "dup(1) failed");
        std::io::stdout().flush().unwrap();
        let rc = unsafe { libc::dup2(write_fd, 1) };
        assert_eq!(rc, 1, "dup2 failed");
        unsafe { libc::close(write_fd) };
        StdoutCapture {
            saved_fd,
            read_end: unsafe { std::fs::File::from_raw_fd(read_fd) },
        }
    }

    fn stop(mut self) -> String {
        std::io::stdout().flush().unwrap();
        unsafe {
            libc::dup2(self.saved_fd, 1);
            libc::close(self.saved_fd);
        }
        let mut buf = String::new();
        self.read_end.read_to_string(&mut buf).unwrap();
        buf
    }
}

/// RAII guard: sets real `$HOME` to `path` for the test body, restores
/// the previous value on drop.
struct HomeVar {
    prev: Option<std::ffi::OsString>,
}

impl HomeVar {
    fn set(path: &std::path::Path) -> Self {
        let prev = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", path) };
        HomeVar { prev }
    }
}

impl Drop for HomeVar {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

async fn run_doctor_captured(home: &std::path::Path) -> (Result<()>, String) {
    let _home = HomeVar::set(home);
    let cap = StdoutCapture::start();
    let result = run_doctor().await;
    let output = cap.stop();
    (result, output)
}

fn sample_agent(id: &str) -> Agent {
    Agent {
        id: id.to_string(),
        prompt: "do things".to_string(),
        trigger: Some(Trigger::Cron {
            schedule_expr: "0 * * * *".to_string(),
        }),
        cli: Cli::new("opencode"),
        model: None,
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: chrono::Utc::now(),
        log_path: "/tmp/doctor-test.log".to_string(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    }
}

/// Writes an executable fake `canopy` into `dir` that prints
/// `version_output` to stdout and exits 0 on `--version` — a synthetic
/// stand-in for a real install, so the C17 tests below never touch the
/// developer's real PATH or real `canopy` binaries.
fn write_fake_canopy(dir: &Path, version_output: &str) -> PathBuf {
    let path = dir.join("canopy");
    std::fs::write(&path, format!("#!/bin/sh\necho '{version_output}'\n")).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    path
}

fn synthetic_path_var(dirs: &[&Path]) -> String {
    std::env::join_paths(dirs).unwrap().into_string().unwrap()
}

// ── CB93: doctor's daily-health-routine renderings ───────────────
// stdout-fd capture is unreliable in CI (see the `run_doctor_*` note
// above) — these work locally and are marked ignored.

fn fresh_home_with_db() -> (tempfile::TempDir, std::path::PathBuf, Database) {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let db_path = canopy_dir.join("background_agents.db");
    let db = Database::new(&db_path).unwrap();
    db.upsert_agent(&sample_agent("agent-1")).unwrap();
    (home, canopy_dir, db)
}

mod path_copies_tests;
mod service_unit_tests;
