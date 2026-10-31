use super::*;

/// Persist `prompt` (and, if the builder is still open, its structured
/// field state) as the last prompt sent for `workdir`. Best-effort: a
/// failure to persist must never block the send itself.
pub(super) fn persist_last_prompt(app: &mut App, workdir: &Path, prompt: &str) {
    let builder_state = app
        .simple_prompt_dialog
        .as_ref()
        .map(crate::tui::app::dialog::PersistedBuilderState::from_dialog)
        .and_then(|state| serde_json::to_string(&state).ok());

    let id = format!("lp-{}", uuid::Uuid::new_v4());
    let workdir_str = workdir.to_string_lossy().to_string();
    if let Err(e) = app.db.insert_last_prompt(
        &id,
        &workdir_str,
        prompt,
        builder_state.as_deref(),
        chrono::Utc::now(),
    ) {
        tracing::warn!("Failed to persist last prompt for '{workdir_str}': {e}");
    }
}

/// Load the current project's last prompt into the builder (Ctrl+L). Shows a
/// one-line hint if nothing has been sent from this project yet; otherwise
/// applies immediately for an empty builder, or stages a confirm for a
/// non-empty one so the user doesn't lose in-progress work.
pub(super) fn recall_last_prompt(app: &mut App) {
    let workdir = app.current_workdir().to_string_lossy().to_string();
    let last = match app.db.get_last_prompt_for_workdir(&workdir) {
        Ok(Some(last)) => last,
        Ok(None) => {
            crate::domain::notification::send_notification(
                "No prompt to recall",
                "Nothing has been sent from this project yet.",
                crate::domain::notification::NotificationLevel::Info,
            );
            return;
        }
        Err(e) => {
            tracing::warn!("Failed to load last prompt for '{workdir}': {e}");
            return;
        }
    };

    let Some(dialog) = app.simple_prompt_dialog.as_mut() else {
        return;
    };

    if dialog.is_empty() {
        apply_last_prompt(dialog, &last);
    } else {
        dialog.pending_recall = Some(last);
    }
}

/// Apply a recalled prompt to `dialog`: a faithful structured restore when
/// the send captured the builder's field state, otherwise the flattened
/// prompt text in a single instruction section (see
/// `SimplePromptDialog::load_flat_text`).
pub(super) fn apply_last_prompt(
    dialog: &mut SimplePromptDialog,
    last: &crate::db::last_prompts::LastPrompt,
) {
    let restored = last.builder_state.as_deref().and_then(|json| {
        serde_json::from_str::<crate::tui::app::dialog::PersistedBuilderState>(json).ok()
    });

    match restored {
        Some(state) => state.restore_into(dialog),
        None => dialog.load_flat_text(&last.prompt_text),
    }
}

/// Resolve the currently selected interactive agent's session id and workdir,
/// if any — the target for a scheduled send or its cancellation.
pub(super) fn selected_session_target(app: &App) -> Option<(String, String)> {
    let idx = selected_interactive_index(app)?;
    let agent = app.interactive_agents.get(idx)?;
    Some((agent.id.clone(), agent.working_dir.clone()))
}

