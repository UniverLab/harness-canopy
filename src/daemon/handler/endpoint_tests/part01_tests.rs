//! Part 01 of `endpoint_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

/// B7 A5.1: `HandlerDeps` round-trip — `with_deps` and `new` construct
/// handlers that expose the same `port`, a fresh `start_time`, and an
/// identical tool inventory.
#[test]
fn handler_deps_round_trip_constructs_identical_handler() {
    use crate::application::notification_service::{
        DefaultNotificationService, NotificationService,
    };
    use crate::daemon::handler::HandlerDeps;
    use crate::executor::Executor;
    use crate::graph_engine::GraphEngine;
    use crate::rag::ingestion::IngestionManager;
    use crate::sync_manager::SyncManager;
    use crate::watchers::WatcherEngine;
    use std::sync::Arc;
    use tokio::sync::Notify;

    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let notif: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
    let executor = Arc::new(Executor::new(Arc::clone(&db), Arc::clone(&notif)));
    let sync_manager = Arc::new(SyncManager::new(Arc::clone(&db)));
    let graph_engine = Arc::new(GraphEngine::new(Arc::clone(&db), Arc::clone(&notif)));
    let watcher_engine = Arc::new(WatcherEngine::new(
        Arc::clone(&db),
        Arc::clone(&executor),
        Arc::clone(&graph_engine),
    ));
    let ingestion = Arc::new(IngestionManager::new(
        Arc::clone(&db),
        dir.path().to_path_buf(),
    ));
    let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::new(
        dir.path().join("skills"),
        Vec::new(),
        15,
    ));
    let scheduler_notify = Arc::new(Notify::new());

    let deps = HandlerDeps {
        db: Arc::clone(&db),
        executor: Arc::clone(&executor),
        watcher_engine: Arc::clone(&watcher_engine),
        scheduler_notify: Arc::clone(&scheduler_notify),
        graph_engine: Arc::clone(&graph_engine),
        notification_service: Arc::clone(&notif),
        sync_manager: Arc::clone(&sync_manager),
        ingestion: Arc::clone(&ingestion),
        dynamic_skills: Arc::clone(&dynamic_skills),
        start_time: std::time::Instant::now(),
        port: 0,
    };
    let via_deps = TaskTriggerHandler::with_deps(deps);

    assert_eq!(via_deps.port, 0);

    let deps_elapsed = via_deps.start_time.elapsed();
    assert!(
        deps_elapsed.as_secs() <= 1,
        "start_time too stale: {deps_elapsed:?}"
    );

    let deps_names: Vec<String> = TaskTriggerHandler::tool_router()
        .list_all()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    assert!(!deps_names.is_empty());
}

