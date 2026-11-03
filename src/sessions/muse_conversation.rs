//! muse harness conversation discovery: map a freshly spawned interactive
//! muse session back to the harness's own conversation id, so a later
//! restart resumes *that* conversation (`muse resume <uuid>`) instead of
//! the workspace's newest one (`resume --last`).
//!
//! Verified against the real muse 1.4.14 on this machine: there is no
//! `--session-id` flag and no `list` command, so the session store is the
//! only source. Layout (both confirmed by spawning the real binary):
//!
//! * store root: `$XDG_DATA_HOME/muse/sessions`, or
//!   `$HOME/.local/share/muse/sessions` when `XDG_DATA_HOME` is unset/empty;
//! * conversations: `<store>/YYYY/MM/DD/<uuid>/session.jsonl`, whose early
//!   records carry `"workspace_root":"<cwd>"` unescaped;
//! * `<uuid>/subagent/*/session.jsonl` files also carry a workspace root but
//!   are not top-level conversations and are never matched;
//! * `<uuid>/.session.lock` names the live process (`pid=<pid>\nhost=...`)
//!   and appears within a second of spawning — while `session.jsonl` only
//!   appears after the first prompt is submitted. A live interactive
//!   session is therefore discovered by its child's pid, exactly (no
//!   newest-wins race between sessions sharing a directory); the
//!   `session.jsonl` scan stays as the fallback for completed sessions and
//!   for spawns whose pid was never reported.
//! * an idle session (no prompt ever submitted) has no `session.jsonl` yet,
//!   but its lock uuid is already its conversation id: once prompted, the
//!   log lands in that same uuid dir and `resume <that uuid>` boots it.
//!   Resuming a never-prompted uuid fails with "has no saved log" — the
//!   session truly has nothing to resume — and surfaces as an error row
//!   with the tail attached, never silently.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::db::Database;

/// How many times the background recorder scans the store before giving up.
const DISCOVER_ATTEMPTS: u32 = 10;
/// Delay between two discovery scans.
const DISCOVER_DELAY: Duration = Duration::from_millis(300);
/// Bytes read from the head of each `session.jsonl`: the workspace root is
/// recorded in the first records, while a full transcript can be megabytes.
const SCAN_HEAD_BYTES: u64 = 64 * 1024;
/// Grace subtracted from `since` when comparing mtimes: a file created in
/// the same timestamp tick the caller sampled can otherwise compare older.
const MTIME_GRACE: Duration = Duration::from_secs(2);
/// Same grace for pruning untouched directory subtrees (see [`discover_conversation`]).
const DIR_PRUNE_GRACE: Duration = Duration::from_secs(5);

/// The muse session store directory, or `None` when neither
/// `XDG_DATA_HOME` nor the home directory resolves.
pub fn muse_store_dir() -> Option<PathBuf> {
    muse_store_dir_from(
        dirs::home_dir().as_deref(),
        std::env::var_os("XDG_DATA_HOME"),
    )
}

/// [`muse_store_dir`] with its environment inputs as parameters, so tests
/// assert the selection without mutating process environment.
fn muse_store_dir_from(
    home: Option<&Path>,
    xdg_data_home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(xdg) = xdg_data_home.filter(|xdg| !xdg.is_empty()) {
        return Some(PathBuf::from(xdg).join("muse").join("sessions"));
    }
    home.map(|home| {
        home.join(".local")
            .join("share")
            .join("muse")
            .join("sessions")
    })
}

