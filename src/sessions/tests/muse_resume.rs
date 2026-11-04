//! muse resume-by-identity: every active muse session relaunches as
//! `muse resume <its own conversation id>` after a daemon restart, `args`
//! is never rewritten with the resume command, at most one id-less session
//! per cli+workdir takes the `--last` fallback, and a resume that dies
//! immediately leaves its stderr tail on the row.
//!
//! The fake-muse fixtures below pin shapes; the `real_muse_*` tests prove
//! the identity round-trip against the REAL muse binary in scratch dirs
//! (never the live daemon, store or database) and skip when no `muse` is
//! on `PATH`.

use super::*;
use crate::db::session::{RegistrationOutcome, SessionRegistration};
use crate::sessions::hub_command::build_spawn_command;
use crate::sessions::muse_conversation::{
    discover_conversation, discover_conversation_by_pid, muse_store_dir, record_fresh_conversation,
};
use crate::sessions::resume::reconcile_at_startup;

// ── discovery against fixture stores ─────────────────────────────

/// One fixture conversation: `<store>/day/<id>/session.jsonl` recording
/// `workdir` as its workspace root.
fn write_fixture_session(store: &std::path::Path, id: &str, workdir: &str) -> std::path::PathBuf {
    let dir = store.join("day").join(id);
    std::fs::create_dir_all(&dir).expect("fixture session dir");
    let file = dir.join("session.jsonl");
    std::fs::write(
        &file,
        format!(
            "{{\"schema_version\":1,\"stream\":{{\"kind\":\"session\",\"id\":\"{id}\"}},\"sequence\":3,\"payload\":{{\"workspace_root\":\"{workdir}\"}}}}\n"
        ),
    )
    .expect("fixture session.jsonl");
    file
}

#[test]
fn discover_finds_the_session_for_the_workdir() {
    let store = tempfile::tempdir().expect("store");
    let since = std::time::SystemTime::now();
    write_fixture_session(store.path(), "uuid-aaa", "/work/proj");
    write_fixture_session(store.path(), "uuid-bbb", "/work/other");

    let found = discover_conversation(store.path(), "/work/proj", since);
    assert_eq!(found.as_deref(), Some("uuid-aaa"));
}

#[test]
fn discover_ignores_subagent_sessions() {
    let store = tempfile::tempdir().expect("store");
    let since = std::time::SystemTime::now();
    // A subagent log records the same workspace root but is not a
    // top-level conversation: matching it would resume an unresumable id.
    let sub = store
        .path()
        .join("day")
        .join("uuid-parent")
        .join("subagent")
        .join("uuid-child");
    std::fs::create_dir_all(&sub).expect("subagent dir");
    std::fs::write(
        sub.join("session.jsonl"),
        "{\"payload\":{\"workspace_root\":\"/work/proj\"}}\n",
    )
    .expect("subagent log");

    assert_eq!(
        discover_conversation(store.path(), "/work/proj", since),
        None
    );
}

#[test]
fn discover_ignores_sessions_older_than_since() {
    let store = tempfile::tempdir().expect("store");
    write_fixture_session(store.path(), "uuid-old", "/work/proj");
    let since = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);

    assert_eq!(
        discover_conversation(store.path(), "/work/proj", since),
        None
    );
}

#[test]
fn discover_prefers_the_newest_match() {
    let store = tempfile::tempdir().expect("store");
    let since = std::time::SystemTime::now();
    write_fixture_session(store.path(), "uuid-first", "/work/proj");
    std::thread::sleep(std::time::Duration::from_millis(20));
    write_fixture_session(store.path(), "uuid-second", "/work/proj");

    let found = discover_conversation(store.path(), "/work/proj", since);
    assert_eq!(found.as_deref(), Some("uuid-second"));
}

#[test]
fn discover_on_a_missing_store_is_none() {
    let missing = std::path::PathBuf::from("/definitely/not/a/muse/store");
    let since = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    assert_eq!(discover_conversation(&missing, "/work/proj", since), None);
}

#[test]
fn store_dir_reads_the_live_environment() {
    // No assertion on the value (it depends on the test host): the call
    // must simply agree with the documented selection.
    let xdg = std::env::var_os("XDG_DATA_HOME").filter(|xdg| !xdg.is_empty());
    let expected = match xdg {
        Some(xdg) => Some(std::path::PathBuf::from(xdg).join("muse").join("sessions")),
        None => dirs::home_dir().map(|home| home.join(".local/share/muse/sessions")),
    };
    assert_eq!(muse_store_dir(), expected);
}

