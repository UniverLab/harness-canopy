#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

const TEST_VERSION: &str = "9.9.9-test";

fn target_archive_name() -> String {
    let os = match std::env::consts::OS {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        other => panic!("unsupported test OS: {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => panic!("unsupported test arch: {other}"),
    };
    format!("canopy-v{TEST_VERSION}-{arch}-{os}.tar.gz")
}

fn write_executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn setup_install_fixture(root: &Path) {
    let bin_dir = root.join("bin");
    let home_dir = root.join("home");
    let install_dir = root.join("install");
    let fixture_dir = root.join("fixture");
    let pkg_dir = fixture_dir.join("pkg");

    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&home_dir).unwrap();
    fs::create_dir_all(&install_dir).unwrap();
    fs::create_dir_all(&pkg_dir).unwrap();

    write_executable(
        &pkg_dir.join("canopy"),
        &format!("#!/bin/sh\necho \"$*\" >> \"$CANOPY_LOG\"\necho \"canopy {TEST_VERSION}\"\n"),
    );

    let archive_path = fixture_dir.join("archive.tar.gz");
    let tar_status = Command::new("tar")
        .args(["czf"])
        .arg(&archive_path)
        .arg("-C")
        .arg(&pkg_dir)
        .arg("canopy")
        .status()
        .unwrap();
    assert!(tar_status.success(), "tar failed");

    let sha_output = Command::new("sha256sum")
        .arg(&archive_path)
        .output()
        .unwrap();
    assert!(sha_output.status.success(), "sha256sum failed");
    let sha = String::from_utf8_lossy(&sha_output.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();

    let archive_name = target_archive_name();
    fs::write(fixture_dir.join("sums"), format!("{sha}  {archive_name}\n")).unwrap();
    fs::write(
        fixture_dir.join("minisig"),
        format!("untrusted comment: test\nAAA==\ntrusted comment: canopy v{TEST_VERSION}\nBBB==\n"),
    )
    .unwrap();

    write_executable(
        &bin_dir.join("curl"),
        r#"#!/bin/sh
OUT=""
URL=""
PREV=""
for a in "$@"; do
  if [ "$PREV" = "-o" ]; then OUT="$a"; PREV=""; continue; fi
  case "$a" in
    -o) PREV="-o" ;;
    -*) ;;
    *) URL="$a" ;;
  esac
done
case "$URL" in
  *SHA256SUMS.txt.minisig) cp "$FIXTURE_DIR/minisig" "$OUT" || exit 22; printf '200' ;;
  *SHA256SUMS.txt) cp "$FIXTURE_DIR/sums" "$OUT" || exit 22; printf '200' ;;
  *.tar.gz) cp "$FIXTURE_DIR/archive.tar.gz" "$OUT" || exit 22; printf '200' ;;
  *) exit 22 ;;
esac
"#,
    );

    write_executable(
        &bin_dir.join("minisign"),
        "#!/bin/sh\necho \"Signature verified\"\nexit 0\n",
    );
}

fn run_install_script(
    root: &Path,
    no_setup: bool,
    detach_tty: bool,
) -> (std::process::Output, String) {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install.sh");
    let real_path = std::env::var("PATH").unwrap_or_default();
    let log_path = root.join("canopy.log");

    let mut cmd = Command::new("sh");
    cmd.arg(script)
        .env(
            "PATH",
            format!("{}:{}", root.join("bin").display(), real_path),
        )
        .env("HOME", root.join("home"))
        .env("INSTALL_DIR", root.join("install"))
        .env("VERSION", TEST_VERSION)
        .env("FIXTURE_DIR", root.join("fixture"))
        .env("CANOPY_LOG", &log_path);

    if no_setup {
        cmd.env("CANOPY_NO_SETUP", "1");
    } else {
        cmd.env_remove("CANOPY_NO_SETUP");
    }

    if detach_tty {
        // SAFETY: setsid() is async-signal-safe and detaches the child from any controlling tty.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let output = cmd.output().expect("failed to execute install.sh");
    let log = fs::read_to_string(&log_path).unwrap_or_default();
    (output, log)
}

#[test]
fn install_script_skips_setup_when_canopy_no_setup_is_set() {
    let tmp = tempfile::tempdir().unwrap();
    setup_install_fixture(tmp.path());

    let (output, log) = run_install_script(tmp.path(), true, false);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "install.sh failed: stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("Run 'canopy setup' to finish setup."),
        "expected setup hint in stdout, got: {stdout}"
    );
    assert!(
        !log.lines().any(|line| line.trim() == "setup"),
        "canopy setup must not be called when CANOPY_NO_SETUP=1; log={log}"
    );
}

#[test]
fn install_script_skips_setup_without_controlling_tty() {
    let tmp = tempfile::tempdir().unwrap();
    setup_install_fixture(tmp.path());

    let (output, log) = run_install_script(tmp.path(), false, true);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "install.sh failed: stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("Run 'canopy setup' to finish setup."),
        "expected setup hint in stdout, got: {stdout}"
    );
    assert!(
        !log.lines().any(|line| line.trim() == "setup"),
        "canopy setup must not be called without a controlling tty; log={log}"
    );
}
