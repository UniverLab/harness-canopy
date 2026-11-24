//! Bounded MCP session management (audit finding F-16).
//!
//! Every `initialize` on `/mcp` allocates a session (~72 KB) that used to
//! live forever: the daemon built its [`LocalSessionManager`] with
//! `keep_alive = None`, disabling rmcp's idle timeout, and nothing capped
//! the count. [`CappedSessionManager`] wraps the inner manager, keeps a
//! session id to last-used [`Instant`] map, and evicts the least recently
//! used session once [`MCP_MAX_LIVE_SESSIONS`] are live. The map is LRU
//! bookkeeping only — liveness stays with the inner manager, and entries
//! for sessions it has already dropped are pruned when observed.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::common::server_side_http::SessionId;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManagerError;
use rmcp::transport::streamable_http_server::session::local::LocalSessionWorker;
use rmcp::transport::streamable_http_server::session::RestoreOutcome;
use rmcp::transport::streamable_http_server::session::ServerSseMessage;
use rmcp::transport::streamable_http_server::session::SessionManager;
use rmcp::transport::WorkerTransport;

/// Idle time after which rmcp itself ends a session worker
/// (`WorkerQuitReason::IdleTimeout`); no reaper task is needed.
pub const MCP_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Maximum live MCP sessions. `canopy bridge` keeps one session per process
/// and re-initializes on the eviction `404`, so this never breaks it.
pub const MCP_MAX_LIVE_SESSIONS: usize = 64;

/// [`SessionManager`] that delegates everything to an inner
/// [`LocalSessionManager`] and evicts the least recently used session when
/// the cap is reached.
pub struct CappedSessionManager {
    inner: LocalSessionManager,
    last_used: tokio::sync::Mutex<HashMap<SessionId, Instant>>,
}

