//! Read-only view over the host-metrics history written by
//! [`super::host_metrics_recorder`]: `~/.canopy/metrics/host-YYYY-MM-DD.jsonl`
//! (UTC day), one JSON object per line.
//!
//! This module never writes or deletes files: a missing directory, a missing
//! day file and a malformed line (a power-off can leave a truncated last
//! line) all read as "no data", never as an error. The recorder stays
//! untouched so it can ship alone.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One of the hottest processes on the machine, as recorded.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TopProcSample {
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cpu: f32,
    #[serde(default)]
    pub rss_mb: u64,
}

/// One history line. Mirrors the recorder's `HostMetricsLine`; every field
/// defaults so a newer recorder never breaks this older reader, and unknown
/// fields round-trip through `extra` so `--json` prints raw lines.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Sample {
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub cpu_usage: Option<f32>,
    #[serde(default)]
    pub cpu_temperature_c: Option<f32>,
    #[serde(default)]
    pub memory_used_mb: Option<u64>,
    #[serde(default)]
    pub swap_used_mb: u64,
    #[serde(default)]
    pub load1: Option<f64>,
    #[serde(default)]
    pub daemon_cpu: f32,
    #[serde(default)]
    pub daemon_rss_mb: u64,
    #[serde(default)]
    pub running_nodes: u64,
    #[serde(default)]
    pub loop_lag_ms: u64,
    #[serde(default)]
    pub top_procs: Vec<TopProcSample>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Average, 95th percentile and maximum of one numeric key, with the time of
/// the maximum (first sample wins a tie).
#[derive(Debug, Clone, Serialize)]
pub struct Stat {
    pub avg: f64,
    pub p95: f64,
    pub max: f64,
    pub max_at: String,
}

/// Highest `cpu_temperature_c` seen, when any sample carries one.
#[derive(Debug, Clone, Serialize)]
pub struct TempMax {
    pub max: f64,
    pub max_at: String,
}

/// One of the three process names seen most often, with its share of samples.
#[derive(Debug, Clone, Serialize)]
pub struct TopProcShare {
    pub name: String,
    pub share: f64,
}

/// The eight numeric keys the summary covers, in display order.
const NUMERIC_KEYS: [&str; 8] = [
    "cpu_usage",
    "memory_used_mb",
    "swap_used_mb",
    "load1",
    "daemon_cpu",
    "daemon_rss_mb",
    "running_nodes",
    "loop_lag_ms",
];

/// Parse one sample value for a summary key. `None` means the sample carries
/// no reading for the key and is skipped by the statistics.
fn key_value(sample: &Sample, key: &str) -> Option<f64> {
    match key {
        "cpu_usage" => sample.cpu_usage.map(f64::from),
        "memory_used_mb" => sample.memory_used_mb.map(|v| v as f64),
        "swap_used_mb" => Some(sample.swap_used_mb as f64),
        "load1" => sample.load1,
        "daemon_cpu" => Some(f64::from(sample.daemon_cpu)),
        "daemon_rss_mb" => Some(sample.daemon_rss_mb as f64),
        "running_nodes" => Some(sample.running_nodes as f64),
        "loop_lag_ms" => Some(sample.loop_lag_ms as f64),
        _ => None,
    }
}