// ── exit_detail round-trip ───────────────────────────────────────

#[test]
fn exit_detail_round_trip_overwrites_and_clears() {
    let (db, _dir) = test_db();
    db.insert_interactive_session(
        "sess-1",
        "one",
        "muse",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    assert_eq!(db.get_session_exit_detail("sess-1").unwrap(), None);

    db.set_session_exit_detail("sess-1", "first failure")
        .unwrap();
    assert_eq!(
        db.get_session_exit_detail("sess-1").unwrap().as_deref(),
        Some("first failure")
    );
    // Overwrite: the latest failure is the one the row shows.
    db.set_session_exit_detail("sess-1", "second failure")
        .unwrap();
    assert_eq!(
        db.get_session_exit_detail("sess-1").unwrap().as_deref(),
        Some("second failure")
    );
    db.clear_session_exit_detail("sess-1").unwrap();
    assert_eq!(db.get_session_exit_detail("sess-1").unwrap(), None);
}

#[test]
fn legacy_database_gains_the_exit_detail_column() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("legacy.db");
    {
        let conn = rusqlite::Connection::open(&path).expect("legacy connection");
        conn.execute_batch(
            "CREATE TABLE interactive_sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                cli TEXT NOT NULL,
                working_dir TEXT NOT NULL,
                args TEXT,
                started_at TEXT NOT NULL,
                exited_at TEXT,
                exit_code INTEGER,
                status TEXT NOT NULL DEFAULT 'active',
                session_type TEXT NOT NULL DEFAULT 'interactive',
                pid INTEGER,
                boot_id TEXT,
                harness_conversation_id TEXT
             );",
        )
        .expect("seed legacy schema");
    }
    let db = Database::new(&path).expect("open pre-exit_detail db, running migration");
    db.insert_interactive_session(
        "sess-1",
        "one",
        "muse",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.set_session_exit_detail("sess-1", "migrated detail")
        .unwrap();
    assert_eq!(
        db.get_session_exit_detail("sess-1").unwrap().as_deref(),
        Some("migrated detail")
    );
}

// ── harness id clear + exit code ───────────────────────────────

