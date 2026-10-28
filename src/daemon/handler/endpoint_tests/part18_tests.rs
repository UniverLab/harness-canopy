//! Part 18 of `endpoint_tests`: the MCP `sandbox` tool — the same list, land
//! and discard the `canopy sandbox` CLI performs, with its refusals and its
//! `sandbox_runs` row updates — plus the `graph` `get` report of a graph's
//! newest sandbox.
//!
//! Every mutation (`land`, `discard`) is guarded by
//! `domain::sandbox::guard_is_managed` against `worktrees_base_dir()`, so the
//! fixture pins `CANOPY_HOME_OVERRIDE` to a temp home and creates its sandbox
//! under that root — never the real `~/.canopy`.
use super::*;
use serde_json::json;
use std::path::Path;

/// Serializes this module's `CANOPY_HOME_OVERRIDE` users (process-wide var).
static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard setting `CANOPY_HOME_OVERRIDE` (what `canopy_dir()` — and so
/// `worktrees_base_dir()` — reads) for the test body and restoring it on drop.
struct CanopyHome {
    _lock: std::sync::MutexGuard<'static, ()>,
    prev: Option<std::ffi::OsString>,
}

impl CanopyHome {
    fn set(path: &Path) -> Self {
        let lock = HOME_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prev = std::env::var_os("CANOPY_HOME_OVERRIDE");
        unsafe { std::env::set_var("CANOPY_HOME_OVERRIDE", path) };
        Self { _lock: lock, prev }
    }
}

impl Drop for CanopyHome {
    fn drop(&mut self) {
        match &self.prev {
            Some(value) => unsafe { std::env::set_var("CANOPY_HOME_OVERRIDE", value) },
            None => unsafe { std::env::remove_var("CANOPY_HOME_OVERRIDE") },
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "true")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A one-commit repo on `main` — the base checkout a sandbox is cut from (the
/// same fixture `domain::sandbox`'s own tests use).
fn init_test_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create temp dir");
    git(dir.path(), &["init", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "test@test.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    std::fs::write(dir.path().join("README.md"), "# Test\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "initial"]);
    dir
}

/// The fixture: a temp canopy home (the guard root), a git repo, and one real
/// sandbox worktree cut from it. Field order matters — the env guard drops
/// (restoring the var) before the temp home it named is removed.
struct SandboxFixture {
    _guard: CanopyHome,
    _home: tempfile::TempDir,
    repo: tempfile::TempDir,
    sandbox: crate::domain::sandbox::Sandbox,
}

impl SandboxFixture {
    async fn new() -> Self {
        let home = tempfile::tempdir().expect("temp canopy home");
        let guard = CanopyHome::set(home.path());
        let repo = init_test_repo();
        let base = worktrees_root(home.path());
        let sandbox = crate::domain::sandbox::create_sandbox_in(
            &base,
            &repo.path().to_string_lossy(),
            "opencode",
            "PROTOCOL_CONTENT",
        )
        .await
        .expect("create sandbox");
        Self {
            _guard: guard,
            _home: home,
            repo,
            sandbox,
        }
    }

    /// Record the fixture's sandbox row for `graph_id`.
    fn record(&self, db: &Database, graph_id: &str) {
        db.insert_sandbox_run(&self.sandbox, "graph", graph_id)
            .expect("insert sandbox row");
    }

    /// Commit `file` with `content` inside the sandbox worktree, giving the
    /// branch work that exists nowhere else.
    fn commit_in_sandbox(&self, file: &str, content: &str) {
        std::fs::write(self.sandbox.worktree_path.join(file), content).unwrap();
        git(&self.sandbox.worktree_path, &["add", file]);
        git(
            &self.sandbox.worktree_path,
            &["commit", "-m", "sandbox work"],
        );
    }
}

fn worktrees_root(home: &Path) -> std::path::PathBuf {
    home.join(".canopy").join("worktrees")
}

fn row_status(db: &Database, sandbox_id: &str) -> String {
    db.get_sandbox_run(sandbox_id)
        .expect("read sandbox row")
        .expect("sandbox row exists")
        .status
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    let output = std::process::Command::new("git")
        .args(["branch", "--list", branch])
        .current_dir(repo)
        .output()
        .expect("git branch --list");
    !String::from_utf8_lossy(&output.stdout).trim().is_empty()
}

// ── list / get ───────────────────────────────────────────────────────

#[tokio::test]
async fn sandbox_list_returns_info_shape_and_filters_by_graph_and_status() {
    let fixture = SandboxFixture::new().await;
    let (_dir, db, handler) = endpoint_test_handler();
    fixture.record(&db, "graph-a");

    // A second row for another graph, with no live worktree behind it: the
    // graph filter must exclude it, and its status filter must be usable.
    let mut other = fixture.sandbox.clone();
    other.id = "sandbox-graph-b".to_string();
    other.project_hash = fixture.sandbox.project_hash.clone();
    other.sandbox_branch = "canopy/sandbox-other".to_string();
    other.worktree_path = worktrees_root(fixture._home.path()).join("sandbox-graph-b");
    other.created_at = chrono::Utc::now();
    db.insert_sandbox_run(&other, "graph", "graph-b").unwrap();

    let listed = call_resource_action(&handler, "sandbox", "list", ())
        .await
        .unwrap();
    assert!(!is_err(&listed), "{}", text(&listed));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&listed)).expect("list is JSON");
    let entries = body.as_array().expect("list returns an array");
    assert_eq!(entries.len(), 2, "both rows are listed: {body}");

    // FR2: every SandboxInfo field is present on every entry.
    for entry in entries {
        for field in [
            "id",
            "path",
            "graph_id",
            "branch",
            "base_branch",
            "original_workdir",
            "status",
            "has_unique_commits",
            "behind_count",
            "worktree_registered",
        ] {
            assert!(
                entry.get(field).is_some(),
                "entry must carry `{field}`: {entry}"
            );
        }
    }
    let ours = entries
        .iter()
        .find(|entry| entry["id"] == json!(fixture.sandbox.id))
        .expect("the real sandbox is listed");
    assert_eq!(ours["graph_id"], json!("graph-a"));
    assert_eq!(ours["branch"], json!(fixture.sandbox.sandbox_branch));
    assert_eq!(ours["base_branch"], json!("main"));
    assert_eq!(ours["status"], json!("active"));
    assert_eq!(ours["has_unique_commits"], json!(false));
    assert_eq!(ours["behind_count"], json!(0));
    assert_eq!(ours["worktree_registered"], json!(true));

    // graph_id narrows the list to that graph's rows only.
    let filtered = call_resource_action(
        &handler,
        "sandbox",
        "list",
        params_id(SandboxListParams {
            graph_id: Some("graph-a".to_string()),
            status: None,
        }),
    )
    .await
    .unwrap();
    let body: serde_json::Value = serde_json::from_str(&raw_text(&filtered)).unwrap();
    let entries = body.as_array().expect("array");
    assert_eq!(entries.len(), 1, "graph-a has one sandbox: {body}");
    assert_eq!(entries[0]["id"], json!(fixture.sandbox.id));

    // status narrows it too — and a status nothing carries yields an empty
    // list, never an error.
    let kept = call_resource_action(
        &handler,
        "sandbox",
        "list",
        params_id(SandboxListParams {
            graph_id: None,
            status: Some("kept".to_string()),
        }),
    )
    .await
    .unwrap();
    let body: serde_json::Value = serde_json::from_str(&raw_text(&kept)).unwrap();
    assert_eq!(body.as_array().expect("array").len(), 0, "{body}");

    db.update_sandbox_run_status("sandbox-graph-b", "kept")
        .unwrap();
    let kept = call_resource_action(
        &handler,
        "sandbox",
        "list",
        params_id(SandboxListParams {
            graph_id: None,
            status: Some("kept".to_string()),
        }),
    )
    .await
    .unwrap();
    let body: serde_json::Value = serde_json::from_str(&raw_text(&kept)).unwrap();
    let entries = body.as_array().expect("array");
    assert_eq!(entries.len(), 1, "{body}");
    assert_eq!(entries[0]["id"], json!("sandbox-graph-b"));
}

#[tokio::test]
async fn sandbox_get_returns_one_sandbox_and_refuses_an_unknown_id() {
    let fixture = SandboxFixture::new().await;
    let (_dir, db, handler) = endpoint_test_handler();
    fixture.record(&db, "graph-a");

    let got = call_resource_action(
        &handler,
        "sandbox",
        "get",
        params_id(SandboxIdParams {
            id: fixture.sandbox.id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&got), "{}", text(&got));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&got)).expect("get is JSON");
    assert_eq!(body["id"], json!(fixture.sandbox.id));
    assert_eq!(body["graph_id"], json!("graph-a"));
    assert_eq!(body["status"], json!("active"));
    assert_eq!(body["worktree_registered"], json!(true));

    // An unambiguous prefix resolves to the same row (the CLI's own rule).
    let by_prefix = call_resource_action(
        &handler,
        "sandbox",
        "get",
        params_id(SandboxIdParams {
            id: fixture.sandbox.id[..8].to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&by_prefix), "{}", text(&by_prefix));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&by_prefix)).unwrap();
    assert_eq!(body["id"], json!(fixture.sandbox.id));

    let missing = call_resource_action(
        &handler,
        "sandbox",
        "get",
        params_id(SandboxIdParams {
            id: "no-such-sandbox".to_string(),
        }),
    )
    .await;
    match missing {
        Ok(result) => panic!("expected an error, got: {}", raw_text(&result)),
        Err(err) => assert!(
            err.message
                .contains("no sandbox found for 'no-such-sandbox'"),
            "{}",
            err.message
        ),
    }
}

// ── parameter refusals ───────────────────────────────────────────────

#[tokio::test]
async fn sandbox_list_refuses_an_undeclared_parameter_naming_it() {
    let (_dir, _db, handler) = endpoint_test_handler();
    match call_resource_action_value(&handler, "sandbox", "list", json!({ "bogus": 1 })).await {
        Ok(result) => panic!("expected a refusal, got: {}", raw_text(&result)),
        Err(err) => {
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
            let message = err.message.to_string();
            assert!(
                message.contains("does not declare parameter(s): bogus"),
                "{message}"
            );
            assert!(
                message.contains("Action 'list'"),
                "the refusal must name the action: {message}"
            );
        }
    }
}

#[tokio::test]
async fn sandbox_land_refuses_a_missing_id_naming_the_action_and_parameter() {
    let (_dir, _db, handler) = endpoint_test_handler();
    match call_resource_action_value(&handler, "sandbox", "land", json!({})).await {
        Ok(result) => panic!("expected a refusal, got: {}", raw_text(&result)),
        Err(err) => {
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
            let message = err.message.to_string();
            assert!(
                message.contains("Action 'land' is missing required parameter(s): id"),
                "{message}"
            );
            assert!(
                message.contains("`discard`") && message.contains("force"),
                "the refusal must list the whole table: {message}"
            );
        }
    }
}

// ── land ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn sandbox_land_merges_the_work_and_marks_the_row_merged() {
    let fixture = SandboxFixture::new().await;
    let (_dir, db, handler) = endpoint_test_handler();
    fixture.record(&db, "graph-a");
    fixture.commit_in_sandbox("landed.txt", "sandbox work\n");
    assert!(fixture.sandbox.worktree_path.exists());

    let landed = call_resource_action(
        &handler,
        "sandbox",
        "land",
        params_id(SandboxIdParams {
            id: fixture.sandbox.id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&landed), "{}", text(&landed));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&landed)).expect("land is JSON");
    assert_eq!(body["id"], json!(fixture.sandbox.id));
    assert_eq!(body["status"], json!("merged"));
    assert_eq!(body["branch"], json!(fixture.sandbox.sandbox_branch));
    assert_eq!(body["base_branch"], json!("main"));

    // The work is really on the base branch, and the sandbox is gone.
    assert_eq!(
        std::fs::read_to_string(fixture.repo.path().join("landed.txt")).unwrap(),
        "sandbox work\n"
    );
    assert!(
        !fixture.sandbox.worktree_path.exists(),
        "a landed sandbox's worktree is removed"
    );
    assert!(
        !branch_exists(fixture.repo.path(), &fixture.sandbox.sandbox_branch),
        "a landed sandbox's branch is deleted"
    );
    assert_eq!(row_status(&db, &fixture.sandbox.id), "merged");
}

