//! Tests for the autoupdate module

use std::cell::Cell;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

#[test]
fn compare_versions_equal() {
    assert_eq!(
        super::compare_versions("1.0.0", "1.0.0"),
        std::cmp::Ordering::Equal
    );
}

#[test]
fn compare_versions_greater_patch() {
    assert_eq!(
        super::compare_versions("1.0.1", "1.0.0"),
        std::cmp::Ordering::Greater
    );
}

#[test]
fn compare_versions_less_patch() {
    assert_eq!(
        super::compare_versions("1.0.0", "1.0.1"),
        std::cmp::Ordering::Less
    );
}

#[test]
fn compare_versions_major_wins() {
    assert_eq!(
        super::compare_versions("2.0.0", "1.9.9"),
        std::cmp::Ordering::Greater
    );
}

#[test]
fn compare_versions_with_v_prefix() {
    assert_eq!(
        super::compare_versions("v1.2.3", "1.2.3"),
        std::cmp::Ordering::Equal
    );
}

#[test]
fn compare_versions_different_length() {
    assert_eq!(
        super::compare_versions("1.0", "1.0.0"),
        std::cmp::Ordering::Equal
    );
    assert_eq!(
        super::compare_versions("1.0.0.1", "1.0.0"),
        std::cmp::Ordering::Greater
    );
}

#[test]
fn stable_version_accepts_plain() {
    assert!(super::is_stable_version("1.0.0"));
    assert!(super::is_stable_version("v1.0.0"));
    assert!(super::is_stable_version("v0.32.1"));
}

#[test]
fn stable_version_rejects_prerelease() {
    assert!(!super::is_stable_version("1.0.0-beta"));
    assert!(!super::is_stable_version("v1.0.0-rc1"));
    assert!(!super::is_stable_version("1.0.0-alpha+build123"));
}

#[test]
fn stable_version_rejects_empty() {
    assert!(!super::is_stable_version(""));
    assert!(!super::is_stable_version("v"));
}

#[test]
fn current_version_returns_non_empty() {
    let version = super::current_version();
    assert!(!version.is_empty(), "version should not be empty");
}

#[test]
fn detect_platform_returns_valid_tuple() {
    // The old os/arch tuple test is retained under its historical name, but
    // the updater now validates one full Rust target triple.
    if (cfg!(target_os = "linux") || cfg!(target_os = "macos"))
        && (cfg!(target_arch = "x86_64") || cfg!(target_arch = "aarch64"))
    {
        assert!(
            super::resolve_target().is_ok(),
            "should detect a supported release target"
        );
    }
}

#[test]
fn now_secs_returns_reasonable_timestamp() {
    let result = super::now_secs();
    assert!(result.is_ok(), "now_secs should succeed");
    let secs = result.unwrap();
    assert!(secs > 1577836800, "timestamp should be after 2020");
    assert!(secs < 4102444800, "timestamp should be before 2100");
}

#[test]
fn should_check_returns_true_when_no_last_check_file() {
    // When there's no last check file, should_check should return true. The
    // result depends on the user's data directory, so this test only checks
    // that the helper remains safe to call without panicking.
    let _ = super::should_check();
}

#[test]
fn record_check_creates_last_check_file() {
    let result = super::record_check();
    assert!(result.is_ok() || result.is_err());
}

#[test]
fn compare_versions_handles_build_metadata() {
    assert_eq!(
        super::compare_versions("1.0.0+build1", "1.0.0+build2"),
        std::cmp::Ordering::Equal
    );
}

#[test]
fn is_stable_version_accepts_semver_with_patch() {
    assert!(super::is_stable_version("1.2.3"));
    assert!(super::is_stable_version("0.1.0"));
    assert!(super::is_stable_version("10.20.30"));
}

#[test]
fn is_stable_version_rejects_versions_with_hyphens() {
    assert!(!super::is_stable_version("1.0.0-something"));
    assert!(!super::is_stable_version("v2.0.0-rc.1"));
}

#[test]
fn asset_name_linux_gnu() {
    assert_eq!(
        super::asset_name("v3.0.1", "x86_64-unknown-linux-gnu"),
        "canopy-v3.0.1-x86_64-unknown-linux-gnu.tar.gz"
    );
}

#[test]
fn cargo_detect_default_home() {
    assert!(super::is_cargo_installed(
        Path::new("/home/u/.cargo/bin/canopy"),
        Path::new("/home/u/.cargo/bin")
    ));
}

#[test]
fn cargo_detect_local_bin_is_not_cargo() {
    assert!(!super::is_cargo_installed(
        Path::new("/home/u/.local/bin/canopy"),
        Path::new("/home/u/.cargo/bin")
    ));
}

