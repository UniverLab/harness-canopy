//! Part 05 of `tests`: delivery-task unit tests (moved from
//! `scheduled_prompts.rs`, pure move). Covers single-send delivery, the sleep
//! math, hold-while-typing, notification text and the delivery loop.
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::application::ports::AgentRepository;
use crate::db::scheduled_prompts::{ScheduledPrompt, ScheduledPromptProvenance};
use crate::db::Database;
use crate::scheduler::cron_scheduler::held_sends::{hold_sleep, HeldTracker};
use crate::scheduler::cron_scheduler::scheduled_prompts::{
    agent_trigger_delivery_body, deliver_due_scheduled_prompts, hook_delivery_body,
    hook_graph_name, next_delivery_sleep, notify_hook_delivery, run_delivery_loop,
    trigger_agent_name, MAX_DELIVERY_WAIT,
};
use crate::sessions::{SessionHub, SessionKind, SpawnRequest};

fn test_db() -> (Arc<Database>, TempDir) {
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

/// Poll the session's parsed screen until `needle` shows up, failing the
/// test if it never does.
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

/// FR4: with no TUI client anywhere, the daemon delivers a due send to the
/// live session that owns the target id.
#[test]
fn due_send_is_delivered_to_a_live_session_without_a_tui() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    // `cat` stands in for a chat CLI: whatever is written to its PTY comes
    // back on the screen.
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");

    db.insert_scheduled_prompt(
        "ss-live",
        "deliver me from the daemon",
        &session.id,
        Some("/tmp"),
        chrono::Utc::now() - chrono::Duration::minutes(1),
        None,
        None,
    )
    .expect("insert scheduled send");

    deliver_due_scheduled_prompts(&db, &hub, &mut HeldTracker::new());

    wait_for_output(&session, "deliver me from the daemon");
    assert!(
        db.list_due_scheduled_prompts(chrono::Utc::now())
            .expect("list due")
            .is_empty(),
        "a delivered send is consumed"
    );
    assert!(
        db.list_failed_scheduled_prompts_for_workdir("/tmp")
            .expect("list failed")
            .is_empty(),
        "a delivered send is never preserved as failed"
    );
    let _ = hub.kill(&session.id);
}

/// FR4: a send whose target is gone is preserved for recall, recorded as
/// the project's last prompt, and never silently dropped.
#[test]
fn due_send_to_a_dead_target_is_preserved_and_notified() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let workdir = "/tmp/scheduled-send-dead-target";

    db.insert_scheduled_prompt(
        "ss-dead",
        "please recover me",
        "session-that-no-longer-exists",
        Some(workdir),
        chrono::Utc::now() - chrono::Duration::minutes(1),
        None,
        None,
    )
    .expect("insert scheduled send");

    deliver_due_scheduled_prompts(&db, &hub, &mut HeldTracker::new());

    assert!(
        db.list_due_scheduled_prompts(chrono::Utc::now())
            .expect("list due")
            .is_empty(),
        "the send fired once, whether delivered or preserved"
    );
    let failed = db
        .list_failed_scheduled_prompts_for_workdir(workdir)
        .expect("list failed");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].prompt, "please recover me");
    let last = db
        .get_last_prompt_for_workdir(workdir)
        .expect("query last prompt")
        .expect("the dead target's prompt stays recallable");
    assert_eq!(last.prompt_text, "please recover me");
    assert!(last.builder_state.is_none());
}

/// A send targeting a session that exited is as dead as a missing one.
#[test]
fn due_send_to_an_exited_session_is_preserved() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let workdir = "/tmp/scheduled-send-exited";

    db.insert_scheduled_prompt(
        "ss-exited",
        "nobody is listening",
        "exited-session",
        Some(workdir),
        chrono::Utc::now() - chrono::Duration::minutes(1),
        None,
        None,
    )
    .expect("insert scheduled send");

    deliver_due_scheduled_prompts(&db, &hub, &mut HeldTracker::new());

    assert_eq!(
        db.list_failed_scheduled_prompts_for_workdir(workdir)
            .expect("list failed")
            .len(),
        1
    );
}

#[test]
fn next_scheduled_prompt_at_is_the_earliest_fire_time() {
    let (db, _dir) = test_db();
    assert_eq!(db.next_scheduled_prompt_at().expect("query"), None);

    let earlier = chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + 300, 0)
        .expect("whole-second timestamp");
    let later = chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + 1800, 0)
        .expect("whole-second timestamp");
    db.insert_scheduled_prompt("ss-later", "b", "sess", None, later, None, None)
        .unwrap();
    db.insert_scheduled_prompt("ss-earlier", "a", "sess", None, earlier, None, None)
        .unwrap();

    assert_eq!(db.next_scheduled_prompt_at().expect("query"), Some(earlier));
}