/// The newest muse conversation id for `workdir` whose `session.jsonl` was
/// written at or after `since`: `<uuid>/session.jsonl` files under
/// `store_dir` (any nesting — the real layout is date-partitioned) whose
/// head records this `workdir` as their workspace root. Newest mtime wins.
///
/// Skips hidden entries, `subagent/` subtrees (not top-level conversations)
/// and directory subtrees untouched since well before `since` — creating a
/// new session entry bumps its parents' mtimes, so a stale subtree cannot
/// hold a fresh `session.jsonl`.
pub fn discover_conversation(store_dir: &Path, workdir: &str, since: SystemTime) -> Option<String> {
    let effective_since = since.checked_sub(MTIME_GRACE).unwrap_or(since);
    let prune_before = since.checked_sub(MTIME_GRACE + DIR_PRUNE_GRACE);
    let mut best: Option<(SystemTime, String)> = None;
    for path in session_files(store_dir, prune_before, "session.jsonl") {
        let Ok(mtime) = path.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if mtime < effective_since || !session_file_matches_workdir(&path, workdir) {
            continue;
        }
        let Some(id) = parent_dir_name(&path) else {
            continue;
        };
        let newer = best.as_ref().is_none_or(|(known, _)| mtime > *known);
        if newer {
            best = Some((mtime, id));
        }
    }
    best.map(|(_, id)| id)
}

/// The live muse conversation owned by `pid`: the `<uuid>` dir whose
/// `.session.lock` names that pid, when the lock was written at or after
/// `since`. Exact — two sessions sharing a directory never collide — and
/// immediate: the lock exists from spawn, while `session.jsonl` waits for
/// the first prompt. `None` for `pid <= 0`, an unreadable store, or no
/// fresh lock naming the pid. Newest mtime wins if several match (a stale
/// lock can only name this pid through pid recycling, and a stale lock
/// predates `since`).
pub fn discover_conversation_by_pid(
    store_dir: &Path,
    pid: i64,
    since: SystemTime,
) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    let effective_since = since.checked_sub(MTIME_GRACE).unwrap_or(since);
    let prune_before = since.checked_sub(MTIME_GRACE + DIR_PRUNE_GRACE);
    let mut best: Option<(SystemTime, String)> = None;
    for path in session_files(store_dir, prune_before, ".session.lock") {
        let Ok(mtime) = path.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if mtime < effective_since || lock_pid(&path) != Some(pid) {
            continue;
        }
        let Some(id) = parent_dir_name(&path) else {
            continue;
        };
        let newer = best.as_ref().is_none_or(|(known, _)| mtime > *known);
        if newer {
            best = Some((mtime, id));
        }
    }
    best.map(|(_, id)| id)
}

/// Every `file_name` file under `store_dir` (any nesting — the real layout
/// is date-partitioned), skipping hidden directories, `subagent/` subtrees
/// (not top-level conversations) and directory subtrees untouched since
/// `prune_before`. The wanted file itself may be hidden (`.session.lock`).
fn session_files(
    store_dir: &Path,
    prune_before: Option<SystemTime>,
    file_name: &str,
) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![store_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if name.starts_with('.')
                    || name == "subagent"
                    || dir_prunable(&entry.path(), prune_before)
                {
                    continue;
                }
                stack.push(entry.path());
                continue;
            }
            if file_type.is_file() && name == file_name {
                found.push(entry.path());
            }
        }
    }
    found
}

/// The conversation id a session file belongs to: its parent dir name.
fn parent_dir_name(path: &Path) -> Option<String> {
    path.parent()
        .and_then(|parent| parent.file_name())
        .map(|id| id.to_string_lossy().into_owned())
}

/// The pid a `.session.lock` names (`pid=<pid>` on its first line), or
/// `None` when the lock cannot be read or parsed — a racing read during
/// the lock's creation retries on the next poll.
fn lock_pid(path: &Path) -> Option<i64> {
    let body = std::fs::read_to_string(path).ok()?;
    let first = body.lines().next()?.trim();
    first.strip_prefix("pid=")?.trim().parse().ok()
}

/// Whether `dir`'s whole subtree is too stale to hold a fresh session file.
fn dir_prunable(dir: &Path, prune_before: Option<SystemTime>) -> bool {
    let Some(prune_before) = prune_before else {
        return false;
    };
    dir.metadata()
        .and_then(|meta| meta.modified())
        .is_ok_and(|mtime| mtime < prune_before)
}