/// Parse an RFC 3339 instant.
fn parse_instant(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Parse a duration suffix (`30m`, `6h`, `1d`) into seconds.
fn duration_secs(s: &str) -> Option<i64> {
    let s = s.trim().to_lowercase();
    let units = [
        ("seconds", 1),
        ("second", 1),
        ("secs", 1),
        ("sec", 1),
        ("minutes", 60),
        ("minute", 60),
        ("mins", 60),
        ("min", 60),
        ("hours", 3_600),
        ("hour", 3_600),
        ("days", 86_400),
        ("day", 86_400),
        ("weeks", 604_800),
        ("week", 604_800),
        ("s", 1),
        ("m", 60),
        ("h", 3_600),
        ("d", 86_400),
        ("w", 604_800),
    ];
    for (suffix, mult) in units {
        if let Some(num) = s.strip_suffix(suffix) {
            let num = num.trim();
            if num.is_empty() {
                continue;
            }
            let n: f64 = num.parse().ok()?;
            return Some((n * mult as f64) as i64);
        }
    }
    s.parse::<f64>().ok().map(|n| n as i64)
}

/// Resolve a `--since`/`--until` bound: an RFC 3339 instant wins, otherwise
/// the value is a duration before `now` (durations like `30m`).
pub fn parse_time(s: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    if let Some(t) = parse_instant(s) {
        return Ok(t);
    }
    if let Some(secs) = duration_secs(s) {
        return Ok(now - chrono::Duration::seconds(secs));
    }
    anyhow::bail!("cannot parse time {s:?}: want RFC 3339 or a duration like 30m, 6h, 1d")
}

/// Read every sample with `since <= at <= until`, spanning daily files.
/// Missing files and malformed lines (including a truncated last line from a
/// power-off) are skipped, never errors.
pub fn read_window(dir: &Path, since: DateTime<Utc>, until: DateTime<Utc>) -> Vec<Sample> {
    let mut out = Vec::new();
    let mut day = since.date_naive();
    let last = until.date_naive();
    while day <= last {
        read_day_file(
            dir,
            &day.format("%Y-%m-%d").to_string(),
            since,
            until,
            &mut out,
        );
        let Some(next) = day.succ_opt() else {
            break;
        };
        day = next;
    }
    out.sort_by(|a, b| a.at.cmp(&b.at));
    out
}

/// Read one `host-<date>.jsonl` file, keeping the lines inside the window.
fn read_day_file(
    dir: &Path,
    date: &str,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    out: &mut Vec<Sample>,
) {
    let path = dir.join(format!("host-{date}.jsonl"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(sample) = serde_json::from_str::<Sample>(line) else {
            continue;
        };
        let Some(at) = parse_instant(&sample.at) else {
            continue;
        };
        if at >= since && at <= until {
            out.push(sample);
        }
    }
}

/// Nearest-rank 95th percentile of sorted values: `ceil(0.95 * n)`.
fn percentile95(sorted: &[f64]) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((0.95 * sorted.len() as f64).ceil() as usize).max(1);
    sorted[rank.min(sorted.len()) - 1]
}

/// Statistics for one key over time-ordered values: average, p95, maximum
/// and the time of the first maximum.
fn stat_for(values: &[(f64, &str)]) -> Option<Stat> {
    if values.is_empty() {
        return None;
    }
    let sum: f64 = values.iter().map(|(v, _)| v).sum();
    let mut sorted: Vec<f64> = values.iter().map(|(v, _)| *v).collect();
    sorted.sort_by(f64::total_cmp);
    let max = sorted[sorted.len() - 1];
    let max_at = values
        .iter()
        .find(|(v, _)| *v == max)
        .map(|(_, at)| (*at).to_string())
        .unwrap_or_default();
    Some(Stat {
        avg: sum / values.len() as f64,
        p95: percentile95(&sorted),
        max,
        max_at,
    })
}

/// Summary of a window: per-key stats for the eight numeric keys (only keys
/// with at least one reading), the temperature maximum, and the top-3
/// process names.
pub struct Summary {
    pub stats: Vec<(String, Stat)>,
    pub temp_max: Option<TempMax>,
    pub top_procs: Vec<TopProcShare>,
}

/// Summarize a window. Samples should be in time order (as `read_window`
/// returns them) so a `max_at` tie picks the first maximum.
pub fn summarize(samples: &[Sample]) -> Summary {
    let mut stats = Vec::new();
    for key in NUMERIC_KEYS {
        let values: Vec<(f64, &str)> = samples
            .iter()
            .filter_map(|s| key_value(s, key).map(|v| (v, s.at.as_str())))
            .collect();
        if let Some(stat) = stat_for(&values) {
            stats.push((key.to_string(), stat));
        }
    }
    Summary {
        stats,
        temp_max: temp_max_of(samples),
        top_procs: top3(samples),
    }
}

/// Highest `cpu_temperature_c` in the window, or `None` when no sample
/// carries one.
fn temp_max_of(samples: &[Sample]) -> Option<TempMax> {
    let mut best: Option<(f64, &str)> = None;
    for s in samples {
        if let Some(t) = s.cpu_temperature_c {
            let t = f64::from(t);
            if best.is_none_or(|(b, _)| t > b) {
                best = Some((t, s.at.as_str()));
            }
        }
    }
    best.map(|(max, at)| TempMax {
        max,
        max_at: at.to_string(),
    })
}

/// The three process names appearing in the most samples' `top_procs`, with
/// each name's share of samples. Ties break alphabetically.
pub fn top3(samples: &[Sample]) -> Vec<TopProcShare> {
    if samples.is_empty() {
        return Vec::new();
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for s in samples {
        let mut seen = std::collections::BTreeSet::new();
        for p in &s.top_procs {
            seen.insert(p.name.as_str());
        }
        for name in seen {
            *counts.entry(name).or_default() += 1;
        }
    }
    let mut names: Vec<(&str, usize)> = counts.into_iter().collect();
    names.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    names
        .into_iter()
        .take(3)
        .map(|(name, n)| TopProcShare {
            name: name.to_string(),
            share: n as f64 / samples.len() as f64,
        })
        .collect()
}

/// Samples within 60 seconds of `at` (`|sample.at - at| <= 60s`), for "what
/// was happening at 11:32".
pub fn around(samples: &[Sample], at: DateTime<Utc>) -> Vec<Sample> {
    samples
        .iter()
        .filter(|s| parse_instant(&s.at).is_some_and(|t| (t - at).num_seconds().abs() <= 60))
        .cloned()
        .collect()
}

/// Options for [`run_host`].
pub struct HostOptions {
    pub since: String,
    pub until: Option<String>,
    pub at: Option<String>,
    pub json: bool,
    pub no_samples: bool,
}

/// Run `canopy metrics host`: resolve the window, read the files from `dir`
/// directly (never creating anything), and print the report. An empty window
/// prints "no samples in this window" with exit code 0; `--json` prints the
/// same shape with an empty array instead.
pub fn run_host(dir: &Path, opts: &HostOptions) -> Result<()> {
    let now = Utc::now();
    let until = match &opts.until {
        Some(u) => parse_time(u, now)?,
        None => now,
    };
    if let Some(at) = &opts.at {
        let at = parse_time(at, now)?;
        return run_at(dir, at, opts.json, opts.no_samples);
    }
    let since = parse_time(&opts.since, until)?;
    let (since, until) = if since <= until {
        (since, until)
    } else {
        (until, since)
    };
    let samples = read_window(dir, since, until);
    if opts.json {
        print_json(since, until, &samples, opts.no_samples);
    } else {
        print_human(since, until, &samples);
    }
    Ok(())
}

/// Print the `--at` view: every sample within 60 seconds of `at`, each with
/// its `top_procs`.
fn run_at(dir: &Path, at: DateTime<Utc>, json: bool, no_samples: bool) -> Result<()> {
    let span = chrono::Duration::seconds(60);
    let samples = read_window(dir, at - span, at + span);
    let near = around(&samples, at);
    if json {
        print_json(at - span, at + span, &near, no_samples);
    } else {
        print_at_human(at, &near);
    }
    Ok(())
}

/// Number formatting: integers print bare, others with two decimals.
fn fmt_num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.2}")
    }
}