// ── notification text (moved from `tui::app::data`) ─────────────

#[test]
fn hook_delivery_body_names_the_graph() {
    let prov = ScheduledPromptProvenance::hook("g-canopy", "on_completed");
    assert_eq!(
        hook_delivery_body(&prov, Some("canopy-next")),
        "canopy-next · on_completed: message delivered to this session"
    );
}

#[test]
fn hook_delivery_body_falls_back_to_short_id() {
    let prov =
        ScheduledPromptProvenance::hook("969ee102-0138-4054-95fd-2946ef237431", "on_completed");
    assert_eq!(
        hook_delivery_body(&prov, None),
        "969ee102 · on_completed: message delivered to this session"
    );
}

#[test]
fn agent_trigger_delivery_body_names_the_agent() {
    let prov = ScheduledPromptProvenance::agent_trigger("nudger", "cron");
    assert_eq!(
        agent_trigger_delivery_body(&prov, Some("nudger")),
        "nudger (cron): message delivered to this session"
    );
}

#[test]
fn hook_delivery_names_an_existing_graph_from_the_db() {
    use crate::domain::graphs::{Graph, GraphStatus};
    let (db, _dir) = test_db();
    db.insert_graph(&Graph {
        trigger_prompt: None,
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "g-canopy-next".into(),
        name: "canopy-next".into(),
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
    })
    .expect("insert graph");

    let prov = ScheduledPromptProvenance::hook("g-canopy-next", "on_completed");
    let name = hook_graph_name(&db, &prov);
    assert_eq!(
        hook_delivery_body(&prov, name.as_deref()),
        "canopy-next · on_completed: message delivered to this session"
    );

    let unknown =
        ScheduledPromptProvenance::hook("969ee102-0138-4054-95fd-2946ef237431", "on_completed");
    assert_eq!(hook_graph_name(&db, &unknown), None);
}

#[test]
fn agent_trigger_delivery_names_an_existing_agent_from_the_db() {
    use crate::domain::models::{Agent, AgentAction, Cli, Trigger};
    let (db, _dir) = test_db();
    db.upsert_agent(&Agent {
        id: "nudger".into(),
        prompt: "test".into(),
        trigger: Some(Trigger::Cron {
            schedule_expr: "0 9 * * *".into(),
        }),
        cli: Cli::new("claude"),
        model: None,
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: chrono::Utc::now(),
        log_path: "/tmp/test.log".into(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: AgentAction::Headless,
    })
    .expect("insert agent");

    let prov = ScheduledPromptProvenance::agent_trigger("nudger", "cron");
    let name = trigger_agent_name(&db, &prov);
    assert_eq!(
        agent_trigger_delivery_body(&prov, name.as_deref()),
        "nudger (cron): message delivered to this session"
    );

    let unknown =
        ScheduledPromptProvenance::agent_trigger("969ee102-0138-4054-95fd-2946ef237431", "cron");
    assert_eq!(trigger_agent_name(&db, &unknown), None);
}

#[test]
fn notify_hook_delivery_is_true_for_hook_and_agent_trigger_provenance() {
    let (db, _dir) = test_db();
    let send = |provenance: Option<ScheduledPromptProvenance>| ScheduledPrompt {
        id: "ss".into(),
        prompt: "test".into(),
        target_session_id: "sess".into(),
        workdir: None,
        fire_at: chrono::Utc::now(),
        created_at: chrono::Utc::now(),
        builder_state: None,
        provenance,
    };

    assert!(notify_hook_delivery(
        &db,
        &send(Some(ScheduledPromptProvenance::hook(
            "graph-1",
            "on_failed"
        )))
    ));
    assert!(notify_hook_delivery(
        &db,
        &send(Some(ScheduledPromptProvenance::agent_trigger(
            "nudger", "cron"
        )))
    ));
    let other = ScheduledPromptProvenance {
        kind: "other".into(),
        graph_id: "graph-1".into(),
        event: "on_failed".into(),
        session_id: None,
        agent_id: None,
        trigger: None,
        target_session_name: None,
        node_id: None,
        run_id: None,
    };
    assert!(!notify_hook_delivery(&db, &send(Some(other))));
    assert!(!notify_hook_delivery(&db, &send(None)));
}

// ── dedicated delivery task (`run_delivery_loop`) ─────────────

/// Poll the session's parsed screen until `needle` shows up, failing the
/// test if it never does within `timeout`. Async, so the delivery task
/// keeps running while waiting.
async fn wait_for_delivery(session: &crate::sessions::HubSession, needle: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let contents = session.test_contents().unwrap_or_default();
        if contents.contains(needle) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "send not delivered in time: expected {needle:?} in {contents:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn delivery_loop(
    db: &Arc<Database>,
    hub: &SessionHub,
    notify: Arc<tokio::sync::Notify>,
) -> (
    tokio_util::sync::CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let cancel = tokio_util::sync::CancellationToken::new();
    // Window 0 here: these loop tests cover wake/delivery mechanics, and
    // merging is covered separately through the windowed entry point.
    let task = tokio::spawn(run_delivery_loop(
        Arc::clone(db),
        hub.clone(),
        notify,
        cancel.clone(),
        0,
    ));
    (cancel, task)
}

/// A notified send due in 1 s is delivered within 3 s, even with the
/// cron loop otherwise idle.
#[tokio::test]
async fn delivery_loop_wakes_on_notify_and_delivers_within_3s() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");

    let notify = Arc::new(tokio::sync::Notify::new());
    let (cancel, task) = delivery_loop(&db, &hub, Arc::clone(&notify));

    db.insert_scheduled_prompt(
        "ss-notify",
        "notify me fast",
        &session.id,
        Some("/tmp"),
        chrono::Utc::now() + chrono::Duration::seconds(1),
        None,
        None,
    )
    .expect("insert scheduled send");
    notify.notify_one();

    wait_for_delivery(&session, "notify me fast", Duration::from_secs(3)).await;
    cancel.cancel();
    task.await.expect("delivery task stops on cancel");
    let _ = hub.kill(&session.id);
}

