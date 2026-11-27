//! Delivering due scheduled sends from the daemon.
//!
//! Moved here from `tui::app::data`: with the hub owning every PTY, a schedule
//! fires on the daemon's dedicated delivery task whether or not a TUI is
//! attached. A send whose target session is gone keeps the prompt recoverable
//! and says so, exactly as `preserve_dead_scheduled_prompt` did in the TUI.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::delivery_merge::{self, ResolvedPart};
use super::held_sends::{hold_sleep, HeldTracker, MergeWindows};
use crate::application::ports::AgentRepository;
use crate::db::scheduled_prompts::{is_target_alive, ScheduledPrompt, ScheduledPromptProvenance};
use crate::db::Database;
use crate::domain::notification::{send_notification, NotificationLevel};
use crate::sessions::compose_guard::Delivery;
use crate::sessions::{SessionHub, SessionKind, SessionStatus};

/// Longest the delivery task sleeps between iterations. Sends created by
/// another process (the TUI, which cannot reach the daemon's notifier) are
/// picked up on this ceiling instead of a creation wake.
pub(crate) const MAX_DELIVERY_WAIT: Duration = Duration::from_secs(2);

/// Dedicated delivery task, spawned by [`super::CronScheduler::start`] when
/// a hub is present. Each iteration delivers everything due off the async
/// executor, then waits for the earliest of a creation wake, the next
/// `fire_at`, and the 2-second ceiling. Stops on the scheduler's
/// cancellation token.
pub(crate) async fn run_delivery_loop(
    db: Arc<Database>,
    hub: SessionHub,
    notify: Arc<Notify>,
    cancel: CancellationToken,
    merge_window_s: u64,
) {
    let mut held = HeldTracker::new();
    loop {
        deliver_once(&db, &hub, &mut held, merge_window_s).await;
        let sleep = next_delivery_sleep(&db, !held.is_empty(), merge_window_s);
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = notify.notified() => {},
            _ = tokio::time::sleep(sleep) => {},
        }
    }
}

/// One delivery pass, off the async executor: delivery writes to a PTY and
/// waits out the target CLI's paste-settle window. Held sends ride along in
/// `held` so their hold times survive the blocking hop.
async fn deliver_once(
    db: &Arc<Database>,
    hub: &SessionHub,
    held: &mut HeldTracker,
    merge_window_s: u64,
) {
    let db = Arc::clone(db);
    let hub = hub.clone();
    let mut tracker = std::mem::take(held);
    let joined = tokio::task::spawn_blocking(move || {
        deliver_due_scheduled_prompts_with_window(&db, &hub, &mut tracker, merge_window_s);
        tracker
    })
    .await;
    match joined {
        Ok(tracker) => *held = tracker,
        Err(_) => tracing::warn!("Scheduled-send delivery task failed"),
    }
}

/// How long the delivery task sleeps before its next pass: the time until
/// the next send is due, capped at the 2-second ceiling — or at the 500 ms
/// hold retry while sends are held, so a held send is retried promptly
/// without spinning. Anything else — no pending sends, a send already due
/// (the pass just above handled it), or a failed query — sleeps the full
/// cap, so a persistently failing query retries on a bounded interval
/// instead of spinning.
pub(crate) fn next_delivery_sleep(db: &Database, held: bool, merge_window_s: u64) -> Duration {
    let cap = if held {
        hold_sleep().min(MAX_DELIVERY_WAIT)
    } else {
        MAX_DELIVERY_WAIT
    };
    let now = chrono::Utc::now();
    let mut sleep = match db.next_scheduled_prompt_at() {
        Ok(Some(fire_at)) => {
            if fire_at > now {
                (fire_at - now).to_std().unwrap_or(cap).min(cap)
            } else {
                cap
            }
        }
        _ => cap,
    };
    if merge_window_s > 0 {
        sleep = sleep.min(window_close_wake(db, now, merge_window_s).unwrap_or(cap));
    }
    sleep
}

/// How long until the earliest still-open merge window closes, so the loop
/// wakes exactly at a window close instead of up to one ceiling late.
fn window_close_wake(
    db: &Database,
    now: chrono::DateTime<chrono::Utc>,
    merge_window_s: u64,
) -> Option<Duration> {
    let due = db.list_due_scheduled_prompts(now).ok()?;
    let mut earliest: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
    for send in &due {
        earliest
            .entry(send.target_session_id.clone())
            .and_modify(|at| *at = (*at).min(send.fire_at))
            .or_insert(send.fire_at);
    }
    let mut windows = MergeWindows::new();
    windows.seed(&earliest, Duration::from_secs(merge_window_s));
    windows.until_earliest_close(now)
}