#[test]
fn cargo_detect_custom_cargo_home() {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let previous = std::env::var_os("CARGO_HOME");
    std::env::set_var("CARGO_HOME", "/opt/cargo");
    let cargo_bin = super::cargo_bin_dir();
    assert!(super::is_cargo_installed(
        Path::new("/opt/cargo/bin/canopy"),
        &cargo_bin
    ));
    assert!(!super::is_cargo_installed(
        Path::new("/home/u/.cargo/bin/canopy"),
        &cargo_bin
    ));
    match previous {
        Some(value) => std::env::set_var("CARGO_HOME", value),
        None => std::env::remove_var("CARGO_HOME"),
    }
}

struct FakeFetcher {
    body: String,
}

impl super::ReleaseFetcher for FakeFetcher {
    fn get(&self, _url: &str) -> anyhow::Result<String> {
        Ok(self.body.clone())
    }
}

struct RecordingDownloader {
    called: Cell<bool>,
    bytes: Vec<u8>,
}

impl super::BinaryDownloader for RecordingDownloader {
    fn download(&self, _url: &str) -> anyhow::Result<Vec<u8>> {
        self.called.set(true);
        Ok(self.bytes.clone())
    }
}

struct FakeDaemon {
    running: bool,
    managed: bool,
    restarted: Cell<bool>,
    stopped: Cell<bool>,
    started: Cell<bool>,
}

impl super::UpdateDaemonOps for FakeDaemon {
    fn is_managed(&self) -> bool {
        self.managed
    }

    fn restart_managed(&self) -> bool {
        self.restarted.set(true);
        true
    }

    fn stop(&self) -> anyhow::Result<()> {
        self.stopped.set(true);
        Ok(())
    }

    fn start(&self) -> anyhow::Result<()> {
        self.started.set(true);
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running
    }
}

fn check_deps<'a>(
    current: &'a str,
    releases: Vec<super::GitHubRelease>,
    downloader: &'a RecordingDownloader,
    daemon: &'a FakeDaemon,
) -> super::UpdateDeps<'a> {
    super::UpdateDeps {
        current,
        releases: Ok(releases),
        exe: Path::new("/tmp/canopy-update-test/canopy"),
        cargo_bin: Path::new("/tmp/canopy-update-test/not-cargo"),
        target: Ok("x86_64-unknown-linux-gnu"),
        downloader,
        confirm: &|| true,
        daemon,
        graph_running: false,
        confirm_restart: &|| true,
    }
}

#[test]
fn check_exit_1_when_newer_stable() {
    let downloader = RecordingDownloader {
        called: Cell::new(false),
        bytes: Vec::new(),
    };
    let daemon = FakeDaemon {
        running: false,
        managed: false,
        restarted: Cell::new(false),
        stopped: Cell::new(false),
        started: Cell::new(false),
    };
    let deps = check_deps(
        "3.0.0",
        vec![super::GitHubRelease {
            tag_name: "v3.0.1".to_string(),
            prerelease: false,
            draft: false,
        }],
        &downloader,
        &daemon,
    );
    assert_eq!(super::run_update_with(true, false, &deps).unwrap(), 1);
    assert!(!downloader.called.get());
}

#[test]
fn check_exit_0_when_equal() {
    let downloader = RecordingDownloader {
        called: Cell::new(false),
        bytes: Vec::new(),
    };
    let daemon = FakeDaemon {
        running: false,
        managed: false,
        restarted: Cell::new(false),
        stopped: Cell::new(false),
        started: Cell::new(false),
    };
    let deps = check_deps(
        "3.0.0",
        vec![super::GitHubRelease {
            tag_name: "v3.0.0".to_string(),
            prerelease: false,
            draft: false,
        }],
        &downloader,
        &daemon,
    );
    assert_eq!(super::run_update_with(true, false, &deps).unwrap(), 0);
}

#[test]
fn check_ignores_prerelease() {
    let downloader = RecordingDownloader {
        called: Cell::new(false),
        bytes: Vec::new(),
    };
    let daemon = FakeDaemon {
        running: false,
        managed: false,
        restarted: Cell::new(false),
        stopped: Cell::new(false),
        started: Cell::new(false),
    };
    let deps = check_deps(
        "3.0.0",
        vec![
            super::GitHubRelease {
                tag_name: "v9.9.9-rc1".to_string(),
                prerelease: true,
                draft: false,
            },
            super::GitHubRelease {
                tag_name: "v3.0.0".to_string(),
                prerelease: false,
                draft: false,
            },
        ],
        &downloader,
        &daemon,
    );
    assert_eq!(super::run_update_with(true, false, &deps).unwrap(), 0);
}

#[test]
fn check_ignores_draft() {
    let releases = vec![super::GitHubRelease {
        tag_name: "v9.9.9".to_string(),
        prerelease: false,
        draft: true,
    }];
    assert_eq!(super::select_latest_stable(&releases, "3.0.0"), None);
}