/// One summary line: `<key>: avg X p95 Y max Z at T`.
fn stat_line(key: &str, stat: &Stat) -> String {
    format!(
        "{}: avg {} p95 {} max {} at {}",
        key,
        fmt_num(stat.avg),
        fmt_num(stat.p95),
        fmt_num(stat.max),
        stat.max_at
    )
}

/// Human summary for a window (FR2).
fn print_human(since: DateTime<Utc>, until: DateTime<Utc>, samples: &[Sample]) {
    if samples.is_empty() {
        println!("no samples in this window");
        return;
    }
    let summary = summarize(samples);
    println!(
        "host metrics {} to {} ({} samples)",
        since.to_rfc3339(),
        until.to_rfc3339(),
        samples.len()
    );
    for (key, stat) in &summary.stats {
        println!("  {}", stat_line(key, stat));
    }
    if let Some(t) = &summary.temp_max {
        println!("  cpu_temperature_c max {} at {}", fmt_num(t.max), t.max_at);
    }
    if !summary.top_procs.is_empty() {
        let names: Vec<String> = summary
            .top_procs
            .iter()
            .map(|p| format!("{} ({:.0}%)", p.name, p.share * 100.0))
            .collect();
        println!("  top processes: {}", names.join(", "));
    }
}

/// Human `--at` view: one block per nearby sample with its `top_procs`.
fn print_at_human(at: DateTime<Utc>, samples: &[Sample]) {
    if samples.is_empty() {
        println!("no samples in this window");
        return;
    }
    println!(
        "host metrics around {} ({} samples)",
        at.to_rfc3339(),
        samples.len()
    );
    for s in samples {
        println!("  {}", sample_line(s));
    }
}

