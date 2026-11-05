//! Frame-size and ordering tests for the remote bridge's chunking.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::Value;

use super::{
    flush_coal, pump, split_out_frames, split_snapshot_frames, RemoteBridge, CHUNK_BYTES,
    MAX_FRAME_BYTES,
};
use crate::db::Database;
use crate::remote::metrics::{
    clamp_interval, host_metrics_frame, spawn_metrics_watch, MetricsSource,
};
use crate::remote::trust::Grant;
use crate::sessions::outbound::{self, ConnRx, PushOut, SessionCoal};
use crate::sessions::protocol::{parse_client_frame, ClientFrame, ERR_FORBIDDEN, ERR_NOT_FOUND};
use crate::sessions::ws::SessionsState;
use crate::sessions::{HubEvent, HubSession, SessionHub, SessionKind, SpawnRequest};
use crate::system::host_metrics::{GpuMetrics, HostMetrics, PowerMetrics};
use crate::system::{GpuInfo, PowerSource, SystemInfo};

fn json(frame: &str) -> Value {
    serde_json::from_str(frame).expect("frame is JSON")
}

fn data(frame: &Value) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(frame["data"].as_str().expect("data"))
        .expect("base64")
}

#[test]
fn a_hundred_kib_output_arrives_intact_in_order_and_within_the_frame_limit() {
    let payload: Vec<u8> = (0..100 * 1024).map(|index| (index % 251) as u8).collect();
    let frames = split_out_frames("s1", 42, &payload);
    assert!(frames.len() > 1, "100 KiB must be split");
    let mut rebuilt = Vec::new();
    for frame in &frames {
        assert!(
            frame.len() <= MAX_FRAME_BYTES,
            "frame of {} bytes exceeds {MAX_FRAME_BYTES}",
            frame.len()
        );
        let parsed = json(frame);
        assert_eq!(parsed["t"], "out");
        assert_eq!(parsed["id"], "s1");
        assert_eq!(parsed["seq"], 42, "every chunk keeps the source seq");
        let chunk = data(&parsed);
        assert!(chunk.len() <= CHUNK_BYTES);
        rebuilt.extend_from_slice(&chunk);
    }
    assert_eq!(rebuilt, payload, "nothing truncated, nothing reordered");
}

#[test]
fn a_small_output_is_one_frame_and_an_empty_one_still_gets_a_frame() {
    assert_eq!(split_out_frames("s1", 7, b"hi").len(), 1);
    let empty = split_out_frames("s1", 7, &[]);
    assert_eq!(empty.len(), 1);
    assert_eq!(json(&empty[0])["seq"], 7);
    assert!(data(&json(&empty[0])).is_empty());
}

#[test]
fn a_large_snapshot_keeps_its_geometry_in_the_first_frame_then_marks_the_last() {
    let payload = vec![b'x'; 100 * 1024];
    let frames = split_snapshot_frames("s1", 120, 40, 9, &payload);
    assert!(frames.len() > 1);
    let first = json(&frames[0]);
    assert_eq!(first["t"], "snapshot");
    assert_eq!(first["cols"], 120);
    assert_eq!(first["rows"], 40);
    assert_eq!(first["seq"], 9);
    let mut rebuilt = data(&first);
    for (index, frame) in frames.iter().enumerate().skip(1) {
        assert!(frame.len() <= MAX_FRAME_BYTES);
        let parsed = json(frame);
        assert_eq!(parsed["t"], "snapshot_more");
        assert_eq!(parsed["id"], "s1");
        assert_eq!(
            parsed["last"],
            index + 1 == frames.len(),
            "only the final continuation is last"
        );
        rebuilt.extend_from_slice(&data(&parsed));
    }
    assert_eq!(rebuilt, payload);
}

