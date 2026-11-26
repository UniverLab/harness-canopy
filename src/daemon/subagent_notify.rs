//! Completion notice for a non-blocking `subagent` spawn: when the child
//! ends, the spawning session learns the outcome through the same
//! `scheduled_prompts` pipeline every other automated message uses (held
//! while the user types, merged with same-window deliveries). Split out of
//! `daemon::subagent` so that module stays within the size gate.

use std::sync::Arc;

use chrono::Utc;

use crate::db::scheduled_prompts::ScheduledPromptProvenance;
use crate::db::session_target::SessionTarget;
use crate::db::Database;

/// Who a finishing subagent reports to: the resolved live session plus the
/// workdir the notice is filed under (the spawn workdir) and the daemon
/// waker that makes delivery land within seconds.
pub struct SubagentNotify {
    pub target_id: String,
    pub workdir: String,
    pub waker: Arc<tokio::sync::Notify>,
}

/// A resolved notify target before the handler attaches its waker.
#[derive(Debug)]
pub struct ResolvedNotifyTarget {
    pub id: String,
    pub name: String,
    pub workdir: String,
}

/// First 2,000 Unicode chars of a subagent's output or error.
pub const NOTIFY_SNIPPET_CHARS: usize = 2000;

/// Normalize a workdir for the same-directory check: resolve symlinks and
/// trailing slashes so `/tmp/x/` matches `/tmp/x`. Falls back to a
/// slash-trimmed literal when the path cannot be canonicalized.
fn canonical_dir(raw: &str) -> String {
    if let Ok(path) = std::fs::canonicalize(raw) {
        return path.to_string_lossy().to_string();
    }
    let trimmed = raw.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    trimmed.to_string()
}

/// Look up one live session by id, requiring it to still be `active`.
fn live_session_by_id(db: &Database, id: &str) -> Result<Option<String>, String> {
    let session = db
        .get_hookable_session(id)
        .map_err(|e| format!("could not look up notify target session '{id}': {e}"))?;
    Ok(session.filter(|s| s.status == "active").map(|s| s.name))
}

/// Resolve where a finishing subagent reports: an explicit `notify_session`
/// (live id first, else exactly one live session of that name), or — when
/// omitted — the caller's own live session, or nobody when the caller is
/// not a live interactive session. An explicit target in another workdir is
/// refused naming both workdirs. Returns `Ok(None)` when nothing is
/// notified, `Err(message)` for a refusal the spawn surfaces verbatim.
pub fn resolve_spawn_notify_target(
    db: &Database,
    requested: Option<&str>,
    caller_session_id: Option<&str>,
    spawn_workdir: &str,
) -> Result<Option<ResolvedNotifyTarget>, String> {
    let Some(name_or_id) = requested.map(str::trim).filter(|s| !s.is_empty()) else {
        return resolve_default_notify_target(db, caller_session_id, spawn_workdir);
    };
    let (id, name) = match live_session_by_id(db, name_or_id)? {
        Some(name) => (name_or_id.to_string(), name),
        None => match db
            .resolve_target_session_name(name_or_id)
            .map_err(|e| format!("could not resolve notify_session '{name_or_id}': {e}"))?
        {
            SessionTarget::Found { id, name } => {
                (id, name.unwrap_or_else(|| name_or_id.to_string()))
            }
            SessionTarget::Missing => {
                return Err(format!(
                    "notify_session '{name_or_id}' does not exist or is no longer live; notification not scheduled"
                ));
            }
            SessionTarget::Ambiguous(many) => {
                return Err(format!(
                    "notify_session '{name_or_id}' matches {} live sessions ({}); notification not scheduled",
                    many.len(),
                    many.join(", ")
                ));
            }
        },
    };
    let target_workdir = db
        .get_hookable_session(&id)
        .map_err(|e| format!("could not look up notify target session '{id}': {e}"))?
        .map(|s| s.working_dir)
        .unwrap_or_default();
    // FR2 scopes the same-workdir rule to targets *other than* the caller's
    // own session: notifying yourself about your own subagent never leaks
    // across projects, so an explicit self-target skips the check exactly
    // like the omitted-target default does below.
    let is_own_session = caller_session_id.is_some_and(|caller| caller == id);
    if !is_own_session && canonical_dir(&target_workdir) != canonical_dir(spawn_workdir) {
        return Err(format!(
            "notify target session '{name}' runs in '{target_workdir}', not the spawn workdir '{spawn_workdir}'; notification not scheduled"
        ));
    }
    Ok(Some(ResolvedNotifyTarget {
        id,
        name,
        workdir: spawn_workdir.to_string(),
    }))
}

