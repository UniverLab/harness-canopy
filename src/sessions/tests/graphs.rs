//! Graph plate tests: `graphs` answers the filtered, ordered list with
//! real progress and node *names*; `graph_watch` answers the export
//! document and pushes `graph_state` only on a real diff; unknown ids,
//! the fifth watch and a closed socket are all refused or reaped.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::json;

use super::{
    connect_client, recv_json, send_json, serve_state, test_db, test_hub, try_recv_json, WsStream,
};
use crate::db::Database;
use crate::domain::graph_transfer::GraphExportDocument;
use crate::domain::graphs::{
    Graph, GraphNode, GraphNodeKind, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus,
    GraphStatus,
};
use crate::domain::queues::Queue;
use crate::sessions::graphs::{
    graph_frame, graph_state_frame, graphs_frame, GraphState, GraphSummary, MAX_SUMMARIES,
};
use crate::sessions::protocol::{parse_client_frame, parse_host_frame, ClientFrame, HostFrame};
use crate::sessions::ws::SessionsState;

// ── seeding ─────────────────────────────────────────────────────────────

fn graph_row(id: &str, status: GraphStatus, workdir: &str) -> Graph {
    Graph {
        id: id.to_string(),
        name: format!("graph-{id}"),
        description: None,
        workdir: workdir.to_string(),
        status,
        trigger: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: BTreeMap::new(),
        archived: false,
        paused_by_reconciliation: false,
        infra_node_id: None,
        allow_dirty_start: false,
        require_spec_type: false,
    }
}

fn spec_row(
    id: &str,
    graph_id: Option<&str>,
    name: &str,
    position: i64,
    status: GraphSpecStatus,
) -> GraphSpec {
    GraphSpec {
        id: id.to_string(),
        graph_id: graph_id.map(str::to_string),
        name: name.to_string(),
        description: None,
        position,
        parallelizable: false,
        status,
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
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    }
}

fn node_row(id: &str, spec_id: Option<&str>, graph_id: Option<&str>, name: &str) -> GraphNode {
    node_row_at(id, spec_id, graph_id, name, 0)
}

/// Same, at an explicit position — `(spec_id, position)` is unique in the
/// DB, so a second node under one spec needs its own slot.
fn node_row_at(
    id: &str,
    spec_id: Option<&str>,
    graph_id: Option<&str>,
    name: &str,
    position: i64,
) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: spec_id.map(str::to_string),
        graph_id: graph_id.map(str::to_string),
        name: name.to_string(),
        kind: GraphNodeKind::Agent,
        config: json!({"platform": "claude", "model": "sonnet"}),
        position,
        created_at: Utc::now(),
    }
}

fn run_row(
    graph_id: &str,
    spec_id: &str,
    node_id: &str,
    status: GraphRunStatus,
    started_at: DateTime<Utc>,
    iteration: i64,
) -> GraphNodeRun {
    let settled = !matches!(status, GraphRunStatus::Running | GraphRunStatus::Waiting);
    GraphNodeRun {
        id: format!(
            "{node_id}-{iteration}-{}",
            started_at.timestamp_nanos_opt().unwrap_or(0)
        ),
        graph_id: graph_id.to_string(),
        spec_id: spec_id.to_string(),
        node_id: node_id.to_string(),
        status,
        input: None,
        output: None,
        started_at,
        completed_at: settled.then_some(started_at),
        iteration,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    }
}

