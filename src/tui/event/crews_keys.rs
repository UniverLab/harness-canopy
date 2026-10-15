//! CT30: key routing for the Crews panel face.
//!
//! Routed from `handle_key` only while the panel is focused and the Crews
//! face is showing (D1): everything else falls through to the global keys
//! untouched. Open dialogs swallow every key (Esc closes them). All writes
//! go through `App::spawn_crews_write`, never a synchronous call on the UI
//! thread (D3).

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::domain::models_db::{self, ModelEntry};
use crate::tui::app::crews_calls;
use crate::tui::app::crews_dialog::CrewsDialog;
use crate::tui::app::crews_face::{CrewsSection, WriteOutcome};
use crate::tui::app::crews_logic::{
    keep_visible, parse_local_until, plan_toggle, plan_unavailable_until, prefix_coverage,
    provider_prefix, selected_member_platform, MemberEdit, RosterWrite,
};
use crate::tui::app::types::{App, PanelFace};

/// Route one key to the Crews face. Returns `true` when the key belongs to
/// the face (or an open dialog) so the global/focus handlers skip it.
pub(super) fn handle_crews_face_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if app.panel_face != PanelFace::Crews || !app.panel_focused {
        return false;
    }
    if app.crews.dialog.is_some() {
        dialog_keys(app, code);
        return true;
    }
    if modifiers.contains(KeyModifiers::SHIFT) && matches!(code, KeyCode::Up | KeyCode::Down) {
        crew_member_move(app, code == KeyCode::Down);
        return true;
    }
    match code {
        KeyCode::Tab | KeyCode::BackTab => {
            // D17: two sections, so both directions toggle.
            app.crews.section = match app.crews.section {
                CrewsSection::Pairs => CrewsSection::Crews,
                CrewsSection::Crews => CrewsSection::Pairs,
            };
            return true;
        }
        KeyCode::Up | KeyCode::Down => {
            move_active_selection(app, code == KeyCode::Down);
            return true;
        }
        _ => {}
    }
    match app.crews.section {
        CrewsSection::Pairs => pairs_keys(app, code),
        CrewsSection::Crews => crews_keys(app, code),
    }
}

/// The next index after a ↑/↓ step, clamped to `[0, len)`.
fn step(current: usize, len: usize, down: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if down {
        current.saturating_add(1).min(len - 1)
    } else {
        current.saturating_sub(1)
    }
}

/// ↑/↓ in the active section: the expanded crew's members win over the
/// crew list (D16's expanded row). The scroll offset follows the selection
/// through the pure `keep_visible`, using the height the last draw wrote.
fn move_active_selection(app: &mut App, down: bool) {
    let height = app.crews.last_list_height.get();
    match app.crews.section {
        CrewsSection::Pairs => {
            app.crews.pair_idx = step(app.crews.pair_idx, app.crews.pair_rows.len(), down);
            app.crews.pair_scroll = keep_visible(app.crews.pair_idx, app.crews.pair_scroll, height);
        }
        CrewsSection::Crews => {
            if app.crews.expanded.is_some() {
                let len = app
                    .selected_crew()
                    .map(|crew| crew.members.len())
                    .unwrap_or(0);
                app.crews.member_idx = step(app.crews.member_idx, len, down);
            } else {
                let len = app
                    .crews
                    .snapshot
                    .as_ref()
                    .map(|snap| snap.crews.len())
                    .unwrap_or(0);
                app.crews.crew_idx = step(app.crews.crew_idx, len, down);
                app.crews.crew_scroll =
                    keep_visible(app.crews.crew_idx, app.crews.crew_scroll, height);
            }
        }
    }
}

/// The Space-toggle plan for the selected pair — pure so the key-handler
/// tests can assert exactly which writes are queued (D9).
fn toggle_writes(app: &App) -> Vec<RosterWrite> {
    let Some(row) = app.selected_pair() else {
        return Vec::new();
    };
    let roster = app
        .crews
        .snapshot
        .as_ref()
        .map(|snap| snap.roster.as_slice())
        .unwrap_or(&[]);
    plan_toggle(row, roster, &chrono::Utc::now())
}

