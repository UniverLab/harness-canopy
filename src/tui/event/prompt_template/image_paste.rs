//! Prompt-builder image keys (split from `prompt_template`): the
//! clipboard-image paste chord and the attachment chips row's keys.
use std::path::Path;

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::{build_prompt_action, PromptAction};
use crate::db::Database;
use crate::tui::app::dialog::{attach_image_bytes, SectionPickerMode, SimplePromptDialog};
use crate::tui::app::types::App;
use crate::tui::clipboard::{ImageClipboard as _, SystemClipboard};

/// Ctrl+V / Alt+V with exactly one of the two modifiers: the
/// clipboard-image paste chord. (Ctrl+Alt+V is AltGr on some layouts, not a
/// paste.)
fn is_image_paste_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('v') | KeyCode::Char('V'))
        && (modifiers == KeyModifiers::CONTROL || modifiers == KeyModifiers::ALT)
}

/// Try the clipboard-image branch: true when the key was consumed. Only when
/// no picker or overlay owns the keys; without an image it is a no-op —
/// pasted text arrives as a bracketed Paste event, never through this key.
pub(super) fn try_paste_clipboard_image(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> bool {
    if !is_image_paste_key(code, modifiers) {
        return false;
    }
    let builder_idle = app.simple_prompt_dialog.as_ref().is_some_and(|dialog| {
        dialog.pending_recall.is_none()
            && dialog.picker_mode == SectionPickerMode::None
            && dialog.at_picker.is_none()
    });
    if !builder_idle {
        return false;
    }
    paste_clipboard_image(app);
    true
}

/// Attach the clipboard image to the open builder, if any. `arboard` answers
/// synchronously; under WSL — where it cannot see the Windows clipboard — a
/// miss spawns the off-thread PowerShell reader instead of touching text.
/// Anywhere else a miss is a silent no-op.
fn paste_clipboard_image(app: &mut App) {
    if let Some(png) = SystemClipboard.read_image() {
        let db = app.db.clone();
        let shelf_dir = app.data_dir.join("shelf");
        let session_key = app.current_prompt_session_key();
        if let Some(dialog) = app.simple_prompt_dialog.as_mut() {
            match attach_image_bytes(
                &db,
                &shelf_dir,
                &session_key,
                &mut dialog.attachments,
                &png,
                "png",
            ) {
                Ok(()) => dialog.attach_error = None,
                Err(error) => dialog.attach_error = Some(error),
            }
        }
        return;
    }
    if crate::tui::clipboard::is_wsl() && !app.clipboard_image_pending {
        app.clipboard_image_pending = true;
        crate::tui::clipboard::spawn_wsl_clipboard_fetch(app.clipboard_image_tx.clone());
    }
}

/// Keys while the attachment chips row holds focus: Backspace/Delete remove
/// the focused chip, lateral arrows move along the row, Tab cycles out.
/// Always consumes the key.
pub(super) fn handle_chips_row_key(
    dialog: &mut SimplePromptDialog,
    code: KeyCode,
    modifiers: KeyModifiers,
    db: &Database,
    workdir: &Path,
) -> Result<PromptAction> {
    match code {
        KeyCode::Backspace | KeyCode::Delete => {
            dialog.remove_focused_chip();
            Ok(PromptAction::None)
        }
        KeyCode::Left => {
            dialog.chips_move(-1);
            Ok(PromptAction::None)
        }
        KeyCode::Right => {
            dialog.chips_move(1);
            Ok(PromptAction::None)
        }
        KeyCode::Tab => {
            dialog.focus_next();
            Ok(PromptAction::None)
        }
        KeyCode::BackTab => {
            dialog.focus_prev();
            Ok(PromptAction::None)
        }
        KeyCode::Esc => Ok(PromptAction::Close),
        KeyCode::Char('s') if modifiers.contains(KeyModifiers::CONTROL) => {
            Ok(build_prompt_action(dialog, db, workdir))
        }
        _ => Ok(PromptAction::None),
    }
}