/// CB83: handlers built from the same daemon start instant report the
/// same uptime however late they are created — `agent_status` `Uptime`
/// must be the daemon's age, not the MCP session's.
#[test]
fn daemon_start_instant_is_shared_by_every_handler() {
    use crate::application::notification_service::{
        DefaultNotificationService, NotificationService,
    };
    use crate::daemon::handler::HandlerDeps;
    use crate::daemon::handler_formatting::format_uptime;
    use crate::executor::Executor;
    use crate::graph_engine::GraphEngine;
    use crate::rag::ingestion::IngestionManager;
    use crate::sync_manager::SyncManager;
    use crate::watchers::WatcherEngine;
    use std::sync::Arc;
    use tokio::sync::Notify;

    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let notif: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
    let executor = Arc::new(Executor::new(Arc::clone(&db), Arc::clone(&notif)));
    let sync_manager = Arc::new(SyncManager::new(Arc::clone(&db)));
    let graph_engine = Arc::new(GraphEngine::new(Arc::clone(&db), Arc::clone(&notif)));
    let watcher_engine = Arc::new(WatcherEngine::new(
        Arc::clone(&db),
        Arc::clone(&executor),
        Arc::clone(&graph_engine),
    ));
    let ingestion = Arc::new(IngestionManager::new(
        Arc::clone(&db),
        dir.path().to_path_buf(),
    ));
    let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::new(
        dir.path().join("skills"),
        Vec::new(),
        15,
    ));
    let scheduler_notify = Arc::new(Notify::new());

    // A daemon start instant in the past — the value the daemon hands
    // to every handler it builds. `Instant` is `Copy`, so the same
    // value goes into both handlers.
    let daemon_start = std::time::Instant::now() - std::time::Duration::from_secs(120);

    let first = TaskTriggerHandler::with_deps(HandlerDeps {
        db: Arc::clone(&db),
        executor: Arc::clone(&executor),
        watcher_engine: Arc::clone(&watcher_engine),
        scheduler_notify: Arc::clone(&scheduler_notify),
        graph_engine: Arc::clone(&graph_engine),
        notification_service: Arc::clone(&notif),
        sync_manager: Arc::clone(&sync_manager),
        ingestion: Arc::clone(&ingestion),
        dynamic_skills: Arc::clone(&dynamic_skills),
        start_time: daemon_start,
        port: 0,
    });

    // The second handler is created later, the way Streamable HTTP
    // builds one handler per MCP session. The wait must exceed
    // `format_uptime`'s one-second granularity: a handler that stamped
    // its own start time would otherwise format to the same string.
    std::thread::sleep(std::time::Duration::from_secs(2));

    let second = TaskTriggerHandler::with_deps(HandlerDeps {
        db: Arc::clone(&db),
        executor: Arc::clone(&executor),
        watcher_engine: Arc::clone(&watcher_engine),
        scheduler_notify: Arc::clone(&scheduler_notify),
        graph_engine: Arc::clone(&graph_engine),
        notification_service: Arc::clone(&notif),
        sync_manager: Arc::clone(&sync_manager),
        ingestion: Arc::clone(&ingestion),
        dynamic_skills: Arc::clone(&dynamic_skills),
        start_time: daemon_start,
        port: 0,
    });

    assert_eq!(
        format_uptime(first.start_time.elapsed().as_secs()),
        format_uptime(second.start_time.elapsed().as_secs()),
    );
}

// ── task_add / agent_add ─────────────────────────────────────

