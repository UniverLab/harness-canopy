use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::fields::{
    append_directory_section, append_identity_section, append_prompt_section,
    append_sandbox_section, append_yolo_section, terminal_shell_row, DirSection,
};
use super::{
    background_trigger_label, filter_row, focus_style, interactive_mode_row, model_value,
    picker_detail_style, picker_item_style, push_spaced_row, selector_spans, truncate_str,
    truncate_with_ellipsis, FieldLayout, PickerWindow, BACKGROUND_TRIGGER_FIELD,
    CLI_PICKER_VISIBLE, MODEL_PICKER_VISIBLE, SESSION_PICKER_VISIBLE,
    SESSION_RESUME_PICKER_VISIBLE, TERMINAL_DIR_FIELD,
};
use crate::tui::app::dialog::new_agent::{BackgroundTrigger, NewAgentDialog};
use crate::tui::ui::theme::Theme;

/// Layout inputs shared by the section builders: the per-field layout, the
/// inner field width, and how many directory rows are visible.
#[derive(Clone, Copy)]
pub(super) struct SectionLayout {
    pub fields: FieldLayout,
    pub field_width: usize,
    pub dir_visible: usize,
}

pub(super) fn append_interactive_sections(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    filtered_clis: &[usize],
    theme: &Theme,
    section: SectionLayout,
) {
    let layout = section.fields;
    if !dialog.is_edit_mode() {
        push_spaced_row(lines, interactive_mode_row(dialog, accent, theme));
        // Sits between choosing Resume and CLI selection: the harness
        // follows from whichever session is picked here.
        append_session_resume_picker_rows(lines, dialog, theme);
    }

    append_cli_section(lines, dialog, accent, filtered_clis, layout.cli, theme);
    append_session_picker_rows(lines, dialog, theme);
    append_identity_section(lines, dialog, accent, layout.identity, theme);
    append_yolo_section(lines, dialog, accent, layout.yolo, theme);
    append_sandbox_section(lines, dialog, accent, layout.sandbox, theme);
    append_directory_section(
        lines,
        dialog,
        accent,
        theme,
        DirSection {
            field: layout.dir,
            hidden: false,
            browser_field: layout.dir,
            visible: section.dir_visible,
        },
    );
}

pub(super) fn append_terminal_sections(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    theme: &Theme,
    dir_visible: usize,
) {
    append_directory_section(
        lines,
        dialog,
        accent,
        theme,
        DirSection {
            field: TERMINAL_DIR_FIELD,
            hidden: false,
            browser_field: TERMINAL_DIR_FIELD,
            visible: dir_visible,
        },
    );
    push_spaced_row(lines, terminal_shell_row(dialog, accent, theme));
}

pub(super) fn append_background_sections(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    filtered_clis: &[usize],
    theme: &Theme,
    section: SectionLayout,
) {
    let layout = section.fields;
    append_trigger_section(lines, dialog, accent, theme);
    append_action_section(lines, dialog, accent, theme);
    if dialog.is_send_to_session() {
        append_target_section(lines, dialog, accent, layout.cli, theme);
    } else {
        append_cli_section(lines, dialog, accent, filtered_clis, layout.cli, theme);
        append_model_section(lines, dialog, accent, layout.model, theme);
    }
    append_prompt_section(lines, dialog, accent, layout, section.field_width, theme);

    let hide_dir = dialog.background_trigger == BackgroundTrigger::Watch;
    let browser_field = if hide_dir { layout.extra } else { layout.dir };
    append_directory_section(
        lines,
        dialog,
        accent,
        theme,
        DirSection {
            field: layout.dir,
            hidden: hide_dir,
            browser_field,
            visible: section.dir_visible,
        },
    );
}

/// Status text for the canopy-native resume picker: which session (and
/// harness) is resolved, that none exist, or that the picker is open.
pub(super) fn session_resume_label(dialog: &NewAgentDialog) -> String {
    if dialog.resume_sessions_empty {
        return "  (no resumable sessions)".to_string();
    }
    if dialog.session_resume_picker.is_some() {
        return "  ↵/↑↓ choose session to resume".to_string();
    }
    match &dialog.selected_resume_session {
        Some(session) => format!(
            "  [{}] on {}",
            truncate_with_ellipsis(&session.name, 32),
            session.cli
        ),
        None => String::new(),
    }
}

