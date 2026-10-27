//! Graph-engine test fixtures (a) (split from `src/graph_engine/mod.rs`, pure move).
use super::super::*;
use crate::application::notification_service::DefaultNotificationService;
use tempfile::{tempdir, TempDir};

/// CB57: shared by `graph_fixture`/`bare_graph_fixture` and every call site
/// that needs to hand a fake member script its own DB path for
/// self-reporting, so the filename can't drift between the two.
const TEST_DB_FILENAME: &str = "test.db";

/// CB57 (FR5): the test binary itself — the executable fake member
/// scripts re-execute to run `self_report_verdict_writer` and file
/// their own verdicts. `std::env::current_exe()` is that binary under
/// both `cargo test` and `cargo nextest run` (nextest invokes the same
/// compiled test executables), and it always contains the tests
/// module. This replaces the separate `cm_verdict_writer` binary: a
/// second `[[bin]]` would ship with `cargo install harness-canopy`
/// and put a program that writes verdicts straight into a canopy
/// database on every user's PATH (spec: no new binary target).
fn self_report_exe() -> String {
    std::env::current_exe()
        .expect("path of the running test binary")
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn graph_fixture() -> Result<(TempDir, Arc<Database>, GraphEngine, String, String)> {
    let dir = tempdir()?;
    let db = Arc::new(Database::new(&dir.path().join(TEST_DB_FILENAME))?);
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-test".to_string(),
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
    };
    let spec = crate::domain::graphs::GraphSpec {
            id: "spec-test".to_string(),
            graph_id: Some(lp.id.clone()),
            name: "Spec".to_string(),
            description: Some(
                "Functional Requirements:\n- A\n\nNon-Functional Requirements:\n- B\n\nObjective:\n- C\n\nConstraints:\n- D\n\nGuidelines:\n- E\n\nIn Scope:\n- F\n\nOut of Scope:\n- G".to_string(),
            ),
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
        };

    db.insert_graph(&lp)?;
    db.insert_graph_spec(&spec)?;

    Ok((
        dir,
        Arc::clone(&db),
        GraphEngine::new(db, Arc::new(DefaultNotificationService)),
        lp.id,
        spec.id,
    ))
}

#[derive(Debug, Clone, PartialEq)]
enum RecordedNotification {
    GraphStarted {
        graph_name: String,
        spec_count: usize,
        resumed: bool,
        first_pending: Option<String>,
    },
    SpecCompleted {
        graph_name: String,
        spec_name: String,
        done: usize,
        total: usize,
        next_pending: Option<String>,
    },
    GraphFinishedCompleted {
        graph_name: String,
        done: usize,
        total: usize,
        hook_launched: bool,
    },
    GraphFinishedFailed {
        graph_name: String,
        spec_name: String,
    },
    GraphFinishedBlocked {
        graph_name: String,
        summary: String,
    },
    CompletionHookFailed {
        graph_name: String,
        error: String,
    },
}

#[derive(Default)]
struct MockNotificationService {
    events: std::sync::Mutex<Vec<RecordedNotification>>,
}

impl MockNotificationService {
    fn events(&self) -> Vec<RecordedNotification> {
        self.events.lock().unwrap().clone()
    }
}

impl NotificationService for MockNotificationService {
    fn notify_task_completed(&self, _task_id: &str, _success: bool, _exit_code: Option<i32>) {}
    fn notify_task_failed(&self, _task_id: &str, _exit_code: i32, _error_msg: &str) {}
    fn notify_watcher_triggered(&self, _watcher_id: &str, _path: &str, _event: &str) {}
    fn notify_agent_failed(&self, _agent_id: &str, _cli: &str, _exit_code: i32, _output: &str) {}
    fn notify_nursery_failed(&self, _error_msg: &str) {}

    fn notify_graph_started(
        &self,
        graph_name: &str,
        spec_count: usize,
        resumed: bool,
        first_pending: Option<&str>,
    ) {
        self.events
            .lock()
            .unwrap()
            .push(RecordedNotification::GraphStarted {
                graph_name: graph_name.to_string(),
                spec_count,
                resumed,
                first_pending: first_pending.map(str::to_string),
            });
    }

    fn notify_spec_completed(
        &self,
        graph_name: &str,
        spec_name: &str,
        done: usize,
        total: usize,
        next_pending: Option<&str>,
    ) {
        self.events
            .lock()
            .unwrap()
            .push(RecordedNotification::SpecCompleted {
                graph_name: graph_name.to_string(),
                spec_name: spec_name.to_string(),
                done,
                total,
                next_pending: next_pending.map(str::to_string),
            });
    }