#[test]
fn harness_id_clear_unblocks_rerecording() {
    let (db, _dir) = test_db();
    db.insert_interactive_session(
        "sess-1",
        "one",
        "muse",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.set_session_harness_conversation_id("sess-1", "conv-first")
        .unwrap();
    // The recorder never overwrites: without the clear, a fresh fallback
    // would keep pointing at the dead conversation forever.
    db.set_session_harness_conversation_id("sess-1", "conv-second")
        .unwrap();
    assert_eq!(
        db.get_interactive_session("sess-1")
            .unwrap()
            .expect("row")
            .harness_conversation_id
            .as_deref(),
        Some("conv-first")
    );

    db.clear_session_harness_conversation_id("sess-1").unwrap();
    assert_eq!(
        db.get_interactive_session("sess-1")
            .unwrap()
            .expect("row")
            .harness_conversation_id,
        None
    );
    db.set_session_harness_conversation_id("sess-1", "conv-second")
        .unwrap();
    assert_eq!(
        db.get_interactive_session("sess-1")
            .unwrap()
            .expect("row")
            .harness_conversation_id
            .as_deref(),
        Some("conv-second")
    );
}

#[test]
fn exit_code_reads_none_while_active() {
    let (db, _dir) = test_db();
    db.insert_interactive_session(
        "sess-1",
        "one",
        "muse",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    assert_eq!(db.get_session_exit_code("sess-1").unwrap(), None);
    assert_eq!(db.get_session_exit_code("missing").unwrap(), None);
    db.finish_interactive_session("sess-1", 3).unwrap();
    assert_eq!(db.get_session_exit_code("sess-1").unwrap(), Some(3));
}

// ── preserve_args registration ──────────────────────────────────

#[test]
fn resume_registration_with_preserve_args_keeps_the_original_args() {
    let (db, _dir) = test_db();
    db.insert_interactive_session(
        "sess-1",
        "one",
        "muse",
        "/tmp",
        Some("--provider echo"),
        None,
        "interactive",
        None,
    )
    .unwrap();

    let outcome = db
        .register_interactive_session(&SessionRegistration {
            id: "sess-1",
            name: "one",
            cli: "muse",
            working_dir: "/tmp",
            args: Some("--provider echo resume uuid-aaa"),
            pid: Some(4242),
            session_type: "interactive",
            boot_id: Some("boot-new"),
            harness_conversation_id: Some("uuid-aaa"),
            preserve_args: true,
        })
        .unwrap();
    assert_eq!(
        outcome,
        RegistrationOutcome::Reactivated {
            id: "sess-1".to_string(),
            name: "one".to_string(),
        }
    );
    assert_eq!(
        db.get_interactive_session_args("sess-1")
            .unwrap()
            .as_deref(),
        Some("--provider echo"),
        "a resume relaunch must not rewrite the recorded launch args"
    );
}

#[test]
fn registration_without_preserve_args_rewrites_them() {
    let (db, _dir) = test_db();
    db.insert_interactive_session(
        "sess-1",
        "one",
        "muse",
        "/tmp",
        Some("--provider echo"),
        None,
        "interactive",
        None,
    )
    .unwrap();

    db.register_interactive_session(&SessionRegistration {
        id: "sess-1",
        name: "one",
        cli: "muse",
        working_dir: "/tmp",
        args: Some("--provider echo resume uuid-aaa"),
        pid: Some(4242),
        session_type: "interactive",
        boot_id: Some("boot-new"),
        harness_conversation_id: Some("uuid-aaa"),
        preserve_args: false,
    })
    .unwrap();
    assert_eq!(
        db.get_interactive_session_args("sess-1")
            .unwrap()
            .as_deref(),
        Some("--provider echo resume uuid-aaa")
    );
}

// ── hub output tail ─────────────────────────────────────────────

#[test]
fn output_tail_returns_the_newest_text_capped() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let workdir = tempfile::tempdir().expect("workdir");
    let req = agent_spawn("cat", &workdir.path().to_string_lossy());
    let (session, _guard) = KillGuard::spawn(&hub, &req);

    session.ingest_chunk(b"first line\nsecond line\n");
    let tail = session.output_tail(1024).expect("tail");
    assert!(
        tail.contains("second line"),
        "tail holds the newest text: {tail:?}"
    );

    let capped = session.output_tail(6).expect("capped tail");
    assert!(capped.len() <= 6, "tail is capped: {capped:?}");
    assert!(
        capped.contains("line"),
        "the cap keeps the tail end: {capped:?}"
    );
}

// ── kill clears, natural failures record ─────────────────────────

#[test]
fn killed_session_keeps_no_failure_detail() {
    // An explicit close is not a failure: killing must leave no tail
    // behind for the TUI to retain as an error card.
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let workdir = tempfile::tempdir().expect("workdir");
    let req = agent_spawn("cat", &workdir.path().to_string_lossy());
    let (session, _guard) = KillGuard::spawn(&hub, &req);
    session.ingest_chunk(b"screen output at kill time\n");
    let id = session.id.clone();

    hub.kill(&id);

    assert_eq!(
        db.get_session_exit_detail(&id).unwrap(),
        None,
        "a killed session must read as closed, never failed with a tail"
    );
}

#[test]
fn natural_nonzero_exit_records_the_output_tail() {
    // `ls` of a missing path exits 2 with stderr on the PTY: the row must
    // fail with that tail attached, without the real muse binary.
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let workdir = tempfile::tempdir().expect("workdir");
    let mut req = agent_spawn("ls", &workdir.path().to_string_lossy());
    req.interactive_args = Some("/definitely/not/a/muse/store".to_string());
    let (session, _guard) = KillGuard::spawn(&hub, &req);
    let id = session.id.clone();

    assert!(
        poll_until(Duration::from_secs(30), || {
            db.get_interactive_session_status(&id)
                .unwrap_or(None)
                .as_deref()
                == Some("error")
        }),
        "the nonzero exit must surface as an error row"
    );
    let detail = db.get_session_exit_detail(&id).unwrap().unwrap_or_default();
    assert!(
        detail.contains("No such file") || detail.contains("not/a/muse"),
        "the harness's stderr tail is attached to the row: {detail:?}"
    );
    assert!(hub.get(&id).is_none(), "the dead session leaves the hub");
}

// ── the stale session-id flag is never passed ───────────────────

#[test]
fn hub_spawn_command_never_carries_a_session_id_flag() {
    // The registry still carries a stale `session_id_set_flag` for muse
    // (canopy-registry backlog); the interactive spawn path must not honor
    // it — the real binary has no such flag.
    let workdir = tempfile::tempdir().expect("workdir");
    let mut req = agent_spawn("muse", &workdir.path().to_string_lossy());
    req.interactive_args = Some("--provider echo resume uuid-aaa".to_string());
    req.harness_conversation_id = Some("uuid-aaa".to_string());

    let (_, args) = build_spawn_command(&req, "sess-1", "one");
    let args = args.expect("agent args");
    assert!(
        !args.contains("session-id") && !args.contains("session_id"),
        "spawn must never pass a session-id flag: {args:?}"
    );
    assert_eq!(args, "--provider echo resume uuid-aaa");
}

// ── real muse binary (scratch dirs only) ─────────────────────────

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::domain::canopy_config::CanopyConfig;

/// Serializes the real-muse tests: they prepend a `PATH` shim, which is
/// process-global. The shim dir holds only `muse`, so every other spawn
/// in the suite resolves exactly as before.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Scratch muse world: home, XDG store, workdir, a `muse` shim that logs
/// its argv and execs the real binary with the scratch environment, and the
/// `PATH` override that points spawns at it. Restores `PATH` on drop.
struct ScratchMuse {
    #[allow(dead_code)]
    root: tempfile::TempDir,
    home: PathBuf,
    xdg: PathBuf,
    work: PathBuf,
    real: PathBuf,
    argv_log: PathBuf,
    old_path: Option<OsString>,
}

impl ScratchMuse {
    /// `None` when no `muse` is on `PATH`: the caller skips.
    fn setup() -> Option<Self> {
        let real = find_real_muse()?;
        let root = tempfile::tempdir().ok()?;
        let home = root.path().join("home");
        let xdg = root.path().join("xdg");
        let work = root.path().join("work");
        let shim = root.path().join("shim");
        for dir in [&home, &xdg, &work, &shim] {
            std::fs::create_dir_all(dir).ok()?;
        }
        let argv_log = root.path().join("argv.log");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nHOME={} XDG_DATA_HOME={} exec {} \"$@\"\n",
            shell_quote(&argv_log),
            shell_quote(&home),
            shell_quote(&xdg),
            shell_quote(&real),
        );
        std::fs::write(shim.join("muse"), script).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(shim.join("muse"), std::fs::Permissions::from_mode(0o755))
                .ok()?;
        }
        let old_path = std::env::var_os("PATH");
        let mut paths = vec![shim];
        paths.extend(std::env::split_paths(&old_path.clone().unwrap_or_default()));
        let joined = std::env::join_paths(paths).ok()?;
        // `ENV_LOCK` serializes every test that touches `PATH`, and the old
        // value is restored on drop.
        std::env::set_var("PATH", joined);
        Some(Self {
            root,
            home,
            xdg,
            work,
            real,
            argv_log,
            old_path,
        })
    }

    fn store_dir(&self) -> PathBuf {
        self.xdg.join("muse").join("sessions")
    }

    fn work_str(&self) -> String {
        self.work.to_string_lossy().into_owned()
    }

    fn argv_lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.argv_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