/// The running plate graph the list and watch tests share: a queue of two
/// completed specs plus a pending one whose node has one passing run, and
/// a top-level node for the plate document.
fn seed_plate_graph(db: &Database, now: DateTime<Utc>) {
    let mut running = graph_row("g-run", GraphStatus::Running, "/srv/work/plate-project");
    running.created_at = now - ChronoDuration::hours(2);
    running.started_at = Some(now - ChronoDuration::minutes(30));
    running.active_run_queue_id = Some("q-plate".to_string());
    db.insert_graph(&running).unwrap();

    db.insert_queue(&Queue {
        id: "q-plate".to_string(),
        name: "plate queue".to_string(),
        created_at: now,
    })
    .unwrap();
    let members = [
        (
            "s-1",
            "first spec",
            GraphSpecStatus::Completed,
            Some("FEAT-12"),
        ),
        ("s-2", "second spec", GraphSpecStatus::Completed, None),
        ("s-3", "third spec", GraphSpecStatus::Pending, None),
    ];
    for (position, (id, name, status, code)) in members.into_iter().enumerate() {
        let mut spec = spec_row(id, None, name, position as i64, status);
        spec.spec_code = code.map(str::to_string);
        db.insert_graph_spec(&spec).unwrap();
        db.append_queue_member("q-plate", id, None).unwrap();
    }
    db.insert_graph_node(&node_row("n-plate", Some("s-3"), None, "check-plate"))
        .unwrap();
    db.insert_graph_run(&run_row(
        "g-run",
        "s-3",
        "n-plate",
        GraphRunStatus::Pass,
        now - ChronoDuration::minutes(5),
        1,
    ))
    .unwrap();
    db.insert_graph_node(&node_row("n-entry", None, Some("g-run"), "entry"))
        .unwrap();
}