    fn notify_graph_finished(&self, graph_name: &str, outcome: GraphFinishOutcome<'_>) {
        let event = match outcome {
            GraphFinishOutcome::Completed {
                done,
                total,
                hook_launched,
            } => RecordedNotification::GraphFinishedCompleted {
                graph_name: graph_name.to_string(),
                done,
                total,
                hook_launched,
            },
            GraphFinishOutcome::Failed { spec_name } => RecordedNotification::GraphFinishedFailed {
                graph_name: graph_name.to_string(),
                spec_name: spec_name.to_string(),
            },
            GraphFinishOutcome::Blocked { summary } => RecordedNotification::GraphFinishedBlocked {
                graph_name: graph_name.to_string(),
                summary: summary.to_string(),
            },
        };
        self.events.lock().unwrap().push(event);
    }

    fn notify_graph_completion_hook_failed(&self, graph_name: &str, error: &str) {
        self.events
            .lock()
            .unwrap()
            .push(RecordedNotification::CompletionHookFailed {
                graph_name: graph_name.to_string(),
                error: error.to_string(),
            });
    }

    fn notify_announcement(&self, _title: &str, _body: &str) {}
}

type MockGraphFixture = (
    TempDir,
    Arc<Database>,
    GraphEngine,
    Arc<MockNotificationService>,
    String,
    String,
);

fn graph_fixture_with_mock() -> Result<MockGraphFixture> {
    let dir = tempdir()?;
    let db = Arc::new(Database::new(&dir.path().join("test.db"))?);
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-test".to_string(),
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
    };
    let spec = crate::domain::graphs::GraphSpec {
        id: "spec-test".to_string(),
        graph_id: Some(lp.id.clone()),
        name: "Spec".to_string(),
        description: Some("Objective:\n- test".to_string()),
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
    };

    db.insert_graph(&lp)?;
    db.insert_graph_spec(&spec)?;

    let notifications = Arc::new(MockNotificationService::default());
    Ok((
        dir,
        Arc::clone(&db),
        GraphEngine::new(
            db,
            Arc::clone(&notifications) as Arc<dyn NotificationService>,
        ),
        notifications,
        lp.id,
        spec.id,
    ))
}

fn init_git_repo(path: &std::path::Path) {
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .expect("git command failed to run");
        assert!(status.success(), "git {:?} failed", args);
    };
    run(&["init", "-q"]);
    // Repo-local identity so a node that shells out to `git commit`
    // works regardless of the machine's global git config.
    run(&["config", "user.name", "Test"]);
    run(&["config", "user.email", "test@example.com"]);
    std::fs::write(path.join("README.md"), "test").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "init"]);
}

