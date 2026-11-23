//! Delivery-merge behaviour: due sends for one session inside one window
//! arrive as a single message (FR8). Uses the real hub with `cat` sessions,
//! the same stub the delivery task's own tests use.

use crate::db::Database;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::db::scheduled_prompts::ScheduledPromptProvenance;
use crate::scheduler::cron_scheduler::held_sends::HeldTracker;
use crate::scheduler::cron_scheduler::scheduled_prompts::deliver_due_scheduled_prompts_with_window;
use crate::sessions::{SessionHub, SessionKind, SpawnRequest};

fn test_db() -> (Arc<Database>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::new(&dir.path().join("test.db")).expect("test database");
    (Arc::new(db), dir)
}

fn test_hub(db: &Arc<Database>) -> SessionHub {
    SessionHub::with_broadcast_capacity(Arc::clone(db), "test-host".to_string(), 256)
}

fn agent_spawn(cli: &str) -> SpawnRequest {
    SpawnRequest {
        kind: SessionKind::Agent,
        cli: cli.to_string(),
        model: None,
        workdir: "/tmp".to_string(),
        name: None,
        prompt: None,
        interactive_args: None,
        fallback_args: None,
        model_flag: None,
        id: None,
        harness_conversation_id: None,
        seed_id: None,
        accent: None,
        yolo: false,
        preserve_args: false,
    }
}

fn whole_seconds_ago(secs: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() - secs, 0)
        .expect("whole-second timestamp")
}

fn due_send(
    db: &Database,
    id: &str,
    prompt: &str,
    session_id: &str,
    fire_at: chrono::DateTime<chrono::Utc>,
    provenance: Option<&ScheduledPromptProvenance>,
) {
    db.insert_scheduled_prompt(
        id,
        prompt,
        session_id,
        Some("/tmp"),
        fire_at,
        None,
        provenance,
    )
    .expect("insert scheduled send");
}

fn due_ids(db: &Database) -> Vec<String> {
    db.list_due_scheduled_prompts(chrono::Utc::now())
        .expect("list due")
        .iter()
        .map(|send| send.id.clone())
        .collect()
}

fn wait_for_output(session: &crate::sessions::HubSession, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let contents = session.test_contents().unwrap_or_default();
        if contents.contains(needle) {
            return contents;
        }
        assert!(
            Instant::now() < deadline,
            "output never arrived: expected {needle:?} in {contents:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn named_graph(id: &str, name: &str) -> crate::domain::graphs::Graph {
    use crate::domain::graphs::{Graph, GraphStatus};
    Graph {
        trigger_prompt: None,
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: id.into(),
        name: name.into(),
        description: None,
        workdir: "/tmp".into(),
        status: GraphStatus::Draft,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
        enabled: true,
        enable_at: None,
        expires_at: None,
    }
}

/// Two sends 10 s apart arrive as one message, in order, with separators
/// and origin lines.
#[test]
fn two_sends_ten_seconds_apart_arrive_as_one_ordered_message() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    db.insert_graph(&named_graph("g-merge", "merge-graph"))
        .expect("insert graph");

    due_send(
        &db,
        "ss-first",
        "merge-first-AAA",
        &session.id,
        whole_seconds_ago(120),
        Some(&ScheduledPromptProvenance::hook("g-merge", "on_failed")),
    );
    due_send(
        &db,
        "ss-second",
        "merge-second-BBB",
        &session.id,
        whole_seconds_ago(110),
        None,
    );

    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut HeldTracker::new(), 60);

    assert!(due_ids(&db).is_empty(), "a merged group is consumed");
    let contents = wait_for_output(&session, "--- delivery 2 of 2 ---");
    assert!(contents.contains("--- delivery 1 of 2 ---"), "{contents:?}");
    assert!(
        contents.contains("from: graph hook merge-graph (on_failed)"),
        "{contents:?}"
    );
    assert!(contents.contains("from: you (scheduled"), "{contents:?}");
    let first = contents.find("merge-first-AAA").expect("first echoed");
    let second = contents.find("merge-second-BBB").expect("second echoed");
    assert!(first < second, "parts stay in fire_at order: {contents:?}");
    let _ = hub.kill(&session.id);
}

/// A lone send is delivered when its window closes, and not before.
#[test]
fn lone_send_waits_for_its_window_close() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    let fire_at = chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + 2, 0)
        .expect("whole-second timestamp");
    due_send(&db, "ss-lone", "lone-prompt", &session.id, fire_at, None);

    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut HeldTracker::new(), 4);
    std::thread::sleep(Duration::from_millis(2500));
    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut HeldTracker::new(), 4);
    assert_eq!(
        due_ids(&db),
        vec!["ss-lone".to_string()],
        "the window is still open: nothing delivered yet"
    );

    std::thread::sleep(Duration::from_millis(4500));
    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut HeldTracker::new(), 4);
    assert!(due_ids(&db).is_empty(), "the closed window delivers");
    wait_for_output(&session, "lone-prompt");
    let _ = hub.kill(&session.id);
}

