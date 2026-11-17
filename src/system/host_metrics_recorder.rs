//! Per-10-second host-metrics history: one JSON line per sample appended to
//! `~/.canopy/metrics/host-YYYY-MM-DD.jsonl` (UTC day), `sync_data`ed before
//! the sampler sleeps again so a hard power-off loses at most the sample in
//! flight.
//!
//! [`super::host_metrics`] stays the remote-contract cache (`snapshot()`
//! warms it on first call); this module owns the file, loop and lag logic.
//! A write failure is throttled to one `tracing::warn!` per minute and never
//! stops the sampler or the daemon.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use serde::Serialize;

use super::host_metrics::{GpuMetrics, PowerMetrics};
use crate::db::Database;
use crate::sessions::hub::SessionHub;

/// Seconds between samples. The spec pins 10 s: do not sample faster.
const SAMPLE_SECS: u64 = 10;
/// Seconds between retention sweeps.
const PRUNE_SECS: u64 = 3_600;
/// Minimum seconds between write-failure warnings.
const WARN_THROTTLE_SECS: u64 = 60;
/// Bytes per MiB.
const BYTES_PER_MB: u64 = 1_048_576;

/// One of the 5 hottest processes on the machine. `name` is the process
/// name only — never the command line, which can hold secrets.
#[derive(Debug, Clone, Serialize)]
pub struct TopProc {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub rss_mb: u64,
}

/// One JSONL line: the seven [`super::host_metrics::HostMetrics`] keys the
/// spec enumerates (deliberately no `sampled_at`; the line carries its own
/// `at`) plus the recorder's own readings.
#[derive(Debug, Clone, Serialize)]
pub struct HostMetricsLine {
    pub at: String,
    pub cpu_usage: Option<f32>,
    pub cpu_temperature_c: Option<f32>,
    pub cpu_temp_source: Option<String>,
    pub memory_used_mb: Option<u64>,
    pub memory_total_mb: Option<u64>,
    pub gpu: Option<GpuMetrics>,
    pub power: Option<PowerMetrics>,
    pub swap_used_mb: u64,
    pub load1: Option<f64>,
    pub load5: Option<f64>,
    pub load15: Option<f64>,
    pub daemon_cpu: f32,
    pub daemon_rss_mb: u64,
    pub running_nodes: u64,
    pub live_sessions: usize,
    pub loop_lag_ms: u64,
    pub top_procs: Vec<TopProc>,
}

/// The gathered readings one line is built from. Tests construct this
/// directly; the sampler builds it from the live system.
pub struct LineParts {
    pub cpu_usage: Option<f32>,
    pub cpu_temperature_c: Option<f32>,
    pub cpu_temp_source: Option<String>,
    pub memory_used_mb: Option<u64>,
    pub memory_total_mb: Option<u64>,
    pub gpu: Option<GpuMetrics>,
    pub power: Option<PowerMetrics>,
    pub swap_used_mb: u64,
    pub load1: Option<f64>,
    pub load5: Option<f64>,
    pub load15: Option<f64>,
    pub daemon_cpu: f32,
    pub daemon_rss_mb: u64,
    pub running_nodes: u64,
    pub live_sessions: usize,
    pub loop_lag_ms: u64,
    pub top_procs: Vec<TopProc>,
}

/// Render one JSON object line (no trailing newline; [`write_line`] adds it).
pub fn build_line(at: &str, parts: LineParts) -> String {
    let line = HostMetricsLine {
        at: at.to_string(),
        cpu_usage: parts.cpu_usage,
        cpu_temperature_c: parts.cpu_temperature_c,
        cpu_temp_source: parts.cpu_temp_source,
        memory_used_mb: parts.memory_used_mb,
        memory_total_mb: parts.memory_total_mb,
        gpu: parts.gpu,
        power: parts.power,
        swap_used_mb: parts.swap_used_mb,
        load1: parts.load1,
        load5: parts.load5,
        load15: parts.load15,
        daemon_cpu: parts.daemon_cpu,
        daemon_rss_mb: parts.daemon_rss_mb,
        running_nodes: parts.running_nodes,
        live_sessions: parts.live_sessions,
        loop_lag_ms: parts.loop_lag_ms,
        top_procs: parts.top_procs,
    };
    serde_json::to_string(&line).unwrap_or_else(|_| String::from("{}"))
}

