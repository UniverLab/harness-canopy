//! CM51: a hand edit to `config.toml` takes effect without a restart.
//!
//! The watcher never writes the file. It re-reads it, validates every key and
//! keeps the previous effective value for anything invalid, then publishes a
//! `settings_changed` revision so a running TUI re-reads what it renders.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::application::ports::StateRepository;
use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::domain::settings::inspect;

/// DB state row publishing "the settings changed": `<n>|<key1,key2>`.
pub const SETTINGS_REVISION_KEY: &str = "settings_revision";

/// Quiet period that must pass before a burst of file events is acted on.
const DEBOUNCE: Duration = Duration::from_secs(1);

/// Bump the settings revision, carrying the changed keys so a consumer knows
/// what to re-read (D4).
pub fn publish_revision(db: &Database, keys: &[String]) {
    let next = db
        .get_state(SETTINGS_REVISION_KEY)
        .ok()
        .flatten()
        .and_then(|value| value.split('|').next().and_then(|n| n.parse::<u64>().ok()))
        .unwrap_or(0)
        + 1;
    let _ = db.set_state(SETTINGS_REVISION_KEY, &format!("{next}|{}", keys.join(",")));
}

/// Watch `config.toml` for hand edits and publish the changed keys.
pub fn spawn(canopy_dir: PathBuf, db: Arc<Database>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let watcher = match RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            let _ = tx.send(res);
        },
        notify::Config::default(),
    ) {
        Ok(watcher) => watcher,
        Err(error) => {
            eprintln!("config watch: could not start watcher: {error}");
            return;
        }
    };
    let Some(watcher) = watch_dir(watcher, &canopy_dir) else {
        return;
    };

    std::thread::spawn(move || {
        // Holding the watcher alive for the life of the thread.
        let _watcher: RecommendedWatcher = watcher;
        let prev = Mutex::new(CanopyConfig::load(&canopy_dir));
        while let Ok(event) = rx.recv() {
            if !touches_config(&event, &canopy_dir) {
                continue;
            }
            drain_until_quiet(&rx);
            apply_hand_edit(&canopy_dir, &db, &prev);
        }
    });
}

fn watch_dir(mut watcher: RecommendedWatcher, canopy_dir: &Path) -> Option<RecommendedWatcher> {
    if let Err(error) = watcher.watch(canopy_dir, RecursiveMode::NonRecursive) {
        eprintln!(
            "config watch: cannot watch {}: {error}",
            canopy_dir.display()
        );
        return None;
    }
    Some(watcher)
}

fn touches_config(event: &Result<notify::Event, notify::Error>, canopy_dir: &Path) -> bool {
    let Ok(event) = event else {
        return false;
    };
    let target = canopy_dir.join("config.toml");
    event.paths.iter().any(|path| path == &target)
}

/// Wait out the burst of events an editor's save produces.
fn drain_until_quiet(rx: &std::sync::mpsc::Receiver<Result<notify::Event, notify::Error>>) {
    let deadline = std::time::Instant::now() + DEBOUNCE;
    loop {
        let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
            return;
        };
        if rx.recv_timeout(remaining).is_err() {
            return;
        }
    }
}

fn apply_hand_edit(canopy_dir: &Path, db: &Database, prev: &Mutex<CanopyConfig>) {
    let path = canopy_dir.join("config.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let mut guard = prev.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (effective, problems) = inspect::effective_after_hand_edit(&guard, &text);
    for problem in &problems {
        eprintln!("config: {} invalid: {}", problem.key, problem.reason);
    }
    let changed = inspect::diff_keys(&guard, &effective);
    *guard = effective;
    drop(guard);

    if !changed.is_empty() {
        eprintln!("config: settings_changed [{}]", changed.join(", "));
        publish_revision(db, &changed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::settings::inspect::effective_after_hand_edit;
    use tempfile::TempDir;

    #[test]
    fn an_invalid_hand_edit_changes_no_effective_value() {
        let prev = CanopyConfig {
            spec_attempt_limit: 5,
            ..Default::default()
        };
        let (effective, problems) = effective_after_hand_edit(&prev, "spec_attempt_limit = 0\n");
        assert_eq!(problems.len(), 1);
        assert!(
            inspect::diff_keys(&prev, &effective).is_empty(),
            "an invalid value must not count as a change"
        );
    }

    #[test]
    fn publish_revision_carries_the_changed_keys_and_increments() {
        let dir = TempDir::new().unwrap();
        let db = Database::new(&dir.path().join("state.db")).unwrap();
        publish_revision(&db, &["theme".to_string()]);
        assert_eq!(
            db.get_state(SETTINGS_REVISION_KEY).unwrap().as_deref(),
            Some("1|theme")
        );
        publish_revision(&db, &["theme".to_string(), "pinned_panel_face".to_string()]);
        assert_eq!(
            db.get_state(SETTINGS_REVISION_KEY).unwrap().as_deref(),
            Some("2|theme,pinned_panel_face")
        );
    }
}
