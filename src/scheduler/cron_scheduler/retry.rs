//! Retry policy and run-with-retry loop for the cron scheduler.
//!
//! A cron miss/failure (e.g. a CLI hitting its quota) used to wait for the
//! next scheduled slot — hours away. With retry enabled, a failing run is
//! re-attempted after `delay_minutes`, up to `max_retries` times, without
//! disturbing the regular cron schedule.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::domain::models::Agent;
use crate::executor::Executor;

/// How a failed scheduled run is retried, independently of the cron slot.
///
/// Configurable via environment (read once at scheduler construction):
/// - `CANOPY_RETRY_ENABLED`      (bool, default true)
/// - `CANOPY_RETRY_DELAY_MINUTES` (u64,  default 60)
/// - `CANOPY_RETRY_MAX`          (u32,  default 3)
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub enabled: bool,
    pub delay_minutes: u64,
    pub max_retries: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            delay_minutes: 60,
            max_retries: 3,
        }
    }
}

impl RetryPolicy {
    /// Build from environment variables, falling back to defaults.
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            enabled: std::env::var("CANOPY_RETRY_ENABLED")
                .ok()
                .and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => Some(true),
                    "0" | "false" | "no" | "off" => Some(false),
                    _ => None,
                })
                .unwrap_or(d.enabled),
            delay_minutes: std::env::var("CANOPY_RETRY_DELAY_MINUTES")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(d.delay_minutes),
            max_retries: std::env::var("CANOPY_RETRY_MAX")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(d.max_retries),
        }
    }
}

/// Whether a run outcome counts as a failure eligible for retry.
///
/// An `Err` (spawn/IO failure) and any non-zero exit code are failures.
/// A clean exit (code 0) is a success. Callers treat lock-skips — which
/// surface as an `Ok` with the process's own code — like any other run.
pub(super) fn run_outcome_is_failure(outcome: &anyhow::Result<i32>) -> bool {
    !matches!(outcome, Ok(0))
}

/// Given a failed run, decide whether another attempt is warranted.
/// `attempt` is the 0-based index of the attempt that just failed.
pub(super) fn should_retry(retry: &RetryPolicy, attempt: u32) -> bool {
    retry.enabled && attempt < retry.max_retries
}

/// Run a scheduled agent, retrying on failure per [`RetryPolicy`].
///
/// The first attempt runs immediately (the cron slot fired). On failure,
/// waits `delay_minutes` and re-runs, up to `max_retries` extra attempts.
/// The wait is cancellation-aware, so a stopping daemon does not leave a
/// pending retry sleeping.
pub(super) async fn run_with_retry(
    executor: Arc<Executor>,
    agent: Agent,
    retry: RetryPolicy,
    cancel: CancellationToken,
) {
    let mut attempt: u32 = 0;
    loop {
        let outcome = executor.execute_agent(&agent, false).await;
        if !run_outcome_is_failure(&outcome) {
            log_scheduled_agent_success(&agent, &outcome);
            return;
        }

        log_scheduled_agent_failure(&agent, &outcome, attempt);

        if !should_retry(&retry, attempt) {
            log_scheduled_retries_exhausted(&retry, &agent);
            return;
        }

        attempt += 1;
        log_scheduled_retry_scheduled(&agent, attempt, &retry);
        if !wait_retry_delay(&retry, &cancel, &agent).await {
            return;
        }
    }
}

/// Log the successful completion of a scheduled run.
pub(super) fn log_scheduled_agent_success(agent: &Agent, outcome: &anyhow::Result<i32>) {
    if let Ok(code) = outcome {
        tracing::info!(
            "Scheduled agent '{}' completed (exit code: {})",
            agent.id,
            code
        );
    }
}

/// Log which kind of failure a scheduled attempt ended in (exit code vs.
/// spawn/IO error), numbered by attempt.
pub(super) fn log_scheduled_agent_failure(
    agent: &Agent,
    outcome: &anyhow::Result<i32>,
    attempt: u32,
) {
    match outcome {
        Ok(code) => tracing::warn!(
            "Scheduled agent '{}' failed (exit code: {}), attempt {}",
            agent.id,
            code,
            attempt + 1
        ),
        Err(e) => tracing::error!(
            "Scheduled agent '{}' failed: {}, attempt {}",
            agent.id,
            e,
            attempt + 1
        ),
    }
}

/// Warn that the retry budget is used up and the agent waits for its next
/// cron slot (only when retries are enabled at all).
pub(super) fn log_scheduled_retries_exhausted(retry: &RetryPolicy, agent: &Agent) {
    if retry.enabled {
        tracing::warn!(
            "Scheduled agent '{}' exhausted {} retries; waiting for next cron slot",
            agent.id,
            retry.max_retries
        );
    }
}

/// Announce the scheduled retry that was just queued.
pub(super) fn log_scheduled_retry_scheduled(agent: &Agent, attempt: u32, retry: &RetryPolicy) {
    tracing::info!(
        "Scheduled agent '{}' will retry ({}/{}) in {} min",
        agent.id,
        attempt,
        retry.max_retries,
        retry.delay_minutes
    );
}

/// Wait out the retry delay; returns `false` when the wait was cut short by
/// cancellation (the daemon is stopping), `true` when it elapsed fully.
pub(super) async fn wait_retry_delay(
    retry: &RetryPolicy,
    cancel: &CancellationToken,
    agent: &Agent,
) -> bool {
    let wait = Duration::from_secs(retry.delay_minutes.saturating_mul(60));
    tokio::select! {
        _ = tokio::time::sleep(wait) => true,
        _ = cancel.cancelled() => {
            tracing::info!("Retry for agent '{}' cancelled (daemon stopping)", agent.id);
            false
        }
    }
}
