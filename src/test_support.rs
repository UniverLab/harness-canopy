//! Per-process and per-test temporary directory isolation and cleanup.
//!
//! `isolate_temp_dir` runs as a `#[ctor::ctor]` before the test harness starts,
//! creating `<base>/canopy-test-<pid>` and pointing `TMPDIR` at it so every
//! `tempfile` allocation in the process lands inside that root. On Unix, an
//! `atexit` handler removes the root recursively when the test process exits.
//! On non-Unix targets the root is created but not removed automatically.

use std::any::Any;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::OnceLock;

static TEST_TMP_ROOT: OnceLock<PathBuf> = OnceLock::new();

thread_local! {
    static HELD_TEMPS: RefCell<Vec<Box<dyn Any>>> = const { RefCell::new(Vec::new()) };
}

#[ctor::ctor]
fn isolate_temp_dir() {
    let base = match std::env::var_os("CANOPY_TEST_BASE_TMPDIR").filter(|v| !v.is_empty()) {
        Some(val) => PathBuf::from(val),
        None => {
            let dir = std::env::temp_dir();
            std::env::set_var("CANOPY_TEST_BASE_TMPDIR", &dir);
            dir
        }
    };

    let root = base.join(format!("canopy-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create per-process test temp root");
    std::env::set_var("TMPDIR", &root);
    let _ = TEST_TMP_ROOT.set(root);

    #[cfg(unix)]
    unsafe {
        libc::atexit(cleanup_temp_root);
    }
    // On non-unix targets the directory is created but not removed.
}

#[cfg(unix)]
extern "C" fn cleanup_temp_root() {
    if let Some(root) = TEST_TMP_ROOT.get() {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Create a temporary directory with prefix `canopy-test-`, hold it in
/// thread-local storage until the thread exits, and return `<dir>/test.db`.
pub fn temp_db_path() -> PathBuf {
    let dir = tempfile::Builder::new()
        .prefix("canopy-test-")
        .tempdir()
        .expect("create per-test temp db dir");
    let path = dir.path().join("test.db");
    hold(dir);
    path
}

/// Store a temporary value in thread-local storage so it stays alive for the
/// duration of the test (and is dropped when the thread exits).
pub fn hold<T: 'static>(value: T) {
    HELD_TEMPS.with(|cell| cell.borrow_mut().push(Box::new(value)));
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    #[test]
    #[ignore]
    fn leak_probe() {
        if std::env::var_os("CANOPY_LEAK_PROBE").is_none_or(|v| v != "1") {
            return;
        }
        let tmpdir = std::env::var("TMPDIR").expect("TMPDIR must be set in leak_probe");
        println!("PROBE_ROOT={tmpdir}");

        let file = tempfile::NamedTempFile::new().expect("probe NamedTempFile");
        std::mem::forget(file); // forget-ok: deliberate leak probe

        let dir = tempfile::TempDir::new().expect("probe TempDir");
        std::mem::forget(dir); // forget-ok: deliberate leak probe

        let db_file = tempfile::NamedTempFile::new().expect("probe db file");
        let db_path = db_file.path().to_path_buf();
        std::mem::forget(db_file); // forget-ok: deliberate leak probe
        let _db = crate::db::Database::new(&db_path).expect("probe Database::new");

        let count = std::fs::read_dir(&tmpdir).expect("read TMPDIR").count();
        println!("PROBE_FILES={count}");
    }

    #[test]
    fn test_process_leaves_nothing_in_the_temp_dir() {
        let base = std::env::var_os("CANOPY_TEST_BASE_TMPDIR")
            .expect("CANOPY_TEST_BASE_TMPDIR must be set");
        let exe = std::env::current_exe().expect("current_exe");

        let child = Command::new(exe)
            .args([
                "test_support::tests::leak_probe",
                "--exact",
                "--nocapture",
                "--include-ignored",
            ])
            .env("CANOPY_LEAK_PROBE", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn leak_probe child");

        let child_pid = child.id();
        let output = child.wait_with_output().expect("wait_with_output");
        assert!(
            output.status.success(),
            "leak_probe child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8(output.stdout).expect("child stdout utf8");
        let probe_root = stdout
            .lines()
            .find_map(|line| line.strip_prefix("PROBE_ROOT="))
            .expect("PROBE_ROOT= line missing from child stdout");
        let expected_root = PathBuf::from(&base).join(format!("canopy-test-{child_pid}"));
        assert_eq!(
            PathBuf::from(probe_root),
            expected_root,
            "child root must equal <base>/canopy-test-<child_pid> and not nest"
        );

        let probe_files: usize = stdout
            .lines()
            .find_map(|line| line.strip_prefix("PROBE_FILES="))
            .expect("PROBE_FILES= line missing from child stdout")
            .parse()
            .expect("PROBE_FILES must be a usize");
        assert!(
            probe_files >= 3,
            "expected at least 3 entries in child TMPDIR, got {probe_files}"
        );

        #[cfg(unix)]
        assert!(
            !expected_root.exists(),
            "child temp root {} still exists after exit",
            expected_root.display()
        );
    }

    #[test]
    fn no_forgotten_tempfiles() {
        const FORGET_PAT: &str = "mem::forget("; // lint-pattern
        const LEAK_PATS: [&str; 3] = [
            ".keep()",          // lint-pattern
            "into_temp_path()", // lint-pattern
            ".persist(",        // lint-pattern
        ];

        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut violations = Vec::new();

        for sub in ["src", "tests"] {
            let root = manifest_dir.join(sub);
            if !root.exists() {
                continue;
            }
            for entry in walkdir::WalkDir::new(&root)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| {
                    e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "rs")
                })
            {
                let content = std::fs::read_to_string(entry.path()).expect("read rs file");
                let rel = entry
                    .path()
                    .strip_prefix(manifest_dir)
                    .unwrap_or(entry.path());
                for (idx, line) in content.lines().enumerate() {
                    if line.contains("// lint-pattern") {
                        continue;
                    }
                    let line_no = idx + 1;
                    if line.contains(FORGET_PAT) && !line.contains("forget-ok") {
                        violations.push(format!(
                            "{}:{}: mem::forget without forget-ok",
                            rel.display(),
                            line_no
                        ));
                    }
                    if LEAK_PATS.iter().any(|pat| line.contains(pat)) {
                        violations.push(format!(
                            "{}:{}: tempfile leak method",
                            rel.display(),
                            line_no
                        ));
                    }
                }
            }
        }

        assert!(
            violations.is_empty(),
            "found forgotten tempfiles:\n{}",
            violations.join("\n")
        );
    }
}