/// A send stored without a notify (the TUI process, which cannot reach
/// the daemon's notifier) is still picked up on the 2-second ceiling.
#[tokio::test]
async fn delivery_loop_picks_up_unnotified_send_within_4s() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");

    let (cancel, task) = delivery_loop(&db, &hub, Arc::new(tokio::sync::Notify::new()));
    // Let the loop finish its first (empty) pass and settle into its
    // wait, so the send below can only be found by the ceiling wake.
    tokio::time::sleep(Duration::from_millis(200)).await;

    db.insert_scheduled_prompt(
        "ss-unnotified",
        "ceiling picks me up",
        &session.id,
        Some("/tmp"),
        chrono::Utc::now(),
        None,
        None,
    )
    .expect("insert scheduled send");

    wait_for_delivery(&session, "ceiling picks me up", Duration::from_secs(4)).await;
    cancel.cancel();
    task.await.expect("delivery task stops on cancel");
    let _ = hub.kill(&session.id);
}

/// Sends already due at startup are delivered on the first iteration.
#[tokio::test]
async fn delivery_loop_delivers_overdue_send_on_first_iteration() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");

    db.insert_scheduled_prompt(
        "ss-overdue",
        "already due at startup",
        &session.id,
        Some("/tmp"),
        chrono::Utc::now() - chrono::Duration::minutes(1),
        None,
        None,
    )
    .expect("insert scheduled send");

    let (cancel, task) = delivery_loop(&db, &hub, Arc::new(tokio::sync::Notify::new()));

    wait_for_delivery(&session, "already due at startup", Duration::from_secs(3)).await;
    cancel.cancel();
    task.await.expect("delivery task stops on cancel");
    let _ = hub.kill(&session.id);
}