/// Window 0 delivers each send separately, exactly as today.
#[test]
fn zero_window_disables_merging() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    due_send(
        &db,
        "ss-a",
        "zero-first",
        &session.id,
        whole_seconds_ago(120),
        None,
    );
    due_send(
        &db,
        "ss-b",
        "zero-second",
        &session.id,
        whole_seconds_ago(110),
        None,
    );

    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut HeldTracker::new(), 0);

    assert!(due_ids(&db).is_empty(), "both sends are consumed");
    let contents = wait_for_output(&session, "zero-second");
    assert!(contents.contains("zero-first"), "{contents:?}");
    assert!(
        !contents.contains("--- delivery"),
        "no separator without merging: {contents:?}"
    );
    let _ = hub.kill(&session.id);
}

/// A held group stays unsplit until typing clears, and keeps collecting
/// meanwhile.
#[test]
fn held_group_collects_and_delivers_unsplit() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    session.write_to_pty(b"abc").expect("type abc");
    due_send(
        &db,
        "ss-h1",
        "held-one",
        &session.id,
        whole_seconds_ago(120),
        None,
    );
    due_send(
        &db,
        "ss-h2",
        "held-two",
        &session.id,
        whole_seconds_ago(110),
        None,
    );

    let mut held = HeldTracker::new();
    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut held, 60);
    assert_eq!(due_ids(&db).len(), 2, "typing holds the whole group");

    due_send(
        &db,
        "ss-h3",
        "held-three",
        &session.id,
        chrono::Utc::now(),
        None,
    );
    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut held, 60);
    assert_eq!(due_ids(&db).len(), 3, "the open group keeps collecting");

    session.write_to_pty(b"\x15").expect("ctrl-u clears");
    std::thread::sleep(Duration::from_millis(2200));
    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut held, 60);

    assert!(due_ids(&db).is_empty(), "the released group is consumed");
    let contents = wait_for_output(&session, "--- delivery 3 of 3 ---");
    assert!(contents.contains("--- delivery 1 of 3 ---"), "{contents:?}");
    let one = contents.find("held-one").expect("one echoed");
    let two = contents.find("held-two").expect("two echoed");
    let three = contents.find("held-three").expect("three echoed");
    assert!(one < two && two < three, "order kept: {contents:?}");
    let _ = hub.kill(&session.id);
}

/// 25 due parts arrive as a 20-part message then a 5-part message, in order.
#[test]
fn twenty_five_parts_arrive_as_twenty_then_five() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    for i in 0..25 {
        due_send(
            &db,
            &format!("ss-{i:02}"),
            &format!("merge-part-{i:02}"),
            &session.id,
            whole_seconds_ago(600 - i64::from(i)),
            None,
        );
    }

    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut HeldTracker::new(), 60);

    assert!(due_ids(&db).is_empty(), "both chunks are consumed");
    let contents = wait_for_output(&session, "--- delivery 25 of 25 ---");
    assert!(
        contents.contains("--- delivery 21 of 25 ---"),
        "{contents:?}"
    );
    // The first chunk scrolled past the live view: assert over all text
    // that the chunks went out consecutive and in order.
    let all = session.test_all_text().unwrap_or_default();
    assert!(all.contains("--- delivery 1 of 25 ---"), "first chunk");
    assert!(all.contains("--- delivery 20 of 25 ---"), "first chunk end");
    let first = all.find("merge-part-00").expect("first echoed");
    let last = all.find("merge-part-24").expect("last echoed");
    assert!(first < last, "chunks preserve order");
    let _ = hub.kill(&session.id);
}

/// A single 100 KiB part goes alone in its own message, intact.
#[test]
fn oversize_part_goes_alone_and_intact() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    let big = format!("BIGSTART{}BIGEND", "z".repeat(100 * 1024));
    due_send(
        &db,
        "ss-big",
        &big,
        &session.id,
        whole_seconds_ago(120),
        None,
    );
    due_send(
        &db,
        "ss-small",
        "tiny-part",
        &session.id,
        whole_seconds_ago(110),
        None,
    );

    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut HeldTracker::new(), 60);

    assert!(due_ids(&db).is_empty(), "both messages are consumed");
    let contents = wait_for_output(&session, "tiny-part");
    assert!(contents.contains("--- delivery 2 of 2 ---"), "{contents:?}");
    // The oversize part scrolled far past the live view: assert over all
    // text that it went out alone in its own message, intact and in order.
    let all = session.test_all_text().unwrap_or_default();
    assert!(all.contains("BIGSTART"), "the oversize write landed");
    assert!(all.contains("BIGEND"), "the oversize part is never split");
    assert!(
        all.find("BIGSTART") < all.find("BIGEND"),
        "the oversize part is intact"
    );
    assert!(all.contains("--- delivery 1 of 2 ---"), "goes alone");
    let _ = hub.kill(&session.id);
}

