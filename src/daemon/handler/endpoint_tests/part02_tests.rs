//! Part 02 of `endpoint_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

// ── task_schedule_enable / agent_schedule_enable ─────────────

#[tokio::test]
async fn task_schedule_enable_sets_future_enable_time() {
    let (_dir, db, handler) = endpoint_test_handler();
    let mut agent = sample_agent("sched-1", None);
    agent.enabled = false;
    db.upsert_agent(&agent).unwrap();

    let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let result = call_resource_action(
        &handler,
        "agent_edit",
        "schedule",
        params_id(AgentScheduleEnableParams {
            id: "sched-1".to_string(),
            at: at.clone(),
        }),
    )
    .await
    .unwrap();

    assert!(!is_err(&result));
    assert!(text(&result).contains("scheduled to enable"));
}

#[tokio::test]
async fn task_schedule_enable_rejects_bad_timestamp() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.upsert_agent(&sample_agent("sched-2", None)).unwrap();

    let result = call_resource_action(
        &handler,
        "agent_edit",
        "schedule",
        params_id(AgentScheduleEnableParams {
            id: "sched-2".to_string(),
            at: "not-a-timestamp".to_string(),
        }),
    )
    .await
    .unwrap();

    assert!(is_err(&result));
    assert!(text(&result).contains("Invalid ISO 8601"));
}

#[tokio::test]
async fn task_schedule_enable_reports_missing_agent() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let result = call_resource_action(
        &handler,
        "agent_edit",
        "schedule",
        params_id(AgentScheduleEnableParams {
            id: "ghost".to_string(),
            at,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&result));
}

// ── task_disable / agent_disable ─────────────────────────────

#[tokio::test]
async fn task_disable_stops_a_watch_agent() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.upsert_agent(&sample_agent(
        "watch-disable",
        Some(Trigger::Watch {
            path: "/tmp".to_string(),
            events: vec![crate::domain::models::WatchEvent::Modify],
            debounce_seconds: 2,
            recursive: false,
        }),
    ))
    .unwrap();

    let result = call_resource_action(
        &handler,
        "agent_edit",
        "disable",
        params_id(IdParam {
            id: "watch-disable".to_string(),
        }),
    )
    .await
    .unwrap();

    assert!(!is_err(&result));
    let stored = db.get_agent("watch-disable").unwrap().unwrap();
    assert!(!stored.enabled);
}

#[tokio::test]
async fn task_disable_reports_missing_agent() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "agent_edit",
        "disable",
        params_id(IdParam {
            id: "ghost".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&result));
}

// ── agent_run ─────────────────────────────────────────────────

#[tokio::test]
async fn agent_run_launches_existing_agent_in_background() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.upsert_agent(&sample_agent("runnable", None)).unwrap();

    let result = call_resource_action(
        &handler,
        "agent_edit",
        "run",
        params_id(IdParam {
            id: "runnable".to_string(),
        }),
    )
    .await
    .unwrap();

    assert!(!is_err(&result));
    assert!(text(&result).contains("launched in background"));
}

#[tokio::test]
async fn agent_run_reports_missing_agent() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "agent_edit",
        "run",
        params_id(IdParam {
            id: "ghost".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&result));
    assert!(text(&result).contains("No agent found"));
}

// ── task_status / agent_status ───────────────────────────────

#[tokio::test]
async fn task_status_reports_agent_counts() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.upsert_agent(&sample_agent(
        "cron-a",
        Some(Trigger::Cron {
            schedule_expr: "* * * * *".to_string(),
        }),
    ))
    .unwrap();
    db.upsert_agent(&sample_agent("manual-a", None)).unwrap();

    let result = call_resource_action(&handler, "agent", "status", ())
        .await
        .unwrap();
    let out = text(&result);
    assert!(out.contains("canopy v"));
    assert!(out.contains("cron: 1"));
    assert!(out.contains("manual: 1"));
}

// ── task_models / agent_models ───────────────────────────────