/// Pairs-section keys. `Enter`/`a`/`d` belong to the Crews section and
/// fall through unhandled here.
fn pairs_keys(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Char(' ') => {
            toggle_selected_pair(app);
            true
        }
        KeyCode::Char('e') => {
            if let Some((platform, model)) = app
                .selected_pair()
                .map(|row| (row.platform.clone(), row.model.clone()))
            {
                app.crews.dialog = Some(CrewsDialog::Unavailable {
                    platform,
                    model,
                    reason: String::new(),
                    until: String::new(),
                    field: 0,
                });
            }
            true
        }
        KeyCode::Char('p') => {
            open_prefix_dialog(app);
            true
        }
        KeyCode::Char('r') => {
            app.request_crews_refresh();
            true
        }
        _ => false,
    }
}

/// Space: queue the planned roster writes for the selected pair (D9). An
/// empty plan means nothing about the pair's availability could change.
fn toggle_selected_pair(app: &mut App) {
    let Some(row) = app.selected_pair() else {
        return;
    };
    let label = row.label();
    let will_be = if row.available {
        "unavailable"
    } else {
        "available"
    };
    let writes = toggle_writes(app);
    if writes.is_empty() {
        app.set_crews_message(true, "nothing to change");
        return;
    }
    let text = format!("Marked {label} {will_be}.");
    app.spawn_crews_write(Box::new(move |port| {
        crews_calls::run_roster_writes(port, &writes)
            .map(|_| WriteOutcome::Status {
                is_error: false,
                text: text.clone(),
            })
            .map_err(|error| format!("{error:#}"))
    }));
}

/// `p`: mark the whole provider prefix of the selected pair unavailable
/// (D11). A model without a `/` has no prefix and is refused outright.
fn open_prefix_dialog(app: &mut App) {
    let Some(row) = app.selected_pair() else {
        return;
    };
    let Some(pattern) = provider_prefix(&row.model) else {
        app.set_crews_message(true, "model has no provider prefix");
        return;
    };
    let platform = row.platform.clone();
    let covers = prefix_coverage(&app.crews.pair_rows, &platform, &pattern);
    app.crews.dialog = Some(CrewsDialog::ProviderPrefix {
        platform,
        pattern,
        covers,
    });
}

/// Crews-section keys: Enter expands (and reads bindings), `a`/`d` edit
/// the selected crew's member list.
fn crews_keys(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Enter => {
            toggle_expand(app);
            true
        }
        KeyCode::Char('a') => {
            open_add_member(app);
            true
        }
        KeyCode::Char('d') => {
            remove_member(app);
            true
        }
        KeyCode::Char('r') => {
            app.request_crews_refresh();
            true
        }
        _ => false,
    }
}

/// Enter: expand the selected crew (dispatching the D16 binding read) or
/// collapse it if it is already expanded.
fn toggle_expand(app: &mut App) {
    let Some(crew) = app.selected_crew() else {
        return;
    };
    let name = crew.name.clone();
    if app.crews.expanded.as_deref() == Some(name.as_str()) {
        app.crews.expanded = None;
        app.crews.member_idx = 0;
        return;
    }
    app.crews.expanded = Some(name.clone());
    app.crews.member_idx = 0;
    app.spawn_crews_bindings(name);
}

/// `a`: open the model-suggestion dialog for the selected crew. The member
/// is added on the platform of the crew's LAST member (D12); no catalog
/// means no dialog, just a status error.
fn open_add_member(app: &mut App) {
    let Some(crew) = app.selected_crew() else {
        return;
    };
    let crew_name = crew.name.clone();
    let Some(platform) = selected_member_platform(&crew.members) else {
        app.set_crews_message(true, "a crew needs at least one member");
        return;
    };
    let catalog_ttl = dirs::home_dir()
        .map(|home| {
            crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"))
                .models
                .catalog_ttl()
        })
        .unwrap_or(models_db::DEFAULT_CATALOG_TTL);
    let Some(catalog) = models_db::load_catalog_nonblocking(catalog_ttl) else {
        app.set_crews_message(true, "no model catalog");
        return;
    };
    let suggestions = models_db::suggestions_for(&catalog, &platform, "");
    app.crews.dialog = Some(CrewsDialog::AddMember {
        crew: crew_name,
        platform,
        query: String::new(),
        suggestions,
        idx: 0,
        catalog: Some(catalog),
    });
}