/// Whether the head of `session.jsonl` records `workdir` as its workspace
/// root in any occurrence.
fn session_file_matches_workdir(path: &Path, workdir: &str) -> bool {
    let head = read_head(path).unwrap_or_default();
    let matched = workspace_roots(&head).any(|root| same_workdir(&root, workdir));
    matched
}

/// The first [`SCAN_HEAD_BYTES`] of a session file, or `None` when it cannot
/// be read.
fn read_head(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let mut head = String::new();
    file.take(SCAN_HEAD_BYTES).read_to_string(&mut head).ok()?;
    Some(head)
}

/// Every `"workspace_root":"..."` value in a session file head, unescaped.
/// Occurrences inside the first line's escaped `record_json` blob do not
/// match the literal `"workspace_root"` key and are skipped; the same root
/// is recorded unescaped in the records that follow.
fn workspace_roots(head: &str) -> impl Iterator<Item = String> + '_ {
    const KEY: &str = "\"workspace_root\"";
    let mut rest = head;
    std::iter::from_fn(move || {
        let key_at = rest.find(KEY)?;
        rest = &rest[key_at + KEY.len()..];
        let value_start = rest.find(':').map(|at| at + 1)?;
        rest = rest[value_start..].trim_start_matches([' ', '\t']);
        let quoted = rest.strip_prefix('"')?;
        let (value, len) = unescape_json_string(quoted)?;
        rest = &quoted[len..];
        Some(value)
    })
}

/// Unescape one JSON string body (without the opening quote): the value and
/// the bytes consumed including the closing quote, or `None` when the body
/// never closes.
fn unescape_json_string(body: &str) -> Option<(String, usize)> {
    let mut value = String::new();
    let mut chars = body.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '"' => return Some((value, at + 1)),
            '\\' => {
                let (_, escaped) = chars.next()?;
                match escaped {
                    '"' | '\\' | '/' => value.push(escaped),
                    'n' => value.push('\n'),
                    'r' => value.push('\r'),
                    't' => value.push('\t'),
                    'u' => {
                        let digits: String = chars.by_ref().take(4).map(|(_, c)| c).collect();
                        let code = u32::from_str_radix(&digits, 16).ok()?;
                        value.push(char::from_u32(code)?);
                    }
                    _ => return None,
                }
            }
            _ => value.push(c),
        }
    }
    None
}

/// Whether a recorded workspace root names `workdir`: exact match after
/// trimming cosmetic slashes, else canonicalized comparison so symlinked
/// roots (scratch dirs, `/tmp` aliases) still match.
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

