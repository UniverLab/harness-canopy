//! Retry policy and run-with-retry loop for the cron scheduler.
//!
//! A cron miss/failure (e.g. a CLI hitting its quota) used to wait for the
//! next scheduled slot — hours away. With retry enabled, a failing run is
//! re-attempted after `delay_minutes`, up to `max_retries` times, without
//! disturbing the regular cron schedule.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::domain::models::Agent;

/// How a failed scheduled run is retried, independently of the cron slot.
///
/// Configurable through the `[scheduler]` table in `config.toml`
/// (`retry_enabled`, `retry_delay_minutes`, `retry_max`); the
/// `CANOPY_RETRY_ENABLED`, `CANOPY_RETRY_DELAY_MINUTES` and
/// `CANOPY_RETRY_MAX` environment variables still override those when set.
/// The defaults live in the CM51 settings registry.
///
/// Legacy agent-retry policy: kept for API stability (`with_retry`) and its
/// unit tests. Failure handling for scheduled work now lives in the graph
/// engine.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub enabled: bool,
    pub delay_minutes: u64,
    pub max_retries: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            enabled: crate::domain::settings::default_bool("scheduler.retry_enabled"),
            delay_minutes: crate::domain::settings::default_duration_units(
                "scheduler.retry_delay_minutes",
            ),
            max_retries: crate::domain::settings::default_int("scheduler.retry_max") as u32,
        }
    }
}

impl RetryPolicy {
    /// Build from the loaded config's `[scheduler]` table (which already
    /// carries any `CANOPY_RETRY_*` environment override).
    pub fn from_config(config: &crate::domain::canopy_config::SchedulerConfig) -> Self {
        Self {
            enabled: config.retry_enabled,
            delay_minutes: config.retry_delay_minutes,
            max_retries: config.retry_max,
        }
    }
}

/// Whether a run outcome counts as a failure eligible for retry.
///
/// An `Err` (spawn/IO failure) and any non-zero exit code are failures.
/// A clean exit (code 0) is a success. Callers treat lock-skips — which
/// surface as an `Ok` with the process's own code — like any other run.
/// Kept for the unit tests; production retries run through the graph engine.
#[allow(dead_code)]
pub(super) fn run_outcome_is_failure(outcome: &anyhow::Result<i32>) -> bool {
    !matches!(outcome, Ok(0))
}

/// Given a failed run, decide whether another attempt is warranted.
/// `attempt` is the 0-based index of the attempt that just failed.
/// Kept for the unit tests; production retries run through the graph engine.
#[allow(dead_code)]
pub(super) fn should_retry(retry: &RetryPolicy, attempt: u32) -> bool {
    retry.enabled && attempt < retry.max_retries
}

/// Legacy scheduled-agent retry loop (`run_with_retry`, which ran the agent
/// through the executor on every cron fire) and its log helpers are deleted:
/// agents fire through their backing graphs since the agent-to-graph
/// migration, and the graph engine owns failure handling. [`RetryPolicy`],
/// [`should_retry`] and [`wait_retry_delay`] stay for API stability and
/// their unit tests.
///
/// Wait out the retry delay; returns `false` when the wait was cut short by
/// cancellation (the daemon is stopping), `true` when it elapsed fully.
/// Kept for the unit tests; production retries run through the graph engine.
#[allow(dead_code)]
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