/// `d`: remove the selected member. Refuses to empty a crew before any
/// daemon call happens (D13).
fn remove_member(app: &mut App) {
    let Some(crew) = app.selected_crew() else {
        return;
    };
    if crew.members.len() <= 1 {
        app.set_crews_message(true, "a crew needs at least one member");
        return;
    }
    let name = crew.name.clone();
    let index = app.crews.member_idx.min(crew.members.len() - 1);
    let last_index = crew.members.len() - 2;
    let edit = MemberEdit::Remove { index };
    app.spawn_crews_write(Box::new(move |port| {
        crews_calls::run_member_edit(port, &name, &edit)
            .map(|outcome| WriteOutcome::CrewEdit {
                crew_name: name.clone(),
                version: outcome.version,
                bound_graph_ids: outcome.bound_graph_ids,
            })
            .map_err(|error| format!("{error:#}"))
    }));
    app.crews.member_idx = app.crews.member_idx.min(last_index);
}

/// Shift+↑/↓: reorder the selected member inside the expanded crew. The
/// optimistic index move only happens when the step is legal; a move past
/// either end is still sent so the worker's refusal surfaces as a message.
fn crew_member_move(app: &mut App, down: bool) {
    if app.crews.section != CrewsSection::Crews {
        return;
    }
    let Some(crew) = app.selected_crew() else {
        return;
    };
    if app.crews.expanded.as_deref() != Some(crew.name.as_str()) {
        return;
    }
    let len = crew.members.len();
    let index = app.crews.member_idx.min(len.saturating_sub(1));
    let name = crew.name.clone();
    let edit = MemberEdit::Move { index, down };
    app.spawn_crews_write(Box::new(move |port| {
        crews_calls::run_member_edit(port, &name, &edit)
            .map(|outcome| WriteOutcome::CrewEdit {
                crew_name: name.clone(),
                version: outcome.version,
                bound_graph_ids: outcome.bound_graph_ids,
            })
            .map_err(|error| format!("{error:#}"))
    }));
    let target = if down {
        Some(index + 1)
    } else {
        index.checked_sub(1)
    };
    if let Some(target) = target {
        if target < len {
            app.crews.member_idx = target;
        }
    }
}

/// Every key goes to the open dialog; `true` always (dialogs swallow).
/// A returned `Some` keeps the dialog open, `None` closes it.
fn dialog_keys(app: &mut App, code: KeyCode) -> bool {
    let Some(dialog) = app.crews.dialog.take() else {
        return true;
    };
    let kept = if matches!(&dialog, CrewsDialog::Unavailable { .. }) {
        unavailable_keys(app, dialog, code)
    } else if matches!(&dialog, CrewsDialog::ProviderPrefix { .. }) {
        prefix_keys(app, dialog, code)
    } else {
        add_member_keys(app, dialog, code)
    };
    if let Some(dialog) = kept {
        app.crews.dialog = Some(dialog);
    }
    true
}

/// The `e`-dialog: Tab/↑/↓ switch field, typing edits the active one, Enter
/// submits (a malformed or past Until keeps the dialog open with a status
/// error — no write is planned, D10).
fn unavailable_keys(app: &mut App, dialog: CrewsDialog, code: KeyCode) -> Option<CrewsDialog> {
    let CrewsDialog::Unavailable {
        platform,
        model,
        mut reason,
        mut until,
        mut field,
    } = dialog
    else {
        return None;
    };
    let kept = |reason: String, until: String, field: usize| CrewsDialog::Unavailable {
        platform: platform.clone(),
        model: model.clone(),
        reason,
        until,
        field,
    };
    match code {
        KeyCode::Esc => None,
        KeyCode::Tab | KeyCode::Up | KeyCode::Down => {
            field = 1 - field;
            Some(kept(reason, until, field))
        }
        KeyCode::Backspace => {
            if field == 0 {
                reason.pop();
            } else {
                until.pop();
            }
            Some(kept(reason, until, field))
        }
        KeyCode::Char(c) => {
            if field == 0 {
                reason.push(c);
            } else {
                until.push(c);
            }
            Some(kept(reason, until, field))
        }
        KeyCode::Enter => submit_unavailable(app, &platform, model.clone(), reason, &until),
        _ => Some(kept(reason, until, field)),
    }
}