// Note: These tests fail in CI due to HOME env var contention between
// concurrent async tests. Marked as ignored until test isolation is improved.
#[tokio::test]
#[ignore]
async fn task_models_rejects_unconfigured_platform() {
    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "echo-cli".to_string(),
            binary: "/bin/echo".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    let _home_guard = HomeVar::set(home.path());

    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "harness",
        "models",
        params_id(TaskModelsParams {
            platform: Some("not-configured-platform".to_string()),
            refresh: None,
            full: None,
        }),
    )
    .await
    .unwrap();

    assert!(is_err(&result));
    assert!(text(&result).contains("is not configured in canopy"));
}

/// CM35 FR2: `agent_models` on an interactive-only platform is a
/// *success* carrying an empty list plus the rejection reason — not an
/// error — and it never runs `models_list_cmd` or falls through to
/// models.dev (the fixture binary would `touch` a marker if it ran).
#[tokio::test]
async fn task_models_returns_empty_list_with_reason_for_interactive_only_platform() {
    let home = tempdir().unwrap();
    let marker = write_freebuff_config(home.path());
    let _home_guard = HomeVar::set(home.path());

    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "harness",
        "models",
        params_id(TaskModelsParams {
            platform: Some("freebuff".to_string()),
            refresh: Some(true),
            full: None,
        }),
    )
    .await
    .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    assert_eq!(body["platform"], serde_json::json!("freebuff"), "{body}");
    assert_eq!(body["models"], serde_json::json!([]), "{body}");
    assert!(
        body["reason"].as_str().is_some_and(
            |r| r.contains("platform 'freebuff' is interactive-only (no headless mode)")
        ),
        "{body}"
    );
    assert!(
        !marker.exists(),
        "no enumeration process may be spawned for an interactive-only platform"
    );
}

#[tokio::test]
async fn task_models_enumerates_native_platform_models() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let script = home.path().join("list-models.sh");
    std::fs::write(&script, "#!/bin/sh\necho model-a\necho model-b\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let config = crate::domain::canopy_config::CanopyConfig {
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "native-cli".to_string(),
            binary: script.to_string_lossy().to_string(),
            models_list_cmd: Some("--list".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    let _home_guard = HomeVar::set(home.path());

    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "harness",
        "models",
        params_id(TaskModelsParams {
            platform: Some("native-cli".to_string()),
            refresh: Some(true),
            full: None,
        }),
    )
    .await
    .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let out = text(&result);
    assert!(out.contains("model-a"));
    assert!(out.contains("model-b"));
    assert!(out.contains("Source:"));
    assert!(
        out.contains("CLI enumeration"),
        "native-enumerated result must state it came from the CLI: {out}"
    );
    assert!(
        !out.contains("PROVIDER'S CATALOG"),
        "native-enumerated result must NOT carry the provider catalogue warning: {out}"
    );
}

#[tokio::test]
async fn task_models_warns_for_platform_without_model_flag() {
    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "mistral".to_string(),
            binary: "/bin/echo".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    // Seed a minimal models.dev catalog so the models.dev path succeeds
    // without a network fetch — `load_catalog_with_source` serves a fresh
    // cache with no network when the file exists and is within TTL.
    let cache_dir = canopy_dir.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let catalog_json = format!(
        r#"{{"models":[{{"id":"mistral-medium-latest","name":"Mistral Medium","provider":"mistral"}}],"fetched_at":{now_secs}}}"#
    );
    std::fs::write(cache_dir.join("models_catalog.json"), catalog_json).unwrap();
    let _home_guard = HomeVar::set(home.path());

    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "harness",
        "models",
        params_id(TaskModelsParams {
            platform: Some("mistral".to_string()),
            refresh: None,
            full: None,
        }),
    )
    .await
    .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let out = text(&result);
    assert!(
        out.contains("does not support model selection") || out.contains("NOT valid input"),
        "must warn that this platform does not support model selection: {out}"
    );
}