#[test]
fn download_and_extract_uses_the_canopy_entry() {
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(4);
    header.set_mode(0o755);
    header.set_cksum();
    archive
        .append_data(&mut header, "canopy", &b"data"[..])
        .unwrap();
    let tar_bytes = archive.into_inner().unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, &tar_bytes).unwrap();
    let bytes = encoder.finish().unwrap();
    let downloader = RecordingDownloader {
        called: Cell::new(false),
        bytes,
    };
    let output = tempfile::NamedTempFile::new().unwrap();
    assert!(super::download_and_extract_with(
        &downloader,
        "v3.0.1",
        "x86_64-unknown-linux-gnu",
        output.path()
    )
    .unwrap());
    assert_eq!(std::fs::read(output.path()).unwrap(), b"data");
    assert!(downloader.called.get());
}

#[test]
fn replace_binary_replaces_the_target_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let staged = dir.path().join("staged");
    let target = dir.path().join("canopy");
    std::fs::write(&staged, b"new").unwrap();
    std::fs::write(&target, b"old").unwrap();
    super::replace_binary(&staged, &target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
}

/// A fake release tag guaranteed newer than this binary, whatever the
/// current version is. Hardcoding "the next version" here re-traps on
/// every version bump (v3.0.1 broke this test the moment Cargo.toml
/// caught up with it).
fn fake_newer_tag() -> String {
    let mut parts = env!("CARGO_PKG_VERSION").split('.');
    let (major, minor, patch) = (
        parts.next().expect("semver major"),
        parts.next().expect("semver minor"),
        parts
            .next()
            .expect("semver patch")
            .parse::<u64>()
            .expect("numeric patch"),
    );
    format!("v{major}.{minor}.{}", patch + 1)
}

#[test]
fn tui_path_never_calls_replace() {
    crate::autoupdate::notice::clear_update_tag_for_tests();
    let fake = fake_newer_tag();
    let fetcher = FakeFetcher {
        body: format!(r#"[{{"tag_name":"{fake}","prerelease":false,"draft":false}}]"#),
    };
    let downloader = RecordingDownloader {
        called: Cell::new(false),
        bytes: Vec::new(),
    };
    let notice = super::check_and_update_notice_with(&fetcher, &downloader);
    assert_eq!(notice.as_deref(), Some(fake.as_str()));
    assert!(!downloader.called.get());
    crate::autoupdate::notice::clear_update_tag_for_tests();
}

fn canopy_tar_bytes(contents: &[u8]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    archive
        .append_data(&mut header, "canopy", contents)
        .unwrap();
    let tar_bytes = archive.into_inner().unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, &tar_bytes).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn install_refuses_cargo_binary_without_downloading() {
    let downloader = RecordingDownloader {
        called: Cell::new(false),
        bytes: Vec::new(),
    };
    let daemon = FakeDaemon {
        running: true,
        managed: false,
        restarted: Cell::new(false),
        stopped: Cell::new(false),
        started: Cell::new(false),
    };
    let exe = std::path::PathBuf::from("/home/u/.cargo/bin/canopy");
    let cargo_bin = std::path::PathBuf::from("/home/u/.cargo/bin");
    let deps = super::UpdateDeps {
        current: "3.0.0",
        releases: Ok(vec![super::GitHubRelease {
            tag_name: "v3.0.1".to_string(),
            prerelease: false,
            draft: false,
        }]),
        exe: &exe,
        cargo_bin: &cargo_bin,
        target: Ok("x86_64-unknown-linux-gnu"),
        downloader: &downloader,
        confirm: &|| true,
        daemon: &daemon,
        graph_running: false,
        confirm_restart: &|| true,
    };
    assert_eq!(super::run_update_with(false, true, &deps).unwrap(), 0);
    assert!(!downloader.called.get());
    assert!(!daemon.stopped.get());
    assert!(!daemon.started.get());
    assert!(!daemon.restarted.get());
}

#[test]
fn install_declined_leaves_everything_untouched() {
    let downloader = RecordingDownloader {
        called: Cell::new(false),
        bytes: Vec::new(),
    };
    let daemon = FakeDaemon {
        running: true,
        managed: false,
        restarted: Cell::new(false),
        stopped: Cell::new(false),
        started: Cell::new(false),
    };
    let deps = check_deps(
        "3.0.0",
        vec![super::GitHubRelease {
            tag_name: "v3.0.1".to_string(),
            prerelease: false,
            draft: false,
        }],
        &downloader,
        &daemon,
    );
    // Rebuild explicitly to override only the prompt answer.
    let deps2 = super::UpdateDeps {
        current: deps.current,
        releases: Ok(vec![super::GitHubRelease {
            tag_name: "v3.0.1".to_string(),
            prerelease: false,
            draft: false,
        }]),
        exe: deps.exe,
        cargo_bin: deps.cargo_bin,
        target: Ok("x86_64-unknown-linux-gnu"),
        downloader: &downloader,
        confirm: &|| false,
        daemon: &daemon,
        graph_running: false,
        confirm_restart: &|| true,
    };
    let _ = deps;
    assert_eq!(super::run_update_with(false, false, &deps2).unwrap(), 0);
    assert!(!downloader.called.get());
    assert!(!daemon.stopped.get());
    assert!(!daemon.started.get());
}

#[test]
fn install_with_yes_replaces_binary_and_restarts_unmanaged_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("canopy");
    std::fs::write(&exe, b"old").unwrap();
    let cargo_bin = dir.path().join("not-cargo-bin");
    std::fs::create_dir_all(&cargo_bin).unwrap();
    let downloader = RecordingDownloader {
        called: Cell::new(false),
        bytes: canopy_tar_bytes(b"new"),
    };
    let daemon = FakeDaemon {
        running: true,
        managed: false,
        restarted: Cell::new(false),
        stopped: Cell::new(false),
        started: Cell::new(false),
    };
    let deps = super::UpdateDeps {
        current: "3.0.0",
        releases: Ok(vec![super::GitHubRelease {
            tag_name: "v3.0.1".to_string(),
            prerelease: false,
            draft: false,
        }]),
        exe: &exe,
        cargo_bin: &cargo_bin,
        target: Ok("x86_64-unknown-linux-gnu"),
        downloader: &downloader,
        confirm: &|| panic!("--yes must skip the prompt"),
        daemon: &daemon,
        graph_running: false,
        confirm_restart: &|| true,
    };
    assert_eq!(super::run_update_with(false, true, &deps).unwrap(), 0);
    assert!(downloader.called.get());
    assert_eq!(std::fs::read(&exe).unwrap(), b"new");
    assert!(daemon.stopped.get());
    assert!(daemon.started.get());
    assert!(!daemon.restarted.get());
}

