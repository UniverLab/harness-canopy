use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use crate::db::Database;
use crate::domain::graphs::{
    Graph, GraphSpec, GraphSpecStatus, GraphStatus, SpecReviewFinding, SpecReviewState,
    SpecReviewTier, SpecType,
};
use crate::domain::queues::Queue;
use crate::graph_engine::spec_review;

struct HomeGuard(Option<String>);
impl HomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let old = std::env::var("HOME").ok();
        std::env::set_var("HOME", path);
        Self(old)
    }
}
impl Drop for HomeGuard {
    fn drop(&mut self) {
        if let Some(v) = &self.0 {
            std::env::set_var("HOME", v);
        } else {
            std::env::remove_var("HOME");
        }
    }
}

fn test_db() -> (tempfile::TempDir, Arc<Database>) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    (dir, Arc::new(db))
}

fn make_spec(id: &str, name: &str, intent: Option<&str>) -> GraphSpec {
    GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: name.to_string(),
        description: Some("objective\n# Functional requirements\n- r".to_string()),
        intent: intent.map(|s| s.to_string()),
        position: 0,
        parallelizable: false,
        status: GraphSpecStatus::Pending,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_type: Some(SpecType::Feat),
        spec_scope: None,
        spec_code: None,
        review_state: SpecReviewState::Unreviewed,
        review_tier: None,
        review_findings: None,
        created_by_session: None,
    }
}

fn write_config(home: &std::path::Path, enabled: bool, max_parallel: u32) {
    let dir = home.join(".canopy");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), format!(
        "[spec_review]\nenabled = {enabled}\nplatform = \"stub\"\nmodel = \"m\"\nmax_parallel = {max_parallel}\n"
    )).unwrap();
}

async fn process_all(db: &Arc<Database>) {
    let _ = db.requeue_in_review_specs();
    for _ in 0..50 {
        let Some(spec) = db.claim_next_unreviewed_spec().unwrap() else {
            break;
        };
        spec_review::review_one(Arc::clone(db), spec).await.unwrap();
    }
}

#[tokio::test]
async fn approve_trivial_and_normal() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 2);
    let (_dir, db) = test_db();
    db.insert_graph_spec(&make_spec("s1", "a", Some("i")))
        .unwrap();
    db.insert_graph_spec(&make_spec("s2", "b", Some("i")))
        .unwrap();
    let n = Arc::new(AtomicUsize::new(0));
    spec_review::set_review_runner_for_tests(Arc::new(move |_, _, _| {
        let n = n.clone();
        Box::pin(async move {
            let i = n.fetch_add(1, Ordering::SeqCst);
            if i.is_multiple_of(2) {
                Ok(r#"{"verdict":"approve","tier":"trivial","findings":[]}"#.into())
            } else {
                Ok(r#"{"verdict":"approve","tier":"normal","findings":[]}"#.into())
            }
        })
    }));
    process_all(&db).await;
    spec_review::clear_review_runner_for_tests();
    let a = db.get_graph_spec("s1").unwrap().unwrap();
    let b = db.get_graph_spec("s2").unwrap().unwrap();
    assert_eq!(a.review_state, SpecReviewState::Approved);
    assert_eq!(b.review_state, SpecReviewState::Approved);
    assert!(a.review_tier.is_some() && b.review_tier.is_some());
}

#[tokio::test]
async fn reject_with_findings() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 2);
    let (_dir, db) = test_db();
    db.insert_graph_spec(&make_spec("s1", "a", Some("i")))
        .unwrap();
    spec_review::set_review_runner_for_tests(Arc::new(|_, _, _| {
        Box::pin(async {
            Ok(r#"{"verdict":"reject","tier":"normal","findings":[{"severity":"high","text":"bad"}]}"#.into())
        })
    }));
    process_all(&db).await;
    spec_review::clear_review_runner_for_tests();
    let s = db.get_graph_spec("s1").unwrap().unwrap();
    assert_eq!(s.review_state, SpecReviewState::Rejected);
    assert_eq!(s.review_findings.as_ref().unwrap()[0].text, "bad");
}

