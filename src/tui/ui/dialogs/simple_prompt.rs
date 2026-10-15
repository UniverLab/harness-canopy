use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use super::{draw_dialog_left_wave, truncate_str};
use crate::db::scheduled_sends::ScheduledSend;
use crate::tui::app::dialog::{PromptTab, SimplePromptDialog};
use crate::tui::app::types::{AgentEntry, App};
use crate::tui::ui::dialogs::at_picker::draw_at_picker_dropdown;
use crate::tui::ui::dialogs::section_picker::draw_section_picker_modal;
use crate::tui::ui::theme::Theme;

mod content;
mod geometry;
mod hints;
mod scheduled;
mod sections;

use geometry::{centered_rect_fixed, compute_prompt_geometry};
use hints::prompt_hint_line;
use scheduled::{draw_raw_tab_content, draw_scheduled_list, ScheduledList};
use sections::{draw_prompt_normal_content, prompt_accent};

#[cfg(test)]
use content::{style_collapsed_paste_blocks, wrap_styled_content};
#[cfg(test)]
use geometry::{generate_bottom_border, generate_top_border};
#[cfg(test)]
use hints::{active_send_shortcut_label, all_shortcut_hints, select_shortcut_hints, ShortcutHint};
#[cfg(test)]
use scheduled::scheduled_row_text;

/// Max rows the scheduled-sends list panel shows at once before it scrolls (B33).
const SCHEDULED_LIST_MAX_ROWS: usize = 4;

/// Draw the prompt builder. Returns the tab bar's origin `(x, y)` so the caller
/// can store it for mouse hit-testing the clickable Normal/Raw tabs, or `None`
/// when the dialog is not open.
/// Layout values the dialog needs before it can draw anything.
struct PromptGeometry {
    dialog_width: u16,
    height: u16,
    field_width: usize,
    section_heights: Vec<u16>,
}

/// Borrowed values the Normal-tab section renderer needs.
struct NormalContentCtx<'a> {
    dialog: &'a SimplePromptDialog,
    inner: Rect,
    field_width: usize,
    section_heights: &'a [u16],
    list_panel_height: u16,
    accent: Color,
    theme: &'a Theme,
}

/// Pending scheduled sends targeting the currently selected session —
/// shown next to the Schedule field and cancelable with Ctrl+K there.
fn pending_scheduled_for_selection(app: &App) -> Vec<ScheduledSend> {
    let selected_session_id = app.selected_agent().and_then(|a| match a {
        AgentEntry::Interactive(idx) => app.interactive_agents.get(*idx).map(|ia| ia.id.clone()),
        _ => None,
    });
    selected_session_id
        .as_deref()
        .and_then(|id| app.db.list_pending_scheduled_sends_for_session(id).ok())
        .unwrap_or_default()
}

/// Render the clickable Normal / Raw tab bar.
///
/// The active tab is drawn in accent (reversed for a clear "selected"
/// block); the inactive one is dim. Hit-boxes come from the same pure
/// geometry the mouse handler uses, so clicks land exactly on the labels.
fn draw_prompt_tab_bar(
    frame: &mut Frame,
    inner: Rect,
    dialog: &SimplePromptDialog,
    accent: Color,
    theme: &Theme,
) {
    let tab_boxes = SimplePromptDialog::tab_hitboxes(inner.x, inner.y);
    let tab_spans: Vec<Span> = tab_boxes
        .iter()
        .map(|(tab, _)| {
            let label = match tab {
                PromptTab::Normal => crate::tui::app::dialog::TAB_NORMAL_LABEL,
                PromptTab::Raw => crate::tui::app::dialog::TAB_RAW_LABEL,
            };
            if *tab == dialog.active_tab {
                Span::styled(
                    label,
                    Style::default()
                        .fg(accent)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                )
            } else {
                Span::styled(label, Style::default().fg(theme.dim_text))
            }
        })
        .collect();
    let tab_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(Line::from(tab_spans)), tab_area);
}