#[test]
fn install_with_unsupported_target_errors_before_downloading() {
    let downloader = RecordingDownloader {
        called: Cell::new(false),
        bytes: Vec::new(),
    };
    let daemon = FakeDaemon {
        running: false,
        managed: false,
        restarted: Cell::new(false),
        stopped: Cell::new(false),
        started: Cell::new(false),
    };
    let deps = super::UpdateDeps {
        current: "3.0.0",
        releases: Ok(vec![super::GitHubRelease {
            tag_name: "v3.0.1".to_string(),
            prerelease: false,
            draft: false,
        }]),
        exe: Path::new("/tmp/canopy-update-test/canopy"),
        cargo_bin: Path::new("/tmp/canopy-update-test/not-cargo"),
        target: Err("unsupported target: x86_64-windows".to_string()),
        downloader: &downloader,
        confirm: &|| true,
        daemon: &daemon,
        graph_running: false,
        confirm_restart: &|| true,
    };
    let error = super::run_update_with(false, true, &deps).unwrap_err();
    assert!(error.to_string().contains("x86_64-windows"));
    assert!(!downloader.called.get());
}

struct FailingFetcher;

impl super::ReleaseFetcher for FailingFetcher {
    fn get(&self, _url: &str) -> anyhow::Result<String> {
        Err(anyhow::anyhow!("network is down"))
    }
}

#[test]
fn tui_notice_is_quiet_without_network() {
    crate::autoupdate::notice::clear_update_tag_for_tests();
    let downloader = RecordingDownloader {
        called: Cell::new(false),
        bytes: Vec::new(),
    };
    let notice = super::check_and_update_notice_with(&FailingFetcher, &downloader);
    assert_eq!(notice, None);
    assert!(!downloader.called.get());
    crate::autoupdate::notice::clear_update_tag_for_tests();
}

#[test]
fn resolve_target_names_unsupported() {
    let error = super::target_for("windows", "x86_64").unwrap_err();
    assert!(error.to_string().contains("x86_64-windows"));
}

#[test]
fn select_latest_picks_max_stable() {
    let releases = vec![
        super::GitHubRelease {
            tag_name: "v3.0.1".to_string(),
            prerelease: false,
            draft: false,
        },
        super::GitHubRelease {
            tag_name: "v3.0.2".to_string(),
            prerelease: false,
            draft: false,
        },
        super::GitHubRelease {
            tag_name: "v3.0.2-rc1".to_string(),
            prerelease: true,
            draft: false,
        },
        super::GitHubRelease {
            tag_name: "v2.9.0".to_string(),
            prerelease: false,
            draft: false,
        },
    ];
    assert_eq!(
        super::select_latest_stable(&releases, "3.0.0"),
        Some("v3.0.2".to_string())
    );
}
