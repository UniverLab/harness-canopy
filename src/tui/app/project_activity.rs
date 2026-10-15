//! The project list's own refresh: where it comes from and which row stays
//! selected once the order moves.
//!
//! Kept out of `refresh.rs` (which was already at the 800-code-line ceiling)
//! so the ordering-by-activity behaviour and the hash-following selection read
//! in one place.

use super::*;

use crate::db::project::ProjectActivity;

impl App {
    /// Reload the Knowledge layer's project list, ordered by most recent
    /// activity.
    ///
    /// The order can change between ticks — another project gets written to,
    /// a session ends — so the selection follows the selected project's
    /// *hash* rather than its old index, and the same project stays selected.
    pub(crate) fn refresh_projects(&mut self) -> Result<()> {
        let previous = self.selected_project;
        let selected_hash = self.selected_project().map(|p| p.hash.clone());
        let ranked = self.db.list_projects_by_activity()?;
        self.project_last_activity = project_activity_index(&ranked);
        self.projects = ranked.into_iter().map(|entry| entry.project).collect();
        self.selected_project = if self.projects.is_empty() {
            0
        } else {
            selected_hash
                .and_then(|hash| self.projects.iter().position(|p| p.hash == hash))
                .unwrap_or_else(|| previous.min(self.projects.len() - 1))
        };
        self.refresh_project_knowledge()?;
        self.refresh_backlog_specs()?;
        self.refresh_project_preview_cache();
        // Keep an already-open History tab live instead of only loading it
        // once on first show — cheap (one indexed query) and scoped to just
        // the project currently being looked at.
        if self.project_focus == Some(ProjectTab::History) {
            if let Some(hash) = self.selected_project().map(|p| p.hash.clone()) {
                self.load_project_history(&hash);
            }
        }
        Ok(())
    }
}

/// Last-activity timestamps keyed by project hash — the recency the sidebar
/// renders on each project row, taken from the same ranked list the project
/// order comes from.
fn project_activity_index(ranked: &[ProjectActivity]) -> HashMap<String, i64> {
    ranked
        .iter()
        .map(|entry| (entry.project.hash.clone(), entry.last_activity))
        .collect()
}
