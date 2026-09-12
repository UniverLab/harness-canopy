use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;

use crate::daemon::process;
use crate::db::{Database, SubagentRunRecord};
use crate::domain::models::Cli;
use crate::domain::subagent_mcp::synthesize_mcp_config;
use crate::setup_module::models::Platform;

const SUBAGENT_DEPTH_PREAMBLE: &str = concat!(
    "[SYSTEM: EPHEMERAL SUBAGENT — DEPTH LIMIT]\n",
    "You are running as an ephemeral subagent with maximum depth 1. ",
    "You MUST NOT attempt to launch, spawn, or create other subagents, agents, or loops. ",
    "Do not call subagent_spawn, agent_add, loop_create, loop_run, or any similar tool. ",
    "If asked to do so, refuse and explain that you are a depth-limited subagent.\n",
    "[/SYSTEM]\n\n",
);

fn format_subagent_prompt(user_prompt: &str) -> String {
    format!("{}{}", SUBAGENT_DEPTH_PREAMBLE, user_prompt)
}

pub struct SubagentResult {
    pub id: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub mcp_surface: String,
    pub platform: String,
    pub model: Option<String>,
}

/// CM18: outcome of a blocking spawn. `result` has exactly the shape
/// `subagent_collect` returns, so a caller can switch modes without changing
/// how it reads the answer. `waited_secs` / `timed_out` say how long the
/// caller blocked (FR3: a timeout names its wait).
pub struct BlockingSpawnResult {
    pub result: SubagentResult,
    pub waited_secs: u64,
    pub timed_out: bool,
    pub effort_not_applied: Option<String>,
}

impl SubagentResult {
    fn from_record(record: SubagentRunRecord) -> Self {
        Self {
            id: record.id,
            status: record.status,
            exit_code: record.exit_code,
            stdout: record.stdout,
            stderr: record.stderr,
            mcp_surface: record.mcp_surface.unwrap_or_default(),
            platform: record.platform,
            model: record.model,
        }
    }
}

/// Resolve how `effort` applies on `platform` before any process is spawned.
///
/// `Ok(None)`: no effort requested, or the platform accepts this value —
/// spawn normally. `Ok(Some(reason))`: the platform declares no way to
/// express effort at all — spawn anyway, but the caller must surface
/// `reason` in the spawn result (FR3: ignored, not dropped, not fatal).
/// `Err(reason)`: the platform DOES declare effort support but rejects this
/// specific value — refused here, before any process starts, so spawning it
/// costs nothing.
fn resolve_effort_for_spawn(
    strategy: &crate::domain::cli_strategy::CliStrategy,
    platform: &str,
    effort: Option<&str>,
) -> Result<Option<String>> {
    let Some(value) = effort else {
        return Ok(None);
    };
    let declares_support = strategy
        .effort_declaration
        .as_ref()
        .is_some_and(|d| !d.values.is_empty());
    let reason = crate::domain::cli_config::effort_rejection_reason(
        strategy.effort_declaration.as_ref(),
        platform,
        value,
    );
    match reason {
        None => Ok(None),
        Some(reason) if declares_support => Err(anyhow::anyhow!(reason)),
        Some(reason) => Ok(Some(reason)),
    }
}