fn git_head(path: &std::path::Path) -> String {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .expect("git rev-parse failed to run");
    assert!(output.status.success(), "git rev-parse HEAD failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// An empty commit at a fixed author/committer date, so a test can
/// assert on both the commit list's contents and its rendering.
fn empty_commit(path: &std::path::Path, subject: &str, date: &str) {
    let status = std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-q", "-m", subject])
        .current_dir(path)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .status()
        .expect("git commit failed to run");
    assert!(status.success(), "git commit --allow-empty failed");
}

/// The `<short hash> <subject> (<author date>)` line git should render
/// for `rev`, built from the same `git log` fields the blocker's commit
/// listing uses.
fn commit_line(path: &std::path::Path, rev: &str, subject: &str) -> String {
    let log = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git log failed to run");
        assert!(output.status.success(), "git log {:?} failed", args);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    let hash = log(&["log", "-1", "--format=%h", rev]);
    let date = log(&["log", "-1", "--format=%ad", "--date=short", rev]);
    format!("{hash} {subject} ({date})")
}

/// A spec not bound to `graph_id` at all, tagged with its own `workdir`
/// (the shape `find_dirty_predecessor_spec` also matches) — the minimal
/// "previous spec" fixture the `dirty_start_check` tests need, without
/// having to actually run anything through it.
fn dirty_predecessor_spec(id: &str, workdir: &str, status: GraphSpecStatus) -> GraphSpec {
    GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: "Predecessor".to_string(),
        description: None,
        position: 1,
        parallelizable: false,
        status,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: Some(workdir.to_string()),
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    }
}

fn dirty_predecessor_spec_named(
    id: &str,
    name: &str,
    workdir: &str,
    status: GraphSpecStatus,
) -> GraphSpec {
    let mut spec = dirty_predecessor_spec(id, workdir, status);
    spec.name = name.to_string();
    spec
}

/// CB69: insert a top-level Check node under `graph_id` plus one finished
/// (`Pass`) `graph_runs` row for `spec_id` started at `started_secs`
/// (epoch seconds — `insert_graph_run` stores `started_at.timestamp()`). The
/// run row is what `find_last_dirty_run_spec` joins. `graph_nodes` has a
/// UNIQUE index on (graph_id, position) (`idx_graph_nodes_graph_position`
/// in `src/db/mod.rs`) — pass a distinct `position` per node in a test.
pub(crate) fn insert_run_row(
    db: &Database,
    graph_id: &str,
    spec_id: &str,
    node_id: &str,
    position: i64,
    started_secs: i64,
) -> Result<()> {
    db.insert_graph_node(&GraphNode {
        id: node_id.to_string(),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        name: format!("Node {node_id}"),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({}),
        position,
        created_at: chrono::Utc::now(),
    })?;
    db.insert_graph_run(&GraphNodeRun {
        id: format!("run-{spec_id}-{started_secs}"),
        graph_id: graph_id.to_string(),
        spec_id: spec_id.to_string(),
        node_id: node_id.to_string(),
        status: GraphRunStatus::Pass,
        input: None,
        output: None,
        started_at: chrono::DateTime::from_timestamp(started_secs, 0).unwrap(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })?;
    Ok(())
}

// ── B37: node-level commit rights, enforced by the engine ────────────

/// A node whose command runs in the workdir. `commit_rights` is attached
/// verbatim when `Some`, and omitted entirely when `None` — the two cases
/// that decide whether the graph opts into enforcement at all.
fn rights_node(
    spec_id: &str,
    id: &str,
    command: &str,
    commit_rights: Option<bool>,
    position: i64,
) -> GraphNode {
    let mut config = serde_json::json!({
        "command": command,
        "success_condition": "exit_code_0"
    });
    if let Some(rights) = commit_rights {
        config["commit_rights"] = serde_json::json!(rights);
    }
    GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Check,
        config,
        position,
        created_at: chrono::Utc::now(),
    }
}

const COMMIT_CMD: &str = "git commit -q --allow-empty -m 'unauthorized'";

// ── C15: spec_committed_head — tied to *this run's* committer, not to
// "any commit since spec_start_head" ──────────────────────────────────

/// The canonical stronger check template: clean tree, *and* this run's
/// own committer actually produced a commit, *and* HEAD still is that
/// exact commit (nothing landed on top of it unaccounted for).
const COMMITTED_CHECK_CMD: &str = "test -z \"$(git status --porcelain -- src/)\" \
         && test -n \"{{spec_committed_head}}\" \
         && test \"$(git rev-parse HEAD)\" = \"{{spec_committed_head}}\"";

fn agent_node_with_config(config: Value) -> GraphNode {
    GraphNode {
        id: "node-1".to_string(),
        spec_id: Some("spec".to_string()),
        graph_id: None,
        name: "Agent".to_string(),
        kind: GraphNodeKind::Agent,
        config,
        position: 1,
        created_at: chrono::Utc::now(),
    }
}

/// Build a local git repo (no real network — a local path is a valid git
/// remote) with one directory per `(name, body)` skill, each holding a
/// minimal `SKILL.md`. Mirrors `dynamic_skills`'s own test fixtures.
fn make_skill_registry(dir: &std::path::Path, skills: &[(&str, &str)]) {
    use std::process::Command;
    Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "test"])
        .current_dir(dir)
        .status()
        .unwrap();
    for (name, body) in skills {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: \"{name} skill\"\n---\n# {name}\n{body}\n"),
        )
        .unwrap();
    }
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-q", "-m", "init"])
        .current_dir(dir)
        .status()
        .unwrap();
}

fn skill_store_for(
    registry: &std::path::Path,
    store_dir: &std::path::Path,
) -> crate::dynamic_skills::SkillStore {
    crate::dynamic_skills::SkillStore::new(
        store_dir.to_path_buf(),
        vec![crate::dynamic_skills::GitSource::new(
            registry.to_string_lossy().to_string(),
            None,
        )],
        15,
    )
}