impl Drop for ScratchMuse {
    fn drop(&mut self) {
        // Same serialization as the override above.
        match self.old_path.take() {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
}

fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// The real `muse` binary, resolved before any shim dir exists on `PATH`.
fn find_real_muse() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("muse"))
        .find(|candidate| candidate.is_file())
}

/// Run the real binary with the scratch environment, killing it after
/// `timeout`. `None` on spawn failure or timeout.
fn run_muse(
    scratch: &ScratchMuse,
    args: &[&str],
    timeout: Duration,
) -> Option<std::process::Output> {
    let mut child = Command::new(&scratch.real)
        .args(args)
        .env("HOME", &scratch.home)
        .env("XDG_DATA_HOME", &scratch.xdg)
        .env("TERM", "xterm-256color")
        .current_dir(&scratch.work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
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

/// Create one real session headlessly and return its discovered id.
fn create_muse_session(scratch: &ScratchMuse, prompt: &str) -> String {
    let since = std::time::SystemTime::now();
    let out = run_muse(
        scratch,
        &["exec", "--provider", "echo", prompt],
        Duration::from_secs(120),
    )
    .expect("muse exec must complete");
    assert!(
        out.status.success(),
        "muse exec failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    discover_conversation(&scratch.store_dir(), &scratch.work_str(), since)
        .expect("the created session must be discoverable")
}

/// The session dir holding `id`, if any.
fn find_session_dir(store: &std::path::Path, id: &str) -> Option<PathBuf> {
    find_child_dir(store, id, "session.jsonl")
}

/// The live session dir holding `id` (lock present, log maybe not yet), if any.
fn find_lock_dir(store: &std::path::Path, id: &str) -> Option<PathBuf> {
    find_child_dir(store, id, ".session.lock")
}

fn find_child_dir(store: &std::path::Path, id: &str, marker: &str) -> Option<PathBuf> {
    let mut stack = vec![store.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == id) && path.join(marker).is_file() {
                    return Some(path);
                }
                stack.push(path);
            }
        }
    }
    None
}