#[test]
fn a_snapshot_that_fits_is_one_snapshot_frame_closed_by_an_empty_last_part() {
    let frames = split_snapshot_frames("s1", 80, 24, 3, b"tiny");
    assert_eq!(frames.len(), 2);
    assert_eq!(json(&frames[0])["t"], "snapshot");
    assert_eq!(data(&json(&frames[0])), b"tiny");
    // The closing part is what tells the device the snapshot is whole.
    let last = json(&frames[1]);
    assert_eq!(last["t"], "snapshot_more");
    assert_eq!(last["last"], true);
    assert_eq!(data(&last), b"");
}

fn test_state() -> (Arc<SessionsState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Arc::new(Database::new(&dir.path().join("test.db")).expect("test database"));
    let hub = SessionHub::with_broadcast_capacity(db, "test-host".to_string(), 256);
    (SessionsState::for_test(hub, "token".to_string()), dir)
}

fn fake_source(metrics: HostMetrics) -> MetricsSource {
    Arc::new(move || metrics.clone())
}

async fn recv_frame(rx: &mut ConnRx, what: &str) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .expect("channel open");
    json(&frame)
}

#[test]
fn host_metrics_frame_keeps_all_eight_keys_present_as_null_when_absent() {
    let frame = json(&host_metrics_frame(Some("r1"), &HostMetrics::default()));
    assert_eq!(frame["t"], "host.metrics");
    assert_eq!(frame["req"], "r1");
    let metrics = &frame["metrics"];
    for key in [
        "cpu_usage",
        "cpu_temperature_c",
        "cpu_temp_source",
        "memory_used_mb",
        "memory_total_mb",
        "gpu",
        "power",
        "sampled_at",
    ] {
        assert!(metrics.get(key).is_some(), "key '{key}' is present");
        assert!(metrics[key].is_null(), "key '{key}' is null");
    }
}

#[test]
fn host_metrics_frame_carries_gpu_power_and_sampled_at() {
    let metrics = HostMetrics {
        cpu_usage: Some(12.5),
        cpu_temperature_c: Some(61.0),
        cpu_temp_source: Some("components".to_string()),
        memory_used_mb: Some(8192),
        memory_total_mb: Some(16384),
        gpu: Some(GpuMetrics {
            name: Some("RTX 4080".to_string()),
            usage: Some(33.0),
            temperature_c: Some(58.0),
            vram_used_mb: Some(4096),
            vram_total_mb: Some(16384),
        }),
        power: Some(PowerMetrics {
            watts: Some(120.5),
            limit_watts: Some(320.0),
            source: Some("gpu".to_string()),
        }),
        sampled_at: Some("2026-10-31T00:00:00Z".to_string()),
    };
    let frame = json(&host_metrics_frame(None, &metrics));
    assert_eq!(frame["t"], "host.metrics");
    assert!(frame.get("req").is_none(), "push frames carry no req");
    assert_eq!(frame["metrics"]["cpu_usage"], 12.5);
    assert_eq!(frame["metrics"]["memory_total_mb"], 16384);
    assert_eq!(frame["metrics"]["gpu"]["name"], "RTX 4080");
    assert_eq!(frame["metrics"]["gpu"]["vram_used_mb"], 4096);
    assert_eq!(frame["metrics"]["power"]["watts"], 120.5);
    assert_eq!(frame["metrics"]["power"]["source"], "gpu");
    assert_eq!(frame["metrics"]["sampled_at"], "2026-10-31T00:00:00Z");
}