fn append_trigger_section(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    theme: &Theme,
) {
    let mut trigger_row = vec![Span::styled(
        "  Trigger:",
        Style::default().fg(theme.dim_text),
    )];
    trigger_row.extend(selector_spans(
        background_trigger_label(dialog.background_trigger),
        BACKGROUND_TRIGGER_FIELD,
        dialog.is_edit_mode(),
        accent,
        dialog.field,
        theme,
    ));
    push_spaced_row(lines, Line::from(trigger_row));
}

/// The delivery action row (CM53): headless CLI run, or delivery into a live
/// session. Space toggles it; edit mode shows it read-only.
fn append_action_section(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    theme: &Theme,
) {
    let mut row = vec![Span::styled(
        "  Action:",
        Style::default().fg(theme.dim_text),
    )];
    row.extend(selector_spans(
        dialog.background_action.label(),
        BACKGROUND_TRIGGER_FIELD,
        dialog.is_edit_mode(),
        accent,
        dialog.field,
        theme,
    ));
    if !dialog.is_edit_mode() {
        row.push(Span::styled(
            "  (Space toggles)",
            Style::default().fg(theme.dim_text),
        ));
    }
    push_spaced_row(lines, Line::from(row));
}

/// In send mode, the target session picker replaces the CLI/model rows.
fn append_target_section(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    cli_field: usize,
    theme: &Theme,
) {
    let target = dialog
        .selected_send_target()
        .unwrap_or_else(|| "(no live session — open one)".to_string());
    lines.push(Line::from(vec![
        Span::styled("  Target: ", Style::default().fg(theme.dim_text)),
        Span::styled(
            format!(" {target} "),
            focus_style(dialog.field, cli_field, accent),
        ),
        Span::styled("  (◂▸ cycle)", Style::default().fg(theme.dim_text)),
    ]));
    lines.push(Line::from(""));
}

fn append_cli_section(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    filtered_clis: &[usize],
    cli_field: usize,
    theme: &Theme,
) {
    // The harness name always reads in that harness's own accent; focus only
    // adds the header-coloured highlight behind it.
    let cli_accent = dialog.cli_accent_color(dialog.cli_index, theme);
    let value_style = if dialog.field == cli_field {
        Style::default()
            .fg(cli_accent)
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(cli_accent)
    };
    lines.push(Line::from(vec![
        Span::styled("  Harness: ", Style::default().fg(theme.dim_text)),
        Span::styled(
            format!(" {} ", dialog.cli_display_label(dialog.cli_index)),
            value_style,
        ),
        Span::styled(
            "  (◂▸ cycle · type/Space pick)",
            Style::default().fg(theme.dim_text),
        ),
    ]));

    append_cli_picker_rows(lines, dialog, accent, filtered_clis, cli_field, theme);
    lines.push(Line::from(""));
}

fn append_cli_picker_rows(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    filtered_clis: &[usize],
    cli_field: usize,
    theme: &Theme,
) {
    if !dialog.cli_picker_open {
        return;
    }

    let total = dialog.available_clis.len();
    let total_matches = filtered_clis.len();
    let window = PickerWindow::new(dialog.cli_picker_idx, total_matches, CLI_PICKER_VISIBLE);

    lines.push(filter_row(
        "    filter: ",
        &dialog.cli_picker_filter,
        dialog.field == cli_field,
        accent,
        theme,
    ));

    if filtered_clis.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (no matches)",
            Style::default().fg(theme.dim_text),
        )));
    } else {
        for (i, cli_idx) in filtered_clis
            .iter()
            .enumerate()
            .skip(window.scroll)
            .take(CLI_PICKER_VISIBLE)
        {
            let is_selected = i == dialog.cli_picker_idx;
            let style = picker_item_style(accent, is_selected);
            // Each row names its own harness in that harness's accent; the
            // selection highlight only adds the header background behind it.
            let label_style = if is_selected {
                Style::default()
                    .fg(dialog.cli_accent_color(*cli_idx, theme))
                    .bg(accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(dialog.cli_accent_color(*cli_idx, theme))
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("    {} ", if is_selected { "›" } else { " " }),
                    style,
                ),
                Span::styled(dialog.cli_display_label(*cli_idx), label_style),
            ]));
        }
    }

    let footer = if filtered_clis.is_empty() {
        "    Backspace clear  Esc close".to_string()
    } else if total_matches > CLI_PICKER_VISIBLE {
        format!("    … {total_matches}/{total} harnesses  ↑↓ scroll  Enter/Esc close")
    } else {
        format!("    {total_matches}/{total} harnesses  type to filter  Enter/Esc close")
    };
    lines.push(Line::from(Span::styled(
        footer,
        Style::default().fg(theme.dim_text),
    )));
}