/// Omitted `notify_session` defaults to the caller's own live session; a
/// caller that is not a live interactive session notifies nobody.
fn resolve_default_notify_target(
    db: &Database,
    caller_session_id: Option<&str>,
    spawn_workdir: &str,
) -> Result<Option<ResolvedNotifyTarget>, String> {
    let Some(caller) = caller_session_id else {
        return Ok(None);
    };
    match live_session_by_id(db, caller)? {
        Some(name) => Ok(Some(ResolvedNotifyTarget {
            id: caller.to_string(),
            name,
            workdir: spawn_workdir.to_string(),
        })),
        None => Ok(None),
    }
}

/// Status label for a finished subagent without changing the persisted row
/// (`'finished'`/`'failed'`): `"finished"` on success, `"timed_out"` when
/// the wait expired, `"failed"` otherwise.
fn notify_label(timed_out: bool, status: &str) -> &'static str {
    if timed_out {
        "timed_out"
    } else if status == "finished" {
        "finished"
    } else {
        "failed"
    }
}

/// Body excerpt for the notice: stdout on success, else stderr (falling
/// back to stdout when stderr is empty), cut at [`NOTIFY_SNIPPET_CHARS`].
fn notify_snippet(label: &str, stdout: &str, stderr: &str) -> String {
    let body = if label == "finished" || stderr.is_empty() {
        stdout
    } else {
        stderr
    };
    body.chars().take(NOTIFY_SNIPPET_CHARS).collect()
}

/// Validate the `notify_session` / `blocking` combination and resolve the
/// notice for a spawn. Returns the notice to hand to the child plus the
/// name the async JSON reports (`None` renders as `"none"`).
/// `Err(message)` is a refusal the spawn surfaces verbatim: `blocking`
/// with `notify_session` (the caller already gets the result), or an
/// unusable explicit target.
pub fn prepare_spawn_notify(
    db: &Database,
    notify_session: Option<&str>,
    blocking: bool,
    caller_session_id: Option<&str>,
    spawn_workdir: &str,
    waker: Arc<tokio::sync::Notify>,
) -> Result<(Option<SubagentNotify>, Option<String>), String> {
    if blocking && notify_session.map(str::trim).is_some_and(|s| !s.is_empty()) {
        return Err(
            "notify_session needs async spawn: blocking: true already returns the result directly, so no notification is sent"
                .to_string(),
        );
    }
    let target = resolve_spawn_notify_target(db, notify_session, caller_session_id, spawn_workdir)?;
    let name = target.as_ref().map(|resolved| resolved.name.clone());
    let notify = target.map(|resolved| SubagentNotify {
        target_id: resolved.id,
        workdir: resolved.workdir,
        waker,
    });
    Ok((notify, name))
}

