//! `SyncManager` — in-memory fan-out plus advisory sync context.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};

use crate::db::Database;
use crate::domain::sync::{
    summarize_sync_context, IntentPayload, MessageKind, MissionImpact, StatusPayload,
    SyncContextSnapshot, SyncMessage, WorkspaceStatus,
};

const BROADCAST_CAPACITY: usize = 64;
const CONTEXT_WINDOW: usize = 100;

struct WorkdirState {
    tx: broadcast::Sender<SyncMessage>,
}

pub struct SyncManager {
    db: Arc<Database>,
    state: Mutex<HashMap<String, WorkdirState>>,
}

impl SyncManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            state: Mutex::new(HashMap::new()),
        }
    }

    #[allow(dead_code)]
    pub async fn subscribe(&self, workdir: &str) -> broadcast::Receiver<SyncMessage> {
        self.ensure_sender(workdir).await.subscribe()
    }

    pub async fn declare_intent(
        &self,
        workdir: &str,
        agent_id: &str,
        client_name: Option<&str>,
        mission: &str,
        impact: MissionImpact,
        description: &str,
    ) -> anyhow::Result<SyncMessage> {
        let agent_name = self.build_display_name(workdir, agent_id, client_name)?;
        let payload = serde_json::to_string(&IntentPayload {
            mission: mission.to_owned(),
            impact,
            description: description.to_owned(),
        })?;

        let message = self
            .publish(
                workdir,
                agent_id,
                &agent_name,
                MessageKind::Intent,
                &format!("{agent_name}: {mission}"),
                Some(&payload),
            )
            .await?;
        self.upsert_sync_operational_session(crate::db::intelligence::OperationalSessionInput {
            id: Some(format!("sync:{workdir}:{agent_id}")),
            title: mission.to_owned(),
            body: description.to_owned(),
            metadata: Some(serde_json::json!({
                "source": "sync",
                "workdir": workdir,
                "agent_id": agent_id,
                "agent_name": agent_name,
                "kind": "intent",
                "payload": payload,
            })),
            project_hash: None,
            session_id: Some(format!("sync:{workdir}:{agent_id}")),
        })?;
        Ok(message)
    }

    pub async fn report_status(
        &self,
        workdir: &str,
        agent_id: &str,
        client_name: Option<&str>,
        status: WorkspaceStatus,
        message: &str,
    ) -> anyhow::Result<SyncMessage> {
        let agent_name = self.build_display_name(workdir, agent_id, client_name)?;
        let payload = serde_json::to_string(&StatusPayload {
            status,
            message: message.to_owned(),
        })?;

        let sync_message = self
            .publish(
                workdir,
                agent_id,
                &agent_name,
                MessageKind::Status,
                message,
                Some(&payload),
            )
            .await?;
        self.upsert_sync_operational_session(crate::db::intelligence::OperationalSessionInput {
            id: Some(format!("sync:{workdir}:{agent_id}")),
            title: message.to_owned(),
            body: message.to_owned(),
            metadata: Some(serde_json::json!({
                "source": "sync",
                "workdir": workdir,
                "agent_id": agent_id,
                "agent_name": agent_name,
                "kind": "status",
                "payload": payload,
            })),
            project_hash: None,
            session_id: Some(format!("sync:{workdir}:{agent_id}")),
        })?;
        Ok(sync_message)
    }

    pub async fn broadcast(
        &self,
        workdir: &str,
        agent_id: &str,
        client_name: Option<&str>,
        kind: MessageKind,
        message: &str,
        payload: Option<&str>,
    ) -> anyhow::Result<SyncMessage> {
        let agent_name = self.build_display_name(workdir, agent_id, client_name)?;
        self.publish(workdir, agent_id, &agent_name, kind, message, payload)
            .await
    }

    pub fn get_context(
        &self,
        workdir: &str,
        chatter_limit: usize,
    ) -> anyhow::Result<SyncContextSnapshot> {
        let recent_entries = self.db.list_activity_log_entries(workdir, CONTEXT_WINDOW)?;
        let mut recent_messages: Vec<SyncMessage> =
            recent_entries.into_iter().map(SyncMessage::from).collect();
        for message in &mut recent_messages {
            message.agent_name = self
                .db
                .resolve_sync_actor_display_name(workdir, &message.agent_id)
                .unwrap_or_else(|_| message.agent_name.clone());
        }
        let active_agent_ids: HashSet<String> = self
            .db
            .list_active_sync_agent_ids(workdir)?
            .into_iter()
            .collect();

        Ok(summarize_sync_context(
            &recent_messages,
            &active_agent_ids,
            chatter_limit,
        ))
    }

    async fn publish(
        &self,
        workdir: &str,
        agent_id: &str,
        agent_name: &str,
        kind: MessageKind,
        message: &str,
        payload: Option<&str>,
    ) -> anyhow::Result<SyncMessage> {
        let sync_message = self
            .db
            .insert_sync_message(workdir, agent_id, agent_name, kind, message, payload)?;
        // Bitácora: mirror every published event into the durable activity
        // log, off the hot path. Spawned rather than awaited so a loop
        // node's status report isn't slowed by its own log line (NFR:
        // writing must not block the producer); best-effort — a full or
        // locked activity table must never break the live broadcast path.
        let db = Arc::clone(&self.db);
        let workdir_owned = workdir.to_owned();
        let agent_id_owned = agent_id.to_owned();
        let kind_str = kind.as_str();
        let message_owned = message.to_owned();
        let payload_owned = payload.map(str::to_owned);
        tokio::spawn(async move {
            let _ = db.insert_activity_log_entry(
                &workdir_owned,
                "sync",
                Some(&agent_id_owned),
                kind_str,
                &message_owned,
                payload_owned.as_deref(),
            );
        });
        let sender = self.ensure_sender(workdir).await;
        let _ = sender.send(sync_message.clone());
        Ok(sync_message)
    }

    /// Resolves the TUI session name from the DB and appends the client harness name when known.
    /// Result: "laetiporus · copilot" or just "laetiporus" if client_name is unavailable.
    /// The DB name already includes "· cli" for interactive sessions; the header is only appended
    /// when its value differs (avoids duplicating "cortinarius · copilot · copilot").
    fn build_display_name(
        &self,
        workdir: &str,
        agent_id: &str,
        client_name: Option<&str>,
    ) -> anyhow::Result<String> {
        let session_name = self.db.resolve_sync_actor_display_name(workdir, agent_id)?;
        Ok(match client_name {
            Some(c) if !c.is_empty() && !session_name.contains(c) => {
                format!("{session_name} · {c}")
            }
            _ => session_name,
        })
    }

    fn upsert_sync_operational_session(
        &self,
        node: crate::db::intelligence::OperationalSessionInput,
    ) -> anyhow::Result<()> {
        self.db.upsert_operational_session(node)?;
        Ok(())
    }

    async fn ensure_sender(&self, workdir: &str) -> broadcast::Sender<SyncMessage> {
        let mut state = self.state.lock().await;
        state
            .entry(workdir.to_owned())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
                WorkdirState { tx }
            })
            .tx
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manager() -> (SyncManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("sync_manager_test.db");
        let db = Database::new(&db_path).expect("create test db");
        (SyncManager::new(Arc::new(db)), dir)
    }

    /// A bridge session that resolved a real identity (mirrors what
    /// `daemon::bridge::run_bridge` registers for a known `agent_id`) must
    /// have its actual session name land in `sync_messages.agent_name`.
    #[tokio::test]
    async fn report_status_uses_real_session_name_not_standalone() {
        let (manager, _dir) = test_manager();
        let workdir = "/tmp/known-workdir";
        let agent_id = "sess-boletus";

        manager
            .db
            .insert_interactive_session(
                agent_id,
                "boletus",
                "claude",
                workdir,
                None,
                Some(4242),
                "interactive",
                None,
            )
            .expect("seed known session");

        let message = manager
            .report_status(
                workdir,
                agent_id,
                None,
                WorkspaceStatus::Stable,
                "all green",
            )
            .await
            .expect("report_status");

        assert_eq!(message.agent_name, "boletus · claude");
        assert_ne!(message.agent_name, "standalone");
    }

    /// A bridge call for a NAMED session — through either write path — must
    /// reproduce the exact same display shape the TUI sidebar shows for that
    /// session ("boletus · claude"), the shape `daemon::bridge::forward_request`
    /// now preserves by no longer sending its own `x-canopy-client-name:
    /// bridge` header (see `forward_request_omits_client_name_header` in
    /// `daemon::bridge`, which guards the actual injection site). A stray
    /// non-empty `client_name` here would still get appended — this only
    /// pins the None case that real bridge traffic exercises today.
    #[tokio::test]
    async fn report_status_and_broadcast_from_named_session_match_tui_display_name() {
        let (manager, _dir) = test_manager();
        let workdir = "/tmp/named-bridge-workdir";
        let agent_id = "sess-boletus-2";

        manager
            .db
            .insert_interactive_session(
                agent_id,
                "boletus",
                "claude",
                workdir,
                None,
                Some(4242),
                "interactive",
                None,
            )
            .expect("seed known session");

        let tui_display_name = manager
            .db
            .resolve_sync_actor_display_name(workdir, agent_id)
            .expect("resolve display name");
        assert_eq!(tui_display_name, "boletus · claude");

        let status_message = manager
            .report_status(
                workdir,
                agent_id,
                None,
                WorkspaceStatus::Stable,
                "all green",
            )
            .await
            .expect("report_status");
        assert_eq!(status_message.agent_name, tui_display_name);

        let broadcast_message = manager
            .broadcast(workdir, agent_id, None, MessageKind::Info, "hello", None)
            .await
            .expect("broadcast");
        assert_eq!(broadcast_message.agent_name, tui_display_name);
    }

    /// Every broadcast lands in BOTH `sync_messages` (live fan-out) and
    /// `activity_log` (durable bitácora) — one call, two rows, same content.
    #[tokio::test]
    async fn publish_writes_to_both_tables() {
        let (manager, _dir) = test_manager();
        let workdir = "/tmp/bitacora-dual-write";

        let message = manager
            .broadcast(
                workdir,
                "agent-x",
                None,
                MessageKind::Info,
                "dual write probe",
                None,
            )
            .await
            .expect("broadcast");

        let live = manager.db.list_sync_messages(workdir, 10).expect("live");
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].message, message.message);

        // The bitácora write is spawned off the hot path (NFR: must not
        // block the producer), so it can land shortly after `broadcast`
        // returns rather than being visible immediately.
        let stored = wait_for_activity_entry(&manager.db, workdir).await;
        assert_eq!(stored.message, "dual write probe");
        assert_eq!(stored.source, "sync");
        assert_eq!(stored.source_id.as_deref(), Some("agent-x"));
    }

    async fn wait_for_activity_entry(
        db: &Database,
        workdir: &str,
    ) -> crate::db::activity_log::ActivityLogEntry {
        for _ in 0..100 {
            let entries = db.list_activity_log_entries(workdir, 10).expect("bitacora");
            if let Some(entry) = entries.into_iter().next() {
                return entry;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("activity log entry for {workdir} did not appear in time");
    }

    /// A bridge with no resolvable identity (mirrors
    /// `daemon::bridge::register_standalone_session` after the fix) must
    /// still be distinguishable from a real session and must not collapse
    /// to the bare, collidable literal "standalone".
    #[tokio::test]
    async fn broadcast_from_unidentified_bridge_is_explicit_and_distinguishable() {
        let (manager, _dir) = test_manager();
        let workdir = "/tmp/unknown-workdir";
        let fallback_agent_id = format!("standalone-{}", uuid::Uuid::new_v4());

        // Mirrors `register_standalone_session`: the fallback agent_id is
        // reused as the session name so it stays unique per bridge instance.
        manager
            .db
            .insert_interactive_session(
                &fallback_agent_id,
                &fallback_agent_id,
                "bridge",
                workdir,
                Some("canopy bridge"),
                Some(4343),
                "bridge",
                None,
            )
            .expect("seed standalone session");

        let message = manager
            .broadcast(
                workdir,
                &fallback_agent_id,
                None,
                MessageKind::Info,
                "hello",
                None,
            )
            .await
            .expect("broadcast");

        assert_ne!(message.agent_name, "standalone");
        assert!(
            message.agent_name.starts_with("standalone-"),
            "expected an explicit, distinguishable fallback name, got {:?}",
            message.agent_name
        );
        assert_eq!(message.agent_name, format!("{fallback_agent_id} · bridge"));
    }
}
