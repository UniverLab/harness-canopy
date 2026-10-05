//! Directory-browser responsibility of the new-agent dialog: reading the
//! current directory, filtering entries, and navigating the working-directory
//! picker. Split out of `new_agent.rs` to keep every production file within
//! the file-size gate.

use super::{BackgroundTrigger, NewAgentDialog, NewTaskType};

impl NewAgentDialog {
    pub fn refresh_dir_entries(&mut self) {
        let Ok(entries) = std::fs::read_dir(&self.current_path) else {
            self.dir_entries.clear();
            return;
        };

        let include_files = self.task_type == NewTaskType::Background
            && self.background_trigger == BackgroundTrigger::Watch;

        let all: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        let mut dirs = collect_dir_names(&all);
        let mut files = if include_files {
            collect_file_names(&all)
        } else {
            Vec::new()
        };
        dirs.sort();
        files.sort();
        dirs.extend(files);

        self.dir_entries = dirs;
        self.dir_selected = 0;
        self.dir_scroll = 0;
        self.dir_filter.clear();
    }

    /// Return dir_entries filtered by dir_filter (case-insensitive).
    pub fn filtered_dir_entries(&self) -> Vec<String> {
        if self.dir_filter.is_empty() {
            return self.dir_entries.clone();
        }
        let q = self.dir_filter.to_lowercase();
        self.dir_entries
            .iter()
            .filter(|e| e.to_lowercase().contains(&q))
            .cloned()
            .collect()
    }

    /// Go up one directory level (← key).
    /// Remembers the directory we came from and positions cursor on it.
    pub fn go_up(&mut self) {
        if self.current_path == "/" {
            return;
        }
        // Remember the directory name we're leaving
        let leaving_name = self.current_path.rfind('/').and_then(|pos| {
            let name = &self.current_path[pos + 1..];
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        });
        let new_path = if let Some(pos) = self.current_path.rfind('/') {
            if pos == 0 {
                "/".to_string()
            } else {
                self.current_path[..pos].to_string()
            }
        } else {
            return;
        };
        self.current_path = new_path;
        self.working_dir = self.current_path.clone();
        if self.task_type == NewTaskType::Background
            && self.background_trigger == BackgroundTrigger::Watch
        {
            self.watch_path = self.current_path.clone();
        }
        self.dir_filter.clear();
        self.refresh_dir_entries();
        // Position cursor on the directory we came from
        if let Some(name) = leaving_name {
            let target = format!("{name}/");
            if let Some(idx) = self.dir_entries.iter().position(|e| e == &target) {
                self.dir_selected = idx;
            }
        }
    }

    /// Update working_dir preview based on currently selected item in picker
    /// This is called while navigating with ↑↓ to show real-time preview
    pub fn update_dir_preview(&mut self) {
        let filtered = self.filtered_dir_entries();
        if self.dir_selected >= filtered.len() {
            // Nothing selected, use current_path
            self.working_dir = self.current_path.clone();
            return;
        }

        let selected = filtered[self.dir_selected].clone();
        let name = selected.strip_suffix('/').unwrap_or(selected.as_str());
        let full_path = format!("{}/{}", self.current_path.trim_end_matches('/'), name);
        self.working_dir = full_path;
    }

    /// Navigate into the selected directory entry (→ key).
    pub fn navigate_to_selected(&mut self) {
        let filtered = self.filtered_dir_entries();
        if self.dir_selected >= filtered.len() {
            return;
        }

        let selected = filtered[self.dir_selected].clone();
        let name = selected.strip_suffix('/').unwrap_or(selected.as_str());
        let full_path = format!("{}/{}", self.current_path.trim_end_matches('/'), name);
        let is_dir = std::fs::metadata(&full_path)
            .map(|m| m.is_dir())
            .unwrap_or(false);

        if is_dir {
            self.current_path = full_path;
            self.working_dir = self.current_path.clone();
            if self.task_type == NewTaskType::Background
                && self.background_trigger == BackgroundTrigger::Watch
            {
                self.watch_path = self.current_path.clone();
            }
            self.dir_filter.clear();
            self.refresh_dir_entries();
        } else {
            // File selected (Watcher only) — set watch_path, stay in current dir
            self.watch_path = full_path;
        }
    }
}

fn collect_dir_names(entries: &[std::fs::DirEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                None
            } else {
                Some(format!("{name}/"))
            }
        })
        .collect()
}

fn collect_file_names(entries: &[std::fs::DirEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                None
            } else {
                Some(name)
            }
        })
        .collect()
}
