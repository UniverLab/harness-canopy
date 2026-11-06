//! The owner gate: `spawn`, `kill`, `graphs`, `graph_watch`, `catalog` and
//! `compose` answer only the host owner's own devices.
//!
//! A guest — full grant or narrow — answers `forbidden` naming the frame for
//! each of them, and the refusal has no side effect: nothing spawns, nothing
//! is killed, nothing is read or watched. `kill` keeps its grant check first,
//! so a session outside the grant still answers `not_found`. Each test breaks
//! when the owner check is skipped.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::Grant;
use crate::db::Database;
use crate::remote::bridge::RemoteBridge;
use crate::sessions::outbound::ConnRx;
use crate::sessions::protocol::{ERR_FORBIDDEN, ERR_NOT_FOUND};
use crate::sessions::ws::{OutTx, SessionsState};
use crate::sessions::{HubSession, SessionHub, SessionKind, SpawnRequest};

/// A spawned session, killed when the guard drops so no test leaves a child.
struct KillGuard {
    hub: SessionHub,
    id: String,
}

impl Drop for KillGuard {
    fn drop(&mut self) {
        let _ = self.hub.kill(&self.id);
    }
}

fn spawn_named(hub: &SessionHub, name: &str, workdir: &Path) -> (Arc<HubSession>, KillGuard) {
    let request = SpawnRequest {
        kind: SessionKind::Agent,
        cli: "cat".to_string(),
        model: None,
        workdir: workdir.to_string_lossy().to_string(),
        name: Some(name.to_string()),
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
    };
    let session = hub.spawn(&request).expect("test spawn");
    assert_eq!(session.name, name);
    let guard = KillGuard {
        hub: hub.clone(),
        id: session.id.clone(),
    };
    (session, guard)
}

/// A hub holding `alpha` (granted by the narrow grant) and `beta` (not).
struct Fixture {
    state: Arc<SessionsState>,
    alpha: Arc<HubSession>,
    beta: Arc<HubSession>,
    _kills: Vec<KillGuard>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Arc::new(Database::new(&dir.path().join("test.db")).expect("test database"));
    let hub = SessionHub::with_broadcast_capacity(db, "test-host".to_string(), 256);
    let workdir = tempfile::tempdir().expect("workdir");
    let (alpha, kill_alpha) = spawn_named(&hub, "alpha", workdir.path());
    let (beta, kill_beta) = spawn_named(&hub, "beta", workdir.path());
    Fixture {
        state: SessionsState::for_test(hub, "token".to_string()),
        alpha,
        beta,
        _kills: vec![kill_alpha, kill_beta],
        _dir: dir,
    }
}

fn narrow() -> Grant {
    Grant::Sessions(HashSet::from(["alpha".to_string()]))
}

async fn ask(bridge: &mut RemoteBridge, out: &OutTx, rx: &mut ConnRx, frame: &Value) -> Value {
    bridge.handle_text(&frame.to_string(), out).await;
    let reply = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("an answer arrives")
        .expect("the channel stays open");
    serde_json::from_str(&reply).expect("frames are JSON")
}

/// Assert one frame answers `forbidden`, echoing its `req` and naming it.
async fn assert_forbidden(
    bridge: &mut RemoteBridge,
    out: &OutTx,
    rx: &mut ConnRx,
    frame: &Value,
    name: &str,
) {
    let answer = ask(bridge, out, rx, frame).await;
    assert_eq!(answer["t"], "error", "{name}: got {answer}");
    assert_eq!(answer["code"], ERR_FORBIDDEN, "{name}: got {answer}");
    assert_eq!(answer["req"], frame["req"], "{name}: must echo the req");
    assert_eq!(
        answer["message"],
        format!("{name} is only available to the host owner's devices"),
        "{name}: must name the frame"
    );
}

/// The full refusal matrix for one guest grant.
async fn assert_guest_refusals(grant: Grant) {
    let fx = fixture();
    let sessions_before = fx.state.hub.list().len();
    let (out, mut rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&fx.state), grant, true);
    assert!(
        !bridge.scope.is_owner(),
        "a with_grant peer is never the owner"
    );

    let cases = [
        (
            json!({"t": "spawn", "req": "s1", "kind": "terminal", "workdir": "/tmp"}),
            "spawn",
        ),
        (json!({"t": "kill", "req": "k1", "id": fx.alpha.id}), "kill"),
        (json!({"t": "graphs", "req": "g1"}), "graphs"),
        (
            json!({"t": "graph_watch", "req": "w1", "id": "g-run"}),
            "graph_watch",
        ),
        (json!({"t": "catalog", "req": "c1"}), "catalog"),
        (
            json!({
                "t": "compose", "req": "m1",
                "draft": {"sections": [{"title": "goal", "content": "g"}]},
            }),
            "compose",
        ),
    ];
    for (frame, name) in &cases {
        assert_forbidden(&mut bridge, &out, &mut rx, frame, name).await;
    }

    // The refused spawn created no session, and the refused kill left the
    // granted session alive.
    assert_eq!(
        fx.state.hub.list().len(),
        sessions_before,
        "the refused spawn must not spawn"
    );
    assert!(
        fx.state.hub.get(&fx.alpha.id).is_some(),
        "the refused kill must leave the session alive"
    );

    // `graph_unwatch` stays a silent no-op for a guest: no error, no answer.
    bridge
        .handle_text(
            &json!({"t": "graph_unwatch", "id": "g-run"}).to_string(),
            &out,
        )
        .await;
    assert!(rx.try_recv().is_err(), "graph_unwatch must answer nothing");
}

#[tokio::test]
async fn full_grant_guest_is_refused_every_owner_only_frame() {
    assert_guest_refusals(Grant::All).await;
}

#[tokio::test]
async fn narrow_grant_guest_is_refused_every_owner_only_frame() {
    assert_guest_refusals(narrow()).await;
}

#[tokio::test]
async fn narrow_grant_kill_outside_the_grant_stays_not_found() {
    let fx = fixture();
    let (out, mut rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&fx.state), narrow(), true);

    let answer = ask(
        &mut bridge,
        &out,
        &mut rx,
        &json!({"t": "kill", "req": "k-out", "id": fx.beta.id}),
    )
    .await;
    assert_eq!(answer["t"], "error", "got {answer}");
    assert_eq!(
        answer["code"], ERR_NOT_FOUND,
        "outside the grant the guest must not learn the session exists"
    );
    assert!(
        fx.state.hub.get(&fx.beta.id).is_some(),
        "the out-of-grant kill must leave the session alive"
    );
}

#[tokio::test]
async fn owner_bridge_still_spawns_and_kills() {
    let fx = fixture();
    let (out, mut rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&fx.state), Grant::All, true);
    bridge.set_owner(true);

    let workdir = tempfile::tempdir().expect("workdir");
    let spawned = ask(
        &mut bridge,
        &out,
        &mut rx,
        &json!({
            "t": "spawn", "req": "s1", "kind": "terminal",
            "workdir": workdir.path().to_string_lossy(),
        }),
    )
    .await;
    assert_eq!(spawned["t"], "spawned", "got {spawned}");
    assert_eq!(spawned["req"], "s1");
    let id = spawned["id"]
        .as_str()
        .expect("spawned session id")
        .to_string();
    let _guard = KillGuard {
        hub: fx.state.hub.clone(),
        id: id.clone(),
    };

    let killed = ask(
        &mut bridge,
        &out,
        &mut rx,
        &json!({"t": "kill", "req": "k1", "id": id}),
    )
    .await;
    assert_eq!(killed["t"], "ok", "got {killed}");
    assert_eq!(killed["req"], "k1");
}
