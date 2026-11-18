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
