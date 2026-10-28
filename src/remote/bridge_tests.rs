//! Frame-size and ordering tests for the remote bridge's chunking.

use base64::Engine as _;
use serde_json::Value;

use super::{split_out_frames, split_snapshot_frames, CHUNK_BYTES, MAX_FRAME_BYTES};

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
fn a_snapshot_that_fits_is_a_single_snapshot_frame() {
    let frames = split_snapshot_frames("s1", 80, 24, 3, b"tiny");
    assert_eq!(frames.len(), 1);
    assert_eq!(json(&frames[0])["t"], "snapshot");
    assert_eq!(data(&json(&frames[0])), b"tiny");
}