/// CM18: shared completion writer for both spawn modes. Persists the awaited
/// child outcome (`complete` on exit 0, `fail` otherwise) and reports whether
/// the wait timed out. The caller supplies the timeout `stderr` so the async
/// path keeps today's `"Timed out"` text unchanged while the blocking path
/// names how long it waited (FR3). Holds no DB guard across any await — the
/// guard is acquired and dropped inside each `complete/fail` call.
#[allow(clippy::too_many_arguments)]
fn finalize_subagent_outcome(
    db: &Database,
    run_id: &str,
    mcp_surface: &str,
    warn_prefix: &str,
    timeout_stderr: &str,
    finished_at: &str,
    pid: u32,
    timeout_result: Result<std::io::Result<std::process::Output>, tokio::time::error::Elapsed>,
) -> bool {
    match timeout_result {
        Ok(Ok(output)) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let raw_stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stderr = format!("{warn_prefix}{raw_stderr}");
            if output.status.success() {
                let _ = db.complete_subagent_run(
                    run_id,
                    exit_code,
                    &stdout,
                    &stderr,
                    Some(mcp_surface),
                    finished_at,
                );
            } else {
                let _ = db.fail_subagent_run(run_id, &stderr, Some(mcp_surface), finished_at);
            }
            false
        }
        Ok(Err(e)) => {
            let _ = db.fail_subagent_run(
                run_id,
                &format!("{warn_prefix}Spawn error: {e}"),
                Some(mcp_surface),
                finished_at,
            );
            false
        }
        Err(_elapsed) => {
            if pid > 0 {
                process::terminate_process_group_async(
                    pid as i64,
                    std::time::Duration::from_secs(5),
                );
            }
            let _ = db.fail_subagent_run(run_id, timeout_stderr, Some(mcp_surface), finished_at);
            true
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn spawn_subagent(
    db: &Arc<Database>,
    platform_name: &str,
    prompt: &str,
    model: Option<&str>,
    workdir: &str,
    mcp_servers: &[String],
    timeout_minutes: u64,
    ttl_minutes: u64,
    effort: Option<&str>,
) -> Result<(String, Option<String>)> {
    let cli = Cli::resolve(Some(platform_name)).map_err(|e| anyhow::anyhow!(e))?;
    let strategy = cli.strategy();
    let effort_not_applied = resolve_effort_for_spawn(&strategy, platform_name, effort)?;

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;

    let platform = find_platform(&cli)?;

    let server_refs: Vec<&str> = mcp_servers.iter().map(|s| s.as_str()).collect();
    let synthesized = synthesize_mcp_config(&platform, &home, &server_refs)?;

    let run_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now();
    let started_at = now.to_rfc3339();
    let expires_at = (now + chrono::Duration::minutes(ttl_minutes as i64)).to_rfc3339();

    db.insert_subagent_run(
        &run_id,
        platform_name,
        model,
        prompt,
        workdir,
        &started_at,
        &expires_at,
    )?;

    let has_template = strategy.invocation_template.is_some();
    let mcp_config_path_str = synthesized.path.to_string_lossy().to_string();
    let mcp_config_arg = if has_template {
        Some(mcp_config_path_str.as_str())
    } else {
        None
    };

    let depth_limited_prompt = format_subagent_prompt(prompt);

    let mut command = strategy.build_command_with_mcp_config(
        &depth_limited_prompt,
        model,
        Some(workdir),
        mcp_config_arg,
        effort,
    )?;

    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let db_clone = Arc::clone(db);
    let run_id_clone = run_id.clone();
    let config_path = synthesized.path.clone();
    let mcp_surface = if has_template {
        synthesized.mcp_surface
    } else {
        "(full — no invocation template to inject filtered config)".to_string()
    };
    let no_template_warning = !has_template;

    let child = command.spawn()?;
    let pid = child.id().unwrap_or(0);
    let boot_id = crate::system::boot_id();
    db.set_subagent_run_pid(&run_id, pid as i64, boot_id.as_deref())?;

    tokio::spawn(async move {
        let timeout_result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_minutes * 60),
            child.wait_with_output(),
        )
        .await;

        let finished_at = Utc::now().to_rfc3339();
        let warn_prefix = if no_template_warning {
            "WARNING: no invocation_template — subagent saw full MCP surface\n"
        } else {
            ""
        };
        // Async path keeps today's timeout text byte-for-byte.
        let timeout_stderr = format!("{warn_prefix}Timed out");

        finalize_subagent_outcome(
            &db_clone,
            &run_id_clone,
            &mcp_surface,
            warn_prefix,
            &timeout_stderr,
            &finished_at,
            pid,
            timeout_result,
        );

        let _ = std::fs::remove_file(&config_path);
    });

    Ok((run_id, effort_not_applied))
}