/// ── Send control (virtual focus index 0, U11) ───────────────────────
/// A single ghost line centered at the BOTTOM of the dialog — no box.
/// States: `send: now` / `send: date` selector (←→ toggles), inline
/// date-time picker while editing, `send: 2026-07-16 08:30` once picked.
fn draw_send_control(
    frame: &mut Frame,
    inner: Rect,
    dialog: &SimplePromptDialog,
    accent: Color,
    theme: &Theme,
    datetime_format: &str,
) {
    let send_is_focused = dialog.focused_section == 0 && !dialog.enabled_sections.is_empty();
    let send_y = inner.y + inner.height.saturating_sub(1);

    let mut send_spans: Vec<Span> = Vec::new();
    let ghost = Style::default().fg(theme.dim_text);
    let lit = Style::default().fg(accent).add_modifier(Modifier::BOLD);
    let label_style = if send_is_focused { lit } else { ghost };

    if let Some(edit) = dialog.send_edit.as_ref() {
        // Inline picker: highlight the focused field.
        send_spans.push(Span::styled("send: ", label_style));
        let field_style = |idx: usize| {
            if edit.field == idx {
                Style::default()
                    .fg(theme.accent_fg)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_primary)
            }
        };
        let v = edit.value;
        send_spans.push(Span::styled(v.format("%Y").to_string(), field_style(0)));
        send_spans.push(Span::styled("-", ghost));
        send_spans.push(Span::styled(v.format("%m").to_string(), field_style(1)));
        send_spans.push(Span::styled("-", ghost));
        send_spans.push(Span::styled(v.format("%d").to_string(), field_style(2)));
        send_spans.push(Span::styled(" ", ghost));
        send_spans.push(Span::styled(v.format("%H").to_string(), field_style(3)));
        send_spans.push(Span::styled(":", ghost));
        send_spans.push(Span::styled(v.format("%M").to_string(), field_style(4)));
        send_spans.push(Span::styled(
            "  ↑↓ adjust · ←→ field · Enter ok · Esc cancel",
            ghost,
        ));
    } else if send_is_focused {
        send_spans.push(Span::styled("‹ ", ghost));
        send_spans.push(Span::styled(
            format!("send: {}", dialog.send_display(datetime_format)),
            lit,
        ));
        send_spans.push(Span::styled(" ›", ghost));
        let hint = match (dialog.send_choice, dialog.send_at) {
            (crate::tui::app::dialog::SendChoice::Date, None) => "  Enter: pick date & time",
            (crate::tui::app::dialog::SendChoice::Date, Some(_)) => {
                "  Enter: edit · Backspace: now"
            }
            _ => "  ←→ change",
        };
        send_spans.push(Span::styled(hint, ghost));
    } else {
        send_spans.push(Span::styled(
            format!("send: {}", dialog.send_display(datetime_format)),
            ghost,
        ));
    }

    if let Some(error) = dialog.send_error.as_ref() {
        send_spans.push(Span::styled(
            format!("  ⚠ {error}"),
            Style::default().fg(theme.warning),
        ));
    }
    // Pending scheduled sends now render as a dedicated LIST panel above the
    // send line (see `draw_scheduled_list`), not crammed onto this bar (B33).

    let send_text_width: usize = send_spans.iter().map(|s| s.content.chars().count()).sum();
    let send_x = inner.x
        + inner
            .width
            .saturating_sub(send_text_width as u16)
            .saturating_div(2);
    let send_area = Rect {
        x: send_x,
        y: send_y,
        width: inner.width.saturating_sub(send_x - inner.x),
        height: 1,
    };
    frame.render_widget(Paragraph::new(Line::from(send_spans)), send_area);
}

