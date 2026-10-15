//! Type-ahead typing on the Knowledge layer's project list.
//!
//! Runs before every other Preview key handler, so on this one list a plain
//! letter is a filter character rather than a command: `j`/`k`/`h`/`l` and
//! `R` stop acting there, which is the point — every letter has to be
//! typable. Arrows, Enter and Esc keep their meaning (Esc clears a non-empty
//! filter instead of leaving for Home).

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::tui::app::types::{App, Focus, SidebarLayer};

/// Whether the type-ahead applies: on the sidebar's project list itself
/// (Knowledge layer, no project entered) and not while the RAG summary owns
/// the keyboard.
fn project_filter_active(app: &App) -> bool {
    app.focus == Focus::Preview
        && app.sidebar_layer == SidebarLayer::Knowledge
        && app.project_focus.is_none()
        && !app.agents_rag_focused
}

/// Consume keys belonging to the project type-ahead. Returns `true` when the
/// key was used for the filter, `false` to let the normal handler see it.
pub(super) fn preview_project_filter_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> bool {
    if !project_filter_active(app) {
        return false;
    }
    match code {
        // Ctrl/Alt combinations keep their own bindings; Shift is how
        // uppercase letters arrive, so it must stay allowed.
        KeyCode::Char(c) if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            app.push_project_filter(c);
            true
        }
        KeyCode::Backspace if !app.project_filter.is_empty() => {
            app.pop_project_filter();
            true
        }
        KeyCode::Esc if !app.project_filter.is_empty() => {
            app.clear_project_filter();
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::project::Project;
    use crate::tui::event::home_preview::handle_preview_key;
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

    /// Preview focus on the Knowledge layer's project list, holding three
    /// projects: "aardvark", "cadforge", "canopy" (the list's own order).
    fn knowledge_app() -> (App, TempDir) {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).expect("create test db"));
        for (hash, name) in [
            ("h-aardvark", "aardvark"),
            ("h-cadforge", "cadforge"),
            ("h-canopy", "canopy"),
        ] {
            db.upsert_project(&project(hash, name)).expect("upsert");
        }
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.sidebar_layer = SidebarLayer::Knowledge;
        app.focus = Focus::Preview;
        (app, data_dir)
    }

    fn names(app: &App) -> Vec<&str> {
        app.visible_project_indices()
            .into_iter()
            .map(|idx| app.projects[idx].name.as_str())
            .collect()
    }

    fn press(app: &mut App, code: KeyCode) {
        handle_preview_key(app, code, KeyModifiers::NONE).unwrap();
    }

    fn type_str(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn typing_ca_keeps_the_matches_and_selects_the_first() {
        let (mut app, _dir) = knowledge_app();
        type_str(&mut app, "ca");
        assert_eq!(app.project_filter, "ca");
        assert_eq!(names(&app), vec!["cadforge", "canopy"]);
        assert_eq!(app.selected_project, 1);
        assert_eq!(app.projects[app.selected_project].name, "cadforge");
    }

    #[test]
    fn backspace_widens_the_filter_back_one_character_at_a_time() {
        let (mut app, _dir) = knowledge_app();
        type_str(&mut app, "cad");
        assert_eq!(names(&app), vec!["cadforge"]);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.project_filter, "ca");
        assert_eq!(names(&app), vec!["cadforge", "canopy"]);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.project_filter, "");
        assert_eq!(names(&app), vec!["aardvark", "cadforge", "canopy"]);
    }

    #[test]
    fn esc_clears_a_non_empty_filter_and_stays_on_the_list() {
        let (mut app, _dir) = knowledge_app();
        type_str(&mut app, "cad");
        assert_eq!(names(&app), vec!["cadforge"]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.project_filter, "");
        assert_eq!(names(&app), vec!["aardvark", "cadforge", "canopy"]);
        assert!(app.focus == Focus::Preview, "Esc must not leave the list");
        assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);
    }

    #[test]
    fn esc_with_an_empty_filter_still_leaves_for_home() {
        let (mut app, _dir) = knowledge_app();
        press(&mut app, KeyCode::Esc);
        assert!(app.focus == Focus::Home);
    }

    #[test]
    fn plain_j_k_h_l_are_typed_instead_of_moving_the_list() {
        let (mut app, _dir) = knowledge_app();
        // `j` used to move the selection down one project.
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.project_filter, "j");
        assert!(names(&app).is_empty(), "no project is named with a j");
        // `h` used to leave for Home; `l` used to enter the project.
        press(&mut app, KeyCode::Backspace);
        assert!(app.focus == Focus::Preview);
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.project_filter, "h");
        assert!(app.focus == Focus::Preview);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.project_filter, "l");
        assert_eq!(app.project_focus, None);
    }

    #[test]
    fn arrows_wrap_inside_the_filtered_matches_only() {
        let (mut app, _dir) = knowledge_app();
        type_str(&mut app, "ca");
        assert_eq!(app.projects[app.selected_project].name, "cadforge");

        press(&mut app, KeyCode::Down);
        assert_eq!(app.projects[app.selected_project].name, "canopy");
        // Wrapping must land on the other match, never on "aardvark".
        press(&mut app, KeyCode::Down);
        assert_eq!(app.projects[app.selected_project].name, "cadforge");
        press(&mut app, KeyCode::Up);
        assert_eq!(app.projects[app.selected_project].name, "canopy");
        assert!(
            app.visible_project_indices()
                .contains(&app.selected_project),
            "arrows must stay inside the matches"
        );
    }

    #[test]
    fn enter_is_a_no_op_when_nothing_matches() {
        let (mut app, _dir) = knowledge_app();
        type_str(&mut app, "zz");
        assert!(names(&app).is_empty());
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.project_focus, None, "nothing to enter");
    }

    #[test]
    fn enter_still_enters_the_matching_project() {
        let (mut app, _dir) = knowledge_app();
        type_str(&mut app, "cad");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.project_focus,
            Some(crate::tui::app::types::ProjectTab::Overview)
        );
        assert!(app.focus == Focus::Agent);
    }

    #[test]
    fn ctrl_r_opens_the_relation_dialog_and_plain_r_no_longer_does() {
        let (mut app, _dir) = knowledge_app();
        press(&mut app, KeyCode::Char('R'));
        assert!(
            app.project_relation_dialog.is_none(),
            "plain R is now a filter character"
        );
        assert_eq!(app.project_filter, "R");

        // Ctrl is not a filter character, so it reaches the dialog binding
        // even with the type-ahead still active.
        handle_preview_key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL).unwrap();
        assert!(
            app.project_relation_dialog.is_some(),
            "Ctrl+R must open the relation dialog"
        );
    }

    #[test]
    fn ctrl_r_does_not_relate_a_project_the_filter_hides() {
        let (mut app, _dir) = knowledge_app();
        type_str(&mut app, "zz");
        assert!(names(&app).is_empty());
        handle_preview_key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL).unwrap();
        assert!(
            app.project_relation_dialog.is_none(),
            "the selected project is hidden, so there is nothing to relate"
        );
    }

    #[test]
    fn typing_does_nothing_inside_an_entered_project() {
        let (mut app, _dir) = knowledge_app();
        app.project_focus = Some(crate::tui::app::types::ProjectTab::Overview);
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(app.project_filter, "");
    }
}