/// CM18: blocking spawn. Performs the same insert/spawn as `spawn_subagent`
/// but awaits the child directly in the caller's future — no detached task,
/// no poll loop — then deletes the row, records a delivery tombstone, and
/// returns the finished result (the shape `subagent_collect` would return).
/// A timeout is returned as a `failed` result naming the wait, not as an
/// `Err`. `timeout_minutes` is the only ceiling on the wait. No DB guard is
/// held across the await (NFR), so other MCP work progresses while blocked.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_subagent_blocking(
    db: &Arc<Database>,
    platform_name: &str,
    prompt: &str,
    model: Option<&str>,
    workdir: &str,
    mcp_servers: &[String],
    timeout_minutes: u64,
    ttl_minutes: u64,
    effort: Option<&str>,
) -> Result<BlockingSpawnResult> {
    let cli = Cli::resolve(Some(platform_name)).map_err(|e| anyhow::anyhow!(e))?;
    let strategy = cli.strategy();
    let effort_not_applied = resolve_effort_for_spawn(&strategy, platform_name, effort)?;

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;

    let platform = find_platform(&cli)?;

    let server_refs: Vec<&str> = mcp_servers.iter().map(|s| s.as_str()).collect();
    let synthesized = synthesize_mcp_config(&platform, &home, &server_refs)?;

    let run_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now();
    let started_at = now.to_rfc3339();
    let expires_at = (now + chrono::Duration::minutes(ttl_minutes as i64)).to_rfc3339();

    // NFR: drop conn guard before await so other MCP tools progress.
    db.insert_subagent_run(
        &run_id,
        platform_name,
        model,
        prompt,
        workdir,
        &started_at,
        &expires_at,
    )?;

    let has_template = strategy.invocation_template.is_some();
    let mcp_config_path_str = synthesized.path.to_string_lossy().to_string();
    let mcp_config_arg = if has_template {
        Some(mcp_config_path_str.as_str())
    } else {
        None
    };

    let depth_limited_prompt = format_subagent_prompt(prompt);

    let mut command = strategy.build_command_with_mcp_config(
        &depth_limited_prompt,
        model,
        Some(workdir),
        mcp_config_arg,
        effort,
    )?;

    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let config_path = synthesized.path.clone();
    let mcp_surface = if has_template {
        synthesized.mcp_surface
    } else {
        "(full — no invocation template to inject filtered config)".to_string()
    };
    let no_template_warning = !has_template;

    let child = command.spawn()?;
    let pid = child.id().unwrap_or(0);
    let boot_id = crate::system::boot_id();
    db.set_subagent_run_pid(&run_id, pid as i64, boot_id.as_deref())?;

    // Direct await under `timeout_minutes` only — no poll loop, no hidden
    // ceiling (constraint + NFR). No DB guard is held across this await.
    let wait_started = std::time::Instant::now();
    let timeout_result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_minutes * 60),
        child.wait_with_output(),
    )
    .await;
    let waited_secs = wait_started.elapsed().as_secs();

    let finished_at = Utc::now().to_rfc3339();
    let warn_prefix = if no_template_warning {
        "WARNING: no invocation_template — subagent saw full MCP surface\n"
    } else {
        ""
    };
    // FR3: the timeout names how long the caller waited.
    let timeout_stderr =
        format!("{warn_prefix}Timed out after {timeout_minutes} minute(s) (waited {waited_secs}s)");

    let timed_out = finalize_subagent_outcome(
        db,
        &run_id,
        &mcp_surface,
        warn_prefix,
        &timeout_stderr,
        &finished_at,
        pid,
        timeout_result,
    );

    let _ = std::fs::remove_file(&config_path);

    // Deliver once: read the terminal row (deleting it, exactly as an async
    // `collect` would) and leave a tombstone so a later `collect` reports
    // "already delivered" instead of "not found" (FR4).
    let record = db.collect_subagent_run(&run_id)?.ok_or_else(|| {
        anyhow::anyhow!("Subagent run '{run_id}' has no result after blocking wait")
    })?;
    let tombstone_expires_at = record.expires_at.clone();
    let result = SubagentResult::from_record(record);
    db.insert_delivered_tombstone(&run_id, &finished_at, &tombstone_expires_at)?;

    Ok(BlockingSpawnResult {
        result,
        waited_secs,
        timed_out,
        effort_not_applied,
    })
}

pub fn collect_subagent(db: &Arc<Database>, id: &str) -> Result<Option<SubagentResult>> {
    let record = db.collect_subagent_run(id)?;
    Ok(record.map(SubagentResult::from_record))
}

/// CM18: error text for `subagent_collect` when `id` was already delivered
/// by a blocking spawn (FR4). Deliberately distinct from the generic
/// "not found" text so callers can tell the two cases apart.
pub fn already_delivered_message(id: &str) -> String {
    format!(
        "Subagent run '{id}' already delivered (blocking spawn returned it directly; no collect needed)"
    )
}