#[tokio::test]
async fn unparsable_then_retry_then_review_error() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 2);
    let (_dir, db) = test_db();
    db.insert_graph_spec(&make_spec("s1", "a", Some("i")))
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = calls.clone();
    spec_review::set_review_runner_for_tests(Arc::new(move |_, _, _| {
        let calls = calls2.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok("not json".into())
        })
    }));
    process_all(&db).await;
    spec_review::clear_review_runner_for_tests();
    let s = db.get_graph_spec("s1").unwrap().unwrap();
    assert_eq!(s.review_state, SpecReviewState::ReviewError);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn restart_recovery_requeues_in_review() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 2);
    let (_dir, db) = test_db();
    db.insert_graph_spec(&make_spec("s1", "a", Some("i")))
        .unwrap();
    db.claim_next_unreviewed_spec().unwrap();
    assert_eq!(db.count_in_review_specs().unwrap(), 1);
    db.requeue_in_review_specs().unwrap();
    assert_eq!(db.count_in_review_specs().unwrap(), 0);
    let s = db.get_graph_spec("s1").unwrap().unwrap();
    assert_eq!(s.review_state, SpecReviewState::Unreviewed);
}

#[tokio::test]
async fn concurrency_never_above_max_parallel() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 2);
    let (_dir, db) = test_db();
    for (id, name) in [("s1", "a"), ("s2", "b"), ("s3", "c")] {
        db.insert_graph_spec(&make_spec(id, name, Some("i")))
            .unwrap();
    }
    let active = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));
    let active2 = active.clone();
    let max2 = max_seen.clone();
    spec_review::set_review_runner_for_tests(Arc::new(move |_, _, _| {
        let active = active2.clone();
        let max_seen = max2.clone();
        Box::pin(async move {
            let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
            loop {
                let prev = max_seen.load(Ordering::SeqCst);
                if cur <= prev {
                    break;
                }
                if max_seen
                    .compare_exchange(prev, cur, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(r#"{"verdict":"approve","tier":"normal","findings":[]}"#.into())
        })
    }));
    // First pass claims/spawns up to max_parallel.
    spec_review::run_pending_reviews(Arc::clone(&db))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    // Drain any remainder.
    for _ in 0..10 {
        spec_review::run_pending_reviews(Arc::clone(&db))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if db.count_in_review_specs().unwrap() == 0 {
            let next = db.claim_next_unreviewed_spec().unwrap();
            if let Some(s) = next {
                db.mark_spec_unreviewed(&s.id).unwrap();
            } else {
                break;
            }
        }
    }
    spec_review::clear_review_runner_for_tests();
    assert!(max_seen.load(Ordering::SeqCst) <= 2);
    for id in ["s1", "s2", "s3"] {
        let s = db.get_graph_spec(id).unwrap().unwrap();
        assert_eq!(s.review_state, SpecReviewState::Approved);
    }
}

#[tokio::test]
async fn update_spec_resetting_state_marks_unreviewed() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 2);
    let (_dir, db) = test_db();
    let mut spec = make_spec("s1", "a", Some("i"));
    spec.review_state = SpecReviewState::Approved;
    spec.review_tier = Some(SpecReviewTier::Normal);
    db.insert_graph_spec(&spec).unwrap();
    crate::daemon::handler_specs_helpers::mark_spec_dirty_for_review(&db, "s1").unwrap();
    let s = db.get_graph_spec("s1").unwrap().unwrap();
    assert_eq!(s.review_state, SpecReviewState::Unreviewed);
    assert!(s.review_tier.is_none());
    assert!(s.review_findings.is_none());
}

fn sample_graph(id: &str) -> Graph {
    Graph {
        id: id.to_string(),
        name: format!("g-{id}"),
        description: None,
        workdir: "/tmp".to_string(),
        status: GraphStatus::Draft,
        trigger: None,
        trigger_prompt: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        archived: false,
        paused_by_reconciliation: false,
        infra_node_id: None,
        allow_dirty_start: false,
        require_spec_type: false,
        enabled: true,
        enable_at: None,
        expires_at: None,
    }
}