fn sample_agent_node() -> GraphNode {
    GraphNode {
        id: "node-agent".to_string(),
        spec_id: Some("spec".to_string()),
        graph_id: None,
        name: "Agent".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    }
}

/// A throwaway `Database` for `run_agent_process` tests that only need
/// somewhere to (harmlessly) persist a pid — no graph/spec/node rows are
/// inserted, so `set_graph_run_pid`/`update_graph_run_result` against the
/// fake `run_id` below just affect zero rows.
fn test_db() -> (TempDir, Database) {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    (dir, db)
}

fn sample_strategy(binary: &str) -> crate::domain::cli_strategy::CliStrategy {
    crate::domain::cli_strategy::CliStrategy {
        binary: binary.to_string(),
        headless_mode: String::new(),
        model_flag: None,
        supports_working_dir: false,
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
        invocation_template: None,
        effort_declaration: None,
        decision_args: None,
        identity: None,
    }
}

/// Writes a fake session-aware CLI as an executable shell script with two
/// modes dispatched on its first arg. `list` prints the ids in
/// `$STATEFILE` as opencode-family JSON (`[{"id":"..."}]`), or exits 1
/// when `$FAIL_LIST` is set. `run` is the agent invocation (headless mode
/// is `run`); it appends `$APPEND_ID` to `$STATEFILE` when set (simulating
/// the CLI creating a new session), then exits 0. This lets one binary
/// serve as both the agent process and the session list the capture diffs
/// — exactly how the real CLIs behave.
fn write_fake_session_cli(dir: &std::path::Path) -> std::path::PathBuf {
    let script = dir.join("fake-session-cli");
    std::fs::write(
        &script,
        r#"#!/bin/sh
case "$1" in
  list)
    [ -n "$FAIL_LIST" ] && exit 1
    printf '['
    sep=""
    if [ -f "$STATEFILE" ]; then
      while IFS= read -r line; do
        [ -z "$line" ] && continue
        printf '%s{"id":"%s"}' "$sep" "$line"
        sep=","
      done < "$STATEFILE"
    fi
    printf ']\n'
    ;;
  run)
    [ -n "$APPEND_ID" ] && echo "$APPEND_ID" >> "$STATEFILE"
    echo done
    ;;