/// The `e`-dialog's Enter: validate the Until, plan the write, close on
/// success — or keep the dialog open with the status error.
fn submit_unavailable(
    app: &mut App,
    platform: &str,
    model: String,
    reason: String,
    until: &str,
) -> Option<CrewsDialog> {
    let reopen = CrewsDialog::Unavailable {
        platform: platform.to_string(),
        model: model.clone(),
        reason: reason.clone(),
        until: until.to_string(),
        field: 1,
    };
    match parse_local_until(until, &chrono::Utc::now()) {
        Err(error) => {
            app.set_crews_message(true, error);
            Some(reopen)
        }
        Ok(until_utc) => {
            let plan_reason = if reason.trim().is_empty() {
                None
            } else {
                Some(reason)
            };
            let write = plan_unavailable_until(platform, &model, plan_reason, until_utc);
            let model_label = if model.is_empty() {
                "(default)".to_string()
            } else {
                model
            };
            let text = format!("Marked {platform}/{model_label} unavailable.");
            let writes = vec![write];
            app.spawn_crews_write(Box::new(move |port| {
                crews_calls::run_roster_writes(port, &writes)
                    .map(|_| WriteOutcome::Status {
                        is_error: false,
                        text: text.clone(),
                    })
                    .map_err(|error| format!("{error:#}"))
            }));
            None
        }
    }
}

/// The `p`-dialog: Enter confirms the prefix write (D11), everything else
/// except Esc is ignored.
fn prefix_keys(app: &mut App, dialog: CrewsDialog, code: KeyCode) -> Option<CrewsDialog> {
    let CrewsDialog::ProviderPrefix {
        platform,
        pattern,
        covers,
    } = dialog
    else {
        return None;
    };
    match code {
        KeyCode::Esc => None,
        KeyCode::Enter => {
            let writes = vec![RosterWrite::Set {
                platform,
                model_pattern: Some(pattern.clone()),
                state: "unavailable",
                reason: Some("marked in TUI".to_string()),
                until: None,
            }];
            let text = format!("Marked {pattern} unavailable. ({covers} pair(s))");
            app.spawn_crews_write(Box::new(move |port| {
                crews_calls::run_roster_writes(port, &writes)
                    .map(|_| WriteOutcome::Status {
                        is_error: false,
                        text: text.clone(),
                    })
                    .map_err(|error| format!("{error:#}"))
            }));
            None
        }
        _ => Some(CrewsDialog::ProviderPrefix {
            platform,
            pattern,
            covers,
        }),
    }
}