/// Record the harness conversation id of a freshly spawned muse session:
/// poll `store_dir` [`DISCOVER_ATTEMPTS`] times on a background thread, then
/// store the first id found. A live session is matched by its child's pid
/// through `.session.lock` (exact, and immediate — the lock exists from
/// spawn); the `session.jsonl` scan is the fallback for completed sessions
/// and for spawns whose pid was never reported. Best-effort — an
/// undiscoverable session (or `None` store) simply keeps its NULL id.
/// `since` must be sampled before the spawn.
pub fn record_fresh_conversation(
    db: Arc<Database>,
    session_id: String,
    workdir: String,
    since: SystemTime,
    pid: Option<i64>,
    store_dir: Option<PathBuf>,
) {
    std::thread::spawn(move || {
        let Some(store) = store_dir else {
            return;
        };
        for _ in 0..DISCOVER_ATTEMPTS {
            std::thread::sleep(DISCOVER_DELAY);
            let found = pid
                .and_then(|pid| discover_conversation_by_pid(&store, pid, since))
                .or_else(|| discover_conversation(&store, &workdir, since));
            if let Some(id) = found {
                let _ = db.set_session_harness_conversation_id(&session_id, &id);
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_dir_prefers_xdg_data_home() {
        let store = muse_store_dir_from(Some(Path::new("/home/u")), Some("/xdg".into()));
        assert_eq!(store, Some(PathBuf::from("/xdg/muse/sessions")));
    }

    #[test]
    fn store_dir_falls_back_to_home_local_share() {
        let store = muse_store_dir_from(Some(Path::new("/home/u")), None);
        assert_eq!(
            store,
            Some(PathBuf::from("/home/u/.local/share/muse/sessions"))
        );
    }

    #[test]
    fn store_dir_treats_empty_xdg_as_unset() {
        let store = muse_store_dir_from(Some(Path::new("/home/u")), Some("".into()));
        assert_eq!(
            store,
            Some(PathBuf::from("/home/u/.local/share/muse/sessions"))
        );
    }

    #[test]
    fn store_dir_is_none_without_home_or_xdg() {
        assert_eq!(muse_store_dir_from(None, None), None);
    }

    #[test]
    fn workspace_root_values_are_unescaped() {
        let head = r#"{"a":1,"workspace_root":"/tmp/my work"},{"workspace_root":"/x\"y"}"#;
        let roots: Vec<String> = workspace_roots(head).collect();
        assert_eq!(roots, vec!["/tmp/my work".to_string(), "/x\"y".to_string()]);
    }

    #[test]
    fn unterminated_value_yields_no_root() {
        assert_eq!(workspace_roots(r#"{"workspace_root":"/abc"#).count(), 0);
    }

    fn write_lock(store: &Path, id: &str, pid: i64) {
        let dir = store.join("2026").join("11").join("03").join(id);
        std::fs::create_dir_all(&dir).expect("lock dir");
        std::fs::write(dir.join(".session.lock"), format!("pid={pid}\nhost=TEST\n"))
            .expect("lock file");
    }

    #[test]
    fn lock_discovery_matches_the_child_pid_exactly() {
        let store = tempfile::tempdir().expect("store");
        let since = SystemTime::now();
        write_lock(store.path(), "uuid-aaa", 1111);
        write_lock(store.path(), "uuid-bbb", 2222);

        assert_eq!(
            discover_conversation_by_pid(store.path(), 2222, since).as_deref(),
            Some("uuid-bbb")
        );
        assert_eq!(
            discover_conversation_by_pid(store.path(), 1111, since).as_deref(),
            Some("uuid-aaa")
        );
    }

    #[test]
    fn lock_discovery_ignores_other_pids_and_bad_pids() {
        let store = tempfile::tempdir().expect("store");
        let since = SystemTime::now();
        write_lock(store.path(), "uuid-aaa", 1111);

        assert_eq!(
            discover_conversation_by_pid(store.path(), 9999, since),
            None
        );
        assert_eq!(discover_conversation_by_pid(store.path(), 0, since), None);
        assert_eq!(discover_conversation_by_pid(store.path(), -1, since), None);
    }

    #[test]
    fn lock_discovery_ignores_stale_locks() {
        let store = tempfile::tempdir().expect("store");
        write_lock(store.path(), "uuid-old", 1111);
        let since = SystemTime::now() + Duration::from_secs(3600);

        assert_eq!(
            discover_conversation_by_pid(store.path(), 1111, since),
            None
        );
    }

    #[test]
    fn lock_discovery_ignores_malformed_locks() {
        let store = tempfile::tempdir().expect("store");
        let since = SystemTime::now();
        let dir = store.path().join("day").join("uuid-bad");
        std::fs::create_dir_all(&dir).expect("lock dir");
        std::fs::write(dir.join(".session.lock"), "not-a-pid-line\n").expect("lock file");

        assert_eq!(
            discover_conversation_by_pid(store.path(), 1111, since),
            None
        );
    }

    #[test]
    fn lock_discovery_skips_subagent_subtrees() {
        let store = tempfile::tempdir().expect("store");
        let since = SystemTime::now();
        let sub = store
            .path()
            .join("day")
            .join("uuid-parent")
            .join("subagent")
            .join("uuid-child");
        std::fs::create_dir_all(&sub).expect("subagent dir");
        std::fs::write(sub.join(".session.lock"), "pid=1111\nhost=TEST\n").expect("lock file");

        assert_eq!(
            discover_conversation_by_pid(store.path(), 1111, since),
            None
        );
    }
}