/// Deliver every send whose `fire_at` has passed: to its live target session
/// (unless the user is typing there, in which case the send stays queued),
/// or — when that session is gone — to the failed-sends table plus a desktop
/// notification.
///
/// Test-only shorthand for [`deliver_due_scheduled_prompts_with_window`]
/// with the registry default: production always threads the configured
/// window through, so the shorthand has no non-test caller.
#[cfg(test)]
pub fn deliver_due_scheduled_prompts(db: &Database, hub: &SessionHub, held: &mut HeldTracker) {
    deliver_due_scheduled_prompts_with_window(
        db,
        hub,
        held,
        delivery_merge::default_merge_window_s(),
    );
}

/// `deliver_due_scheduled_prompts` with an explicit merge window, in seconds.
/// `0` restores today's per-send behaviour exactly; otherwise due sends for
/// one session that fall inside one window arrive as a single message.
pub fn deliver_due_scheduled_prompts_with_window(
    db: &Database,
    hub: &SessionHub,
    held: &mut HeldTracker,
    window_s: u64,
) {
    let window_s = window_s.min(300);
    let now = chrono::Utc::now();
    let due = match db.list_due_scheduled_prompts(now) {
        Ok(due) => due,
        Err(e) => {
            tracing::warn!("Failed to list due scheduled sends: {e}");
            return;
        }
    };
    if due.is_empty() {
        return;
    }
    held.retain(&due.iter().map(|send| send.id.clone()).collect());

    let live_session_ids = live_agent_ids(hub);
    if window_s == 0 {
        // Sends arrive ordered by `fire_at` and are delivered in that order. Once
        // a send is held, later sends for the same session wait behind it, so a
        // composing state flipping mid-pass can never reorder them.
        let mut held_sessions: HashSet<String> = HashSet::new();
        for send in &due {
            deliver_one_send(
                db,
                hub,
                send,
                now,
                &live_session_ids,
                held,
                &mut held_sessions,
            );
        }
        return;
    }
    deliver_grouped_sends(db, hub, &due, now, &live_session_ids, held, window_s);
}

/// Deliver due sends grouped by target session: each session's group goes
/// out as one merged message once its window closes, in `fire_at` order.
fn deliver_grouped_sends(
    db: &Database,
    hub: &SessionHub,
    due: &[ScheduledPrompt],
    now: chrono::DateTime<chrono::Utc>,
    live_session_ids: &[String],
    held: &mut HeldTracker,
    window_s: u64,
) {
    let mut groups: HashMap<String, Vec<&ScheduledPrompt>> = HashMap::new();
    for send in due {
        groups
            .entry(send.target_session_id.clone())
            .or_default()
            .push(send);
    }
    let mut ordered: Vec<Vec<&ScheduledPrompt>> = groups.into_values().collect();
    ordered.sort_by(|a, b| {
        (earliest_fire_at(a), &a[0].target_session_id)
            .cmp(&(earliest_fire_at(b), &b[0].target_session_id))
    });
    let mut windows = MergeWindows::new();
    windows.seed(
        &ordered
            .iter()
            .map(|group| (group[0].target_session_id.clone(), earliest_fire_at(group)))
            .collect(),
        Duration::from_secs(window_s),
    );
    let ctx = GroupCtx {
        db,
        hub,
        now,
        live_session_ids,
    };
    let mut held_sessions: HashSet<String> = HashSet::new();
    for group in &ordered {
        deliver_group(&ctx, group, held, &mut held_sessions, &windows);
    }
}

/// Read-only per-pass context for delivering one session's group.
struct GroupCtx<'a> {
    db: &'a Database,
    hub: &'a SessionHub,
    now: chrono::DateTime<chrono::Utc>,
    live_session_ids: &'a [String],
}

/// The earliest `fire_at` in a non-empty group: the window's anchor.
fn earliest_fire_at(group: &[&ScheduledPrompt]) -> chrono::DateTime<chrono::Utc> {
    group
        .iter()
        .map(|send| send.fire_at)
        .min()
        .expect("a delivery group is never empty")
}