#[tokio::test]
async fn task_models_warns_for_native_enumeration_without_model_flag() {
    use std::os::unix::fs::PermissionsExt;

    // FR4: a platform can have a `models_list_cmd` (so `agent_models`
    // takes the native-enumeration path) while still having no
    // `model_flag` — the enumerated ids are not valid `model` input and
    // the output must say so, exactly as the models.dev path does.
    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let script = home.path().join("list-agents.sh");
    std::fs::write(&script, "#!/bin/sh\necho agent-one\necho agent-two\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let config = crate::domain::canopy_config::CanopyConfig {
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "named-agent-cli".to_string(),
            binary: script.to_string_lossy().to_string(),
            models_list_cmd: Some("--list".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    let _home_guard = HomeVar::set(home.path());

    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "harness",
        "models",
        params_id(TaskModelsParams {
            platform: Some("named-agent-cli".to_string()),
            refresh: Some(true),
            full: None,
        }),
    )
    .await
    .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let out = text(&result);
    assert!(out.contains("agent-one"), "enumeration still shown: {out}");
    assert!(
        out.contains("does not support model selection") || out.contains("NOT valid input"),
        "native-enumeration output must also warn when there is no model_flag: {out}"
    );
}

#[tokio::test]
async fn native_result_labels_source_as_cli_and_drops_catalogue_warning() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let script = home.path().join("list-models.sh");
    // Print ids that match the kiro-shaped regression: use the dot
    // separator the CLI accepts, not the hyphenated provider form.
    std::fs::write(
        &script,
        "#!/bin/sh\necho claude-sonnet-4.5\necho deepseek-3.2\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let config = crate::domain::canopy_config::CanopyConfig {
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "kiro-shaped".to_string(),
            binary: script.to_string_lossy().to_string(),
            models_list_cmd: Some("--list".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    let _home_guard = HomeVar::set(home.path());

    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "harness",
        "models",
        params_id(TaskModelsParams {
            platform: Some("kiro-shaped".to_string()),
            refresh: Some(true),
            full: None,
        }),
    )
    .await
    .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let out = text(&result);
    // Must come from the CLI, not the catalogue.
    assert!(
        out.contains("CLI enumeration"),
        "native result must label its source as CLI: {out}"
    );
    assert!(
        !out.contains("PROVIDER'S CATALOG"),
        "native result must NOT carry the catalogue warning: {out}"
    );
    // Regression guard: the kiro-shaped dot-separated ids must be present.
    assert!(
        out.contains("claude-sonnet-4.5"),
        "must include the CLI-accepted id, not the provider form: {out}"
    );
    assert!(
        !out.contains("claude-sonnet-5"),
        "must NOT offer the provider-catalogue id that kiro rejects: {out}"
    );
}

#[tokio::test]
async fn catalogue_result_keeps_cb9_warning_and_labels_source() {
    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    // Seed a models.dev catalog so the catalogue path succeeds without
    // network — same pattern as `task_models_warns_for_platform_without_model_flag`.
    let cache_dir = canopy_dir.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let catalog_json = format!(
        r#"{{"models":[{{"id":"some-model","name":"Some Model","provider":"mistral"}}],"fetched_at":{now_secs}}}"#
    );
    std::fs::write(cache_dir.join("models_catalog.json"), catalog_json).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "mistral".to_string(),
            binary: "/bin/echo".to_string(),
            // No models_list_cmd → catalogue path.
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    let _home_guard = HomeVar::set(home.path());

    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "harness",
        "models",
        params_id(TaskModelsParams {
            platform: Some("mistral".to_string()),
            refresh: None,
            full: None,
        }),
    )
    .await
    .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let out = text(&result);
    assert!(
        out.contains("PROVIDER'S CATALOG"),
        "catalogue result must carry CB9 warning: {out}"
    );
    assert!(
        out.contains("provider catalogue (models.dev)"),
        "catalogue result must label its source: {out}"
    );
}