pub(super) fn schedule_send_prompt(app: &mut App, prompt: &str, when: chrono::NaiveDateTime) {
    let session_key = app.current_prompt_session_key();

    // Edit-in-place (B33): when the builder is editing an existing scheduled
    // send, re-confirming REPLACES it — delete the old row first, then insert
    // the edited one below, so the list count stays the same (no duplicate).
    let editing_id = app
        .simple_prompt_dialog
        .as_ref()
        .and_then(|d| d.editing_scheduled_id.clone());
    if let Some(old_id) = editing_id.as_deref() {
        if let Err(e) = app.db.delete_scheduled_prompt(old_id) {
            tracing::warn!("Failed to replace scheduled send '{old_id}': {e}");
        }
    }

    // Resolve the target session ID/workdir from the currently selected agent.
    let (target_session_id, target_workdir) = selected_session_target(app).unwrap_or_default();

    // `when` is a full local date-time picked in the builder (U11) and was
    // validated as future by the picker; resolve DST ambiguity leniently.
    let fire_time = when
        .and_local_timezone(chrono::Local)
        .earliest()
        .unwrap_or_else(chrono::Local::now)
        .with_timezone(&chrono::Utc);

    // Persist the scheduled send.
    let id = format!("ss-{}", uuid::Uuid::new_v4());
    let workdir_opt = (!target_workdir.is_empty()).then_some(target_workdir.as_str());
    // Only persist the structured view when it is what's being sent. A
    // non-empty Raw buffer sends verbatim (see `resolve_outgoing_prompt`), so
    // the structured sections no longer describe the outgoing text — raw
    // wins, and the structure is discarded (spec CP2). Reopening such a row
    // then falls back to the Raw tab with the flat text, rather than
    // restoring stale sections the user never sees.
    let raw_wins = app.simple_prompt_dialog.as_ref().is_some_and(|d| {
        d.active_tab == crate::tui::app::dialog::PromptTab::Raw && !d.raw_is_empty()
    });
    let builder_state_json = if raw_wins {
        None
    } else {
        app.simple_prompt_dialog
            .as_ref()
            .map(crate::tui::app::dialog::PersistedBuilderState::from_dialog)
            .and_then(|state| serde_json::to_string(&state).ok())
    };
    if let Err(e) = app.db.insert_scheduled_prompt(
        &id,
        prompt,
        &target_session_id,
        workdir_opt,
        fire_time,
        builder_state_json.as_deref(),
        None,
    ) {
        tracing::error!("Failed to persist scheduled send: {e}");
        crate::domain::notification::send_notification(
            "Schedule failed",
            &format!("Could not save scheduled send: {e}"),
            crate::domain::notification::NotificationLevel::Error,
        );
        return;
    }

    // Persist as the project's last prompt before discarding — a scheduled
    // send is still a "sent" prompt from the builder's perspective, and this
    // also covers the U7 dead-target path (see `deliver_due_scheduled_prompts`),
    // which recovers only the flattened text once the builder is long gone.
    let workdir = app.current_workdir();
    persist_last_prompt(app, &workdir, prompt);

    // The scheduled prompt was composed with the protocol block if it rides
    // along (automatic first turn or the `Canopy protocol` section) — record
    // that now, exactly like an immediate send, so the session doesn't get
    // the block again when the row fires (FR1). A verbatim Raw send carries
    // no block and records nothing.
    let sends_protocol = app
        .simple_prompt_dialog
        .as_ref()
        .is_some_and(|d| d.sends_protocol());
    if sends_protocol {
        super::record_protocol_sent(app, &session_key);
    }

    // Discard the dialog (don't persist builder session — prompt is scheduled).
    app.prompt_builder_sessions.remove(&session_key);
    app.discard_simple_prompt_dialog();

    // Show confirmation notification.
    let fire_local = fire_time.with_timezone(&chrono::Local);
    crate::domain::notification::send_notification(
        "Prompt scheduled",
        &format!("Will be delivered at {}", fire_local.format("%H:%M")),
        crate::domain::notification::NotificationLevel::Info,
    );
}