pub fn draw_simple_prompt_dialog(
    frame: &mut Frame,
    app: &App,
    theme: &Theme,
) -> Option<((u16, u16), Option<ratatui::layout::Rect>)> {
    let dialog = app.simple_prompt_dialog.as_ref()?;

    let accent = prompt_accent(app, theme);
    let pending_scheduled = pending_scheduled_for_selection(app);

    // Scheduled-sends list panel geometry (B33).
    let (list_selected, list_visible_rows, list_scroll, list_panel_height) =
        scheduled_list_layout(dialog, pending_scheduled.len());
    let geo = compute_prompt_geometry(frame.area(), dialog, list_panel_height);

    let area = centered_rect_fixed(geo.dialog_width, geo.height, frame.area());
    frame.render_widget(Clear, area);
    draw_dialog_left_wave(frame, area, app.animation_tick.into());

    let block = Block::default()
        .title(" Prompt Builder ")
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(theme.dialog_bg));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // ── Tab bar (row 0) ───────────────────────────────────────────────────
    draw_prompt_tab_bar(frame, inner, dialog, accent, theme);

    // Draw hint line (row 1).
    let instructions = prompt_hint_line(
        inner.width,
        app.keyboard_enhancement_active,
        !pending_scheduled.is_empty(),
        theme,
    );
    let instructions_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(instructions), instructions_area);

    draw_send_control(
        frame,
        inner,
        dialog,
        accent,
        theme,
        &app.tui_settings.datetime_format,
    );

    // ── Content region ──────────────────────────────────────────────────
    // The Raw tab draws a single free-text field (or the composed-prompt
    // preview when empty); the Normal tab draws its scrolling section stack.
    let picker_anchor_area = if dialog.active_tab == PromptTab::Raw {
        draw_raw_tab_content(
            frame,
            dialog,
            accent,
            inner,
            geo.field_width,
            list_panel_height,
            theme,
        );
        None
    } else {
        let ctx = NormalContentCtx {
            dialog,
            inner,
            field_width: geo.field_width,
            section_heights: &geo.section_heights,
            list_panel_height,
            accent,
            theme,
        };
        draw_prompt_normal_content(frame, &ctx)
    };

    // ── Scheduled-sends list panel (B33) ─────────────────────────────────
    // A compact list of pending scheduled sends, one row each, docked just
    // above the send line. Navigable/selectable via keyboard (Ctrl+P).
    if list_panel_height > 0 {
        let panel_top = inner.y + inner.height.saturating_sub(1 + list_panel_height);
        draw_scheduled_list(
            frame,
            &ScheduledList {
                pending: &pending_scheduled,
                selected: list_selected,
                scroll: list_scroll,
                visible_rows: list_visible_rows,
                editing_id: dialog.editing_scheduled_id.as_deref(),
            },
            accent,
            inner,
            panel_top,
            theme,
        );
    }

    draw_prompt_pickers(
        frame,
        app,
        dialog,
        accent,
        theme,
        area,
        picker_anchor_area.unwrap_or(inner),
    );

    let content_rect = if dialog.active_tab == PromptTab::Raw {
        Some(SimplePromptDialog::raw_content_rect(
            inner,
            list_panel_height,
        ))
    } else {
        None
    };
    Some(((inner.x, inner.y), content_rect))
}

/// B33: the geometry of the scheduled-sends list panel for `dialog` with
/// `pending_len` pending sends — the selected row, the visible rows, the
/// scroll offset, and the panel height (a label/top-border row plus one row
/// per visible entry, or zero when empty).
fn scheduled_list_layout(
    dialog: &SimplePromptDialog,
    pending_len: usize,
) -> (Option<usize>, usize, usize, u16) {
    let list_selected = dialog.scheduled_list_selected.filter(|_| pending_len > 0);
    let (list_visible_rows, list_scroll) = SimplePromptDialog::scheduled_list_view(
        pending_len,
        SCHEDULED_LIST_MAX_ROWS,
        list_selected,
    );
    let list_panel_height: u16 = if pending_len == 0 {
        0
    } else {
        1 + list_visible_rows as u16
    };
    (
        list_selected,
        list_visible_rows,
        list_scroll,
        list_panel_height,
    )
}