/// The sleep math: nothing pending or a failure-shaped past instant
/// sleeps the full ceiling (no spin); a near-future send sleeps until
/// it; a far-future send is capped at the ceiling.
#[test]
fn next_delivery_sleep_math() {
    let (db, _dir) = test_db();
    assert_eq!(next_delivery_sleep(&db, false, 0), MAX_DELIVERY_WAIT);

    db.insert_scheduled_prompt(
        "ss-far",
        "far",
        "sess",
        None,
        chrono::Utc::now() + chrono::Duration::hours(1),
        None,
        None,
    )
    .unwrap();
    assert_eq!(next_delivery_sleep(&db, false, 0), MAX_DELIVERY_WAIT);

    // Whole-second arithmetic: `fire_at` is stored truncated to seconds,
    // so a sub-2 s offset would be timing-flaky. A send 2 whole seconds
    // out must still land strictly under the ceiling.
    let near = chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + 2, 0)
        .expect("whole-second timestamp");
    db.insert_scheduled_prompt("ss-near", "near", "sess", None, near, None, None)
        .unwrap();
    let sleep = next_delivery_sleep(&db, false, 0);
    assert!(
        sleep < MAX_DELIVERY_WAIT,
        "a send due in ~2 s must shorten the wait, got {sleep:?}"
    );
    assert!(
        sleep > Duration::from_millis(100),
        "a send due in ~2 s must sleep most of it, got {sleep:?}"
    );

    // A send already due (a persistently failing query leaves one
    // behind) sleeps the full ceiling instead of spinning at ~0.
    db.insert_scheduled_prompt(
        "ss-past",
        "past",
        "sess",
        None,
        chrono::Utc::now() - chrono::Duration::minutes(1),
        None,
        None,
    )
    .unwrap();
    assert_eq!(next_delivery_sleep(&db, false, 0), MAX_DELIVERY_WAIT);

    // While sends are held the wait is capped at the 500 ms retry,
    // whether the earliest send is past or far in the future.
    assert_eq!(next_delivery_sleep(&db, true, 0), hold_sleep());
    assert!(hold_sleep() <= Duration::from_millis(500));
}

/// A due send inside an open window shortens the sleep to the window
/// close, so a lone send goes out when its window closes, not later.
#[test]
fn next_delivery_sleep_wakes_at_window_close() {
    let (db, _dir) = test_db();
    // Whole-second arithmetic, like the math test above: the window
    // closes ~2 s out, strictly under the ceiling.
    let fire_at = chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() - 58, 0)
        .expect("whole-second timestamp");
    db.insert_scheduled_prompt("ss-window", "wait", "sess", None, fire_at, None, None)
        .unwrap();
    let sleep = next_delivery_sleep(&db, false, 60);
    assert!(
        sleep < MAX_DELIVERY_WAIT,
        "an open window must shorten the wait, got {sleep:?}"
    );
    assert!(
        sleep > Duration::from_millis(100),
        "a window closing in ~2 s must sleep most of it, got {sleep:?}"
    );
}

// ── hold while the user is typing ─────────────────────────────

/// Ids of the sends currently due, in delivery order.
fn due_ids(db: &Database) -> Vec<String> {
    db.list_due_scheduled_prompts(chrono::Utc::now())
        .expect("list due")
        .iter()
        .map(|send| send.id.clone())
        .collect()
}

fn due_send(
    db: &Database,
    id: &str,
    prompt: &str,
    session_id: &str,
    fire_at: chrono::DateTime<chrono::Utc>,
) {
    db.insert_scheduled_prompt(id, prompt, session_id, Some("/tmp"), fire_at, None, None)
        .expect("insert scheduled send");
}

fn overdue() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() - chrono::Duration::minutes(1)
}

/// Typing holds a due send: the row stays queued and the PTY receives
/// only what the user typed.
#[test]
fn typing_holds_a_due_send_and_keeps_the_row() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    session.write_to_pty(b"abc").expect("type abc");
    due_send(&db, "ss-held", "held-prompt-abc", &session.id, overdue());

    deliver_due_scheduled_prompts(&db, &hub, &mut HeldTracker::new());

    assert_eq!(due_ids(&db), vec!["ss-held".to_string()]);
    wait_for_output(&session, "abc");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !session
            .test_contents()
            .unwrap_or_default()
            .contains("held-prompt-abc"),
        "a held send never reaches the PTY"
    );
    let _ = hub.kill(&session.id);
}

/// Submitting the line releases the held send, delivered after the
/// user's own line.
#[test]
fn submitted_line_releases_the_held_send() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    session.write_to_pty(b"abc").expect("type abc");
    due_send(&db, "ss-held", "held-prompt-enter", &session.id, overdue());
    deliver_due_scheduled_prompts(&db, &hub, &mut HeldTracker::new());
    assert_eq!(due_ids(&db), vec!["ss-held".to_string()]);

    session.write_to_pty(b"\r").expect("submit the line");
    std::thread::sleep(Duration::from_millis(2200));
    deliver_due_scheduled_prompts(&db, &hub, &mut HeldTracker::new());

    assert!(due_ids(&db).is_empty(), "the released send is consumed");
    let contents = wait_for_output(&session, "held-prompt-enter");
    let typed = contents.find("abc").expect("user line echoed");
    let pasted = contents.find("held-prompt-enter").expect("send echoed");
    assert!(
        typed < pasted,
        "send lands after the user line: {contents:?}"
    );
    let _ = hub.kill(&session.id);
}