/// Up to 5 processes with the highest CPU use, hottest first. Names only.
pub fn top_procs(procs: &sysinfo::System) -> Vec<TopProc> {
    let mut procs: Vec<TopProc> = procs
        .processes()
        .values()
        .map(|p| TopProc {
            pid: p.pid().as_u32(),
            name: p.name().to_string_lossy().into_owned(),
            cpu: p.cpu_usage(),
            rss_mb: p.memory() / BYTES_PER_MB,
        })
        .collect();
    procs.sort_by(|a, b| {
        b.cpu
            .partial_cmp(&a.cpu)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    procs.truncate(5);
    procs
}

/// The daemon's own CPU and RSS from an already-refreshed process table.
fn daemon_self(procs: &sysinfo::System) -> (f32, u64) {
    let Some(pid) = sysinfo::get_current_pid().ok() else {
        return (0.0, 0);
    };
    let Some(proc) = procs.process(pid) else {
        return (0.0, 0);
    };
    (proc.cpu_usage(), proc.memory() / BYTES_PER_MB)
}

/// 1/5/15-minute load from `/proc/loadavg`. Null off Linux, per spec.
#[cfg(target_os = "linux")]
fn read_load_avg() -> (Option<f64>, Option<f64>, Option<f64>) {
    let Ok(text) = std::fs::read_to_string("/proc/loadavg") else {
        return (None, None, None);
    };
    let mut fields = text.split_whitespace();
    let parse = |field: Option<&str>| field.and_then(|f| f.parse::<f64>().ok());
    (
        parse(fields.next()),
        parse(fields.next()),
        parse(fields.next()),
    )
}

/// 1/5/15-minute load from `/proc/loadavg`. Null off Linux, per spec.
#[cfg(not(target_os = "linux"))]
fn read_load_avg() -> (Option<f64>, Option<f64>, Option<f64>) {
    (None, None, None)
}

/// Event-loop lag: a 1 s sleeper records how many milliseconds past 1000 it
/// woke, keeping the maximum; each line drains it back to zero.
#[derive(Debug, Default)]
pub struct LagTracker {
    max_ms: AtomicU64,
}

impl LagTracker {
    fn record(&self, lag_ms: u64) {
        self.max_ms.fetch_max(lag_ms, Ordering::Relaxed);
    }

    fn take_max(&self) -> u64 {
        self.max_ms.swap(0, Ordering::Relaxed)
    }
}

/// UTC day stamp for `host-<date>.jsonl` filenames.
pub fn today_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// The metrics directory under the data dir.
pub fn metrics_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("metrics")
}

fn ensure_metrics_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// One `write` of the line plus newline on an `O_APPEND` handle, then
/// `sync_data` before returning: the line is readable through a second
/// handle the moment this returns, and a power-off loses at most the sample
/// in flight.
pub fn write_line(file: &mut File, line: &str) -> io::Result<()> {
    file.write_all(format!("{line}\n").as_bytes())?;
    file.sync_data()
}

/// Ensure the day's file exists and append one synced line to it. The `date`
/// parameter is the seam tests inject a date change through. Test-only, the
/// same way `CanopyConfig::save_for_tests` is: production holds its handle
/// in the sampler loop instead.
#[cfg(test)]
pub fn append_line(metrics: &Path, date: &str, line: &str) -> io::Result<()> {
    ensure_metrics_dir(metrics)?;
    let mut file = open_append(&metrics.join(format!("host-{date}.jsonl")))?;
    write_line(&mut file, line)
}

fn parse_host_date(name: &str) -> Option<NaiveDate> {
    let rest = name.strip_prefix("host-")?.strip_suffix(".jsonl")?;
    NaiveDate::parse_from_str(rest, "%Y-%m-%d").ok()
}

/// Delete `host-*.jsonl` files more than `retention_days` old; leave every
/// other file alone. Returns the number of files removed. Future-dated
/// files are kept.
pub fn prune_old(metrics: &Path, today: NaiveDate, retention_days: u64) -> usize {
    let Ok(entries) = std::fs::read_dir(metrics) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(date) = parse_host_date(&name) else {
            continue;
        };
        if today.signed_duration_since(date).num_days() <= retention_days as i64 {
            continue;
        }
        if entry.path().is_file() && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Read the retention fresh from `config.toml` (Apply::Hot: no restart),
/// falling back to the default for out-of-range hand edits.
fn effective_retention_days(data_dir: &Path) -> u64 {
    crate::domain::canopy_config::CanopyConfig::load(data_dir)
        .metrics_retention_days
        .clamp(1, 30)
}

fn prune_now(metrics: &Path, data_dir: &Path) {
    let today = chrono::Utc::now().date_naive();
    prune_old(metrics, today, effective_retention_days(data_dir));
}

fn collect_parts(
    db: &Database,
    hub: &SessionHub,
    procs: &sysinfo::System,
    lag_ms: u64,
) -> LineParts {
    let metrics = super::host_metrics::snapshot();
    let info = super::SystemInfo::new();
    let (load1, load5, load15) = read_load_avg();
    let (daemon_cpu, daemon_rss_mb) = daemon_self(procs);
    LineParts {
        cpu_usage: metrics.cpu_usage,
        cpu_temperature_c: metrics.cpu_temperature_c,
        cpu_temp_source: metrics.cpu_temp_source,
        memory_used_mb: metrics.memory_used_mb,
        memory_total_mb: metrics.memory_total_mb,
        gpu: metrics.gpu,
        power: metrics.power,
        swap_used_mb: info.swap_used / BYTES_PER_MB,
        load1,
        load5,
        load15,
        daemon_cpu,
        daemon_rss_mb,
        running_nodes: db.count_running_graphs().unwrap_or(0).max(0) as u64,
        live_sessions: hub.list().len(),
        loop_lag_ms: lag_ms,
        top_procs: top_procs(procs),
    }
}

/// Start the daemon's host sampler: `snapshot()` first (preserving the
/// immediate first-sample timing remote watchers rely on), then a 10 s line
/// every 10 s plus the 1 s lag probe and an hourly retention sweep.
/// [`super::host_metrics::snapshot`]'s contract is unchanged.
pub fn spawn(db: Arc<Database>, hub: SessionHub, data_dir: PathBuf) {
    super::host_metrics::snapshot();
    let lag = Arc::new(LagTracker::default());
    let lag_probe = Arc::clone(&lag);
    tokio::spawn(async move {
        let mut last = std::time::Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let now = std::time::Instant::now();
            let elapsed_ms = now
                .saturating_duration_since(last)
                .as_millis()
                .min(u128::from(u64::MAX)) as u64;
            last = now;
            lag_probe.record(elapsed_ms.saturating_sub(1_000));
        }
    });
    tokio::spawn(async move {
        let metrics = metrics_dir(&data_dir);
        prune_now(&metrics, &data_dir);
        let mut last_prune = std::time::Instant::now();
        let mut procs = sysinfo::System::new();
        let mut open_day: Option<(String, File)> = None;
        let mut last_warn: Option<std::time::Instant> = None;
        let mut ticker = tokio::time::interval(Duration::from_secs(SAMPLE_SECS));
        loop {
            ticker.tick().await;
            let date = today_utc();
            let reopen = match &open_day {
                Some((open_date, _)) => open_date != &date,
                None => true,
            };
            if reopen {
                let path = metrics.join(format!("host-{date}.jsonl"));
                match ensure_metrics_dir(&metrics).and_then(|()| open_append(&path)) {
                    Ok(file) => open_day = Some((date.clone(), file)),
                    Err(error) => {
                        warn_throttled(&mut last_warn, &error);
                        continue;
                    }
                }
            }
            procs.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
            let lag_ms = lag.take_max();
            let at = chrono::Utc::now().to_rfc3339();
            let line = build_line(&at, collect_parts(&db, &hub, &procs, lag_ms));
            let write_result = match open_day.as_mut() {
                Some((_, file)) => write_line(file, &line),
                None => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no open metrics file",
                )),
            };
            if let Err(error) = write_result {
                warn_throttled(&mut last_warn, &error);
                open_day = None;
            }
            if last_prune.elapsed().as_secs() >= PRUNE_SECS {
                prune_now(&metrics, &data_dir);
                last_prune = std::time::Instant::now();
            }
        }
    });
}