/// Spec-review and deliver provenances render their origin lines.
#[test]
fn spec_review_and_deliver_origins_render() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    db.insert_graph(&named_graph("g-9", "review-graph"))
        .expect("insert graph");
    let mut review = ScheduledPromptProvenance::spec_review();
    review.graph_id = "g-9".to_string();
    due_send(
        &db,
        "ss-review",
        "review-body",
        &session.id,
        whole_seconds_ago(120),
        Some(&review),
    );
    due_send(
        &db,
        "ss-deliver",
        "deliver-body",
        &session.id,
        whole_seconds_ago(110),
        Some(&ScheduledPromptProvenance::deliver("g-9", "n-1", "r-1")),
    );

    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut HeldTracker::new(), 60);

    assert!(due_ids(&db).is_empty(), "the group is consumed");
    let contents = wait_for_output(&session, "deliver-body");
    assert!(
        contents.contains("from: spec review (graph review-graph)"),
        "{contents:?}"
    );
    assert!(
        contents.contains("from: deliver (graph review-graph)"),
        "{contents:?}"
    );
    let _ = hub.kill(&session.id);
}

/// The builder origin names the row's own `fire_at`.
#[test]
fn builder_origin_carries_the_rows_fire_at() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    let fire_at = whole_seconds_ago(3600);
    due_send(&db, "ss-t1", "time-one", &session.id, fire_at, None);
    due_send(
        &db,
        "ss-t2",
        "time-two",
        &session.id,
        whole_seconds_ago(3500),
        None,
    );

    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut HeldTracker::new(), 60);

    assert!(due_ids(&db).is_empty(), "the group is consumed");
    let expected = fire_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let contents = wait_for_output(&session, "time-two");
    assert!(
        contents.contains(&format!("from: you (scheduled {expected})")),
        "{contents:?}"
    );
    let _ = hub.kill(&session.id);
}

/// Sends for two different sessions are never mixed.
#[test]
fn sessions_are_never_mixed() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let first = hub.spawn(&agent_spawn("cat")).expect("spawn first session");
    let second = hub
        .spawn(&agent_spawn("cat"))
        .expect("spawn second session");
    due_send(
        &db,
        "ss-a1",
        "session-a-one",
        &first.id,
        whole_seconds_ago(120),
        None,
    );
    due_send(
        &db,
        "ss-a2",
        "session-a-two",
        &first.id,
        whole_seconds_ago(110),
        None,
    );
    due_send(
        &db,
        "ss-b1",
        "session-b-only",
        &second.id,
        whole_seconds_ago(115),
        None,
    );

    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut HeldTracker::new(), 60);

    assert!(due_ids(&db).is_empty(), "both groups are consumed");
    let a = wait_for_output(&first, "session-a-two");
    assert!(a.contains("--- delivery 1 of 2 ---"), "{a:?}");
    assert!(!a.contains("session-b-only"), "no mixing: {a:?}");
    let b = wait_for_output(&second, "session-b-only");
    assert!(
        !b.contains("--- delivery"),
        "a lone send has no separator: {b:?}"
    );
    let _ = hub.kill(&first.id);
    let _ = hub.kill(&second.id);
}

/// A gone target fails each of its rows through the dead-target path.
#[test]
fn dead_target_fails_each_row() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let workdir = "/tmp/delivery-merge-dead-target";
    for (id, prompt) in [("ss-d1", "dead-one"), ("ss-d2", "dead-two")] {
        db.insert_scheduled_prompt(
            id,
            prompt,
            "session-that-no-longer-exists",
            Some(workdir),
            whole_seconds_ago(120),
            None,
            None,
        )
        .expect("insert scheduled send");
    }

    deliver_due_scheduled_prompts_with_window(&db, &hub, &mut HeldTracker::new(), 60);

    assert!(due_ids(&db).is_empty(), "dead rows are consumed");
    let failed = db
        .list_failed_scheduled_prompts_for_workdir(workdir)
        .expect("list failed");
    assert_eq!(failed.len(), 2, "one failed row per send: {failed:?}");
}
