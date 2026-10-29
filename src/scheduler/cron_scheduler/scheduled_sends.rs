//! Delivering due scheduled sends from the daemon.
//!
//! Moved here from `tui::app::data`: with the hub owning every PTY, a schedule
//! fires on the daemon's scheduler tick whether or not a TUI is attached. A
//! send whose target session is gone keeps the prompt recoverable and says so,
//! exactly as `preserve_dead_scheduled_send` did in the TUI.

use crate::application::ports::AgentRepository;
use crate::db::scheduled_sends::{is_target_alive, ScheduledSend, ScheduledSendProvenance};
use crate::db::Database;
use crate::domain::notification::{send_notification, NotificationLevel};
use crate::sessions::{SessionHub, SessionKind, SessionStatus};

/// Deliver every send whose `fire_at` has passed: to its live target session,
/// or — when that session is gone — to the failed-sends table plus a desktop
/// notification. Either way the schedule is consumed once.
pub fn deliver_due_scheduled_sends(db: &Database, hub: &SessionHub) {
    let now = chrono::Utc::now();
    let due = match db.list_due_scheduled_sends(now) {
        Ok(due) => due,
        Err(e) => {
            tracing::warn!("Failed to list due scheduled sends: {e}");
            return;
        }
    };
    if due.is_empty() {
        return;
    }

    let live_session_ids = live_agent_ids(hub);
    for send in &due {
        if is_target_alive(&send.target_session_id, &live_session_ids) {
            deliver_live_scheduled_send(db, hub, send);
        } else {
            preserve_dead_scheduled_send(db, send, now);
        }
        if let Err(e) = db.delete_scheduled_send(&send.id) {
            tracing::warn!("Failed to delete scheduled send '{}': {e}", send.id);
        }
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

fn deliver_live_scheduled_send(db: &Database, hub: &SessionHub, send: &ScheduledSend) {
    let Some(session) = hub.get(&send.target_session_id) else {
        return;
    };
    if let Err(e) = session.prompt(&send.prompt) {
        tracing::warn!("Scheduled send '{}' delivery failed: {e}", send.id);
        send_notification(
            "Scheduled send failed",
            &format!("Could not deliver prompt: {e}"),
            NotificationLevel::Error,
        );
        return;
    }
    tracing::info!(
        "Scheduled send '{}' delivered to session '{}'",
        send.id,
        send.target_session_id
    );
    notify_hook_delivery(db, send);
}

fn notify_hook_delivery(db: &Database, send: &ScheduledSend) -> bool {
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
fn hook_graph_name(db: &Database, provenance: &ScheduledSendProvenance) -> Option<String> {
    db.get_graph(&provenance.graph_id)
        .ok()
        .flatten()
        .map(|g| g.name)
}

/// Name of the agent behind an agent-trigger delivery; `None` once it is
/// gone. An agent's name is its id, so the id is what the user sees.
fn trigger_agent_name(db: &Database, provenance: &ScheduledSendProvenance) -> Option<String> {
    provenance
        .agent_id
        .as_deref()
        .and_then(|id| db.get_agent(id).ok().flatten())
        .map(|a| a.id)
}

/// Keep the prompt of a send whose target is gone recoverable, and tell the
/// operator it was not delivered.
fn preserve_dead_scheduled_send(
    db: &Database,
    send: &ScheduledSend,
    now: chrono::DateTime<chrono::Utc>,
) {
    tracing::warn!(
        "Scheduled send '{}': target session '{}' not found; prompt preserved for recall",
        send.id,
        send.target_session_id
    );
    if let Err(e) = db.insert_failed_scheduled_send(
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
    send: &ScheduledSend,
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

fn hook_delivery_body(provenance: &ScheduledSendProvenance, name: Option<&str>) -> String {
    format!(
        "{} · {}: message delivered to this session",
        display_name(name, &provenance.graph_id),
        provenance.event
    )
}

fn agent_trigger_delivery_body(provenance: &ScheduledSendProvenance, name: Option<&str>) -> String {
    format!(
        "{} ({}): message delivered to this session",
        display_name(name, provenance.agent_id.as_deref().unwrap_or("")),
        provenance.trigger.as_deref().unwrap_or("trigger")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use crate::db::scheduled_sends::ScheduledSendProvenance;
    use crate::sessions::{SessionHub, SpawnRequest};
    use tempfile::TempDir;

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

        db.insert_scheduled_send(
            "ss-live",
            "deliver me from the daemon",
            &session.id,
            Some("/tmp"),
            chrono::Utc::now() - chrono::Duration::minutes(1),
            None,
            None,
        )
        .expect("insert scheduled send");

        deliver_due_scheduled_sends(&db, &hub);

        wait_for_output(&session, "deliver me from the daemon");
        assert!(
            db.list_due_scheduled_sends(chrono::Utc::now())
                .expect("list due")
                .is_empty(),
            "a delivered send is consumed"
        );
        assert!(
            db.list_failed_scheduled_sends_for_workdir("/tmp")
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

        db.insert_scheduled_send(
            "ss-dead",
            "please recover me",
            "session-that-no-longer-exists",
            Some(workdir),
            chrono::Utc::now() - chrono::Duration::minutes(1),
            None,
            None,
        )
        .expect("insert scheduled send");

        deliver_due_scheduled_sends(&db, &hub);

        assert!(
            db.list_due_scheduled_sends(chrono::Utc::now())
                .expect("list due")
                .is_empty(),
            "the send fired once, whether delivered or preserved"
        );
        let failed = db
            .list_failed_scheduled_sends_for_workdir(workdir)
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

        db.insert_scheduled_send(
            "ss-exited",
            "nobody is listening",
            "exited-session",
            Some(workdir),
            chrono::Utc::now() - chrono::Duration::minutes(1),
            None,
            None,
        )
        .expect("insert scheduled send");

        deliver_due_scheduled_sends(&db, &hub);

        assert_eq!(
            db.list_failed_scheduled_sends_for_workdir(workdir)
                .expect("list failed")
                .len(),
            1
        );
    }

    #[test]
    fn next_scheduled_send_at_is_the_earliest_fire_time() {
        let (db, _dir) = test_db();
        assert_eq!(db.next_scheduled_send_at().expect("query"), None);

        let earlier = chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + 300, 0)
            .expect("whole-second timestamp");
        let later = chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + 1800, 0)
            .expect("whole-second timestamp");
        db.insert_scheduled_send("ss-later", "b", "sess", None, later, None, None)
            .unwrap();
        db.insert_scheduled_send("ss-earlier", "a", "sess", None, earlier, None, None)
            .unwrap();

        assert_eq!(db.next_scheduled_send_at().expect("query"), Some(earlier));
    }

    // ── notification text (moved from `tui::app::data`) ─────────────

    #[test]
    fn hook_delivery_body_names_the_graph() {
        let prov = ScheduledSendProvenance::hook("g-canopy", "on_completed");
        assert_eq!(
            hook_delivery_body(&prov, Some("canopy-next")),
            "canopy-next · on_completed: message delivered to this session"
        );
    }

    #[test]
    fn hook_delivery_body_falls_back_to_short_id() {
        let prov =
            ScheduledSendProvenance::hook("969ee102-0138-4054-95fd-2946ef237431", "on_completed");
        assert_eq!(
            hook_delivery_body(&prov, None),
            "969ee102 · on_completed: message delivered to this session"
        );
    }

    #[test]
    fn agent_trigger_delivery_body_names_the_agent() {
        let prov = ScheduledSendProvenance::agent_trigger("nudger", "cron");
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
        })
        .expect("insert graph");

        let prov = ScheduledSendProvenance::hook("g-canopy-next", "on_completed");
        let name = hook_graph_name(&db, &prov);
        assert_eq!(
            hook_delivery_body(&prov, name.as_deref()),
            "canopy-next · on_completed: message delivered to this session"
        );

        let unknown =
            ScheduledSendProvenance::hook("969ee102-0138-4054-95fd-2946ef237431", "on_completed");
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

        let prov = ScheduledSendProvenance::agent_trigger("nudger", "cron");
        let name = trigger_agent_name(&db, &prov);
        assert_eq!(
            agent_trigger_delivery_body(&prov, name.as_deref()),
            "nudger (cron): message delivered to this session"
        );

        let unknown =
            ScheduledSendProvenance::agent_trigger("969ee102-0138-4054-95fd-2946ef237431", "cron");
        assert_eq!(trigger_agent_name(&db, &unknown), None);
    }

    #[test]
    fn notify_hook_delivery_is_true_for_hook_and_agent_trigger_provenance() {
        let (db, _dir) = test_db();
        let send = |provenance: Option<ScheduledSendProvenance>| ScheduledSend {
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
            &send(Some(ScheduledSendProvenance::hook("graph-1", "on_failed")))
        ));
        assert!(notify_hook_delivery(
            &db,
            &send(Some(ScheduledSendProvenance::agent_trigger(
                "nudger", "cron"
            )))
        ));
        let other = ScheduledSendProvenance {
            kind: "other".into(),
            graph_id: "graph-1".into(),
            event: "on_failed".into(),
            session_id: None,
            agent_id: None,
            trigger: None,
            target_session_name: None,
        };
        assert!(!notify_hook_delivery(&db, &send(Some(other))));
        assert!(!notify_hook_delivery(&db, &send(None)));
    }
}