#[tokio::test]
async fn attach_spec_null_intent_reviewed_and_refused_until_approved() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 2);
    let (_dir, db) = test_db();
    db.insert_graph(&sample_graph("g1")).unwrap();
    let mut spec = make_spec("s1", "bound", None);
    spec.graph_id = Some("g1".to_string());
    spec.review_state = SpecReviewState::Unreviewed;
    spec.intent = None;
    db.insert_graph_spec(&spec).unwrap();
    let stored = db.get_graph_spec("s1").unwrap().unwrap();
    assert_eq!(stored.intent, None);
    assert_eq!(stored.review_state, SpecReviewState::Unreviewed);

    let err = crate::daemon::handler_run_prepare::validate_specs_approved_for_run(&db, "g1", None)
        .unwrap_err();
    assert!(
        err.contains("spec 'bound' is unreviewed: review findings: []"),
        "{err}"
    );

    db.set_spec_review_result(
        "s1",
        SpecReviewState::Approved,
        Some(SpecReviewTier::Normal),
        Some(&[]),
    )
    .unwrap();
    crate::daemon::handler_run_prepare::validate_specs_approved_for_run(&db, "g1", None).unwrap();
}

#[tokio::test]
async fn queue_refusal_and_run_skip() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 2);
    let (_dir, db) = test_db();
    let mut spec = make_spec("s1", "qspec", Some("i"));
    spec.review_state = SpecReviewState::Rejected;
    spec.review_findings = Some(vec![SpecReviewFinding {
        severity: "high".to_string(),
        text: "bad".to_string(),
    }]);
    db.insert_graph_spec(&spec).unwrap();

    db.insert_queue(&Queue {
        id: "q1".to_string(),
        name: "q1".to_string(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    // Force-insert member to simulate a queue run scenario for skip detection.
    db.append_queue_member("q1", "s1", None).unwrap();
    // Run skip detection: queue run skips non-approved pending members.
    let skipped = db.queue_unapproved_pending_members("q1").unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].id, "s1");

    // Queue run must not be refused at prelaunch for non-approved members; the
    // run proceeds and dispatch skips them (and logs each skip once).
    db.insert_graph(&sample_graph("gq")).unwrap();
    crate::daemon::handler_run_prepare::validate_specs_approved_for_run(&db, "gq", Some("q1"))
        .unwrap();
}

#[tokio::test]
async fn placeholder_never_queued() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 2);
    let (_dir, db) = test_db();
    db.insert_graph(&sample_graph("g1")).unwrap();
    let placeholder = crate::graph_engine::no_spec_placeholder("g1");
    assert_eq!(placeholder.review_state, SpecReviewState::Approved);
    assert!(placeholder.review_tier.is_none());
    assert!(crate::graph_engine::is_no_spec_placeholder(&placeholder));
    db.insert_graph_spec(&placeholder).unwrap();
    // Must not be picked up for review.
    let claimed = db.claim_next_unreviewed_spec().unwrap();
    assert!(claimed.is_none());
    let stored = db.get_graph_spec(&placeholder.id).unwrap().unwrap();
    assert_eq!(stored.review_state, SpecReviewState::Approved);
}

fn make_spec_for_session(id: &str, name: &str, intent: Option<&str>, session: &str) -> GraphSpec {
    let mut spec = make_spec(id, name, intent);
    spec.created_by_session = Some(session.to_string());
    spec
}

fn insert_session(db: &Database, id: &str, workdir: &str) {
    db.insert_interactive_session(id, id, "test", workdir, None, None, "interactive", None)
        .unwrap();
}

async fn process_all_sync(db: &Arc<Database>) {
    // Process sequentially so drains see a stable state per completion.
    let _ = db.requeue_in_review_specs();
    while let Some(spec) = db.claim_next_unreviewed_spec().unwrap() {
        spec_review::review_one(Arc::clone(db), spec).await.unwrap();
    }
}