impl CappedSessionManager {
    /// Wrap an already-configured inner manager (notably its `keep_alive`).
    pub fn new(inner: LocalSessionManager) -> Self {
        Self {
            inner,
            last_used: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Record activity for a session known to be live.
    async fn touch(&self, id: &SessionId) {
        self.last_used
            .lock()
            .await
            .insert(id.clone(), Instant::now());
    }

    /// Test-only read path: how many sessions are currently tracked.
    /// The daemon CLI never reports this (it reads pid file, port and DB).
    #[cfg(test)]
    pub async fn live_count(&self) -> usize {
        self.last_used.lock().await.len()
    }
}

impl SessionManager for CappedSessionManager {
    type Error = LocalSessionManagerError;
    type Transport = WorkerTransport<LocalSessionWorker>;

    async fn create_session(&self) -> Result<(SessionId, Self::Transport), Self::Error> {
        let mut used = self.last_used.lock().await;
        if used.len() >= MCP_MAX_LIVE_SESSIONS {
            let lru = used
                .iter()
                .min_by_key(|entry| *entry.1)
                .map(|entry| entry.0.clone());
            if let Some(lru) = lru {
                // Inner close is idempotent Ok on an already-gone id; either
                // way the slot is freed and creation proceeds.
                let _ = self.inner.close_session(&lru).await;
                used.remove(&lru);
                tracing::info!("mcp session evicted: cap (live={})", MCP_MAX_LIVE_SESSIONS);
            }
        }
        let (id, transport) = self.inner.create_session().await?;
        used.insert(id.clone(), Instant::now());
        Ok((id, transport))
    }

    async fn initialize_session(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<ServerJsonRpcMessage, Self::Error> {
        let response = self.inner.initialize_session(id, message).await?;
        self.touch(id).await;
        Ok(response)
    }

    async fn has_session(&self, id: &SessionId) -> Result<bool, Self::Error> {
        let present = self.inner.has_session(id).await?;
        if present {
            self.touch(id).await;
        } else {
            // The inner manager already dropped it (e.g. idle timeout):
            // prune so the slot frees without an eviction.
            self.last_used.lock().await.remove(id);
        }
        Ok(present)
    }

    async fn close_session(&self, id: &SessionId) -> Result<(), Self::Error> {
        self.last_used.lock().await.remove(id);
        self.inner.close_session(id).await
    }

    async fn create_stream(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<impl futures::Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error>
    {
        let stream = self.inner.create_stream(id, message).await?;
        self.touch(id).await;
        Ok(stream)
    }

    async fn accept_message(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<(), Self::Error> {
        self.inner.accept_message(id, message).await?;
        self.touch(id).await;
        Ok(())
    }

    async fn create_standalone_stream(
        &self,
        id: &SessionId,
    ) -> Result<impl futures::Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error>
    {
        let stream = self.inner.create_standalone_stream(id).await?;
        self.touch(id).await;
        Ok(stream)
    }

    async fn resume(
        &self,
        id: &SessionId,
        last_event_id: String,
    ) -> Result<impl futures::Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error>
    {
        let stream = self.inner.resume(id, last_event_id).await?;
        self.touch(id).await;
        Ok(stream)
    }

    async fn restore_session(
        &self,
        id: SessionId,
    ) -> Result<RestoreOutcome<Self::Transport>, Self::Error> {
        let outcome = self.inner.restore_session(id.clone()).await?;
        match &outcome {
            // Record restores too or they bypass the cap.
            RestoreOutcome::Restored(_) | RestoreOutcome::AlreadyPresent => {
                self.touch(&id).await;
            }
            RestoreOutcome::NotSupported | _ => {}
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create one session after a short beat so consecutive sessions get
    /// strictly increasing last-used timestamps (LRU ties would make
    /// eviction order nondeterministic).
    async fn sequential_create(mgr: &CappedSessionManager) -> SessionId {
        tokio::time::sleep(Duration::from_millis(1)).await;
        let (id, _transport) = mgr.create_session().await.unwrap();
        id
    }

    async fn fill_to_cap(mgr: &CappedSessionManager) -> Vec<SessionId> {
        let mut ids = Vec::with_capacity(MCP_MAX_LIVE_SESSIONS);
        for _ in 0..MCP_MAX_LIVE_SESSIONS {
            ids.push(sequential_create(mgr).await);
        }
        ids
    }

    #[tokio::test]
    async fn sixty_fifth_create_evicts_lru_and_holds_cap() {
        let mgr = CappedSessionManager::new(LocalSessionManager::default());
        let ids = fill_to_cap(&mgr).await;
        assert_eq!(mgr.live_count().await, MCP_MAX_LIVE_SESSIONS);
        sequential_create(&mgr).await;
        assert_eq!(mgr.live_count().await, MCP_MAX_LIVE_SESSIONS);
        assert!(!mgr.has_session(&ids[0]).await.unwrap());
    }

    #[tokio::test]
    async fn touched_session_survives_eviction() {
        let mgr = CappedSessionManager::new(LocalSessionManager::default());
        let ids = fill_to_cap(&mgr).await;
        assert!(mgr.has_session(&ids[0]).await.unwrap());
        sequential_create(&mgr).await;
        assert_eq!(mgr.live_count().await, MCP_MAX_LIVE_SESSIONS);
        assert!(mgr.has_session(&ids[0]).await.unwrap());
        assert!(!mgr.has_session(&ids[1]).await.unwrap());
    }

    #[tokio::test]
    async fn closed_session_frees_slot_without_eviction() {
        let mgr = CappedSessionManager::new(LocalSessionManager::default());
        let ids = fill_to_cap(&mgr).await;
        mgr.close_session(&ids[0]).await.unwrap();
        assert_eq!(mgr.live_count().await, MCP_MAX_LIVE_SESSIONS - 1);
        sequential_create(&mgr).await;
        assert_eq!(mgr.live_count().await, MCP_MAX_LIVE_SESSIONS);
        for id in &ids[1..] {
            assert!(mgr.has_session(id).await.unwrap());
        }
    }

    #[test]
    fn idle_timeout_is_thirty_minutes() {
        assert_eq!(MCP_SESSION_IDLE_TIMEOUT, Duration::from_secs(30 * 60));
    }
}