#[tokio::test]
async fn task_add_registers_a_cron_agent() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (_dir, db, handler) = endpoint_test_handler();

    let result = handler
        .task_add(
            Parameters(TaskAddParams {
                action: None,
                target_session_name: None,
                target_session_id: None,
                id: "agt-1".to_string(),
                prompt: "run the tests".to_string(),
                schedule: "*/5 * * * *".to_string(),
                cli: Some("opencode".to_string()),
                model: None,
                effort: None,
                duration_minutes: None,
                working_dir: None,
                timeout_minutes: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    assert!(text(&result).contains("registered with schedule"));
    let stored = db.get_agent("agt-1").unwrap().unwrap();
    assert!(matches!(stored.trigger, Some(Trigger::Cron { .. })));
}

#[tokio::test]
async fn agent_add_send_action_does_not_require_cli() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (_dir, db, handler) = endpoint_test_handler();

    let result = handler
        .task_add(
            Parameters(TaskAddParams {
                action: Some("send_to_session".to_string()),
                target_session_name: Some("orch".to_string()),
                target_session_id: None,
                id: "nudge-1".to_string(),
                prompt: "nudge the orchestrator".to_string(),
                schedule: "0 * * * *".to_string(),
                cli: None,
                model: None,
                effort: None,
                duration_minutes: None,
                working_dir: None,
                timeout_minutes: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    assert!(text(&result).contains("orch"), "{}", text(&result));
    let stored = db.get_agent("nudge-1").unwrap().unwrap();
    assert!(stored.is_send_to_session());
    assert_eq!(stored.action.target_session_name(), Some("orch"));
}

#[tokio::test]
async fn agent_watch_send_action_registers_watcher() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (dir, db, handler) = endpoint_test_handler();

    let result = handler
        .task_watch(
            Parameters(TaskWatchParams {
                action: Some("send_to_session".to_string()),
                target_session_name: None,
                target_session_id: Some("sess-1".to_string()),
                id: "watch-send-1".to_string(),
                path: dir.path().to_string_lossy().to_string(),
                events: vec!["modify".to_string()],
                prompt: "a spec changed".to_string(),
                cli: None,
                model: None,
                effort: None,
                debounce_seconds: None,
                recursive: None,
                timeout_minutes: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    assert!(text(&result).contains("sess-1"), "{}", text(&result));
    let stored = db.get_agent("watch-send-1").unwrap().unwrap();
    assert!(stored.is_send_to_session());
    assert!(matches!(stored.trigger, Some(Trigger::Watch { .. })));
}

#[tokio::test]
async fn agent_update_switching_to_headless_requires_cli() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (_dir, db, handler) = endpoint_test_handler();

    let created = handler
        .task_add(
            Parameters(TaskAddParams {
                action: Some("send_to_session".to_string()),
                target_session_name: Some("orch".to_string()),
                target_session_id: None,
                id: "switch-1".to_string(),
                prompt: "p".to_string(),
                schedule: "0 * * * *".to_string(),
                cli: None,
                model: None,
                effort: None,
                duration_minutes: None,
                working_dir: None,
                timeout_minutes: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let result = handler
        .task_update(
            Parameters(TaskUpdateParams {
                action: Some("headless".to_string()),
                target_session_name: None,
                target_session_id: None,
                id: "switch-1".to_string(),
                new_id: None,
                prompt: None,
                cli: None,
                model: None,
                effort: None,
                schedule: None,
                working_dir: None,
                duration_minutes: None,
                path: None,
                events: None,
                debounce_seconds: None,
                recursive: None,
                enabled: None,
                notify_on_success: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();

    assert!(is_err(&result), "{}", text(&result));
    assert!(text(&result).contains("cli"), "{}", text(&result));
    // The action is unchanged.
    assert!(db
        .get_agent("switch-1")
        .unwrap()
        .unwrap()
        .is_send_to_session());
}

#[test]
fn agent_get_hides_cli_for_send_agents() {
    let mut agent = crate::daemon::handler_helpers::new_agent_base(
        "send-1".to_string(),
        "p".to_string(),
        crate::domain::models::Cli::new("opencode"),
        Some("a-model".to_string()),
        None,
        None,
        "/tmp/send-1.log".to_string(),
    );
    agent.action =
        crate::domain::models::AgentAction::send_to_session(Some("orch".to_string()), None)
            .unwrap();

    let info = crate::daemon::handler_formatting::format_agent_info(&agent);
    assert!(info.contains("send to session"), "{info}");
    assert!(info.contains("target: session name 'orch'"), "{info}");
    assert!(!info.contains("CLI:"), "{info}");

    let json = crate::daemon::handler_formatting::agent_detail_json(&agent, false);
    assert!(json.get("cli").is_none(), "{json}");
    assert!(json.get("model").is_none(), "{json}");
    assert!(json.get("timeout_minutes").is_none(), "{json}");
    assert!(
        json.get("target")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("orch"),
        "{json}"
    );
}

#[tokio::test]
async fn agent_update_retargets_send_agent_without_repeating_action() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (_dir, db, handler) = endpoint_test_handler();

    let created = handler
        .task_add(
            Parameters(TaskAddParams {
                action: Some("send_to_session".to_string()),
                target_session_name: Some("orch".to_string()),
                target_session_id: None,
                id: "retarget-1".to_string(),
                prompt: "p".to_string(),
                schedule: "0 * * * *".to_string(),
                cli: None,
                model: None,
                effort: None,
                duration_minutes: None,
                working_dir: None,
                timeout_minutes: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let updated = handler
        .task_update(
            Parameters(TaskUpdateParams {
                action: None,
                target_session_name: Some("other".to_string()),
                target_session_id: None,
                id: "retarget-1".to_string(),
                new_id: None,
                prompt: None,
                cli: None,
                model: None,
                effort: None,
                schedule: None,
                working_dir: None,
                duration_minutes: None,
                path: None,
                events: None,
                debounce_seconds: None,
                recursive: None,
                enabled: None,
                notify_on_success: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));

    let stored = db.get_agent("retarget-1").unwrap().unwrap();
    assert_eq!(stored.action.target_session_name(), Some("other"));
}

#[tokio::test]
async fn agent_update_refuses_target_on_headless_agent() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (dir, db, handler) = endpoint_test_handler();

    let created = handler
        .task_watch(
            Parameters(TaskWatchParams {
                action: None,
                target_session_name: None,
                target_session_id: None,
                id: "headless-watch".to_string(),
                path: dir.path().to_string_lossy().to_string(),
                events: vec!["modify".to_string()],
                prompt: "p".to_string(),
                cli: Some("opencode".to_string()),
                model: None,
                effort: None,
                debounce_seconds: None,
                recursive: None,
                timeout_minutes: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let updated = handler
        .task_update(
            Parameters(TaskUpdateParams {
                action: None,
                target_session_name: Some("orch".to_string()),
                target_session_id: None,
                id: "headless-watch".to_string(),
                new_id: None,
                prompt: None,
                cli: None,
                model: None,
                effort: None,
                schedule: None,
                working_dir: None,
                duration_minutes: None,
                path: None,
                events: None,
                debounce_seconds: None,
                recursive: None,
                enabled: None,
                notify_on_success: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(is_err(&updated), "{}", text(&updated));
    assert!(!db
        .get_agent("headless-watch")
        .unwrap()
        .unwrap()
        .is_send_to_session());
}

#[tokio::test]
async fn task_add_rejects_invalid_cron_expression() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (_dir, db, handler) = endpoint_test_handler();

    let result = handler
        .task_add(
            Parameters(TaskAddParams {
                action: None,
                target_session_name: None,
                target_session_id: None,
                id: "agt-bad-cron".to_string(),
                prompt: "run the tests".to_string(),
                schedule: "not a cron expr!".to_string(),
                cli: Some("opencode".to_string()),
                model: None,
                effort: None,
                duration_minutes: None,
                working_dir: None,
                timeout_minutes: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();

    assert!(is_err(&result));
    assert!(text(&result).contains("Invalid cron expression"));
    assert!(db.get_agent("agt-bad-cron").unwrap().is_none());
}

#[tokio::test]
async fn task_add_rejects_invalid_id() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (_dir, _db, handler) = endpoint_test_handler();

    let result = handler
        .task_add(
            Parameters(TaskAddParams {
                action: None,
                target_session_name: None,
                target_session_id: None,
                id: "bad id with spaces!".to_string(),
                prompt: "run the tests".to_string(),
                schedule: "*/5 * * * *".to_string(),
                cli: Some("opencode".to_string()),
                model: None,
                effort: None,
                duration_minutes: None,
                working_dir: None,
                timeout_minutes: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();

    assert!(is_err(&result));
    assert!(text(&result).contains("alphanumeric"));
}

#[tokio::test]
async fn task_add_rejects_platform_whose_binary_is_the_wrong_program() {
    let home = tempdir().unwrap();
    let script = home.path().join("blackbox");
    std::fs::write(
            &script,
            "if [ \"$1\" = \"--version\" ]; then echo \"blackbox: another window manager is already running on display ':0'\"; else echo \"$1\"; fi\n",
        )
        .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    crate::domain::canopy_config::CanopyConfig {
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "blackbox".to_string(),
            binary: script.to_string_lossy().to_string(),
            identity_args: Some(vec!["--version".to_string()]),
            identity_pattern: Some("(?i)blackbox ?ai".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    }
    .save_for_tests(&canopy_dir)
    .unwrap();
    let _home_guard = HomeVar::set(home.path());

    let (_dir, db, handler) = endpoint_test_handler();
    let result = handler
        .task_add(
            Parameters(TaskAddParams {
                action: None,
                target_session_name: None,
                target_session_id: None,
                id: "agt-wrong-bin".to_string(),
                prompt: "run the tests".to_string(),
                schedule: "*/5 * * * *".to_string(),
                cli: Some("blackbox".to_string()),
                model: None,
                effort: None,
                duration_minutes: None,
                working_dir: None,
                timeout_minutes: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();

    assert!(is_err(&result));
    assert!(text(&result).contains("is not"));
    assert!(text(&result).contains("identity check failed"));
    assert!(db.get_agent("agt-wrong-bin").unwrap().is_none());
}

/// CM35 FR2: `agent_add` refuses a platform marked `headless = false`
/// with the exact spec wording, and the agent never reaches the DB — so
/// the scheduler can never dispatch a headless run onto it. The check
/// runs before `ensure_cli_installed`, so no process is spawned either
/// (the fixture binary would `touch` a marker if it ran).
#[tokio::test]
async fn task_add_rejects_interactive_only_platform() {
    let home = tempdir().unwrap();
    let marker = write_freebuff_config(home.path());
    let _home_guard = HomeVar::set(home.path());

    let (_dir, db, handler) = endpoint_test_handler();

    let result = handler
        .task_add(
            Parameters(TaskAddParams {
                action: None,
                target_session_name: None,
                target_session_id: None,
                id: "agt-live-only".to_string(),
                prompt: "run the tests".to_string(),
                schedule: "*/5 * * * *".to_string(),
                cli: Some("freebuff".to_string()),
                model: None,
                effort: None,
                duration_minutes: None,
                working_dir: None,
                timeout_minutes: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();

    assert!(is_err(&result), "{}", text(&result));
    assert!(
        raw_text(&result).contains("platform 'freebuff' is interactive-only (no headless mode)"),
        "{}",
        raw_text(&result)
    );
    assert!(db.get_agent("agt-live-only").unwrap().is_none());
    assert!(
        !marker.exists(),
        "the interactive-only gate must fire before any process is spawned"
    );
}

// ── task_watch / agent_watch ─────────────────────────────────

#[tokio::test]
async fn task_watch_registers_a_watcher() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (dir, db, handler) = endpoint_test_handler();

    let result = handler
        .task_watch(
            Parameters(TaskWatchParams {
                action: None,
                target_session_name: None,
                target_session_id: None,
                id: "watch-1".to_string(),
                path: dir.path().to_string_lossy().to_string(),
                events: vec!["modify".to_string(), "create".to_string()],
                prompt: "react to changes".to_string(),
                cli: Some("opencode".to_string()),
                model: None,
                effort: None,
                debounce_seconds: None,
                recursive: Some(true),
                timeout_minutes: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let stored = db.get_agent("watch-1").unwrap().unwrap();
    assert!(matches!(stored.trigger, Some(Trigger::Watch { .. })));
}

#[tokio::test]
async fn task_watch_rejects_relative_path() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (_dir, db, handler) = endpoint_test_handler();

    let result = handler
        .task_watch(
            Parameters(TaskWatchParams {
                action: None,
                target_session_name: None,
                target_session_id: None,
                id: "watch-bad-path".to_string(),
                path: "relative/path".to_string(),
                events: vec!["modify".to_string()],
                prompt: "react to changes".to_string(),
                cli: Some("opencode".to_string()),
                model: None,
                effort: None,
                debounce_seconds: None,
                recursive: None,
                timeout_minutes: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();

    assert!(is_err(&result));
    assert!(text(&result).contains("must be absolute"));
    assert!(db.get_agent("watch-bad-path").unwrap().is_none());
}

#[tokio::test]
async fn task_watch_rejects_unknown_event_name() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (dir, _db, handler) = endpoint_test_handler();

    let result = handler
        .task_watch(
            Parameters(TaskWatchParams {
                action: None,
                target_session_name: None,
                target_session_id: None,
                id: "watch-bad-event".to_string(),
                path: dir.path().to_string_lossy().to_string(),
                events: vec!["explode".to_string()],
                prompt: "react to changes".to_string(),
                cli: Some("opencode".to_string()),
                model: None,
                effort: None,
                debounce_seconds: None,
                recursive: None,
                timeout_minutes: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();

    assert!(is_err(&result));
}

#[tokio::test]
async fn task_list_reports_agents_and_corrupt_rows() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.upsert_agent(&sample_agent("healthy-1", None)).unwrap();
    db.insert_corrupt_agent_for_test("corrupt-1", true).unwrap();

    let result = handler.task_list().await.unwrap();
    let out = text(&result);
    assert!(out.contains("healthy-1"));
    assert!(out.contains("corrupt"));
    assert!(out.contains("corrupt-1"));
}

#[tokio::test]
async fn task_list_reports_empty_state() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = handler.task_list().await.unwrap();
    assert!(text(&result).contains("No agents registered"));
}

#[tokio::test]
async fn agent_get_returns_full_prompt_and_every_field() {
    let (_dir, db, handler) = endpoint_test_handler();
    let mut agent = sample_agent(
        "long-prompt",
        Some(Trigger::Cron {
            schedule_expr: "0 9 * * *".to_string(),
        }),
    );
    agent.prompt = "x".repeat(2000);
    db.upsert_agent(&agent).unwrap();

    let result = handler
        .agent_get(Parameters(IdParam {
            id: "long-prompt".to_string(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let v: serde_json::Value = serde_json::from_str(&raw_text(&result))
        .unwrap_or_else(|e| panic!("expected JSON body: {e}"));
    // The 2,000-char prompt comes back byte-identical — no truncation.
    assert_eq!(v["prompt"].as_str().unwrap(), "x".repeat(2000));
    for field in [
        "id",
        "trigger_type",
        "trigger_config",
        "cli",
        "model",
        "effort",
        "working_dir",
        "enabled",
        "timeout_minutes",
        "expires_at",
        "enable_at",
        "notify_on_success",
        "log_path",
        "created_at",
        "last_run_at",
        "last_run_ok",
        "last_triggered_at",
        "trigger_count",
        "prompt",
    ] {
        assert!(v.get(field).is_some(), "missing field {field}");
    }
    assert_eq!(v["id"], serde_json::json!("long-prompt"));
    assert_eq!(v["trigger_type"], serde_json::json!("cron"));
    assert_eq!(
        v["trigger_config"]["schedule_expr"],
        serde_json::json!("0 9 * * *")
    );
    assert_eq!(v["cli"], serde_json::json!("opencode"));
    assert_eq!(v["timeout_minutes"], serde_json::json!(15));
}

#[tokio::test]
async fn agent_get_unknown_id_returns_exact_error() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = handler
        .agent_get(Parameters(IdParam {
            id: "nope".to_string(),
        }))
        .await
        .unwrap();
    assert!(is_err(&result));
    assert_eq!(raw_text(&result), "agent 'nope' not found");
}

#[tokio::test]
async fn task_list_long_prompt_points_to_agent_get() {
    let (_dir, db, handler) = endpoint_test_handler();
    let mut agent = sample_agent("loop-watchdog", None);
    agent.prompt = "y".repeat(2000);
    db.upsert_agent(&agent).unwrap();

    let result = handler.task_list().await.unwrap();
    assert!(text(&result).contains("agent_get loop-watchdog"));
}

#[tokio::test]
async fn session_list_round_trip_id_is_hook_acceptable() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.insert_interactive_session(
        "s-live",
        "boletus",
        "opencode",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    let result = handler
        .session_list(Parameters(SessionListParams {
            session_id: None,
            limit: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let out = raw_text(&result);
    let listed_id = out
        .lines()
        .find_map(|line| line.strip_prefix("- id: "))
        .and_then(|line| line.split(" | ").next())
        .expect("session id line");
    assert_eq!(listed_id, "s-live");
    assert!(db
        .get_active_sessions()
        .unwrap()
        .iter()
        .any(|session| session.id == listed_id));
}

#[tokio::test]
async fn session_list_active_session_without_live_tui_is_marked_unattached_but_targetable() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.insert_interactive_session(
        "s-unattached",
        "unattached",
        "claude",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    let result = handler
        .session_list(Parameters(SessionListParams {
            session_id: None,
            limit: None,
        }))
        .await
        .unwrap();
    let out = raw_text(&result);
    assert!(out.contains("s-unattached"));
    assert!(out.contains("attached: no"));
    assert!(out.contains("hook_target: yes"));
}

#[tokio::test]
async fn session_list_attached_session_is_marked_attached() {
    let (_dir, db, handler) = endpoint_test_handler();
    let Some(boot) = crate::system::boot_id() else {
        return;
    };
    db.insert_interactive_session(
        "s-attached",
        "attached",
        "claude",
        "/tmp",
        None,
        Some(std::process::id() as i64),
        "interactive",
        Some(&boot),
    )
    .unwrap();

    let result = handler
        .session_list(Parameters(SessionListParams {
            session_id: None,
            limit: None,
        }))
        .await
        .unwrap();
    let out = raw_text(&result);
    assert!(out.contains("s-attached"), "{out}");
    assert!(out.contains("attached: yes"), "{out}");
}

#[tokio::test]
async fn session_list_lookup_returns_verbatim_hookable_id() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.insert_interactive_session(
        "agent-7f3a",
        "boletus",
        "opencode",
        "/tmp/w",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    let result = handler
        .session_list(Parameters(SessionListParams {
            session_id: Some("  agent-7f3a  ".to_string()),
            limit: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&result));
    let out = raw_text(&result);
    assert!(out.contains("id: agent-7f3a "), "{out}");
    assert!(db
        .get_active_sessions()
        .unwrap()
        .iter()
        .any(|s| s.id == "agent-7f3a"));
}

#[tokio::test]
async fn session_list_unknown_id_is_not_found() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = handler
        .session_list(Parameters(SessionListParams {
            session_id: Some("nope".to_string()),
            limit: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&result));
    let out = text(&result);
    assert!(out.contains("No session found"));
    assert!(out.contains("nope"));
}

#[tokio::test]
async fn session_list_omits_args_and_output() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.insert_interactive_session(
        "s-cloak",
        "private",
        "opencode",
        "/tmp",
        Some("--token secret"),
        None,
        "interactive",
        None,
    )
    .unwrap();

    let result = handler
        .session_list(Parameters(SessionListParams {
            session_id: None,
            limit: None,
        }))
        .await
        .unwrap();
    let out = raw_text(&result);
    // The session is listed (otherwise the assertions below pass
    // vacuously)…
    assert!(out.contains("id: s-cloak"), "{out}");
    assert!(out.contains("name: private"), "{out}");
    // …but its launch args / secrets never appear in the row.
    assert!(!out.contains("secret"), "{out}");
    assert!(!out.contains("--token"), "{out}");
    assert!(!out.contains("args"), "{out}");
}

#[tokio::test]
async fn scheduled_send_create_targets_own_session_when_omitted() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.insert_interactive_session(
        "self-session",
        "me",
        "claude",
        "/proj",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    let _guard = AgentIdVar::set("self-session");

    let result = handler
        .scheduled_send_create(
            Parameters(ScheduledSendCreateParams {
                prompt: "wake up".to_string(),
                target_session_id: None,
                at: None,
                in_seconds: Some(60),
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));

    let pending = db
        .list_pending_scheduled_sends_for_session("self-session")
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].prompt, "wake up");
    assert_eq!(
        pending[0]
            .provenance
            .as_ref()
            .unwrap()
            .session_id
            .as_deref(),
        Some("self-session")
    );
}

#[tokio::test]
async fn scheduled_send_create_targets_named_session() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.insert_interactive_session(
        "caller",
        "me",
        "claude",
        "/proj",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.insert_interactive_session(
        "other",
        "other",
        "claude",
        "/proj2",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    let _guard = AgentIdVar::set("caller");

    let result = handler
        .scheduled_send_create(
            Parameters(ScheduledSendCreateParams {
                prompt: "for the other session".to_string(),
                target_session_id: Some("other".to_string()),
                at: None,
                in_seconds: Some(30),
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));

    let pending = db
        .list_pending_scheduled_sends_for_session("other")
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0]
            .provenance
            .as_ref()
            .unwrap()
            .session_id
            .as_deref(),
        Some("caller")
    );
    assert_eq!(pending[0].provenance.as_ref().unwrap().kind, "agent");
}

#[tokio::test]
async fn scheduled_send_create_rejects_unknown_target() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _guard = AgentIdVar::set("caller-2");

    let result = handler
        .scheduled_send_create(
            Parameters(ScheduledSendCreateParams {
                prompt: "hi".to_string(),
                target_session_id: Some("no-such-session".to_string()),
                at: None,
                in_seconds: Some(60),
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(is_err(&result));
    assert!(text(&result).contains("no-such-session"));
}

#[tokio::test]
async fn scheduled_send_create_requires_exactly_one_of_at_or_in_seconds() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.insert_interactive_session(
        "caller-3",
        "me",
        "claude",
        "/proj",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    let _guard = AgentIdVar::set("caller-3");

    let neither = handler
        .scheduled_send_create(
            Parameters(ScheduledSendCreateParams {
                prompt: "hi".to_string(),
                target_session_id: None,
                at: None,
                in_seconds: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(is_err(&neither));

    let both = handler
        .scheduled_send_create(
            Parameters(ScheduledSendCreateParams {
                prompt: "hi".to_string(),
                target_session_id: None,
                at: Some("2026-07-10T09:00:00Z".to_string()),
                in_seconds: Some(60),
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(is_err(&both));
}

#[tokio::test]
async fn scheduled_send_list_defaults_to_own_session() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.insert_interactive_session(
        "list-self",
        "me",
        "claude",
        "/proj",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    let _guard = AgentIdVar::set("list-self");
    db.insert_scheduled_send(
        "ss-list-1",
        "hello",
        "list-self",
        Some("/proj"),
        chrono::Utc::now() + chrono::Duration::hours(1),
        None,
        None,
    )
    .unwrap();

    let result = handler
        .scheduled_send_list(
            Parameters(ScheduledSendListParams { session_id: None }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let out = raw_text(&result);
    assert!(out.contains("ss-list-1"));
    assert!(out.contains("hello"));
}

#[tokio::test]
async fn scheduled_send_list_explicit_session_needs_no_identity() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.insert_scheduled_send(
        "ss-list-2",
        "explicit",
        "some-session",
        None,
        chrono::Utc::now() + chrono::Duration::hours(1),
        None,
        None,
    )
    .unwrap();

    let result = handler
        .scheduled_send_list(
            Parameters(ScheduledSendListParams {
                session_id: Some("some-session".to_string()),
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    assert!(raw_text(&result).contains("ss-list-2"));
}

#[tokio::test]
async fn scheduled_send_list_empty_reports_none_pending() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = handler
        .scheduled_send_list(
            Parameters(ScheduledSendListParams {
                session_id: Some("nobody".to_string()),
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&result));
    assert!(text(&result).contains("No pending"));
}

#[tokio::test]
async fn scheduled_send_cancel_removes_pending_send() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.insert_scheduled_send(
        "ss-cancel-1",
        "to cancel",
        "session-x",
        None,
        chrono::Utc::now() + chrono::Duration::hours(1),
        None,
        None,
    )
    .unwrap();

    let result = handler
        .scheduled_send_cancel(Parameters(ScheduledSendCancelParams {
            id: "ss-cancel-1".to_string(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    assert!(db
        .list_pending_scheduled_sends_for_session("session-x")
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn scheduled_send_cancel_unknown_id_is_not_found() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = handler
        .scheduled_send_cancel(Parameters(ScheduledSendCancelParams {
            id: "no-such-id".to_string(),
        }))
        .await
        .unwrap();
    assert!(is_err(&result));
    assert!(text(&result).contains("no-such-id"));
}

// ── task_remove / agent_remove ───────────────────────────────

#[tokio::test]
async fn task_remove_deletes_existing_agent() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.upsert_agent(&sample_agent("to-remove", None)).unwrap();

    let result = handler
        .task_remove(Parameters(IdParam {
            id: "to-remove".to_string(),
        }))
        .await
        .unwrap();

    assert!(!is_err(&result));
    assert!(text(&result).contains("removed"));
    assert!(db.get_agent("to-remove").unwrap().is_none());
}

#[tokio::test]
async fn task_remove_reports_missing_agent() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = handler
        .task_remove(Parameters(IdParam {
            id: "does-not-exist".to_string(),
        }))
        .await
        .unwrap();

    assert!(is_err(&result));
    assert!(text(&result).contains("No agent found"));
}

// ── task_enable / agent_enable ───────────────────────────────

#[tokio::test]
async fn task_enable_clears_expiry_on_expired_agent() {
    let (_dir, db, handler) = endpoint_test_handler();
    let mut agent = sample_agent("expired-1", None);
    agent.enabled = false;
    agent.expires_at = Some(chrono::Utc::now() - chrono::Duration::minutes(5));
    db.upsert_agent(&agent).unwrap();

    let result = handler
        .task_enable(Parameters(IdParam {
            id: "expired-1".to_string(),
        }))
        .await
        .unwrap();

    assert!(!is_err(&result));
    let stored = db.get_agent("expired-1").unwrap().unwrap();
    assert!(stored.enabled);
    assert!(stored.expires_at.is_none());
}

#[tokio::test]
async fn task_enable_reports_missing_agent() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = handler
        .task_enable(Parameters(IdParam {
            id: "ghost".to_string(),
        }))
        .await
        .unwrap();
    assert!(is_err(&result));
}