/// Enqueue one completion notice for `run_id` through the shared
/// `scheduled_prompts` pipeline (same writer the interactive hooks use, no
/// second delivery path). Skips silently when the run is already gone or
/// was collected before this fired — the full result stays collectable
/// until its TTL either way. A target that died since spawn time is
/// enqueued anyway: delivery fails its row like any other dead
/// destination. Never changes the persisted run status.
pub fn enqueue_subagent_notification(
    db: &Database,
    notify: &SubagentNotify,
    run_id: &str,
    timed_out: bool,
    elapsed_secs: u64,
) {
    let Ok(Some(record)) = db.get_subagent_run(run_id) else {
        return;
    };
    if record.collected_at.is_some() {
        return;
    }
    let label = notify_label(timed_out, &record.status);
    let stdout = record.stdout.as_deref().unwrap_or("");
    let stderr = record.stderr.as_deref().unwrap_or("");
    let snippet = notify_snippet(label, stdout, stderr);
    let message = format!(
        "[subagent {run_id}] status: {label} ({elapsed_secs}s)\n{snippet}\nfull result: subagent collect {run_id}"
    );
    let provenance = ScheduledPromptProvenance::subagent(run_id);
    let send_id = uuid::Uuid::new_v4().to_string();
    if db
        .insert_scheduled_prompt(
            &send_id,
            &message,
            &notify.target_id,
            Some(&notify.workdir),
            Utc::now(),
            None,
            Some(&provenance),
        )
        .is_ok()
    {
        notify.waker.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::scheduled_prompts::ScheduledPromptProvenance;
    use crate::db::NewSubagentRun;

    fn notify_test_db() -> Database {
        Database::new(&crate::test_support::temp_db_path()).expect("create test db")
    }

    fn add_session(db: &Database, id: &str, name: &str, workdir: &str) {
        db.insert_interactive_session(
            id,
            name,
            "opencode",
            workdir,
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
    }

    fn insert_finished_run(db: &Database, id: &str, stdout: &str, stderr: &str) {
        let now = Utc::now().to_rfc3339();
        let expires = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        db.insert_subagent_run(NewSubagentRun {
            id,
            platform: "opencode",
            model: None,
            prompt: "p",
            workdir: "/tmp",
            started_at: &now,
            expires_at: &expires,
        })
        .unwrap();
        db.complete_subagent_run(id, 0, stdout, stderr, None, &now)
            .unwrap();
    }

    fn insert_failed_run(db: &Database, id: &str, stderr: &str) {
        let now = Utc::now().to_rfc3339();
        let expires = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        db.insert_subagent_run(NewSubagentRun {
            id,
            platform: "opencode",
            model: None,
            prompt: "p",
            workdir: "/tmp",
            started_at: &now,
            expires_at: &expires,
        })
        .unwrap();
        db.fail_subagent_run(id, stderr, None, &now).unwrap();
    }

    fn test_notify(db_workdir: &str) -> SubagentNotify {
        SubagentNotify {
            target_id: "sess-1".to_string(),
            workdir: db_workdir.to_string(),
            waker: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn pending_for(db: &Database, session: &str) -> Vec<String> {
        db.list_pending_scheduled_prompts_for_session(session)
            .unwrap()
            .into_iter()
            .map(|row| row.prompt)
            .collect()
    }

    // ── resolution ────────────────────────────────────────────────

    #[test]
    fn omitted_from_live_session_defaults_to_that_session() {
        let db = notify_test_db();
        add_session(&db, "sess-1", "orch", "/tmp");
        let target = resolve_spawn_notify_target(&db, None, Some("sess-1"), "/tmp")
            .unwrap()
            .expect("caller session must notify by default");
        assert_eq!(target.id, "sess-1");
        assert_eq!(target.name, "orch");
    }

    #[test]
    fn omitted_from_non_interactive_caller_notifies_nobody() {
        let db = notify_test_db();
        add_session(&db, "sess-1", "orch", "/tmp");
        assert!(resolve_spawn_notify_target(&db, None, None, "/tmp")
            .unwrap()
            .is_none());
        assert!(
            resolve_spawn_notify_target(&db, None, Some("ghost"), "/tmp")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn explicit_target_resolves_by_id_and_by_name() {
        let db = notify_test_db();
        add_session(&db, "sess-1", "orch", "/tmp");
        let by_id = resolve_spawn_notify_target(&db, Some("sess-1"), None, "/tmp")
            .unwrap()
            .expect("id must resolve");
        assert_eq!((by_id.id.as_str(), by_id.name.as_str()), ("sess-1", "orch"));
        let by_name = resolve_spawn_notify_target(&db, Some("orch"), None, "/tmp")
            .unwrap()
            .expect("name must resolve");
        assert_eq!(by_name.id, "sess-1");
    }

    #[test]
    fn explicit_target_in_other_workdir_is_refused_naming_both() {
        let spawn_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let db = notify_test_db();
        let target_path = target_dir.path().to_str().unwrap();
        add_session(&db, "sess-1", "orch", target_path);
        let err = resolve_spawn_notify_target(
            &db,
            Some("orch"),
            None,
            spawn_dir.path().to_str().unwrap(),
        )
        .unwrap_err();
        assert!(
            err.contains(target_path),
            "refusal must name target workdir: {err}"
        );
        assert!(
            err.contains(spawn_dir.path().to_str().unwrap()),
            "refusal must name spawn workdir: {err}"
        );
    }

    #[test]
    fn explicit_self_target_skips_the_same_workdir_check() {
        // FR2 scopes the rule to targets *other than* the caller's own
        // session: naming yourself (by id or by name) accepts a spawn from
        // another directory, exactly like the omitted default.
        let spawn_dir = tempfile::tempdir().unwrap();
        let db = notify_test_db();
        add_session(&db, "sess-1", "orch", "/tmp");
        for requested in ["sess-1", "orch"] {
            let target = resolve_spawn_notify_target(
                &db,
                Some(requested),
                Some("sess-1"),
                spawn_dir.path().to_str().unwrap(),
            )
            .unwrap_or_else(|e| panic!("own session must notify, requested {requested}: {e}"))
            .expect("own session must notify");
            assert_eq!(target.id, "sess-1");
        }
    }

    #[test]
    fn other_target_is_refused_even_with_a_live_caller() {
        // The exemption above is identity-scoped: another session in another
        // workdir is still refused when the caller is a live session.
        let spawn_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let db = notify_test_db();
        add_session(&db, "sess-1", "orch", "/tmp");
        let target_path = target_dir.path().to_str().unwrap();
        add_session(&db, "sess-2", "peer", target_path);
        let err = resolve_spawn_notify_target(
            &db,
            Some("peer"),
            Some("sess-1"),
            spawn_dir.path().to_str().unwrap(),
        )
        .unwrap_err();
        assert!(
            err.contains(target_path) && err.contains(spawn_dir.path().to_str().unwrap()),
            "refusal must name both workdirs: {err}"
        );
    }

    #[test]
    fn same_workdir_ignores_trailing_slash() {
        let db = notify_test_db();
        add_session(&db, "sess-1", "orch", "/tmp");
        let target = resolve_spawn_notify_target(&db, Some("orch"), None, "/tmp/")
            .unwrap()
            .expect("trailing slash must not split the directory");
        assert_eq!(target.id, "sess-1");
    }

    #[test]
    fn unknown_target_is_refused() {
        let db = notify_test_db();
        let err = resolve_spawn_notify_target(&db, Some("ghost"), None, "/tmp").unwrap_err();
        assert!(
            err.contains("ghost") && err.contains("no longer live"),
            "got: {err}"
        );
    }

    #[test]
    fn ambiguous_name_is_refused() {
        let db = notify_test_db();
        add_session(&db, "sess-1", "orch", "/tmp");
        add_session(&db, "sess-2", "orch", "/tmp");
        let err = resolve_spawn_notify_target(&db, Some("orch"), None, "/tmp").unwrap_err();
        assert!(err.contains("2 live sessions"), "got: {err}");
    }

    // ── prepare (the name the async JSON reports as "notify") ──────

    #[test]
    fn prepare_default_notify_returns_caller_name() {
        let db = notify_test_db();
        add_session(&db, "sess-1", "orch", "/tmp");
        let (notify, name) = prepare_spawn_notify(
            &db,
            None,
            false,
            Some("sess-1"),
            "/tmp",
            Arc::new(tokio::sync::Notify::new()),
        )
        .unwrap();
        assert_eq!(name.as_deref(), Some("orch"));
        let notify = notify.expect("live caller must notify by default");
        assert_eq!(notify.target_id, "sess-1");
    }

    #[test]
    fn prepare_explicit_target_returns_its_name_by_id_and_by_name() {
        let db = notify_test_db();
        add_session(&db, "sess-1", "orch", "/tmp");
        for requested in ["sess-1", "orch"] {
            let (notify, name) = prepare_spawn_notify(
                &db,
                Some(requested),
                false,
                None,
                "/tmp",
                Arc::new(tokio::sync::Notify::new()),
            )
            .unwrap();
            assert_eq!(name.as_deref(), Some("orch"), "requested {requested}");
            assert!(notify.is_some());
        }
    }

    #[test]
    fn prepare_non_interactive_caller_notifies_nobody() {
        let db = notify_test_db();
        add_session(&db, "sess-1", "orch", "/tmp");
        let (notify, name) = prepare_spawn_notify(
            &db,
            None,
            false,
            None,
            "/tmp",
            Arc::new(tokio::sync::Notify::new()),
        )
        .unwrap();
        assert!(notify.is_none());
        assert!(name.is_none(), "JSON must render \"none\"");
    }

    // ── enqueue ───────────────────────────────────────────────────

    #[test]
    fn finished_run_notifies_with_stdout_and_finished_label() {
        let db = notify_test_db();
        insert_finished_run(&db, "run-fin", "hello out", "warn");
        enqueue_subagent_notification(&db, &test_notify("/tmp"), "run-fin", false, 7);
        let pending = pending_for(&db, "sess-1");
        assert_eq!(pending.len(), 1);
        let message = &pending[0];
        assert!(
            message.contains("[subagent run-fin] status: finished (7s)"),
            "got: {message}"
        );
        assert!(message.contains("hello out"), "got: {message}");
        assert!(
            message.contains("full result: subagent collect run-fin"),
            "got: {message}"
        );
        let row = &db
            .list_pending_scheduled_prompts_for_session("sess-1")
            .unwrap()[0];
        let provenance = row.provenance.clone().expect("notice carries provenance");
        assert_eq!(
            (provenance.kind.as_str(), provenance.event.as_str()),
            ("hook", "subagent")
        );
        assert_eq!(provenance.run_id.as_deref(), Some("run-fin"));
        assert!(db.get_subagent_run("run-fin").unwrap().is_some());
    }

    #[test]
    fn failed_run_notifies_with_failed_label_and_stderr() {
        let db = notify_test_db();
        insert_failed_run(&db, "run-fail", "boom happened");
        enqueue_subagent_notification(&db, &test_notify("/tmp"), "run-fail", false, 3);
        let pending = pending_for(&db, "sess-1");
        assert_eq!(pending.len(), 1);
        assert!(
            pending[0].contains("status: failed (3s)"),
            "got: {}",
            pending[0]
        );
        assert!(pending[0].contains("boom happened"), "got: {}", pending[0]);
    }

    #[test]
    fn timed_out_run_notifies_with_timed_out_label() {
        let db = notify_test_db();
        insert_failed_run(&db, "run-timeout", "Timed out");
        enqueue_subagent_notification(&db, &test_notify("/tmp"), "run-timeout", true, 90);
        let pending = pending_for(&db, "sess-1");
        assert_eq!(pending.len(), 1);
        assert!(
            pending[0].contains("status: timed_out (90s)"),
            "got: {}",
            pending[0]
        );
        assert!(pending[0].contains("Timed out"), "got: {}", pending[0]);
    }

    #[test]
    fn failed_run_with_empty_stderr_falls_back_to_stdout() {
        let db = notify_test_db();
        insert_failed_run(&db, "run-fallback", "");
        enqueue_subagent_notification(&db, &test_notify("/tmp"), "run-fallback", false, 1);
        let pending = pending_for(&db, "sess-1");
        assert_eq!(pending.len(), 1);
        assert!(pending[0].contains("status: failed"), "got: {}", pending[0]);
    }

    #[test]
    fn long_output_is_cut_at_two_thousand_chars() {
        let db = notify_test_db();
        let long = "x".repeat(2500);
        insert_finished_run(&db, "run-long", &long, "");
        enqueue_subagent_notification(&db, &test_notify("/tmp"), "run-long", false, 2);
        let pending = pending_for(&db, "sess-1");
        assert_eq!(pending.len(), 1);
        let message = &pending[0];
        let snippet_part = message
            .strip_prefix("[subagent run-long] status: finished (2s)\n")
            .expect("header line shape")
            .strip_suffix("\nfull result: subagent collect run-long")
            .expect("collect line shape");
        assert_eq!(snippet_part.chars().count(), NOTIFY_SNIPPET_CHARS);
        assert!(db.get_subagent_run("run-long").unwrap().is_some());
    }

    #[test]
    fn dead_target_still_enqueues_and_result_stays_collectable() {
        let db = notify_test_db();
        insert_finished_run(&db, "run-dead", "out", "");
        let mut notify = test_notify("/tmp");
        notify.target_id = "gone-session".to_string();
        enqueue_subagent_notification(&db, &notify, "run-dead", false, 1);
        assert_eq!(pending_for(&db, "gone-session").len(), 1);
        assert!(db.get_subagent_run("run-dead").unwrap().is_some());
    }

    #[test]
    fn collected_run_enqueues_nothing() {
        let db = notify_test_db();
        insert_finished_run(&db, "run-gone", "out", "");
        db.collect_subagent_run("run-gone").unwrap();
        enqueue_subagent_notification(&db, &test_notify("/tmp"), "run-gone", false, 1);
        assert!(pending_for(&db, "sess-1").is_empty());
    }

    #[test]
    fn provenance_constructor_carries_subagent_run_id() {
        let provenance = ScheduledPromptProvenance::subagent("run-9");
        assert_eq!(provenance.kind, "hook");
        assert_eq!(provenance.graph_id, "");
        assert_eq!(provenance.event, "subagent");
        assert_eq!(provenance.run_id.as_deref(), Some("run-9"));
        assert!(provenance.session_id.is_none());
    }
}
