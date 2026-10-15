//! Keys for the read-only knowledge viewer dialog.

use anyhow::Result;
use ratatui::crossterm::event::KeyCode;

use crate::tui::app::dialog::KnowledgeViewerDialog;
use crate::tui::app::types::{App, Focus};

use super::knowledge_dialog::edit_knowledge_dialog;

/// Open the read-only viewer for the currently selected knowledge entry.
pub fn open_knowledge_viewer(app: &mut App) {
    let Some(node) = app.project_knowledge.get(app.selected_knowledge).cloned() else {
        return;
    };

    let mut dialog = KnowledgeViewerDialog::new(node.title, node.kind, node.body);
    dialog.prev_focus = Some(app.focus);
    app.knowledge_viewer = Some(dialog);
    app.focus = Focus::KnowledgeViewer;
}

/// Esc closes back to the list with the selection untouched; `e` closes and
/// opens the edit dialog for the same entry; ↑↓/PgUp/PgDn scroll the body.
pub fn handle_knowledge_viewer_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Esc => {
            close_knowledge_viewer(app);
            return Ok(());
        }
        KeyCode::Char('e') => {
            close_knowledge_viewer(app);
            edit_knowledge_dialog(app);
            return Ok(());
        }
        _ => {}
    }

    let Some(dialog) = app.knowledge_viewer.as_mut() else {
        return Ok(());
    };

    let max_scroll = (dialog.body.lines().count() as u16).saturating_sub(1);
    match code {
        KeyCode::Up => dialog.scroll = dialog.scroll.saturating_sub(1),
        KeyCode::Down => dialog.scroll = (dialog.scroll + 1).min(max_scroll),
        KeyCode::PageUp => dialog.scroll = dialog.scroll.saturating_sub(10),
        KeyCode::PageDown => dialog.scroll = (dialog.scroll + 10).min(max_scroll),
        _ => {}
    }

    Ok(())
}

fn close_knowledge_viewer(app: &mut App) {
    let prev_focus = app
        .knowledge_viewer
        .as_ref()
        .and_then(|d| d.prev_focus)
        .unwrap_or(Focus::Agent);
    app.knowledge_viewer = None;
    app.focus = prev_focus;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn node(id: &str, title: &str) -> crate::db::intelligence::IntelligenceNodeRecord {
        crate::db::intelligence::IntelligenceNodeRecord {
            id: id.to_string(),
            kind: "pattern".to_string(),
            status: "noted".to_string(),
            title: title.to_string(),
            body: format!("body of {title}"),
            metadata: None,
            project_hash: None,
            session_id: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn app_with_entries() -> App {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).expect("create test db"));
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.project_knowledge = vec![node("n1", "Entry 1"), node("n2", "Entry 2")];
        app.selected_knowledge = 1;
        app.focus = Focus::Agent;
        app
    }

    /// FR2: opening the viewer keeps the list selection; `Esc` closes it back
    /// to the list with the selection untouched.
    #[test]
    fn esc_closes_the_viewer_and_keeps_the_selection() {
        let mut app = app_with_entries();

        open_knowledge_viewer(&mut app);
        assert!(matches!(app.focus, Focus::KnowledgeViewer));
        assert_eq!(app.knowledge_viewer.as_ref().unwrap().title, "Entry 2");

        handle_knowledge_viewer_key(&mut app, KeyCode::Esc).expect("esc handled");

        assert!(app.knowledge_viewer.is_none(), "Esc closes the viewer");
        assert!(
            matches!(app.focus, Focus::Agent),
            "focus returns to the list"
        );
        assert_eq!(app.selected_knowledge, 1, "Esc must not move the selection");
    }
}
