//! Type-ahead filter over the Knowledge layer's project list.
//!
//! The filter lives next to — but separate from — `knowledge_filter`, which
//! narrows the entries *inside* a project. This one narrows the project list
//! itself, like a file browser filtering as you type.

use super::types::{App, SidebarLayer};

impl App {
    /// Indices into `projects` that pass the type-ahead filter, in recency
    /// order (the order `projects` is already ranked by). With an empty
    /// filter this is every index, so callers never need a special case.
    pub(crate) fn visible_project_indices(&self) -> Vec<usize> {
        if self.project_filter.is_empty() {
            return (0..self.projects.len()).collect();
        }
        let needle = self.project_filter.to_lowercase();
        self.projects
            .iter()
            .enumerate()
            .filter(|(_, project)| project.name.to_lowercase().contains(&needle))
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Append a character to the filter and move the selection to the first
    /// match (the list is re-sorted by the filter, so the old index would
    /// point at an unrelated project).
    pub(crate) fn push_project_filter(&mut self, c: char) {
        self.project_filter.push(c);
        self.select_first_visible_project();
    }

    /// Drop the filter's last character (Backspace).
    pub(crate) fn pop_project_filter(&mut self) {
        self.project_filter.pop();
        self.select_first_visible_project();
    }

    /// Clear the whole filter (Esc), staying on the project list.
    pub(crate) fn clear_project_filter(&mut self) {
        if self.project_filter.is_empty() {
            return;
        }
        self.project_filter.clear();
        self.select_first_visible_project();
    }

    fn select_first_visible_project(&mut self) {
        if let Some(&idx) = self.visible_project_indices().first() {
            self.selected_project = idx;
            self.refresh_graphs_selection();
        }
    }

    /// The filter only ever applies to the Knowledge layer's list, so drop it
    /// as soon as another layer is active. Called from the refresh tick, which
    /// covers every way of leaving the layer (arrows, F2, click, Home).
    pub(crate) fn sync_project_filter_to_layer(&mut self) {
        if self.sidebar_layer != SidebarLayer::Knowledge {
            self.project_filter.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::project::Project;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile, TempDir};

    fn project(hash: &str, name: &str) -> Project {
        Project {
            hash: hash.to_string(),
            path: format!("/tmp/{hash}"),
            name: name.to_string(),
            description: None,
            tags: None,
            indexed_at: None,
            created_at: 0,
        }
    }

    /// An `App` holding three projects — "canopy", "cadforge", "univerlab" —
    /// as if the list had already been ranked by recency.
    fn app_with_projects() -> (App, TempDir) {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).expect("create test db"));
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.projects = vec![
            project("h1", "canopy"),
            project("h2", "cadforge"),
            project("h3", "univerlab"),
        ];
        app.selected_project = 1;
        (app, data_dir)
    }

    #[test]
    fn empty_filter_exposes_every_project() {
        let (app, _dir) = app_with_projects();
        assert_eq!(app.visible_project_indices(), vec![0, 1, 2]);
    }

    #[test]
    fn typing_selects_the_first_match() {
        let (mut app, _dir) = app_with_projects();
        app.push_project_filter('c');
        assert_eq!(app.visible_project_indices(), vec![0, 1]);
        app.push_project_filter('a');
        // "canopy" and "cadforge" both contain "ca"; the first wins.
        assert_eq!(app.visible_project_indices(), vec![0, 1]);
        assert_eq!(app.selected_project, 0);
        assert_eq!(app.project_filter, "ca");
        app.push_project_filter('d');
        assert_eq!(app.visible_project_indices(), vec![1]);
        assert_eq!(app.selected_project, 1);
    }

    #[test]
    fn filter_matches_case_insensitively() {
        let (mut app, _dir) = app_with_projects();
        app.push_project_filter('C');
        app.push_project_filter('A');
        assert_eq!(app.visible_project_indices(), vec![0, 1]);
    }

    #[test]
    fn backspace_widens_the_filter_again() {
        let (mut app, _dir) = app_with_projects();
        app.push_project_filter('c');
        app.push_project_filter('a');
        app.push_project_filter('d');
        assert_eq!(app.visible_project_indices(), vec![1]);
        app.pop_project_filter();
        assert_eq!(app.visible_project_indices(), vec![0, 1]);
        assert_eq!(app.selected_project, 0);
        app.pop_project_filter();
        app.pop_project_filter();
        assert_eq!(app.visible_project_indices(), vec![0, 1, 2]);
        assert_eq!(app.project_filter, "");
    }

    #[test]
    fn clearing_restores_the_whole_list() {
        let (mut app, _dir) = app_with_projects();
        app.push_project_filter('z');
        assert!(app.visible_project_indices().is_empty());
        app.clear_project_filter();
        assert_eq!(app.project_filter, "");
        assert_eq!(app.visible_project_indices(), vec![0, 1, 2]);
    }

    #[test]
    fn filter_is_dropped_outside_the_knowledge_layer() {
        let (mut app, _dir) = app_with_projects();
        app.sidebar_layer = SidebarLayer::Knowledge;
        app.push_project_filter('c');
        app.sync_project_filter_to_layer();
        assert_eq!(app.project_filter, "c");

        app.sidebar_layer = SidebarLayer::Live;
        app.sync_project_filter_to_layer();
        assert_eq!(app.project_filter, "");
    }

    #[test]
    fn refresh_keeps_the_selected_project_by_hash_when_the_order_changes() {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).expect("create test db"));
        for (hash, name) in [("h-a", "alpha"), ("h-b", "bravo"), ("h-c", "charlie")] {
            db.upsert_project(&project(hash, name)).expect("upsert");
        }
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.refresh_projects().unwrap();
        assert_eq!(app.projects.len(), 3);

        // Select the second row, then drop the first project from the list:
        // every later row shifts up one, so an index-preserving refresh would
        // silently jump to a different project.
        let dropped = app.projects[0].hash.clone();
        let selected = app.projects[1].hash.clone();
        app.selected_project = 1;
        db.delete_project(&dropped).unwrap();
        app.refresh_projects().unwrap();

        assert_eq!(app.projects.len(), 2);
        assert_eq!(app.selected_project, 0);
        assert_eq!(app.projects[app.selected_project].hash, selected);
    }
}