#[tokio::test]
async fn three_parallel_specs_produce_one_message() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 3);
    let (_dir, db) = test_db();
    insert_session(&db, "sess-a", "/tmp/proj");
    for (id, name) in [("s1", "one"), ("s2", "two"), ("s3", "three")] {
        db.insert_graph_spec(&make_spec_for_session(id, name, Some("i"), "sess-a"))
            .unwrap();
    }
    spec_review::set_review_runner_for_tests(Arc::new(|_, _, _| {
        Box::pin(async { Ok(r#"{"verdict":"approve","tier":"normal","findings":[]}"#.into()) })
    }));
    // Run all reviews concurrently via the worker path logic: claim+spawn
    // up to max_parallel, wait for completions, then drain remaining.
    for _ in 0..20 {
        spec_review::run_pending_reviews(Arc::clone(&db))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if db.count_in_review_specs().unwrap() == 0 {
            if db.claim_next_unreviewed_spec().unwrap().is_none() {
                break;
            } else {
                // shouldn't happen, but keep consistent
            }
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    spec_review::clear_review_runner_for_tests();
    let sends = db
        .list_pending_scheduled_prompts_for_session("sess-a")
        .unwrap();
    assert_eq!(
        sends.len(),
        1,
        "expected one batched message, got {sends:?}"
    );
    assert!(sends[0].prompt.contains("3 reviewed"));
    assert!(sends[0].prompt.contains("one -> approved"));
    assert!(sends[0].prompt.contains("two -> approved"));
    assert!(sends[0].prompt.contains("three -> approved"));
}

#[tokio::test]
async fn mid_drain_spec_joins_same_message() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 2);
    let (_dir, db) = test_db();
    insert_session(&db, "sess-a", "/tmp/proj");
    db.insert_graph_spec(&make_spec_for_session("s1", "first", Some("i"), "sess-a"))
        .unwrap();
    db.insert_graph_spec(&make_spec_for_session("s2", "second", Some("i"), "sess-a"))
        .unwrap();

    spec_review::set_review_runner_for_tests(Arc::new(|_, _, _| {
        Box::pin(async { Ok(r#"{"verdict":"approve","tier":"normal","findings":[]}"#.into()) })
    }));

    // Complete s1 while s2 remains in_review: no drain fires because session
    // still has a pending review.
    let a = db.claim_next_unreviewed_spec().unwrap().unwrap();
    let b = db.claim_next_unreviewed_spec().unwrap().unwrap();
    spec_review::review_one(Arc::clone(&db), a).await.unwrap();
    assert!(db
        .list_pending_scheduled_prompts_for_session("sess-a")
        .unwrap()
        .is_empty());

    // Spec added while drain is pending joins the same eventual message.
    db.insert_graph_spec(&make_spec_for_session("s3", "third", Some("i"), "sess-a"))
        .unwrap();
    let c = db.claim_next_unreviewed_spec().unwrap().unwrap();
    spec_review::review_one(Arc::clone(&db), c).await.unwrap();
    assert!(db
        .list_pending_scheduled_prompts_for_session("sess-a")
        .unwrap()
        .is_empty());

    // Last pending spec completes -> one drain containing all three.
    spec_review::review_one(Arc::clone(&db), b).await.unwrap();
    spec_review::clear_review_runner_for_tests();
    let sends = db
        .list_pending_scheduled_prompts_for_session("sess-a")
        .unwrap();
    assert_eq!(
        sends.len(),
        1,
        "expected one batched message, got {sends:?}"
    );
    assert!(
        sends[0].prompt.contains("3 reviewed"),
        "{}",
        sends[0].prompt
    );
    assert!(sends[0].prompt.contains("first"), "{}", sends[0].prompt);
    assert!(sends[0].prompt.contains("second"), "{}", sends[0].prompt);
    assert!(sends[0].prompt.contains("third"), "{}", sends[0].prompt);
}

#[tokio::test]
async fn two_sessions_get_one_message_each() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 2);
    let (_dir, db) = test_db();
    insert_session(&db, "sess-a", "/tmp/a");
    insert_session(&db, "sess-b", "/tmp/b");
    db.insert_graph_spec(&make_spec_for_session("s1", "a1", Some("i"), "sess-a"))
        .unwrap();
    db.insert_graph_spec(&make_spec_for_session("s2", "b1", Some("i"), "sess-b"))
        .unwrap();
    spec_review::set_review_runner_for_tests(Arc::new(|_, _, _| {
        Box::pin(async { Ok(r#"{"verdict":"approve","tier":"normal","findings":[]}"#.into()) })
    }));
    process_all_sync(&db).await;
    spec_review::clear_review_runner_for_tests();
    let a = db
        .list_pending_scheduled_prompts_for_session("sess-a")
        .unwrap();
    let b = db
        .list_pending_scheduled_prompts_for_session("sess-b")
        .unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    assert!(a[0].prompt.contains("a1") && !a[0].prompt.contains("b1"));
    assert!(b[0].prompt.contains("b1") && !b[0].prompt.contains("a1"));
}

#[tokio::test]
async fn dead_session_preserves_message() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 1);
    let (_dir, db) = test_db();
    insert_session(&db, "sess-dead", "/tmp/dead");
    db.insert_graph_spec(&make_spec_for_session("s1", "gone", Some("i"), "sess-dead"))
        .unwrap();
    // make session not live/dead by marking it finished/removed from active? delivery uses hub liveness;
    // simulate by removing session row so delivery treats as dead target? get_session_workdir may fail.
    // Keep row for workdir resolution but deliver against empty hub.
    spec_review::set_review_runner_for_tests(Arc::new(|_, _, _| {
        Box::pin(async { Ok(r#"{"verdict":"approve","tier":"normal","findings":[]}"#.into()) })
    }));
    process_all_sync(&db).await;
    spec_review::clear_review_runner_for_tests();
    let sends = db
        .list_pending_scheduled_prompts_for_session("sess-dead")
        .unwrap();
    assert_eq!(sends.len(), 1);
    // Deliver with no live sessions -> preserved as failed (dead-target path).
    let hub = crate::sessions::SessionHub::with_broadcast_capacity(Arc::clone(&db), "h".into(), 8);
    let mut held = crate::scheduler::cron_scheduler::held_sends::HeldTracker::new();
    crate::scheduler::cron_scheduler::scheduled_prompts::deliver_due_scheduled_prompts(
        &db, &hub, &mut held,
    );
    let failed = db
        .list_failed_scheduled_prompts_for_workdir("/tmp/dead")
        .unwrap();
    assert_eq!(failed.len(), 1);
    assert!(failed[0].prompt.contains("gone"));
}

#[tokio::test]
async fn null_session_produces_no_message_but_spec_readable() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 1);
    let (_dir, db) = test_db();
    db.insert_graph_spec(&make_spec("s1", "anon", Some("i")))
        .unwrap();
    spec_review::set_review_runner_for_tests(Arc::new(|_, _, _| {
        Box::pin(async { Ok(r#"{"verdict":"approve","tier":"trivial","findings":[]}"#.into()) })
    }));
    process_all_sync(&db).await;
    spec_review::clear_review_runner_for_tests();
    // No scheduled prompts at all.
    let due = db
        .list_due_scheduled_prompts(chrono::Utc::now() + chrono::Duration::hours(1))
        .unwrap();
    assert!(due.is_empty());
    let spec = db.get_graph_spec("s1").unwrap().unwrap();
    assert_eq!(spec.review_state, SpecReviewState::Approved);
    assert_eq!(spec.name, "anon");
}

#[tokio::test]
async fn reupdate_after_drain_opens_new_set_without_resending_old() {
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    write_config(home.path(), true, 1);
    let (_dir, db) = test_db();
    insert_session(&db, "sess-a", "/tmp/proj");
    db.insert_graph_spec(&make_spec_for_session("s1", "first", Some("i"), "sess-a"))
        .unwrap();
    spec_review::set_review_runner_for_tests(Arc::new(|_, _, _| {
        Box::pin(async { Ok(r#"{"verdict":"approve","tier":"normal","findings":[]}"#.into()) })
    }));
    process_all_sync(&db).await;
    let sends = db
        .list_pending_scheduled_prompts_for_session("sess-a")
        .unwrap();
    assert_eq!(sends.len(), 1);
    // consume first send
    for s in &sends {
        db.delete_scheduled_prompt(&s.id).unwrap();
    }

    // re-update triggers re-review: mark unreviewed as handler would
    crate::daemon::handler_specs_helpers::mark_spec_dirty_for_review(&db, "s1").unwrap();
    process_all_sync(&db).await;
    spec_review::clear_review_runner_for_tests();
    let sends2 = db
        .list_pending_scheduled_prompts_for_session("sess-a")
        .unwrap();
    assert_eq!(sends2.len(), 1, "only new drain, old set never resent");
    assert!(sends2[0].prompt.contains("1 reviewed"));
}

/// The drain send carries the reviewed specs' graph id, so a merged delivery
/// can name it in the `from: spec review (graph ...)` origin line.
#[tokio::test]
async fn drain_send_carries_the_specs_graph_id() {
    let (_dir, db) = test_db();
    insert_session(&db, "sess-g", "/tmp/proj");
    db.insert_graph(&sample_graph("g-9")).unwrap();
    let mut spec = make_spec_for_session("s9", "ninth", Some("i"), "sess-g");
    spec.graph_id = Some("g-9".to_string());
    spec.review_state = SpecReviewState::Approved;
    db.insert_graph_spec(&spec).unwrap();

    crate::graph_engine::spec_review_drain::drain_if_ready(&db, Some("sess-g"), None).unwrap();

    let sends = db
        .list_pending_scheduled_prompts_for_session("sess-g")
        .unwrap();
    assert_eq!(sends.len(), 1);
    let provenance = sends[0]
        .provenance
        .as_ref()
        .expect("drain send carries provenance");
    assert_eq!(provenance.kind, "spec_review");
    assert_eq!(provenance.graph_id, "g-9");
}

fn reviewed_spec(id: &str, state: SpecReviewState, tier: Option<SpecReviewTier>) -> GraphSpec {
    let mut spec = make_spec(id, id, Some("i"));
    spec.review_state = state;
    spec.review_tier = tier;
    spec
}

fn finding(severity: &str, text: &str) -> SpecReviewFinding {
    SpecReviewFinding {
        severity: severity.to_string(),
        text: text.to_string(),
    }
}

/// The drain message pins the spec's exact format: header counts, rejected
/// and error blocks before approved ones, tier brackets, finding lines.
#[test]
fn format_message_orders_blocks_and_names_tiers() {
    use crate::graph_engine::spec_review_drain::format_review_message;
    let mut rejected = reviewed_spec("s-rej", SpecReviewState::Rejected, None);
    rejected.review_findings = Some(vec![finding("high", "contradicts intent")]);
    let mut errored = reviewed_spec("s-err", SpecReviewState::ReviewError, None);
    errored.review_findings = Some(vec![finding("high", "boom")]);
    let mut big = reviewed_spec(
        "s-big",
        SpecReviewState::Approved,
        Some(SpecReviewTier::Normal),
    );
    big.review_findings = Some(vec![finding("low", "nit"), finding("medium", "smell")]);
    let tiny = reviewed_spec(
        "s-tiny",
        SpecReviewState::Approved,
        Some(SpecReviewTier::Trivial),
    );

    let msg = format_review_message(&[tiny, big, errored, rejected]);
    let expected = "Spec review finished: 4 reviewed, 2 approved (1 trivial, 1 normal), 1 rejected, 1 errors.\n\
        s-rej -> rejected\n  [high] contradicts intent\n\
        s-err -> review_error\n  [high] boom\n\
        s-big -> approved [normal]\n  [low] nit\n  [medium] smell\n\
        s-tiny -> approved [trivial]";
    assert_eq!(msg, expected);
}

/// Over-cap messages drop low-severity findings first and end with the
/// recall suffix instead of silently truncating.
#[test]
fn format_message_caps_at_6000_chars_dropping_low_first() {
    use crate::graph_engine::spec_review_drain::format_review_message;
    let mut noisy = reviewed_spec(
        "s-big",
        SpecReviewState::Approved,
        Some(SpecReviewTier::Normal),
    );
    let mut findings = vec![finding("medium", "keep me")];
    for i in 0..200 {
        findings.push(finding("low", &format!("nit number {i} with padding text")));
    }
    noisy.review_findings = Some(findings);

    let msg = format_review_message(&[noisy]);
    assert!(msg.chars().count() <= 6000, "len {}", msg.chars().count());
    assert!(
        msg.contains("  [medium] keep me"),
        "medium findings survive a low-severity trim:\n{msg}"
    );
    assert!(
        !msg.contains("[low]"),
        "low findings are dropped first:\n{msg}"
    );
    assert!(
        msg.ends_with("(+200 more findings: read them with the spec tool)"),
        "capped message ends with the recall suffix:\n{msg}"
    );
}
