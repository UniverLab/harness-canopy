//! Host metrics over a device peer's data channel: one `host.metrics` frame
//! per request, or a push every N seconds while `host.metrics.watch` stands.
//!
//! Frames carry the sampler's last sample ([`HostMetrics`]), never a fresh
//! sample: a remote watcher adds no sampler work of its own.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::sessions::ws::OutTx;
use crate::system::host_metrics::HostMetrics;

/// Where a bridge reads its metrics: the sampler's [`snapshot`] in
/// production, a fake in tests.
///
/// [`snapshot`]: crate::system::host_metrics::snapshot
pub(crate) type MetricsSource = Arc<dyn Fn() -> HostMetrics + Send + Sync>;

/// Clamp the watch interval to 5..=60 seconds, defaulting to 10.
pub(crate) fn clamp_interval(interval_s: Option<u64>) -> u64 {
    interval_s.unwrap_or(10).clamp(5, 60)
}

/// `host.metrics{req?,metrics}`: `req` echoes only on the one-shot answer,
/// never on watch pushes.
pub(crate) fn host_metrics_frame(req: Option<&str>, metrics: &HostMetrics) -> String {
    let mut frame = serde_json::json!({"t": "host.metrics", "metrics": metrics});
    if let Some(req) = req {
        frame["req"] = serde_json::Value::String(req.to_string());
    }
    frame.to_string()
}

/// One channel's metrics watch: dropping it aborts the push task, so a
/// replaced watch, an `unwatch` and a closed channel all end their pushes.
pub(crate) struct MetricsWatch {
    task: JoinHandle<()>,
}

impl Drop for MetricsWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
impl MetricsWatch {
    pub(crate) fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

/// Push `source()` every `interval` until the channel closes. The first
/// push is immediate; a failed send ends the task.
pub(crate) fn spawn_metrics_watch(
    source: MetricsSource,
    out: OutTx,
    interval: Duration,
) -> MetricsWatch {
    let task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            if out.send(host_metrics_frame(None, &source())).is_err() {
                return;
            }
        }
    });
    MetricsWatch { task }
}

/// Answer one `host.metrics` request from the sampler's last sample.
pub(crate) fn handle_host_metrics(source: &MetricsSource, out: &OutTx, req: &str) {
    let _ = out.send(host_metrics_frame(Some(req), &source()));
}

/// Start the channel's metrics push, replacing any previous watch.
pub(crate) fn handle_host_metrics_watch(
    watch: &mut Option<MetricsWatch>,
    source: &MetricsSource,
    out: &OutTx,
    interval_s: Option<u64>,
) {
    let interval = Duration::from_secs(clamp_interval(interval_s));
    *watch = Some(spawn_metrics_watch(source.clone(), out.clone(), interval));
}

/// Stop the channel's metrics push; answers nothing.
pub(crate) fn handle_host_metrics_unwatch(watch: &mut Option<MetricsWatch>) {
    *watch = None;
}