#[test]
fn from_system_info_maps_memory_gpu_and_power() {
    let info = SystemInfo {
        cpu_usage: 42.0,
        cpu_temperature: Some(65.5),
        cpu_temp_source: Some("wmi".to_string()),
        memory_used: 8 * 1_048_576,
        memory_total: 16 * 1_048_576,
        gpu_info: Some(GpuInfo {
            name: "RTX".to_string(),
            vendor: "nvidia".to_string(),
            usage: Some(50.0),
            temperature: Some(70.0),
            vram_used: Some(2048),
            vram_total: Some(8192),
            power_watts: Some(150.0),
            power_limit_watts: Some(300.0),
        }),
        power_watts: Some(150.0),
        power_limit_watts: Some(300.0),
        power_source: Some(PowerSource::Gpu),
        ..SystemInfo::default()
    };
    let metrics = HostMetrics::from_system_info(&info);
    assert_eq!(metrics.cpu_usage, Some(42.0));
    assert_eq!(metrics.cpu_temp_source.as_deref(), Some("wmi"));
    assert_eq!(metrics.memory_used_mb, Some(8));
    assert_eq!(metrics.memory_total_mb, Some(16));
    let gpu = metrics.gpu.expect("gpu present");
    assert_eq!(gpu.name.as_deref(), Some("RTX"));
    assert_eq!(gpu.temperature_c, Some(70.0));
    assert_eq!(gpu.vram_used_mb, Some(2048));
    let power = metrics.power.expect("power present");
    assert_eq!(power.watts, Some(150.0));
    assert_eq!(power.source.as_deref(), Some("gpu"));
    assert!(
        metrics
            .sampled_at
            .as_deref()
            .is_some_and(|at| chrono::DateTime::parse_from_rfc3339(at).is_ok()),
        "sampled_at is RFC 3339"
    );

    let empty = HostMetrics::from_system_info(&SystemInfo::default());
    assert!(empty.memory_used_mb.is_none());
    assert!(empty.memory_total_mb.is_none());
    assert!(empty.gpu.is_none());
    assert!(empty.power.is_none());
}

#[test]
fn watch_interval_clamps_to_five_to_sixty_with_a_ten_second_default() {
    assert_eq!(clamp_interval(None), 10);
    assert_eq!(clamp_interval(Some(0)), 5);
    assert_eq!(clamp_interval(Some(4)), 5);
    assert_eq!(clamp_interval(Some(7)), 7);
    assert_eq!(clamp_interval(Some(600)), 60);
}

