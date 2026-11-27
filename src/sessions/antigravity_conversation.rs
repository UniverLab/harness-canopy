//! antigravity (`agy`) harness conversation discovery: map a freshly spawned
//! interactive antigravity session back to its own conversation UUID so a
//! later restart resumes *that* conversation (`agy --conversation <uuid>`)
//! instead of colliding on `--continue` or an unrelated graph node's
//! conversation.
//!
//! Primary discovery inspects `/proc/<pid>/fd` symlinks for
//! `presence/<uuid>.lock` or `conversations/<uuid>.db`, mapping a live `agy`
//! process 1-to-1 to its conversation even when multiple `agy` sessions share
//! the same working directory. When `/proc/<pid>/fd` is unavailable or the
//! child exits before its fd is observed, a single fallback scan of
//! `~/.gemini/antigravity-cli/history.jsonl` picks the newest unclaimed valid
//! UUID matching `workspace` and `timestamp >= since - 2s`.

use std::collections::HashSet;
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::db::Database;

/// How many times the background recorder polls `/proc/<pid>/fd` before
/// falling back to `history.jsonl` (60 * 500 ms = 30 s).
const DISCOVER_ATTEMPTS: u32 = 60;
/// Delay between two `/proc/<pid>/fd` discovery polls.
const DISCOVER_DELAY: Duration = Duration::from_millis(500);
/// Grace subtracted from `since` when comparing `history.jsonl` timestamps.
const HISTORY_GRACE: Duration = Duration::from_secs(2);

/// Default `antigravity-cli` history file:
/// `$HOME/.gemini/antigravity-cli/history.jsonl`.
pub fn default_history_path() -> Option<PathBuf> {
    default_history_path_from(dirs::home_dir().as_deref())
}

fn default_history_path_from(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|h| {
        h.join(".gemini")
            .join("antigravity-cli")
            .join("history.jsonl")
    })
}

/// Extract a conversation UUID from a `/proc/<pid>/fd`-style directory by
/// scanning symlink targets ending in `presence/<uuid>.lock` or
/// `conversations/<uuid>.db`.
pub fn discover_conversation_by_pid_in(proc_fd_dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(proc_fd_dir).ok()?;
    for entry in entries.flatten() {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        if let Some(id) = extract_uuid_from_fd_target(&target) {
            return Some(id);
        }
    }
    None
}

/// Extract a conversation UUID from `/proc/<pid>/fd` for a live `agy` child.
pub fn discover_conversation_by_pid(pid: i64) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    let fd_dir = PathBuf::from(format!("/proc/{pid}/fd"));
    discover_conversation_by_pid_in(&fd_dir)
}

fn extract_uuid_from_fd_target(target: &Path) -> Option<String> {
    let file_name = target.file_name()?.to_str()?;
    let parent_name = target.parent()?.file_name()?.to_str()?;
    let stem = match parent_name {
        "presence" => file_name.strip_suffix(".lock")?,
        "conversations" => file_name.strip_suffix(".db")?,
        _ => return None,
    };
    uuid::Uuid::parse_str(stem).ok()?;
    Some(stem.to_string())
}

#[derive(serde::Deserialize)]
struct HistoryEntry {
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default, rename = "conversationId")]
    conversation_id: Option<String>,
}

/// Fallback discovery from `history.jsonl`: returns the newest unclaimed valid
/// UUID whose `workspace` matches `workdir` and whose `timestamp` (epoch ms)
/// is at or after `since` minus a 2 s grace window.
pub fn discover_conversation_from_history(
    history_path: &Path,
    workdir: &str,
    since: SystemTime,
    already_claimed: &HashSet<String>,
) -> Option<String> {
    let file = std::fs::File::open(history_path).ok()?;
    let effective_since = since.checked_sub(HISTORY_GRACE).unwrap_or(since);
    let effective_since_ms = effective_since
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    let mut best: Option<(i64, String)> = None;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<HistoryEntry>(trimmed) else {
            continue;
        };
        let Some(ts) = entry.timestamp else {
            continue;
        };
        if ts < effective_since_ms {
            continue;
        }
        let Some(ws) = entry.workspace.as_deref() else {
            continue;
        };
        if !same_workdir(ws, workdir) {
            continue;
        }
        let Some(cid) = entry.conversation_id else {
            continue;
        };
        if already_claimed.contains(&cid) || uuid::Uuid::parse_str(&cid).is_err() {
            continue;
        }
        if best.as_ref().is_none_or(|(known, _)| ts >= *known) {
            best = Some((ts, cid));
        }
    }
    best.map(|(_, id)| id)
}

fn same_workdir(recorded: &str, workdir: &str) -> bool {
    if trim_slashes(recorded) == trim_slashes(workdir) {
        return true;
    }
    match (
        std::fs::canonicalize(recorded),
        std::fs::canonicalize(workdir),
    ) {
        (Ok(recorded), Ok(workdir)) => recorded == workdir,
        _ => false,
    }
}