fn append_model_section(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    model_field: usize,
    theme: &Theme,
) {
    lines.push(Line::from(vec![
        Span::styled("  Model: ", Style::default().fg(theme.dim_text)),
        Span::styled(
            model_value(dialog),
            focus_style(dialog.field, model_field, accent),
        ),
    ]));

    append_model_picker_rows(lines, dialog, accent, model_field, theme);
    lines.push(Line::from(""));
}

fn append_model_picker_rows(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    model_field: usize,
    theme: &Theme,
) {
    if dialog.field != model_field || !dialog.model_picker_open {
        return;
    }
    if dialog.model_suggestions.is_empty() {
        return;
    }

    let total = dialog.model_suggestions.len();
    let window = PickerWindow::new(dialog.model_suggestion_idx, total, MODEL_PICKER_VISIBLE);

    for (i, entry) in dialog
        .model_suggestions
        .iter()
        .enumerate()
        .skip(window.scroll)
        .take(MODEL_PICKER_VISIBLE)
    {
        let is_selected = i == dialog.model_suggestion_idx;
        let style = picker_item_style(accent, is_selected);
        lines.push(Line::from(vec![
            Span::styled(
                format!("    {} ", if is_selected { "›" } else { " " }),
                style,
            ),
            Span::styled(truncate_str(&entry.id, 38), style),
            Span::styled(
                format!(" [{}]", entry.provider),
                picker_detail_style(is_selected, style, theme),
            ),
        ]));
    }

    if total > MODEL_PICKER_VISIBLE {
        lines.push(Line::from(Span::styled(
            format!("    … {total} models  ↑↓ scroll  Enter accept  Esc close"),
            Style::default().fg(theme.dim_text),
        )));
    }
}

fn append_session_picker_rows(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    theme: &Theme,
) {
    if !dialog.session_picker_open {
        return;
    }

    let total = dialog.session_entries.len();
    let window = PickerWindow::new(dialog.session_picker_idx, total, SESSION_PICKER_VISIBLE);

    if total == 0 {
        lines.push(Line::from(Span::styled(
            "    (no sessions found)",
            Style::default().fg(theme.dim_text),
        )));
        return;
    }

    for (i, (id, label)) in dialog
        .session_entries
        .iter()
        .enumerate()
        .skip(window.scroll)
        .take(SESSION_PICKER_VISIBLE)
    {
        let is_selected = i == dialog.session_picker_idx;
        let style = picker_item_style(Color::Cyan, is_selected);
        lines.push(Line::from(vec![
            Span::styled(
                format!("    {} ", if is_selected { "›" } else { " " }),
                style,
            ),
            Span::styled(truncate_str(id, 18), style),
            Span::styled(
                format!("  {}", truncate_str(label, 36)),
                picker_detail_style(is_selected, style, theme),
            ),
        ]));
    }

    if total > SESSION_PICKER_VISIBLE {
        lines.push(Line::from(Span::styled(
            format!("    … {total} sessions  ↑↓ scroll  Enter accept  Esc close"),
            Style::default().fg(theme.dim_text),
        )));
    }
}

/// Rows for the canopy-native session-resume picker (C12): each row shows
/// name, harness and last-active, most-recent-first (FR 2, 3).
fn append_session_resume_picker_rows(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    theme: &Theme,
) {
    let Some(picker) = &dialog.session_resume_picker else {
        return;
    };

    let total = picker.sessions.len();
    let window = PickerWindow::new(picker.index, total, SESSION_RESUME_PICKER_VISIBLE);

    for (i, session) in picker
        .sessions
        .iter()
        .enumerate()
        .skip(window.scroll)
        .take(SESSION_RESUME_PICKER_VISIBLE)
    {
        let is_selected = i == picker.index;
        let style = picker_item_style(Color::Cyan, is_selected);
        lines.push(Line::from(vec![
            Span::styled(
                format!("    {} ", if is_selected { "›" } else { " " }),
                style,
            ),
            Span::styled(truncate_str(&session.name, 22), style),
            Span::styled(
                format!("  {}", truncate_str(&session.cli, 12)),
                picker_detail_style(is_selected, style, theme),
            ),
            Span::styled(
                format!("  {}", truncate_str(&session.last_active, 20)),
                picker_detail_style(is_selected, style, theme),
            ),
        ]));
    }

    if total > SESSION_RESUME_PICKER_VISIBLE {
        lines.push(Line::from(Span::styled(
            format!("    … {total} sessions  ↑↓ scroll  Enter resume  Esc cancel"),
            Style::default().fg(theme.dim_text),
        )));
    }
}