/// Deliver one session's group: dead targets fail per row as today, an open
/// window waits, a lone send follows today's path exactly, and several sends
/// go out as one merged message.
fn deliver_group(
    ctx: &GroupCtx<'_>,
    group: &[&ScheduledPrompt],
    held: &mut HeldTracker,
    held_sessions: &mut HashSet<String>,
    windows: &MergeWindows,
) {
    let session_id = group[0].target_session_id.clone();
    if held_sessions.contains(&session_id) {
        hold_group(ctx.hub, group, held);
        return;
    }
    if !is_target_alive(&session_id, ctx.live_session_ids) {
        for send in group {
            preserve_dead_scheduled_prompt(ctx.db, send, ctx.now);
            held.record_delivered(&send.id);
            delete_send_row(ctx.db, send);
        }
        return;
    }
    if windows.is_open(&session_id, ctx.now) {
        return;
    }
    if group.len() == 1 {
        deliver_one_send(
            ctx.db,
            ctx.hub,
            group[0],
            ctx.now,
            ctx.live_session_ids,
            held,
            held_sessions,
        );
        return;
    }
    deliver_merged_group(ctx.db, ctx.hub, group, held, held_sessions);
}

/// Hold every send of a group, keeping each send's original hold time.
fn hold_group(hub: &SessionHub, group: &[&ScheduledPrompt], held: &mut HeldTracker) {
    for send in group {
        hold_send(hub, send, held);
    }
}

/// One send plus the display names its origin line needs.
fn resolve_part<'a>(
    db: &Database,
    hub: &SessionHub,
    send: &'a ScheduledPrompt,
) -> ResolvedPart<'a> {
    let provenance = send.provenance.as_ref();
    let graph_name = provenance
        .filter(|provenance| !provenance.graph_id.is_empty())
        .and_then(|provenance| hook_graph_name(db, provenance));
    let session_name = provenance
        .and_then(|provenance| provenance.session_id.as_deref())
        .and_then(|id| hub.get(id).map(|session| session.name()));
    let agent_name = provenance.and_then(|provenance| trigger_agent_name(db, provenance));
    ResolvedPart {
        send,
        graph_name,
        session_name,
        agent_name,
    }
}

/// Write one session's group as consecutive merged messages, in order. Each
/// message goes through the same paste-and-submit path a single send uses;
/// rows are consumed only after their message's write succeeds, and a write
/// failure leaves every remaining row queued for the next pass.
fn deliver_merged_group(
    db: &Database,
    hub: &SessionHub,
    group: &[&ScheduledPrompt],
    held: &mut HeldTracker,
    held_sessions: &mut HashSet<String>,
) {
    let session_id = group[0].target_session_id.clone();
    let Some(session) = hub.get(&session_id) else {
        // Raced with the liveness check above: preserve, never drop.
        for send in group {
            preserve_dead_scheduled_prompt(db, send, chrono::Utc::now());
            held.record_delivered(&send.id);
            delete_send_row(db, send);
        }
        return;
    };
    let mut parts: Vec<ResolvedPart<'_>> = group
        .iter()
        .map(|send| resolve_part(db, hub, send))
        .collect();
    delivery_merge::sort_parts(&mut parts);
    let total = parts.len();
    let blocks: Vec<String> = parts
        .iter()
        .enumerate()
        .map(|(index, part)| delivery_merge::render_block(index, total, part))
        .collect();
    let packed = delivery_merge::pack_blocks(&blocks);
    let ready = PackedGroup {
        parts,
        blocks,
        packed,
    };
    write_merged_chunks(db, hub, &session, &ready, held, held_sessions);
}

/// One session's merged group ready to write: parts in delivery order, their
/// rendered blocks, and the size-capped chunking.
struct PackedGroup<'a> {
    parts: Vec<ResolvedPart<'a>>,
    blocks: Vec<String>,
    packed: Vec<Vec<usize>>,
}