#[tokio::test]
async fn sandbox_land_refuses_a_conflict_names_the_paths_and_leaves_the_row_active() {
    let fixture = SandboxFixture::new().await;
    let (_dir, db, handler) = endpoint_test_handler();
    fixture.record(&db, "graph-a");
    fixture.commit_in_sandbox("a.txt", "sandbox side\n");
    // The base branch moves the same path independently.
    std::fs::write(fixture.repo.path().join("a.txt"), "base side\n").unwrap();
    git(fixture.repo.path(), &["add", "a.txt"]);
    git(fixture.repo.path(), &["commit", "-m", "base side"]);

    let refused = call_resource_action(
        &handler,
        "sandbox",
        "land",
        params_id(SandboxIdParams {
            id: fixture.sandbox.id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(
        is_err(&refused),
        "a conflicting land is refused: {}",
        text(&refused)
    );
    let message = raw_text(&refused);
    assert!(message.contains("merge conflict in"), "{message}");
    assert!(
        message.contains("a.txt"),
        "the refusal must name the conflicting path: {message}"
    );

    // Nothing was forced and nothing was recorded as landed.
    assert_eq!(row_status(&db, &fixture.sandbox.id), "active");
    assert!(
        fixture.sandbox.worktree_path.exists(),
        "a refused land leaves the sandbox in place"
    );
    assert!(branch_exists(
        fixture.repo.path(),
        &fixture.sandbox.sandbox_branch
    ));
    assert_eq!(
        std::fs::read_to_string(fixture.repo.path().join("a.txt")).unwrap(),
        "base side\n",
        "a refused land leaves the base branch untouched"
    );
}

// ── discard ──────────────────────────────────────────────────────────

#[tokio::test]
async fn sandbox_discard_refuses_unique_commits_without_force() {
    let fixture = SandboxFixture::new().await;
    let (_dir, db, handler) = endpoint_test_handler();
    fixture.record(&db, "graph-a");
    fixture.commit_in_sandbox("unique.txt", "only here\n");

    let refused = call_resource_action(
        &handler,
        "sandbox",
        "discard",
        params_id(SandboxDiscardParams {
            id: fixture.sandbox.id.clone(),
            force: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&refused), "{}", text(&refused));
    let message = raw_text(&refused);
    assert!(
        message.contains("holds commits that exist nowhere else"),
        "{message}"
    );
    assert!(
        message.contains("force: true"),
        "the refusal must name the parameter that consents: {message}"
    );

    // The unique work survives: worktree, branch and row all intact.
    assert_eq!(row_status(&db, &fixture.sandbox.id), "active");
    assert!(fixture.sandbox.worktree_path.join("unique.txt").exists());
    assert!(branch_exists(
        fixture.repo.path(),
        &fixture.sandbox.sandbox_branch
    ));
}

#[tokio::test]
async fn sandbox_discard_with_force_deletes_the_work_and_marks_the_row_discarded() {
    let fixture = SandboxFixture::new().await;
    let (_dir, db, handler) = endpoint_test_handler();
    fixture.record(&db, "graph-a");
    fixture.commit_in_sandbox("unique.txt", "only here\n");

    let discarded = call_resource_action(
        &handler,
        "sandbox",
        "discard",
        params_id(SandboxDiscardParams {
            id: fixture.sandbox.id.clone(),
            force: Some(true),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&discarded), "{}", text(&discarded));
    let body: serde_json::Value =
        serde_json::from_str(&raw_text(&discarded)).expect("discard is JSON");
    assert_eq!(body["id"], json!(fixture.sandbox.id));
    assert_eq!(body["status"], json!("discarded"));
    assert_eq!(body["branch"], json!(fixture.sandbox.sandbox_branch));

    assert_eq!(row_status(&db, &fixture.sandbox.id), "discarded");
    assert!(
        !fixture.sandbox.worktree_path.exists(),
        "a discarded sandbox's worktree is removed"
    );
    assert!(
        !branch_exists(fixture.repo.path(), &fixture.sandbox.sandbox_branch),
        "a discarded sandbox's branch is deleted"
    );
}

// ── graph get: the newest sandbox ────────────────────────────────────

#[tokio::test]
async fn graph_get_reports_the_newest_sandbox_id_and_status() {
    let fixture = SandboxFixture::new().await;
    let (_dir, db, handler) = endpoint_test_handler();
    let graph = insert_test_graph(&db, fixture.repo.path());

    // Two rows for the graph: an older `cleaned` one and the newest `kept`
    // one the graph detail must name.
    let mut older = fixture.sandbox.clone();
    older.id = "older-sandbox".to_string();
    older.created_at = chrono::Utc::now() - chrono::Duration::hours(1);
    db.insert_sandbox_run(&older, "graph", &graph.id).unwrap();
    db.update_sandbox_run_status("older-sandbox", "cleaned")
        .unwrap();
    fixture.record(&db, &graph.id);
    db.update_sandbox_run_status(&fixture.sandbox.id, "kept")
        .unwrap();

    let got = call_graph_action(
        &handler,
        "graph",
        "get",
        params_id(GraphGetParams {
            graph_id: graph.id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&got), "{}", text(&got));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&got)).expect("graph get is JSON");
    assert_eq!(body["sandbox"]["id"], json!(fixture.sandbox.id));
    assert_eq!(body["sandbox"]["status"], json!("kept"));

    // A graph with no sandbox row reports null, never a stale id.
    let bare = insert_test_graph(&db, fixture.repo.path());
    let got = call_graph_action(
        &handler,
        "graph",
        "get",
        params_id(GraphGetParams {
            graph_id: bare.id.clone(),
        }),
    )
    .await
    .unwrap();
    let body: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
    assert_eq!(body["sandbox"], serde_json::Value::Null, "{body}");
}