fn poll_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    done()
}

/// A canopy data dir whose config carries the muse resume machinery.
fn write_muse_config(data_dir: &std::path::Path) {
    std::fs::write(
        data_dir.join("config.toml"),
        "[[clis]]\n\
         name = \"muse\"\n\
         binary = \"muse\"\n\
         interactive_args = \"--provider echo\"\n\
         resume_args = \"resume --last\"\n\
         session_resume_cmd = \"resume\"\n",
    )
    .expect("write test config");
}

/// A canopy data dir whose muse entry has an identity resume command but no
/// shared most-recent fallback: id-less sessions relaunch their own command
/// line, so the single-`--last` guard must not fire.
fn write_muse_config_without_resume_args(data_dir: &std::path::Path) {
    std::fs::write(
        data_dir.join("config.toml"),
        "[[clis]]\n\
         name = \"muse\"\n\
         binary = \"muse\"\n\
         interactive_args = \"--provider echo\"\n\
         session_resume_cmd = \"resume\"\n",
    )
    .expect("write test config");
}

fn insert_muse_row(db: &Database, id: &str, name: &str, workdir: &str, harness_id: Option<&str>) {
    db.insert_interactive_session(
        id,
        name,
        "muse",
        workdir,
        Some("--provider echo"),
        None,
        "interactive",
        None,
    )
    .unwrap();
    if let Some(harness_id) = harness_id {
        db.set_session_harness_conversation_id(id, harness_id)
            .unwrap();
    }
}

#[test]
fn real_muse_two_sessions_have_distinct_discoverable_ids() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(scratch) = ScratchMuse::setup() else {
        eprintln!("SKIP: no `muse` on PATH");
        return;
    };

    let id_a = create_muse_session(&scratch, "first session prompt");
    let id_b = create_muse_session(&scratch, "second session prompt");
    assert_ne!(
        id_a, id_b,
        "two sessions in one directory must have distinct harness ids"
    );
    for id in [&id_a, &id_b] {
        let dir = find_session_dir(&scratch.store_dir(), id)
            .unwrap_or_else(|| panic!("session dir for {id} must exist"));
        assert!(
            dir.starts_with(&scratch.xdg),
            "muse must honor XDG_DATA_HOME: {dir:?}"
        );
    }
}

