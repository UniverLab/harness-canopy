//! The TUI's shared control connection and the in-process test hub.
//!
//! Split out of `client::mod` for the file-size gate: the production hub
//! client and the test hub share this so `with_active` needs no setup edits
//! in spawn-based tests.

use std::sync::{Arc, Mutex, OnceLock};

#[cfg(test)]
use std::sync::mpsc;

#[cfg(test)]
use crate::sessions::{ws::SessionsState, SessionHub};

use super::HubClient;

/// The TUI's shared control connection plus the endpoint that session
/// streams dial. Spawns go through this so no call site changes.
pub struct ActiveHub {
    pub url: String,
    pub token: String,
    pub client: HubClient,
    /// The hub this connection talks to: production sets it at connect, the
    /// in-process test hub shares its own, and the spawn helpers read a fast
    /// failure's verdict off its database row.
    pub db: Arc<crate::db::Database>,
}

fn active_cell() -> &'static Mutex<Option<ActiveHub>> {
    static ACTIVE: OnceLock<Mutex<Option<ActiveHub>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(None))
}

/// Install the shared control connection (replacing any previous one).
pub fn set_active(url: String, token: String, client: HubClient, db: Arc<crate::db::Database>) {
    if let Ok(mut active) = active_cell().lock() {
        *active = Some(ActiveHub {
            url,
            token,
            client,
            db,
        });
    }
}

/// Run `f` on the shared control connection. In tests the shared
/// connection self-installs against the in-process hub, so existing
/// spawn-based tests need no setup edits.
pub fn with_active<R>(f: impl FnOnce(&ActiveHub) -> R) -> Option<R> {
    #[cfg(test)]
    ensure_test_active();
    let active = active_cell().lock().ok()?;
    let active = active.as_ref()?;
    Some(f(active))
}

/// The in-process hub that tests spawn against: its endpoint plus the hub
/// itself for assertions. One per test process.
#[cfg(test)]
pub struct TestHub {
    pub url: String,
    pub token: String,
    pub hub: SessionHub,
}

/// Serve the hub on 127.0.0.1 with an ephemeral port. The server thread
/// runs until the test process exits.
#[cfg(test)]
pub fn test_client() -> &'static TestHub {
    static TEST_HUB: OnceLock<TestHub> = OnceLock::new();
    TEST_HUB.get_or_init(|| {
        let dir = Box::leak(Box::new(tempfile::tempdir().expect("test hub dir")));
        let db = Arc::new(
            crate::db::Database::new(&dir.path().join("test.db")).expect("test hub database"),
        );
        let hub = SessionHub::with_broadcast_capacity(db, "test-host".to_string(), 256);
        let token = "test-token".to_string();
        let state = SessionsState::for_test(hub.clone(), token.clone());
        let (addr_tx, addr_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("test hub runtime");
            rt.block_on(async move {
                let app = axum::Router::new()
                    .route(
                        "/sessions/ws",
                        axum::routing::get(crate::sessions::ws::sessions_ws_handler),
                    )
                    .with_state(state);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind test hub");
                let addr = listener.local_addr().expect("test hub addr");
                addr_tx.send(addr).expect("test hub addr channel");
                axum::serve(listener, app).await.expect("serve test hub");
            });
        });
        let addr: std::net::SocketAddr = addr_rx.recv().expect("test hub address");
        TestHub {
            url: format!("ws://{addr}/sessions/ws"),
            token,
            hub,
        }
    })
}

/// Install the in-process hub as the shared connection, once.
#[cfg(test)]
fn ensure_test_active() {
    let needs = active_cell()
        .lock()
        .map(|active| active.is_none())
        .unwrap_or(false);
    if !needs {
        return;
    }
    let test = test_client();
    if let Ok(client) = HubClient::connect(&test.url, &test.token) {
        set_active(
            test.url.clone(),
            test.token.clone(),
            client,
            Arc::clone(test.hub.db()),
        );
    }
}
