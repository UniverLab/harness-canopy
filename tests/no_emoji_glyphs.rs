//! Source guard: the emoji-presentation status glyphs removed by FIX-19
//! (U+23F8 paused, U+26D4 blocked) and the autorun/RAG follow-up (U+23F0
//! alarm clock, U+23F2 timer clock, U+23F3 hourglass, U+2B07 down arrow,
//! U+2699 gear) must never return to `src/` — many terminals draw them two
//! cells wide while `unicode-width` counts one, so the text after them loses
//! its space or overlaps it.

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
    let needles = [
        "\u{23F8}", "\u{26D4}", "\u{23F0}", "\u{23F2}", "\u{23F3}", "\u{2B07}", "\u{2699}",
    ];
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

/// Every status-glyph literal renders exactly one space after the glyph
/// (`‖ paused`, `◷ autorun …`). A two-space literal is the pre-fix shape,
/// so it fails here.
#[test]
fn status_glyph_literals_take_exactly_one_space() {
    // Built with `concat!` so this guard itself never contains the needles.
    let needles = [
        concat!("‖", "  "),
        concat!("⊘", "  "),
        concat!("✓", "  "),
        concat!("◷", "  "),
        concat!("▼", "  "),
        concat!("◉", "  "),
        concat!("▲", "  "),
        concat!("▶", "  "),
    ];
    let mut files = Vec::new();
    rs_files(&src_dir(), &mut files);
    assert!(!files.is_empty(), "expected to find sources under src");
    let mut hits = Vec::new();
    for path in &files {
        let content = std::fs::read_to_string(path).expect("read source file");
        for needle in &needles {
            if content.contains(needle) {
                hits.push(format!("{} contains {needle:?}", path.display()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "status glyph followed by two spaces in src:\n{}",
        hits.join("\n")
    );
}

#[test]
fn status_glyphs_are_single_width() {
    for glyph in [
        "▶", "‖", "⊘", "○", "✓", "✗", "▼", "◉", "●", "·", "◷", "⚑", "▲", "◐",
    ] {
        assert_eq!(
            UnicodeWidthStr::width(glyph),
            1,
            "status glyph {glyph:?} must be single-width"
        );
    }
}