/// Draw the @ file picker dropdown (when active) and the section picker modal
/// (when open).
fn draw_prompt_pickers(
    frame: &mut Frame,
    app: &App,
    dialog: &SimplePromptDialog,
    accent: Color,
    theme: &Theme,
    area: Rect,
    anchor: Rect,
) {
    // Draw @ file picker dropdown if active
    if dialog.at_picker.is_some() {
        draw_at_picker_dropdown(frame, area, anchor, accent, dialog, theme);
    }

    // Draw picker modal if open
    draw_section_picker_modal(frame, app, accent, &dialog.picker_mode, theme);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn active_send_shortcut_prefers_shift_enter_when_supported() {
        assert_eq!(active_send_shortcut_label(true), ("Shift+Enter ", "send"));
    }

    #[test]
    fn active_send_shortcut_falls_back_to_ctrl_s_when_unsupported() {
        assert_eq!(active_send_shortcut_label(false), ("Ctrl+S ", "send"));
    }

    #[test]
    fn wrap_preserves_spaces_and_breaks_on_newline() {
        let styled = vec![("hola mundo\nsegunda línea".to_string(), None)];
        let lines = wrap_styled_content(styled, None, 40, Color::Black, Color::White);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(texts, vec!["hola mundo", "segunda línea"]);
    }

    #[test]
    fn wrap_matches_visual_line_count_on_overflow() {
        use crate::tui::app::dialog::SimplePromptDialog;
        let content = "una frase que se pasa del ancho del campo";
        let width = 10;
        let lines = wrap_styled_content(
            vec![(content.to_string(), None)],
            None,
            width,
            Color::Black,
            Color::White,
        );
        assert_eq!(
            lines.len(),
            SimplePromptDialog::visual_line_count(content, width)
        );
        // No characters are lost or altered by wrapping.
        let joined: String = lines.iter().map(|l| line_text(l)).collect();
        assert_eq!(joined, content.replace('\n', ""));
    }

    #[test]
    fn wrap_renders_cursor_at_end_as_blank_cell() {
        let styled = vec![("ab".to_string(), None)];
        let lines = wrap_styled_content(styled, Some(2), 40, Color::Black, Color::White);
        assert_eq!(line_text(&lines[0]), "ab ");
    }

    fn hint_keys(width: usize) -> Vec<String> {
        select_shortcut_hints(width, "Ctrl+S ", "send", false)
            .iter()
            .map(|hint| hint.key.trim().to_string())
            .collect()
    }

    fn hint_keys_scheduled(width: usize) -> Vec<String> {
        select_shortcut_hints(width, "Ctrl+S ", "send", true)
            .iter()
            .map(|hint| hint.key.trim().to_string())
            .collect()
    }

    #[test]
    fn shortcut_bar_shows_every_hint_at_full_width() {
        let keys = hint_keys(200);
        assert_eq!(
            keys,
            vec![
                "Shift+↑↓",
                "@",
                "Ctrl+A",
                "Ctrl+X",
                "Shift+←→",
                "Ctrl+L",
                "Ctrl+S",
                "Esc"
            ]
        );
        // The removed "↑↓ fields" entry must not reappear.
        assert!(!keys.iter().any(|k| k == "↑↓"));
    }

    #[test]
    fn shortcut_bar_advertises_ctrl_p_only_when_sends_are_pending() {
        // With pending sends, Ctrl+P appears (between recall and send); without,
        // it never does.
        let with = hint_keys_scheduled(200);
        assert_eq!(
            with,
            vec![
                "Shift+↑↓",
                "@",
                "Ctrl+A",
                "Ctrl+X",
                "Shift+←→",
                "Ctrl+L",
                "Ctrl+P",
                "Ctrl+S",
                "Esc"
            ]
        );
        assert!(!hint_keys(200).iter().any(|k| k == "Ctrl+P"));
    }

    #[test]
    fn scheduled_row_text_shows_time_and_truncated_preview() {
        let fire = chrono::NaiveDate::from_ymd_opt(2026, 7, 20)
            .unwrap()
            .and_hms_opt(14, 5, 0)
            .unwrap();
        // Only the first line of the prompt is previewed.
        let row = scheduled_row_text(fire, "run the build\nthen deploy", 40);
        assert_eq!(row, "07-20 14:05  run the build");
        // Narrow widths truncate with an ellipsis and never overflow.
        let narrow = scheduled_row_text(fire, "a very long prompt line here", 18);
        assert!(narrow.chars().count() <= 18);
        assert!(narrow.starts_with("07-20 14:05"));
    }

    #[test]
    fn shortcut_bar_keeps_only_send_and_hide_when_very_narrow() {
        // Even below the width the two mandatory hints need, they stay visible.
        assert_eq!(hint_keys(10), vec!["Ctrl+S", "Esc"]);
        assert_eq!(hint_keys(1), vec!["Ctrl+S", "Esc"]);
    }

    #[test]
    fn shortcut_bar_drops_least_important_first_at_medium_width() {
        // 45 cols fits navigate(19) + send(13) + hide(9) = 41, but not recall.
        let keys = hint_keys(45);
        assert_eq!(keys, vec!["Shift+↑↓", "Ctrl+S", "Esc"]);
        assert!(!keys.iter().any(|k| k == "@"));
        assert!(!keys.iter().any(|k| k == "Ctrl+L"));
    }

    #[test]
    fn shortcut_bar_preserves_display_order_after_dropping_middle_hints() {
        // 60 cols keeps navigate + recall but not @ file (dropped by priority),
        // and the survivors stay in left-to-right display order.
        let keys = hint_keys(60);
        assert_eq!(keys, vec!["Shift+↑↓", "Ctrl+L", "Ctrl+S", "Esc"]);
    }

    #[test]
    fn shortcut_bar_always_keeps_the_two_mandatory_hints() {
        for width in [0usize, 5, 22, 50, 99, 300] {
            let keys = hint_keys(width);
            assert!(keys.contains(&"Ctrl+S".to_string()), "width {width}");
            assert!(keys.contains(&"Esc".to_string()), "width {width}");
        }
    }

    #[test]
    fn generate_top_border_basic() {
        let line = generate_top_border(" Title ", 20, Style::default());
        assert_eq!(line.width(), 20);
    }

    #[test]
    fn generate_top_border_empty_title() {
        let line = generate_top_border("", 10, Style::default());
        assert_eq!(line.width(), 10);
    }

    #[test]
    fn generate_top_border_narrow() {
        let line = generate_top_border("Hi", 4, Style::default());
        assert_eq!(line.width(), 4);
    }

    #[test]
    fn generate_bottom_border_basic() {
        let line = generate_bottom_border(20, Style::default());
        assert_eq!(line.width(), 20);
    }

    #[test]
    fn generate_bottom_border_narrow() {
        let line = generate_bottom_border(3, Style::default());
        assert_eq!(line.width(), 3);
    }

    #[test]
    fn style_collapsed_paste_blocks_basic() {
        let result = style_collapsed_paste_blocks(
            "line1\n[Pasted ~3 lines]\nline3",
            Color::Black,
            Color::Black,
        );
        assert!(!result.is_empty());
    }

    #[test]
    fn style_collapsed_paste_blocks_no_paste() {
        let result = style_collapsed_paste_blocks("hello world", Color::Black, Color::Black);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "hello world");
        assert!(result[0].1.is_none());
    }

    #[test]
    fn style_collapsed_paste_blocks_empty() {
        let result = style_collapsed_paste_blocks("", Color::Black, Color::Black);
        assert!(result.is_empty());
    }

    #[test]
    fn centered_rect_fixed_basic() {
        let area = ratatui::layout::Rect::new(0, 0, 100, 40);
        let result = centered_rect_fixed(50, 10, area);
        assert!(result.width <= 50);
        assert_eq!(result.height, 10);
    }

    #[test]
    fn centered_rect_fixed_full_width() {
        let area = ratatui::layout::Rect::new(0, 0, 100, 40);
        let result = centered_rect_fixed(100, 5, area);
        assert_eq!(result.height, 5);
    }

    #[test]
    fn wrap_empty_content() {
        // The function always pushes at least one line
        let lines = wrap_styled_content(vec![], None, 40, Color::Black, Color::White);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn wrap_single_short_line() {
        let lines = wrap_styled_content(
            vec![("hello".to_string(), None)],
            None,
            40,
            Color::Black,
            Color::White,
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "hello");
    }

    #[test]
    fn scheduled_row_text_short_prompt() {
        let fire = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap();
        let row = scheduled_row_text(fire, "short", 40);
        assert!(row.contains("09:00"));
        assert!(row.contains("short"));
    }

    #[test]
    fn shortcut_bar_zero_width() {
        let keys = hint_keys(0);
        assert!(keys.contains(&"Ctrl+S".to_string()));
        assert!(keys.contains(&"Esc".to_string()));
    }

    #[test]
    fn wrap_with_style() {
        let styled = vec![("hello".to_string(), Some(Color::Red))];
        let lines = wrap_styled_content(styled, None, 40, Color::Black, Color::White);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn generate_top_border_long_title() {
        let line =
            generate_top_border("A Very Long Title That Exceeds Width", 10, Style::default());
        assert_eq!(line.width(), 10);
    }

    #[test]
    fn wrap_preserves_newline_at_end() {
        let styled = vec![("hello\n".to_string(), None)];
        let lines = wrap_styled_content(styled, None, 40, Color::Black, Color::White);
        // "hello" on first line, empty second line
        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "hello");
    }

    #[test]
    fn wrap_multiple_newlines() {
        let styled = vec![("a\nb\nc".to_string(), None)];
        let lines = wrap_styled_content(styled, None, 40, Color::Black, Color::White);
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "a");
        assert_eq!(line_text(&lines[1]), "b");
        assert_eq!(line_text(&lines[2]), "c");
    }

    #[test]
    fn wrap_tab_expands_to_spaces() {
        let styled = vec![("a\tb".to_string(), None)];
        let lines = wrap_styled_content(styled, None, 40, Color::Black, Color::White);
        let text = line_text(&lines[0]);
        assert!(text.contains("b"));
        // Tab should be expanded to spaces
        assert!(text.len() > 3);
    }

    #[test]
    fn wrap_tab_at_width_boundary() {
        let styled = vec![("abcd\tef".to_string(), None)];
        let lines = wrap_styled_content(styled, None, 5, Color::Black, Color::White);
        // Tab after "abcd" (col 4) wraps to next line
        assert!(lines.len() >= 2);
    }

    #[test]
    fn wrap_cursor_in_middle() {
        let styled = vec![("hello".to_string(), None)];
        let lines = wrap_styled_content(styled, Some(2), 40, Color::Black, Color::White);
        // Cursor at position 2 should be visible
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn wrap_cursor_at_start() {
        let styled = vec![("hello".to_string(), None)];
        let lines = wrap_styled_content(styled, Some(0), 40, Color::Black, Color::White);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn wrap_styled_content_with_accent_color() {
        let styled = vec![
            ("hello ".to_string(), Some(Color::Red)),
            ("world".to_string(), Some(Color::Blue)),
        ];
        let lines = wrap_styled_content(styled, None, 40, Color::Black, Color::White);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "hello world");
    }

    #[test]
    fn centered_rect_fixed_wider_than_area() {
        let area = ratatui::layout::Rect::new(0, 0, 50, 20);
        let result = centered_rect_fixed(100, 10, area);
        // Should be clamped to area width
        assert!(result.width <= 50);
    }

    #[test]
    fn centered_rect_fixed_taller_than_area() {
        let area = ratatui::layout::Rect::new(0, 0, 100, 10);
        let result = centered_rect_fixed(50, 20, area);
        // Should be clamped to area height
        assert!(result.height <= 10);
    }

    #[test]
    fn centered_rect_fixed_zero_dimensions() {
        let area = ratatui::layout::Rect::new(0, 0, 0, 0);
        let result = centered_rect_fixed(10, 5, area);
        // Should handle gracefully
        assert!(result.width >= 1);
        assert!(result.height >= 1);
    }

    #[test]
    fn active_send_shortcut_both_variants() {
        let (key1, desc1) = active_send_shortcut_label(true);
        let (key2, desc2) = active_send_shortcut_label(false);
        assert_eq!(desc1, "send");
        assert_eq!(desc2, "send");
        assert_ne!(key1, key2);
    }

    #[test]
    fn all_shortcut_hints_includes_mandatory() {
        let hints = all_shortcut_hints("Ctrl+S ", "send", false);
        let keys: Vec<String> = hints.iter().map(|h| h.key.trim().to_string()).collect();
        assert!(keys.contains(&"Ctrl+S".to_string()));
        assert!(keys.contains(&"Esc".to_string()));
    }

    #[test]
    fn all_shortcut_hints_with_scheduled() {
        let hints = all_shortcut_hints("Ctrl+S ", "send", true);
        let keys: Vec<String> = hints.iter().map(|h| h.key.trim().to_string()).collect();
        assert!(keys.contains(&"Ctrl+P".to_string()));
    }

    #[test]
    fn select_shortcut_hints_tiny_width() {
        let hints = select_shortcut_hints(1, "Ctrl+S ", "send", false);
        let keys: Vec<String> = hints.iter().map(|h| h.key.trim().to_string()).collect();
        assert!(keys.contains(&"Ctrl+S".to_string()));
        assert!(keys.contains(&"Esc".to_string()));
    }

    #[test]
    fn style_collapsed_paste_blocks_unclosed_bracket() {
        let result = style_collapsed_paste_blocks(
            "text [Pasted ~3 lines more text here",
            Color::Red,
            Color::Black,
        );
        // No closing bracket, so rest is treated as normal text
        assert!(!result.is_empty());
    }

    #[test]
    fn style_collapsed_paste_blocks_multiple_pastes() {
        let result = style_collapsed_paste_blocks(
            "[Pasted ~2 lines] and [Pasted ~3 lines]",
            Color::Red,
            Color::Black,
        );
        assert!(!result.is_empty());
    }

    #[test]
    fn scheduled_row_text_empty_prompt() {
        let fire = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let row = scheduled_row_text(fire, "", 40);
        assert!(row.contains("00:00"));
    }

    #[test]
    fn scheduled_row_text_multiline_prompt() {
        let fire = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(12, 30, 0)
            .unwrap();
        let row = scheduled_row_text(fire, "first line\nsecond line\nthird line", 40);
        assert!(row.contains("first line"));
        assert!(!row.contains("second line"));
    }

    #[test]
    fn shortcut_hint_width() {
        let hint = ShortcutHint {
            key: "Ctrl+S ".to_string(),
            desc: "send  ".to_string(),
            priority: 1,
        };
        // "Ctrl+S " (7 chars) + "send  " (6 chars) = 13
        assert_eq!(hint.width(), 13);
    }

    #[test]
    fn wrap_content_exactly_field_width() {
        let styled = vec![("abcde".to_string(), None)];
        let lines = wrap_styled_content(styled, None, 5, Color::Black, Color::White);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "abcde");
    }

    #[test]
    fn wrap_content_one_over_field_width() {
        let styled = vec![("abcdef".to_string(), None)];
        let lines = wrap_styled_content(styled, None, 5, Color::Black, Color::White);
        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "abcde");
        assert_eq!(line_text(&lines[1]), "f");
    }

    #[test]
    fn wrap_content_empty_segments() {
        let styled = vec![(String::new(), None), ("hello".to_string(), None)];
        let lines = wrap_styled_content(styled, None, 40, Color::Black, Color::White);
        assert_eq!(line_text(&lines[0]), "hello");
    }

    /// active_send_shortcut_label returns keyboard-enhancement-dependent labels
    #[test]
    fn active_send_shortcut_label_with_enhancement() {
        let (key, hint) = active_send_shortcut_label(true);
        assert_eq!(key, "Shift+Enter ");
        assert_eq!(hint, "send");
    }

    /// active_send_shortcut_label returns Ctrl+S when enhancement is off
    #[test]
    fn active_send_shortcut_label_without_enhancement() {
        let (key, hint) = active_send_shortcut_label(false);
        assert_eq!(key, "Ctrl+S ");
        assert_eq!(hint, "send");
    }

    /// all_shortcut_hints always includes core shortcuts
    #[test]
    fn all_shortcut_hints_includes_core_shortcuts() {
        let hints = all_shortcut_hints("Ctrl+S ", "send", false);
        // Should have at least 8 entries (without scheduled)
        assert!(hints.len() >= 8);
        // Find key hints
        let has_ctrl_s = hints.iter().any(|h| h.key.contains("Ctrl+S"));
        let has_esc = hints.iter().any(|h| h.key.contains("Esc"));
        assert!(has_ctrl_s, "should include Ctrl+S");
        assert!(has_esc, "should include Esc");
    }

    /// all_shortcut_hints includes Ctrl+P only when has_scheduled is true
    #[test]
    fn all_shortcut_hints_ctrl_p_conditional() {
        let without = all_shortcut_hints("Ctrl+S ", "send", false);
        let with = all_shortcut_hints("Ctrl+S ", "send", true);

        let without_ctrl_p = without.iter().any(|h| h.key.contains("Ctrl+P"));
        let with_ctrl_p = with.iter().any(|h| h.key.contains("Ctrl+P"));

        assert!(
            !without_ctrl_p,
            "should not have Ctrl+P when has_scheduled=false"
        );
        assert!(with_ctrl_p, "should have Ctrl+P when has_scheduled=true");
    }

    /// all_shortcut_hints uses provided send_label and send_hint
    #[test]
    fn all_shortcut_hints_uses_provided_labels() {
        let hints = all_shortcut_hints("Custom ", "action", false);
        let send_hint = hints
            .iter()
            .find(|h| h.key == "Custom ")
            .expect("should have custom send key");
        assert_eq!(send_hint.desc, "action  ");
    }

    /// select_shortcut_hints keeps core hints even when narrow
    #[test]
    fn select_shortcut_hints_keeps_core_shortcuts() {
        // Very narrow width should still keep send and hide
        let hints = select_shortcut_hints(5, "Ctrl+S ", "send", false);
        let priorities: Vec<_> = hints.iter().map(|h| h.priority).collect();
        // Should still have priorities 1 and 2 (send and hide)
        assert!(priorities.contains(&1), "should keep send (priority 1)");
        assert!(priorities.contains(&2), "should keep hide (priority 2)");
    }

    /// select_shortcut_hints returns all hints when width is large
    #[test]
    fn select_shortcut_hints_all_fit_when_wide() {
        let all = all_shortcut_hints("Ctrl+S ", "send", true);
        let selected = select_shortcut_hints(500, "Ctrl+S ", "send", true);
        assert_eq!(
            selected.len(),
            all.len(),
            "all hints should fit when width=500"
        );
    }

    /// all_shortcut_hints priorities are maintained
    #[test]
    fn all_shortcut_hints_have_valid_priorities() {
        let hints = all_shortcut_hints("Ctrl+S ", "send", true);
        for hint in &hints {
            // Priority 1 and 2 are reserved for send/hide
            // Others should be 3+
            assert!(hint.priority > 0, "all priorities should be positive");
        }
        // Send should have priority 1
        let send = hints.iter().find(|h| h.key.contains("Ctrl+S"));
        assert_eq!(send.map(|h| h.priority), Some(1));
        // Hide should have priority 2
        let hide = hints.iter().find(|h| h.key.contains("Esc"));
        assert_eq!(hide.map(|h| h.priority), Some(2));
    }
}
