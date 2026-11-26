//! Optional TUI performance logger (`tui.perf_log` / `CANOPY_TUI_PERF=1`).
//!
//! Records slow frames, slow refreshes, and slow input dispatches plus a 60 s
//! rolling summary to `~/.canopy/logs/tui-perf.log` (mode `0600`, rotating at
//! 5 MiB to `tui-perf.log.1`). When disabled, callers perform only a single
//! boolean check with no `Instant::now()` calls or file I/O.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};

const DRAW_THRESHOLD: Duration = Duration::from_millis(16);
const REFRESH_FAST_THRESHOLD: Duration = Duration::from_millis(5);
const REFRESH_SLOW_THRESHOLD: Duration = Duration::from_millis(100);
const INPUT_THRESHOLD: Duration = Duration::from_millis(20);
const SUMMARY_WINDOW: Duration = Duration::from_secs(60);
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Default path: `~/.canopy/logs/tui-perf.log`.
pub(crate) fn perf_log_path() -> PathBuf {
    crate::tui::logging::canopy_logs_dir().join("tui-perf.log")
}

fn rotated_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".1");
    PathBuf::from(os)
}

fn rfc3339_utc_ms(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn format_slow_event(
    ts: &str,
    event: &str,
    elapsed: Duration,
    sessions: usize,
    size: (u16, u16),
    slowest_panel: Option<&str>,
) -> String {
    let ms = elapsed.as_secs_f64() * 1000.0;
    let base = format!(
        "{ts} event={event} ms={ms:.1} sessions={sessions} size={}x{}",
        size.0, size.1
    );
    match slowest_panel {
        Some(panel) => format!("{base} slowest_panel={panel}"),
        None => base,
    }
}

fn nearest_rank_percentile(sorted: &[f32], pct: usize) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (pct * sorted.len()).div_ceil(100);
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

fn format_summary_line(
    ts: &str,
    draw_ms: &[f32],
    frames: u64,
    fingerprint_skips: u64,
    slow_refreshes: u64,
) -> String {
    let mut sorted = draw_ms.to_vec();
    sorted.sort_by(f32::total_cmp);
    let p50 = nearest_rank_percentile(&sorted, 50);
    let p95 = nearest_rank_percentile(&sorted, 95);
    format!(
        "{ts} event=summary window_s=60 frames={frames} p50_ms={p50:.1} p95_ms={p95:.1} fingerprint_skips={fingerprint_skips} slow_refreshes={slow_refreshes}"
    )
}

/// Per-TUI-session performance logger with 60 s window aggregation and 5 MiB
/// log rotation.
pub(crate) struct PerfLogger {
    enabled: bool,
    path: PathBuf,
    writer: Option<File>,
    bytes_written: u64,
    max_bytes: u64,
    window_start: Instant,
    draw_ms: Vec<f32>,
    frames: u64,
    fingerprint_skips: u64,
    slow_refreshes: u64,
}

impl PerfLogger {
    pub(crate) fn new(enabled: bool) -> Self {
        Self::with_path(enabled, perf_log_path(), MAX_LOG_BYTES)
    }

    pub(crate) fn with_path(enabled: bool, path: PathBuf, max_bytes: u64) -> Self {
        Self {
            enabled,
            path,
            writer: None,
            bytes_written: 0,
            max_bytes,
            window_start: Instant::now(),
            draw_ms: Vec::new(),
            frames: 0,
            fingerprint_skips: 0,
            slow_refreshes: 0,
        }
    }

    #[inline]
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.writer = None;
        self.bytes_written = 0;
        self.reset_window(Instant::now());
    }

    pub(crate) fn record_draw(
        &mut self,
        elapsed: Duration,
        sessions: usize,
        size: (u16, u16),
        slowest_panel: &str,
    ) {
        if !self.enabled {
            return;
        }
        self.frames += 1;
        self.draw_ms.push((elapsed.as_secs_f64() * 1000.0) as f32);
        if elapsed > DRAW_THRESHOLD {
            let line = format_slow_event(
                &rfc3339_utc_ms(Utc::now()),
                "draw",
                elapsed,
                sessions,
                size,
                Some(slowest_panel),
            );
            self.write_line(&line);
        }
    }

    pub(crate) fn record_refresh_fast(
        &mut self,
        elapsed: Duration,
        sessions: usize,
        size: (u16, u16),
    ) {
        if !self.enabled {
            return;
        }
        if elapsed > REFRESH_FAST_THRESHOLD {
            self.slow_refreshes += 1;
            let line = format_slow_event(
                &rfc3339_utc_ms(Utc::now()),
                "refresh_fast",
                elapsed,
                sessions,
                size,
                None,
            );
            self.write_line(&line);
        }
    }

    pub(crate) fn record_refresh_slow(
        &mut self,
        elapsed: Duration,
        sessions: usize,
        size: (u16, u16),
    ) {
        if !self.enabled {
            return;
        }
        if elapsed > REFRESH_SLOW_THRESHOLD {
            self.slow_refreshes += 1;
            let line = format_slow_event(
                &rfc3339_utc_ms(Utc::now()),
                "refresh_slow",
                elapsed,
                sessions,
                size,
                None,
            );
            self.write_line(&line);
        }
    }

    pub(crate) fn record_input(&mut self, elapsed: Duration, sessions: usize, size: (u16, u16)) {
        if !self.enabled {
            return;
        }
        if elapsed > INPUT_THRESHOLD {
            let line = format_slow_event(
                &rfc3339_utc_ms(Utc::now()),
                "input",
                elapsed,
                sessions,
                size,
                None,
            );
            self.write_line(&line);
        }
    }

    pub(crate) fn record_fingerprint_skip(&mut self) {
        if !self.enabled {
            return;
        }
        self.fingerprint_skips += 1;
    }

    pub(crate) fn flush_summary_if_due(&mut self) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if now.duration_since(self.window_start) >= SUMMARY_WINDOW {
            self.emit_summary(now);
        }
    }

    fn emit_summary(&mut self, now: Instant) {
        let line = format_summary_line(
            &rfc3339_utc_ms(Utc::now()),
            &self.draw_ms,
            self.frames,
            self.fingerprint_skips,
            self.slow_refreshes,
        );
        self.write_line(&line);
        self.reset_window(now);
    }

    fn reset_window(&mut self, now: Instant) {
        self.window_start = now;
        self.draw_ms.clear();
        self.frames = 0;
        self.fingerprint_skips = 0;
        self.slow_refreshes = 0;
    }

    fn write_line(&mut self, line: &str) {
        self.rotate_if_needed();
        if self.writer.is_none() {
            if let Some(parent) = self.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(file) = crate::state_dir::open_private_log(&self.path) {
                self.bytes_written = file.metadata().map(|m| m.len()).unwrap_or(0);
                self.writer = Some(file);
            }
        }
        if let Some(file) = self.writer.as_mut() {
            if writeln!(file, "{line}").is_ok() {
                self.bytes_written += line.len() as u64 + 1;
            }
        }
    }

    fn rotate_if_needed(&mut self) {
        let at_limit = if self.writer.is_some() {
            self.bytes_written >= self.max_bytes
        } else {
            std::fs::metadata(&self.path)
                .map(|m| m.len() >= self.max_bytes)
                .unwrap_or(false)
        };
        if !at_limit {
            return;
        }
        self.writer = None;
        let backup = rotated_path(&self.path);
        #[cfg(not(unix))]
        let _ = std::fs::remove_file(&backup);
        let _ = std::fs::rename(&self.path, &backup);
        self.bytes_written = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn perf_log_threshold_gating() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tui-perf.log");
        let mut logger = PerfLogger::with_path(true, path.clone(), MAX_LOG_BYTES);

        // At or below threshold: silent.
        logger.record_draw(Duration::from_millis(16), 2, (120, 40), "sidebar");
        logger.record_refresh_fast(Duration::from_millis(5), 2, (120, 40));
        logger.record_refresh_slow(Duration::from_millis(100), 2, (120, 40));
        logger.record_input(Duration::from_millis(20), 2, (120, 40));
        assert!(!path.exists(), "at-threshold events must not create log");

        // Strictly above threshold: emits one line per event.
        logger.record_draw(Duration::from_millis(17), 2, (120, 40), "sidebar");
        logger.record_refresh_fast(Duration::from_millis(6), 2, (120, 40));
        logger.record_refresh_slow(Duration::from_millis(101), 2, (120, 40));
        logger.record_input(Duration::from_millis(21), 2, (120, 40));

        let content = std::fs::read_to_string(&path).expect("read log");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 4, "expected 4 slow lines, got: {content}");
        assert!(lines[0].contains("event=draw "));
        assert!(lines[0].contains("slowest_panel=sidebar"));
        assert!(lines[1].contains("event=refresh_fast "));
        assert!(!lines[1].contains("slowest_panel="));
        assert!(lines[2].contains("event=refresh_slow "));
        assert!(lines[3].contains("event=input "));
    }

    #[test]
    fn perf_log_exact_line_formats() {
        let ts = rfc3339_utc_ms(
            Utc.with_ymd_and_hms(2026, 11, 26, 15, 0, 0).unwrap()
                + chrono::Duration::milliseconds(123),
        );
        assert_eq!(ts, "2026-11-26T15:00:00.123Z");

        let draw = format_slow_event(
            &ts,
            "draw",
            Duration::from_micros(24_300),
            4,
            (160, 48),
            Some("sidebar"),
        );
        assert_eq!(
            draw,
            "2026-11-26T15:00:00.123Z event=draw ms=24.3 sessions=4 size=160x48 slowest_panel=sidebar"
        );

        let fast = format_slow_event(
            &ts,
            "refresh_fast",
            Duration::from_micros(7_500),
            1,
            (80, 24),
            None,
        );
        assert_eq!(
            fast,
            "2026-11-26T15:00:00.123Z event=refresh_fast ms=7.5 sessions=1 size=80x24"
        );

        let summary = format_summary_line(&ts, &[10.0, 20.0, 30.0, 40.0, 50.0], 5, 3, 2);
        assert_eq!(
            summary,
            "2026-11-26T15:00:00.123Z event=summary window_s=60 frames=5 p50_ms=30.0 p95_ms=50.0 fingerprint_skips=3 slow_refreshes=2"
        );
    }

    #[test]
    fn perf_log_rotation_at_5_mib_overwrites_existing_backup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tui-perf.log");
        let backup = rotated_path(&path);

        // Pre-existing .1 backup that must be overwritten.
        std::fs::write(&backup, b"stale backup\n").expect("write stale backup");
        // Seed primary log right at the 5 MiB limit.
        let file = crate::state_dir::open_private_log(&path).expect("open");
        file.set_len(MAX_LOG_BYTES).expect("set_len");
        drop(file);

        let mut logger = PerfLogger::with_path(true, path.clone(), MAX_LOG_BYTES);
        logger.record_input(Duration::from_millis(35), 1, (80, 24));

        let backup_meta = std::fs::metadata(&backup).expect("backup exists");
        assert_eq!(backup_meta.len(), MAX_LOG_BYTES);
        let fresh = std::fs::read_to_string(&path).expect("fresh log");
        assert_eq!(fresh.lines().count(), 1);
        assert!(fresh.contains("event=input ms=35.0"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn perf_log_summary_percentiles_and_window_reset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tui-perf.log");
        let mut logger = PerfLogger::with_path(true, path.clone(), MAX_LOG_BYTES);

        // Empty window emits 0.0 percentiles.
        logger.emit_summary(Instant::now());

        // Populate 20 draws (1.0 ..= 20.0 ms), 2 fingerprint skips, 1 slow refresh.
        for ms in 1..=20 {
            logger.record_draw(Duration::from_millis(ms), 1, (80, 24), "log_panel");
        }
        logger.record_fingerprint_skip();
        logger.record_fingerprint_skip();
        logger.record_refresh_fast(Duration::from_millis(8), 1, (80, 24));
        logger.emit_summary(Instant::now());

        // After reset, another summary must be empty again.
        logger.emit_summary(Instant::now());

        let content = std::fs::read_to_string(&path).expect("read log");
        let summaries: Vec<&str> = content
            .lines()
            .filter(|l| l.contains("event=summary"))
            .collect();
        assert_eq!(summaries.len(), 3, "content: {content}");
        assert!(summaries[0].ends_with(
            "event=summary window_s=60 frames=0 p50_ms=0.0 p95_ms=0.0 fingerprint_skips=0 slow_refreshes=0"
        ));
        assert!(summaries[1].ends_with(
            "event=summary window_s=60 frames=20 p50_ms=10.0 p95_ms=19.0 fingerprint_skips=2 slow_refreshes=1"
        ));
        assert!(summaries[2].ends_with(
            "event=summary window_s=60 frames=0 p50_ms=0.0 p95_ms=0.0 fingerprint_skips=0 slow_refreshes=0"
        ));
    }

    #[test]
    fn perf_log_disabled_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tui-perf.log");
        let mut logger = PerfLogger::with_path(false, path.clone(), MAX_LOG_BYTES);

        assert!(!logger.is_enabled());
        logger.record_draw(Duration::from_millis(200), 5, (160, 48), "sidebar");
        logger.record_refresh_fast(Duration::from_millis(50), 5, (160, 48));
        logger.record_refresh_slow(Duration::from_millis(500), 5, (160, 48));
        logger.record_input(Duration::from_millis(100), 5, (160, 48));
        logger.record_fingerprint_skip();
        logger.flush_summary_if_due();

        assert!(!path.exists(), "disabled logger must not create file");
    }
}