fn trim_slashes(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

/// Poll `/proc/<pid>/fd` every 500 ms for up to 30 s to discover the live
/// `antigravity` conversation ID, stopping early if found, if the session
/// already has a `harness_conversation_id`, or if `pid` exits. Falls back once
/// to `history.jsonl` if PID discovery returned `None`.
pub fn record_fresh_conversation(
    db: Arc<Database>,
    session_id: String,
    workdir: String,
    since: SystemTime,
    pid: Option<i64>,
    history_path_override: Option<PathBuf>,
) {
    std::thread::spawn(move || {
        for _ in 0..DISCOVER_ATTEMPTS {
            if db
                .get_session_harness_conversation_id(&session_id)
                .ok()
                .flatten()
                .is_some()
            {
                return;
            }
            if let Some(p) = pid {
                if let Some(id) = discover_conversation_by_pid(p) {
                    let _ = db.set_session_harness_conversation_id(&session_id, &id);
                    return;
                }
                if !crate::system::process_is_alive(p) {
                    break;
                }
            } else {
                break;
            }
            std::thread::sleep(DISCOVER_DELAY);
        }

        if db
            .get_session_harness_conversation_id(&session_id)
            .ok()
            .flatten()
            .is_some()
        {
            return;
        }
        let Some(history_path) = history_path_override.or_else(default_history_path) else {
            return;
        };
        let claimed = db
            .list_claimed_harness_conversation_ids()
            .unwrap_or_default();
        if let Some(id) =
            discover_conversation_from_history(&history_path, &workdir, since, &claimed)
        {
            let _ = db.set_session_harness_conversation_id(&session_id, &id);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn symlink_fd(dir: &Path, name: &str, target: &str) {
        std::os::unix::fs::symlink(target, dir.join(name)).expect("create fd symlink");
    }

    #[cfg(unix)]
    #[test]
    fn pid_discovery_extracts_uuid_from_presence_lock_and_conversations_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let uuid_lock = "11111111-1111-4111-8111-111111111111";
        let fd_lock = dir.path().join("fd_lock");
        std::fs::create_dir_all(&fd_lock).expect("fd_lock dir");
        symlink_fd(
            &fd_lock,
            "3",
            &format!("/home/u/.gemini/antigravity-cli/presence/{uuid_lock}.lock"),
        );
        assert_eq!(
            discover_conversation_by_pid_in(&fd_lock).as_deref(),
            Some(uuid_lock)
        );

        let uuid_db = "22222222-2222-4222-8222-222222222222";
        let fd_db = dir.path().join("fd_db");
        std::fs::create_dir_all(&fd_db).expect("fd_db dir");
        symlink_fd(
            &fd_db,
            "4",
            &format!("/home/u/.gemini/antigravity-cli/conversations/{uuid_db}.db"),
        );
        assert_eq!(
            discover_conversation_by_pid_in(&fd_db).as_deref(),
            Some(uuid_db)
        );
    }

    #[cfg(unix)]
    #[test]
    fn pid_discovery_ignores_non_uuid_and_unrelated_symlinks() {
        let dir = tempfile::tempdir().expect("tempdir");
        symlink_fd(dir.path(), "0", "/dev/null");
        symlink_fd(
            dir.path(),
            "1",
            "/home/u/.gemini/antigravity-cli/presence/not-a-uuid.lock",
        );
        symlink_fd(
            dir.path(),
            "2",
            "/home/u/.gemini/antigravity-cli/other/33333333-3333-4333-8333-333333333333.lock",
        );
        assert_eq!(discover_conversation_by_pid_in(dir.path()), None);
    }

    #[cfg(unix)]
    #[test]
    fn pid_discovery_separates_two_concurrent_fake_pids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let uuid_one = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let uuid_two = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        let fd_one = dir.path().join("pid1_fd");
        let fd_two = dir.path().join("pid2_fd");
        std::fs::create_dir_all(&fd_one).expect("fd1");
        std::fs::create_dir_all(&fd_two).expect("fd2");
        symlink_fd(&fd_one, "5", &format!("/tmp/agy/presence/{uuid_one}.lock"));
        symlink_fd(
            &fd_two,
            "5",
            &format!("/tmp/agy/conversations/{uuid_two}.db"),
        );

        assert_eq!(
            discover_conversation_by_pid_in(&fd_one).as_deref(),
            Some(uuid_one)
        );
        assert_eq!(
            discover_conversation_by_pid_in(&fd_two).as_deref(),
            Some(uuid_two)
        );
    }

    #[test]
    fn history_discovery_matches_workspace_timestamp_and_skips_claimed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let history = dir.path().join("history.jsonl");
        let since = SystemTime::now();
        let now_ms = since
            .duration_since(UNIX_EPOCH)
            .expect("unix epoch")
            .as_millis() as i64;
        let old_ms = now_ms - 60_000;

        let uuid_old = "11111111-1111-4111-8111-111111111111";
        let uuid_other_ws = "22222222-2222-4222-8222-222222222222";
        let uuid_claimed = "33333333-3333-4333-8333-333333333333";
        let uuid_free = "44444444-4444-4444-8444-444444444444";

        let lines = [
            format!(
                "{{\"timestamp\":{old_ms},\"workspace\":\"/work/proj\",\"conversationId\":\"{uuid_old}\"}}"
            ),
            format!(
                "{{\"timestamp\":{},\"workspace\":\"/work/other\",\"conversationId\":\"{uuid_other_ws}\"}}",
                now_ms + 10
            ),
            format!(
                "{{\"timestamp\":{},\"workspace\":\"/work/proj\",\"conversationId\":\"{uuid_free}\"}}",
                now_ms + 20
            ),
            format!(
                "{{\"timestamp\":{},\"workspace\":\"/work/proj\",\"conversationId\":\"{uuid_claimed}\"}}",
                now_ms + 30
            ),
            format!(
                "{{\"timestamp\":{},\"workspace\":\"/work/proj\",\"conversationId\":\"not-a-uuid\"}}",
                now_ms + 40
            ),
        ];
        std::fs::write(&history, lines.join("\n")).expect("write history");

        let mut claimed = HashSet::new();
        claimed.insert(uuid_claimed.to_string());

        let found = discover_conversation_from_history(&history, "/work/proj", since, &claimed);
        assert_eq!(found.as_deref(), Some(uuid_free));

        claimed.insert(uuid_free.to_string());
        assert_eq!(
            discover_conversation_from_history(&history, "/work/proj", since, &claimed),
            None
        );
    }
}