#[test]
fn real_muse_restart_resumes_each_session_by_its_own_id() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(scratch) = ScratchMuse::setup() else {
        eprintln!("SKIP: no `muse` on PATH");
        return;
    };
    let id_a = create_muse_session(&scratch, "restart probe one");
    let id_b = create_muse_session(&scratch, "restart probe two");

    let (db, db_dir) = test_db();
    write_muse_config(db_dir.path());
    let config = CanopyConfig::load(db_dir.path());
    assert!(
        config
            .get_cli("muse")
            .is_some_and(|cli| cli.session_resume_cmd.as_deref() == Some("resume")),
        "the test config must carry the muse resume machinery"
    );
    insert_muse_row(&db, "s1", "one", &scratch.work_str(), Some(&id_a));
    insert_muse_row(&db, "s2", "two", &scratch.work_str(), Some(&id_b));

    // First restart.
    let hub = test_hub(&db);
    reconcile_at_startup(&hub, db_dir.path());

    let lines = scratch.argv_lines();
    assert!(
        lines
            .iter()
            .any(|line| line.contains(&format!("resume {id_a}"))),
        "s1 must relaunch as `resume <its own id>`: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains(&format!("resume {id_b}"))),
        "s2 must relaunch as `resume <its own id>`: {lines:?}"
    );
    assert!(
        lines.iter().all(|line| !line.contains("--last")),
        "identity resumes must never take the shared fallback: {lines:?}"
    );
    assert!(
        lines.iter().all(|line| !line.contains("session-id")),
        "no spawn may pass a session-id flag: {lines:?}"
    );
    // The built command lines spawn successfully: both relaunched
    // processes are still running seconds later.
    std::thread::sleep(Duration::from_secs(3));
    for id in ["s1", "s2"] {
        let session = hub
            .get(id)
            .unwrap_or_else(|| panic!("{id} must be relaunched"));
        assert_eq!(
            session.status(),
            crate::sessions::SessionStatus::Running,
            "{id} must stay alive after an identity resume"
        );
        assert_eq!(
            db.get_interactive_session_args(id).unwrap().as_deref(),
            Some("--provider echo"),
            "{id} keeps its original launch args, never the resume command"
        );
    }

    // Second restart: the previous run's children are gone, and the next
    // daemon finds the rows it left behind — active again with their launch
    // args untouched. Reactivate them in place instead of re-inserting
    // (which would replace the row and reset `args`), so this second
    // assertion really proves the column survives two resumes.
    hub.kill("s1");
    hub.kill("s2");
    for id in ["s1", "s2"] {
        db.reactivate_keep_args(id, None, &scratch.work_str(), None)
            .expect("reactivate the row for the second restart");
        assert_eq!(
            db.get_interactive_session_args(id).unwrap().as_deref(),
            Some("--provider echo"),
            "{id} still holds its original launch args before the second restart"
        );
    }
    let hub = test_hub(&db);
    reconcile_at_startup(&hub, db_dir.path());

    let lines = scratch.argv_lines();
    for id in [&id_a, &id_b] {
        let relaunches = lines
            .iter()
            .filter(|line| line.contains(&format!("resume {id}")))
            .count();
        assert_eq!(
            relaunches, 2,
            "two restarts relaunch {id} twice by identity: {lines:?}"
        );
    }
    for id in ["s1", "s2"] {
        assert_eq!(
            db.get_interactive_session_args(id).unwrap().as_deref(),
            Some("--provider echo"),
            "{id} keeps its original launch args across two restarts"
        );
    }
    hub.kill("s1");
    hub.kill("s2");
}

#[test]
fn real_muse_last_fallback_resumes_at_most_one_idless_session() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(scratch) = ScratchMuse::setup() else {
        eprintln!("SKIP: no `muse` on PATH");
        return;
    };
    // One saved conversation, so the single `--last` fallback has a live
    // target to land on.
    create_muse_session(&scratch, "fallback target");

    let (db, db_dir) = test_db();
    write_muse_config(db_dir.path());
    insert_muse_row(&db, "g1", "first", &scratch.work_str(), None);
    insert_muse_row(&db, "g2", "second", &scratch.work_str(), None);

    let hub = test_hub(&db);
    reconcile_at_startup(&hub, db_dir.path());

    let lines = scratch.argv_lines();
    assert_eq!(
        lines.len(),
        1,
        "exactly one id-less session may take the shared fallback: {lines:?}"
    );
    assert!(
        lines[0].contains("resume --last"),
        "the fallback is `resume --last`: {lines:?}"
    );
    let survivor = ["g1", "g2"]
        .into_iter()
        .find(|id| hub.get(id).is_some())
        .expect("one session is resumed");
    let loser = if survivor == "g1" { "g2" } else { "g1" };
    assert_eq!(
        db.get_interactive_session_status(loser).unwrap().as_deref(),
        Some("error"),
        "the guarded session fails instead of silently colliding"
    );
    let detail = db
        .get_session_exit_detail(loser)
        .unwrap()
        .unwrap_or_default();
    assert!(
        detail.contains("no recorded harness conversation id"),
        "the guarded session explains itself: {detail:?}"
    );
    hub.kill(survivor);
}