/// Cancel a pending scheduled send targeting the currently selected
/// interactive session (Ctrl+K). When a list entry is selected (the user is
/// browsing the scheduled-list panel, B33) that entry is canceled; otherwise it
/// falls back to the soonest pending send. No-op if there is none.
pub(super) fn cancel_scheduled_prompt(app: &mut App) {
    let Some((target_session_id, _)) = selected_session_target(app) else {
        return;
    };

    let pending = match app
        .db
        .list_pending_scheduled_prompts_for_session(&target_session_id)
    {
        Ok(pending) => pending,
        Err(e) => {
            tracing::warn!("Failed to list pending scheduled sends: {e}");
            return;
        }
    };

    if pending.is_empty() {
        return;
    }

    // A live list selection targets that row; with nothing selected the list is
    // ordered soonest-first, so index 0 is the soonest send.
    let selected = app
        .simple_prompt_dialog
        .as_ref()
        .and_then(|d| d.scheduled_list_selected);
    let idx = selected.unwrap_or(0).min(pending.len() - 1);
    let target_id = pending[idx].id.clone();

    match app.db.delete_scheduled_prompt(&target_id) {
        Ok(true) => {
            crate::domain::notification::send_notification(
                "Scheduled send canceled",
                "The scheduled prompt was canceled.",
                crate::domain::notification::NotificationLevel::Info,
            );
        }
        Ok(false) => {}
        Err(e) => tracing::warn!("Failed to cancel scheduled send '{target_id}': {e}"),
    }

    // Re-clamp the list selection against the shortened list (or drop out of
    // list-browse mode when the last entry was just canceled).
    if selected.is_some() {
        let remaining = pending.len().saturating_sub(1);
        if let Some(dialog) = app.simple_prompt_dialog.as_mut() {
            dialog.scheduled_list_selected = (remaining > 0).then(|| idx.min(remaining - 1));
        }
    }
}

/// Move keyboard focus into the pending-scheduled-sends list (B33), selecting
/// the first (soonest) entry. No-op when the selected session has no pending
/// sends — there is nothing to browse.
pub(super) fn enter_scheduled_list(app: &mut App) {
    let Some((target_session_id, _)) = selected_session_target(app) else {
        return;
    };
    let has_pending = app
        .db
        .list_pending_scheduled_prompts_for_session(&target_session_id)
        .map(|pending| !pending.is_empty())
        .unwrap_or(false);
    if !has_pending {
        return;
    }
    if let Some(dialog) = app.simple_prompt_dialog.as_mut() {
        dialog.scheduled_list_selected = Some(0);
    }
}

/// Handle a key while the scheduled-sends list panel has focus (B33): arrows
/// move the selection, Enter loads the selected send into the Raw tab for
/// in-place editing, Ctrl+K cancels the selected send, and Esc/Ctrl+P leave the
/// list. The list content is read live from the DB so it always reflects state.
pub(super) fn handle_scheduled_list_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let Some((target_session_id, _)) = selected_session_target(app) else {
        clear_scheduled_list_focus(app);
        return;
    };
    let pending = app
        .db
        .list_pending_scheduled_prompts_for_session(&target_session_id)
        .unwrap_or_default();
    if pending.is_empty() {
        clear_scheduled_list_focus(app);
        return;
    }

    let sel = app
        .simple_prompt_dialog
        .as_ref()
        .and_then(|d| d.scheduled_list_selected)
        .unwrap_or(0)
        .min(pending.len() - 1);
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);

    match code {
        KeyCode::Up => set_scheduled_list_selection(app, sel.saturating_sub(1)),
        KeyCode::Down => set_scheduled_list_selection(app, (sel + 1).min(pending.len() - 1)),
        KeyCode::Esc => clear_scheduled_list_focus(app),
        KeyCode::Char('p') if ctrl => clear_scheduled_list_focus(app),
        KeyCode::Char('k') if ctrl => cancel_scheduled_prompt(app),
        KeyCode::Enter => {
            let send = &pending[sel];
            let fire_local = send.fire_at.with_timezone(&chrono::Local).naive_local();
            let (id, prompt) = (send.id.clone(), send.prompt.clone());
            let builder_state = send.builder_state.clone();
            if let Some(dialog) = app.simple_prompt_dialog.as_mut() {
                dialog.load_scheduled_for_edit(&id, &prompt, fire_local, builder_state.as_deref());
            }
        }
        _ => {}
    }
}

pub(super) fn set_scheduled_list_selection(app: &mut App, index: usize) {
    if let Some(dialog) = app.simple_prompt_dialog.as_mut() {
        dialog.scheduled_list_selected = Some(index);
    }
}

pub(super) fn clear_scheduled_list_focus(app: &mut App) {
    if let Some(dialog) = app.simple_prompt_dialog.as_mut() {
        dialog.scheduled_list_selected = None;
    }
}
