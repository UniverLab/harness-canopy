//! Hello-frame tests: the `harnesses` key lists the resolvable configured
//! CLIs, sorted ascending, and omits the unresolvable ones.

use std::path::PathBuf;

use crate::domain::canopy_config::CanopyConfig;
use crate::domain::cli_config::CliConfig;
use crate::sessions::protocol::{harnesses_in, hello_frame, OWNER_FEATURES};

fn cli(name: &str, binary: &str) -> CliConfig {
    CliConfig {
        name: name.to_string(),
        binary: binary.to_string(),
        ..Default::default()
    }
}

/// A canopy dir holding `clis`, kept alive by the returned temp dir.
fn fixture(clis: Vec<CliConfig>) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let canopy_dir = dir.path().join(".canopy");
    let config = CanopyConfig {
        clis,
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).expect("save fixture");
    (dir, canopy_dir)
}

#[test]
fn hello_frame_lists_resolvable_cli_and_omits_unresolvable() {
    let probe = tempfile::tempdir().expect("tempdir");
    let missing = probe.path().join("missing-bin");
    assert!(!missing.exists(), "fixture precondition");
    let current = std::env::current_exe().expect("current test binary");

    let (_dir, canopy_dir) = fixture(vec![
        cli("zulu", &current.to_string_lossy()),
        cli("kilo", "sh"),
        cli("bravo", &missing.to_string_lossy()),
        cli("mike", "canopy-no-such-binary-xyz"),
    ]);

    assert_eq!(harnesses_in(&canopy_dir), vec!["kilo", "zulu"]);
}

#[test]
fn hello_frame_carries_sorted_harnesses_key() {
    let list = vec!["claude".to_string(), "muse".to_string()];
    let raw = hello_frame("test-host", &list, false);
    assert!(raw.contains("\"harnesses\""), "missing key: {raw}");

    let hello: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    assert_eq!(hello["t"], "hello");
    assert_eq!(hello["proto"], 2);
    assert_eq!(hello["host_label"], "test-host");
    assert!(hello["features"].is_array(), "features stays an array");
    let harnesses = hello["harnesses"].as_array().expect("harnesses array");
    assert_eq!(harnesses.len(), list.len());
    let names: Vec<&str> = harnesses
        .iter()
        .map(|v| v.as_str().expect("name string"))
        .collect();
    assert!(
        names.windows(2).all(|w| w[0] <= w[1]),
        "harnesses must be ascending: {raw}"
    );
    assert_eq!(names, ["claude", "muse"]);

    println!("{raw}");
}

#[test]
fn hello_owner_features_extend_the_guest_list() {
    let guest: serde_json::Value =
        serde_json::from_str(&hello_frame("test-host", &[], false)).expect("valid JSON");
    assert_eq!(
        guest["features"],
        serde_json::json!(["shelf", "shelf_upload"])
    );

    let owner: serde_json::Value =
        serde_json::from_str(&hello_frame("test-host", &[], true)).expect("valid JSON");
    let mut expected: Vec<&str> = vec!["shelf", "shelf_upload"];
    expected.extend_from_slice(OWNER_FEATURES);
    assert_eq!(owner["features"], serde_json::json!(expected));
}

#[test]
fn hello_owner_features_offer_graph_control() {
    assert!(
        OWNER_FEATURES.contains(&"graph_control"),
        "the owner device must be offered graph_control: {OWNER_FEATURES:?}"
    );
}