esac
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// Strategy for the fake session CLI: `run` headless mode, `list` session
/// command, opencode-family id pattern. `env` carries the fixture's
/// `STATEFILE`/`APPEND_ID`/`FAIL_LIST` toggles to both invocations.
fn fake_session_strategy(
    binary: &std::path::Path,
    env: HashMap<String, String>,
) -> crate::domain::cli_strategy::CliStrategy {
    crate::domain::cli_strategy::CliStrategy {
        binary: binary.to_string_lossy().to_string(),
        headless_mode: "run".to_string(),
        model_flag: None,
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: env,
        prompt_via_stdin: false,
        session_id_set_flag: None,
        session_list_cmd: Some("list".to_string()),
        session_list_format_args: None,
        session_id_pattern: Some(r#""id"\s*:\s*"([^"]+)""#.to_string()),
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

// ── RS2: resume on fail-edge bounce ─────────────────────────────────

/// Fake CLI that records its full argv (one arg per line, `===` between
/// invocations) to `$ARGV_FILE`. When resuming (its argv contains
/// `$RESUME_FLAG`) and `$FAIL_RESUME` is set, it exits nonzero at once to
/// simulate a rejected resume flag; otherwise it prints `done` and exits 0.
fn write_argv_echo_cli(dir: &std::path::Path) -> std::path::PathBuf {
    let script = dir.join("argv-echo-cli");
    std::fs::write(
            &script,
            r#"#!/bin/sh
{
  for a in "$@"; do printf '%s\n' "$a"; done
  printf '===\n'
} >> "$ARGV_FILE"
is_resume=0
for a in "$@"; do [ "$a" = "$RESUME_FLAG" ] && is_resume=1; done
if [ "$is_resume" = "1" ] && [ -n "$FAIL_RESUME" ]; then
  echo "resume rejected" >&2
  exit 1
fi
# CM13 test support: linger, historically so a background verdict filer
# had time to land its write. Unset (the default) keeps the instant
# behavior every other test relies on.
if [ -n "$LINGER_SECONDS" ]; then
  sleep "$LINGER_SECONDS"
fi
# CB57 test support: when SELF_REPORT_NODE_ID is set, file this run's own
# self-report verdict before exiting (replaces the old background-poller
# verdict filer). Unset (the default) keeps every other test's "never
# reports" behavior unchanged.
if [ -n "$SELF_REPORT_NODE_ID" ]; then
  CANOPY_TEST_SELF_REPORT_DB="$SELF_REPORT_DB" \
  CANOPY_TEST_SELF_REPORT_NODE_ID="$SELF_REPORT_NODE_ID" \
  CANOPY_TEST_SELF_REPORT_STATUS="${SELF_REPORT_STATUS:-pass}" \
  CANOPY_TEST_SELF_REPORT_STDOUT="$SELF_REPORT_STDOUT" \
    "$SELF_REPORT_EXE" --exact graph_engine::tests::fixtures_a::fixtures_b::fixtures_c::part10_tests::self_report_verdict_writer --nocapture >/dev/null
fi
echo done
"#,
        )
        .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// Write a `~/.canopy/config.toml` fixture holding a single CLI named
/// `resume-cli` backed by the argv-echo script, so `Cli::strategy()`
/// resolves it under a [`HomeGuard`].
fn write_resume_cli_home(cli: crate::domain::cli_config::CliConfig) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        configured_at: Some(chrono::Utc::now().to_rfc3339()),
        clis: vec![cli],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    home
}

/// Build the `resume-cli` [`CliConfig`] for the argv-echo binary.
fn argv_cli_config(
    binary: &std::path::Path,
    env: HashMap<String, String>,
    resume: Option<&str>,
    set_flag: Option<&str>,
    list_cmd: Option<&str>,
) -> crate::domain::cli_config::CliConfig {
    crate::domain::cli_config::CliConfig {
        name: "resume-cli".into(),
        binary: binary.to_string_lossy().into_owned(),
        headless_mode: "run".into(),
        env_vars: env,
        session_resume_cmd: resume.map(str::to_string),
        session_id_set_flag: set_flag.map(str::to_string),
        session_list_cmd: list_cmd.map(str::to_string),
        session_list_format_args: list_cmd.map(|_| "--format json".to_string()),
        session_id_pattern: list_cmd.map(|_| r#""id"\s*:\s*"([^"]+)""#.to_string()),
        ..Default::default()
    }
}

/// An agent node driven by the `resume-cli` platform, with optional extra
/// config keys merged in (e.g. `{"resume": false}`).
fn resume_agent_node(extra: Value) -> GraphNode {
    let mut config = serde_json::json!({ "platform": "resume-cli" });
    if let Value::Object(extra) = extra {
        for (k, v) in extra {
            config[k] = v;
        }
    }
    GraphNode {
        id: "node-impl".to_string(),
        spec_id: Some("sid-spec".to_string()),
        graph_id: None,
        name: "impl".to_string(),
        kind: GraphNodeKind::Agent,
        config,
        position: 1,
        created_at: chrono::Utc::now(),
    }
}

fn impl_session(db: &Database, spec_id: &str) -> Option<String> {
    db.list_graph_runs_for_spec(spec_id)
        .unwrap()
        .into_iter()
        .find(|r| r.node_id == "node-impl")
        .and_then(|r| r.session_id)
}

fn ensemble_details_fixture(
    ensemble_id: &str,
    join_node_id: &str,
    member_node_ids: &[&str],
) -> crate::domain::graphs::EnsembleDetails {
    crate::domain::graphs::EnsembleDetails {
        ensemble: crate::domain::graphs::Ensemble {
            mutex: Vec::new(),
            commit_rights: false,
            id: ensemble_id.to_string(),
            spec_id: Some("spec".to_string()),
            graph_id: None,
            name: "Proposers".to_string(),
            prompt_template: "{{spec_content}}".to_string(),
            join_node_id: join_node_id.to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: crate::domain::graphs::GraphEdgeCondition::Always,
            min_pass: member_node_ids.len() as i64,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: crate::domain::graphs::EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: chrono::Utc::now(),

            crew_id: None,
            crew_name: None,
            crew_version_applied: None,
        },
        members: member_node_ids
            .iter()
            .enumerate()
            .map(|(i, node_id)| crate::domain::graphs::EnsembleMember {
                ensemble_id: ensemble_id.to_string(),
                node_id: node_id.to_string(),
                position: i as i64,
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            })
            .collect(),
    }
}

fn second_spec(graph_id: &str, id: &str, position: i64) -> GraphSpec {
    GraphSpec {
            id: id.to_string(),
            graph_id: Some(graph_id.to_string()),
            name: format!("Spec {id}"),
            description: Some(
                "Functional Requirements:\n- A\n\nNon-Functional Requirements:\n- B\n\nObjective:\n- C\n\nConstraints:\n- D\n\nGuidelines:\n- E\n\nIn Scope:\n- F\n\nOut of Scope:\n- G".to_string(),
            ),
            position,
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
        }
}

// ── R5: `graph_run` with a queue ──────────────────────────────────────

/// A graph with no bound specs — the queue's own standalone specs supply
/// the work instead. Distinct from [`graph_fixture`], which always seeds
/// one bound spec.
fn bare_graph_fixture() -> Result<(TempDir, Arc<Database>, GraphEngine, String)> {
    let dir = tempdir()?;
    let db = Arc::new(Database::new(&dir.path().join(TEST_DB_FILENAME))?);
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-test".to_string(),
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
    };
    db.insert_graph(&lp)?;
    Ok((
        dir,
        Arc::clone(&db),
        GraphEngine::new(db, Arc::new(DefaultNotificationService)),
        lp.id,
    ))
}

/// A standalone spec (`graph_id: None`), the shape queue members take —
/// queue membership never binds the spec to a graph.
fn standalone_spec(id: &str, position: i64) -> GraphSpec {
    GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: id.to_string(),
            description: Some(
                "Functional Requirements:\n- A\n\nNon-Functional Requirements:\n- B\n\nObjective:\n- C\n\nConstraints:\n- D\n\nGuidelines:\n- E\n\nIn Scope:\n- F\n\nOut of Scope:\n- G".to_string(),
            ),
            position,
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
        }
}

/// Seed a spec + agent node + `running` run under `graph_id` so
/// `run_agent_process` tests can read the run row back (`graph_runs`
/// enforces foreign keys). Returns the inserted node.
fn seed_agent_run(db: &Database, graph_id: &str, run_id: &str) -> GraphNode {
    let spec = standalone_spec("sid-spec", 1);
    db.insert_graph_spec(&spec).unwrap();
    let mut node = sample_agent_node();
    node.spec_id = Some(spec.id.clone());
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: run_id.to_string(),
        graph_id: graph_id.to_string(),
        spec_id: spec.id,
        node_id: node.id.clone(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();
    node
}

/// Drive `execute_agent_node` once against the argv-echo CLI, returning the
/// node execution, the (re-read) run row, and the recorded argv text.
async fn run_resume_agent_node(
    node_extra: Value,
    resume_session_id: Option<&str>,
    cross_spec: bool,
    set_flag: Option<&str>,
    list_cmd: Option<&str>,
    fail_resume: bool,
) -> (NodeExecution, GraphNodeRun, String) {
    let (dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let argv_file = dir.path().join("argv.log");
    let script = write_argv_echo_cli(dir.path());
    let mut env = HashMap::new();
    env.insert(
        "ARGV_FILE".to_string(),
        argv_file.to_string_lossy().into_owned(),
    );
    env.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    if fail_resume {
        env.insert("FAIL_RESUME".to_string(), "1".to_string());
    }
    let cli = argv_cli_config(&script, env, Some("--resume"), set_flag, list_cmd);
    let home = write_resume_cli_home(cli);
    let node = resume_agent_node(node_extra);

    // Acquire the HomeGuard lock BEFORE seeding the run row: the
    // resume-failure fallback compares the run's age against
    // `infra_crash_max_seconds`, so `started_at` must be stamped right
    // before the spawn. Seeding first and then blocking on the (shared,
    // serialized) HomeGuard under a loaded full suite could otherwise
    // inflate the measured age past the window and defeat the fallback.
    let guard = HomeGuard::set(home.path());
    seed_agent_run(&db, &graph_id, "run-r");
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let spec = db.get_graph_spec("sid-spec").unwrap().unwrap();
    let execution = execute_agent_node(
        &db,
        &lp,
        &spec,
        &node,
        None,
        "run-r",
        dir.path().to_str().unwrap(),
        resume_session_id,
        cross_spec,
        None,
        &HashMap::new(),
        &[],
    )
    .await
    .unwrap();
    drop(guard);

    let run = db.get_graph_run("run-r").unwrap().unwrap();
    let argv = std::fs::read_to_string(&argv_file).unwrap_or_default();
    (execution, run, argv)
}

fn insert_queue_with_members(db: &Database, queue_id: &str, member_ids: &[&str]) {
    db.insert_queue(&crate::domain::queues::Queue {
        id: queue_id.to_string(),
        name: queue_id.to_string(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    for spec_id in member_ids {
        db.append_queue_member(queue_id, spec_id, None).unwrap();
    }
}

/// RS3 variant of [`insert_queue_with_members`]: each member is `(spec_id,
/// group_name)`, so a test can queue grouped and ungrouped members side by
/// side.
fn insert_queue_with_grouped_members(
    db: &Database,
    queue_id: &str,
    members: &[(&str, Option<&str>)],
) {
    db.insert_queue(&crate::domain::queues::Queue {
        id: queue_id.to_string(),
        name: queue_id.to_string(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    for (spec_id, group) in members {
        db.append_queue_member(queue_id, spec_id, *group).unwrap();
    }
}

/// FR2 fixture: a queue whose first member `failed` on the graph's worktree
/// (dirty when `dirty`), followed by a pending `next` member that a passing
/// check node completes.
fn dirty_queue_fixture(
    dirty: bool,
    allow_dirty_start: bool,
) -> Result<(TempDir, Arc<Database>, GraphEngine, String, String)> {
    let (dir, db, engine, graph_id) = bare_graph_fixture()?;
    // The fixture's database lives in the worktree: ignore it before the
    // repo commits, or every write to it reads as a dirty tree.
    std::fs::write(
        dir.path().join(".gitignore"),
        format!("{TEST_DB_FILENAME}*\n"),
    )?;
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();
    db.insert_graph_spec(&dirty_predecessor_spec(
        "pred-a",
        &workdir,
        GraphSpecStatus::Failed,
    ))?;
    let next = standalone_spec("queue-next", 2);
    db.insert_graph_spec(&next)?;
    insert_queue_with_members(&db, "queue-1", &["pred-a", &next.id]);
    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })?;
    db.update_graph_allow_dirty_start(&graph_id, allow_dirty_start)?;
    if dirty {
        std::fs::write(dir.path().join("x.txt"), "x")?;
    }
    Ok((dir, db, engine, graph_id, next.id))
}

/// Seed the graph-level `graph-check` node (a passing check) and one
/// finished run of it for `spec_id`, as a spec that stopped on that node
/// leaves behind.
fn seed_stopped_at_check_node(db: &Database, graph_id: &str, spec_id: &str) {
    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: format!("run-{spec_id}-seed"),
        graph_id: graph_id.to_string(),
        spec_id: spec_id.to_string(),
        node_id: "graph-check".to_string(),
        status: GraphRunStatus::Fail,
        input: None,
        output: None,
        started_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();
}

/// The engine side of `graph_continue retry_current_node` where
/// `retry_restart_target` picks a directed restart: the same target the
/// handler resolves, dispatched synchronously (the handler backgrounds this
/// call via `restart_at_node`, so it uses the resume dispatch that
/// `resume_background` uses). Returns the node the restart named.
async fn run_retry_restart(db: &Database, engine: &GraphEngine, graph_id: &str) -> Option<String> {
    let lp = db.get_graph(graph_id).unwrap().unwrap();
    let (spec, node) =
        crate::daemon::handler_run_directive::retry_restart_target(db, &lp).unwrap()?;
    engine
        .run_graph_dispatch(
            graph_id.to_string(),
            Some("queue-1".to_string()),
            None,
            DispatchMode::resume_from(DirectedStart {
                spec_id: spec.id.clone(),
                node_arg: node.clone(),
            }),
            None,
            None,
        )
        .await
        .unwrap();
    Some(node)
}

// ── R6: live queues — append and reorder while running ────────────────

async fn wait_for_file(path: &std::path::Path) {
    for _ in 0..500 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for file: {}", path.display());
}

fn record_node(id: &str, spec_id: &str, log: &std::path::Path, label: &str) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": format!(
                "echo {label} >> \"{log}\" && printf APPROVED",
                log = log.display(),
            ),
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    }
}

fn touch_gate_node(
    id: &str,
    spec_id: &str,
    marker: &std::path::Path,
    gate: &std::path::Path,
) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": format!(
                "touch \"{marker}\"; while [ ! -f \"{gate}\" ]; do sleep 0.02; done; printf APPROVED",
                marker = marker.display(),
                gate = gate.display(),
            ),
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    }
}

// ── B28: timeout-as-fail routing ────────────────────────────────────

fn test_crew_member(platform: &str) -> crate::domain::crews::CrewMember {
    crate::domain::crews::CrewMember {
        position: 0,
        platform: platform.to_string(),
        model: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }
}

/// A terminal check node the crew-node tests route into on pass, so a
/// passing agent node has an outgoing edge and the spec completes.
fn insert_crew_done_node(db: &Database, spec_id: &str) {
    db.insert_graph_node(&GraphNode {
        id: "node-done".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "done".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf ok",
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-next".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        from_node: "node-crew".to_string(),
        to_node: "node-done".to_string(),
        condition: GraphEdgeCondition::Always,
    })
    .unwrap();
}

// ── N2: on_completed hook tests ────────────────────────────────────

/// Set up a temporary HOME with a canopy config containing a `test-cli`
/// entry that points at `/bin/sh` — needed because `Cli::strategy()`
/// reads from `~/.canopy/config.toml`.
fn setup_test_cli_home() -> tempfile::TempDir {
    let fake_home = tempfile::tempdir().unwrap();
    let canopy_dir = fake_home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        configured_at: Some(chrono::Utc::now().to_rfc3339()),
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "test-cli".to_string(),
            binary: "/bin/sh".to_string(),
            headless_mode: "-c".to_string(),
            model_flag: None,
            supports_working_dir: false,
            working_dir_flag: None,
            env_vars: std::collections::HashMap::new(),
            interactive_args: None,
            fallback_interactive_args: None,
            resume_args: None,
            session_list_cmd: None,
            session_resume_cmd: None,
            accent_color: None,
            yolo_flag: None,
            prompt_via_stdin: false,
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    fake_home
}

/// Set up a temporary HOME with a canopy config containing a `sleep-cli`
/// entry that points at a fixed-behavior script (ignores its prompt
/// argument entirely — a real composed node prompt is multi-line prose,
/// not valid shell, so unlike `test-cli` (`/bin/sh -c "<prompt>"`) this
/// fixture can't just execute the prompt as a command). The script
/// sleeps `$SLEEP_SECONDS` (default 4) then exits 0.
fn setup_sleeping_cli_home() -> tempfile::TempDir {
    let fake_home = tempfile::tempdir().unwrap();
    let canopy_dir = fake_home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let script = fake_home.path().join("sleep-cli.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nsleep \"${SLEEP_SECONDS:-4}\"\necho done\nexit 0\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let config = crate::domain::canopy_config::CanopyConfig {
        configured_at: Some(chrono::Utc::now().to_rfc3339()),
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "sleep-cli".to_string(),
            binary: script.to_string_lossy().to_string(),
            headless_mode: "-c".to_string(),
            model_flag: None,
            supports_working_dir: false,
            working_dir_flag: None,
            env_vars: std::collections::HashMap::new(),
            interactive_args: None,
            fallback_interactive_args: None,
            resume_args: None,
            session_list_cmd: None,
            session_resume_cmd: None,
            accent_color: None,
            yolo_flag: None,
            prompt_via_stdin: false,
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    fake_home
}

/// Serializes [`HomeGuard`] users against each other. `HomeGuard` sets
/// `CANOPY_HOME_OVERRIDE` rather than the real `HOME` specifically so
/// unrelated tests (which never read that var) are unaffected — but the
/// var is still process-wide, so the handful of tests that *do* use it
/// must not run concurrently with each other.
static HOME_OVERRIDE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard that sets `CANOPY_HOME_OVERRIDE` (consulted by
/// [`crate::domain::models::Cli::strategy`]) for the duration of its
/// lifetime and restores the previous value on drop. Deliberately not
/// `HOME` itself: an earlier version of this guard swapped the real
/// `HOME` env var, which raced with concurrently-running tests that
/// shell out to git (git reads `HOME` for `user.name`/`user.email`),
/// intermittently failing unrelated reviewer-commit tests under
/// `cargo test`'s default parallel execution.
pub(crate) struct HomeGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    prev: Option<String>,
}

impl HomeGuard {
    pub(crate) fn set(path: &std::path::Path) -> Self {
        let lock = HOME_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prev = std::env::var("CANOPY_HOME_OVERRIDE").ok();
        unsafe {
            std::env::set_var("CANOPY_HOME_OVERRIDE", path);
        }
        Self { _lock: lock, prev }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(val) => unsafe {
                std::env::set_var("CANOPY_HOME_OVERRIDE", val);
            },
            None => unsafe {
                std::env::remove_var("CANOPY_HOME_OVERRIDE");
            },
        }
    }
}

mod fixtures_b;