/// The `a`-dialog: ↑/↓ pick, typing refilters the catalog suggestions,
/// Enter appends the chosen member to the crew (D12).
fn add_member_keys(app: &mut App, dialog: CrewsDialog, code: KeyCode) -> Option<CrewsDialog> {
    let CrewsDialog::AddMember {
        crew,
        platform,
        mut query,
        suggestions,
        mut idx,
        catalog,
    } = dialog
    else {
        return None;
    };
    let kept = |query: String, suggestions: Vec<ModelEntry>, idx: usize| CrewsDialog::AddMember {
        crew: crew.clone(),
        platform: platform.clone(),
        query,
        suggestions,
        idx,
        catalog: catalog.clone(),
    };
    match code {
        KeyCode::Esc => None,
        KeyCode::Up => {
            idx = idx.saturating_sub(1);
            Some(kept(query, suggestions, idx))
        }
        KeyCode::Down => {
            if idx + 1 < suggestions.len() {
                idx += 1;
            }
            Some(kept(query, suggestions, idx))
        }
        KeyCode::Backspace => {
            query.pop();
            let mut next = kept(query, suggestions, idx);
            next.refilter();
            Some(next)
        }
        KeyCode::Char(c) => {
            query.push(c);
            let mut next = kept(query, suggestions, idx);
            next.refilter();
            Some(next)
        }
        KeyCode::Enter => {
            let Some(entry) = suggestions.get(idx) else {
                app.set_crews_message(true, "no model matches");
                return Some(kept(query, suggestions, idx));
            };
            let member = serde_json::json!({
                "platform": platform.clone(),
                "model": entry.id.clone(),
            });
            let edit = MemberEdit::Add { member };
            let name = crew.clone();
            app.spawn_crews_write(Box::new(move |port| {
                crews_calls::run_member_edit(port, &name, &edit)
                    .map(|outcome| WriteOutcome::CrewEdit {
                        crew_name: name.clone(),
                        version: outcome.version,
                        bound_graph_ids: outcome.bound_graph_ids,
                    })
                    .map_err(|error| format!("{error:#}"))
            }));
            None
        }
        _ => Some(kept(query, suggestions, idx)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::StateRepository;
    use crate::db::roster_entries::{RosterEntry, RosterState};
    use crate::db::Database;
    use crate::tui::app::crews_logic::{build_pair_rows, CrewView, CrewsSnapshot, PairSummary};
    use chrono::Utc;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_app() -> App {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).expect("create test db"));
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        // Point the daemon client at a closed port so spawned write threads
        // fail fast instead of reaching the live daemon on 127.0.0.1:7755.
        StateRepository::set_state(&*app.db, "port", "1").expect("set test port");
        app
    }

    fn summary(platform: &str, model: &str) -> PairSummary {
        PairSummary {
            platform: platform.to_string(),
            model: model.to_string(),
            graph_count: 1,
            graph_names: vec!["g1".to_string()],
        }
    }

    fn entry(id: i64, platform: &str, pattern: Option<&str>) -> RosterEntry {
        RosterEntry {
            id,
            platform: platform.to_string(),
            model_pattern: pattern.map(str::to_string),
            state: RosterState::Unavailable,
            reason: Some("quota".to_string()),
            until: None,
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn crew(name: &str, members: &[(&str, &str)]) -> CrewView {
        let members: Vec<serde_json::Value> = members
            .iter()
            .map(|(platform, model)| {
                serde_json::json!({
                    "platform": platform,
                    "model": if model.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(model.to_string()) },
                })
            })
            .collect();
        CrewView {
            name: name.to_string(),
            description: None,
            version: 1,
            member_pairs: members
                .iter()
                .map(|m| {
                    (
                        m["platform"].as_str().unwrap_or_default().to_string(),
                        m["model"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect(),
            members,
            bindings: Vec::new(),
            binding_total: 0,
        }
    }

    /// A focused Crews face seeded with `snapshot` and its built pair rows.
    fn face_app(snapshot: CrewsSnapshot) -> App {
        let mut app = test_app();
        app.panel_face = PanelFace::Crews;
        app.panel_focused = true;
        app.crews.pair_rows = build_pair_rows(&snapshot, &Utc::now());
        app.crews.snapshot = Some(snapshot);
        app
    }

    #[test]
    fn space_on_available_pair_queues_unavailable_write() {
        let mut app = face_app(CrewsSnapshot {
            roster: Vec::new(),
            pairs: vec![summary("opencode-go", "kimi-k2")],
            crews: Vec::new(),
        });
        let expected = vec![RosterWrite::Set {
            platform: "opencode-go".to_string(),
            model_pattern: Some("kimi-k2".to_string()),
            state: "unavailable",
            reason: Some("marked in TUI".to_string()),
            until: None,
        }];
        assert_eq!(toggle_writes(&app), expected, "the exact planned write");

        assert!(handle_crews_face_key(
            &mut app,
            KeyCode::Char(' '),
            KeyModifiers::NONE
        ));
        assert!(app.crews.write_rx.is_some(), "the write was dispatched");
    }

    #[test]
    fn space_on_unavailable_exact_pair_queues_clear() {
        let mut app = face_app(CrewsSnapshot {
            roster: vec![entry(7, "opencode-go", Some("kimi-k2"))],
            pairs: vec![summary("opencode-go", "kimi-k2")],
            crews: Vec::new(),
        });
        let expected = vec![RosterWrite::Clear {
            platform: "opencode-go".to_string(),
            model_pattern: Some("kimi-k2".to_string()),
        }];
        assert_eq!(toggle_writes(&app), expected, "the exact planned write");

        assert!(handle_crews_face_key(
            &mut app,
            KeyCode::Char(' '),
            KeyModifiers::NONE
        ));
        assert!(app.crews.write_rx.is_some(), "the write was dispatched");
    }

    #[test]
    fn p_coverage_count_matches_visible_pairs() {
        // Full-id models (`opencode-go/a`) — the shape a provider prefix
        // (`opencode-go/*`) can be derived from.
        let mut app = face_app(CrewsSnapshot {
            roster: Vec::new(),
            pairs: vec![
                summary("opencode-go", "opencode-go/a"),
                summary("opencode-go", "opencode-go/b"),
                summary("claude", "x"),
            ],
            crews: Vec::new(),
        });
        app.crews.pair_idx = app
            .crews
            .pair_rows
            .iter()
            .position(|row| row.model == "opencode-go/a")
            .expect("row present");

        assert!(handle_crews_face_key(
            &mut app,
            KeyCode::Char('p'),
            KeyModifiers::NONE
        ));
        match &app.crews.dialog {
            Some(CrewsDialog::ProviderPrefix {
                pattern, covers, ..
            }) => {
                assert_eq!(pattern, "opencode-go/*");
                assert_eq!(*covers, 2, "both opencode-go rows, not claude/x");
            }
            other => panic!("expected the prefix dialog, got {other:?}"),
        }
    }

    #[test]
    fn p_on_model_without_slash_refused() {
        let mut app = face_app(CrewsSnapshot {
            roster: Vec::new(),
            pairs: vec![summary("opencode", "gpt-5")],
            crews: Vec::new(),
        });
        assert!(handle_crews_face_key(
            &mut app,
            KeyCode::Char('p'),
            KeyModifiers::NONE
        ));
        assert!(
            app.crews.dialog.is_none(),
            "no dialog for a prefixless model"
        );
        let message = app.crews.message.as_ref().expect("a status message");
        assert!(message.is_error);
        assert_eq!(message.text, "model has no provider prefix");
    }

    #[test]
    fn d_refuses_to_empty_a_crew() {
        let mut app = face_app(CrewsSnapshot {
            roster: Vec::new(),
            pairs: Vec::new(),
            crews: vec![crew("review", &[("claude", "sonnet")])],
        });
        app.crews.section = CrewsSection::Crews;

        assert!(handle_crews_face_key(
            &mut app,
            KeyCode::Char('d'),
            KeyModifiers::NONE
        ));
        assert!(app.crews.write_rx.is_none(), "no daemon call happened");
        let message = app.crews.message.as_ref().expect("a status message");
        assert!(message.is_error);
        assert_eq!(message.text, "a crew needs at least one member");
    }

    #[test]
    fn keys_do_nothing_when_panel_not_focused() {
        let mut app = face_app(CrewsSnapshot {
            roster: Vec::new(),
            pairs: vec![summary("opencode-go", "kimi-k2")],
            crews: Vec::new(),
        });
        app.panel_focused = false;
        assert!(!handle_crews_face_key(
            &mut app,
            KeyCode::Char(' '),
            KeyModifiers::NONE
        ));
        assert!(app.crews.write_rx.is_none());
        assert!(app.crews.dialog.is_none());
    }

    #[test]
    fn tab_switches_section() {
        let mut app = face_app(CrewsSnapshot::default());
        assert_eq!(app.crews.section, CrewsSection::Pairs);
        assert!(handle_crews_face_key(
            &mut app,
            KeyCode::Tab,
            KeyModifiers::NONE
        ));
        assert_eq!(app.crews.section, CrewsSection::Crews);
        assert!(handle_crews_face_key(
            &mut app,
            KeyCode::BackTab,
            KeyModifiers::NONE
        ));
        assert_eq!(app.crews.section, CrewsSection::Pairs);
    }

    #[test]
    fn dialog_swallows_keys_and_esc_closes_it() {
        let mut app = face_app(CrewsSnapshot {
            roster: Vec::new(),
            pairs: vec![summary("opencode-go", "kimi-k2")],
            crews: Vec::new(),
        });
        assert!(handle_crews_face_key(
            &mut app,
            KeyCode::Char('e'),
            KeyModifiers::NONE
        ));
        assert!(app.crews.dialog.is_some(), "`e` opened the dialog");

        // Every key is swallowed and lands in the active field…
        assert!(handle_crews_face_key(
            &mut app,
            KeyCode::Char('x'),
            KeyModifiers::NONE
        ));
        match &app.crews.dialog {
            Some(CrewsDialog::Unavailable { reason, .. }) => assert_eq!(reason, "x"),
            other => panic!("expected the Unavailable dialog, got {other:?}"),
        }

        // …and Esc closes it without queuing anything.
        assert!(handle_crews_face_key(
            &mut app,
            KeyCode::Esc,
            KeyModifiers::NONE
        ));
        assert!(app.crews.dialog.is_none());
        assert!(app.crews.write_rx.is_none());
    }
}