/// Connect a client to a hub serving `db` and consume the greeting.
async fn connected_client(
    db: &std::sync::Arc<Database>,
) -> (WsStream, tokio::task::JoinHandle<()>) {
    let state = SessionsState::for_test(test_hub(db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");
    (client, server)
}

// ── `graphs` ────────────────────────────────────────────────────────────

#[tokio::test]
async fn graphs_answers_progress_names_and_filters_archived_and_stale() {
    let (db, _dir) = test_db();
    let now = Utc::now();
    seed_plate_graph(&db, now);

    // Archived, otherwise recent: the archived filter must drop it.
    let mut archived = graph_row("g-arch", GraphStatus::Completed, "/srv/work/archived");
    archived.archived = true;
    archived.completed_at = Some(now);
    db.insert_graph(&archived).unwrap();

    // Completed 25h ago: outside the 24h window.
    let mut stale = graph_row("g-stale", GraphStatus::Completed, "/srv/work/stale");
    stale.created_at = now - ChronoDuration::hours(25);
    stale.started_at = Some(now - ChronoDuration::hours(25));
    stale.completed_at = Some(now - ChronoDuration::hours(25));
    db.insert_graph(&stale).unwrap();

    // Completed an hour ago: inside the window, listed after the runner.
    let mut fresh = graph_row("g-fresh", GraphStatus::Completed, "/srv/work/fresh");
    fresh.created_at = now - ChronoDuration::hours(2);
    fresh.completed_at = Some(now - ChronoDuration::hours(1));
    db.insert_graph(&fresh).unwrap();

    let (mut client, server) = connected_client(&db).await;
    send_json(&mut client, &json!({"t": "graphs", "req": "gr1"})).await;
    let answer = recv_json(&mut client, "graphs").await;
    assert_eq!(answer["t"], "graphs");
    assert_eq!(answer["req"], "gr1", "must echo the request: {answer}");
    let listed = answer["graphs"].as_array().expect("graphs array");
    let ids: Vec<&str> = listed.iter().map(|g| g["id"].as_str().unwrap()).collect();

    assert!(ids.contains(&"g-run"), "running graph listed: {ids:?}");
    assert!(ids.contains(&"g-fresh"), "recent completed listed: {ids:?}");
    assert!(!ids.contains(&"g-arch"), "archived graph absent: {ids:?}");
    assert!(
        !ids.contains(&"g-stale"),
        "completed graph older than 24h absent: {ids:?}"
    );
    let run_pos = ids.iter().position(|id| *id == "g-run").unwrap();
    let fresh_pos = ids.iter().position(|id| *id == "g-fresh").unwrap();
    assert!(
        run_pos < fresh_pos,
        "running first, then completed: {ids:?}"
    );

    for row in listed {
        let workdir = row["workdir"].as_str().expect("workdir string");
        assert!(
            !workdir.starts_with('/'),
            "workdir is a basename, never an absolute path: {row}"
        );
    }

    let row = listed
        .iter()
        .find(|g| g["id"] == "g-run")
        .expect("the running graph row");
    assert_eq!(row["status"], "running");
    assert_eq!(row["done"], 2, "two completed queue specs: {row}");
    assert_eq!(row["total"], 3, "three queue members: {row}");
    assert_eq!(row["workdir"], "plate-project", "basename only: {row}");
    assert_eq!(
        row["current_spec"], "third spec",
        "queue member spec: {row}"
    );
    assert_eq!(
        row["current_node"], "check-plate",
        "a NAME, not an id: {row}"
    );
    assert_ne!(row["current_node"], "n-plate", "never the node id: {row}");
    assert_eq!(row["blocked"], false);
    assert!(row["updated_at"].is_string(), "RFC3339 stamp: {row}");
    server.abort();
}

// ── poisoned rows ───────────────────────────────────────────────────────

/// One unreadable graph must never blank the whole served list: the running
/// graph stays listed while the poisoned one is skipped.
#[tokio::test]
async fn graphs_skips_poisoned_row_and_lists_running() {
    let (db, dir) = test_db();
    db.insert_graph(&graph_row("g-run", GraphStatus::Running, "/srv/work/run"))
        .unwrap();
    db.insert_graph(&graph_row(
        "g-poison",
        GraphStatus::Running,
        "/srv/work/poison",
    ))
    .unwrap();
    db.insert_graph_spec(&spec_row(
        "s-poison",
        Some("g-poison"),
        "poisoned spec",
        0,
        GraphSpecStatus::Pending,
    ))
    .unwrap();
    // TEXT in an INTEGER column: the i64 read in summarize->current_spec
    // fails for this graph only.
    let poison = rusqlite::Connection::open(dir.path().join("test.db")).unwrap();
    poison
        .execute(
            "UPDATE graph_specs SET started_at='not-an-integer' WHERE id='s-poison'",
            [],
        )
        .unwrap();
    drop(poison);

    let (mut client, server) = connected_client(&db).await;
    send_json(&mut client, &json!({"t": "graphs", "req": "gp1"})).await;
    let answer = recv_json(&mut client, "graphs").await;
    assert_eq!(
        answer["t"], "graphs",
        "poisoned row must not fail the list: {answer}"
    );
    assert_eq!(answer["req"], "gp1");
    let listed = answer["graphs"].as_array().expect("graphs array");
    let ids: Vec<&str> = listed.iter().map(|g| g["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"g-run"), "running graph listed: {ids:?}");
    assert!(
        !ids.contains(&"g-poison"),
        "poisoned graph skipped: {ids:?}"
    );
    server.abort();
}

/// Truncation still favors running graphs: one runner plus more fresh
/// completed graphs than the cap fit keeps the runner at the head.
#[tokio::test]
async fn graphs_truncation_keeps_running_first() {
    let (db, _dir) = test_db();
    db.insert_graph(&graph_row("g-run", GraphStatus::Running, "/srv/work/run"))
        .unwrap();
    for index in 0..(MAX_SUMMARIES + 5) {
        db.insert_graph(&graph_row(
            &format!("g-done-{index}"),
            GraphStatus::Completed,
            "/srv/work/done",
        ))
        .unwrap();
    }

    let (mut client, server) = connected_client(&db).await;
    send_json(&mut client, &json!({"t": "graphs", "req": "gt1"})).await;
    let answer = recv_json(&mut client, "graphs").await;
    assert_eq!(answer["t"], "graphs");
    let listed = answer["graphs"].as_array().expect("graphs array");
    assert_eq!(listed.len(), MAX_SUMMARIES, "list capped: {listed:?}");
    assert_eq!(
        listed[0]["id"], "g-run",
        "running graph survives truncation ahead of completed ones: {listed:?}"
    );
    server.abort();
}

/// Zero readable graphs answers an empty list, not an error.
#[tokio::test]
async fn graphs_all_poisoned_answers_empty_list() {
    let (db, dir) = test_db();
    db.insert_graph(&graph_row(
        "g-poison",
        GraphStatus::Running,
        "/srv/work/poison",
    ))
    .unwrap();
    db.insert_graph_spec(&spec_row(
        "s-poison",
        Some("g-poison"),
        "poisoned spec",
        0,
        GraphSpecStatus::Pending,
    ))
    .unwrap();
    let poison = rusqlite::Connection::open(dir.path().join("test.db")).unwrap();
    poison
        .execute(
            "UPDATE graph_specs SET started_at='not-an-integer' WHERE id='s-poison'",
            [],
        )
        .unwrap();
    drop(poison);

    let (mut client, server) = connected_client(&db).await;
    send_json(&mut client, &json!({"t": "graphs", "req": "ge1"})).await;
    let answer = recv_json(&mut client, "graphs").await;
    assert_eq!(
        answer["t"], "graphs",
        "only poisoned rows must not fail the list: {answer}"
    );
    let listed = answer["graphs"].as_array().expect("graphs array");
    assert!(listed.is_empty(), "empty list, not an error: {listed:?}");
    server.abort();
}

// ── `graph_watch` ───────────────────────────────────────────────────────

#[tokio::test]
async fn graph_watch_answers_document_and_state_then_one_diffed_push() {
    let (db, _dir) = test_db();
    let now = Utc::now();
    seed_plate_graph(&db, now);

    let (mut client, server) = connected_client(&db).await;
    send_json(
        &mut client,
        &json!({"t": "graph_watch", "req": "w1", "id": "g-run"}),
    )
    .await;
    let first = recv_json(&mut client, "graph").await;
    assert_eq!(first["t"], "graph");
    assert_eq!(first["req"], "w1");
    assert_eq!(first["id"], "g-run");

    // The document is the catalogue-plate export of this graph, platform
    // config kept so the plate can colour nodes by harness.
    let document = &first["document"];
    assert!(
        document["format_version"].as_i64().is_some(),
        "export document: {document}"
    );
    let doc_nodes = document["nodes"].as_array().expect("document nodes");
    assert_eq!(doc_nodes.len(), 1, "only the top-level node: {document}");
    assert_eq!(doc_nodes[0]["name"], "entry");
    assert_eq!(
        doc_nodes[0]["config"]["platform"], "claude",
        "node configs keep platform/model: {document}"
    );

    // The state is the plate overlay: queue order, node names, statuses.
    let state = &first["state"];
    assert_eq!(state["status"], "running");
    assert_eq!(state["current_spec_id"], "s-3");
    assert_eq!(state["current_node"], "check-plate");
    assert_eq!(state["blocked"], false);
    assert_eq!(state["blocker"], serde_json::Value::Null);
    assert_eq!(state["nodes"]["check-plate"], "pass");
    assert!(
        state["nodes"].get("entry").is_none(),
        "a node with no run is absent: {state}"
    );
    let specs = state["specs"].as_array().expect("specs array");
    let spec_ids: Vec<&str> = specs.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(spec_ids, ["s-1", "s-2", "s-3"], "queue order: {specs:?}");
    assert_eq!(specs[0]["code"], "FEAT-12");
    assert_eq!(specs[1]["code"], serde_json::Value::Null);
    assert_eq!(specs[0]["status"], "completed");
    assert_eq!(specs[2]["status"], "pending");
    assert!(specs[0]["attempts"].is_i64(), "attempts present: {specs:?}");
    assert!(state["updated_at"].is_string());

    // Change the seeded graph's current node in the test DB: exactly one
    // `graph_state` push follows.
    db.insert_graph_node(&node_row_at("n-two", Some("s-3"), None, "check-two", 1))
        .unwrap();
    db.insert_graph_run(&run_row(
        "g-run",
        "s-3",
        "n-two",
        GraphRunStatus::Running,
        now,
        1,
    ))
    .unwrap();

    let push = recv_json(&mut client, "graph_state").await;
    assert_eq!(push["t"], "graph_state");
    assert_eq!(push["id"], "g-run");
    assert!(
        push.get("req").is_none(),
        "a push carries no request: {push}"
    );
    assert_eq!(push["state"]["current_node"], "check-two");
    assert_eq!(push["state"]["nodes"]["check-two"], "running");
    assert_eq!(push["state"]["nodes"]["check-plate"], "pass");

    // An unchanged tick yields no frame (a poll period is 2s; wait past it).
    assert!(
        try_recv_json(&mut client, Duration::from_millis(2500))
            .await
            .is_none(),
        "an unchanged tick must send nothing"
    );
    server.abort();
}

#[tokio::test]
async fn graph_unwatch_stops_the_pushes_without_an_answer_frame() {
    let (db, _dir) = test_db();
    let now = Utc::now();
    seed_plate_graph(&db, now);

    let (mut client, server) = connected_client(&db).await;
    send_json(
        &mut client,
        &json!({"t": "graph_watch", "req": "w1", "id": "g-run"}),
    )
    .await;
    assert_eq!(recv_json(&mut client, "graph").await["t"], "graph");

    send_json(&mut client, &json!({"t": "graph_unwatch", "id": "g-run"})).await;
    assert!(
        try_recv_json(&mut client, Duration::from_millis(500))
            .await
            .is_none(),
        "unwatch answers nothing"
    );

    // The watch is gone: a later state change pushes nothing.
    db.insert_graph_node(&node_row_at("n-two", Some("s-3"), None, "check-two", 1))
        .unwrap();
    db.insert_graph_run(&run_row(
        "g-run",
        "s-3",
        "n-two",
        GraphRunStatus::Running,
        now,
        1,
    ))
    .unwrap();
    assert!(
        try_recv_json(&mut client, Duration::from_millis(2500))
            .await
            .is_none(),
        "an unwatched graph pushes nothing"
    );
    server.abort();
}

#[tokio::test]
async fn graph_watch_unknown_or_archived_id_answers_not_found_and_starts_nothing() {
    let (db, _dir) = test_db();
    let now = Utc::now();
    seed_plate_graph(&db, now);
    let mut archived = graph_row("g-arch", GraphStatus::Completed, "/srv/work/archived");
    archived.archived = true;
    db.insert_graph(&archived).unwrap();

    let (mut client, server) = connected_client(&db).await;
    send_json(
        &mut client,
        &json!({"t": "graph_watch", "req": "u1", "id": "g-missing"}),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["t"], "error");
    assert_eq!(error["code"], "not_found");
    assert_eq!(error["req"], "u1", "must echo the request: {error}");
    assert!(
        error["message"]
            .as_str()
            .expect("message")
            .contains("g-missing"),
        "must name the id: {error}"
    );
    assert!(
        try_recv_json(&mut client, Duration::from_millis(2500))
            .await
            .is_none(),
        "an unknown id starts no watch: no graph frame may ever arrive"
    );

    send_json(
        &mut client,
        &json!({"t": "graph_watch", "req": "u2", "id": "g-arch"}),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["t"], "error");
    assert_eq!(
        error["code"], "not_found",
        "archived reads as not found: {error}"
    );
    assert_eq!(error["req"], "u2");
    server.abort();
}

// ── the watch cap ───────────────────────────────────────────────────────

#[tokio::test]
async fn the_fifth_graph_watch_answers_busy_and_starts_nothing() {
    let (db, _dir) = test_db();
    for index in 1..=5 {
        db.insert_graph(&graph_row(
            &format!("g-{index}"),
            GraphStatus::Draft,
            "/srv/work/w",
        ))
        .unwrap();
    }

    let (mut client, server) = connected_client(&db).await;
    for index in 1..=4 {
        send_json(
            &mut client,
            &json!({"t": "graph_watch", "req": format!("w{index}"), "id": format!("g-{index}")}),
        )
        .await;
    }
    let mut watched: Vec<String> = Vec::new();
    for _ in 0..4 {
        let frame = recv_json(&mut client, "graph").await;
        assert_eq!(frame["t"], "graph");
        watched.push(frame["id"].as_str().unwrap().to_string());
    }
    watched.sort();
    assert_eq!(
        watched,
        ["g-1", "g-2", "g-3", "g-4"],
        "four distinct watches answered"
    );

    send_json(
        &mut client,
        &json!({"t": "graph_watch", "req": "w5", "id": "g-5"}),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["t"], "error");
    assert_eq!(error["code"], "busy", "fifth watch refused: {error}");
    assert_eq!(error["req"], "w5", "must echo the request: {error}");
    assert!(
        try_recv_json(&mut client, Duration::from_millis(2500))
            .await
            .is_none(),
        "the refused fifth watch starts nothing"
    );
    server.abort();
}

// ── connection lifetime ─────────────────────────────────────────────────

/// A watch whose graph vanished ends on its own; the connection must be able
/// to spend that slot again instead of answering `busy` forever.
#[tokio::test]
async fn a_watch_that_ended_frees_its_slot_for_a_new_one() {
    let (db, _dir) = test_db();
    for index in 1..=5 {
        db.insert_graph(&graph_row(
            &format!("g-{index}"),
            GraphStatus::Draft,
            "/srv/work/w",
        ))
        .unwrap();
    }

    let (mut client, server) = connected_client(&db).await;
    for index in 1..=4 {
        send_json(
            &mut client,
            &json!({"t": "graph_watch", "req": format!("w{index}"), "id": format!("g-{index}")}),
        )
        .await;
    }
    for _ in 0..4 {
        assert_eq!(recv_json(&mut client, "graph").await["t"], "graph");
    }

    // g-1 vanishes: its watch's next poll finds no graph and ends the watch.
    db.delete_graph("g-1").unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;

    send_json(
        &mut client,
        &json!({"t": "graph_watch", "req": "w5", "id": "g-5"}),
    )
    .await;
    let frame = recv_json(&mut client, "graph").await;
    assert_eq!(
        frame["t"], "graph",
        "three live watches plus a freed slot accept a fourth: {frame}"
    );
    assert_eq!(frame["id"], "g-5");
    assert_eq!(frame["req"], "w5");
    server.abort();
}

#[tokio::test]
async fn closing_the_socket_ends_every_watch_task_promptly() {
    let (db, _dir) = test_db();
    for index in 1..=4 {
        db.insert_graph(&graph_row(
            &format!("g-{index}"),
            GraphStatus::Draft,
            "/srv/work/w",
        ))
        .unwrap();
    }
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(std::sync::Arc::clone(&state)).await;

    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");
    for index in 1..=4 {
        send_json(
            &mut client,
            &json!({"t": "graph_watch", "req": format!("w{index}"), "id": format!("g-{index}")}),
        )
        .await;
    }
    for _ in 0..4 {
        assert_eq!(recv_json(&mut client, "graph").await["t"], "graph");
    }
    assert_eq!(
        state.live_graph_watches(),
        4,
        "four live watches on the connection"
    );

    // Close the socket: every watch task must end with it, not on its next
    // 2-second tick — the deadline is deliberately shorter than one poll
    // period, so a watch left running until `out_tx` closes fails here.
    drop(client);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
    while state.live_graph_watches() > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        state.live_graph_watches(),
        0,
        "no watch task may outlive its socket"
    );

    // A fresh connection is unaffected: it answers and holds its own four.
    let mut fresh = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut fresh, "hello").await["t"], "hello");
    send_json(&mut fresh, &json!({"t": "graphs", "req": "again"})).await;
    assert_eq!(recv_json(&mut fresh, "graphs").await["req"], "again");
    for index in 1..=4 {
        send_json(
            &mut fresh,
            &json!({"t": "graph_watch", "req": format!("f{index}"), "id": format!("g-{index}")}),
        )
        .await;
    }
    for _ in 0..4 {
        assert_eq!(recv_json(&mut fresh, "graph").await["t"], "graph");
    }
    server.abort();
}

// ── protocol round-trips ────────────────────────────────────────────────

#[test]
fn graph_frames_round_trip_through_both_parsers() {
    assert!(matches!(
        parse_client_frame(r#"{"t": "graphs", "req": "c1"}"#),
        Ok(ClientFrame::Graphs { req }) if req == "c1"
    ));
    assert!(matches!(
        parse_client_frame(r#"{"t": "graph_watch", "req": "c2", "id": "g1"}"#),
        Ok(ClientFrame::GraphWatch { req, id }) if req == "c2" && id == "g1"
    ));
    assert!(matches!(
        parse_client_frame(r#"{"t": "graph_unwatch", "id": "g1"}"#),
        Ok(ClientFrame::GraphUnwatch { id }) if id == "g1"
    ));
    let missing = parse_client_frame(r#"{"t": "graph_watch", "id": "g1"}"#).unwrap_err();
    assert!(missing.message.contains("req"), "{}", missing.message);

    let summaries = vec![GraphSummary {
        id: "g1".to_string(),
        name: "plate".to_string(),
        status: "running".to_string(),
        workdir: "proj".to_string(),
        current_spec: Some("third spec".to_string()),
        current_node: Some("check-plate".to_string()),
        done: 2,
        total: 3,
        blocked: false,
        updated_at: "2026-10-28T00:00:00+00:00".to_string(),
    }];
    match parse_host_frame(&graphs_frame("h1", &summaries)) {
        Ok(HostFrame::Graphs { req, graphs }) => {
            assert_eq!(req, "h1");
            assert_eq!(graphs.len(), 1);
            assert_eq!(graphs[0].done, 2);
            assert_eq!(graphs[0].total, 3);
            assert_eq!(graphs[0].current_node.as_deref(), Some("check-plate"));
        }
        other => panic!("expected a graphs frame, got {other:?}"),
    }

    let document = GraphExportDocument {
        format_version: 6,
        name: "plate".to_string(),
        description: None,
        nodes: vec![],
        edges: vec![],
        ensembles: vec![],
        infra_node: None,
        hooks: BTreeMap::new(),
        prompt_presets: Vec::new(),
    };
    let state = GraphState {
        status: "running".to_string(),
        blocked: false,
        blocker: None,
        current_spec_id: Some("s-3".to_string()),
        current_node: Some("check-plate".to_string()),
        nodes: BTreeMap::from([("check-plate".to_string(), "pass".to_string())]),
        specs: vec![],
        updated_at: "2026-10-28T00:00:00+00:00".to_string(),
    };
    match parse_host_frame(&graph_frame("h2", "g1", &document, &state)) {
        Ok(HostFrame::Graph {
            req,
            id,
            document,
            state,
        }) => {
            assert_eq!(req, "h2");
            assert_eq!(id, "g1");
            assert_eq!(document.name, "plate");
            assert_eq!(state.status, "running");
            assert_eq!(state.current_node.as_deref(), Some("check-plate"));
        }
        other => panic!("expected a graph frame, got {other:?}"),
    }

    match parse_host_frame(&graph_state_frame("g1", &state)) {
        Ok(HostFrame::GraphState { id, state }) => {
            assert_eq!(id, "g1");
            assert_eq!(
                state.nodes.get("check-plate").map(String::as_str),
                Some("pass")
            );
        }
        other => panic!("expected a graph_state frame, got {other:?}"),
    }
}