/// One `--at` sample line with its top process names.
fn sample_line(s: &Sample) -> String {
    let procs: Vec<String> = s
        .top_procs
        .iter()
        .map(|p| format!("{} ({:.0}%)", p.name, f64::from(p.cpu)))
        .collect();
    format!(
        "{} cpu {} mem {}MB swap {}MB load1 {} daemon {} nodes {} lag {}ms procs [{}]",
        s.at,
        s.cpu_usage
            .map_or_else(|| "-".to_string(), |v| fmt_num(f64::from(v))),
        s.memory_used_mb
            .map_or_else(|| "-".to_string(), |v| v.to_string()),
        s.swap_used_mb,
        s.load1.map_or_else(|| "-".to_string(), fmt_num),
        fmt_num(f64::from(s.daemon_cpu)),
        s.running_nodes,
        s.loop_lag_ms,
        procs.join(", ")
    )
}

/// JSON report (FR3): window holds the resolved instants and the sample
/// count; `samples` are the raw lines of the window.
fn print_json(since: DateTime<Utc>, until: DateTime<Utc>, samples: &[Sample], no_samples: bool) {
    let summary = summarize(samples);
    let mut map = serde_json::Map::new();
    for (key, stat) in &summary.stats {
        map.insert(key.clone(), serde_json::to_value(stat).unwrap_or_default());
    }
    if let Some(t) = &summary.temp_max {
        map.insert(
            "temp_max".to_string(),
            serde_json::to_value(t).unwrap_or_default(),
        );
    }
    if !summary.top_procs.is_empty() {
        map.insert(
            "top_procs".to_string(),
            serde_json::to_value(&summary.top_procs).unwrap_or_default(),
        );
    }
    let mut root = serde_json::Map::new();
    root.insert(
        "window".to_string(),
        serde_json::json!({
            "since": since.to_rfc3339(),
            "until": until.to_rfc3339(),
            "samples": samples.len(),
        }),
    );
    root.insert("summary".to_string(), serde_json::Value::Object(map));
    if !no_samples {
        root.insert(
            "samples".to_string(),
            serde_json::to_value(samples).unwrap_or_default(),
        );
    }
    println!("{}", serde_json::Value::Object(root));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// One fixture line for `at` with `cpu_usage` = `cpu`, `memory_used_mb`
    /// = `mem`, and `top_procs` = `procs` (names at 50% cpu each).
    fn line(at: &str, cpu: f32, mem: u64, procs: &[&str]) -> String {
        let top: Vec<String> = procs
            .iter()
            .map(|n| format!(r#"{{"pid":1,"name":{n:?},"cpu":50.0,"rss_mb":10}}"#))
            .collect();
        format!(
            concat!(
                r#"{{"at":{at:?},"cpu_usage":{cpu},"memory_used_mb":{mem},"swap_used_mb":7,"load1":0.5,"#,
                r#""daemon_cpu":1.0,"daemon_rss_mb":20,"running_nodes":2,"loop_lag_ms":3,"top_procs":[{top}]}}"#
            ),
            at = at,
            cpu = cpu,
            mem = mem,
            top = top.join(",")
        )
    }

    /// Fixture dir with samples straddling midnight: two lines on day 1, one
    /// on day 2.
    fn two_day_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let day1 = "2026-03-01T23:59:40+00:00,2026-03-01T23:59:50+00:00";
        let mut d1 = String::new();
        for (i, at) in day1.split(',').enumerate() {
            d1.push_str(&line(at, 10.0 * (i as f32 + 1.0), 100, &["alpha"]));
            d1.push('\n');
        }
        std::fs::write(dir.path().join("host-2026-03-01.jsonl"), d1).unwrap();
        let d2 = format!(
            "{}\n",
            line("2026-03-02T00:00:10+00:00", 30.0, 300, &["beta"])
        );
        std::fs::write(dir.path().join("host-2026-03-02.jsonl"), d2).unwrap();
        dir
    }

    fn utc(s: &str) -> DateTime<Utc> {
        parse_instant(s).unwrap()
    }

    #[test]
    fn t_window_spans_2_files() {
        let dir = two_day_dir();
        let all = read_window(
            dir.path(),
            utc("2026-03-01T00:00:00+00:00"),
            utc("2026-03-02T23:59:59+00:00"),
        );
        assert_eq!(all.len(), 3);
        let first_day = read_window(
            dir.path(),
            utc("2026-03-01T00:00:00+00:00"),
            utc("2026-03-01T23:59:59+00:00"),
        );
        assert_eq!(first_day.len(), 2);
        let second_day = read_window(
            dir.path(),
            utc("2026-03-02T00:00:00+00:00"),
            utc("2026-03-02T23:59:59+00:00"),
        );
        assert_eq!(second_day.len(), 1);
        assert_eq!(second_day[0].memory_used_mb, Some(300));
    }

    #[test]
    fn t_stats_p95_max_at() {
        let base = Utc.with_ymd_and_hms(2026, 3, 2, 12, 0, 0).unwrap();
        let samples: Vec<Sample> = (1..=20)
            .map(|i| Sample {
                at: (base + chrono::Duration::seconds(i * 10)).to_rfc3339(),
                cpu_usage: Some(i as f32),
                ..Sample::default()
            })
            .collect();
        let summary = summarize(&samples);
        let (_, stat) = summary
            .stats
            .iter()
            .find(|(k, _)| k == "cpu_usage")
            .unwrap();
        assert_eq!(stat.avg, 10.5);
        assert_eq!(stat.p95, 19.0);
        assert_eq!(stat.max, 20.0);
        assert_eq!(stat.max_at, samples[19].at);
    }

    #[test]
    fn t_max_at_first_tie_wins() {
        let samples: Vec<Sample> = ["2026-03-02T12:00:00+00:00", "2026-03-02T12:00:10+00:00"]
            .iter()
            .map(|at| Sample {
                at: (*at).to_string(),
                cpu_usage: Some(9.0),
                ..Sample::default()
            })
            .collect();
        let summary = summarize(&samples);
        let (_, stat) = summary
            .stats
            .iter()
            .find(|(k, _)| k == "cpu_usage")
            .unwrap();
        assert_eq!(stat.max_at, "2026-03-02T12:00:00+00:00");
    }

    #[test]
    fn t_at_shows_top_procs() {
        let dir = two_day_dir();
        let all = read_window(
            dir.path(),
            utc("2026-03-01T00:00:00+00:00"),
            utc("2026-03-02T23:59:59+00:00"),
        );
        // The midnight straddle puts all three lines within 60 s of `at`
        // (40 s, 20 s and 0 s away): the window crosses the day-file edge.
        let near = around(&all, utc("2026-03-02T00:00:10+00:00"));
        assert_eq!(near.len(), 3);
        assert_eq!(near[2].top_procs.len(), 1);
        assert_eq!(near[2].top_procs[0].name, "beta");
        let far = around(&all, utc("2026-03-02T06:00:00+00:00"));
        assert!(far.is_empty());
    }

    #[test]
    fn t_truncated_tail_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut text = format!(
            "{}\n",
            line("2026-03-02T12:00:00+00:00", 5.0, 50, &["alpha"])
        );
        text.push_str("{\"at\":\"2026-03-02T12:00:10+00:00\",\"cpu_usage\":");
        std::fs::write(dir.path().join("host-2026-03-02.jsonl"), text).unwrap();
        let samples = read_window(
            dir.path(),
            utc("2026-03-02T00:00:00+00:00"),
            utc("2026-03-02T23:59:59+00:00"),
        );
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].cpu_usage, Some(5.0));
    }

    #[test]
    fn t_empty_window() {
        let dir = two_day_dir();
        let samples = read_window(
            dir.path(),
            utc("2020-01-01T00:00:00+00:00"),
            utc("2020-01-02T00:00:00+00:00"),
        );
        assert!(samples.is_empty());
        let summary = summarize(&samples);
        assert!(summary.stats.is_empty());
        assert!(summary.temp_max.is_none());
        assert!(summary.top_procs.is_empty());
        let missing = read_window(
            Path::new("/nonexistent-canopy-metrics-dir"),
            utc("2026-03-01T00:00:00+00:00"),
            utc("2026-03-02T00:00:00+00:00"),
        );
        assert!(missing.is_empty());
    }

    #[test]
    fn t_top3_shares() {
        let dir = two_day_dir();
        let all = read_window(
            dir.path(),
            utc("2026-03-01T00:00:00+00:00"),
            utc("2026-03-02T23:59:59+00:00"),
        );
        let top = top3(&all);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].name, "alpha");
        assert!((top[0].share - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(top[1].name, "beta");
    }

    #[test]
    fn t_parse_time_duration_and_rfc3339() {
        let now = Utc.with_ymd_and_hms(2026, 3, 2, 12, 0, 0).unwrap();
        assert_eq!(
            parse_time("30m", now).unwrap(),
            now - chrono::Duration::seconds(1_800)
        );
        assert_eq!(
            parse_time("1h", now).unwrap(),
            now - chrono::Duration::seconds(3_600)
        );
        assert_eq!(
            parse_time("1d", now).unwrap(),
            now - chrono::Duration::seconds(86_400)
        );
        assert_eq!(
            parse_time("2026-03-01T10:00:00+00:00", now).unwrap(),
            utc("2026-03-01T10:00:00+00:00")
        );
        assert!(parse_time("yesterday-ish", now).is_err());
    }
}