#[test]
fn real_muse_interactive_spawn_records_its_conversation_id() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(scratch) = ScratchMuse::setup() else {
        eprintln!("SKIP: no `muse` on PATH");
        return;
    };
    let (db, db_dir) = test_db();
    write_muse_config(db_dir.path());
    let hub = test_hub(&db);

    // A fresh interactive spawn through the hub (which records on its own),
    // plus an explicit recorder run: the no-overwrite setter makes the
    // second recording of the same id a no-op.
    let since = std::time::SystemTime::now();
    let mut req = agent_spawn("muse", &scratch.work_str());
    req.interactive_args = Some("--provider echo".to_string());
    req.id = Some("rec-1".to_string());
    req.name = Some("recorded".to_string());
    let (session, _guard) = KillGuard::spawn(&hub, &req);
    let pid = session.pid.expect("the spawned child has a pid");
    record_fresh_conversation(
        db.clone(),
        "rec-1".to_string(),
        scratch.work_str(),
        since,
        Some(pid),
        Some(scratch.store_dir()),
    );

    assert!(
        poll_until(Duration::from_secs(30), || {
            db.get_interactive_session("rec-1")
                .ok()
                .flatten()
                .and_then(|row| row.harness_conversation_id)
                .is_some()
        }),
        "the fresh interactive spawn must record its harness conversation id"
    );
    let recorded = db
        .get_interactive_session("rec-1")
        .unwrap()
        .expect("row")
        .harness_conversation_id
        .expect("recorded id");
    // The recorded id is the live lock's uuid for this child — exact, never
    // the directory's newest conversation by luck.
    let live = discover_conversation_by_pid(&scratch.store_dir(), pid, since)
        .expect("the child's lock names its conversation");
    assert_eq!(recorded, live);
    let dir = find_lock_dir(&scratch.store_dir(), &recorded)
        .unwrap_or_else(|| panic!("live session dir for {recorded} must exist"));
    assert!(
        dir.starts_with(&scratch.xdg),
        "the session must live under the scratch store: {dir:?}"
    );

    // Restart: the row relaunches by its recorded identity, never `--last`.
    hub.kill("rec-1");
    assert!(
        find_session_dir(&scratch.store_dir(), &recorded).is_none(),
        "a never-prompted session leaves no saved log behind"
    );
    insert_muse_row(
        &db,
        "rec-1",
        "recorded",
        &scratch.work_str(),
        Some(&recorded),
    );
    let hub = test_hub(&db);
    reconcile_at_startup(&hub, db_dir.path());

    let lines = scratch.argv_lines();
    assert!(
        lines
            .iter()
            .any(|line| line.contains(&format!("resume {recorded}"))),
        "the restart must relaunch as `resume <the recorded id>`: {lines:?}"
    );
    assert!(
        lines.iter().all(|line| !line.contains("--last")),
        "an identity resume must never take the shared fallback: {lines:?}"
    );
    // Nothing was ever prompted, so the identity resume fails fast with the
    // harness's own explanation attached — an honest error, not silence and
    // not a collision with another session.
    assert!(
        poll_until(Duration::from_secs(90), || {
            db.get_interactive_session_status("rec-1")
                .unwrap_or(None)
                .as_deref()
                == Some("error")
        }),
        "the unresumable identity must surface as an error row"
    );
    let detail = db
        .get_session_exit_detail("rec-1")
        .unwrap()
        .unwrap_or_default();
    assert!(
        detail.contains("no saved log"),
        "the harness's stderr tail is attached to the row: {detail:?}"
    );
    hub.kill("rec-1");
}

/// Restores `PATH` when dropped; the caller holds [`ENV_LOCK`].
struct PathGuard(Option<OsString>);

impl PathGuard {
    fn prepend(dir: &std::path::Path) -> Self {
        let old = std::env::var_os("PATH");
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&old.clone().unwrap_or_default()));
        std::env::set_var("PATH", std::env::join_paths(paths).expect("join PATH"));
        Self(old)
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
}

/// A hub spawn of a fresh muse session records its conversation id on its
/// own — the path a remote or scheduled spawn takes, with no TUI launcher
/// to do it — and finds it under the real `YYYY/MM/DD` layout even though
/// the year and month partitions carry months-old mtimes.
#[cfg(unix)]
#[test]
fn hub_spawn_records_a_fresh_muse_conversation() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = tempfile::tempdir().expect("scratch root");
    let store = root.path().join("store");
    let today = chrono::Utc::now().date_naive();
    let month = store
        .join(today.format("%Y").to_string())
        .join(today.format("%m").to_string());
    let day = month.join(today.format("%d").to_string());
    std::fs::create_dir_all(&day).expect("day partition");
    let work = root.path().join("work");
    let shim = root.path().join("shim");
    std::fs::create_dir_all(&work).expect("workdir");
    std::fs::create_dir_all(&shim).expect("shim dir");
    // A fake muse: write the live lock for its own pid, then idle.
    let conv = day.join("uuid-hub-fresh");
    std::fs::write(
        shim.join("muse"),
        format!(
            "#!/bin/sh\nmkdir -p {dir}\nprintf 'pid=%s\\nhost=TEST\\n' \"$$\" > {dir}/.session.lock\nexec sleep 30\n",
            dir = shell_quote(&conv),
        ),
    )
    .expect("fake muse");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(shim.join("muse"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake muse");
    }
    let old = std::time::SystemTime::now() - Duration::from_secs(90 * 24 * 3600);
    for dir in [&month, month.parent().expect("year")] {
        std::fs::File::open(dir)
            .and_then(|dir| dir.set_modified(old))
            .expect("age partition");
    }
    let _path = PathGuard::prepend(&shim);

    let (db, _db_dir) = test_db();
    let hub = test_hub(&db);
    hub.set_muse_store_dir(store.clone());
    let mut req = agent_spawn("muse", &work.to_string_lossy());
    req.interactive_args = Some("--provider echo".to_string());
    req.id = Some("hub-fresh".to_string());
    let (_session, _guard) = KillGuard::spawn(&hub, &req);

    assert!(
        poll_until(Duration::from_secs(10), || {
            db.get_interactive_session("hub-fresh")
                .ok()
                .flatten()
                .and_then(|row| row.harness_conversation_id)
                .as_deref()
                == Some("uuid-hub-fresh")
        }),
        "the hub must record the fresh muse session's conversation id"
    );
}