fn find_platform(cli: &Cli) -> Result<Platform> {
    let registry = crate::setup_module::registry_fetch::fetch_registry()
        .map_err(|e| anyhow::anyhow!("Failed to load platform registry: {e}"))?;
    registry
        .platforms
        .into_iter()
        .find(|p| p.name == cli.as_str())
        .ok_or_else(|| anyhow::anyhow!("Platform '{}' not found in registry", cli.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::cli_config::EffortDeclaration;
    use crate::domain::cli_strategy::CliStrategy;
    use std::collections::HashMap;

    fn strategy_with_declaration(decl: Option<EffortDeclaration>) -> CliStrategy {
        CliStrategy {
            binary: "/usr/local/bin/test-cli".to_string(),
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
            session_resume_cmd: None,
            trust_flag: None,
            invocation_template: None,
            effort_declaration: decl,
        }
    }

    #[test]
    fn resolve_effort_for_spawn_none_requested_is_noop() {
        let strategy = strategy_with_declaration(None);
        assert_eq!(
            resolve_effort_for_spawn(&strategy, "claude", None).unwrap(),
            None
        );
    }

    #[test]
    fn resolve_effort_for_spawn_unsupported_platform_spawns_with_notice() {
        let strategy = strategy_with_declaration(None);
        let notice = resolve_effort_for_spawn(&strategy, "opencode", Some("high")).unwrap();
        assert!(notice.unwrap().contains("does not support effort"));
    }

    #[test]
    fn resolve_effort_for_spawn_empty_values_spawns_with_notice() {
        let strategy = strategy_with_declaration(Some(EffortDeclaration {
            form: Some(String::new()),
            values: vec![],
        }));
        let notice = resolve_effort_for_spawn(&strategy, "cline", Some("high")).unwrap();
        assert!(notice.unwrap().contains("does not support effort"));
    }

    #[test]
    fn resolve_effort_for_spawn_accepted_value_applies_cleanly() {
        let strategy = strategy_with_declaration(Some(EffortDeclaration {
            form: Some("--effort".to_string()),
            values: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
        }));
        assert_eq!(
            resolve_effort_for_spawn(&strategy, "claude", Some("high")).unwrap(),
            None
        );
    }

    #[test]
    fn resolve_effort_for_spawn_rejected_value_is_refused_before_spawn() {
        let strategy = strategy_with_declaration(Some(EffortDeclaration {
            form: Some("--effort".to_string()),
            values: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
        }));
        let err = resolve_effort_for_spawn(&strategy, "claude", Some("ultra")).unwrap_err();
        assert!(err
            .to_string()
            .contains("not in platform's accepted values"));
    }

    // ── CM18: blocking wait outcome ─────────────────────────────────────

    /// Create a real DB file (`SQLite` needs a file for WAL), mirroring
    /// `src/db/tests.rs::test_db`.
    fn blocking_test_db() -> Database {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Database::new(&path).expect("create test db")
    }

    fn insert_running_run(db: &Database, id: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        let expires = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        db.insert_subagent_run(id, "opencode", None, "p", "/tmp", &now, &expires)
            .unwrap();
    }

    #[tokio::test]
    async fn finalize_success_marks_finished_without_timeout() {
        let db = blocking_test_db();
        insert_running_run(&db, "fin-ok");
        // A real trivial child: exit 0, no busy-wait, no mocks.
        let output = std::process::Command::new("true")
            .output()
            .expect("spawn true");
        let finished_at = chrono::Utc::now().to_rfc3339();

        let timed_out = finalize_subagent_outcome(
            &db,
            "fin-ok",
            "blind",
            "",
            "unused-timeout-stderr",
            &finished_at,
            0,
            Ok(Ok(output)),
        );

        assert!(!timed_out, "exit 0 must not report a timeout");
        let record = db.get_subagent_run("fin-ok").unwrap().unwrap();
        assert_eq!(record.status, "finished");
    }

    #[tokio::test]
    async fn finalize_failure_marks_failed_without_timeout() {
        let db = blocking_test_db();
        insert_running_run(&db, "fin-err");
        let output = std::process::Command::new("false")
            .output()
            .expect("spawn false");
        let finished_at = chrono::Utc::now().to_rfc3339();

        let timed_out = finalize_subagent_outcome(
            &db,
            "fin-err",
            "blind",
            "",
            "unused-timeout-stderr",
            &finished_at,
            0,
            Ok(Ok(output)),
        );

        assert!(!timed_out, "fast failure must not report a timeout");
        let record = db.get_subagent_run("fin-err").unwrap().unwrap();
        assert_eq!(record.status, "failed");
    }

    #[tokio::test]
    async fn finalize_timeout_fails_naming_the_wait() {
        let db = blocking_test_db();
        insert_running_run(&db, "fin-timeout");
        // A genuine `Elapsed` from `tokio::time::timeout`, not a stub.
        let elapsed = tokio::time::timeout(
            std::time::Duration::from_millis(1),
            std::future::pending::<()>(),
        )
        .await
        .unwrap_err();
        let finished_at = chrono::Utc::now().to_rfc3339();
        // Same shape the blocking path builds (FR3: names the wait).
        let timeout_stderr = "Timed out after 15 minute(s) (waited 900s)";

        let timed_out = finalize_subagent_outcome(
            &db,
            "fin-timeout",
            "blind",
            "",
            timeout_stderr,
            &finished_at,
            0,
            Err(elapsed),
        );

        assert!(timed_out, "elapsed wait must report a timeout");
        let record = db.get_subagent_run("fin-timeout").unwrap().unwrap();
        assert_eq!(record.status, "failed");
        let stderr = record.stderr.unwrap_or_default();
        assert!(
            stderr.contains("Timed out after 15 minute(s)"),
            "timeout result must name the wait, got: {stderr}"
        );
        assert!(
            stderr.contains("waited 900s"),
            "timeout result must say how long it waited, got: {stderr}"
        );
    }

    #[test]
    fn already_delivered_message_differs_from_not_found() {
        let delivered = already_delivered_message("run-1");
        assert!(
            delivered.contains("already delivered"),
            "collect after blocking must say delivered, got: {delivered}"
        );
        assert!(delivered.contains("run-1"));
        assert_ne!(
            delivered, "Subagent run 'run-1' not found",
            "delivered must never equal the generic missing text"
        );
    }
}