/// Write each packed chunk; the first held or failed chunk stops the group
/// with the rest untouched.
fn write_merged_chunks(
    db: &Database,
    hub: &SessionHub,
    session: &crate::sessions::HubSession,
    ready: &PackedGroup<'_>,
    held: &mut HeldTracker,
    held_sessions: &mut HashSet<String>,
) {
    let session_id = ready.parts[0].send.target_session_id.as_str();
    let mut position = 0;
    while position < ready.packed.len() {
        let selected: Vec<String> = ready.packed[position]
            .iter()
            .map(|index| ready.blocks[*index].clone())
            .collect();
        let text = delivery_merge::render_message(&selected);
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            session.prompt_if_idle(&text)
        }));
        match attempt {
            Ok(Ok(Delivery::Delivered)) => {
                for index in &ready.packed[position] {
                    let send = ready.parts[*index].send;
                    held.record_delivered(&send.id);
                    delete_send_row(db, send);
                    notify_hook_delivery(db, send);
                }
                position += 1;
            }
            Ok(Ok(Delivery::Held)) => {
                held_sessions.insert(session_id.to_owned());
                for rest in &ready.packed[position..] {
                    for index in rest {
                        hold_send(hub, ready.parts[*index].send, held);
                    }
                }
                return;
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    "Merged send to session '{session_id}' failed: {error}; leaving it queued"
                );
                return;
            }
            Err(_) => {
                tracing::warn!("Merged send to session '{session_id}' panicked; leaving it queued");
                return;
            }
        }
    }
}

/// Deliver one due send and consume its schedule — unless the user is
/// typing, in which case the row is kept untouched for a later pass. A
/// failure — error or panic — is logged and never stops the loop or drops
/// the sends behind it.
fn deliver_one_send(
    db: &Database,
    hub: &SessionHub,
    send: &ScheduledPrompt,
    now: chrono::DateTime<chrono::Utc>,
    live_session_ids: &[String],
    held: &mut HeldTracker,
    held_sessions: &mut HashSet<String>,
) {
    if held_sessions.contains(&send.target_session_id) {
        hold_send(hub, send, held);
        return;
    }
    match attempt_delivery(db, hub, send, now, live_session_ids) {
        Ok(Delivery::Delivered) => {
            held.record_delivered(&send.id);
            delete_send_row(db, send);
        }
        Ok(Delivery::Held) => {
            held_sessions.insert(send.target_session_id.clone());
            hold_send(hub, send, held);
        }
        Err(_) => {
            tracing::warn!(
                "Scheduled send '{}' delivery panicked; continuing with the rest",
                send.id
            );
            delete_send_row(db, send);
        }
    }
}

/// Run one delivery attempt, catching panics so one bad send never stops
/// the loop or drops the sends behind it.
fn attempt_delivery(
    db: &Database,
    hub: &SessionHub,
    send: &ScheduledPrompt,
    now: chrono::DateTime<chrono::Utc>,
    live_session_ids: &[String],
) -> std::thread::Result<Delivery> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if is_target_alive(&send.target_session_id, live_session_ids) {
            deliver_live_scheduled_prompt(db, hub, send)
        } else {
            preserve_dead_scheduled_prompt(db, send, now);
            Delivery::Delivered
        }
    }))
}

/// Delete a consumed schedule row, logging instead of failing.
fn delete_send_row(db: &Database, send: &ScheduledPrompt) {
    if let Err(e) = db.delete_scheduled_prompt(&send.id) {
        tracing::warn!("Failed to delete scheduled send '{}': {e}", send.id);
    }
}

/// Ids of the hub sessions a send can be delivered to: live agent sessions
/// only (a terminal or an exited child cannot take a prompt).
fn live_agent_ids(hub: &SessionHub) -> Vec<String> {
    hub.list()
        .iter()
        .filter(|session| {
            session.kind == SessionKind::Agent && session.status() == SessionStatus::Running
        })
        .map(|session| session.id.clone())
        .collect()
}

/// Deliver one due send to its live target session — unless the user is
/// typing there, in which case nothing is written and the send stays queued.
/// Returns whether the schedule row is consumed.
fn deliver_live_scheduled_prompt(
    db: &Database,
    hub: &SessionHub,
    send: &ScheduledPrompt,
) -> Delivery {
    let Some(session) = hub.get(&send.target_session_id) else {
        return Delivery::Delivered;
    };
    match session.prompt_if_idle(&send.prompt) {
        Ok(Delivery::Delivered) => {
            tracing::info!(
                "Scheduled send '{}' delivered to session '{}'",
                send.id,
                send.target_session_id
            );
            notify_hook_delivery(db, send);
            Delivery::Delivered
        }
        Ok(Delivery::Held) => {
            tracing::info!(
                "Scheduled send '{}' held: user is typing in session '{}'",
                send.id,
                send.target_session_id
            );
            Delivery::Held
        }
        Err(e) => {
            tracing::warn!("Scheduled send '{}' delivery failed: {e}", send.id);
            send_notification(
                "Scheduled send failed",
                &format!("Could not deliver prompt: {e}"),
                NotificationLevel::Error,
            );
            Delivery::Delivered
        }
    }
}