/// Cursor keys, focus reports and mouse reports alone never hold a send.
#[test]
fn escape_sequences_alone_do_not_hold_a_send() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    session
        .write_to_pty(b"\x1b[A\x1b[B\x1bOC\x1bOD\x1b[I\x1b[O")
        .expect("arrows and focus reports");
    session
        .write_to_pty(b"\x1b[<0;10;20M")
        .expect("mouse report");
    due_send(&db, "ss-seq", "seq-prompt", &session.id, overdue());

    deliver_due_scheduled_prompts(&db, &hub, &mut HeldTracker::new());

    assert!(due_ids(&db).is_empty(), "nothing typed: send is consumed");
    wait_for_output(&session, "seq-prompt");
    let _ = hub.kill(&session.id);
}

/// Ctrl-U clears the composing line, but only once the recency window
/// expires; a stale dirty line holds indefinitely until it is cleared.
#[test]
fn ctrl_u_clears_the_line_and_releases_the_send() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    session.write_to_pty(b"abc").expect("type abc");
    due_send(&db, "ss-held", "held-prompt-ctrl-u", &session.id, overdue());
    let mut held = HeldTracker::new();
    deliver_due_scheduled_prompts(&db, &hub, &mut held);
    assert_eq!(due_ids(&db), vec!["ss-held".to_string()]);

    // A stale dirty line never releases on its own.
    std::thread::sleep(Duration::from_millis(2200));
    deliver_due_scheduled_prompts(&db, &hub, &mut held);
    assert_eq!(due_ids(&db), vec!["ss-held".to_string()]);

    // Clearing the line releases the send once the recency window
    // of the Ctrl-U keystroke itself expires.
    session.write_to_pty(b"\x15").expect("ctrl-u clears");
    deliver_due_scheduled_prompts(&db, &hub, &mut held);
    assert_eq!(due_ids(&db), vec!["ss-held".to_string()]);
    std::thread::sleep(Duration::from_millis(2200));
    deliver_due_scheduled_prompts(&db, &hub, &mut held);

    assert!(due_ids(&db).is_empty(), "the cleared send is consumed");
    wait_for_output(&session, "held-prompt-ctrl-u");
    let _ = hub.kill(&session.id);
}

/// Two held sends stay queued in `fire_at` order and deliver in that
/// order once the user is idle — none jumps ahead of an earlier hold.
#[test]
fn two_held_sends_deliver_in_fire_at_order() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    session.write_to_pty(b"abc").expect("type abc");
    // Inserted later-first so the answer must come from `fire_at` order.
    due_send(&db, "ss-second", "order-second-BBB", &session.id, overdue());
    due_send(
        &db,
        "ss-first",
        "order-first-AAA",
        &session.id,
        overdue() - chrono::Duration::minutes(1),
    );

    let mut held = HeldTracker::new();
    deliver_due_scheduled_prompts(&db, &hub, &mut held);
    assert_eq!(
        due_ids(&db),
        vec!["ss-first".to_string(), "ss-second".to_string()]
    );

    session.write_to_pty(b"\x15").expect("ctrl-u clears");
    std::thread::sleep(Duration::from_millis(2200));
    deliver_due_scheduled_prompts(&db, &hub, &mut held);

    assert!(due_ids(&db).is_empty(), "both sends are consumed");
    let contents = wait_for_output(&session, "order-second-BBB");
    assert!(
        contents.contains("order-first-AAA"),
        "first send echoed: {contents:?}"
    );
    let first = contents.find("order-first-AAA").expect("first send echoed");
    let second = contents
        .find("order-second-BBB")
        .expect("second send echoed");
    assert!(
        first < second,
        "sends deliver in fire_at order: {contents:?}"
    );
    let _ = hub.kill(&session.id);
}

/// A session with no user input is never composing: delivery is
/// immediate.
#[test]
fn fresh_session_without_input_delivers_immediately() {
    let (db, _dir) = test_db();
    let hub = test_hub(&db);
    let session = hub.spawn(&agent_spawn("cat")).expect("spawn cat session");
    assert!(!session.user_is_composing());
    due_send(&db, "ss-fresh", "fresh-prompt", &session.id, overdue());

    deliver_due_scheduled_prompts(&db, &hub, &mut HeldTracker::new());

    assert!(due_ids(&db).is_empty(), "no input: send is consumed");
    wait_for_output(&session, "fresh-prompt");
    let _ = hub.kill(&session.id);
}
