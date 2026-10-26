//! `tests` tests (split from `src/tui/app/dialog/prompt.rs`, pure move).
use super::*;
use chrono::Datelike;
use tempfile::tempdir;

fn project_entry(name: &str, path: &str) -> ProjectPickerEntry {
    ProjectPickerEntry {
        hash: format!("{name}-hash"),
        name: name.to_string(),
        path: path.to_string(),
    }
}

mod part01_tests;
mod part02_tests;
