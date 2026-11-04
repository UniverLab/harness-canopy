//! Source guard: `src/tui/` must not own PTYs any more — the daemon hub
//! owns them and the TUI is only a client. Fails on the first needle found.

use std::path::{Path, PathBuf};

fn src_tui() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("tui")
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("read src/tui");
    for entry in entries {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn tui_owns_no_ptys() {
    // Built with `concat!` so this guard itself never contains the needles.
    let needles = [
        concat!("portable", "_pty"),
        concat!("native_", "pty_system"),
    ];
    let mut files = Vec::new();
    rs_files(&src_tui(), &mut files);
    assert!(!files.is_empty(), "expected to find sources under src/tui");
    let mut hits = Vec::new();
    for path in &files {
        let content = std::fs::read_to_string(path).expect("read source file");
        for needle in &needles {
            if content.contains(needle) {
                hits.push(format!("{} contains {needle}", path.display()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "PTY ownership leaked into src/tui:\n{}",
        hits.join("\n")
    );
}
