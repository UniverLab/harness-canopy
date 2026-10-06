use std::path::Path;

use anyhow::Result;

use crate::domain::clean::FileCandidate;

pub(super) fn scan_log_files(data_dir: &Path) -> Result<Vec<FileCandidate>> {
    let dir = data_dir.join("logs");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(out);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        out.push(FileCandidate {
            key: stem.to_string(),
            path,
            mtime: mtime_unix(&meta),
            size_bytes: meta.len(),
        });
    }
    Ok(out)
}

/// Terminal history lives at `terminals/<session_name>/history.toml`
/// (`tui::terminal_history`); `terminals/global_catalog.toml` is a shared
/// file, not a per-session dir, and is skipped by the `is_dir()` check.
pub(super) fn scan_terminal_dirs(data_dir: &Path) -> Result<Vec<FileCandidate>> {
    let dir = data_dir.join("terminals");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(out);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let hist_file = path.join("history.toml");
        let Ok(hist_meta) = std::fs::metadata(&hist_file) else {
            continue;
        };
        out.push(FileCandidate {
            key: name.to_string(),
            path,
            mtime: mtime_unix(&hist_meta),
            size_bytes: hist_meta.len(),
        });
    }
    Ok(out)
}

/// Only the top level of `rag/` is scanned, and never descended into —
/// `vectors.lancedb/` is LanceDB's own on-disk store and must never be
/// touched by name-based heuristics.
pub(super) fn scan_rag_residue(data_dir: &Path) -> Result<Vec<FileCandidate>> {
    let dir = data_dir.join("rag");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(out);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        out.push(FileCandidate {
            key: name.to_string(),
            path,
            mtime: mtime_unix(&meta),
            size_bytes: meta.len(),
        });
    }
    Ok(out)
}

fn mtime_unix(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(super) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}