/// Log a write failure at most once a minute; never stop the sampler.
fn warn_throttled(last_warn: &mut Option<std::time::Instant>, error: &io::Error) {
    let now = std::time::Instant::now();
    let due = match last_warn {
        Some(last) => now.saturating_duration_since(*last).as_secs() >= WARN_THROTTLE_SECS,
        None => true,
    };
    if due {
        tracing::warn!("host metrics recorder: write failed: {error}");
        *last_warn = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_parts() -> LineParts {
        LineParts {
            cpu_usage: Some(12.5),
            cpu_temperature_c: Some(55.0),
            cpu_temp_source: Some("components".to_string()),
            memory_used_mb: Some(4_096),
            memory_total_mb: Some(8_192),
            gpu: None,
            power: None,
            swap_used_mb: 128,
            load1: Some(0.5),
            load5: Some(0.4),
            load15: Some(0.3),
            daemon_cpu: 1.25,
            daemon_rss_mb: 96,
            running_nodes: 2,
            live_sessions: 1,
            loop_lag_ms: 7,
            top_procs: vec![
                TopProc {
                    pid: 1,
                    name: "init".to_string(),
                    cpu: 3.0,
                    rss_mb: 10,
                },
                TopProc {
                    pid: 2,
                    name: "canopy".to_string(),
                    cpu: 9.5,
                    rss_mb: 200,
                },
            ],
        }
    }

    #[test]
    fn line_has_every_required_key_and_no_sampled_at() {
        let value: serde_json::Value =
            serde_json::from_str(&build_line("2026-01-02T03:04:05Z", sample_parts())).unwrap();
        for key in [
            "at",
            "cpu_usage",
            "cpu_temperature_c",
            "cpu_temp_source",
            "memory_used_mb",
            "memory_total_mb",
            "gpu",
            "power",
            "swap_used_mb",
            "load1",
            "load5",
            "load15",
            "daemon_cpu",
            "daemon_rss_mb",
            "running_nodes",
            "live_sessions",
            "loop_lag_ms",
            "top_procs",
        ] {
            assert!(value.get(key).is_some(), "missing key: {key}");
        }
        assert!(value.get("sampled_at").is_none());
        assert_eq!(value["at"], "2026-01-02T03:04:05Z");
        assert_eq!(value["running_nodes"], 2);
    }

    #[test]
    fn top_procs_capped_at_five_without_command_line() {
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let top = top_procs(&sys);
        assert!(top.len() <= 5);
        let rendered = serde_json::to_string(&top).unwrap();
        assert!(!rendered.contains("cmd"));
        assert!(!rendered.contains("command"));
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        for entry in value.as_array().unwrap() {
            assert_eq!(entry.as_object().unwrap().len(), 4);
            for key in ["pid", "name", "cpu", "rss_mb"] {
                assert!(entry.get(key).is_some(), "missing key: {key}");
            }
        }
    }

    #[test]
    fn written_line_readable_via_second_handle_after_return() {
        let dir = tempfile::TempDir::new().unwrap();
        let metrics = dir.path().join("metrics");
        append_line(&metrics, "2026-01-02", "{\"at\":\"x\"}").unwrap();
        let path = metrics.join("host-2026-01-02.jsonl");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "{\"at\":\"x\"}\n");
    }

    #[test]
    fn prune_deletes_only_old_host_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let metrics = dir.path().join("metrics");
        std::fs::create_dir_all(&metrics).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        std::fs::write(metrics.join("host-2025-12-31.jsonl"), "old\n").unwrap();
        std::fs::write(metrics.join("host-2026-01-10.jsonl"), "fresh\n").unwrap();
        std::fs::write(metrics.join("host-2026-01-09.jsonl"), "yesterday\n").unwrap();
        std::fs::write(metrics.join("notes.txt"), "keep\n").unwrap();
        std::fs::write(metrics.join("state.jsonl"), "keep\n").unwrap();
        let removed = prune_old(&metrics, today, 1);
        assert_eq!(removed, 1);
        assert!(!metrics.join("host-2025-12-31.jsonl").exists());
        assert!(metrics.join("host-2026-01-10.jsonl").exists());
        assert!(metrics.join("host-2026-01-09.jsonl").exists());
        assert!(metrics.join("notes.txt").exists());
        assert!(metrics.join("state.jsonl").exists());
    }

    #[test]
    fn failing_write_returns_err_without_panic() {
        let dir = tempfile::TempDir::new().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "not a dir").unwrap();
        let result = append_line(&blocker, "2026-01-02", "{\"at\":\"x\"}");
        assert!(result.is_err());
    }

    #[test]
    fn lag_maximum_resets_between_lines() {
        let lag = LagTracker::default();
        lag.record(5);
        lag.record(12);
        lag.record(3);
        assert_eq!(lag.take_max(), 12);
        assert_eq!(lag.take_max(), 0);
    }

    #[test]
    fn date_change_opens_a_new_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let metrics = dir.path().join("metrics");
        append_line(&metrics, "2026-01-01", "{\"at\":\"a\"}").unwrap();
        append_line(&metrics, "2026-01-02", "{\"at\":\"b\"}").unwrap();
        assert_eq!(
            std::fs::read_to_string(metrics.join("host-2026-01-01.jsonl")).unwrap(),
            "{\"at\":\"a\"}\n"
        );
        assert_eq!(
            std::fs::read_to_string(metrics.join("host-2026-01-02.jsonl")).unwrap(),
            "{\"at\":\"b\"}\n"
        );
    }
}
