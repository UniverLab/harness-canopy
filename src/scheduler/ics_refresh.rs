//! Background `.ics` feed refresher for `CronScheduler`.
//!
//! Fetches configured `[[calendar.feeds]]` sources (HTTP/HTTPS via
//! `reqwest::blocking` or local file paths), parses `VEVENT`s via
//! [`crate::domain::ics::parse_ics`], and upserts them into
//! `calendar_feed_items`. Errors are logged at `WARN` and existing cached
//! entries are preserved.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use tokio_util::sync::CancellationToken;

use crate::db::Database;
use crate::domain::canopy_config::CalendarFeedConfig;
use crate::domain::ics::parse_ics;

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const LOOP_TICK: Duration = Duration::from_secs(60);

/// Fetch raw `.ics` text from an HTTP(S) URL or a local file path.
pub fn fetch_feed_source(source: &str) -> Result<String> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("Feed source must not be empty"));
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let client = reqwest::blocking::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()?;
        let resp = client.get(trimmed).send()?.error_for_status()?;
        return Ok(resp.text()?);
    }
    let path = expand_tilde(trimmed);
    fs::read_to_string(&path).map_err(|e| anyhow!("Failed to read '{}': {e}", path.display()))
}

fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Refresh a single `.ics` feed synchronously and upsert its items into SQLite.
/// Returns the number of parsed events on success, or `0` on failure (logging at `WARN`).
pub fn refresh_feed_sync(db: &Database, feed: &CalendarFeedConfig) -> usize {
    match fetch_feed_source(&feed.source) {
        Ok(raw) => {
            let events = parse_ics(&raw);
            let count = events.len();
            if let Err(err) = db.upsert_calendar_feed_items(&feed.name, &events) {
                tracing::warn!(
                    feed = %feed.name,
                    error = %err,
                    "Failed to cache .ics calendar feed items"
                );
                return 0;
            }
            count
        }
        Err(err) => {
            tracing::warn!(
                feed = %feed.name,
                source = %feed.source,
                error = %err,
                "Failed to refresh .ics calendar feed; keeping cached entries"
            );
            0
        }
    }
}

/// Periodically refresh all configured `.ics` feeds until `cancel` is triggered.
pub async fn run_ics_refresh_loop(
    db: Arc<Database>,
    feeds: Vec<CalendarFeedConfig>,
    cancel: CancellationToken,
) {
    if feeds.is_empty() {
        return;
    }
    let mut last_run: Vec<Option<Instant>> = vec![None; feeds.len()];

    loop {
        let now = Instant::now();
        for (idx, feed) in feeds.iter().enumerate() {
            let interval = Duration::from_secs(feed.refresh_minutes.max(1) * 60);
            let due = last_run[idx].is_none_or(|prev| now.duration_since(prev) >= interval);
            if !due {
                continue;
            }
            let db_clone = Arc::clone(&db);
            let feed_clone = feed.clone();
            let _ = tokio::task::spawn_blocking(move || refresh_feed_sync(&db_clone, &feed_clone))
                .await;
            last_run[idx] = Some(Instant::now());
        }

        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(LOOP_TICK) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn refresh_feed_reads_local_file_and_keeps_cache_on_error() {
        let dir = TempDir::new().unwrap();
        let db = Database::new(&dir.path().join("canopy.db")).unwrap();
        let ics_path = dir.path().join("work.ics");
        std::fs::write(
            &ics_path,
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:e1\r\nSUMMARY:Review\r\nDTSTART:20261201T120000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        )
        .unwrap();

        let feed = CalendarFeedConfig {
            name: "Work".to_string(),
            source: ics_path.to_string_lossy().to_string(),
            refresh_minutes: 15,
        };
        assert_eq!(refresh_feed_sync(&db, &feed), 1);
        assert_eq!(
            db.list_calendar_feed_items(Some("Work"), false)
                .unwrap()
                .len(),
            1
        );

        // Remove file; refresh logs WARN and preserves cached item.
        std::fs::remove_file(&ics_path).unwrap();
        assert_eq!(refresh_feed_sync(&db, &feed), 0);
        assert_eq!(
            db.list_calendar_feed_items(Some("Work"), false)
                .unwrap()
                .len(),
            1
        );
    }
}
