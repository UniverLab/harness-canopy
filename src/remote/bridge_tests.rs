//! Frame-size and ordering tests for the remote bridge's chunking.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::Value;

use super::{split_out_frames, split_snapshot_frames, RemoteBridge, CHUNK_BYTES, MAX_FRAME_BYTES};
use crate::db::Database;
use crate::remote::metrics::{
    clamp_interval, host_metrics_frame, spawn_metrics_watch, MetricsSource,
};
use crate::sessions::protocol::{parse_client_frame, ClientFrame};
use crate::sessions::ws::SessionsState;
use crate::sessions::SessionHub;
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

async fn recv_frame(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>, what: &str) -> Value {
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
    let (out, mut rx) = tokio::sync::mpsc::unbounded_channel();
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
    let (out, rx) = tokio::sync::mpsc::unbounded_channel();
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
    let (out, mut rx) = tokio::sync::mpsc::unbounded_channel();
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
    let (out, mut rx) = tokio::sync::mpsc::unbounded_channel();
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
    while rx.try_recv().is_ok() {}
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(rx.try_recv().is_err(), "no frames after unwatch");

    bridge
        .handle_text(r#"{"t":"host.metrics.watch"}"#, &out)
        .await;
    assert!(bridge.metrics_watch.is_some());
    bridge.abort();
    assert!(bridge.metrics_watch.is_none(), "abort clears the watch");
}