/// Keep a held send queued; after a minute of holding, tell the user once.
fn hold_send(hub: &SessionHub, send: &ScheduledPrompt, held: &mut HeldTracker) {
    held.record_held(&send.id);
    if held.held_overdue(&send.id) {
        let name = hub
            .get(&send.target_session_id)
            .map(|session| session.name())
            .unwrap_or_else(|| send.target_session_id.clone());
        send_notification(
            "Scheduled send held",
            &format!("Delivery held: you are typing in {name}"),
            NotificationLevel::Info,
        );
    }
}

pub(crate) fn notify_hook_delivery(db: &Database, send: &ScheduledPrompt) -> bool {
    let Some(provenance) = send.provenance.as_ref() else {
        return false;
    };
    match provenance.kind.as_str() {
        "hook" => {
            let name = hook_graph_name(db, provenance);
            send_notification(
                "Hook message delivered",
                &hook_delivery_body(provenance, name.as_deref()),
                NotificationLevel::Info,
            );
            true
        }
        "agent_trigger" => {
            let name = trigger_agent_name(db, provenance);
            send_notification(
                "Agent message delivered",
                &agent_trigger_delivery_body(provenance, name.as_deref()),
                NotificationLevel::Info,
            );
            true
        }
        _ => false,
    }
}

/// Name of the graph behind a hook delivery; `None` once it is gone.
pub(crate) fn hook_graph_name(
    db: &Database,
    provenance: &ScheduledPromptProvenance,
) -> Option<String> {
    db.get_graph(&provenance.graph_id)
        .ok()
        .flatten()
        .map(|g| g.name)
}

/// Name of the agent behind an agent-trigger delivery; `None` once it is
/// gone. An agent's name is its id, so the id is what the user sees.
pub(crate) fn trigger_agent_name(
    db: &Database,
    provenance: &ScheduledPromptProvenance,
) -> Option<String> {
    provenance
        .agent_id
        .as_deref()
        .and_then(|id| db.get_agent(id).ok().flatten())
        .map(|a| a.id)
}

/// Keep the prompt of a send whose target is gone recoverable, and tell the
/// operator it was not delivered.
fn preserve_dead_scheduled_prompt(
    db: &Database,
    send: &ScheduledPrompt,
    now: chrono::DateTime<chrono::Utc>,
) {
    tracing::warn!(
        "Scheduled send '{}': target session '{}' not found; prompt preserved for recall",
        send.id,
        send.target_session_id
    );
    if let Err(e) = db.insert_failed_scheduled_prompt(
        &send.id,
        &send.prompt,
        &send.target_session_id,
        send.workdir.as_deref(),
        now,
        send.provenance.as_ref(),
    ) {
        tracing::warn!(
            "Failed to preserve failed scheduled send '{}': {e}",
            send.id
        );
    }
    record_failed_send_recall(db, send, now);
    send_notification(
        "Scheduled send: session not found",
        &format!(
            "Target session '{}' is no longer active. The prompt is preserved for recall.",
            send.target_session_id
        ),
        NotificationLevel::Warning,
    );
}

fn record_failed_send_recall(
    db: &Database,
    send: &ScheduledPrompt,
    now: chrono::DateTime<chrono::Utc>,
) {
    let Some(workdir) = send.workdir.as_deref() else {
        return;
    };
    let id = format!("lp-{}", uuid::Uuid::new_v4());
    if let Err(e) = db.insert_last_prompt(&id, workdir, &send.prompt, None, now) {
        tracing::warn!(
            "Failed to record last prompt for failed send '{}': {e}",
            send.id
        );
    }
}

fn display_name<'a>(resolved: Option<&'a str>, id: &'a str) -> &'a str {
    resolved
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| id.get(..8).unwrap_or(id))
}

pub(crate) fn hook_delivery_body(
    provenance: &ScheduledPromptProvenance,
    name: Option<&str>,
) -> String {
    format!(
        "{} · {}: message delivered to this session",
        display_name(name, &provenance.graph_id),
        provenance.event
    )
}

pub(crate) fn agent_trigger_delivery_body(
    provenance: &ScheduledPromptProvenance,
    name: Option<&str>,
) -> String {
    format!(
        "{} ({}): message delivered to this session",
        display_name(name, provenance.agent_id.as_deref().unwrap_or("")),
        provenance.trigger.as_deref().unwrap_or("trigger")
    )
}
