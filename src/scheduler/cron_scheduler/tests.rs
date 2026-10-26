//! `tests` tests (split from `src/scheduler/cron_scheduler.rs`, pure move).
use super::*;
use crate::application::notification_service::DefaultNotificationService;
use crate::application::ports::{AgentRepository, RunRepository};
use crate::domain::models::{Agent, Cli, Trigger};

fn manual_agent(id: &str, enabled: bool) -> Agent {
    Agent {
        id: id.to_string(),
        prompt: "do nothing".to_string(),
        trigger: None,
        cli: Cli::new("opencode"),
        model: None,
        effort: None,
        working_dir: None,
        enabled,
        enable_at: None,
        created_at: Utc::now(),
        log_path: "/tmp/enable-at-test.log".to_string(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    }
}

fn test_scheduler() -> (Arc<Database>, CronScheduler) {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    std::mem::forget(dir);
    let executor = Arc::new(Executor::new(
        db.clone(),
        Arc::new(DefaultNotificationService),
    ));
    let scheduler = CronScheduler::new(db.clone(), executor);
    (db, scheduler)
}

fn test_scheduler_with_graphs() -> (Arc<Database>, CronScheduler) {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    std::mem::forget(dir);
    let executor = Arc::new(Executor::new(
        db.clone(),
        Arc::new(DefaultNotificationService),
    ));
    let graph_engine = Arc::new(crate::graph_engine::GraphEngine::new(
        db.clone(),
        Arc::new(DefaultNotificationService),
    ));
    let scheduler = CronScheduler::with_graphs(db.clone(), executor, graph_engine);
    (db, scheduler)
}

/// Like [`test_scheduler_with_graphs`], but also hands back the
/// [`GraphEngine`] itself, with the cross-run attempt budget floored to a
/// single attempt, so a test can drive a spec to a genuine C19 `Blocked`
/// graph through real execution and then check the scheduler's autorun
/// behavior against that exact state.
fn test_scheduler_and_engine() -> (Arc<Database>, CronScheduler, Arc<GraphEngine>) {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    std::mem::forget(dir);
    let executor = Arc::new(Executor::new(
        db.clone(),
        Arc::new(DefaultNotificationService),
    ));
    let graph_engine = Arc::new(
        GraphEngine::new(db.clone(), Arc::new(DefaultNotificationService))
            .with_spec_attempt_limit(1),
    );
    let scheduler = CronScheduler::with_graphs(db.clone(), executor, Arc::clone(&graph_engine));
    (db, scheduler, graph_engine)
}

fn sample_graph(
    id: &str,
    status: crate::domain::graphs::GraphStatus,
) -> crate::domain::graphs::Graph {
    crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: id.to_string(),
        name: "Autorun test graph".to_string(),
        description: None,
        workdir: "/tmp/graph-autorun-test".to_string(),
        status,
        trigger: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    }
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
    run(&["config", "user.name", "Test"]);
    run(&["config", "user.email", "test@example.com"]);
    std::fs::write(path.join("README.md"), "test").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "init"]);
}

mod part01_tests;
mod part02_tests;