#[test]
fn hub_records_only_fresh_muse_agent_spawns() {
    use crate::sessions::muse_conversation::hub_spawn_records_conversation;
    let fresh = agent_spawn("muse", "/work");
    assert!(hub_spawn_records_conversation(&fresh));

    let mut resume = agent_spawn("muse", "/work");
    resume.preserve_args = true;
    assert!(!hub_spawn_records_conversation(&resume));

    let mut known = agent_spawn("muse", "/work");
    known.harness_conversation_id = Some("uuid-known".to_string());
    assert!(!hub_spawn_records_conversation(&known));

    assert!(!hub_spawn_records_conversation(&agent_spawn(
        "claude", "/work"
    )));
    let mut terminal = agent_spawn("muse", "/work");
    terminal.kind = SessionKind::Terminal;
    assert!(!hub_spawn_records_conversation(&terminal));
}

#[test]
fn real_muse_cmd_without_fallback_resumes_every_idless_session() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(scratch) = ScratchMuse::setup() else {
        eprintln!("SKIP: no `muse` on PATH");
        return;
    };
    let (db, db_dir) = test_db();
    write_muse_config_without_resume_args(db_dir.path());
    insert_muse_row(&db, "c1", "first", &scratch.work_str(), None);
    insert_muse_row(&db, "c2", "second", &scratch.work_str(), None);

    let hub = test_hub(&db);
    reconcile_at_startup(&hub, db_dir.path());

    // No shared fallback exists, so the single-`--last` guard must not fire:
    // both sessions relaunch their own command line.
    for id in ["c1", "c2"] {
        assert!(
            hub.get(id).is_some(),
            "{id} must be relaunched, not guard-failed"
        );
        assert_eq!(
            db.get_interactive_session_status(id).unwrap().as_deref(),
            Some("active"),
            "{id} stays active"
        );
    }
    let lines = scratch.argv_lines();
    assert_eq!(lines.len(), 2, "both sessions spawn: {lines:?}");
    assert!(
        lines.iter().all(|line| !line.contains("resume")),
        "without resume machinery no resume is built: {lines:?}"
    );
    hub.kill("c1");
    hub.kill("c2");
}

#[test]
fn real_muse_failed_resume_records_error_with_tail() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(scratch) = ScratchMuse::setup() else {
        eprintln!("SKIP: no `muse` on PATH");
        return;
    };

    let (db, db_dir) = test_db();
    write_muse_config(db_dir.path());
    let bogus = "00000000-0000-0000-0000-000000000000";
    insert_muse_row(&db, "f1", "doomed", &scratch.work_str(), Some(bogus));

    let hub = test_hub(&db);
    reconcile_at_startup(&hub, db_dir.path());

    let lines = scratch.argv_lines();
    assert!(
        lines
            .iter()
            .any(|line| line.contains(&format!("resume {bogus}"))),
        "the resume is attempted by identity: {lines:?}"
    );
    assert!(
        poll_until(Duration::from_secs(90), || {
            db.get_interactive_session_status("f1")
                .unwrap_or(None)
                .as_deref()
                == Some("error")
        }),
        "the failed resume must surface as an error row"
    );
    let detail = db
        .get_session_exit_detail("f1")
        .unwrap()
        .unwrap_or_default();
    assert!(
        detail.contains("no saved log"),
        "the harness's stderr tail is attached to the row: {detail:?}"
    );
    assert!(hub.get("f1").is_none(), "the dead session leaves the hub");
}