#[test]
fn client_frames_parse_host_metrics_requests() {
    match parse_client_frame(r#"{"t":"host.metrics","req":"m1"}"#) {
        Ok(ClientFrame::HostMetrics { req }) => assert_eq!(req, "m1"),
        other => panic!("unexpected parse of host.metrics: {other:?}"),
    }
    match parse_client_frame(r#"{"t":"host.metrics.watch"}"#) {
        Ok(ClientFrame::HostMetricsWatch { interval_s }) => assert_eq!(interval_s, None),
        other => panic!("unexpected parse of host.metrics.watch: {other:?}"),
    }
    match parse_client_frame(r#"{"t":"host.metrics.watch","interval_s":7}"#) {
        Ok(ClientFrame::HostMetricsWatch { interval_s }) => assert_eq!(interval_s, Some(7)),
        other => panic!("unexpected parse of host.metrics.watch with interval: {other:?}"),
    }
    assert!(matches!(
        parse_client_frame(r#"{"t":"host.metrics.unwatch"}"#),
        Ok(ClientFrame::HostMetricsUnwatch)
    ));
    let bad = parse_client_frame(r#"{"t":"host.metrics.watch","interval_s":"x"}"#)
        .expect_err("a string interval_s is rejected");
    assert!(bad.message.contains("interval_s"), "{}", bad.message);
    let missing = parse_client_frame(r#"{"t":"host.metrics"}"#).expect_err("req is required");
    assert!(missing.message.contains("'req'"), "{}", missing.message);
}

#[tokio::test]
async fn metrics_watch_pushes_until_dropped() {
    let metrics = HostMetrics {
        cpu_usage: Some(1.0),
        ..HostMetrics::default()
    };
    let (out, mut rx) = outbound::channel();
    let watch = spawn_metrics_watch(fake_source(metrics), out, Duration::from_millis(20));
    for _ in 0..2 {
        let frame = recv_frame(&mut rx, "metrics push").await;
        assert_eq!(frame["t"], "host.metrics");
        assert!(frame.get("req").is_none(), "pushes carry no req");
        assert_eq!(frame["metrics"]["cpu_usage"], 1.0);
    }
    drop(watch);
    while rx.try_recv().is_ok() {}
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(rx.try_recv().is_err(), "no frames after drop");
}

#[tokio::test]
async fn metrics_watch_ends_when_the_channel_closes() {
    let (out, rx) = outbound::channel();
    let watch = spawn_metrics_watch(
        fake_source(HostMetrics::default()),
        out,
        Duration::from_millis(10),
    );
    drop(rx);
    tokio::time::timeout(Duration::from_millis(500), async {
        while !watch.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the watch task ends after close");
}

#[tokio::test]
async fn bridge_answers_host_metrics_with_req() {
    let (state, _dir) = test_state();
    let metrics = HostMetrics {
        cpu_usage: Some(9.0),
        ..HostMetrics::default()
    };
    let mut bridge = RemoteBridge::with_metrics(state, fake_source(metrics));
    let (out, mut rx) = outbound::channel();
    bridge
        .handle_text(r#"{"t":"host.metrics","req":"m1"}"#, &out)
        .await;
    let frame = recv_frame(&mut rx, "host.metrics answer").await;
    assert_eq!(frame["t"], "host.metrics");
    assert_eq!(frame["req"], "m1");
    assert_eq!(frame["metrics"]["cpu_usage"], 9.0);
}

#[tokio::test]
async fn bridge_watch_registers_and_unwatch_clears() {
    let (state, _dir) = test_state();
    let mut bridge = RemoteBridge::with_metrics(state, fake_source(HostMetrics::default()));
    let (out, mut rx) = outbound::channel();
    assert!(bridge.metrics_watch.is_none());
    bridge
        .handle_text(r#"{"t":"host.metrics.watch","interval_s":5}"#, &out)
        .await;
    assert!(bridge.metrics_watch.is_some());
    let first = recv_frame(&mut rx, "first push").await;
    assert_eq!(first["t"], "host.metrics");
    bridge
        .handle_text(r#"{"t":"host.metrics.unwatch"}"#, &out)
        .await;
    assert!(bridge.metrics_watch.is_none());
    assert!(rx.try_recv().is_err(), "unwatch answers nothing");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(rx.try_recv().is_err(), "no frames after unwatch");

    bridge
        .handle_text(r#"{"t":"host.metrics.watch"}"#, &out)
        .await;
    assert!(bridge.metrics_watch.is_some());
    bridge.abort();
    assert!(bridge.metrics_watch.is_none(), "abort clears the watch");
    while rx.try_recv().is_ok() {}
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(rx.try_recv().is_err(), "no frames after abort");
}

#[tokio::test]
async fn bridge_second_watch_replaces_the_first() {
    let (state, _dir) = test_state();
    let mut bridge = RemoteBridge::with_metrics(state, fake_source(HostMetrics::default()));
    let (out, mut rx) = outbound::channel();
    bridge
        .handle_text(r#"{"t":"host.metrics.watch","interval_s":5}"#, &out)
        .await;
    let first = recv_frame(&mut rx, "first watch push").await;
    assert_eq!(first["t"], "host.metrics");
    bridge
        .handle_text(r#"{"t":"host.metrics.watch","interval_s":5}"#, &out)
        .await;
    assert!(bridge.metrics_watch.is_some(), "one watcher per channel");
    let second = recv_frame(&mut rx, "second watch push").await;
    assert_eq!(second["t"], "host.metrics");
    assert!(
        second.get("req").is_none(),
        "replacement pushes carry no req"
    );
}

#[tokio::test]
async fn bridge_watch_clamps_a_zero_interval_instead_of_panicking() {
    let (state, _dir) = test_state();
    let mut bridge = RemoteBridge::with_metrics(state, fake_source(HostMetrics::default()));
    let (out, mut rx) = outbound::channel();
    bridge
        .handle_text(r#"{"t":"host.metrics.watch","interval_s":0}"#, &out)
        .await;
    let frame = recv_frame(&mut rx, "clamped watch push").await;
    assert_eq!(frame["t"], "host.metrics");
}

fn spawn_cat(hub: &SessionHub, workdir: &str) -> Arc<HubSession> {
    let request = SpawnRequest {
        kind: SessionKind::Terminal,
        cli: "cat".to_string(),
        model: None,
        workdir: workdir.to_string(),
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
    };
    hub.spawn(&request).expect("spawn cat")
}

/// A 50 Hz spinner on the remote data channel yields at most 20 `out`
/// frames per second, coalescing several chunks into each frame.
#[tokio::test]
async fn a_fifty_hertz_spinner_yields_at_most_twenty_out_frames_per_second() {
    let (tx, mut rx) = outbound::channel();
    let mut coal = SessionCoal::remote();
    let chunk = vec![b'.'; 100];
    let mut seq = 0u64;
    let mut frames: Vec<String> = Vec::new();
    let start = tokio::time::Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        seq += 1;
        assert_eq!(coal.push_out(&tx, seq, &chunk), PushOut::Buffered);
        flush_coal("s", &tx, &mut coal);
        // A real device drains as it goes; a stalled one would fall back to a
        // snapshot instead of being measured for frame rate.
        while let Ok(frame) = rx.try_recv() {
            frames.push(frame);
        }
    }
    // Let the last buffered frame leave under the rate cap.
    tokio::time::sleep(Duration::from_millis(60)).await;
    flush_coal("s", &tx, &mut coal);
    while let Ok(frame) = rx.try_recv() {
        frames.push(frame);
    }

    let count = frames.len();
    let mut last_seq = 0;
    let mut largest = 0;
    for frame in &frames {
        let value = json(frame);
        assert_eq!(value["t"], "out");
        last_seq = value["seq"].as_u64().expect("out seq");
        largest = largest.max(data(&value).len());
    }
    assert!(
        count <= 20,
        "a 50 Hz spinner produced {count} out frames in one second"
    );
    assert!(count >= 1, "the spinner does produce output");
    assert_eq!(last_seq, seq, "the final frame carries the latest seq");
    assert!(
        largest > chunk.len(),
        "chunks are coalesced into a single frame (largest {largest})"
    );
}

/// After a lagged broadcast (or a bound fallback) the forwarder sends a fresh
/// snapshot, then resumes live output at or after the snapshot's seq.
#[tokio::test]
async fn a_lagged_forwarder_resumes_live_output_after_the_snapshot_seq() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Arc::new(Database::new(&dir.path().join("test.db")).expect("test database"));
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = SessionHub::with_broadcast_capacity(db, "test-host".to_string(), 8);
    let session = spawn_cat(&hub, &workdir.path().to_string_lossy());
    session.ingest_chunk(b"before\r\n");

    let (tx, mut rx) = outbound::channel();
    let mut coal = SessionCoal::remote();
    let mut next = 0u64;
    assert!(pump(
        &session,
        &mut next,
        Err(tokio::sync::broadcast::error::RecvError::Lagged(1)),
        &tx,
        &mut coal
    ));
    let snapshot_seq = next;
    assert_eq!(snapshot_seq, 1, "the snapshot covers ingested output");

    session.ingest_chunk(b"after\r\n");
    assert!(pump(
        &session,
        &mut next,
        Ok((2, b"after\r\n".to_vec())),
        &tx,
        &mut coal
    ));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(flush_coal(&session.id, &tx, &mut coal));

    let mut saw_snapshot = false;
    let mut live_seq = None;
    while let Ok(frame) = rx.try_recv() {
        let value = json(&frame);
        match value["t"].as_str() {
            Some("snapshot") => {
                assert_eq!(value["seq"].as_u64(), Some(snapshot_seq));
                saw_snapshot = true;
            }
            Some("out") => live_seq = value["seq"].as_u64(),
            Some("snapshot_more") => {}
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert!(saw_snapshot, "the lagged forwarder sends a snapshot");
    let live_seq = live_seq.expect("live output resumes after the snapshot");
    assert!(
        live_seq > snapshot_seq,
        "live seq {live_seq} must advance past the snapshot seq {snapshot_seq}"
    );
    let _ = hub.kill(&session.id);
}

fn release_frame(id: &str) -> String {
    serde_json::json!({"t": "resize_release", "id": id}).to_string()
}

/// A guest may never release a size: `forbidden` for a session it can see
/// (narrow or full grant alike), `not_found` outside its grant.
#[tokio::test]
async fn guest_resize_release_is_refused() {
    let (state, _dir) = test_state();
    let session = spawn_cat(&state.hub, "/tmp");
    let frame = release_frame(&session.id);

    let narrow = Grant::Sessions(std::collections::HashSet::from([session.name.clone()]));
    let (out_tx, mut out_rx) = outbound::channel();
    let mut guest = RemoteBridge::with_grant(Arc::clone(&state), narrow, true);
    guest.handle_text(&frame, &out_tx).await;
    assert_eq!(
        recv_frame(&mut out_rx, "refusal").await["code"],
        ERR_FORBIDDEN
    );
    guest
        .handle_text(&release_frame("no-such-session"), &out_tx)
        .await;
    assert_eq!(
        recv_frame(&mut out_rx, "refusal").await["code"],
        ERR_NOT_FOUND
    );
    assert!(
        guest.resize_gates.is_empty(),
        "a refused release registers no throttle"
    );
    guest.abort();

    let (out_tx, mut out_rx) = outbound::channel();
    let mut full = RemoteBridge::with_grant(Arc::clone(&state), Grant::All, true);
    full.handle_text(&frame, &out_tx).await;
    assert_eq!(
        recv_frame(&mut out_rx, "refusal").await["code"],
        ERR_FORBIDDEN
    );
    full.abort();

    let _ = state.hub.kill(&session.id);
}

/// An owner release answers nothing and announces `SizeReleased`; an abort
/// announces one release per session the device sized.
#[tokio::test]
async fn owner_release_and_abort_emit_size_released() {
    let (state, _dir) = test_state();
    let first = spawn_cat(&state.hub, "/tmp");
    let second = spawn_cat(&state.hub, "/tmp");
    let mut events = state.hub.subscribe_events();

    let (out_tx, mut out_rx) = outbound::channel();
    let mut owner = RemoteBridge::with_grant(Arc::clone(&state), Grant::All, true);
    owner.set_owner(true);
    for session in [&first, &second] {
        let sized = serde_json::json!({
            "t": "resize", "id": session.id, "cols": 100, "rows": 30,
        })
        .to_string();
        owner.handle_text(&sized, &out_tx).await;
    }
    assert_eq!(owner.resize_gates.len(), 2);

    owner.handle_text(&release_frame(&first.id), &out_tx).await;
    assert!(
        out_rx.try_recv().is_err(),
        "a release answers nothing, like a resize"
    );
    assert!(
        !owner.resize_gates.contains_key(&first.id),
        "a release clears the session's throttle"
    );
    recv_released(&mut events, &first.id).await;

    owner.abort();
    recv_released(&mut events, &second.id).await;

    let _ = state.hub.kill(&first.id);
    let _ = state.hub.kill(&second.id);
}

async fn recv_released(events: &mut tokio::sync::broadcast::Receiver<HubEvent>, id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(HubEvent::SizeReleased { id: got }) = events.recv().await {
                if got == id {
                    return;
                }
            }
        }
    })
    .await
    .expect("the hub announces the release");
}