#[tokio::test]
async fn native_enumeration_failure_falls_back_to_catalogue_and_says_so() {
    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    // Seed a models.dev catalog for the fallback path.
    let cache_dir = canopy_dir.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let catalog_json = format!(
        r#"{{"models":[{{"id":"fallback-model","name":"Fallback","provider":"mistral"}}],"fetched_at":{now_secs}}}"#
    );
    std::fs::write(cache_dir.join("models_catalog.json"), catalog_json).unwrap();

    // Configure a platform whose models_list_cmd binary does NOT exist,
    // so enumeration fails and the fallback triggers. Use `mistral`
    // (a platform with a known provider mapping) so the catalogue
    // fallback has a provider list to render.
    let config = crate::domain::canopy_config::CanopyConfig {
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "mistral".to_string(),
            binary: "/nonexistent/binary".to_string(),
            models_list_cmd: Some("--list".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    let _home_guard = HomeVar::set(home.path());

    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "harness",
        "models",
        params_id(TaskModelsParams {
            platform: Some("mistral".to_string()),
            refresh: None,
            full: None,
        }),
    )
    .await
    .unwrap();

    // Must NOT be an error — the fallback is a degraded but usable result.
    assert!(!is_err(&result), "{}", text(&result));
    let out = text(&result);
    assert!(
        out.contains("enumeration failed"),
        "must explain that enumeration failed: {out}"
    );
    // The catalogue models must still be present as fallback.
    assert!(
        out.contains("fallback-model"),
        "catalogue fallback models must be present: {out}"
    );
}

#[tokio::test]
async fn native_cache_and_refresh_match_catalogue_semantics() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let script = home.path().join("list-models.sh");
    std::fs::write(&script, "#!/bin/sh\necho model-x\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let config = crate::domain::canopy_config::CanopyConfig {
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "cache-test-cli".to_string(),
            binary: script.to_string_lossy().to_string(),
            models_list_cmd: Some("--list".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    let _home_guard = HomeVar::set(home.path());

    let (_dir, _db, handler) = endpoint_test_handler();

    // First call with refresh: true → should be Live.
    let result1 = call_resource_action(
        &handler,
        "harness",
        "models",
        params_id(TaskModelsParams {
            platform: Some("cache-test-cli".to_string()),
            refresh: Some(true),
            full: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&result1), "{}", text(&result1));
    let out1 = text(&result1);
    assert!(
        out1.contains("live"),
        "first call with refresh should be live: {out1}"
    );

    // Second call without refresh → should be Cache (served from the
    // fresh cache populated by the first call).
    let result2 = call_resource_action(
        &handler,
        "harness",
        "models",
        params_id(TaskModelsParams {
            platform: Some("cache-test-cli".to_string()),
            refresh: None,
            full: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&result2), "{}", text(&result2));
    let out2 = text(&result2);
    assert!(
        out2.contains("cache"),
        "second call without refresh should be cache: {out2}"
    );
}

#[tokio::test]
async fn catalogue_platform_without_provider_mapping_says_so_not_network_error() {
    // A platform that is configured but has no models.dev provider mapping
    // and no `models_list_cmd` must report *that* — not a misleading
    // "could not reach models.dev". FR3: behaviour here is unchanged from
    // before native enumeration was factored out.
    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    // Seed a fresh catalog so the failure cannot be blamed on the network.
    let cache_dir = canopy_dir.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let catalog_json = format!(
        r#"{{"models":[{{"id":"some-model","name":"Some Model","provider":"mistral"}}],"fetched_at":{now_secs}}}"#
    );
    std::fs::write(cache_dir.join("models_catalog.json"), catalog_json).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        clis: vec![crate::domain::cli_config::CliConfig {
            // Not among `providers_for_cli`'s arms → empty provider list.
            name: "unmapped-cli".to_string(),
            binary: "/bin/echo".to_string(),
            // No models_list_cmd → catalogue path.
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    let _home_guard = HomeVar::set(home.path());

    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "harness",
        "models",
        params_id(TaskModelsParams {
            platform: Some("unmapped-cli".to_string()),
            refresh: None,
            full: None,
        }),
    )
    .await
    .unwrap();

    assert!(is_err(&result), "{}", text(&result));
    let out = text(&result);
    assert!(
        out.contains("No known model providers are mapped for platform 'unmapped-cli'"),
        "must name the real cause (no provider mapping): {out}"
    );
    assert!(
        !out.contains("could not reach models.dev"),
        "must NOT blame the network when the catalog is reachable: {out}"
    );
}

// ── task_logs / agent_logs ───────────────────────────────────

#[tokio::test]
async fn task_logs_reports_no_logs_for_unexecuted_agent() {
    let (dir, db, handler) = endpoint_test_handler();
    let mut agent = sample_agent("no-logs-yet", None);
    agent.log_path = dir
        .path()
        .join("no-logs-yet.log")
        .to_string_lossy()
        .to_string();
    db.upsert_agent(&agent).unwrap();

    let result = call_resource_action(
        &handler,
        "agent",
        "logs",
        params_id(TaskLogsParams {
            id: "no-logs-yet".to_string(),
            lines: None,
            since: None,
        }),
    )
    .await
    .unwrap();

    assert!(!is_err(&result));
    assert!(text(&result).contains("No logs found"));
}

#[tokio::test]
async fn task_logs_returns_recent_lines() {
    let (dir, db, handler) = endpoint_test_handler();
    let log_path = dir.path().join("has-logs.log");
    std::fs::write(&log_path, "line one\nline two\nline three\n").unwrap();
    let mut agent = sample_agent("has-logs", None);
    agent.log_path = log_path.to_string_lossy().to_string();
    db.upsert_agent(&agent).unwrap();

    let result = call_resource_action(
        &handler,
        "agent",
        "logs",
        params_id(TaskLogsParams {
            id: "has-logs".to_string(),
            lines: Some(2),
            since: None,
        }),
    )
    .await
    .unwrap();

    let out = text(&result);
    assert!(out.contains("line two"));
    assert!(out.contains("line three"));
    assert!(!out.contains("line one"));
}

// ── task_update / agent_update ───────────────────────────────

#[tokio::test]
async fn task_update_renames_and_updates_prompt() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (_dir, db, handler) = endpoint_test_handler();
    db.upsert_agent(&sample_agent(
        "old-id",
        Some(Trigger::Cron {
            schedule_expr: "* * * * *".to_string(),
        }),
    ))
    .unwrap();

    let result = call_resource_action(
        &handler,
        "agent_edit",
        "update",
        params_id(TaskUpdateParams {
            delivery_action: None,
            target_session_name: None,
            target_session_id: None,
            id: "old-id".to_string(),
            new_id: Some("new-id".to_string()),
            prompt: Some("updated prompt".to_string()),
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
    )
    .await
    .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    assert!(db.get_agent("old-id").unwrap().is_none());
    let renamed = db.get_agent("new-id").unwrap().unwrap();
    assert_eq!(renamed.prompt, "updated prompt");
}

#[tokio::test]
async fn task_update_reports_missing_agent() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (_dir, _db, handler) = endpoint_test_handler();

    let result = call_resource_action(
        &handler,
        "agent_edit",
        "update",
        params_id(TaskUpdateParams {
            delivery_action: None,
            target_session_name: None,
            target_session_id: None,
            id: "ghost".to_string(),
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
    )
    .await
    .unwrap();

    assert!(is_err(&result));
    assert!(text(&result).contains("No agent found"));
}

#[tokio::test]
async fn task_update_rejects_invalid_new_id() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (_dir, db, handler) = endpoint_test_handler();
    db.upsert_agent(&sample_agent("keep-id", None)).unwrap();

    let result = call_resource_action(
        &handler,
        "agent_edit",
        "update",
        params_id(TaskUpdateParams {
            delivery_action: None,
            target_session_name: None,
            target_session_id: None,
            id: "keep-id".to_string(),
            new_id: Some("bad id!".to_string()),
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
    )
    .await
    .unwrap();

    assert!(is_err(&result));
    assert!(db.get_agent("keep-id").unwrap().is_some());
}

#[tokio::test]
async fn task_report_transitions_in_progress_to_success() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.upsert_agent(&sample_agent("reporter", None)).unwrap();
    db.insert_run(&sample_run("run-1", "reporter", RunStatus::InProgress))
        .unwrap();

    let result = handler
        .task_report(Parameters(TaskReportParams {
            run_id: "run-1".to_string(),
            status: "success".to_string(),
            summary: Some("all good".to_string()),
        }))
        .await
        .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let run = db.get_run("run-1").unwrap().unwrap();
    assert!(matches!(run.status, RunStatus::Success));
    let agent = db.get_agent("reporter").unwrap().unwrap();
    assert_eq!(agent.last_run_ok, Some(true));
}

#[tokio::test]
async fn task_report_rejects_invalid_status() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = handler
        .task_report(Parameters(TaskReportParams {
            run_id: "run-x".to_string(),
            status: "sideways".to_string(),
            summary: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&result));
    assert!(text(&result).contains("Invalid status"));
}

#[tokio::test]
async fn task_report_requires_summary_on_success() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.upsert_agent(&sample_agent("reporter-2", None)).unwrap();
    db.insert_run(&sample_run("run-2", "reporter-2", RunStatus::InProgress))
        .unwrap();

    let result = handler
        .task_report(Parameters(TaskReportParams {
            run_id: "run-2".to_string(),
            status: "success".to_string(),
            summary: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&result));
    assert!(text(&result).contains("summary is required"));
}

#[tokio::test]
async fn task_report_rejects_invalid_transition() {
    let (_dir, db, handler) = endpoint_test_handler();
    db.upsert_agent(&sample_agent("reporter-3", None)).unwrap();
    // Already terminal (Success) — Success -> Success is not a valid
    // transition per validate_run_transition.
    db.insert_run(&sample_run("run-3", "reporter-3", RunStatus::Success))
        .unwrap();

    let result = handler
        .task_report(Parameters(TaskReportParams {
            run_id: "run-3".to_string(),
            status: "success".to_string(),
            summary: Some("again".to_string()),
        }))
        .await
        .unwrap();
    assert!(is_err(&result));
    assert!(text(&result).contains("Invalid transition"));
}

#[tokio::test]
async fn task_report_errors_on_unknown_run_id() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = handler
        .task_report(Parameters(TaskReportParams {
            run_id: "no-such-run".to_string(),
            status: "success".to_string(),
            summary: Some("done".to_string()),
        }))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn graph_create_and_update_round_trip() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();

    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "My Graph".to_string(),
            description: None,
            workdir: workdir.clone(),
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    let graph_id = extract_id(&created, "graph_id");
    assert!(db.get_graph(&graph_id).unwrap().is_some());

    let updated = call_graph_action(
        &handler,
        "graph_edit",
        "update",
        params_id(GraphUpdateParams {
            graph_id: graph_id.clone(),
            name: Some("Renamed Graph".to_string()),
            description: None,
            workdir: None,
            trigger: None,
            on_completed: None,
            hooks: None,
            infra_node_id: None,
            allow_dirty_start: None,
            require_spec_type: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().name,
        "Renamed Graph"
    );
}

#[tokio::test]
async fn graph_update_response_names_remaining_events_when_run_active() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Active Hook Graph".to_string(),
            description: None,
            workdir,
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    let graph_id = extract_id(&created, "graph_id");

    db.update_graph_status(
        &graph_id,
        GraphStatus::Running,
        Some(chrono::Utc::now()),
        None,
    )
    .unwrap();

    let mut hooks = std::collections::BTreeMap::new();
    for event in ["on_completed", "on_spec_completed"] {
        hooks.insert(
            event.to_string(),
            vec![GraphCompletionHookParams {
                platform: None,
                model: None,
                effort: None,
                prompt: None,
                command: Some("printf ok".to_string()),
                target_session_id: None,
                target_session_name: None,
                timeout_minutes: Some(1),
                target_graph_id: None,
                queue_id: None,
                workdir_override: None,
                idea: None,

                crew: None,
            }],
        );
    }
    let updated = call_graph_action(
        &handler,
        "graph_edit",
        "update",
        params_id(GraphUpdateParams {
            graph_id: graph_id.clone(),
            name: None,
            description: None,
            workdir: None,
            trigger: None,
            on_completed: None,
            hooks: Some(hooks),
            infra_node_id: None,
            allow_dirty_start: None,
            require_spec_type: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    let message = text(&updated);
    assert!(message.contains("remaining events"), "{message}");
    assert!(message.contains("on_completed"), "{message}");
    assert!(message.contains("on_spec_completed"), "{message}");
}

#[tokio::test]
async fn graph_update_response_has_no_hook_note_without_active_run() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Idle Hook Graph".to_string(),
            description: None,
            workdir,
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created));
    let graph_id = extract_id(&created, "graph_id");

    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        "on_completed".to_string(),
        vec![GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: Some("printf ok".to_string()),
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(1),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,

            crew: None,
        }],
    );
    let updated = call_graph_action(
        &handler,
        "graph_edit",
        "update",
        params_id(GraphUpdateParams {
            graph_id: graph_id.clone(),
            name: None,
            description: None,
            workdir: None,
            trigger: None,
            on_completed: None,
            hooks: Some(hooks),
            infra_node_id: None,
            allow_dirty_start: None,
            require_spec_type: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    assert!(!text(&updated).contains("remaining events"));
}

#[tokio::test]
async fn graph_update_rejects_unknown_hook_placeholder_before_persisting() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Hook Graph".to_string(),
            description: None,
            workdir,
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let mut valid_hooks = std::collections::BTreeMap::new();
    valid_hooks.insert(
        "on_failed".to_string(),
        vec![GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: Some("printf '{{node}}'".to_string()),
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(1),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,

            crew: None,
        }],
    );
    let updated = call_graph_action(
        &handler,
        "graph_edit",
        "update",
        params_id(GraphUpdateParams {
            graph_id: graph_id.clone(),
            name: None,
            description: None,
            workdir: None,
            trigger: None,
            on_completed: None,
            hooks: Some(valid_hooks),
            infra_node_id: None,
            allow_dirty_start: None,
            require_spec_type: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    let before = db.get_graph(&graph_id).unwrap().unwrap().hooks;

    let mut invalid_hooks = std::collections::BTreeMap::new();
    invalid_hooks.insert(
        "on_failed".to_string(),
        vec![GraphCompletionHookParams {
            platform: None,
            model: None,
            effort: None,
            prompt: Some("failure at {{nodee}}".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(1),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,

            crew: None,
        }],
    );
    let rejected = call_graph_action(
        &handler,
        "graph_edit",
        "update",
        params_id(GraphUpdateParams {
            graph_id: graph_id.clone(),
            name: None,
            description: None,
            workdir: None,
            trigger: None,
            on_completed: None,
            hooks: Some(invalid_hooks),
            infra_node_id: None,
            allow_dirty_start: None,
            require_spec_type: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&rejected));
    let message = text(&rejected);
    assert!(message.contains("nodee"), "{message}");
    for marker in crate::graph_engine::GRAPH_HOOK_BINDINGS {
        assert!(message.contains(marker), "{message}");
    }
    let after = db.get_graph(&graph_id).unwrap().unwrap().hooks;
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
}

#[tokio::test]
async fn graph_create_rejects_empty_name_and_relative_workdir() {
    let (_dir, _db, handler) = endpoint_test_handler();

    let empty_name = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "  ".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&empty_name));

    let bad_workdir = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Graph".to_string(),
            description: None,
            workdir: "relative/dir".to_string(),
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&bad_workdir));
    assert!(text(&bad_workdir).contains("absolute") || text(&bad_workdir).contains("exist"));
}

#[tokio::test]
async fn graph_export_returns_error_for_unknown_graph() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_graph_action(
        &handler,
        "graph",
        "export",
        params_id(GraphExportParams {
            graph_id: "missing".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&result));
    assert!(text(&result).contains("not found"));
}

#[tokio::test]
async fn graph_export_always_includes_platform_and_model() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let (graph_id, ..) = build_simple_graph(&handler, &workdir, "Export Graph").await;

    let exported = call_graph_action(
        &handler,
        "graph",
        "export",
        params_id(GraphExportParams { graph_id }),
    )
    .await
    .unwrap();
    assert!(!is_err(&exported), "{}", text(&exported));
    let doc: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();
    assert_eq!(doc["format_version"], 6);
    let implementer = doc["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "implementer")
        .unwrap();
    assert_eq!(implementer["config"]["platform"], "claude");
    // build_simple_graph's agent configs carry no model: export states
    // the platform default explicitly as null.
    assert!(implementer["config"].get("model").is_some());
    assert!(implementer["config"]["model"].is_null());
}

/// Decision 2's enforced consequence: `graph_node` doesn't itself
/// forbid two nodes sharing a name, so export must catch it — naming
/// the offending node(s) rather than producing an ambiguous file.
#[tokio::test]
async fn graph_export_rejects_duplicate_node_names() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Dup Graph".to_string(),
            description: None,
            workdir,
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");
    for _ in 0..2 {
        call_graph_action(
            &handler,
            "graph_node",
            "add",
            params_id(GraphAddNodeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                name: "dup".to_string(),
                kind: Some("check".to_string()),
                config: Some(check_node_config("true")),
                blueprint: None,
                node: None,
                config_overrides: None,
            }),
        )
        .await
        .unwrap();
    }

    let result = call_graph_action(
        &handler,
        "graph",
        "export",
        params_id(GraphExportParams { graph_id }),
    )
    .await
    .unwrap();
    assert!(is_err(&result));
    assert!(text(&result).contains("dup"));
    assert!(text(&result).to_lowercase().contains("duplicate"));
}

#[tokio::test]
async fn graph_import_creates_new_graph_and_reports_missing_platform() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let (source_graph_id, ..) = build_simple_graph(&handler, &workdir, "Source Graph").await;

    let exported = call_graph_action(
        &handler,
        "graph",
        "export",
        params_id(GraphExportParams {
            graph_id: source_graph_id,
        }),
    )
    .await
    .unwrap();
    let mut document: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();
    // Simulate a v1-style unbound document: strip the harness bindings
    // the v2 export now always carries, so import must report them.
    document["format_version"] = serde_json::json!(1);
    for node in document["nodes"].as_array_mut().unwrap().iter_mut() {
        if let Some(config) = node["config"].as_object_mut() {
            config.remove("platform");
            config.remove("model");
        }
    }

    let imported = call_graph_action(
        &handler,
        "graph_edit",
        "import",
        params_id(GraphImportParams {
            document,
            workdir: workdir.clone(),
            name: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&imported), "{}", text(&imported));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
    // The source graph itself is still named "Source Graph" in this same
    // workdir, so decision 4's collision handling suffixes the import.
    assert_eq!(body["name"], "Source Graph (2)");
    let new_graph_id = body["graph_id"].as_str().unwrap().to_string();
    assert_ne!(new_graph_id, "");

    let missing: Vec<&str> = body["nodes_missing_platform"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(missing.contains(&"implementer"));
    assert!(missing.contains(&"committer"));
    assert!(body["terminals"]
        .as_array()
        .is_some_and(|terminals| !terminals.is_empty()));

    let nodes = db.list_graph_nodes_for_graph(&new_graph_id).unwrap();
    assert_eq!(nodes.len(), 3);
    let edges = db.list_graph_edges_for_graph(&new_graph_id).unwrap();
    assert_eq!(edges.len(), 2);
    // Import always creates a new graph, never touching the source.
    assert_eq!(db.list_graphs(Some(&workdir), true).unwrap().len(), 2);
}

#[tokio::test]
async fn graph_import_name_param_overrides_document_name() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let (source_graph_id, ..) = build_simple_graph(&handler, &workdir, "Source Graph").await;
    let exported = call_graph_action(
        &handler,
        "graph",
        "export",
        params_id(GraphExportParams {
            graph_id: source_graph_id,
        }),
    )
    .await
    .unwrap();
    let document: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();

    let imported = call_graph_action(
        &handler,
        "graph_edit",
        "import",
        params_id(GraphImportParams {
            document,
            workdir,
            name: Some("Custom Name".to_string()),
        }),
    )
    .await
    .unwrap();
    let body: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
    assert_eq!(body["name"], "Custom Name");
}
