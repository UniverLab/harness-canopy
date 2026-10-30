//! Source guard: the two emoji-presentation status glyphs this spec removed
//! (U+23F8 paused, U+26D4 blocked) must never return to `src/` — many
//! terminals draw them two cells wide while `unicode-width` counts one, so
//! the text after them loses its space or overlaps it. Narrowly scoped on
//! purpose: other glyphs in the tree are covered by follow-up specs, not by
//! this guard.

use std::path::{Path, PathBuf};
use unicode_width::UnicodeWidthStr;

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("read src");
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
fn no_pause_or_blocked_emoji_in_src() {
    // Escapes, never literals: this guard itself must not contain the needles.
    let needles = ["\u{23F8}", "\u{26D4}"];
    let mut files = Vec::new();
    rs_files(&src_dir(), &mut files);
    assert!(!files.is_empty(), "expected to find sources under src");
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
        "emoji status glyphs leaked back into src:\n{}",
        hits.join("\n")
    );
}

#[test]
fn status_glyphs_are_single_width() {
    for glyph in ["▶", "‖", "⊘", "○", "✓", "✗"] {
        assert_eq!(
            UnicodeWidthStr::width(glyph),
            1,
            "status glyph {glyph:?} must be single-width"
        );
    }
}
